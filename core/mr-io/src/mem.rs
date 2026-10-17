use crate::{check_bounds, validate_sector_size, BlockDevice, Result};

/// Dispositivo en memoria (para pruebas).
pub struct MemDevice {
    data: Vec<u8>,
    sector_size: u32,
}

impl MemDevice {
    pub fn new(data: Vec<u8>, sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        Ok(Self { data, sector_size })
    }
}

impl BlockDevice for MemDevice {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(offset, buf.len(), self.size())?;
        let start = offset as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IoError;

    #[test]
    fn reads_and_rejects_out_of_bounds() {
        let d = MemDevice::new((0..=255u8).cycle().take(2048).collect(), 512).unwrap();
        assert_eq!(d.sector_count(), 4);
        let mut b = [0u8; 4];
        d.read_at(510, &mut b).unwrap();
        assert_eq!(b, [254, 255, 0, 1]);
        assert!(matches!(
            d.read_at(2046, &mut b),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            d.read_at(u64::MAX, &mut b),
            Err(IoError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn rejects_bad_sector_size() {
        assert!(MemDevice::new(vec![], 500).is_err());
        assert!(MemDevice::new(vec![], 4096).is_ok());
    }
}
