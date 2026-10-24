//! JSON estricto y acotado para la copia del particionado (`docs/formatos/copia-particionado.md`).
//!
//! Subconjunto de RFC 8259 suficiente para el formato y pensado para datos no confiables:
//! - Solo enteros (`-?(0|[1-9][0-9]*)`, como mucho 20 dígitos); fracciones y exponentes se rechazan.
//! - Cadenas UTF-8 con los escapes de RFC 8259; un sustituto UTF-16 suelto o un carácter de control
//!   sin escapar se rechazan.
//! - Claves repetidas, BOM, contenido tras el valor raíz y anidamiento mayor que [`MAX_DEPTH`] se
//!   rechazan; cadenas, listas y objetos tienen límites explícitos.
//!
//! La escritura es determinista: [`write_pretty`] para el fichero y [`write_compact`] para la
//! forma canónica sobre la que se calcula la suma de integridad del cuerpo.

use super::BackupError;

/// Profundidad máxima de anidamiento.
pub const MAX_DEPTH: usize = 32;
/// Bytes máximos de una cadena (ya decodificada).
pub const MAX_STRING_BYTES: usize = 64 * 1024;
/// Elementos máximos de una lista.
pub const MAX_ARRAY_ITEMS: usize = 1 << 20;
/// Miembros máximos de un objeto.
pub const MAX_OBJECT_MEMBERS: usize = 256;
/// Dígitos máximos de un entero.
const MAX_DIGITS: usize = 20;

/// Valor JSON. Los objetos conservan el orden del fichero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Analiza un documento completo.
pub fn parse(text: &str) -> Result<Json, BackupError> {
    let mut p = Parser {
        s: text.as_bytes(),
        pos: 0,
        text,
    };
    if text.starts_with('\u{feff}') {
        return Err(p.err("el fichero empieza por una marca de orden de bytes (BOM)"));
    }
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.pos != p.s.len() {
        return Err(p.err("contenido después del valor principal"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    text: &'a str,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> BackupError {
        let before = &self.text.as_bytes()[..self.pos.min(self.s.len())];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let column = String::from_utf8_lossy(&before[start..]).chars().count() + 1;
        BackupError::Syntax {
            line,
            column,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8, what: &str) -> Result<(), BackupError> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else if self.peek().is_none() {
            Err(self.err(&format!("fin inesperado del fichero: se esperaba {what}")))
        } else {
            Err(self.err(&format!("se esperaba {what}")))
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, BackupError> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.err("valor no válido"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, BackupError> {
        match self.peek() {
            None => Err(self.err("fin inesperado del fichero: se esperaba un valor")),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("valor no válido")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, BackupError> {
        if depth > MAX_DEPTH {
            return Err(self.err("anidamiento demasiado profundo"));
        }
        self.pos += 1;
        let mut members: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(members));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("se esperaba el nombre de un campo"));
            }
            let key_pos = self.pos;
            let key = self.string()?;
            if members.iter().any(|(k, _)| *k == key) {
                self.pos = key_pos;
                return Err(self.err(&format!("campo repetido: {key}")));
            }
            if members.len() >= MAX_OBJECT_MEMBERS {
                return Err(self.err("demasiados campos en un objeto"));
            }
            self.ws();
            self.expect(b':', "«:»")?;
            self.ws();
            let v = self.value(depth)?;
            members.push((key, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(members));
                }
                _ => self.expect(b'}', "«,» o «}»")?,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, BackupError> {
        if depth > MAX_DEPTH {
            return Err(self.err("anidamiento demasiado profundo"));
        }
        self.pos += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.ws();
            if items.len() >= MAX_ARRAY_ITEMS {
                return Err(self.err("demasiados elementos en una lista"));
            }
            items.push(self.value(depth)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => self.expect(b']', "«,» o «]»")?,
            }
        }
    }

    fn number(&mut self) -> Result<Json, BackupError> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let digits = &self.s[digits_start..self.pos];
        if digits.is_empty() {
            return Err(self.err("número no válido"));
        }
        if digits.len() > 1 && digits[0] == b'0' {
            self.pos = start;
            return Err(self.err("número con ceros a la izquierda"));
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(self.err("solo se admiten números enteros"));
        }
        if digits.len() > MAX_DIGITS {
            self.pos = start;
            return Err(self.err("número demasiado grande"));
        }
        let mut n: i128 = 0;
        for &d in digits {
            n = n * 10 + i128::from(d - b'0');
        }
        Ok(Json::Int(if negative { -n } else { n }))
    }

    fn hex4(&mut self) -> Result<u32, BackupError> {
        let h = self
            .s
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.err("escape \\u incompleto"))?;
        let txt = std::str::from_utf8(h).map_err(|_| self.err("escape \\u no válido"))?;
        if !txt.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.err("escape \\u no válido"));
        }
        let v = u32::from_str_radix(txt, 16).map_err(|_| self.err("escape \\u no válido"))?;
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, BackupError> {
        self.pos += 1; // comilla inicial
        let mut out = String::new();
        loop {
            // Tramo sin escapes ni comillas: se copia tal cual (el texto ya es UTF-8 válido).
            let run_start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.text[run_start..self.pos]);
            if out.len() > MAX_STRING_BYTES {
                return Err(self.err("cadena demasiado larga"));
            }
            match self.peek() {
                None => return Err(self.err("fin inesperado del fichero dentro de una cadena")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                if !self.s[self.pos..].starts_with(b"\\u") {
                                    return Err(self.err("sustituto UTF-16 sin pareja"));
                                }
                                self.pos += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("sustituto UTF-16 sin pareja"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err(self.err("sustituto UTF-16 sin pareja"));
                            } else {
                                hi
                            };
                            let c = char::from_u32(code)
                                .ok_or_else(|| self.err("escape \\u no válido"))?;
                            out.push(c);
                            continue;
                        }
                        _ => return Err(self.err("escape no válido")),
                    };
                    self.pos += 1;
                    out.push(c);
                }
                Some(_) => return Err(self.err("carácter de control sin escapar en una cadena")),
            }
        }
    }
}

/// Escribe una cadena con los escapes mínimos: `"`, `\` y los caracteres de control (como
/// `\u00XX`); el resto, tal cual en UTF-8.
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_scalar(out: &mut String, v: &Json) -> bool {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Str(s) => write_str(out, s),
        _ => return false,
    }
    true
}

/// Forma canónica compacta: sin espacios, miembros en el orden del árbol.
pub fn write_compact(out: &mut String, v: &Json) {
    if write_scalar(out, v) {
        return;
    }
    match v {
        Json::Arr(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, x);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (i, (k, x)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_compact(out, x);
            }
            out.push('}');
        }
        _ => {}
    }
}

/// Listas cortas de escalares en una línea; el resto, un elemento por línea con dos espacios de
/// sangría por nivel.
pub fn write_pretty(out: &mut String, v: &Json, indent: usize) {
    if write_scalar(out, v) {
        return;
    }
    let pad = |out: &mut String, n: usize| out.extend(std::iter::repeat_n(' ', n));
    match v {
        Json::Arr(items) if items.is_empty() => out.push_str("[]"),
        Json::Obj(members) if members.is_empty() => out.push_str("{}"),
        Json::Arr(items) => {
            let mut inline = String::new();
            let short = items.iter().all(|x| {
                write_scalar(&mut inline, x) && {
                    inline.push_str(", ");
                    inline.len() <= 80
                }
            });
            if short {
                inline.truncate(inline.len() - 2);
                out.push('[');
                out.push_str(&inline);
                out.push(']');
                return;
            }
            out.push_str("[\n");
            for (i, x) in items.iter().enumerate() {
                pad(out, indent + 2);
                write_pretty(out, x, indent + 2);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, indent);
            out.push(']');
        }
        Json::Obj(members) => {
            out.push_str("{\n");
            for (i, (k, x)) in members.iter().enumerate() {
                pad(out, indent + 2);
                write_str(out, k);
                out.push_str(": ");
                write_pretty(out, x, indent + 2);
                if i + 1 < members.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(out, indent);
            out.push('}');
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> Json {
        parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn bad(s: &str) {
        assert!(parse(s).is_err(), "debería rechazarse: {s:?}");
    }

    #[test]
    fn parses_the_subset() {
        assert_eq!(
            ok(r#" {"a": [1, -2, true, null, "x\u00e9\ud83d\ude00\n"], "b": {}} "#),
            Json::Obj(vec![
                (
                    "a".into(),
                    Json::Arr(vec![
                        Json::Int(1),
                        Json::Int(-2),
                        Json::Bool(true),
                        Json::Null,
                        Json::Str("xé\u{1F600}\n".into())
                    ])
                ),
                ("b".into(), Json::Obj(vec![]))
            ])
        );
        assert_eq!(ok("18446744073709551615"), Json::Int(u64::MAX as i128));
        assert_eq!(ok("-9223372036854775808"), Json::Int(i64::MIN as i128));
    }

    #[test]
    fn rejects_everything_outside_the_subset() {
        for s in [
            "",
            "1.5",
            "1e3",
            "01",
            "-",
            "123456789012345678901",
            "[1,]",
            "{\"a\":1,}",
            "{\"a\":1,\"a\":2}",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\x\"",
            "\"a\u{1}\"",
            "\"abc",
            "[1] 2",
            "\u{feff}{}",
            "tru",
            "{a:1}",
            "[",
        ] {
            bad(s);
        }
        bad(&"[".repeat(MAX_DEPTH + 1));
        bad(&"[".repeat(100_000));
        bad(&format!("\"{}\"", "a".repeat(MAX_STRING_BYTES + 1)));
    }

    #[test]
    fn writers_round_trip() {
        let v = ok(r#"{"k":[1,2,{"x":"a\"\\\u0001\u007fé"}],"e":[],"o":{},"n":null}"#);
        let mut c = String::new();
        write_compact(&mut c, &v);
        assert_eq!(ok(&c), v);
        let mut p = String::new();
        write_pretty(&mut p, &v, 0);
        assert_eq!(ok(&p), v);
        let mut c2 = String::new();
        write_compact(&mut c2, &ok(&p));
        assert_eq!(c, c2);
    }

    #[test]
    fn syntax_errors_carry_line_and_column() {
        match parse("{\n  \"a\": x\n}") {
            Err(BackupError::Syntax { line, column, .. }) => assert_eq!((line, column), (2, 8)),
            other => panic!("{other:?}"),
        }
    }
}
