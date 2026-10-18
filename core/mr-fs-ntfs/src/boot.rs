//! Sector de arranque NTFS (BPB extendido): interpretación y validación estricta.
//!
//! Disposición pública del sector de arranque NTFS (little-endian): salto x86 en 0x00, identificador
//! OEM `NTFS    ` en 0x03, bytes por sector (u16) en 0x0B, sectores por clúster (u8) en 0x0D, campos
//! heredados de FAT que NTFS exige a cero (0x0E, 0x10, 0x11, 0x13, 0x16, 0x20), total de sectores
//! (u64) en 0x28 (no incluye el sector de la copia), LCN de `$MFT` (u64) en 0x30, LCN de `$MFTMirr`
//! (u64) en 0x38, tamaño de registro MFT (i8) en 0x40, tamaño de registro de índice (i8) en 0x44,
//! número de serie (u64) en 0x48 y firma `55 AA` en 0x1FE.
//!
//! Los tamaños de registro se codifican así: un valor positivo es un número de clústeres; uno
//! negativo `-n` significa `2^n` bytes. Los sectores por clúster admiten la codificación de
//! exponente negativo de los clústeres grandes: un valor `v > 0x80` significa `2^(256 - v)` sectores.

use std::fmt;

/// Bytes de un sector de arranque.
pub const BOOT_SIZE: usize = 512;
/// Tamaño máximo de clúster admitido (2 MiB).
pub const MAX_CLUSTER_SIZE: u64 = 2 * 1024 * 1024;
/// Tamaños de registro (MFT e índice) admitidos: potencias de 2 en este intervalo. El mínimo es
/// el paso de la secuencia de actualización (512 B).
pub const MIN_RECORD_SIZE: u32 = 512;
pub const MAX_RECORD_SIZE: u32 = 65536;

/// Campos del sector de arranque ya validados.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSector {
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub cluster_size: u32,
    /// Sectores del volumen según el campo 0x28 (sin el sector de la copia del arranque).
    pub total_sectors: u64,
    /// Clústeres completos del volumen (`total_sectors / sectors_per_cluster`).
    pub total_clusters: u64,
    pub mft_lcn: u64,
    pub mftmirr_lcn: u64,
    pub mft_record_size: u32,
    pub index_record_size: u32,
    pub serial: u64,
    /// Valores tal como están codificados (0x0D, 0x40 y 0x44).
    pub raw_sectors_per_cluster: u8,
    pub raw_mft_record: i8,
    pub raw_index_record: i8,
    /// Particularidades que no impiden usar el sector (avisos).
    pub notes: Vec<BootNote>,
}

impl BootSector {
    /// Bytes del volumen según el arranque (`total_sectors × bytes_per_sector`; ya comprobado sin
    /// desbordamiento al validar).
    pub fn volume_bytes(&self) -> u64 {
        self.total_sectors * u64::from(self.bytes_per_sector)
    }

    /// Desplazamiento (bytes desde el inicio del volumen) donde debe estar la copia del arranque:
    /// el sector siguiente al último que cuenta `total_sectors`.
    pub fn backup_offset(&self) -> u64 {
        self.volume_bytes()
    }

    /// Desplazamiento en bytes del clúster `lcn`, si cabe en `u64`.
    pub fn cluster_offset(&self, lcn: u64) -> Option<u64> {
        lcn.checked_mul(u64::from(self.cluster_size))
    }
}

/// Particularidad del sector de arranque que no lo invalida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootNote {
    /// Los bytes 0x00..0x03 no son un salto x86 (`EB xx 90` o `E9 xx xx`).
    NoJump,
    /// Bytes de relleno de los campos de tamaño de registro (0x41..0x43 o 0x45..0x47) distintos
    /// de cero.
    RecordSizePadding,
}

impl BootNote {
    pub fn code(self) -> &'static str {
        match self {
            BootNote::NoJump => "boot_no_jump",
            BootNote::RecordSizePadding => "boot_record_size_padding",
        }
    }
}

impl fmt::Display for BootNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootNote::NoJump => write!(f, "el sector de arranque no empieza por un salto x86"),
            BootNote::RecordSizePadding => write!(
                f,
                "relleno distinto de cero en los campos de tamaño de registro"
            ),
        }
    }
}

/// Motivo por el que un sector no es un arranque NTFS válido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootError {
    /// Menos de 512 bytes.
    TooShort(usize),
    /// Sin identificador OEM `NTFS    `.
    NoOemId,
    /// Sin firma `55 AA` en 0x1FE.
    NoSignature,
    /// Bytes por sector fuera de 512, 1024, 2048 o 4096.
    BytesPerSector(u16),
    /// Codificación de sectores por clúster no válida.
    SectorsPerCluster(u8),
    /// Clúster mayor que [`MAX_CLUSTER_SIZE`].
    ClusterTooLarge(u64),
    /// Campo heredado de FAT distinto de cero (desplazamiento del campo).
    FatFieldNotZero(usize),
    /// Total de sectores 0 o volumen sin ningún clúster completo.
    TotalSectors(u64),
    /// `total_sectors × bytes_per_sector` no cabe en 64 bits.
    VolumeTooLarge(u64),
    /// LCN de `$MFT` 0 o fuera del volumen.
    MftLcn { lcn: u64, clusters: u64 },
    /// LCN de `$MFTMirr` 0 o fuera del volumen.
    MftMirrLcn { lcn: u64, clusters: u64 },
    /// Tamaño de registro MFT no admitido (valor codificado).
    MftRecordSize(i8),
    /// Tamaño de registro de índice no admitido (valor codificado).
    IndexRecordSize(i8),
}

impl BootError {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            BootError::TooShort(_) => "too_short",
            BootError::NoOemId => "no_oem_id",
            BootError::NoSignature => "no_signature",
            BootError::BytesPerSector(_) => "bytes_per_sector",
            BootError::SectorsPerCluster(_) => "sectors_per_cluster",
            BootError::ClusterTooLarge(_) => "cluster_too_large",
            BootError::FatFieldNotZero(_) => "fat_field_not_zero",
            BootError::TotalSectors(_) => "total_sectors",
            BootError::VolumeTooLarge(_) => "volume_too_large",
            BootError::MftLcn { .. } => "mft_lcn",
            BootError::MftMirrLcn { .. } => "mftmirr_lcn",
            BootError::MftRecordSize(_) => "mft_record_size",
            BootError::IndexRecordSize(_) => "index_record_size",
        }
    }
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootError::TooShort(n) => write!(f, "sector de {n} bytes (se necesitan 512)"),
            BootError::NoOemId => write!(f, "sin identificador OEM «NTFS»"),
            BootError::NoSignature => write!(f, "sin firma 55 AA"),
            BootError::BytesPerSector(v) => write!(f, "bytes por sector no admitidos: {v}"),
            BootError::SectorsPerCluster(v) => {
                write!(f, "sectores por clúster no válidos: 0x{v:02X}")
            }
            BootError::ClusterTooLarge(v) => {
                write!(f, "clúster de {v} bytes (máximo {MAX_CLUSTER_SIZE})")
            }
            BootError::FatFieldNotZero(o) => {
                write!(f, "campo heredado de FAT en 0x{o:02X} distinto de cero")
            }
            BootError::TotalSectors(v) => write!(f, "total de sectores no válido: {v}"),
            BootError::VolumeTooLarge(v) => {
                write!(f, "total de sectores desmesurado: {v}")
            }
            BootError::MftLcn { lcn, clusters } => {
                write!(
                    f,
                    "LCN de $MFT {lcn} fuera del volumen ({clusters} clústeres)"
                )
            }
            BootError::MftMirrLcn { lcn, clusters } => write!(
                f,
                "LCN de $MFTMirr {lcn} fuera del volumen ({clusters} clústeres)"
            ),
            BootError::MftRecordSize(v) => {
                write!(f, "tamaño de registro MFT no admitido (valor {v})")
            }
            BootError::IndexRecordSize(v) => {
                write!(f, "tamaño de registro de índice no admitido (valor {v})")
            }
        }
    }
}

fn le16(s: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([s[o], s[o + 1]])
}

fn le32(s: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]])
}

fn le64(s: &[u8], o: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[o..o + 8]);
    u64::from_le_bytes(b)
}

/// Sectores por clúster a partir del byte 0x0D: potencia de 2 hasta 128, o `2^(256 - v)` para
/// `v` entre 0xF4 y 0xFF (clústeres de 2^1 a 2^12 sectores).
pub fn decode_sectors_per_cluster(raw: u8) -> Option<u32> {
    match raw {
        1..=128 if raw.is_power_of_two() => Some(u32::from(raw)),
        0xF4..=0xFF => Some(1u32 << (256 - u32::from(raw))),
        _ => None,
    }
}

/// Tamaño de registro en bytes a partir del valor codificado: positivo = clústeres (resultado
/// potencia de 2), negativo `-n` = `2^n` bytes. Debe quedar en
/// [`MIN_RECORD_SIZE`]..=[`MAX_RECORD_SIZE`].
pub fn decode_record_size(raw: i8, cluster_size: u32) -> Option<u32> {
    let bytes = match raw {
        1..=127 => u64::from(cluster_size).checked_mul(raw as u64)?,
        -31..=-1 => 1u64 << (-i32::from(raw)),
        _ => return None,
    };
    (bytes.is_power_of_two()
        && (u64::from(MIN_RECORD_SIZE)..=u64::from(MAX_RECORD_SIZE)).contains(&bytes))
    .then_some(bytes as u32)
}

/// Interpreta y valida los primeros 512 bytes de `sector` como sector de arranque NTFS.
pub fn parse_boot(sector: &[u8]) -> Result<BootSector, BootError> {
    if sector.len() < BOOT_SIZE {
        return Err(BootError::TooShort(sector.len()));
    }
    let s = &sector[..BOOT_SIZE];
    if &s[3..11] != b"NTFS    " {
        return Err(BootError::NoOemId);
    }
    if s[0x1FE] != 0x55 || s[0x1FF] != 0xAA {
        return Err(BootError::NoSignature);
    }
    let bps = le16(s, 0x0B);
    if !matches!(bps, 512 | 1024 | 2048 | 4096) {
        return Err(BootError::BytesPerSector(bps));
    }
    let spc = decode_sectors_per_cluster(s[0x0D]).ok_or(BootError::SectorsPerCluster(s[0x0D]))?;
    let cluster = u64::from(spc) * u64::from(bps);
    if cluster > MAX_CLUSTER_SIZE {
        return Err(BootError::ClusterTooLarge(cluster));
    }
    let cluster_size = cluster as u32;
    // Campos heredados de FAT: sectores reservados, número de FAT, entradas raíz, sectores de 16
    // bits, sectores por FAT y sectores de 32 bits.
    for (off, zero) in [
        (0x0E, le16(s, 0x0E) == 0),
        (0x10, s[0x10] == 0),
        (0x11, le16(s, 0x11) == 0),
        (0x13, le16(s, 0x13) == 0),
        (0x16, le16(s, 0x16) == 0),
        (0x20, le32(s, 0x20) == 0),
    ] {
        if !zero {
            return Err(BootError::FatFieldNotZero(off));
        }
    }
    let total = le64(s, 0x28);
    let clusters = total / u64::from(spc);
    if total == 0 || clusters == 0 {
        return Err(BootError::TotalSectors(total));
    }
    if total.checked_mul(u64::from(bps)).is_none() {
        return Err(BootError::VolumeTooLarge(total));
    }
    let mft = le64(s, 0x30);
    if mft == 0 || mft >= clusters {
        return Err(BootError::MftLcn { lcn: mft, clusters });
    }
    let mirr = le64(s, 0x38);
    if mirr == 0 || mirr >= clusters {
        return Err(BootError::MftMirrLcn {
            lcn: mirr,
            clusters,
        });
    }
    let raw_mft = s[0x40] as i8;
    let mft_record =
        decode_record_size(raw_mft, cluster_size).ok_or(BootError::MftRecordSize(raw_mft))?;
    let raw_index = s[0x44] as i8;
    let index_record =
        decode_record_size(raw_index, cluster_size).ok_or(BootError::IndexRecordSize(raw_index))?;
    let mut notes = Vec::new();
    if !((s[0] == 0xEB && s[2] == 0x90) || s[0] == 0xE9) {
        notes.push(BootNote::NoJump);
    }
    if s[0x41..0x44] != [0, 0, 0] || s[0x45..0x48] != [0, 0, 0] {
        notes.push(BootNote::RecordSizePadding);
    }
    Ok(BootSector {
        bytes_per_sector: u32::from(bps),
        sectors_per_cluster: spc,
        cluster_size,
        total_sectors: total,
        total_clusters: clusters,
        mft_lcn: mft,
        mftmirr_lcn: mirr,
        mft_record_size: mft_record,
        index_record_size: index_record,
        serial: le64(s, 0x48),
        raw_sectors_per_cluster: s[0x0D],
        raw_mft_record: raw_mft,
        raw_index_record: raw_index,
        notes,
    })
}

/// `true` si dos sectores de arranque describen el mismo volumen (BPB idéntico, 0x0B..0x50).
pub fn same_bpb(a: &[u8], b: &[u8]) -> bool {
    a.len() >= 0x50 && b.len() >= 0x50 && a[0x0B..0x50] == b[0x0B..0x50]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::{boot_sector, xorshift};

    fn good() -> Vec<u8> {
        boot_sector(512, 2, 511, 16, 64, 0xF6, 0x01, 0x1122_3344_5566_7788)
    }

    #[test]
    fn valid_boot_fields() {
        let b = parse_boot(&good()).unwrap();
        assert_eq!(b.bytes_per_sector, 512);
        assert_eq!(b.sectors_per_cluster, 2);
        assert_eq!(b.cluster_size, 1024);
        assert_eq!(b.total_sectors, 511);
        assert_eq!(b.total_clusters, 255);
        assert_eq!((b.mft_lcn, b.mftmirr_lcn), (16, 64));
        assert_eq!(b.mft_record_size, 1024);
        assert_eq!(b.index_record_size, 1024);
        assert_eq!(b.serial, 0x1122_3344_5566_7788);
        assert_eq!(b.backup_offset(), 511 * 512);
        assert!(b.notes.is_empty());
    }

    #[test]
    fn every_field_is_checked() {
        let cases: Vec<(usize, &[u8], &str)> = vec![
            (3, b"NTFX", "no_oem_id"),
            (0x1FE, &[0x55, 0xAB], "no_signature"),
            (0x0B, &[0x00, 0x01], "bytes_per_sector"),
            (0x0B, &[0x00, 0x03], "bytes_per_sector"),
            (0x0B, &[0x00, 0x20], "bytes_per_sector"),
            (0x0D, &[0], "sectors_per_cluster"),
            (0x0D, &[3], "sectors_per_cluster"),
            (0x0D, &[0x81], "sectors_per_cluster"),
            (0x0D, &[0xF3], "sectors_per_cluster"),
            (0x0E, &[1], "fat_field_not_zero"),
            (0x10, &[2], "fat_field_not_zero"),
            (0x11, &[0, 2], "fat_field_not_zero"),
            (0x13, &[1], "fat_field_not_zero"),
            (0x16, &[1], "fat_field_not_zero"),
            (0x20, &[0, 0, 1, 0], "fat_field_not_zero"),
            (0x28, &[0; 8], "total_sectors"),
            (0x28, &[1, 0, 0, 0, 0, 0, 0, 0], "total_sectors"),
            (0x28, &[0xFF; 8], "volume_too_large"),
            (0x30, &[0; 8], "mft_lcn"),
            (0x30, &[255, 0, 0, 0, 0, 0, 0, 0], "mft_lcn"),
            (0x38, &[0; 8], "mftmirr_lcn"),
            (0x38, &[0, 1, 0, 0, 0, 0, 0, 0], "mftmirr_lcn"),
            (0x40, &[0], "mft_record_size"),
            (0x40, &[0xF8], "mft_record_size"), // 2^8 = 256 < 512
            (0x40, &[0xEF], "mft_record_size"), // 2^17
            (0x40, &[3], "mft_record_size"),    // 3 clústeres: no potencia de 2
            (0x40, &[128], "mft_record_size"),  // -128
            (0x44, &[0], "index_record_size"),
            (0x44, &[0x80], "index_record_size"),
        ];
        for (off, bytes, code) in cases {
            let mut s = good();
            s[off..off + bytes.len()].copy_from_slice(bytes);
            let e = parse_boot(&s).unwrap_err();
            assert_eq!(e.code(), code, "0x{off:02X} {bytes:?}: {e}");
            assert!(!e.to_string().is_empty());
        }
        assert_eq!(parse_boot(&good()[..511]).unwrap_err().code(), "too_short");
    }

    #[test]
    fn cluster_and_record_encodings() {
        // Exponente negativo en sectores por clúster: 0xF8 = 2^8 sectores.
        assert_eq!(decode_sectors_per_cluster(0xF8), Some(256));
        assert_eq!(decode_sectors_per_cluster(0xF4), Some(4096));
        assert_eq!(decode_sectors_per_cluster(0xFF), Some(2));
        assert_eq!(decode_sectors_per_cluster(128), Some(128));
        assert_eq!(decode_sectors_per_cluster(0xF0), None);
        // Clúster de 4096 sectores de 512 B = 2 MiB: admitido; con 1024 B/sector, 4 MiB: no.
        let mut s = good();
        s[0x0D] = 0xF4;
        s[0x28..0x30].copy_from_slice(&(1u64 << 20).to_le_bytes());
        s[0x40] = 0xF6;
        s[0x44] = 0xF4;
        let b = parse_boot(&s).unwrap();
        assert_eq!(b.cluster_size, 2 * 1024 * 1024);
        s[0x0B..0x0D].copy_from_slice(&1024u16.to_le_bytes());
        assert_eq!(parse_boot(&s).unwrap_err().code(), "cluster_too_large");
        // Registros: clústeres o potencias de 2.
        assert_eq!(decode_record_size(1, 4096), Some(4096));
        assert_eq!(decode_record_size(2, 512), Some(1024));
        assert_eq!(decode_record_size(-10, 4096), Some(1024));
        assert_eq!(decode_record_size(-12, 512), Some(4096));
        assert_eq!(decode_record_size(-16, 512), Some(65536));
        assert_eq!(decode_record_size(-17, 512), None);
        assert_eq!(decode_record_size(32, 4096), None); // 128 KiB
        assert_eq!(decode_record_size(0, 4096), None);
    }

    #[test]
    fn notes_do_not_invalidate() {
        let mut s = good();
        s[0] = 0;
        s[0x41] = 0xFF;
        let b = parse_boot(&s).unwrap();
        assert_eq!(b.notes, vec![BootNote::NoJump, BootNote::RecordSizePadding]);
        assert!(!same_bpb(&s, &good()));
        s[0x41] = 0;
        assert!(same_bpb(&s, &good()));
    }

    /// Fuzzing determinista: mutaciones de un arranque válido y sectores aleatorios. Nunca hay
    /// pánico y lo aceptado cumple los límites.
    #[test]
    fn boot_fuzz_never_panics_and_respects_limits() {
        let mut rng = xorshift(0x5EED_B007);
        let base = good();
        let mut accepted = 0;
        for i in 0..20_000u32 {
            let mut s = if i.is_multiple_of(10) {
                (0..512).map(|_| rng() as u8).collect::<Vec<_>>()
            } else {
                base.clone()
            };
            let n = 1 + (rng() % 6) as usize;
            for _ in 0..n {
                let at = if rng().is_multiple_of(2) {
                    (rng() % 0x50) as usize
                } else {
                    (rng() % 512) as usize
                };
                s[at] = rng() as u8;
            }
            if let Ok(b) = parse_boot(&s) {
                accepted += 1;
                assert!(matches!(b.bytes_per_sector, 512 | 1024 | 2048 | 4096));
                assert!(u64::from(b.cluster_size) <= MAX_CLUSTER_SIZE);
                assert!(b.mft_lcn > 0 && b.mft_lcn < b.total_clusters);
                assert!(b.mftmirr_lcn > 0 && b.mftmirr_lcn < b.total_clusters);
                for r in [b.mft_record_size, b.index_record_size] {
                    assert!(
                        r.is_power_of_two() && (MIN_RECORD_SIZE..=MAX_RECORD_SIZE).contains(&r)
                    );
                }
                let _ = b.volume_bytes();
            }
        }
        assert!(accepted > 1000, "{accepted}");
    }
}
