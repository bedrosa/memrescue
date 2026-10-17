//! Capa de bloques: abstracción común sobre discos, imágenes y dispositivos virtuales.
//!
//! En M0 solo existen dos implementaciones, ambas de solo lectura:
//! - [`ImageFile`]: fichero de imagen sector a sector.
//! - [`MemDevice`]: búfer en memoria para pruebas.
//!
//! El acceso a dispositivos físicos está **prohibido** en esta fase: [`ImageFile::open`] rechaza
//! las rutas del espacio de nombres de dispositivos de Windows.

mod image;
mod mem;

pub use image::ImageFile;
pub use mem::MemDevice;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("E/S: {0}")]
    Io(#[from] io::Error),
    #[error("lectura fuera de límites: desplazamiento {offset}, longitud {len}, tamaño {size}")]
    OutOfBounds { offset: u64, len: usize, size: u64 },
    #[error("ruta no permitida en esta fase (acceso a dispositivos deshabilitado): {0}")]
    DevicePathRejected(String),
    #[error("tamaño de sector no válido: {0}")]
    InvalidSectorSize(u32),
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
}

pub(crate) fn check_bounds(offset: u64, len: usize, size: u64) -> Result<()> {
    let end = offset.checked_add(len as u64);
    match end {
        Some(e) if e <= size => Ok(()),
        _ => Err(IoError::OutOfBounds { offset, len, size }),
    }
}

pub(crate) fn validate_sector_size(s: u32) -> Result<()> {
    if (512..=65536).contains(&s) && s.is_power_of_two() {
        Ok(())
    } else {
        Err(IoError::InvalidSectorSize(s))
    }
}
