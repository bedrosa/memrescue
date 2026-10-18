//! Cabeceras de atributo de un registro FILE: iterador acotado.
//!
//! Disposición pública de la cabecera común (little-endian): tipo (u32, 0x00; `0xFFFFFFFF` marca el
//! fin), longitud total (u32, 0x04), no residente (u8, 0x08), longitud del nombre en unidades UTF-16
//! (u8, 0x09), desplazamiento del nombre (u16, 0x0A), indicadores (u16, 0x0C: 0x0001 comprimido,
//! 0x4000 cifrado, 0x8000 disperso) e identificador (u16, 0x0E).
//!
//! - Residente: longitud del valor (u32, 0x10), desplazamiento del valor (u16, 0x14), indicador de
//!   indexado (u8, 0x16).
//! - No residente: VCN inicial (u64, 0x10), VCN final (u64, 0x18), desplazamiento de los *mapping
//!   pairs* (u16, 0x20), unidad de compresión (u16, 0x22), tamaño asignado (u64, 0x28), tamaño de
//!   los datos (u64, 0x30), tamaño inicializado (u64, 0x38) y, si está comprimido o disperso y la
//!   cabecera lo incluye, tamaño comprimido (u64, 0x40).
//!
//! El iterador nunca lee fuera del área en uso del registro: cada atributo debe caber entero, medir
//! al menos su cabecera y ser múltiplo de 8; tras el primer error se detiene. Esta capa no
//! interpreta el contenido de los atributos (lo hacen #21 y #22) salvo lo necesario para localizar
//! la MFT y leer la etiqueta y la versión del volumen.

use std::fmt;

/// Tipos de atributo públicos.
pub mod types {
    pub const STANDARD_INFORMATION: u32 = 0x10;
    pub const ATTRIBUTE_LIST: u32 = 0x20;
    pub const FILE_NAME: u32 = 0x30;
    pub const OBJECT_ID: u32 = 0x40;
    pub const SECURITY_DESCRIPTOR: u32 = 0x50;
    pub const VOLUME_NAME: u32 = 0x60;
    pub const VOLUME_INFORMATION: u32 = 0x70;
    pub const DATA: u32 = 0x80;
    pub const INDEX_ROOT: u32 = 0x90;
    pub const INDEX_ALLOCATION: u32 = 0xA0;
    pub const BITMAP: u32 = 0xB0;
    pub const REPARSE_POINT: u32 = 0xC0;
    pub const EA_INFORMATION: u32 = 0xD0;
    pub const EA: u32 = 0xE0;
    pub const LOGGED_UTILITY_STREAM: u32 = 0x100;
    /// Marca de fin de la lista de atributos.
    pub const END: u32 = 0xFFFF_FFFF;
}

/// Nombre público de un tipo de atributo.
pub fn type_name(t: u32) -> Option<&'static str> {
    Some(match t {
        types::STANDARD_INFORMATION => "$STANDARD_INFORMATION",
        types::ATTRIBUTE_LIST => "$ATTRIBUTE_LIST",
        types::FILE_NAME => "$FILE_NAME",
        types::OBJECT_ID => "$OBJECT_ID",
        types::SECURITY_DESCRIPTOR => "$SECURITY_DESCRIPTOR",
        types::VOLUME_NAME => "$VOLUME_NAME",
        types::VOLUME_INFORMATION => "$VOLUME_INFORMATION",
        types::DATA => "$DATA",
        types::INDEX_ROOT => "$INDEX_ROOT",
        types::INDEX_ALLOCATION => "$INDEX_ALLOCATION",
        types::BITMAP => "$BITMAP",
        types::REPARSE_POINT => "$REPARSE_POINT",
        types::EA_INFORMATION => "$EA_INFORMATION",
        types::EA => "$EA",
        types::LOGGED_UTILITY_STREAM => "$LOGGED_UTILITY_STREAM",
        _ => return None,
    })
}

/// Texto UTF-16LE (las unidades no válidas se sustituyen por U+FFFD; un byte impar final se
/// ignora).
pub fn utf16_lossy(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Indicadores de la cabecera de atributo.
pub mod attr_flags {
    pub const COMPRESSED: u16 = 0x0001;
    pub const ENCRYPTED: u16 = 0x4000;
    pub const SPARSE: u16 = 0x8000;
}

/// Cabecera mínima de un atributo residente.
pub const RESIDENT_HEADER: usize = 0x18;
/// Cabecera mínima de un atributo no residente.
pub const NON_RESIDENT_HEADER: usize = 0x40;

/// Un atributo del registro, con referencias al búfer del registro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute<'a> {
    pub type_code: u32,
    /// Desplazamiento del atributo dentro del registro.
    pub offset: usize,
    pub length: usize,
    pub flags: u16,
    pub id: u16,
    /// Nombre en UTF-16LE (bytes crudos; vacío si no tiene nombre).
    pub name_utf16: &'a [u8],
    pub body: AttrBody<'a>,
}

/// Forma del contenido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrBody<'a> {
    Resident { value: &'a [u8], indexed: bool },
    NonResident(NonResident<'a>),
}

/// Campos de un atributo no residente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonResident<'a> {
    pub start_vcn: u64,
    pub last_vcn: u64,
    /// Bytes de los *mapping pairs* (hasta el final del atributo; los decodifica [`crate::runs`]).
    pub mapping_pairs: &'a [u8],
    pub compression_unit: u16,
    pub allocated_size: u64,
    pub data_size: u64,
    pub initialized_size: u64,
    pub compressed_size: Option<u64>,
}

impl Attribute<'_> {
    /// Nombre decodificado (las unidades UTF-16 no válidas se sustituyen por U+FFFD).
    pub fn name(&self) -> String {
        utf16_lossy(self.name_utf16)
    }

    pub fn is_named(&self) -> bool {
        !self.name_utf16.is_empty()
    }

    pub fn is_resident(&self) -> bool {
        matches!(self.body, AttrBody::Resident { .. })
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & attr_flags::COMPRESSED != 0
    }

    pub fn is_sparse(&self) -> bool {
        self.flags & attr_flags::SPARSE != 0
    }

    pub fn is_encrypted(&self) -> bool {
        self.flags & attr_flags::ENCRYPTED != 0
    }

    /// Valor residente, si lo es.
    pub fn resident_value(&self) -> Option<&[u8]> {
        match &self.body {
            AttrBody::Resident { value, .. } => Some(value),
            AttrBody::NonResident(_) => None,
        }
    }

    pub fn non_resident(&self) -> Option<&NonResident<'_>> {
        match &self.body {
            AttrBody::NonResident(n) => Some(n),
            AttrBody::Resident { .. } => None,
        }
    }
}

/// Atributo mal formado: el recorrido se detiene en él.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrError {
    /// No cabe la cabecera (o la marca de fin) dentro de lo usado: falta la marca de fin.
    MissingEnd { offset: usize },
    /// Longitud menor que la cabecera, no múltiplo de 8 o que se sale de lo usado.
    Length { offset: usize, length: u32 },
    /// Byte de no residente distinto de 0 y 1.
    FormFlag { offset: usize, value: u8 },
    /// Nombre fuera del atributo o solapado con la cabecera.
    Name { offset: usize },
    /// Valor residente fuera del atributo o solapado con la cabecera.
    ResidentValue { offset: usize },
    /// Desplazamiento de los *mapping pairs* fuera del atributo o dentro de la cabecera.
    MappingPairs { offset: usize },
    /// VCN final menor que la inicial (salvo atributo vacío) o tamaños incoherentes.
    NonResidentSizes { offset: usize },
}

impl AttrError {
    pub fn code(&self) -> &'static str {
        match self {
            AttrError::MissingEnd { .. } => "attr_missing_end",
            AttrError::Length { .. } => "attr_length",
            AttrError::FormFlag { .. } => "attr_form_flag",
            AttrError::Name { .. } => "attr_name",
            AttrError::ResidentValue { .. } => "attr_resident_value",
            AttrError::MappingPairs { .. } => "attr_mapping_pairs",
            AttrError::NonResidentSizes { .. } => "attr_non_resident_sizes",
        }
    }

    pub fn offset(&self) -> usize {
        match *self {
            AttrError::MissingEnd { offset }
            | AttrError::Length { offset, .. }
            | AttrError::FormFlag { offset, .. }
            | AttrError::Name { offset }
            | AttrError::ResidentValue { offset }
            | AttrError::MappingPairs { offset }
            | AttrError::NonResidentSizes { offset } => offset,
        }
    }
}

impl fmt::Display for AttrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.offset();
        match self {
            AttrError::MissingEnd { .. } => {
                write!(
                    f,
                    "atributo en 0x{at:X}: falta la marca de fin dentro de lo usado"
                )
            }
            AttrError::Length { length, .. } => {
                write!(f, "atributo en 0x{at:X}: longitud {length} no válida")
            }
            AttrError::FormFlag { value, .. } => {
                write!(f, "atributo en 0x{at:X}: indicador de no residente {value}")
            }
            AttrError::Name { .. } => write!(f, "atributo en 0x{at:X}: nombre fuera de límites"),
            AttrError::ResidentValue { .. } => {
                write!(f, "atributo en 0x{at:X}: valor residente fuera de límites")
            }
            AttrError::MappingPairs { .. } => {
                write!(f, "atributo en 0x{at:X}: mapping pairs fuera de límites")
            }
            AttrError::NonResidentSizes { .. } => {
                write!(f, "atributo en 0x{at:X}: VCN o tamaños incoherentes")
            }
        }
    }
}

/// Iterador de atributos de un registro (ver el módulo).
#[derive(Debug, Clone)]
pub struct Attributes<'a> {
    rec: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> Attributes<'a> {
    /// `rec` es el área en uso del registro (ya con los fixups deshechos); `first` el desplazamiento
    /// del primer atributo.
    pub fn new(rec: &'a [u8], first: usize) -> Self {
        Attributes {
            rec,
            pos: first,
            done: false,
        }
    }

    fn le16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.rec[o], self.rec[o + 1]])
    }

    fn le32(&self, o: usize) -> u32 {
        u32::from_le_bytes([
            self.rec[o],
            self.rec[o + 1],
            self.rec[o + 2],
            self.rec[o + 3],
        ])
    }

    fn le64(&self, o: usize) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.rec[o..o + 8]);
        u64::from_le_bytes(b)
    }

    fn parse_one(&self, at: usize) -> Result<Option<Attribute<'a>>, AttrError> {
        let end = self.rec.len();
        if at.checked_add(4).is_none_or(|e| e > end) {
            return Err(AttrError::MissingEnd { offset: at });
        }
        let type_code = self.le32(at);
        if type_code == types::END {
            return Ok(None);
        }
        if at + RESIDENT_HEADER > end {
            return Err(AttrError::MissingEnd { offset: at });
        }
        let length = self.le32(at + 4);
        let len = length as usize;
        let form = self.rec[at + 8];
        let min = match form {
            0 => RESIDENT_HEADER,
            1 => NON_RESIDENT_HEADER,
            v => {
                return Err(AttrError::FormFlag {
                    offset: at,
                    value: v,
                })
            }
        };
        if len < min || !len.is_multiple_of(8) || at.checked_add(len).is_none_or(|e| e > end) {
            return Err(AttrError::Length { offset: at, length });
        }
        let a = &self.rec[at..at + len];
        let name_len = usize::from(self.rec[at + 9]) * 2;
        let name_off = usize::from(self.le16(at + 0x0A));
        let flags = self.le16(at + 0x0C);
        let id = self.le16(at + 0x0E);
        let name_utf16 = if name_len == 0 {
            &a[..0]
        } else {
            if name_off < min || name_off + name_len > len {
                return Err(AttrError::Name { offset: at });
            }
            &a[name_off..name_off + name_len]
        };
        let body = if form == 0 {
            let vlen = self.le32(at + 0x10) as usize;
            let voff = usize::from(self.le16(at + 0x14));
            if voff < RESIDENT_HEADER || voff.checked_add(vlen).is_none_or(|e| e > len) {
                return Err(AttrError::ResidentValue { offset: at });
            }
            if name_len > 0 && name_off < voff && name_off + name_len > voff {
                return Err(AttrError::Name { offset: at });
            }
            AttrBody::Resident {
                value: &a[voff..voff + vlen],
                indexed: self.rec[at + 0x16] != 0,
            }
        } else {
            let start_vcn = self.le64(at + 0x10);
            let last_vcn = self.le64(at + 0x18);
            let runs_off = usize::from(self.le16(at + 0x20));
            let compressed = flags & (attr_flags::COMPRESSED | attr_flags::SPARSE) != 0;
            let has_compressed_size = compressed && runs_off >= 0x48 && len >= 0x48;
            let header_end = if has_compressed_size { 0x48 } else { 0x40 };
            if flags & attr_flags::COMPRESSED != 0 && !has_compressed_size {
                return Err(AttrError::MappingPairs { offset: at });
            }
            if runs_off < header_end || runs_off > len {
                return Err(AttrError::MappingPairs { offset: at });
            }
            if name_len > 0 && name_off < runs_off && name_off + name_len > runs_off {
                return Err(AttrError::Name { offset: at });
            }
            let allocated_size = self.le64(at + 0x28);
            let data_size = self.le64(at + 0x30);
            let initialized_size = self.le64(at + 0x38);
            // Atributo vacío: VCN final = VCN inicial - 1 (p. ej. 0 y u64::MAX).
            if last_vcn < start_vcn && last_vcn.wrapping_add(1) != start_vcn {
                return Err(AttrError::NonResidentSizes { offset: at });
            }
            if start_vcn == 0 && (initialized_size > data_size || data_size > allocated_size) {
                return Err(AttrError::NonResidentSizes { offset: at });
            }
            AttrBody::NonResident(NonResident {
                start_vcn,
                last_vcn,
                mapping_pairs: &a[runs_off..],
                compression_unit: self.le16(at + 0x22),
                allocated_size,
                data_size,
                initialized_size,
                compressed_size: has_compressed_size.then(|| self.le64(at + 0x40)),
            })
        };
        Ok(Some(Attribute {
            type_code,
            offset: at,
            length: len,
            flags,
            id,
            name_utf16,
            body,
        }))
    }
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<Attribute<'a>, AttrError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.parse_one(self.pos) {
            Ok(Some(a)) => {
                // La longitud es al menos la cabecera: el recorrido siempre avanza.
                self.pos += a.length;
                Some(Ok(a))
            }
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

impl std::iter::FusedIterator for Attributes<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{flags, FileRecord};
    use crate::testvol::{non_resident, record, resident};

    fn rec(attrs: &[Vec<u8>]) -> FileRecord {
        FileRecord::parse(30, record(30, 1, flags::IN_USE, attrs, 1024), 1024).unwrap()
    }

    #[test]
    fn iterates_resident_and_non_resident_with_names() {
        let r = rec(&[
            resident(0x10, "", &[0xAA; 48]),
            resident(0x30, "", &[0xBB; 66]),
            resident(0x80, "flujo", b"hola"),
            non_resident(
                0x80,
                "",
                0,
                9,
                &[0x11, 0x0A, 0x20, 0x00],
                40960,
                40000,
                40000,
            ),
        ]);
        let v: Vec<_> = r.attributes().collect::<Result<_, _>>().unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].type_code, types::STANDARD_INFORMATION);
        assert_eq!(v[0].resident_value().unwrap().len(), 48);
        assert_eq!(v[2].name(), "flujo");
        assert!(v[2].is_named() && v[2].is_resident());
        assert_eq!(v[2].resident_value().unwrap(), b"hola");
        let nr = v[3].non_resident().unwrap();
        assert_eq!((nr.start_vcn, nr.last_vcn), (0, 9));
        assert_eq!((nr.allocated_size, nr.data_size), (40960, 40000));
        assert_eq!(&nr.mapping_pairs[..4], &[0x11, 0x0A, 0x20, 0x00]);
        assert_eq!(type_name(v[3].type_code), Some("$DATA"));
        assert_eq!(type_name(0x1234), None);
        // Cada atributo empieza donde acaba el anterior.
        for w in v.windows(2) {
            assert_eq!(w[0].offset + w[0].length, w[1].offset);
        }
    }

    /// Edita el área en uso de un registro ya interpretado y recorre de nuevo sus atributos.
    fn walk(edit: impl FnOnce(&mut Vec<u8>, usize)) -> Vec<Result<u32, &'static str>> {
        let r = rec(&[
            resident(0x30, "nombre", &[1; 16]),
            resident(0x80, "", &[2; 8]),
        ]);
        let mut used = r.data()[..r.header().bytes_in_use as usize].to_vec();
        let first = usize::from(r.header().attrs_offset);
        edit(&mut used, first);
        Attributes::new(&used, first)
            .map(|x| x.map(|a| a.type_code).map_err(|e| e.code()))
            .collect()
    }

    #[test]
    fn bounds_are_enforced() {
        assert_eq!(walk(|_, _| {}), vec![Ok(0x30), Ok(0x80)]);
        // Longitud 0, no múltiplo de 8, menor que la cabecera o que se sale de lo usado.
        for bad in [0u32, 0x19, 0x10, 0x1000] {
            let r = walk(|u, f| u[f + 4..f + 8].copy_from_slice(&bad.to_le_bytes()));
            assert_eq!(r, vec![Err("attr_length")], "{bad}");
        }
        // Sin marca de fin: el área en uso termina justo tras el último atributo.
        let r = walk(|u, _| {
            let n = u.len() - 8;
            u.truncate(n);
        });
        assert_eq!(r, vec![Ok(0x30), Ok(0x80), Err("attr_missing_end")]);
        let r = walk(|u, f| u[f + 8] = 2);
        assert_eq!(r, vec![Err("attr_form_flag")]);
        // Nombre fuera del atributo o dentro de la cabecera.
        let r = walk(|u, f| u[f + 9] = 200);
        assert_eq!(r, vec![Err("attr_name")]);
        let r = walk(|u, f| u[f + 0x0A..f + 0x0C].copy_from_slice(&4u16.to_le_bytes()));
        assert_eq!(r, vec![Err("attr_name")]);
        // Valor residente fuera del atributo o dentro de la cabecera.
        let r = walk(|u, f| u[f + 0x10..f + 0x14].copy_from_slice(&1000u32.to_le_bytes()));
        assert_eq!(r, vec![Err("attr_resident_value")]);
        let r = walk(|u, f| u[f + 0x14..f + 0x16].copy_from_slice(&8u16.to_le_bytes()));
        assert_eq!(r, vec![Err("attr_resident_value")]);
        // Pasar a no residente un atributo de 0x30 bytes: más corto que la cabecera no residente.
        let r = walk(|u, f| u[f + 8] = 1);
        assert_eq!(r, vec![Err("attr_length")]);
    }

    #[test]
    fn non_resident_rules() {
        let check = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut a = non_resident(0x80, "", 0, 3, &[0x11, 0x04, 0x30, 0], 16384, 16000, 16000);
            edit(&mut a);
            let r = rec(&[a]);
            r.attributes()
                .map(|x| x.map(|a| a.type_code).map_err(|e| e.code()))
                .collect::<Vec<_>>()
        };
        assert_eq!(check(&|_| {}), vec![Ok(0x80)]);
        assert_eq!(
            check(&|a| a[0x20..0x22].copy_from_slice(&0x30u16.to_le_bytes())),
            vec![Err("attr_mapping_pairs")]
        );
        assert_eq!(
            check(&|a| a[0x20..0x22].copy_from_slice(&0x400u16.to_le_bytes())),
            vec![Err("attr_mapping_pairs")]
        );
        // Comprimido sin tamaño comprimido en la cabecera.
        assert_eq!(check(&|a| a[0x0C] = 0x01), vec![Err("attr_mapping_pairs")]);
        // VCN final menor que la inicial.
        assert_eq!(
            check(&|a| a[0x10..0x18].copy_from_slice(&5u64.to_le_bytes())),
            vec![Err("attr_non_resident_sizes")]
        );
        // Tamaño inicializado mayor que el de los datos.
        assert_eq!(
            check(&|a| a[0x38..0x40].copy_from_slice(&20000u64.to_le_bytes())),
            vec![Err("attr_non_resident_sizes")]
        );
        // Atributo vacío (VCN 0 y final u64::MAX) admitido.
        let ok = check(&|a| {
            a[0x18..0x20].copy_from_slice(&u64::MAX.to_le_bytes());
            a[0x28..0x40].fill(0);
        });
        assert_eq!(ok, vec![Ok(0x80)]);
    }
}
