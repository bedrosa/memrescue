//! Escritura de JSON mínima y determinista para el diario (sin dependencias): objetos con las
//! claves en el orden en que se añaden, cadenas escapadas según RFC 8259 (los caracteres de
//! control y `"`/`\` escapados; el resto en UTF-8 tal cual) y enteros sin signo.

/// Objeto JSON en construcción.
#[derive(Debug, Default, Clone)]
pub struct Obj {
    buf: String,
    first: bool,
}

/// Cadena JSON escapada (con comillas).
pub fn string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7F}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            // Separadores de línea de Unicode: válidos en JSON pero se escapan para que una línea
            // del diario nunca se parta en un visor.
            '\u{2028}' | '\u{2029}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Obj {
    pub fn new() -> Self {
        Obj {
            buf: String::from("{"),
            first: true,
        }
    }

    fn key(&mut self, k: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        self.buf.push_str(&string(k));
        self.buf.push(':');
    }

    pub fn str(mut self, k: &str, v: &str) -> Self {
        self.key(k);
        self.buf.push_str(&string(v));
        self
    }

    pub fn opt_str(self, k: &str, v: Option<&str>) -> Self {
        match v {
            Some(v) => self.str(k, v),
            None => self.null(k),
        }
    }

    pub fn num(mut self, k: &str, v: u64) -> Self {
        self.key(k);
        self.buf.push_str(&v.to_string());
        self
    }

    pub fn opt_num(self, k: &str, v: Option<u64>) -> Self {
        match v {
            Some(v) => self.num(k, v),
            None => self.null(k),
        }
    }

    pub fn bool(mut self, k: &str, v: bool) -> Self {
        self.key(k);
        self.buf.push_str(if v { "true" } else { "false" });
        self
    }

    pub fn null(mut self, k: &str) -> Self {
        self.key(k);
        self.buf.push_str("null");
        self
    }

    /// Valor JSON ya formado (objeto, lista).
    pub fn raw(mut self, k: &str, json: &str) -> Self {
        self.key(k);
        self.buf.push_str(json);
        self
    }

    pub fn obj(self, k: &str, o: Obj) -> Self {
        let j = o.finish();
        self.raw(k, &j)
    }

    pub fn opt_obj(self, k: &str, o: Option<Obj>) -> Self {
        match o {
            Some(o) => self.obj(k, o),
            None => self.null(k),
        }
    }

    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// Lista JSON de valores ya formados.
pub fn array<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut o = String::from("[");
    for (i, x) in items.into_iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str(&x);
    }
    o.push(']');
    o
}

/// Fecha y hora UTC en ISO 8601 (`AAAA-MM-DDTHH:MM:SSZ`) de segundos desde 1970.
pub fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Fecha civil (proleptic gregoriana) de los días desde 1970-01-01 (algoritmo de Howard
/// Hinnant, de dominio público).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// FILETIME (100 ns desde 1601) en ISO 8601 con 7 decimales, o `None` si es 0.
pub fn filetime_iso(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let secs = ft / 10_000_000;
    let frac = ft % 10_000_000;
    if secs < EPOCH_DIFF {
        // Antes de 1970: se da el valor en bruto (el diario lo guarda siempre aparte).
        return None;
    }
    let base = iso_utc(secs - EPOCH_DIFF);
    Some(format!("{}.{frac:07}Z", &base[..base.len() - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_objects() {
        assert_eq!(string("a\"b\\c\n\u{1}ñ"), "\"a\\\"b\\\\c\\n\\u0001ñ\"");
        let o = Obj::new()
            .str("a", "x")
            .num("b", 7)
            .bool("c", true)
            .null("d")
            .raw("e", &array(vec!["1".into(), "2".into()]))
            .obj("f", Obj::new())
            .finish();
        assert_eq!(o, r#"{"a":"x","b":7,"c":true,"d":null,"e":[1,2],"f":{}}"#);
    }

    #[test]
    fn dates() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_767_225_600), "2026-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(
            filetime_iso(134_116_992_000_000_001).as_deref(),
            Some("2026-01-01T00:00:00.0000001Z")
        );
        assert_eq!(filetime_iso(0), None);
    }
}
