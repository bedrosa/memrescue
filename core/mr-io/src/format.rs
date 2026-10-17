//! Detección del formato de un fichero de imagen por sus firmas públicas.
//!
//! Referencias (especificaciones públicas):
//! - VHD: Microsoft "Virtual Hard Disk Image Format Specification", pie `conectix` en los últimos
//!   512 bytes (511 en versiones antiguas); los VHD dinámicos llevan además una copia en el byte 0.
//! - VHDX: [MS-VHDX], identificador de tipo de fichero `vhdxfile` en el byte 0.
//! - VMDK: VMware "Virtual Disk Format 1.1", cabecera de extensión dispersa `KDMV`, `COWD` (ESX),
//!   o fichero descriptor de texto que empieza por `# Disk DescriptorFile`.
//! - EWF (E01/L01/Ex01): firmas `EVF\x09\x0d\x0a\xff\x00`, `LVF…`, `EVF2\r\n\x81\x00`.
//! - QCOW/QCOW2: `QFI\xfb`. VDI: firma 0xBEDA107F en el desplazamiento 0x40. DMG (UDIF): `koly`
//!   en el pie de 512 bytes.
//!
//! Punto de extensión: cada formato que se implemente añadirá su lector `BlockDevice`; mientras no
//! exista, abrirlo como imagen devuelve [`crate::IoError::UnsupportedFormat`].

use crate::image::read_exact_at;
use crate::path_policy::{admit, open_regular_file};
use crate::Result;
use std::fmt;
use std::fs::File;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ImageFormat {
    /// Volcado sector a sector sin cabeceras (RAW, IMG, DD, BIN).
    Raw = 1,
    Vhd = 2,
    Vhdx = 3,
    Vmdk = 4,
    VmdkDescriptor = 5,
    Ewf = 6,
    Qcow = 7,
    Vdi = 8,
    Dmg = 9,
}

impl ImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Raw => "RAW (sector a sector)",
            ImageFormat::Vhd => "VHD",
            ImageFormat::Vhdx => "VHDX",
            ImageFormat::Vmdk => "VMDK (extensión dispersa)",
            ImageFormat::VmdkDescriptor => "VMDK (descriptor)",
            ImageFormat::Ewf => "EWF (E01/L01/Ex01)",
            ImageFormat::Qcow => "QCOW",
            ImageFormat::Vdi => "VDI",
            ImageFormat::Dmg => "DMG (UDIF)",
        }
    }

    pub fn code(self) -> u32 {
        self as u32
    }

    /// `true` si MemRescue sabe leer este formato en la versión actual.
    pub fn is_supported(self) -> bool {
        matches!(self, ImageFormat::Raw)
    }
}

impl fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Clasifica a partir de los primeros y los últimos 512 bytes (o menos si el fichero es menor).
pub fn classify(head: &[u8], tail: &[u8]) -> ImageFormat {
    let starts = |m: &[u8]| head.len() >= m.len() && &head[..m.len()] == m;
    if starts(b"vhdxfile") {
        return ImageFormat::Vhdx;
    }
    if starts(b"conectix") {
        return ImageFormat::Vhd;
    }
    if starts(b"KDMV") || starts(b"COWD") {
        return ImageFormat::Vmdk;
    }
    if starts(b"# Disk DescriptorFile") {
        return ImageFormat::VmdkDescriptor;
    }
    if starts(b"EVF\x09\x0d\x0a\xff\x00")
        || starts(b"LVF\x09\x0d\x0a\xff\x00")
        || starts(b"EVF2\r\n\x81\x00")
    {
        return ImageFormat::Ewf;
    }
    if starts(b"QFI\xfb") {
        return ImageFormat::Qcow;
    }
    if head.len() >= 0x44 && head[0x40..0x44] == 0xBEDA_107Fu32.to_le_bytes() {
        return ImageFormat::Vdi;
    }
    // Pies: VHD (512 o 511 bytes) y DMG (512 bytes).
    if tail.len() >= 512 && &tail[tail.len() - 512..tail.len() - 504] == b"conectix" {
        return ImageFormat::Vhd;
    }
    if tail.len() >= 511 && &tail[tail.len() - 511..tail.len() - 503] == b"conectix" {
        return ImageFormat::Vhd;
    }
    if tail.len() >= 512 && &tail[tail.len() - 512..tail.len() - 508] == b"koly" {
        return ImageFormat::Dmg;
    }
    ImageFormat::Raw
}

pub(crate) fn probe_file(file: &File, size: u64) -> Result<ImageFormat> {
    let n = size.min(512) as usize;
    let mut head = vec![0u8; n];
    read_exact_at(file, 0, &mut head)?;
    let mut tail = vec![0u8; n];
    read_exact_at(file, size - n as u64, &mut tail)?;
    Ok(classify(&head, &tail))
}

/// Detecta el formato de un fichero (aplicando la misma política de rutas que la apertura).
pub fn probe_format(path: impl AsRef<Path>) -> Result<ImageFormat> {
    let canon = admit(path.as_ref())?;
    let file = open_regular_file(&canon)?;
    let size = file.metadata()?.len();
    probe_file(&file, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_head(m: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 1024];
        v[..m.len()].copy_from_slice(m);
        v
    }

    #[test]
    fn detects_each_signature() {
        let cases: Vec<(Vec<u8>, ImageFormat)> = vec![
            (with_head(b"vhdxfile"), ImageFormat::Vhdx),
            (with_head(b"conectix"), ImageFormat::Vhd),
            (with_head(b"KDMV"), ImageFormat::Vmdk),
            (with_head(b"COWD"), ImageFormat::Vmdk),
            (
                with_head(b"# Disk DescriptorFile\n"),
                ImageFormat::VmdkDescriptor,
            ),
            (with_head(b"EVF\x09\x0d\x0a\xff\x00"), ImageFormat::Ewf),
            (with_head(b"LVF\x09\x0d\x0a\xff\x00"), ImageFormat::Ewf),
            (with_head(b"EVF2\r\n\x81\x00"), ImageFormat::Ewf),
            (with_head(b"QFI\xfb"), ImageFormat::Qcow),
            (vec![0u8; 1024], ImageFormat::Raw),
        ];
        for (d, f) in cases {
            assert_eq!(classify(&d[..512], &d[512..]), f);
        }
        let mut vdi = vec![0u8; 1024];
        vdi[0x40..0x44].copy_from_slice(&0xBEDA_107Fu32.to_le_bytes());
        assert_eq!(classify(&vdi[..512], &vdi[512..]), ImageFormat::Vdi);
    }

    #[test]
    fn detects_footers_fixed_vhd_old_vhd_and_dmg() {
        let mut fixed = vec![0u8; 512];
        fixed[..8].copy_from_slice(b"conectix");
        assert_eq!(classify(&[0u8; 512], &fixed), ImageFormat::Vhd);
        let mut old = vec![0u8; 512];
        old[1..9].copy_from_slice(b"conectix");
        assert_eq!(classify(&[0u8; 512], &old), ImageFormat::Vhd);
        let mut dmg = vec![0u8; 512];
        dmg[..4].copy_from_slice(b"koly");
        assert_eq!(classify(&[0u8; 512], &dmg), ImageFormat::Dmg);
    }

    #[test]
    fn mbr_and_gpt_disks_are_raw() {
        let mut mbr = vec![0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        assert_eq!(classify(&mbr, &[0u8; 512]), ImageFormat::Raw);
    }

    #[test]
    fn tiny_and_empty_files() {
        assert_eq!(classify(&[], &[]), ImageFormat::Raw);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"conec").unwrap();
        assert_eq!(probe_format(tmp.path()).unwrap(), ImageFormat::Raw);
        std::fs::write(tmp.path(), b"").unwrap();
        assert_eq!(probe_format(tmp.path()).unwrap(), ImageFormat::Raw);
    }
}
