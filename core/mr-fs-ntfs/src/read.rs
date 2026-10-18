//! Lectura de tramos de bytes del volumen con el mapa de validez (ADR-019).
//!
//! Las estructuras NTFS se direccionan en bytes (clústeres, registros de 1 KiB...) y el origen en
//! sectores. Toda lectura pasa por `mr_part::sector::read_trusted`: un sector ilegible o saltado
//! nunca se entrega como dato; aquí se traduce a los bytes del tramo que cubre y a su LBA, para
//! que quien lee un registro sepa que no puede interpretarlo.

use mr_io::{BlockDevice, IoError};
use mr_part::sector::{read_trusted, MAX_TRUSTED_SECTORS};

/// Tramo leído: bytes y sectores no fiables que lo tocan.
#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub data: Vec<u8>,
    /// `(lba, desde, hasta)`: sector no fiable y los bytes del tramo que cubre.
    pub untrusted: Vec<(u64, usize, usize)>,
}

impl Span {
    /// LBA de los sectores no fiables que tocan `[from, to)` del tramo.
    pub fn untrusted_in(&self, from: usize, to: usize) -> Vec<u64> {
        self.untrusted
            .iter()
            .filter(|&&(_, a, b)| a < to && from < b)
            .map(|&(lba, _, _)| lba)
            .collect()
    }
}

/// Lee `len` bytes desde el byte `offset` del origen. Error si el tramo se sale del origen.
pub(crate) fn read_span(
    dev: &(impl BlockDevice + ?Sized),
    offset: u64,
    len: usize,
    retries: u32,
) -> Result<Span, IoError> {
    let size = dev.sector_count() * u64::from(dev.sector_size());
    let end = offset
        .checked_add(len as u64)
        .filter(|&e| e <= size && len > 0)
        .ok_or(IoError::OutOfBounds { offset, len, size })?;
    let ss = u64::from(dev.sector_size());
    let first = offset / ss;
    let last = (end - 1) / ss;
    let mut data = Vec::with_capacity(len);
    let mut untrusted = Vec::new();
    let mut lba = first;
    while lba <= last {
        let count = (last - lba + 1).min(MAX_TRUSTED_SECTORS);
        let t = read_trusted(dev, lba, count, retries)?;
        for l in lba..lba + count {
            let s_start = l * ss;
            let from = s_start.max(offset);
            let to = (s_start + ss).min(end);
            let a = (from - offset) as usize;
            let b = (to - offset) as usize;
            match t.sector(l) {
                Some(bytes) => {
                    let lo = (from - s_start) as usize;
                    let hi = (to - s_start) as usize;
                    data.extend_from_slice(&bytes[lo..hi]);
                }
                None => {
                    data.resize(data.len() + (b - a), 0);
                    untrusted.push((l, a, b));
                }
            }
        }
        lba += count;
    }
    Ok(Span { data, untrusted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice};

    #[test]
    fn spans_report_untrusted_bytes_and_never_return_fill() {
        let data: Vec<u8> = (0..8 * 4096u64).map(|i| (i / 512) as u8).collect();
        let dev = FaultyDevice::new(MemDevice::new(data.clone(), 4096).unwrap()).with_bad([2]);
        // Tramo dentro de un sector de 4096 B legible.
        let s = read_span(&dev, 100, 1024, 0).unwrap();
        assert_eq!(s.data, &data[100..1124]);
        assert!(s.untrusted.is_empty());
        // Tramo que cruza el sector ilegible: sus bytes quedan a cero y se informan.
        let s = read_span(&dev, 4096 + 2048, 8192, 0).unwrap();
        assert_eq!(s.data.len(), 8192);
        assert_eq!(s.untrusted, vec![(2, 2048, 6144)]);
        assert!(s.data[2048..6144].iter().all(|&b| b == 0));
        assert_eq!(&s.data[..2048], &data[6144..8192]);
        assert_eq!(&s.data[6144..], &data[12288..14336]);
        assert_eq!(s.untrusted_in(0, 2048), Vec::<u64>::new());
        assert_eq!(s.untrusted_in(2000, 2100), vec![2]);
        // Fuera del origen o vacío.
        assert!(read_span(&dev, 8 * 4096 - 10, 11, 0).is_err());
        assert!(read_span(&dev, u64::MAX, 1, 0).is_err());
        assert!(read_span(&dev, 0, 0, 0).is_err());
    }
}
