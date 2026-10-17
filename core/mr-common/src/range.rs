//! Direcciones de sector y rangos de bytes con aritmética comprobada.

use std::fmt;

/// Dirección lógica de bloque (número de sector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Lba(pub u64);

impl Lba {
    /// Desplazamiento en bytes de este sector, o `None` si desborda.
    pub fn byte_offset(self, sector_size: u32) -> Option<u64> {
        self.0.checked_mul(u64::from(sector_size))
    }
}

impl fmt::Display for Lba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LBA {}", self.0)
    }
}

/// Rango semiabierto `[start, end)` en bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Crea un rango; devuelve `None` si `end < start`.
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (end >= start).then_some(Self { start, end })
    }

    /// Crea un rango a partir de inicio y longitud; `None` si desborda.
    pub fn from_len(start: u64, len: u64) -> Option<Self> {
        start.checked_add(len).map(|end| Self { start, end })
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// `true` si el rango está completamente contenido en `[0, total)`.
    pub fn fits_within(&self, total: u64) -> bool {
        self.end <= total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lba_offset_checks_overflow() {
        assert_eq!(Lba(2).byte_offset(512), Some(1024));
        assert_eq!(Lba(u64::MAX).byte_offset(512), None);
    }

    #[test]
    fn range_rejects_inverted_and_overflow() {
        assert!(ByteRange::new(10, 5).is_none());
        assert!(ByteRange::from_len(u64::MAX, 1).is_none());
        let r = ByteRange::from_len(512, 1024).unwrap();
        assert_eq!(r.len(), 1024);
        assert!(r.fits_within(1536));
        assert!(!r.fits_within(1535));
    }
}
