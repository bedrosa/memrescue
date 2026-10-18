//! Registros FILE de la MFT: secuencia de actualización (fixups) y cabecera.
//!
//! Disposición pública de la cabecera de un registro FILE (little-endian): firma `FILE` en 0x00,
//! desplazamiento (u16, 0x04) y número de elementos (u16, 0x06) de la matriz de la secuencia de
//! actualización (USA), LSN (u64, 0x08), número de secuencia (u16, 0x10), número de enlaces (u16,
//! 0x12), desplazamiento del primer atributo (u16, 0x14), indicadores (u16, 0x16: 0x01 en uso, 0x02
//! directorio), bytes en uso (u32, 0x18), bytes asignados (u32, 0x1C), referencia al registro base
//! (u64, 0x20: 48 bits de número y 16 de secuencia), siguiente identificador de atributo (u16,
//! 0x28) y, en NTFS 3.1 (USA en 0x30), el número del propio registro (u32, 0x2C).
//!
//! **Secuencia de actualización**: el registro se escribe en tramos de 512 bytes; los dos últimos
//! bytes de cada tramo se sustituyen en disco por el número de secuencia de actualización (USN, el
//! primer elemento de la USA) y los valores originales se guardan en los elementos siguientes. Al
//! leer, cada tramo debe terminar en el USN (si no, la escritura quedó a medias o el registro está
//! dañado) y se restauran los valores originales. Un desajuste **nunca** se acepta: el registro
//! queda dañado ([`RecordDamage::Fixup`]). [`apply_fixups`] sirve igual para los nodos de índice
//! `INDX` (#22), que usan el mismo mecanismo.

use std::fmt;

use crate::attr::Attributes;

/// Paso de la secuencia de actualización: 512 bytes, sea cual sea el tamaño de sector.
pub const USA_STRIDE: usize = 512;

/// Indicadores de la cabecera de un registro FILE.
pub mod flags {
    /// Registro en uso (un fichero borrado lo tiene a 0).
    pub const IN_USE: u16 = 0x0001;
    /// El registro tiene un índice de nombres de fichero (directorio).
    pub const DIRECTORY: u16 = 0x0002;
}

/// Referencia a un registro MFT: número (48 bits) y número de secuencia (16 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileReference {
    pub record: u64,
    pub sequence: u16,
}

impl FileReference {
    /// Interpreta los 8 bytes de una referencia en disco.
    pub fn from_u64(v: u64) -> Self {
        FileReference {
            record: v & 0x0000_FFFF_FFFF_FFFF,
            sequence: (v >> 48) as u16,
        }
    }

    /// `true` si es la referencia nula (registro base de sí mismo).
    pub fn is_null(&self) -> bool {
        self.record == 0 && self.sequence == 0
    }
}

/// Error de la secuencia de actualización.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixupError {
    /// El búfer no es múltiplo de 512 bytes o es demasiado pequeño.
    BufferSize(usize),
    /// Desplazamiento de la USA impar, dentro de la cabecera fija o fuera del primer tramo.
    UsaOffset(u16),
    /// Número de elementos de la USA distinto de `tramos + 1`.
    UsaCount { count: u16, expected: u16 },
    /// El tramo `stride` no termina en el USN.
    Mismatch { stride: u16 },
}

impl fmt::Display for FixupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixupError::BufferSize(n) => write!(f, "tamaño {n} no múltiplo de 512"),
            FixupError::UsaOffset(o) => {
                write!(f, "desplazamiento de la secuencia de actualización no válido: {o}")
            }
            FixupError::UsaCount { count, expected } => write!(
                f,
                "secuencia de actualización de {count} elementos (se esperaban {expected})"
            ),
            FixupError::Mismatch { stride } => write!(
                f,
                "el tramo {stride} no termina en el número de secuencia de actualización (escritura incompleta o daño)"
            ),
        }
    }
}

/// Comprueba y deshace la secuencia de actualización de un registro multisector (`FILE`, `INDX`)
/// en `buf` (múltiplo de 512 bytes). Devuelve el USN. Si algún tramo no coincide, `buf` puede
/// quedar a medio restaurar y no debe usarse.
pub fn apply_fixups(buf: &mut [u8]) -> Result<u16, FixupError> {
    if buf.len() < USA_STRIDE || !buf.len().is_multiple_of(USA_STRIDE) {
        return Err(FixupError::BufferSize(buf.len()));
    }
    let strides = buf.len() / USA_STRIDE;
    let usa_offset = u16::from_le_bytes([buf[4], buf[5]]);
    let usa_count = u16::from_le_bytes([buf[6], buf[7]]);
    let expected = strides + 1;
    if usize::from(usa_count) != expected {
        return Err(FixupError::UsaCount {
            count: usa_count,
            expected: u16::try_from(expected).unwrap_or(u16::MAX),
        });
    }
    let off = usize::from(usa_offset);
    // La USA debe quedar entera en el primer tramo, antes de sus dos últimos bytes.
    if off < 8 || !off.is_multiple_of(2) || off + 2 * expected > USA_STRIDE - 2 {
        return Err(FixupError::UsaOffset(usa_offset));
    }
    let usn = [buf[off], buf[off + 1]];
    for i in 0..strides {
        let end = (i + 1) * USA_STRIDE - 2;
        if buf[end..end + 2] != usn {
            return Err(FixupError::Mismatch { stride: i as u16 });
        }
    }
    for i in 0..strides {
        let end = (i + 1) * USA_STRIDE - 2;
        let saved = off + 2 * (i + 1);
        let v = [buf[saved], buf[saved + 1]];
        buf[end..end + 2].copy_from_slice(&v);
    }
    Ok(u16::from_le_bytes(usn))
}

/// Motivo por el que un registro MFT no se puede interpretar. Un registro dañado nunca se usa
/// como si fuera válido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordDamage {
    /// Hay menos bytes que el tamaño de registro (origen truncado).
    Truncated { len: usize, expected: usize },
    /// Firma `BAAD`: el propio sistema de archivos marcó el registro como dañado.
    Baad,
    /// Firma distinta de `FILE` (y de `BAAD`).
    BadSignature([u8; 4]),
    /// Secuencia de actualización no válida o tramo que no coincide.
    Fixup(FixupError),
    /// Bytes asignados distintos del tamaño de registro del volumen.
    AllocatedSize { declared: u32, expected: u32 },
    /// Bytes en uso mayores que el registro o que no dejan sitio para la cabecera.
    BytesInUse { value: u32, allocated: u32 },
    /// Desplazamiento del primer atributo no alineado, dentro de la cabecera o fuera de lo usado.
    AttrsOffset(u16),
}

impl RecordDamage {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            RecordDamage::Truncated { .. } => "record_truncated",
            RecordDamage::Baad => "record_baad",
            RecordDamage::BadSignature(_) => "record_bad_signature",
            RecordDamage::Fixup(FixupError::Mismatch { .. }) => "record_fixup_mismatch",
            RecordDamage::Fixup(_) => "record_usa_invalid",
            RecordDamage::AllocatedSize { .. } => "record_allocated_size",
            RecordDamage::BytesInUse { .. } => "record_bytes_in_use",
            RecordDamage::AttrsOffset(_) => "record_attrs_offset",
        }
    }
}

impl fmt::Display for RecordDamage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordDamage::Truncated { len, expected } => {
                write!(f, "registro truncado: {len} de {expected} bytes")
            }
            RecordDamage::Baad => write!(f, "registro marcado BAAD por el sistema de archivos"),
            RecordDamage::BadSignature(s) => write!(
                f,
                "firma {:02X} {:02X} {:02X} {:02X} en lugar de FILE",
                s[0], s[1], s[2], s[3]
            ),
            RecordDamage::Fixup(e) => e.fmt(f),
            RecordDamage::AllocatedSize { declared, expected } => write!(
                f,
                "tamaño asignado {declared} distinto del tamaño de registro {expected}"
            ),
            RecordDamage::BytesInUse { value, allocated } => {
                write!(f, "bytes en uso {value} no válidos (asignados {allocated})")
            }
            RecordDamage::AttrsOffset(o) => {
                write!(f, "desplazamiento del primer atributo no válido: {o}")
            }
        }
    }
}

/// Cabecera de un registro FILE ya validada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordHeader {
    pub usa_offset: u16,
    pub usa_count: u16,
    /// Número de secuencia de actualización (USN) que llevaban los tramos.
    pub update_sequence: u16,
    pub lsn: u64,
    pub sequence: u16,
    pub link_count: u16,
    pub attrs_offset: u16,
    pub flags: u16,
    pub bytes_in_use: u32,
    pub bytes_allocated: u32,
    pub base_record: FileReference,
    pub next_attr_id: u16,
    /// Número del registro guardado en la cabecera (NTFS 3.1); `None` en cabeceras antiguas.
    pub record_number: Option<u32>,
}

impl RecordHeader {
    pub fn in_use(&self) -> bool {
        self.flags & flags::IN_USE != 0
    }

    pub fn is_directory(&self) -> bool {
        self.flags & flags::DIRECTORY != 0
    }

    /// `true` si es un registro base (no una extensión de otro).
    pub fn is_base(&self) -> bool {
        self.base_record.is_null()
    }
}

/// Registro FILE con la secuencia de actualización aplicada y la cabecera validada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    number: u64,
    data: Vec<u8>,
    header: RecordHeader,
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

/// Tamaño mínimo de la cabecera fija de un registro FILE (hasta el campo 0x28 incluido).
const HEADER_MIN: usize = 0x2A;

impl FileRecord {
    /// Interpreta `raw` (exactamente `record_size` bytes leídos de un origen fiable) como el
    /// registro `number`. `record_size` debe ser múltiplo de 512 (lo garantiza el arranque).
    pub fn parse(number: u64, mut raw: Vec<u8>, record_size: u32) -> Result<Self, RecordDamage> {
        let size = record_size as usize;
        if raw.len() < size || size < USA_STRIDE {
            return Err(RecordDamage::Truncated {
                len: raw.len(),
                expected: size,
            });
        }
        raw.truncate(size);
        let sig = [raw[0], raw[1], raw[2], raw[3]];
        if &sig == b"BAAD" {
            return Err(RecordDamage::Baad);
        }
        if &sig != b"FILE" {
            return Err(RecordDamage::BadSignature(sig));
        }
        let usn = apply_fixups(&mut raw).map_err(RecordDamage::Fixup)?;
        let usa_offset = le16(&raw, 4);
        let usa_count = le16(&raw, 6);
        let allocated = le32(&raw, 0x1C);
        if allocated != record_size {
            return Err(RecordDamage::AllocatedSize {
                declared: allocated,
                expected: record_size,
            });
        }
        let in_use = le32(&raw, 0x18);
        let attrs = le16(&raw, 0x14);
        let usa_end = usize::from(usa_offset) + 2 * usize::from(usa_count);
        let a = usize::from(attrs);
        if !a.is_multiple_of(8) || a < usa_end.max(HEADER_MIN) || a + 4 > size {
            return Err(RecordDamage::AttrsOffset(attrs));
        }
        // Lo usado debe contener al menos la marca de fin (4 bytes) tras el primer atributo.
        if in_use > allocated || (in_use as usize) < a + 4 {
            return Err(RecordDamage::BytesInUse {
                value: in_use,
                allocated,
            });
        }
        let header = RecordHeader {
            usa_offset,
            usa_count,
            update_sequence: usn,
            lsn: le64(&raw, 0x08),
            sequence: le16(&raw, 0x10),
            link_count: le16(&raw, 0x12),
            attrs_offset: attrs,
            flags: le16(&raw, 0x16),
            bytes_in_use: in_use,
            bytes_allocated: allocated,
            base_record: FileReference::from_u64(le64(&raw, 0x20)),
            next_attr_id: le16(&raw, 0x28),
            record_number: (usa_offset >= 0x30).then(|| le32(&raw, 0x2C)),
        };
        Ok(FileRecord {
            number,
            data: raw,
            header,
        })
    }

    /// Número del registro en la MFT (por su posición, no por el campo de la cabecera).
    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// Bytes del registro completo, con la secuencia de actualización ya deshecha.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn in_use(&self) -> bool {
        self.header.in_use()
    }

    pub fn is_directory(&self) -> bool {
        self.header.is_directory()
    }

    /// Referencia a este registro con su número de secuencia actual.
    pub fn reference(&self) -> FileReference {
        FileReference {
            record: self.number,
            sequence: self.header.sequence,
        }
    }

    /// `true` si la cabecera guarda un número de registro distinto de la posición (registro
    /// copiado o desplazado). Informativo: la posición manda.
    pub fn number_mismatch(&self) -> bool {
        self.header
            .record_number
            .is_some_and(|n| u64::from(n) != self.number & 0xFFFF_FFFF)
    }

    /// Iterador acotado de los atributos (nunca lee más allá de los bytes en uso).
    pub fn attributes(&self) -> Attributes<'_> {
        Attributes::new(
            &self.data[..self.header.bytes_in_use as usize],
            usize::from(self.header.attrs_offset),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::{protect, record, resident, xorshift};

    fn sample() -> Vec<u8> {
        record(
            42,
            3,
            flags::IN_USE,
            &[resident(0x30, "", &[1, 2, 3, 4, 5, 6, 7, 8])],
            1024,
        )
    }

    #[test]
    fn valid_record_header() {
        let r = FileRecord::parse(42, sample(), 1024).unwrap();
        let h = r.header();
        assert_eq!(h.sequence, 3);
        assert_eq!(h.link_count, 1);
        assert!(r.in_use() && !r.is_directory());
        assert!(h.is_base());
        assert_eq!(h.bytes_allocated, 1024);
        assert_eq!(h.record_number, Some(42));
        assert!(!r.number_mismatch());
        assert_eq!(
            r.reference(),
            FileReference {
                record: 42,
                sequence: 3
            }
        );
        // Fixups deshechos: los dos últimos bytes de cada tramo vuelven a ser los originales.
        assert_eq!(&r.data()[510..512], &[0xA5, 0x5A]);
        assert_eq!(&r.data()[1022..1024], &[0xC3, 0x3C]);
        assert_eq!(r.attributes().count(), 1);
        // Otro número por posición: se informa, no se rechaza.
        let r = FileRecord::parse(7, sample(), 1024).unwrap();
        assert!(r.number_mismatch());
    }

    #[test]
    fn fixup_mismatch_marks_the_record_damaged() {
        for stride in 0..2usize {
            let mut raw = sample();
            raw[(stride + 1) * 512 - 1] ^= 0xFF;
            assert_eq!(
                FileRecord::parse(42, raw, 1024).unwrap_err(),
                RecordDamage::Fixup(FixupError::Mismatch {
                    stride: stride as u16
                })
            );
        }
        // USA desplazada, con número de elementos erróneo o que se sale del primer tramo.
        let cases: [(usize, [u8; 2], &str); 5] = [
            (4, [0x31, 0], "record_usa_invalid"),
            (4, [0x04, 0], "record_usa_invalid"),
            (4, [0xFC, 0x01], "record_usa_invalid"),
            (6, [2, 0], "record_usa_invalid"),
            (6, [4, 0], "record_usa_invalid"),
        ];
        for (at, v, code) in cases {
            let mut raw = sample();
            raw[at..at + 2].copy_from_slice(&v);
            assert_eq!(FileRecord::parse(42, raw, 1024).unwrap_err().code(), code);
        }
    }

    #[test]
    fn signatures_and_truncation() {
        let mut raw = sample();
        raw[..4].copy_from_slice(b"BAAD");
        assert_eq!(
            FileRecord::parse(1, raw, 1024).unwrap_err(),
            RecordDamage::Baad
        );
        let mut raw = sample();
        raw[..4].copy_from_slice(b"INDX");
        assert_eq!(
            FileRecord::parse(1, raw, 1024).unwrap_err().code(),
            "record_bad_signature"
        );
        let raw = sample()[..1000].to_vec();
        assert_eq!(
            FileRecord::parse(1, raw, 1024).unwrap_err(),
            RecordDamage::Truncated {
                len: 1000,
                expected: 1024
            }
        );
        // Tamaño del volumen distinto del asignado en el registro.
        let raw = sample();
        let mut big = raw.clone();
        big.extend_from_slice(&[0; 1024]);
        assert_eq!(
            FileRecord::parse(1, big, 2048).unwrap_err().code(),
            "record_usa_invalid"
        );
    }

    #[test]
    fn header_bounds() {
        let set = |at: usize, v: &[u8]| {
            let mut raw = sample();
            // Sin la protección para editar, y se vuelve a proteger.
            let mut plain = raw.clone();
            apply_fixups(&mut plain).unwrap();
            plain[at..at + v.len()].copy_from_slice(v);
            protect(&mut plain);
            raw.copy_from_slice(&plain);
            FileRecord::parse(42, raw, 1024)
        };
        assert_eq!(
            set(0x1C, &2048u32.to_le_bytes()).unwrap_err().code(),
            "record_allocated_size"
        );
        assert_eq!(
            set(0x18, &1032u32.to_le_bytes()).unwrap_err().code(),
            "record_bytes_in_use"
        );
        assert_eq!(
            set(0x18, &0x38u32.to_le_bytes()).unwrap_err().code(),
            "record_bytes_in_use"
        );
        for v in [0x30u16, 0x3C, 0x400, 0x3FE] {
            assert_eq!(
                set(0x14, &v.to_le_bytes()).unwrap_err().code(),
                "record_attrs_offset",
                "{v}"
            );
        }
        // Registro no en uso (borrado): se interpreta igual.
        let r = set(0x16, &0u16.to_le_bytes()).unwrap();
        assert!(!r.in_use());
        let r = set(0x20, &0x0002_0000_0000_0005u64.to_le_bytes()).unwrap();
        assert_eq!(
            r.header().base_record,
            FileReference {
                record: 5,
                sequence: 2
            }
        );
        assert!(!r.header().is_base());
    }

    #[test]
    fn fixups_on_other_buffers() {
        assert_eq!(
            apply_fixups(&mut [0u8; 100]),
            Err(FixupError::BufferSize(100))
        );
        assert_eq!(
            apply_fixups(&mut [0u8; 700]),
            Err(FixupError::BufferSize(700))
        );
        // Registro de 4096 B (8 tramos) con la USA en 0x28.
        let mut b = vec![0u8; 4096];
        b[..4].copy_from_slice(b"INDX");
        b[4..6].copy_from_slice(&0x28u16.to_le_bytes());
        b[6..8].copy_from_slice(&9u16.to_le_bytes());
        b[0x28..0x2A].copy_from_slice(&7u16.to_le_bytes());
        for i in 0..8 {
            b[(i + 1) * 512 - 2..(i + 1) * 512].copy_from_slice(&7u16.to_le_bytes());
            b[0x2A + 2 * i..0x2C + 2 * i].copy_from_slice(&[i as u8, 0xEE]);
        }
        assert_eq!(apply_fixups(&mut b), Ok(7));
        for i in 0..8 {
            assert_eq!(&b[(i + 1) * 512 - 2..(i + 1) * 512], &[i as u8, 0xEE]);
        }
    }

    /// Fuzzing determinista de registros: sin pánicos; lo aceptado respeta los límites y sus
    /// atributos se recorren sin salir de lo usado.
    #[test]
    fn record_fuzz_never_panics() {
        let mut rng = xorshift(0xF11E_0001);
        let base = crate::testvol::rich_record();
        let mut accepted = 0;
        for i in 0..20_000u32 {
            let mut raw = base.clone();
            if i.is_multiple_of(2) {
                // Mutaciones con la protección rehecha, para pasar de los fixups.
                apply_fixups(&mut raw).unwrap();
                for _ in 0..1 + rng() % 4 {
                    let at = (rng() % 0x200) as usize;
                    raw[at] = rng() as u8;
                }
                protect(&mut raw);
            } else {
                for _ in 0..1 + rng() % 4 {
                    let at = (rng() % 1024) as usize;
                    raw[at] = rng() as u8;
                }
            }
            if let Ok(r) = FileRecord::parse(9, raw, 1024) {
                accepted += 1;
                assert!(r.header().bytes_in_use <= 1024);
                let mut n = 0;
                for a in r.attributes() {
                    n += 1;
                    assert!(n <= 1024 / 0x18 + 1);
                    if let Ok(a) = a {
                        assert!(a.offset + a.length <= r.header().bytes_in_use as usize);
                        let _ = a.name();
                        if let crate::attr::AttrBody::NonResident(nr) = &a.body {
                            let _ =
                                crate::runs::decode_runs(nr.mapping_pairs, nr.start_vcn, 1 << 20);
                        }
                    }
                }
            }
        }
        assert!(accepted > 2000, "{accepted}");
    }
}
