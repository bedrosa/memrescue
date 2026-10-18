//! Reconocedores de sistemas de archivos de la búsqueda exprés (INV-05b §8.6 y §8.7).
//!
//! Cada reconocedor interpreta **un** sector ya fiable (nunca un relleno: el lector solo entrega
//! sectores leídos, recuperados o dispersos) y devuelve los campos que la búsqueda necesita: tamaño
//! de sector propio, extensión, posiciones de sus copias, inicio declarado y número de serie. Las
//! reglas salen de especificaciones públicas:
//! - NTFS: estructura pública del sector de arranque (BPB con campos FAT a cero, total de sectores en
//!   0x28 que excluye la copia del último sector, LCN de `$MFT` y `$MFTMirr`).
//! - FAT12/16/32: Microsoft *FAT32 File System Specification* (fatgen103), «Boot Sector and BPB» y
//!   el cálculo del tipo de FAT por el número de clústeres.
//! - exFAT: Microsoft *exFAT file system specification* §3.1 (región de arranque de 12 sectores,
//!   suma de comprobación del sector 11 y región de respaldo en el sector 12).
//!
//! Añadir otro sistema de archivos (ext, XFS, HFS+, APFS, Btrfs, ReFS en hitos posteriores) es
//! implementar [`Recognizer`] y registrarlo en [`RECOGNIZERS`]: la búsqueda no cambia.

use super::FsKind;

/// Campos de un sector de arranque o superbloque coherente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Boot {
    pub fs: FsKind,
    /// Bytes por sector declarados por la propia estructura.
    pub sector_size: u32,
    /// Extensión del volumen en bytes si no se confirma ninguna copia (la de la primera copia).
    pub length: u64,
    /// Copias posibles en orden de preferencia: `(desplazamiento desde el inicio del volumen,
    /// extensión del volumen si la copia está ahí)`, en bytes.
    pub backups: Vec<(u64, u64)>,
    /// Inicio declarado por la estructura en sus propios sectores (0 = no declarado).
    pub declared_start: u64,
    pub serial: u64,
    /// Primeros 512 bytes, para comparar copias.
    pub raw: Vec<u8>,
}

/// Reconocedor de un sistema de archivos. Implementaciones sin estado y deterministas.
pub(crate) trait Recognizer: Sync {
    /// Bit de familia (`super::family::*`) que lo activa.
    fn family(&self) -> u32;
    /// Desplazamiento de la estructura principal desde el inicio del volumen (0 en los sectores de
    /// arranque; 1024 en superbloques como los de ext o HFS+).
    fn offset(&self) -> u64 {
        0
    }
    /// Interpreta los primeros 512 bytes de `sector` como estructura principal o copia.
    fn parse(&self, sector: &[u8]) -> Option<Boot>;
    /// Bytes de una copia completa desde su posición (exFAT: la región de arranque de 12 sectores).
    fn copy_len(&self, _boot: &Boot) -> u64 {
        512
    }
    /// Comprueba la copia completa (`region` mide [`Recognizer::copy_len`] bytes).
    fn copy_ok(&self, _boot: &Boot, _region: &[u8]) -> bool {
        true
    }
    /// `true` si dos copias describen el mismo volumen (mismos campos clave).
    fn consistent(&self, a: &Boot, b: &Boot) -> bool;
    /// Estructura básica que confirma el volumen: `(desplazamiento desde el inicio, bytes)`.
    fn structures(&self, boot: &Boot) -> Option<(u64, u64)>;
    /// Comprueba la estructura básica leída.
    fn structures_ok(&self, boot: &Boot, data: &[u8]) -> bool;
}

/// Reconocedores en el orden en que se prueban (determinista).
pub(crate) static RECOGNIZERS: [&dyn Recognizer; 3] = [&Ntfs, &ExFat, &Fat];

fn le16(s: &[u8], o: usize) -> u64 {
    u64::from(u16::from_le_bytes([s[o], s[o + 1]]))
}

fn le32(s: &[u8], o: usize) -> u64 {
    u64::from(u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]))
}

fn le64(s: &[u8], o: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[o..o + 8]);
    u64::from_le_bytes(b)
}

/// Salto x86 de un sector de arranque (`EB xx 90` o `E9 xx xx`).
fn jump_ok(s: &[u8]) -> bool {
    (s[0] == 0xEB && s[2] == 0x90) || s[0] == 0xE9
}

fn signature_ok(s: &[u8]) -> bool {
    s[0x1FE] == 0x55 && s[0x1FF] == 0xAA
}

fn sector_size_ok(bps: u64) -> bool {
    matches!(bps, 512 | 1024 | 2048 | 4096)
}

// ---------------------------------------------------------------------------------------------
// NTFS
// ---------------------------------------------------------------------------------------------

pub(crate) struct Ntfs;

impl Ntfs {
    /// Tamaño en bytes de un registro (MFT o índice) codificado en el BPB: positivo = clústeres;
    /// negativo = potencia de dos de bytes.
    fn record_bytes(raw: u8, cluster: u64) -> Option<u64> {
        let v = raw as i8;
        match v {
            1..=64 => cluster.checked_mul(v as u64),
            -31..=-9 => Some(1u64 << (-v) as u32),
            _ => None,
        }
    }

    fn cluster_sectors(raw: u8) -> Option<u64> {
        match raw {
            1..=128 if raw.is_power_of_two() => Some(u64::from(raw)),
            // Clústeres grandes: 2^(256 - valor) sectores.
            0xF4..=0xFF => Some(1u64 << (256 - u32::from(raw))),
            _ => None,
        }
    }

    fn mft_record(boot: &Boot) -> Option<(u64, u64)> {
        let s = &boot.raw;
        let spc = Self::cluster_sectors(s[0x0D])?;
        let cluster = spc * u64::from(boot.sector_size);
        let rec = Self::record_bytes(s[0x40], cluster)?.min(4096);
        Some((le64(s, 0x30).checked_mul(cluster)?, rec))
    }
}

impl Recognizer for Ntfs {
    fn family(&self) -> u32 {
        super::family::NTFS
    }

    fn parse(&self, s: &[u8]) -> Option<Boot> {
        if s.len() < 512 || !jump_ok(s) || &s[3..11] != b"NTFS    " || !signature_ok(s) {
            return None;
        }
        let bps = le16(s, 0x0B);
        let spc = Self::cluster_sectors(s[0x0D])?;
        // Campos heredados de FAT que NTFS exige a cero.
        let zero = le16(s, 0x0E) == 0
            && s[0x10] == 0
            && le16(s, 0x11) == 0
            && le16(s, 0x13) == 0
            && le16(s, 0x16) == 0
            && le32(s, 0x20) == 0;
        if !sector_size_ok(bps) || !zero || spc.checked_mul(bps)? > 2 * 1024 * 1024 {
            return None;
        }
        let total = le64(s, 0x28);
        let clusters = total / spc;
        let (mft, mirr) = (le64(s, 0x30), le64(s, 0x38));
        if total == 0 || clusters == 0 || mft >= clusters || mirr >= clusters {
            return None;
        }
        Self::record_bytes(s[0x40], spc * bps)?;
        Self::record_bytes(s[0x44], spc * bps)?;
        // La copia está en el último sector del volumen, que el total no incluye (variante
        // estándar) o un sector más allá (variante +1, INV-05b §4).
        let at = total.checked_mul(bps)?;
        let len = at.checked_add(bps)?;
        let at1 = len;
        let len1 = at1.checked_add(bps)?;
        Some(Boot {
            fs: FsKind::Ntfs,
            sector_size: bps as u32,
            length: len,
            backups: vec![(at, len), (at1, len1)],
            declared_start: le32(s, 0x1C),
            serial: le64(s, 0x48),
            raw: s[..512].to_vec(),
        })
    }

    fn consistent(&self, a: &Boot, b: &Boot) -> bool {
        a.raw[0x0B..0x50] == b.raw[0x0B..0x50]
    }

    fn structures(&self, boot: &Boot) -> Option<(u64, u64)> {
        Self::mft_record(boot)
    }

    fn structures_ok(&self, _boot: &Boot, data: &[u8]) -> bool {
        data.starts_with(b"FILE")
    }
}

// ---------------------------------------------------------------------------------------------
// FAT12/16/32
// ---------------------------------------------------------------------------------------------

pub(crate) struct Fat;

impl Recognizer for Fat {
    fn family(&self) -> u32 {
        super::family::FAT
    }

    fn parse(&self, s: &[u8]) -> Option<Boot> {
        if s.len() < 512 || !jump_ok(s) || !signature_ok(s) {
            return None;
        }
        let bps = le16(s, 0x0B);
        let spc = u64::from(s[0x0D]);
        let rsvd = le16(s, 0x0E);
        let nfats = u64::from(s[0x10]);
        let root_ent = le16(s, 0x11);
        let tot16 = le16(s, 0x13);
        let media = s[0x15];
        let fatsz16 = le16(s, 0x16);
        let tot32 = le32(s, 0x20);
        if !sector_size_ok(bps)
            || !(1..=128).contains(&spc)
            || !spc.is_power_of_two()
            || rsvd == 0
            || !(1..=2).contains(&nfats)
            || !(media == 0xF0 || media >= 0xF8)
        {
            return None;
        }
        let fatsz = if fatsz16 != 0 { fatsz16 } else { le32(s, 0x24) };
        let tot = if tot16 != 0 { tot16 } else { tot32 };
        let root_sectors = (root_ent * 32).div_ceil(bps);
        let meta = rsvd + nfats * fatsz + root_sectors;
        if fatsz == 0 || tot == 0 || meta >= tot {
            return None;
        }
        let clusters = (tot - meta) / spc;
        let fat32_layout = fatsz16 == 0;
        let (fs, entry_bits) = match clusters {
            0..4085 => (FsKind::Fat12, 12),
            4085..65525 => (FsKind::Fat16, 16),
            _ => (FsKind::Fat32, 32),
        };
        // El tipo lo decide el número de clústeres (fatgen103); la disposición del BPB debe
        // coincidir y la FAT debe tener sitio para todos los clústeres.
        let layout_ok = if fs == FsKind::Fat32 {
            fat32_layout && root_ent == 0 && tot16 == 0
        } else {
            !fat32_layout && root_ent != 0
        };
        if !layout_ok || fatsz * bps * 8 < (clusters + 2) * entry_bits {
            return None;
        }
        let (serial_at, sig_at) = if fs == FsKind::Fat32 {
            (0x43, 0x42)
        } else {
            (0x27, 0x26)
        };
        let serial = if s[sig_at] == 0x29 {
            le32(s, serial_at)
        } else {
            0
        };
        let length = tot.checked_mul(bps)?;
        let mut backups = Vec::new();
        if fs == FsKind::Fat32 {
            let bk = le16(s, 0x32);
            if bk != 0 && bk != 0xFFFF && bk < rsvd {
                backups.push((bk * bps, length));
            }
        }
        Some(Boot {
            fs,
            sector_size: bps as u32,
            length,
            backups,
            declared_start: le32(s, 0x1C),
            serial,
            raw: s[..512].to_vec(),
        })
    }

    fn consistent(&self, a: &Boot, b: &Boot) -> bool {
        let end = if a.fs == FsKind::Fat32 { 0x5A } else { 0x3E };
        a.fs == b.fs && a.raw[0x0B..end] == b.raw[0x0B..end]
    }

    fn structures(&self, boot: &Boot) -> Option<(u64, u64)> {
        Some((le16(&boot.raw, 0x0E) * u64::from(boot.sector_size), 4))
    }

    /// Primera entrada de la FAT con el byte de medio del BPB y el resto a unos.
    fn structures_ok(&self, boot: &Boot, d: &[u8]) -> bool {
        let media = boot.raw[0x15];
        d[0] == media
            && match boot.fs {
                FsKind::Fat12 => d[1] == 0xFF && d[2] & 0x0F == 0x0F,
                FsKind::Fat16 => d[1] == 0xFF,
                _ => d[1] == 0xFF && d[2] == 0xFF && d[3] & 0x0F == 0x0F,
            }
    }
}

// ---------------------------------------------------------------------------------------------
// exFAT
// ---------------------------------------------------------------------------------------------

pub(crate) struct ExFat;

/// Suma de comprobación de la región de arranque (exFAT §3.4): sectores 0 a 10, sin los bytes
/// `VolumeFlags` (106, 107) ni `PercentInUse` (112) del primero.
pub(crate) fn exfat_checksum(region: &[u8]) -> u32 {
    let mut c: u32 = 0;
    for (i, &b) in region.iter().enumerate() {
        if matches!(i, 106 | 107 | 112) {
            continue;
        }
        c = c.rotate_right(1).wrapping_add(u32::from(b));
    }
    c
}

impl Recognizer for ExFat {
    fn family(&self) -> u32 {
        super::family::EXFAT
    }

    fn parse(&self, s: &[u8]) -> Option<Boot> {
        if s.len() < 512
            || s[..3] != [0xEB, 0x76, 0x90]
            || &s[3..11] != b"EXFAT   "
            || s[11..64].iter().any(|&b| b != 0)
            || !signature_ok(s)
        {
            return None;
        }
        let offset = le64(s, 64);
        let len = le64(s, 72);
        let fat_off = le32(s, 80);
        let fat_len = le32(s, 84);
        let heap = le32(s, 88);
        let clusters = le32(s, 92);
        let root = le32(s, 96);
        let revision = le16(s, 104);
        let bps_shift = u32::from(s[108]);
        let spc_shift = u32::from(s[109]);
        let nfats = u64::from(s[110]);
        if !(9..=12).contains(&bps_shift)
            || spc_shift > 25 - bps_shift
            || !(1..=2).contains(&nfats)
            || revision >> 8 != 1
            || len < (1u64 << 20) >> bps_shift
            || fat_off < 24
            || fat_len == 0
            || fat_off + fat_len * nfats > heap
            || heap + (clusters << spc_shift) > len
            || root < 2
            || root > clusters + 1
        {
            return None;
        }
        let ss = 1u64 << bps_shift;
        let length = len.checked_mul(ss)?;
        Some(Boot {
            fs: FsKind::ExFat,
            sector_size: ss as u32,
            length,
            backups: vec![(12 * ss, length)],
            declared_start: offset,
            serial: le32(s, 100),
            raw: s[..512].to_vec(),
        })
    }

    fn copy_len(&self, boot: &Boot) -> u64 {
        12 * u64::from(boot.sector_size)
    }

    fn copy_ok(&self, boot: &Boot, region: &[u8]) -> bool {
        let ss = boot.sector_size as usize;
        if region.len() < 12 * ss {
            return false;
        }
        let sum = exfat_checksum(&region[..11 * ss]);
        let (words, _) = region[11 * ss..12 * ss].as_chunks::<4>();
        words.iter().all(|w| u32::from_le_bytes(*w) == sum)
    }

    fn consistent(&self, a: &Boot, b: &Boot) -> bool {
        (0..512)
            .filter(|i| !matches!(i, 106 | 107 | 112))
            .all(|i| a.raw[i] == b.raw[i])
    }

    fn structures(&self, boot: &Boot) -> Option<(u64, u64)> {
        Some((le32(&boot.raw, 80) * u64::from(boot.sector_size), 8))
    }

    /// Las dos primeras entradas de la FAT: `F8FFFFFF` (descriptor de medio) y `FFFFFFFF`.
    fn structures_ok(&self, _boot: &Boot, d: &[u8]) -> bool {
        d[..8] == [0xF8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    }
}
