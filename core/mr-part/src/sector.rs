//! Lectura de sectores para los parsers de tablas, con el mapa de validez (ADR-019).
//!
//! Los parsers nunca leen con `read_at` directamente: usan [`read_trusted`], que no aborta ante
//! sectores dañados y solo entrega el contenido de los sectores fiables. Un sector ilegible o
//! saltado contiene un patrón de relleno y **no** se devuelve, así que no puede confundirse con una
//! firma ni con una tabla.

use mr_io::resilient::{read_sectors_mapped, OnError, ReadPolicy};
use mr_io::{BlockDevice, Result, SectorState};

/// Máximo de sectores por lectura de [`read_trusted`] (las tablas GPT ocupan como mucho unos
/// pocos MiB; los parsers leen por trozos si necesitan más).
pub const MAX_TRUSTED_SECTORS: u64 = 8192;

/// Sectores leídos con su estado.
#[derive(Debug, Clone)]
pub struct TrustedSectors {
    first_lba: u64,
    sector_size: usize,
    data: Vec<u8>,
    states: Vec<SectorState>,
}

impl TrustedSectors {
    pub fn first_lba(&self) -> u64 {
        self.first_lba
    }

    pub fn len(&self) -> u64 {
        self.states.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Contenido del sector `lba` si es fiable (leído, recuperado o disperso); `None` si es
    /// ilegible, saltado o está fuera de lo leído.
    pub fn sector(&self, lba: u64) -> Option<&[u8]> {
        let i = usize::try_from(lba.checked_sub(self.first_lba)?).ok()?;
        let state = *self.states.get(i)?;
        if !state.is_trustworthy() {
            return None;
        }
        let at = i * self.sector_size;
        self.data.get(at..at + self.sector_size)
    }

    /// Contenido de todo el tramo si **todos** sus sectores son fiables.
    pub fn all(&self) -> Option<&[u8]> {
        self.states
            .iter()
            .all(|s| s.is_trustworthy())
            .then_some(self.data.as_slice())
    }

    /// LBA de los sectores no fiables del tramo.
    pub fn untrusted(&self) -> impl Iterator<Item = u64> + '_ {
        self.states
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_trustworthy())
            .map(|(i, _)| self.first_lba + i as u64)
    }
}

/// Lee `[lba, lba + count)` sin abortar ante sectores dañados (`FillAndContinue`, `retries`
/// reintentos). Rango fuera del origen o `count` mayor que [`MAX_TRUSTED_SECTORS`] → error.
pub fn read_trusted(
    dev: &(impl BlockDevice + ?Sized),
    lba: u64,
    count: u64,
    retries: u32,
) -> Result<TrustedSectors> {
    if count > MAX_TRUSTED_SECTORS {
        return Err(mr_io::IoError::InvalidRange {
            lba,
            count,
            total: dev.sector_count(),
        });
    }
    let policy = ReadPolicy {
        on_error: OnError::FillAndContinue,
        retries,
        ..ReadPolicy::default()
    };
    let (data, states) = read_sectors_mapped(dev, lba, count, &policy)?;
    Ok(TrustedSectors {
        first_lba: lba,
        sector_size: dev.sector_size() as usize,
        data,
        states,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice};

    fn dev() -> FaultyDevice<MemDevice> {
        let data: Vec<u8> = (0..16 * 512u64).map(|i| (i / 512) as u8 + 1).collect();
        FaultyDevice::new(MemDevice::new(data, 512).unwrap())
            .with_bad([3])
            .with_sparse(10, 2)
    }

    #[test]
    fn unreadable_sectors_are_never_returned() {
        let d = dev();
        let t = read_trusted(&d, 0, 16, 1).unwrap();
        assert_eq!(t.len(), 16);
        assert_eq!(t.sector(2).unwrap(), &[3u8; 512][..]);
        assert!(t.sector(3).is_none());
        assert!(t.all().is_none());
        assert_eq!(t.untrusted().collect::<Vec<_>>(), vec![3]);
        // Los dispersos son ceros legítimos.
        assert_eq!(t.sector(10).unwrap(), &[0u8; 512][..]);
        assert!(t.sector(16).is_none());
        let t = read_trusted(&d, 4, 4, 0).unwrap();
        assert!(t.sector(3).is_none());
        assert_eq!(t.all().unwrap().len(), 4 * 512);
    }

    #[test]
    fn rejects_out_of_range_and_oversized_reads() {
        let d = dev();
        assert!(read_trusted(&d, 15, 2, 0).is_err());
        assert!(read_trusted(&d, 0, MAX_TRUSTED_SECTORS + 1, 0).is_err());
    }
}
