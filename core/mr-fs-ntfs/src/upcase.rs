//! Tabla de mayúsculas de NTFS (`$UpCase`, registro 10) y comparación de nombres.
//!
//! NTFS compara los nombres de fichero sin distinguir mayúsculas con la tabla `$UpCase` del propio
//! volumen: 65 536 unidades UTF-16LE (128 KiB), la mayúscula de cada unidad de código. Se usa la
//! del volumen si se puede leer entera con datos fiables, mide exactamente 128 KiB y pasa una
//! comprobación de coherencia (`a`..`z` → `A`..`Z`, las letras mayúsculas ASCII y los dígitos se
//! quedan igual). Si no, se usa un **plegado simple** documentado y se informa el motivo:
//! cada unidad que no es un sustituto (U+D800..U+DFFF) y cuyo `char::to_uppercase` es un único
//! carácter del plano básico pasa a ese carácter; las demás se quedan igual. Coincide con la tabla
//! de Windows en ASCII y en las letras latinas con diacríticos habituales, pero no en todos los
//! casos especiales (p. ej. la `ß` o letras sin mayúscula de un solo carácter).
//!
//! **Colación de `$I30`** (`COLLATION_FILE_NAME`, regla 1): las unidades UTF-16 se comparan una a
//! una tras pasarlas a mayúsculas; a igualdad de prefijo, el nombre más corto va antes. Dos nombres
//! que solo difieren en mayúsculas (posibles en el espacio de nombres POSIX) son iguales para la
//! colación; para ordenarlos de forma estable se desempata con las unidades sin plegar.

use std::cmp::Ordering;

use crate::attr::types;
use crate::NtfsVolume;

/// Registro de `$UpCase`.
pub const UPCASE_RECORD: u64 = 10;
/// Unidades de la tabla.
pub const UPCASE_UNITS: usize = 65_536;

/// Origen de la tabla en uso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpcaseSource {
    /// `$UpCase` del volumen.
    Volume,
    /// Plegado simple; con el motivo por el que no se usa la del volumen.
    SimpleFold(String),
}

/// Tabla de mayúsculas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upcase {
    table: Option<Vec<u16>>,
    source: UpcaseSource,
}

/// Mayúscula por el plegado simple.
fn simple_fold(u: u16) -> u16 {
    if (0xD800..=0xDFFF).contains(&u) {
        return u;
    }
    let Some(c) = char::from_u32(u32::from(u)) else {
        return u;
    };
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(x), None) if (x as u32) < 0x1_0000 => x as u32 as u16,
        _ => u,
    }
}

impl Upcase {
    /// Plegado simple (sin tabla del volumen).
    pub fn simple(reason: impl Into<String>) -> Self {
        Upcase {
            table: None,
            source: UpcaseSource::SimpleFold(reason.into()),
        }
    }

    /// Tabla a partir de los 128 KiB de `$UpCase`; `Err` con el motivo si no es válida.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 2 * UPCASE_UNITS {
            return Err(format!(
                "$UpCase mide {} bytes (se esperaban {})",
                bytes.len(),
                2 * UPCASE_UNITS
            ));
        }
        let table: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let ok = (u16::from(b'a')..=u16::from(b'z')).all(|u| table[usize::from(u)] == u - 32)
            && (u16::from(b'A')..=u16::from(b'Z')).all(|u| table[usize::from(u)] == u)
            && (u16::from(b'0')..=u16::from(b'9')).all(|u| table[usize::from(u)] == u);
        if !ok {
            return Err("$UpCase no pasa la comprobación de coherencia (ASCII)".into());
        }
        Ok(Upcase {
            table: Some(table),
            source: UpcaseSource::Volume,
        })
    }

    /// Carga `$UpCase` del volumen o, si no es utilizable, el plegado simple.
    pub fn load(vol: &NtfsVolume) -> Self {
        if vol.record_count() <= UPCASE_RECORD {
            return Upcase::simple("la MFT no tiene el registro 10 ($UpCase)");
        }
        let stream = match vol
            .file(UPCASE_RECORD)
            .and_then(|f| vol.attribute_stream(&f, types::DATA, ""))
        {
            Ok(s) => s,
            Err(e) => return Upcase::simple(format!("$UpCase no se puede abrir: {e}")),
        };
        if stream.size() != 2 * UPCASE_UNITS as u64 {
            return Upcase::simple(format!(
                "$UpCase mide {} bytes (se esperaban {})",
                stream.size(),
                2 * UPCASE_UNITS
            ));
        }
        match stream.read(0, 2 * UPCASE_UNITS) {
            Ok(c) if c.all_trustworthy() => {
                Upcase::from_bytes(&c.data).unwrap_or_else(Upcase::simple)
            }
            Ok(c) => Upcase::simple(format!(
                "$UpCase tiene tramos no fiables ({})",
                c.untrusted()
                    .next()
                    .map(|r| r.to_string())
                    .unwrap_or_default()
            )),
            Err(e) => Upcase::simple(format!("$UpCase no se puede leer: {e}")),
        }
    }

    pub fn source(&self) -> &UpcaseSource {
        &self.source
    }

    /// Mayúscula de una unidad UTF-16.
    pub fn up(&self, u: u16) -> u16 {
        match &self.table {
            Some(t) => t[usize::from(u)],
            None => simple_fold(u),
        }
    }

    /// Comparación sin distinguir mayúsculas (la colación de `$I30`).
    pub fn compare(&self, a: &[u16], b: &[u16]) -> Ordering {
        for (x, y) in a.iter().zip(b) {
            match self.up(*x).cmp(&self.up(*y)) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        a.len().cmp(&b.len())
    }

    /// Orden total y estable: la colación y, a igualdad, las unidades sin plegar.
    pub fn compare_stable(&self, a: &[u16], b: &[u16]) -> Ordering {
        self.compare(a, b).then_with(|| a.cmp(b))
    }

    /// `true` si los dos nombres son iguales sin distinguir mayúsculas.
    pub fn eq_ignore_case(&self, a: &[u16], b: &[u16]) -> bool {
        a.len() == b.len() && self.compare(a, b) == Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Tabla de pruebas: la identidad salvo ASCII y una letra con tilde.
    pub(crate) fn test_table() -> Vec<u8> {
        let mut t: Vec<u16> = (0..=u16::MAX).collect();
        for c in u16::from(b'a')..=u16::from(b'z') {
            t[usize::from(c)] = c - 32;
        }
        t[0xE1] = 0xC1; // á → Á
        t.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn collation_folds_case_and_orders_by_units() {
        let simple = Upcase::simple("prueba");
        assert_eq!(simple.compare(&u("abc"), &u("ABC")), Ordering::Equal);
        assert_eq!(simple.compare(&u("ab"), &u("abc")), Ordering::Less);
        assert_eq!(simple.compare(&u("b"), &u("A")), Ordering::Greater);
        assert_eq!(simple.compare(&u("año"), &u("AÑO")), Ordering::Equal);
        // Desempate estable: mayúsculas (unidades menores) antes.
        assert_eq!(simple.compare_stable(&u("ABC"), &u("abc")), Ordering::Less);
        // '_' (0x5F) va después de las mayúsculas plegadas: colación por unidades plegadas.
        assert_eq!(simple.compare(&u("a_"), &u("aZ")), Ordering::Greater);
        // Sustitutos sueltos: se comparan tal cual, sin pánico.
        assert_eq!(simple.compare(&[0xD800], &[0xD800]), Ordering::Equal);
        assert_eq!(simple.up(0xDC00), 0xDC00);
        // ß no tiene mayúscula de un solo carácter: se queda igual.
        assert_eq!(simple.up(0xDF), 0xDF);
    }

    #[test]
    fn volume_table_is_validated() {
        let t = Upcase::from_bytes(&test_table()).unwrap();
        assert_eq!(t.source(), &UpcaseSource::Volume);
        assert!(t.eq_ignore_case(&u("Álbum"), &u("álbum")));
        // La tabla del volumen manda: ñ no está en ella, así que no se pliega.
        assert!(!t.eq_ignore_case(&u("ñ"), &u("Ñ")));
        assert!(Upcase::from_bytes(&test_table()[..100]).is_err());
        let mut bad = test_table();
        bad[2 * 0x61] = 0x61; // 'a' sin plegar
        assert!(Upcase::from_bytes(&bad).is_err());
    }
}
