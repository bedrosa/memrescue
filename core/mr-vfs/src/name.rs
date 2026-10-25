//! Nombres: unidades UTF-16 originales (sin pérdida), forma visible y escape para mostrar rutas.
//!
//! **Original.** Cada nodo guarda las unidades UTF-16 tal como están en el origen, incluidas las
//! sueltas (sustitutos sin pareja), que NTFS admite. La recuperación (#26) construye el nombre de
//! destino a partir de ellas.
//!
//! **Forma visible** ([`display`]): UTF-8 con U+FFFD en cada unidad suelta. Puede coincidir para
//! dos nombres distintos; no es una clave.
//!
//! **Componente escapado** ([`escape_component`]): forma visible sin ambigüedad para rutas. Solo
//! se escapan los nombres que lo necesitan (separadores `/` o `\`, códigos de control U+0000 a
//! U+001F y U+007F, unidades UTF-16 sueltas, o un nombre que sea `.`, `..` o vacío); en ellos `%`
//! pasa a `%25`, cada carácter afectado a `%XX` (hexadecimal) y cada unidad suelta a `%uXXXX`;
//! `.` y `..` pasan a `%2E` y `%2E%2E`, y el nombre vacío a `[Sin nombre]`. Un nombre sin nada que
//! escapar se muestra tal cual, aunque contenga `%`.
//!
//! **Comparación** ([`cmp_names`], [`eq_ignore_case`]): sin distinguir mayúsculas con el plegado
//! simple de Unicode unidad a unidad (la tabla `$UpCase` del volumen la aplica `mr-fs-ntfs` al
//! resolver rutas del origen; aquí basta con un orden estable e independiente del volumen).

use std::borrow::Cow;
use std::cmp::Ordering;

/// Nombre que se muestra para un nombre vacío.
pub const EMPTY_NAME: &str = "[Sin nombre]";

/// Unidades UTF-16 de un texto.
pub fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `true` si hay unidades UTF-16 sueltas.
pub fn is_lossy(units: &[u16]) -> bool {
    char::decode_utf16(units.iter().copied()).any(|r| r.is_err())
}

/// Forma visible: UTF-8 con U+FFFD en las unidades sueltas.
pub fn display(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn needs_escape_char(c: char) -> bool {
    matches!(c, '/' | '\\' | '\u{0}'..='\u{1F}' | '\u{7F}')
}

/// `true` si el nombre necesita escape para mostrarse en una ruta.
pub fn needs_escape(units: &[u16]) -> bool {
    units.is_empty()
        || units == [0x2E]
        || units == [0x2E, 0x2E]
        || char::decode_utf16(units.iter().copied()).any(|r| r.map_or(true, needs_escape_char))
}

/// Componente de ruta para mostrar (ver el módulo).
pub fn escape_component(units: &[u16]) -> Cow<'static, str> {
    if !needs_escape(units) {
        return Cow::Owned(display(units));
    }
    if units.is_empty() {
        return Cow::Borrowed(EMPTY_NAME);
    }
    if units == [0x2E] {
        return Cow::Borrowed("%2E");
    }
    if units == [0x2E, 0x2E] {
        return Cow::Borrowed("%2E%2E");
    }
    let mut o = String::with_capacity(units.len() + 8);
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok('%') => o.push_str("%25"),
            Ok(c) if needs_escape_char(c) => o.push_str(&format!("%{:02X}", c as u32)),
            Ok(c) => o.push(c),
            Err(e) => o.push_str(&format!("%u{:04X}", e.unpaired_surrogate())),
        }
    }
    Cow::Owned(o)
}

fn hex(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Inverso de [`escape_component`]: unidades de un componente escapado (`None` si una secuencia
/// `%` no es válida). Sin `%`, las unidades del texto tal cual.
pub fn unescape_component(s: &str) -> Option<Vec<u16>> {
    if s == EMPTY_NAME {
        return Some(Vec::new());
    }
    if !s.contains('%') {
        return Some(units(s));
    }
    let mut out = Vec::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.extend(rest[..i].encode_utf16());
        let tail = &rest[i + 1..];
        if let Some(h) = tail.strip_prefix('u') {
            let v = hex(h.get(..4)?)?;
            out.push(v as u16);
            rest = &h[4..];
        } else {
            let v = hex(tail.get(..2)?)?;
            out.push(v as u16);
            rest = &tail[2..];
        }
    }
    out.extend(rest.encode_utf16());
    Some(out)
}

/// Plegado simple de una unidad UTF-16 a mayúsculas (las unidades que no son un carácter del
/// plano básico, o cuya mayúscula no es un solo carácter del plano básico, se dejan igual).
pub fn fold_unit(u: u16) -> u16 {
    if u < 0x80 {
        return u8::try_from(u).map_or(u, |b| u16::from(b.to_ascii_uppercase()));
    }
    let Some(c) = char::from_u32(u32::from(u)) else {
        return u;
    };
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(x), None) => u16::try_from(x as u32).unwrap_or(u),
        _ => u,
    }
}

/// Orden sin distinguir mayúsculas (después, las unidades originales para que sea total).
pub fn cmp_names(a: &[u16], b: &[u16]) -> Ordering {
    a.iter()
        .map(|u| fold_unit(*u))
        .cmp(b.iter().map(|u| fold_unit(*u)))
        .then_with(|| a.cmp(b))
}

/// Igualdad sin distinguir mayúsculas.
pub fn eq_ignore_case(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| fold_unit(*x) == fold_unit(*y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_replaces_unpaired_surrogates_only() {
        assert_eq!(display(&units("ñandú.bin")), "ñandú.bin");
        assert_eq!(display(&units("😀")), "😀");
        assert_eq!(display(&[0x62, 0xD800, 0x63]), "b\u{FFFD}c");
        assert!(is_lossy(&[0xDC00]));
        assert!(!is_lossy(&units("😀")));
    }

    #[test]
    fn escape_only_when_needed_and_round_trips() {
        let cases: Vec<Vec<u16>> = vec![
            units("normal.txt"),
            units("100%.txt"),
            units("a/b"),
            units("a\\b%"),
            units("tab\there"),
            units("del\u{7F}"),
            units("."),
            units(".."),
            units("..."),
            vec![],
            vec![0x62, 0xD800, 0x63],
            vec![0xDFFF],
            vec![0xD83D, 0xDE00, 0xD83D],
            units("%u0041"),
        ];
        for u in &cases {
            let e = escape_component(u);
            assert!(!e.contains('/'), "{e}");
            if needs_escape(u) || !e.contains('%') {
                assert_eq!(unescape_component(&e).as_deref(), Some(&u[..]), "{e}");
            }
        }
        assert_eq!(escape_component(&units("100%.txt")), "100%.txt");
        assert_eq!(escape_component(&units("a/b%")), "a%2Fb%25");
        assert_eq!(escape_component(&units("a\\b")), "a%5Cb");
        assert_eq!(escape_component(&units("x\u{1}")), "x%01");
        assert_eq!(escape_component(&units(".")), "%2E");
        assert_eq!(escape_component(&units("..")), "%2E%2E");
        assert_eq!(escape_component(&units("...")), "...");
        assert_eq!(escape_component(&[]), EMPTY_NAME);
        assert_eq!(escape_component(&[0x62, 0xD800, 0x63]), "b%uD800c");
        assert_eq!(unescape_component("%zz"), None);
        assert_eq!(unescape_component("%u12"), None);
        assert_eq!(unescape_component("%"), None);
    }

    #[test]
    fn case_insensitive_order_is_total() {
        assert!(eq_ignore_case(&units("ÁGIL.txt"), &units("ágil.TXT")));
        assert!(!eq_ignore_case(&units("a"), &units("b")));
        assert_eq!(cmp_names(&units("b"), &units("A")), Ordering::Greater);
        assert_eq!(cmp_names(&units("a"), &units("A")), Ordering::Greater);
        assert_eq!(cmp_names(&units("A"), &units("a")), Ordering::Less);
        assert_eq!(cmp_names(&units("x"), &units("x")), Ordering::Equal);
        assert_eq!(fold_unit(0xD800), 0xD800);
        assert_eq!(fold_unit(u16::from(b'z')), u16::from(b'Z'));
        // «ß» se pliega a «SS» (dos caracteres): se deja igual.
        assert_eq!(fold_unit(0xDF), 0xDF);
    }
}
