//! Capa de bloques: abstracción común sobre discos, imágenes y dispositivos virtuales.
//!
//! Implementaciones actuales (todas de **solo lectura**):
//! - [`ImageFile`]: imagen sector a sector (RAW/IMG/DD).
//! - [`MemDevice`]: búfer en memoria (pruebas).
//! - [`FaultyDevice`]: envoltorio que simula sectores ilegibles (pruebas del lector tolerante).
//!
//! El acceso a dispositivos físicos está **prohibido** en esta fase: ver [`path_policy`].
//! Los contenedores (VHD, VHDX, VMDK, E01…) se **detectan** ([`format`]) pero no se interpretan
//! todavía, para no confundir su contenido con un disco RAW.

pub mod fault;
pub mod format;
mod image;
mod mem;
pub mod path_policy;
pub mod resilient;
pub mod view;

pub use fault::FaultyDevice;
pub use format::{probe_format, ImageFormat};
pub use image::ImageFile;
pub use mem::MemDevice;

/// Estado de validez de cada sector entregado por una lectura. Toda exportación o recuperación debe
/// conservarlo para no confundir datos sustituidos con datos reales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SectorState {
    /// Leído correctamente al primer intento.
    Read = 0,
    /// Leído correctamente tras uno o más reintentos.
    Recovered = 1,
    /// Ilegible: el contenido entregado es un **patrón de relleno**, no datos.
    Unreadable = 2,
    /// No se intentó leer (salto sobre zona dañada): el contenido es un **patrón de relleno**.
    Skipped = 3,
    /// No asignado en el contenedor (región dispersa): ceros legítimos por definición del formato.
    Sparse = 4,
}

impl SectorState {
    pub const ALL: [SectorState; 5] = [
        SectorState::Read,
        SectorState::Recovered,
        SectorState::Unreadable,
        SectorState::Skipped,
        SectorState::Sparse,
    ];

    /// `true` si el contenido representa fielmente el origen (leído, recuperado o disperso).
    pub fn is_trustworthy(self) -> bool {
        !matches!(self, SectorState::Unreadable | SectorState::Skipped)
    }
}

/// Asignación de un rango de sectores según el formato del origen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Allocation {
    /// Hay datos almacenados (deben leerse).
    Data,
    /// No asignado / cero por definición del formato (no hay nada que leer).
    Sparse,
}

/// Tramo contiguo `(lba_inicial, nº de sectores, asignación)`.
pub type AllocationRun = (u64, u64, Allocation);

use std::io;
use thiserror::Error;

/// Tamaño máximo de una petición de lectura síncrona (límite de memoria por llamada).
pub const MAX_READ_REQUEST: usize = 64 * 1024 * 1024;

/// Tamaños de sector lógico que ofrece la interfaz (el motor admite potencias de 2 de 512 a 65536).
pub const COMMON_SECTOR_SIZES: [u32; 4] = [512, 1024, 2048, 4096];

#[derive(Debug, Error)]
pub enum IoError {
    #[error("E/S: {0}")]
    Io(#[from] io::Error),
    #[error("error de lectura en el desplazamiento {offset} ({len} bytes): {source}")]
    Read {
        offset: u64,
        len: usize,
        #[source]
        source: io::Error,
    },
    #[error("lectura fuera de límites: desplazamiento {offset}, longitud {len}, tamaño {size}")]
    OutOfBounds { offset: u64, len: usize, size: u64 },
    #[error("rango de sectores no válido: LBA {lba} + {count} sectores (el origen tiene {total})")]
    InvalidRange { lba: u64, count: u64, total: u64 },
    #[error("petición demasiado grande: {requested} bytes (máximo {max})")]
    RequestTooLarge { requested: u128, max: usize },
    #[error("ruta no permitida (acceso a dispositivos deshabilitado): {0}")]
    DevicePathRejected(String),
    #[error("tamaño de sector no válido: {0} (debe ser potencia de 2 entre 512 y 65536)")]
    InvalidSectorSize(u32),
    #[error("formato de contenedor {0} detectado: todavía no está soportado (no se interpreta como RAW)")]
    UnsupportedFormat(ImageFormat),
    #[error("operación cancelada")]
    Cancelled,
    /// Estructura del contenedor dañada o incoherente.
    #[error("{format}: estructura dañada o incoherente: {detail}")]
    Corrupt {
        format: &'static str,
        detail: String,
    },
    /// Variante o característica del formato que todavía no se admite.
    #[error("{format}: característica no soportada: {detail}")]
    UnsupportedFeature {
        format: &'static str,
        detail: String,
    },
    /// El contenedor necesita otro fichero (padre, extensión…) que no se abre automáticamente.
    #[error("{format}: requiere el fichero externo «{reference}», que no se abre automáticamente")]
    ExternalReference {
        format: &'static str,
        reference: String,
    },
}

pub type Result<T> = std::result::Result<T, IoError>;

/// Dispositivo de bloques de solo lectura.
pub trait BlockDevice: Send + Sync {
    /// Tamaño total en bytes.
    fn size(&self) -> u64;
    /// Tamaño de sector lógico en bytes.
    fn sector_size(&self) -> u32;
    /// Lee exactamente `buf.len()` bytes desde `offset`. Falla si el rango excede el tamaño.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Número de sectores completos.
    fn sector_count(&self) -> u64 {
        self.size() / u64::from(self.sector_size())
    }

    /// Bytes finales que no completan un sector.
    fn trailing_bytes(&self) -> u64 {
        self.size() % u64::from(self.sector_size())
    }

    /// Mapa de asignación de `count` sectores desde `lba`, en tramos contiguos ordenados que cubren
    /// exactamente el rango. Por defecto todo es `Data`; los contenedores dispersos lo redefinen.
    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        if count == 0 || lba.checked_add(count).is_none_or(|e| e > total) {
            return Err(IoError::InvalidRange { lba, count, total });
        }
        Ok(vec![(lba, count, Allocation::Data)])
    }

    /// Lee `count` sectores completos desde `lba` con validación estricta del rango.
    /// `buf` debe medir exactamente `count × sector_size` bytes.
    fn read_sectors(&self, lba: u64, count: u64, buf: &mut [u8]) -> Result<()> {
        let (offset, len) = sector_span(self, lba, count)?;
        if buf.len() != len {
            return Err(IoError::OutOfBounds {
                offset,
                len: buf.len(),
                size: len as u64,
            });
        }
        self.read_at(offset, buf)
    }
}

/// Valida un rango de sectores y devuelve (desplazamiento en bytes, longitud en bytes).
pub fn sector_span(
    dev: &(impl BlockDevice + ?Sized),
    lba: u64,
    count: u64,
) -> Result<(u64, usize)> {
    let total = dev.sector_count();
    let end = lba.checked_add(count);
    if count == 0 || end.is_none_or(|e| e > total) {
        return Err(IoError::InvalidRange { lba, count, total });
    }
    let ss = u128::from(dev.sector_size());
    let bytes = u128::from(count) * ss;
    if bytes > MAX_READ_REQUEST as u128 {
        return Err(IoError::RequestTooLarge {
            requested: bytes,
            max: MAX_READ_REQUEST,
        });
    }
    // lba < total y total × ss ≤ size ≤ u64::MAX: no puede desbordar.
    let offset = lba * u64::from(dev.sector_size());
    Ok((offset, bytes as usize))
}

pub(crate) fn check_bounds(offset: u64, len: usize, size: u64) -> Result<()> {
    let end = offset.checked_add(len as u64);
    match end {
        Some(e) if e <= size => Ok(()),
        _ => Err(IoError::OutOfBounds { offset, len, size }),
    }
}

/// Valida un tamaño de sector lógico.
pub fn validate_sector_size(s: u32) -> Result<()> {
    if (512..=65536).contains(&s) && s.is_power_of_two() {
        Ok(())
    } else {
        Err(IoError::InvalidSectorSize(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(sectors: usize, ss: u32) -> MemDevice {
        MemDevice::new(
            (0..sectors * ss as usize)
                .map(|i| (i / ss as usize) as u8)
                .collect(),
            ss,
        )
        .unwrap()
    }

    #[test]
    fn read_sectors_validates_strictly() {
        for ss in COMMON_SECTOR_SIZES {
            let d = dev(8, ss);
            let mut b = vec![0u8; 2 * ss as usize];
            d.read_sectors(3, 2, &mut b).unwrap();
            assert!(b[..ss as usize].iter().all(|&x| x == 3));
            assert!(b[ss as usize..].iter().all(|&x| x == 4));
            assert!(matches!(
                d.read_sectors(7, 2, &mut b),
                Err(IoError::InvalidRange { .. })
            ));
            assert!(matches!(
                d.read_sectors(0, 0, &mut []),
                Err(IoError::InvalidRange { .. })
            ));
            assert!(matches!(
                d.read_sectors(u64::MAX, 2, &mut b),
                Err(IoError::InvalidRange { .. })
            ));
            let mut wrong = vec![0u8; ss as usize];
            assert!(d.read_sectors(0, 2, &mut wrong).is_err());
        }
    }

    #[test]
    fn huge_requests_are_refused_before_allocating() {
        // Dispositivo "virtual" enorme sin datos: solo se usa para validar el rango.
        struct Huge;
        impl BlockDevice for Huge {
            fn size(&self) -> u64 {
                u64::MAX
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> Result<()> {
                unreachable!()
            }
        }
        assert!(matches!(
            sector_span(&Huge, 0, 1 << 40),
            Err(IoError::RequestTooLarge { .. })
        ));
        assert!(matches!(
            sector_span(&Huge, u64::MAX - 1, 1),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(sector_span(&Huge, 10, 16).is_ok());
    }

    #[test]
    fn sector_sizes() {
        for s in COMMON_SECTOR_SIZES {
            assert!(validate_sector_size(s).is_ok());
        }
        for s in [0, 256, 500, 513, 3072, 131072] {
            assert!(validate_sector_size(s).is_err());
        }
    }
}
