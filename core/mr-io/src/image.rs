use crate::{check_bounds, validate_sector_size, BlockDevice, IoError, Result};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// Imagen de disco sector a sector, abierta en **solo lectura**.
pub struct ImageFile {
    file: File,
    path: PathBuf,
    size: u64,
    sector_size: u32,
}

impl ImageFile {
    /// Abre `path` en solo lectura. Rechaza rutas de dispositivo (`\\.\`, `\\?\`, `\\?\GLOBALROOT`…).
    pub fn open(path: impl AsRef<Path>, sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        let path = path.as_ref();
        reject_device_path(path)?;
        let file = OpenOptions::new().read(true).write(false).open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(IoError::DevicePathRejected(path.display().to_string()));
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            size: meta.len(),
            sector_size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn reject_device_path(path: &Path) -> Result<()> {
    let s = path.as_os_str().to_string_lossy().replace('/', "\\");
    let lower = s.to_ascii_lowercase();
    let forbidden = lower.starts_with("\\\\.\\")
        || lower.starts_with("\\\\?\\globalroot")
        || lower.starts_with("\\\\?\\volume")
        || lower.starts_with("\\??\\")
        || lower.contains("physicaldrive")
        || lower.starts_with("/dev/");
    if forbidden {
        Err(IoError::DevicePathRejected(s))
    } else {
        Ok(())
    }
}

impl BlockDevice for ImageFile {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(offset, buf.len(), self.size)?;
        read_exact_at(&self.file, offset, buf)
    }
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut offset: u64, mut buf: &mut [u8]) -> Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_read(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    use std::os::unix::fs::FileExt;
    Ok(file.read_exact_at(buf, offset)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn opens_reads_and_never_writes() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 256) as u8).collect();
        tmp.write_all(&data).unwrap();
        tmp.flush().unwrap();
        let img = ImageFile::open(tmp.path(), 512).unwrap();
        assert_eq!(img.size(), 4096);
        assert_eq!(img.sector_count(), 8);
        let mut b = vec![0u8; 1000];
        img.read_at(3000, &mut b).unwrap();
        assert_eq!(&b[..], &data[3000..4000]);
        assert!(img.read_at(4000, &mut [0u8; 200]).is_err());
    }

    #[test]
    fn rejects_device_namespace_paths() {
        for p in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}",
            "//./physicaldrive1",
        ] {
            assert!(
                matches!(ImageFile::open(p, 512), Err(IoError::DevicePathRejected(_))),
                "no rechazó {p}"
            );
        }
    }

    #[test]
    fn rejects_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ImageFile::open(dir.path(), 512).is_err());
    }
}
