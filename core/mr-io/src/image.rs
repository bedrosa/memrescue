use crate::format::{probe_file, ImageFormat};
use crate::path_policy::{admit, open_regular_file};
use crate::{check_bounds, validate_sector_size, BlockDevice, IoError, Result};
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Imagen de disco sector a sector (RAW/IMG/DD), abierta en **solo lectura**.
pub struct ImageFile {
    file: File,
    path: PathBuf,
    size: u64,
    sector_size: u32,
}

impl ImageFile {
    /// Abre `path` en solo lectura tras pasar la política de rutas ([`crate::path_policy`]).
    /// Si el fichero es un contenedor conocido (VHD, VHDX, VMDK…) se rechaza con
    /// [`IoError::UnsupportedFormat`] para no interpretarlo como un disco RAW.
    pub fn open(path: impl AsRef<Path>, sector_size: u32) -> Result<Self> {
        Self::open_inner(path.as_ref(), sector_size, false)
    }

    /// Como [`ImageFile::open`] pero sin comprobar el formato: el usuario declara que el fichero es
    /// un volcado sector a sector aunque parezca un contenedor.
    pub fn open_as_raw(path: impl AsRef<Path>, sector_size: u32) -> Result<Self> {
        Self::open_inner(path.as_ref(), sector_size, true)
    }

    fn open_inner(path: &Path, sector_size: u32, force_raw: bool) -> Result<Self> {
        validate_sector_size(sector_size)?;
        let canon = admit(path)?;
        let file = open_regular_file(&canon)?;
        let size = file.metadata()?.len();
        if !force_raw {
            let fmt = probe_file(&file, size)?;
            if fmt != ImageFormat::Raw {
                return Err(IoError::UnsupportedFormat(fmt));
            }
        }
        Ok(Self {
            file,
            path: canon,
            size,
            sector_size,
        })
    }

    /// Ruta canónica del fichero abierto.
    pub fn path(&self) -> &Path {
        &self.path
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
        read_exact_at(&self.file, offset, buf).map_err(|source| IoError::Read {
            offset,
            len: buf.len(),
            source,
        })
    }
}

/// Lectura posicional completa. Reintenta `Interrupted`; un fichero que se ha acortado desde que
/// se abrió produce `UnexpectedEof` (nunca datos inventados).
pub(crate) fn read_exact_at(
    file: &File,
    mut offset: u64,
    mut buf: &mut [u8],
) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let r = std::os::windows::fs::FileExt::seek_read(file, buf, offset);
        #[cfg(unix)]
        let r = std::os::unix::fs::FileExt::read_at(file, buf, offset);
        match r {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
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
        let before = std::fs::metadata(tmp.path()).unwrap().modified().unwrap();
        let img = ImageFile::open(tmp.path(), 512).unwrap();
        assert_eq!(img.size(), 4096);
        assert_eq!(img.sector_count(), 8);
        let mut b = vec![0u8; 1000];
        img.read_at(3000, &mut b).unwrap();
        assert_eq!(&b[..], &data[3000..4000]);
        assert!(img.read_at(4000, &mut [0u8; 200]).is_err());
        drop(img);
        assert_eq!(std::fs::read(tmp.path()).unwrap(), data);
        assert_eq!(
            std::fs::metadata(tmp.path()).unwrap().modified().unwrap(),
            before
        );
    }

    #[test]
    fn rejects_device_namespace_paths_without_opening() {
        for p in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:",
            r"\\?\C:",
            r"\\?\HarddiskVolume1",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}",
            "//./physicaldrive1",
            "NUL",
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
        assert!(matches!(
            ImageFile::open(dir.path(), 512),
            Err(IoError::DevicePathRejected(_))
        ));
    }

    #[test]
    fn truncated_file_reports_offset_instead_of_garbage() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), vec![9u8; 4096]).unwrap();
        let img = ImageFile::open(tmp.path(), 512).unwrap();
        // Otro proceso acorta el fichero después de abrirlo.
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .unwrap()
            .set_len(1024)
            .unwrap();
        let mut b = [0u8; 512];
        match img.read_at(2048, &mut b) {
            Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (2048, 512)),
            other => panic!("se esperaba error de lectura con contexto, no {other:?}"),
        }
    }

    #[test]
    fn container_formats_are_not_opened_as_raw_unless_forced() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut d = vec![0u8; 4096];
        d[..8].copy_from_slice(b"vhdxfile");
        std::fs::write(tmp.path(), &d).unwrap();
        assert!(matches!(
            ImageFile::open(tmp.path(), 512),
            Err(IoError::UnsupportedFormat(ImageFormat::Vhdx))
        ));
        assert_eq!(
            ImageFile::open_as_raw(tmp.path(), 512).unwrap().size(),
            4096
        );
    }
}
