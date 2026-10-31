//! Contenido tipado de los atributos residentes: `$STANDARD_INFORMATION`, `$FILE_NAME`,
//! `$OBJECT_ID`, `$VOLUME_NAME`, `$VOLUME_INFORMATION` y las entradas de `$ATTRIBUTE_LIST`.
//!
//! Disposiciones públicas (little-endian). Las marcas de tiempo son FILETIME: intervalos de 100 ns
//! desde el 1 de enero de 1601 (UTC), sin interpretar aquí (ver [`filetime_to_unix`]).
//!
//! - `$STANDARD_INFORMATION` (0x10): creación (0x00), modificación (0x08), modificación del
//!   registro MFT (0x10), acceso (0x18), atributos DOS (u32, 0x20), versiones máximas (0x24),
//!   versión (0x28), identificador de clase (0x2C); 48 bytes en NTFS 1.2 y 72 desde 3.0, que añade
//!   propietario (0x30), seguridad (0x34), cuota (u64, 0x38) y USN (u64, 0x40).
//! - `$FILE_NAME` (0x30): referencia al directorio padre (u64, 0x00), cuatro marcas de tiempo
//!   (0x08..0x28), tamaño asignado (0x28) y real (0x30) copiados del flujo cuando se escribió la
//!   entrada, atributos (u32, 0x38), datos de reparación o EA (u32, 0x3C), longitud del nombre en
//!   unidades UTF-16 (u8, 0x40), espacio de nombres (u8, 0x41: 0 POSIX, 1 Win32, 2 DOS, 3 Win32 y
//!   DOS) y el nombre (0x42).
//! - `$OBJECT_ID` (0x40): identificador de 16 bytes y, opcionalmente, volumen y objeto de origen y
//!   dominio (16 bytes cada uno; 64 bytes en total).
//! - `$VOLUME_INFORMATION` (0x70): versión mayor (0x08), menor (0x09) e indicadores (u16, 0x0A).
//! - `$ATTRIBUTE_LIST` (0x20): entradas de tipo (u32, 0x00), longitud (u16, 0x04, múltiplo de 8),
//!   longitud del nombre (u8, 0x06), desplazamiento del nombre (u8, 0x07), VCN inicial (u64, 0x08),
//!   referencia al registro que contiene el atributo (u64, 0x10) e identificador (u16, 0x18).
//!
//! Cada analizador comprueba los límites antes de leer y devuelve un error tipado; nunca entra en
//! pánico con datos del origen.

use std::fmt;

use crate::attr::{types, utf16_lossy};
use crate::record::FileReference;

/// Valor de un atributo que no cumple su disposición.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueError {
    /// Más corto que el mínimo del tipo.
    TooShort {
        type_code: u32,
        len: usize,
        min: usize,
    },
    /// Nombre de `$FILE_NAME` vacío o que se sale del valor.
    FileNameLength { len: usize, name_units: u8 },
    /// Espacio de nombres desconocido.
    Namespace(u8),
}

impl ValueError {
    pub fn code(&self) -> &'static str {
        match self {
            ValueError::TooShort { .. } => "value_too_short",
            ValueError::FileNameLength { .. } => "file_name_length",
            ValueError::Namespace(_) => "file_name_namespace",
        }
    }
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::TooShort {
                type_code,
                len,
                min,
            } => write!(
                f,
                "valor de {} de {len} bytes (mínimo {min})",
                crate::attr::type_name(*type_code).unwrap_or("atributo")
            ),
            ValueError::FileNameLength { len, name_units } => write!(
                f,
                "$FILE_NAME de {len} bytes con un nombre de {name_units} unidades"
            ),
            ValueError::Namespace(n) => write!(f, "espacio de nombres {n} desconocido"),
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

fn need(type_code: u32, v: &[u8], min: usize) -> Result<(), ValueError> {
    if v.len() < min {
        return Err(ValueError::TooShort {
            type_code,
            len: v.len(),
            min,
        });
    }
    Ok(())
}

/// Marcas de tiempo FILETIME de un fichero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Times {
    pub created: u64,
    pub modified: u64,
    pub mft_modified: u64,
    pub accessed: u64,
}

fn times(v: &[u8], at: usize) -> Times {
    Times {
        created: le64(v, at),
        modified: le64(v, at + 8),
        mft_modified: le64(v, at + 16),
        accessed: le64(v, at + 24),
    }
}

/// Segundos Unix (y resto en 100 ns) de una marca FILETIME; `None` si es anterior a 1970.
pub fn filetime_to_unix(ft: u64) -> Option<(i64, u32)> {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let t = ft.checked_sub(EPOCH_DIFF)?;
    Some(((t / 10_000_000) as i64, (t % 10_000_000) as u32))
}

/// Campos de NTFS 3.0 en adelante de `$STANDARD_INFORMATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardInformationV3 {
    pub owner_id: u32,
    pub security_id: u32,
    pub quota_charged: u64,
    pub usn: u64,
}

/// `$STANDARD_INFORMATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardInformation {
    pub times: Times,
    /// Atributos DOS (0x01 solo lectura, 0x02 oculto, 0x04 sistema, 0x20 archivo, 0x200
    /// disperso, 0x800 comprimido, 0x4000 cifrado...).
    pub file_attributes: u32,
    pub max_versions: u32,
    pub version: u32,
    pub class_id: u32,
    pub v3: Option<StandardInformationV3>,
}

impl StandardInformation {
    pub fn parse(v: &[u8]) -> Result<Self, ValueError> {
        need(types::STANDARD_INFORMATION, v, 0x30)?;
        Ok(StandardInformation {
            times: times(v, 0),
            file_attributes: le32(v, 0x20),
            max_versions: le32(v, 0x24),
            version: le32(v, 0x28),
            class_id: le32(v, 0x2C),
            v3: (v.len() >= 0x48).then(|| StandardInformationV3 {
                owner_id: le32(v, 0x30),
                security_id: le32(v, 0x34),
                quota_charged: le64(v, 0x38),
                usn: le64(v, 0x40),
            }),
        })
    }
}

/// Espacio de nombres de un `$FILE_NAME`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    Posix,
    Win32,
    Dos,
    Win32AndDos,
}

impl Namespace {
    pub fn code(self) -> &'static str {
        match self {
            Namespace::Posix => "posix",
            Namespace::Win32 => "win32",
            Namespace::Dos => "dos",
            Namespace::Win32AndDos => "win32_dos",
        }
    }
}

/// `$FILE_NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    pub parent: FileReference,
    pub times: Times,
    pub allocated_size: u64,
    pub data_size: u64,
    pub file_attributes: u32,
    pub reparse_or_ea: u32,
    pub namespace: Namespace,
    pub name: String,
}

impl FileName {
    pub fn parse(v: &[u8]) -> Result<Self, ValueError> {
        need(types::FILE_NAME, v, 0x42)?;
        let units = v[0x40];
        let end = 0x42 + 2 * usize::from(units);
        if units == 0 || end > v.len() {
            return Err(ValueError::FileNameLength {
                len: v.len(),
                name_units: units,
            });
        }
        let namespace = match v[0x41] {
            0 => Namespace::Posix,
            1 => Namespace::Win32,
            2 => Namespace::Dos,
            3 => Namespace::Win32AndDos,
            n => return Err(ValueError::Namespace(n)),
        };
        Ok(FileName {
            parent: FileReference::from_u64(le64(v, 0)),
            times: times(v, 8),
            allocated_size: le64(v, 0x28),
            data_size: le64(v, 0x30),
            file_attributes: le32(v, 0x38),
            reparse_or_ea: le32(v, 0x3C),
            namespace,
            name: utf16_lossy(&v[0x42..end]),
        })
    }

    /// Como [`FileName::parse`], y además las unidades UTF-16 del nombre tal como están en disco
    /// (sin pérdida: los sustitutos sueltos se conservan; #25, pregunta abierta 13 de INV-01).
    pub fn parse_with_units(v: &[u8]) -> Result<(Self, Vec<u16>), ValueError> {
        let f = FileName::parse(v)?;
        let end = 0x42 + 2 * usize::from(v[0x40]);
        let units = v[0x42..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Ok((f, units))
    }
}

/// `$OBJECT_ID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectId {
    pub object_id: [u8; 16],
    /// Volumen de origen, objeto de origen y dominio (si el valor los incluye).
    pub birth: Option<[[u8; 16]; 3]>,
}

impl ObjectId {
    pub fn parse(v: &[u8]) -> Result<Self, ValueError> {
        need(types::OBJECT_ID, v, 16)?;
        let g = |o: usize| {
            let mut b = [0u8; 16];
            b.copy_from_slice(&v[o..o + 16]);
            b
        };
        Ok(ObjectId {
            object_id: g(0),
            birth: (v.len() >= 64).then(|| [g(16), g(32), g(48)]),
        })
    }
}

/// `$VOLUME_INFORMATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeInformation {
    pub major: u8,
    pub minor: u8,
    pub flags: u16,
}

impl VolumeInformation {
    pub fn parse(v: &[u8]) -> Result<Self, ValueError> {
        need(types::VOLUME_INFORMATION, v, 12)?;
        Ok(VolumeInformation {
            major: v[8],
            minor: v[9],
            flags: le16(v, 10),
        })
    }
}

/// Entrada de `$ATTRIBUTE_LIST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrListEntry {
    pub type_code: u32,
    pub name: String,
    pub start_vcn: u64,
    /// Registro que contiene el atributo (con su número de secuencia esperado).
    pub reference: FileReference,
    pub id: u16,
    /// Desplazamiento de la entrada en la lista.
    pub offset: usize,
}

/// Longitud mínima de una entrada de `$ATTRIBUTE_LIST` (cabecera de 0x1A redondeada a 8).
pub const ATTR_LIST_ENTRY_MIN: usize = 0x20;
/// Máximo de entradas de una lista (cada una ocupa al menos 32 bytes de una lista de 1 MiB).
pub const MAX_ATTR_LIST_ENTRIES: usize = 32768;

/// Entrada de `$ATTRIBUTE_LIST` mal formada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrListError {
    /// Quedan bytes que no forman una entrada completa.
    Truncated { at: usize },
    /// Longitud menor que la mínima, no múltiplo de 8 o que se sale de la lista.
    EntryLength { at: usize, length: u16 },
    /// Nombre fuera de la entrada o solapado con su cabecera.
    EntryName { at: usize },
    /// Tipo 0 o marca de fin dentro de la lista.
    EntryType { at: usize, type_code: u32 },
    /// Más de [`MAX_ATTR_LIST_ENTRIES`] entradas.
    TooManyEntries,
}

impl AttrListError {
    pub fn code(&self) -> &'static str {
        match self {
            AttrListError::Truncated { .. } => "attr_list_truncated",
            AttrListError::EntryLength { .. } => "attr_list_entry_length",
            AttrListError::EntryName { .. } => "attr_list_entry_name",
            AttrListError::EntryType { .. } => "attr_list_entry_type",
            AttrListError::TooManyEntries => "attr_list_too_many_entries",
        }
    }
}

impl fmt::Display for AttrListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AttrListError::Truncated { at } => {
                write!(f, "$ATTRIBUTE_LIST: entrada incompleta en el byte {at}")
            }
            AttrListError::EntryLength { at, length } => write!(
                f,
                "$ATTRIBUTE_LIST: entrada en el byte {at} con longitud {length} no válida"
            ),
            AttrListError::EntryName { at } => {
                write!(
                    f,
                    "$ATTRIBUTE_LIST: nombre fuera de la entrada en el byte {at}"
                )
            }
            AttrListError::EntryType { at, type_code } => write!(
                f,
                "$ATTRIBUTE_LIST: tipo 0x{type_code:X} no válido en el byte {at}"
            ),
            AttrListError::TooManyEntries => {
                write!(
                    f,
                    "$ATTRIBUTE_LIST con más de {MAX_ATTR_LIST_ENTRIES} entradas"
                )
            }
        }
    }
}

/// Entradas de una `$ATTRIBUTE_LIST` completa (contenido del atributo). Se detiene en el primer
/// error y devuelve también las entradas válidas anteriores.
pub fn parse_attribute_list(v: &[u8]) -> (Vec<AttrListEntry>, Option<AttrListError>) {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < v.len() {
        if out.len() >= MAX_ATTR_LIST_ENTRIES {
            return (out, Some(AttrListError::TooManyEntries));
        }
        if v.len() - at < ATTR_LIST_ENTRY_MIN {
            // Relleno final a ceros (alineación) admitido; cualquier otra cosa no.
            if v[at..].iter().all(|&b| b == 0) {
                break;
            }
            return (out, Some(AttrListError::Truncated { at }));
        }
        let type_code = le32(v, at);
        let length = le16(v, at + 4);
        let len = usize::from(length);
        if type_code == 0 || type_code == types::END {
            return (out, Some(AttrListError::EntryType { at, type_code }));
        }
        if len < ATTR_LIST_ENTRY_MIN || !len.is_multiple_of(8) || at + len > v.len() {
            return (out, Some(AttrListError::EntryLength { at, length }));
        }
        let name_units = usize::from(v[at + 6]);
        let name_off = usize::from(v[at + 7]);
        let name = if name_units == 0 {
            String::new()
        } else {
            if name_off < 0x1A || name_off + 2 * name_units > len {
                return (out, Some(AttrListError::EntryName { at }));
            }
            utf16_lossy(&v[at + name_off..at + name_off + 2 * name_units])
        };
        out.push(AttrListEntry {
            type_code,
            name,
            start_vcn: le64(v, at + 8),
            reference: FileReference::from_u64(le64(v, at + 0x10)),
            id: le16(v, at + 0x18),
            offset: at,
        });
        at += len;
    }
    (out, None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testvol::xorshift;

    pub(crate) fn file_name_value(parent: u64, name: &str, ns: u8) -> Vec<u8> {
        let u: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut v = vec![0u8; 0x42 + u.len()];
        v[0..8].copy_from_slice(&parent.to_le_bytes());
        for (i, t) in [10u64, 20, 30, 40].iter().enumerate() {
            v[8 + 8 * i..16 + 8 * i].copy_from_slice(&t.to_le_bytes());
        }
        v[0x28..0x30].copy_from_slice(&8192u64.to_le_bytes());
        v[0x30..0x38].copy_from_slice(&5000u64.to_le_bytes());
        v[0x38..0x3C].copy_from_slice(&0x20u32.to_le_bytes());
        v[0x40] = (u.len() / 2) as u8;
        v[0x41] = ns;
        v[0x42..].copy_from_slice(&u);
        v
    }

    /// Entrada de `$ATTRIBUTE_LIST`.
    pub(crate) fn list_entry(t: u32, name: &str, vcn: u64, rec: u64, seq: u16, id: u16) -> Vec<u8> {
        let u: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (0x1A + u.len()).div_ceil(8) * 8;
        let mut e = vec![0u8; len];
        e[0..4].copy_from_slice(&t.to_le_bytes());
        e[4..6].copy_from_slice(&(len as u16).to_le_bytes());
        e[6] = (u.len() / 2) as u8;
        e[7] = 0x1A;
        e[8..16].copy_from_slice(&vcn.to_le_bytes());
        e[0x10..0x18].copy_from_slice(&(rec | (u64::from(seq) << 48)).to_le_bytes());
        e[0x18..0x1A].copy_from_slice(&id.to_le_bytes());
        e[0x1A..0x1A + u.len()].copy_from_slice(&u);
        e
    }

    #[test]
    fn standard_information_versions() {
        let mut v = vec![0u8; 72];
        v[0..8].copy_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&0x20u32.to_le_bytes());
        v[0x34..0x38].copy_from_slice(&0x101u32.to_le_bytes());
        let si = StandardInformation::parse(&v).unwrap();
        assert_eq!(si.times.created, 133_000_000_000_000_000);
        assert_eq!(si.file_attributes, 0x20);
        assert_eq!(si.v3.unwrap().security_id, 0x101);
        assert!(StandardInformation::parse(&v[..48]).unwrap().v3.is_none());
        assert_eq!(
            StandardInformation::parse(&v[..47]).unwrap_err().code(),
            "value_too_short"
        );
        assert_eq!(
            filetime_to_unix(133_000_000_000_000_000),
            Some((1_655_526_400, 0))
        );
        assert_eq!(filetime_to_unix(0), None);
    }

    #[test]
    fn file_name_bounds_and_namespaces() {
        let v = file_name_value(0x0005_0000_0000_0005, "configuración.ini", 1);
        let f = FileName::parse(&v).unwrap();
        assert_eq!(f.name, "configuración.ini");
        assert_eq!(
            f.parent,
            FileReference {
                record: 5,
                sequence: 5
            }
        );
        assert_eq!(f.namespace, Namespace::Win32);
        assert_eq!((f.allocated_size, f.data_size), (8192, 5000));
        assert_eq!(f.times.accessed, 40);
        let mut bad = v.clone();
        bad[0x41] = 4;
        assert_eq!(FileName::parse(&bad).unwrap_err(), ValueError::Namespace(4));
        let mut bad = v.clone();
        bad[0x40] = 200;
        assert_eq!(
            FileName::parse(&bad).unwrap_err().code(),
            "file_name_length"
        );
        let mut bad = v.clone();
        bad[0x40] = 0;
        assert_eq!(
            FileName::parse(&bad).unwrap_err().code(),
            "file_name_length"
        );
        assert_eq!(
            FileName::parse(&v[..0x41]).unwrap_err().code(),
            "value_too_short"
        );
    }

    #[test]
    fn object_id_and_volume_information() {
        let o = ObjectId::parse(&[7u8; 16]).unwrap();
        assert_eq!(o.object_id, [7; 16]);
        assert!(o.birth.is_none());
        let o = ObjectId::parse(&(0..64).collect::<Vec<u8>>()).unwrap();
        assert_eq!(o.birth.unwrap()[2][0], 48);
        assert!(ObjectId::parse(&[0; 15]).is_err());
        let v = VolumeInformation::parse(&[0, 0, 0, 0, 0, 0, 0, 0, 3, 1, 0x01, 0x80]).unwrap();
        assert_eq!((v.major, v.minor, v.flags), (3, 1, 0x8001));
        assert!(VolumeInformation::parse(&[0; 11]).is_err());
    }

    #[test]
    fn attribute_list_entries() {
        let mut v = list_entry(0x10, "", 0, 40, 3, 0);
        v.extend(list_entry(0x80, "", 0, 41, 1, 2));
        v.extend(list_entry(0x80, "flujo", 300, 42, 1, 0));
        let (e, err) = parse_attribute_list(&v);
        assert_eq!(err, None);
        assert_eq!(e.len(), 3);
        assert_eq!(e[2].name, "flujo");
        assert_eq!(e[2].start_vcn, 300);
        assert_eq!(
            e[1].reference,
            FileReference {
                record: 41,
                sequence: 1
            }
        );
        assert_eq!(e[1].offset, 0x20);
        // Relleno a ceros final admitido; bytes sueltos no.
        let mut p = v.clone();
        p.extend([0u8; 8]);
        assert_eq!(parse_attribute_list(&p).1, None);
        let mut p = v.clone();
        p.extend([1u8; 8]);
        assert_eq!(
            parse_attribute_list(&p).1.unwrap().code(),
            "attr_list_truncated"
        );
        // Longitud 0, impar o que se sale; nombre fuera; tipo 0.
        for (at, val, code) in [
            (4usize, 0u8, "attr_list_entry_length"),
            (4, 0x21, "attr_list_entry_length"),
            (5, 0x10, "attr_list_entry_length"),
            (6, 40, "attr_list_entry_name"),
            (7, 2, "attr_list_entry_name"),
            (0, 0, "attr_list_entry_type"),
        ] {
            let mut p = v.clone();
            p[0x40 + at] = val;
            if at == 0 {
                p[0x40..0x44].fill(0);
            }
            let (e, err) = parse_attribute_list(&p);
            assert_eq!(err.unwrap().code(), code, "{at}");
            assert_eq!(e.len(), 2);
        }
    }

    #[test]
    fn values_fuzz_never_panic() {
        let mut rng = xorshift(0x7A1E_0001);
        for _ in 0..20_000 {
            let n = (rng() % 160) as usize;
            let v: Vec<u8> = (0..n).map(|_| rng() as u8).collect();
            let _ = StandardInformation::parse(&v);
            if let Ok(f) = FileName::parse(&v) {
                assert!(!f.name.is_empty());
            }
            let _ = ObjectId::parse(&v);
            let _ = VolumeInformation::parse(&v);
            let (e, _) = parse_attribute_list(&v);
            assert!(e.len() <= n / ATTR_LIST_ENTRY_MIN);
        }
    }

    #[test]
    fn file_name_units_are_lossless() {
        let units = [0x61u16, 0xD800, 0x62, 0xDC00];
        let v = crate::testvol::file_name_units(5 | (5 << 48), &units, 1, 0x20, 7);
        let (f, u) = FileName::parse_with_units(&v).unwrap();
        assert_eq!(u, units);
        assert_eq!(f.name, "a\u{FFFD}b\u{FFFD}");
        assert_eq!(f, FileName::parse(&v).unwrap());
        assert!(FileName::parse_with_units(&v[..0x43]).is_err());
    }
}
