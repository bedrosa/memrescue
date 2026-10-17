//! Dispositivos virtuales sin almacenamiento: [`NullDevice`] (ceros) y [`PatternDevice`] (patrón
//! constante de 4 bytes).
//!
//! Sirven para representar miembros ausentes de un RAID virtual y para componer dispositivos en
//! pruebas sin reservar memoria: el tamaño puede ser cualquiera (hasta `u64::MAX` bytes) porque el
//! contenido se genera al leer. Ambos son de **solo lectura** y comprueban los límites de cada
//! lectura con aritmética comprobada.
//!
//! Asignación: [`NullDevice`] es **totalmente disperso** (`Allocation::Sparse`), de modo que el
//! lector tolerante entrega ceros con estado `Sparse` sin llegar a leer. [`PatternDevice`] solo es
//! disperso si su patrón es todo ceros; con cualquier otro patrón declara `Allocation::Data`,
//! porque un sector disperso se entrega como ceros y eso contradiría el contenido real.

use crate::{
    check_bounds, validate_sector_size, Allocation, AllocationRun, BlockDevice, IoError, Result,
};

/// Valida un rango de sectores frente a `total` con aritmética comprobada.
fn check_range(lba: u64, count: u64, total: u64) -> Result<()> {
    if count == 0 || lba.checked_add(count).is_none_or(|e| e > total) {
        return Err(IoError::InvalidRange { lba, count, total });
    }
    Ok(())
}

/// Dispositivo de solo lectura que devuelve un patrón de 4 bytes repetido.
///
/// El patrón está alineado con el **inicio del dispositivo** (el byte del desplazamiento `o` es
/// `pattern[o % 4]`), así que el contenido no depende de cómo se trocee la lectura.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternDevice {
    size: u64,
    sector_size: u32,
    pattern: [u8; 4],
}

impl PatternDevice {
    /// Crea un dispositivo de `size` bytes con sector lógico `sector_size` (validado con
    /// [`validate_sector_size`]). `size` no tiene por qué ser múltiplo del sector: los bytes
    /// finales se exponen como en cualquier otro dispositivo ([`BlockDevice::trailing_bytes`]).
    pub fn new(size: u64, sector_size: u32, pattern: [u8; 4]) -> Result<Self> {
        validate_sector_size(sector_size)?;
        Ok(Self {
            size,
            sector_size,
            pattern,
        })
    }

    /// Patrón que se repite.
    pub fn pattern(&self) -> [u8; 4] {
        self.pattern
    }
}

impl BlockDevice for PatternDevice {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(offset, buf.len(), self.size)?;
        // Patrón rotado para que el primer byte de `buf` corresponda a `offset`.
        let phase = (offset % 4) as usize;
        let mut rot = self.pattern;
        rot.rotate_left(phase);
        for chunk in buf.chunks_mut(4) {
            chunk.copy_from_slice(&rot[..chunk.len()]);
        }
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        check_range(lba, count, self.sector_count())?;
        let kind = if self.pattern == [0; 4] {
            Allocation::Sparse
        } else {
            Allocation::Data
        };
        Ok(vec![(lba, count, kind)])
    }
}

/// Dispositivo de solo lectura que devuelve ceros y cuyo mapa de asignación es totalmente disperso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NullDevice {
    size: u64,
    sector_size: u32,
}

impl NullDevice {
    /// Crea un dispositivo de `size` bytes con sector lógico `sector_size` (validado con
    /// [`validate_sector_size`]).
    pub fn new(size: u64, sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        Ok(Self { size, sector_size })
    }

    /// Crea un dispositivo de `sectors` sectores completos de `sector_size` bytes. Falla con
    /// [`IoError::InvalidRange`] si el tamaño en bytes no cabe en `u64`.
    pub fn with_sectors(sectors: u64, sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        let size = sectors
            .checked_mul(u64::from(sector_size))
            .ok_or(IoError::InvalidRange {
                lba: 0,
                count: sectors,
                total: u64::MAX / u64::from(sector_size),
            })?;
        Ok(Self { size, sector_size })
    }
}

impl BlockDevice for NullDevice {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(offset, buf.len(), self.size)?;
        buf.fill(0);
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        check_range(lba, count, self.sector_count())?;
        Ok(vec![(lba, count, Allocation::Sparse)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resilient::{read_range, OnError, ReadPolicy};
    use crate::{FaultyDevice, RangeView, SectorState, COMMON_SECTOR_SIZES, MAX_READ_REQUEST};
    use std::sync::Arc;

    #[test]
    fn rejects_bad_sector_size() {
        for s in [0, 256, 500, 3072, 131072] {
            assert!(matches!(
                NullDevice::new(4096, s),
                Err(IoError::InvalidSectorSize(_))
            ));
            assert!(NullDevice::with_sectors(8, s).is_err());
            assert!(PatternDevice::new(4096, s, *b"ABCD").is_err());
        }
        assert!(NullDevice::new(0, 65536).is_ok());
    }

    #[test]
    fn exact_limits_for_all_sector_sizes() {
        for ss in COMMON_SECTOR_SIZES {
            let d = NullDevice::with_sectors(8, ss).unwrap();
            let s = ss as usize;
            assert_eq!(d.size(), 8 * u64::from(ss));
            assert_eq!(d.sector_size(), ss);
            assert_eq!(d.sector_count(), 8);
            assert_eq!(d.trailing_bytes(), 0);
            let mut b = vec![0xEEu8; 2 * s];
            d.read_sectors(6, 2, &mut b).unwrap();
            assert!(b.iter().all(|&x| x == 0));
            assert!(matches!(
                d.read_sectors(7, 2, &mut b),
                Err(IoError::InvalidRange { .. })
            ));
            assert!(d.read_sectors(0, 0, &mut []).is_err());
            // Último byte exacto sí; uno más no.
            let mut one = [0xEEu8; 1];
            d.read_at(d.size() - 1, &mut one).unwrap();
            assert_eq!(one, [0]);
            assert!(matches!(
                d.read_at(d.size(), &mut one),
                Err(IoError::OutOfBounds { .. })
            ));
            // Una lectura vacía en el final es válida (como en el resto de dispositivos).
            d.read_at(d.size(), &mut []).unwrap();

            assert_eq!(
                d.allocation(0, 8).unwrap(),
                vec![(0, 8, Allocation::Sparse)]
            );
            assert_eq!(
                d.allocation(7, 1).unwrap(),
                vec![(7, 1, Allocation::Sparse)]
            );
            assert!(d.allocation(7, 2).is_err());
            assert!(d.allocation(8, 1).is_err());
            assert!(d.allocation(0, 0).is_err());
        }
    }

    #[test]
    fn trailing_bytes_are_readable_but_not_a_sector() {
        let d = NullDevice::new(4 * 512 + 100, 512).unwrap();
        assert_eq!((d.sector_count(), d.trailing_bytes()), (4, 100));
        let mut b = [0xEEu8; 100];
        d.read_at(4 * 512, &mut b).unwrap();
        assert!(b.iter().all(|&x| x == 0));
        assert!(d.allocation(4, 1).is_err());
    }

    #[test]
    fn overflow_is_rejected() {
        assert!(matches!(
            NullDevice::with_sectors(u64::MAX, 512),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(NullDevice::with_sectors(u64::MAX / 4096, 4096).is_ok());

        let d = NullDevice::new(u64::MAX, 4096).unwrap();
        let p = PatternDevice::new(u64::MAX, 4096, *b"ABCD").unwrap();
        let total = d.sector_count();
        assert_eq!(total, u64::MAX / 4096);
        let mut b = vec![0xEEu8; 16];
        for dev in [&d as &dyn BlockDevice, &p] {
            assert!(matches!(
                dev.read_at(u64::MAX, &mut b),
                Err(IoError::OutOfBounds { .. })
            ));
            assert!(dev.read_at(u64::MAX - 8, &mut b).is_err());
            dev.read_at(u64::MAX - 16, &mut b).unwrap();
            assert!(dev.read_sectors(u64::MAX, 1, &mut b).is_err());
            assert!(dev.allocation(u64::MAX, 2).is_err());
            assert!(dev.allocation(1, u64::MAX).is_err());
            assert!(dev.allocation(u64::MAX, u64::MAX).is_err());
            assert_eq!(dev.allocation(total - 1, 1).unwrap().len(), 1);
            // Petición enorme: se rechaza antes de reservar memoria.
            assert!(matches!(
                dev.read_sectors(0, total, &mut []),
                Err(IoError::RequestTooLarge { .. })
            ));
        }
        assert!(b[..16].iter().all(|&x| b"ABCD".contains(&x)));
    }

    #[test]
    fn pattern_is_aligned_to_device_offset() {
        let p = PatternDevice::new(4096, 512, *b"ABCD").unwrap();
        assert_eq!(p.pattern(), *b"ABCD");
        let mut b = [0u8; 7];
        p.read_at(0, &mut b).unwrap();
        assert_eq!(&b, b"ABCDABC");
        p.read_at(1, &mut b).unwrap();
        assert_eq!(&b, b"BCDABCD");
        p.read_at(4094, &mut b[..2]).unwrap();
        assert_eq!(&b[..2], b"CD");
        // Leer de golpe o a trozos da lo mismo.
        let mut whole = vec![0u8; 1000];
        p.read_at(13, &mut whole).unwrap();
        let mut pieces = vec![0u8; 1000];
        p.read_at(13, &mut pieces[..333]).unwrap();
        p.read_at(346, &mut pieces[333..]).unwrap();
        assert_eq!(whole, pieces);
        assert_eq!(p.allocation(0, 8).unwrap(), vec![(0, 8, Allocation::Data)]);
        // Patrón nulo: disperso como `NullDevice`.
        let z = PatternDevice::new(4096, 512, [0; 4]).unwrap();
        assert_eq!(
            z.allocation(2, 3).unwrap(),
            vec![(2, 3, Allocation::Sparse)]
        );
    }

    #[test]
    fn works_inside_range_view() {
        for ss in COMMON_SECTOR_SIZES {
            let d: Arc<dyn BlockDevice> = Arc::new(NullDevice::with_sectors(16, ss).unwrap());
            let v = RangeView::new(d.clone(), 4, 8).unwrap();
            assert_eq!(v.size(), 8 * u64::from(ss));
            assert_eq!(
                v.allocation(0, 8).unwrap(),
                vec![(0, 8, Allocation::Sparse)]
            );
            assert_eq!(
                v.allocation(5, 3).unwrap(),
                vec![(5, 3, Allocation::Sparse)]
            );
            let mut b = vec![0xEEu8; ss as usize];
            v.read_sectors(7, 1, &mut b).unwrap();
            assert!(b.iter().all(|&x| x == 0));
            assert!(v.read_sectors(8, 1, &mut b).is_err());
            assert!(RangeView::new(d, 15, 2).is_err());
        }
        // Patrón dentro de una vista: el desplazamiento se conserva respecto al interior.
        let p: Arc<dyn BlockDevice> = Arc::new(PatternDevice::new(8192, 512, *b"WXYZ").unwrap());
        let v = RangeView::new(p, 1, 2).unwrap();
        let mut b = [0u8; 4];
        v.read_at(1, &mut b).unwrap();
        assert_eq!(&b, b"XYZW");
    }

    /// Lectura completa con el lector tolerante: todo `Sparse`, ceros y ninguna lectura real.
    #[test]
    fn resilient_full_read_is_all_sparse_without_reads() {
        for ss in COMMON_SECTOR_SIZES {
            let sectors = (MAX_READ_REQUEST / ss as usize) as u64 / 64 + 37;
            // Los sectores "malos" demuestran que ni siquiera se intenta leer.
            let d = FaultyDevice::new(NullDevice::with_sectors(sectors, ss).unwrap()).with_bad([
                0,
                5,
                sectors - 1,
            ]);
            let policy = ReadPolicy {
                chunk_sectors: 64,
                on_error: OnError::Fail,
                ..ReadPolicy::default()
            };
            let mut next = 0u64;
            let mut nonzero = false;
            let report = read_range(
                &d,
                0,
                sectors,
                &policy,
                |lba, data, states| {
                    assert_eq!(lba, next);
                    assert_eq!(data.len(), states.len() * ss as usize);
                    assert!(states.iter().all(|&s| s == SectorState::Sparse));
                    nonzero |= data.iter().any(|&x| x != 0);
                    next += states.len() as u64;
                    Ok(())
                },
                |_, _| true,
            )
            .unwrap();
            assert_eq!(next, sectors);
            assert!(!nonzero);
            assert_eq!(report.map.count(SectorState::Sparse), sectors);
            assert_eq!(report.map.runs(), &[(0, sectors, SectorState::Sparse)]);
            assert_eq!(report.retry_reads, 0);
            assert_eq!(d.read_calls(), 0);
        }
    }

    #[test]
    fn resilient_read_of_pattern_reads_data() {
        let d = FaultyDevice::new(PatternDevice::new(32 * 1024, 1024, *b"PATN").unwrap());
        let mut all = Vec::new();
        let report = read_range(
            &d,
            0,
            32,
            &ReadPolicy::default(),
            |_, data, states| {
                assert!(states.iter().all(|&s| s == SectorState::Read));
                all.extend_from_slice(data);
                Ok(())
            },
            |_, _| true,
        )
        .unwrap();
        assert_eq!(report.map.count(SectorState::Read), 32);
        assert!(all.chunks(4).all(|c| c == b"PATN"));
        assert!(d.read_calls() > 0);
    }
}
