//! Imágenes divididas en varios ficheros consecutivos (`x.001`, `x.002`…), concatenados como un
//! único dispositivo de bloques de **solo lectura**.
//!
//! Convenciones admitidas (ver `docs/formatos/division-y-vistas.md`):
//! - Sufijo numérico decimal de **3 o más dígitos** tras el último punto: `disco.001`,
//!   `disco.img.0001`. El nombre base (todo lo anterior al último punto) no puede estar vacío.
//! - El primer segmento es `…001` o, opcionalmente, `…000`. Los siguientes llevan el número
//!   siguiente con el **mismo ancho** de relleno con ceros que el primero (si el número ya no cabe,
//!   simplemente tiene más dígitos: `x.999` → `x.1000`).
//! - Como máximo [`MAX_SEGMENTS`] segmentos.
//!
//! [`discover`] solo **lista** candidatos del mismo directorio; nunca abre su contenido. La interfaz
//! debe mostrar la lista y pedir confirmación antes de llamar a [`SplitImage::open`].

use mr_io::path_policy::{admit, check_lexical, open_regular_file};
use mr_io::{validate_sector_size, BlockDevice, IoError, Result};
use std::collections::HashSet;
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Número máximo de segmentos de una imagen dividida.
pub const MAX_SEGMENTS: usize = 10_000;

const FORMAT: &str = "imagen dividida";

/// Nombre de un segmento descompuesto: base, número y ancho del sufijo.
struct SegmentName<'a> {
    base: &'a str,
    number: u64,
    width: usize,
}

fn parse_segment_name(name: &str) -> Option<SegmentName<'_>> {
    let (base, digits) = name.rsplit_once('.')?;
    if base.is_empty() || digits.len() < 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(SegmentName {
        base,
        number: digits.parse().ok()?,
        width: digits.len(),
    })
}

fn segment_file_name(base: &str, number: u64, width: usize) -> String {
    format!("{base}.{number:0width$}")
}

fn unsupported(detail: String) -> IoError {
    IoError::UnsupportedFeature {
        format: FORMAT,
        detail,
    }
}

fn corrupt(detail: String) -> IoError {
    IoError::Corrupt {
        format: FORMAT,
        detail,
    }
}

/// `true` si existe una entrada de directorio con ese nombre (sin seguir enlaces ni abrirla).
fn entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(IoError::Io(e)),
    }
}

/// Lista los segmentos de una imagen dividida a partir del **primero** (`x.001` o `x.000`).
///
/// Busca únicamente en el directorio de `first` los nombres `base.NNN` consecutivos con el mismo
/// ancho de sufijo y se detiene en el primer número que falta. Cada segmento debe pasar
/// [`admit`] (fichero normal, nunca un dispositivo ni un directorio). No abre ningún segmento para
/// leer su contenido.
///
/// Errores:
/// - el nombre no sigue la convención o no es el primer segmento → `UnsupportedFeature`;
/// - se pasa `x.001` pero existe `x.000` → `ExternalReference` (hay que empezar por `x.000`);
/// - un segmento existe pero no es un fichero normal → `DevicePathRejected`;
/// - hay más de [`MAX_SEGMENTS`] segmentos → `UnsupportedFeature`.
pub fn discover(first: &Path) -> Result<Vec<PathBuf>> {
    check_lexical(&first.to_string_lossy())?;
    let name = first.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        unsupported(format!(
            "«{}» no tiene un nombre de fichero válido en Unicode",
            first.display()
        ))
    })?;
    let parsed = parse_segment_name(name).ok_or_else(|| {
        unsupported(format!(
            "«{name}» no sigue la convención nombre.001 (sufijo numérico de 3 o más dígitos)"
        ))
    })?;
    if parsed.number > 1 {
        return Err(unsupported(format!(
            "«{name}» no es el primer segmento (debe terminar en …000 o …001)"
        )));
    }
    let dir = first.parent().unwrap_or(Path::new(""));
    if parsed.number == 1 {
        let zero = segment_file_name(parsed.base, 0, parsed.width);
        if entry_exists(&dir.join(&zero))? {
            return Err(IoError::ExternalReference {
                format: FORMAT,
                reference: zero,
            });
        }
    }

    let mut out = Vec::new();
    let mut number = parsed.number;
    loop {
        let path = dir.join(segment_file_name(parsed.base, number, parsed.width));
        if !entry_exists(&path)? {
            break;
        }
        if out.len() == MAX_SEGMENTS {
            return Err(unsupported(format!(
                "más de {MAX_SEGMENTS} segmentos en «{}»",
                dir.display()
            )));
        }
        // Rechaza directorios, dispositivos y enlaces a ellos sin abrir el contenido.
        admit(&path)?;
        out.push(path);
        number += 1;
    }
    if out.is_empty() {
        // El primer segmento no existe.
        return Err(IoError::Io(std::io::Error::new(
            ErrorKind::NotFound,
            format!("no existe «{}»", first.display()),
        )));
    }
    Ok(out)
}

struct Segment {
    file: File,
    /// Desplazamiento global del primer byte del segmento.
    start: u64,
    len: u64,
}

/// Imagen dividida en varios ficheros, expuesta como un dispositivo de solo lectura.
pub struct SplitImage {
    paths: Vec<PathBuf>,
    /// Solo segmentos con datos (longitud > 0), ordenados por `start`.
    segments: Vec<Segment>,
    size: u64,
    sector_size: u32,
    anomalies: Vec<String>,
}

impl SplitImage {
    /// Abre en solo lectura los segmentos `paths` (en ese orden) y los concatena.
    ///
    /// Cada ruta pasa [`admit`] y [`open_regular_file`]. Se admiten segmentos de tamaños
    /// distintos (las irregularidades se describen en [`SplitImage::describe`]); un segmento
    /// intermedio vacío, una ruta repetida o una imagen vacía son `Corrupt`.
    pub fn open(paths: &[PathBuf], sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        if paths.is_empty() {
            return Err(corrupt("no se ha indicado ningún segmento".into()));
        }
        if paths.len() > MAX_SEGMENTS {
            return Err(unsupported(format!(
                "{} segmentos (máximo {MAX_SEGMENTS})",
                paths.len()
            )));
        }
        let mut canon_paths: Vec<PathBuf> = Vec::with_capacity(paths.len());
        // Detección de repetidos en O(1) por segmento (hasta MAX_SEGMENTS rutas).
        let mut seen: HashSet<PathBuf> = HashSet::with_capacity(paths.len());
        let mut segments = Vec::with_capacity(paths.len());
        let mut anomalies = Vec::new();
        let mut total: u64 = 0;
        let mut first_len = 0u64;
        let last = paths.len() - 1;
        for (i, p) in paths.iter().enumerate() {
            let canon = admit(p)?;
            if !seen.insert(canon.clone()) {
                return Err(corrupt(format!(
                    "el segmento «{}» aparece más de una vez",
                    canon.display()
                )));
            }
            let file = open_regular_file(&canon)?;
            let len = file.metadata()?.len();
            let label = display_name(&canon);
            if len == 0 {
                if i != last {
                    return Err(corrupt(format!(
                        "el segmento intermedio {label} (nº {}) está vacío",
                        i + 1
                    )));
                }
                if i != 0 {
                    anomalies.push(format!("el último segmento ({label}) está vacío"));
                }
            }
            if i == 0 {
                first_len = len;
            } else if i != last && len != first_len {
                let prev = segments.last().map_or(first_len, |s: &Segment| s.len);
                anomalies.push(format!(
                    "el segmento intermedio {label} mide {len} bytes ({} el primero, {} el anterior)",
                    first_len, prev
                ));
            } else if i == last && len > first_len {
                anomalies.push(format!(
                    "el último segmento ({label}, {len} bytes) es mayor que el primero ({first_len} bytes)"
                ));
            }
            let start = total;
            total = total.checked_add(len).ok_or_else(|| {
                corrupt("la suma de tamaños de los segmentos desborda 64 bits".into())
            })?;
            if len > 0 {
                segments.push(Segment { file, start, len });
            }
            canon_paths.push(canon);
        }
        if total == 0 {
            return Err(corrupt("todos los segmentos están vacíos".into()));
        }
        if !total.is_multiple_of(u64::from(sector_size)) {
            anomalies.push(format!(
                "el tamaño total ({total} bytes) no es múltiplo del sector ({sector_size} bytes): \
                 los últimos {} bytes no forman un sector completo",
                total % u64::from(sector_size)
            ));
        }
        Ok(Self {
            paths: canon_paths,
            segments,
            size: total,
            sector_size,
            anomalies,
        })
    }

    /// Rutas canónicas de los segmentos abiertos, en orden.
    pub fn segments(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Irregularidades detectadas al abrir (tamaños distintos, último vacío…).
    pub fn anomalies(&self) -> &[String] {
        &self.anomalies
    }

    /// Descripción legible, p. ej. «Imagen dividida en 3 segmentos (x.001–x.003), 2,5 GiB».
    pub fn describe(&self) -> String {
        let n = self.paths.len();
        let first = display_name(&self.paths[0]);
        let names = if n == 1 {
            first
        } else {
            format!("{first}–{}", display_name(&self.paths[n - 1]))
        };
        let plural = if n == 1 { "segmento" } else { "segmentos" };
        let mut s = format!(
            "Imagen dividida en {n} {plural} ({names}), {} ({} bytes)",
            human_size(self.size),
            self.size
        );
        if !self.anomalies.is_empty() {
            s.push_str("; anomalías: ");
            s.push_str(&self.anomalies.join("; "));
        }
        s
    }
}

fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Tamaño en unidades binarias con coma decimal («2,5 GiB»).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let mut s = format!("{v:.1}").replace('.', ",");
    if s.ends_with(",0") {
        s.truncate(s.len() - 2);
    }
    format!("{s} {}", UNITS[u])
}

impl BlockDevice for SplitImage {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        if offset.checked_add(len as u64).is_none_or(|e| e > self.size) {
            return Err(IoError::OutOfBounds {
                offset,
                len,
                size: self.size,
            });
        }
        if buf.is_empty() {
            return Ok(());
        }
        // Primer segmento cuyo inicio es ≤ offset (existe: el primero empieza en 0 y offset < size).
        let mut idx = self.segments.partition_point(|s| s.start <= offset) - 1;
        let mut pos = offset;
        let mut rest = buf;
        while !rest.is_empty() {
            let seg = self
                .segments
                .get(idx)
                .ok_or_else(|| corrupt(format!("no hay segmento para el desplazamiento {pos}")))?;
            let local = pos - seg.start;
            let avail = seg.len - local;
            let n = usize::try_from(avail).map_or(rest.len(), |a| a.min(rest.len()));
            let (chunk, tail) = rest.split_at_mut(n);
            mr_io::read_exact_at(&seg.file, local, chunk).map_err(|source| IoError::Read {
                offset: pos,
                len: n,
                source,
            })?;
            rest = tail;
            pos += n as u64;
            idx += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names() {
        let p = parse_segment_name("disco.img.001").unwrap();
        assert_eq!((p.base, p.number, p.width), ("disco.img", 1, 3));
        let p = parse_segment_name("x.0000").unwrap();
        assert_eq!((p.base, p.number, p.width), ("x", 0, 4));
        for bad in [
            "x.01", "x.1", ".001", "x001", "x.00a", "x.", "x.001 ", "x.e01",
        ] {
            assert!(parse_segment_name(bad).is_none(), "{bad}");
        }
        assert_eq!(segment_file_name("a", 7, 3), "a.007");
        assert_eq!(segment_file_name("a", 1000, 3), "a.1000");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(10), "10 bytes");
        assert_eq!(human_size(1024), "1 KiB");
        assert_eq!(human_size(1536), "1,5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024 / 2), "2,5 GiB");
    }
}
