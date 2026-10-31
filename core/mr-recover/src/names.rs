//! Nombres de destino (INV-01 §7.6): del nombre original UTF-16 del origen a un nombre válido en
//! Windows, sin que ningún nombre pueda salir del directorio de destino.
//!
//! | Caso | Tratamiento |
//! |---|---|
//! | `< > : " / \ | ? *` y los códigos 0 a 31 | `%XX` (hexadecimal del código) |
//! | Unidad UTF-16 suelta (sustituto sin pareja) | `%uXXXX` (FAT y exFAT no las admiten; así el nombre vale en cualquier destino) |
//! | `%` en un nombre que necesita escape | `%25` (en los demás se deja tal cual) |
//! | Espacio o punto final | el último carácter pasa a `%20` o `%2E` |
//! | `.` y `..` como nombre completo | `%2E` y `%2E%2E` |
//! | Nombre vacío | `[Sin nombre]` |
//! | Nombre reservado (`CON`, `PRN`, `AUX`, `NUL`, `COM0`..`COM9`, `LPT0`..`LPT9`, `COM¹`..., `CONIN$`, `CONOUT$`, `CLOCK$`), con o sin extensión | `_` tras la base: `NUL.txt` → `NUL_.txt` |
//! | Más de [`MAX_COMPONENT`] unidades | se recorta la base, se conserva la extensión y se añade `~XXXXXXXX` (CRC-32 de las unidades originales, determinista) |
//!
//! El nombre original se guarda siempre en el diario. Los sufijos de conflicto (` (2)`,
//! ` (borrado)`) se insertan antes de la extensión de los ficheros ([`with_suffix`]).

use mr_vfs::name::{display, fold_unit};

/// Longitud máxima de un componente en el destino (NTFS, FAT con nombres largos, exFAT).
pub const MAX_COMPONENT: usize = 255;
/// Unidades que se reservan al recortar para los añadidos posteriores (`.mrbad`, `.001`,
/// sufijos de conflicto).
pub const RESERVE: usize = 24;
/// Nombre de un componente vacío.
pub const EMPTY_NAME: &str = "[Sin nombre]";

/// Nombres reservados de dispositivo de Windows (en minúsculas; la misma lista que la política de
/// rutas de `mr-io`).
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "clock$", "com0", "com1", "com2", "com3",
    "com4", "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

/// Nombre de destino de un componente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestName {
    /// Nombre válido en Windows.
    pub name: String,
    /// El nombre cambió respecto al original (escape, reservado o recorte).
    pub changed: bool,
    /// Se recortó por longitud.
    pub truncated: bool,
}

fn forbidden(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20
}

/// `true` si la base (hasta el primer punto, sin espacios finales) es un nombre reservado.
pub fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let lower = stem.to_lowercase();
    RESERVED.contains(&lower.as_str())
}

/// Escapa los caracteres de un nombre (sin tratar reservados ni longitud).
fn escape_chars(units: &[u16], stream: bool) -> (String, bool) {
    let needs = char::decode_utf16(units.iter().copied()).any(|r| r.map_or(true, forbidden));
    let mut o = String::with_capacity(units.len() + 8);
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok('%') if needs => o.push_str("%25"),
            Ok(c) if forbidden(c) => o.push_str(&format!("%{:02X}", c as u32)),
            Ok(c) => o.push(c),
            Err(e) => o.push_str(&format!("%u{:04X}", e.unpaired_surrogate())),
        }
    }
    // Espacio o punto final (Windows los quita al abrir; en un flujo no importan).
    let mut changed = needs;
    if !stream {
        if let Some(last) = o.chars().last() {
            if last == ' ' || last == '.' {
                o.pop();
                o.push_str(if last == ' ' { "%20" } else { "%2E" });
                changed = true;
            }
        }
    }
    (o, changed)
}

/// Recorta `name` a `max` unidades UTF-16 conservando la extensión y añadiendo `~XXXXXXXX`.
fn truncate(name: &str, original: &[u16], max: usize, keep_ext: bool) -> String {
    let tag = format!("~{:08X}", crc_units(original));
    let (base, ext) = if keep_ext {
        split_ext(name)
    } else {
        (name, "")
    };
    // Una extensión desmesurada no se conserva.
    let ext = if ext.encode_utf16().count() > 32 {
        ""
    } else {
        ext
    };
    let budget = max.saturating_sub(tag.len() + ext.encode_utf16().count());
    let mut out = String::new();
    let mut used = 0;
    for c in base.chars() {
        let w = c.len_utf16();
        if used + w > budget {
            break;
        }
        // No se corta una secuencia `%XX` o `%uXXXX` por la mitad.
        out.push(c);
        used += w;
    }
    if let Some(p) = out.rfind('%') {
        let tail = &out[p..];
        let complete = (tail.len() >= 3 && !tail[1..].starts_with('u'))
            || (tail.starts_with("%u") && tail.len() >= 6);
        if !complete {
            out.truncate(p);
        }
    }
    // Sin espacio ni punto final antes de la marca.
    format!("{out}{tag}{ext}")
}

fn crc_units(u: &[u16]) -> u32 {
    let bytes: Vec<u8> = u.iter().flat_map(|x| x.to_le_bytes()).collect();
    mr_common::hash::crc32(&bytes)
}

/// Nombre de destino de un fichero o directorio (ver el módulo).
pub fn dest_name(units: &[u16]) -> DestName {
    if units.is_empty() {
        return DestName {
            name: EMPTY_NAME.into(),
            changed: true,
            truncated: false,
        };
    }
    if units == [0x2E] {
        return DestName {
            name: "%2E".into(),
            changed: true,
            truncated: false,
        };
    }
    if units == [0x2E, 0x2E] {
        return DestName {
            name: "%2E%2E".into(),
            changed: true,
            truncated: false,
        };
    }
    let (mut name, mut changed) = escape_chars(units, false);
    if is_reserved(&name) {
        let stem_len = name.find('.').unwrap_or(name.len());
        name.insert(stem_len, '_');
        changed = true;
    }
    let mut truncated = false;
    if name.encode_utf16().count() > MAX_COMPONENT - RESERVE {
        name = truncate(&name, units, MAX_COMPONENT - RESERVE, true);
        changed = true;
        truncated = true;
    }
    DestName {
        name,
        changed,
        truncated,
    }
}

/// Nombre de destino de un flujo alternativo (escape de caracteres; sin reservados).
pub fn stream_name(name: &str) -> DestName {
    let units: Vec<u16> = name.encode_utf16().collect();
    let (mut n, mut changed) = escape_chars(&units, true);
    let mut truncated = false;
    if n.encode_utf16().count() > MAX_COMPONENT - RESERVE {
        n = truncate(&n, &units, MAX_COMPONENT - RESERVE, false);
        changed = true;
        truncated = true;
    }
    DestName {
        name: n,
        changed,
        truncated,
    }
}

/// Fichero hermano de un flujo alternativo en un destino sin flujos: `<nombre>%3A<flujo>`,
/// recortado si hace falta.
pub fn sibling_name(main: &str, stream: &str) -> String {
    let s = format!("{main}%3A{}", stream_name(stream).name);
    if s.encode_utf16().count() > MAX_COMPONENT - 8 {
        let units: Vec<u16> = s.encode_utf16().collect();
        truncate(&s, &units, MAX_COMPONENT - 8, false)
    } else {
        s
    }
}

/// Base y extensión (con el punto) de un nombre de fichero. Un punto inicial no separa
/// extensión (`.config` no tiene extensión).
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Inserta `suffix` antes de la extensión en ficheros y al final en directorios.
pub fn with_suffix(name: &str, suffix: &str, directory: bool) -> String {
    if directory {
        return format!("{name}{suffix}");
    }
    let (b, e) = split_ext(name);
    format!("{b}{suffix}{e}")
}

/// Sufijo de conflicto número `n` (1 = primer intento con sufijo). Los ficheros borrados que
/// chocan llevan ` (borrado)`, ` (borrado 2)`...; los demás ` (2)`, ` (3)`...
pub fn conflict_suffix(n: u32, deleted: bool) -> String {
    match (deleted, n) {
        (true, 1) => " (borrado)".into(),
        (true, n) => format!(" (borrado {n})"),
        (false, n) => format!(" ({})", n + 1),
    }
}

/// Clave de comparación de un nombre en el destino: sin distinguir mayúsculas (plegado simple
/// unidad a unidad) salvo que el destino las distinga.
pub fn fold_key(name: &str, case_sensitive: bool) -> Vec<u16> {
    if case_sensitive {
        name.encode_utf16().collect()
    } else {
        name.encode_utf16().map(fold_unit).collect()
    }
}

/// Forma visible del nombre original (U+FFFD en las unidades sueltas).
pub fn original_display(units: &[u16]) -> String {
    display(units)
}

/// Unidades UTF-16 en hexadecimal (4 dígitos por unidad, en orden): forma sin pérdida del nombre
/// original en el diario cuando la forma visible no lo es.
pub fn units_hex(units: &[u16]) -> String {
    units.iter().map(|u| format!("{u:04X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn escapes_forbidden_reserved_and_trailing() {
        assert_eq!(dest_name(&u("normal.txt")).name, "normal.txt");
        assert!(!dest_name(&u("normal.txt")).changed);
        assert_eq!(dest_name(&u("100%.txt")).name, "100%.txt");
        assert_eq!(dest_name(&u("a:b")).name, "a%3Ab");
        assert_eq!(dest_name(&u("a:b%")).name, "a%3Ab%25");
        assert_eq!(dest_name(&u("a\\..\\..\\x")).name, "a%5C..%5C..%5Cx");
        assert_eq!(dest_name(&u("<>\"|?*")).name, "%3C%3E%22%7C%3F%2A");
        assert_eq!(dest_name(&u("x\u{1}")).name, "x%01");
        assert_eq!(dest_name(&u("fin.")).name, "fin%2E");
        assert_eq!(dest_name(&u("fin ")).name, "fin%20");
        assert_eq!(dest_name(&u(".")).name, "%2E");
        assert_eq!(dest_name(&u("..")).name, "%2E%2E");
        assert_eq!(dest_name(&u("...")).name, "..%2E");
        assert_eq!(dest_name(&[]).name, EMPTY_NAME);
        assert_eq!(dest_name(&u("CON")).name, "CON_");
        assert_eq!(dest_name(&u("nul.txt")).name, "nul_.txt");
        assert_eq!(dest_name(&u("Com1.tar.gz")).name, "Com1_.tar.gz");
        assert_eq!(dest_name(&u("lpt¹")).name, "lpt¹_");
        assert_eq!(dest_name(&u("console")).name, "console");
        assert_eq!(dest_name(&[0x62, 0xD800, 0x63]).name, "b%uD800c");
        assert_eq!(dest_name(&u("ñandú 😀.bin")).name, "ñandú 😀.bin");
        // Ningún resultado pasa la política de rutas como reservado ni contiene separadores.
        for n in [
            "CON", "nul.txt", "aux ", "a/b", "a\\b", "..", ".", "COM9.x", "fin.", "x:y",
        ] {
            let d = dest_name(&u(n)).name;
            assert!(!d.contains(['/', '\\', ':']), "{d}");
            assert!(!is_reserved(&d), "{d}");
            assert!(
                mr_io::path_policy::check_lexical(&format!("C:\\d\\{d}")).is_ok(),
                "{d}"
            );
        }
    }

    #[test]
    fn long_names_are_truncated_deterministically() {
        let long = "x".repeat(400) + ".txt";
        let a = dest_name(&u(&long));
        assert!(a.truncated && a.changed);
        assert!(a.name.encode_utf16().count() <= MAX_COMPONENT - RESERVE);
        assert!(a.name.ends_with(".txt"));
        assert_eq!(a, dest_name(&u(&long)));
        let other = "x".repeat(401) + ".txt";
        assert_ne!(a.name, dest_name(&u(&other)).name);
        // No se corta un escape por la mitad.
        let esc = ":".repeat(300);
        let d = dest_name(&u(&esc)).name;
        let (b, _) = d.split_once('~').unwrap();
        assert_eq!(b.len() % 3, 0, "{d}");
    }

    #[test]
    fn suffixes_and_keys() {
        assert_eq!(with_suffix("a.txt", " (2)", false), "a (2).txt");
        assert_eq!(with_suffix(".config", " (2)", false), ".config (2)");
        assert_eq!(with_suffix("dir.d", " (2)", true), "dir.d (2)");
        assert_eq!(conflict_suffix(1, false), " (2)");
        assert_eq!(conflict_suffix(2, false), " (3)");
        assert_eq!(conflict_suffix(1, true), " (borrado)");
        assert_eq!(conflict_suffix(2, true), " (borrado 2)");
        assert_eq!(fold_key("Ágil.TXT", false), fold_key("ágil.txt", false));
        assert_ne!(fold_key("A", true), fold_key("a", true));
        assert_eq!(
            sibling_name("a.txt", "Zone.Identifier"),
            "a.txt%3AZone.Identifier"
        );
        assert_eq!(stream_name("a:b").name, "a%3Ab");
        assert_eq!(units_hex(&[0x62, 0xD800]), "0062D800");
    }
}
