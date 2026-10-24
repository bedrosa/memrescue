//! Copia del particionado en un formato propio, versionado y documentado (issue #18, PT-10).
//!
//! Especificación completa en `docs/formatos/copia-particionado.md`. Resumen:
//! - Documento JSON (subconjunto estricto, [`json`]) con `format = "memrescue-partition-backup"`,
//!   `version = 1`, el cuerpo y la suma SHA-256 de la forma canónica del cuerpo (`body_sha256`).
//! - El cuerpo guarda la identidad del origen (tamaño y sector), el particionado **interpretado**
//!   completo (esquema, particiones, anomalías, lectura GPT con sus dos copias, copia elegida y
//!   desplazamiento), los **bytes** de los sectores de tabla leídos (LBA 0, cadena de EBR,
//!   cabeceras y tablas de entradas GPT de las dos copias) con su SHA-256, y opcionalmente el
//!   resultado de la búsqueda exprés.
//! - Ida y vuelta sin pérdidas: [`LayoutBackup::from_text`] de [`LayoutBackup::to_text`] devuelve
//!   un valor igual (`==`) al original.
//! - La lectura valida todo (versión, campos exactos, rangos, límites, sumas de cada región y del
//!   cuerpo) y [`compare`] informa de las diferencias con un origen **sin escribir nada**:
//!   restaurar en disco queda fuera de alcance hasta M12 (ADR-006).

mod codec;
pub mod json;
#[cfg(test)]
mod tests;

use std::fmt;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_io::BlockDevice;

use crate::gpt::{self, GptCopy, GptRead, MAX_ENTRY_ARRAY_BYTES};
use crate::mbr::{self, MAX_EBR_CHAIN};
use crate::search::SearchResult;
use crate::sector::read_trusted;
use crate::{Anomaly, Origin, Partition, PartitionTable, Scheme};
use codec::{hex_decode, hex_encode, invalid, Codec, Obj, Res};
use json::Json;

/// Valor del campo `format`.
pub const FORMAT: &str = "memrescue-partition-backup";
/// Versión del formato que escribe y lee este código.
pub const VERSION: u32 = 1;
/// Tamaño máximo del fichero de copia que se acepta.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes máximos de una región (la mayor tabla de entradas GPT admitida más un sector).
pub const MAX_REGION_BYTES: u64 = MAX_ENTRY_ARRAY_BYTES + 65536;
/// Regiones máximas: LBA 0, la cadena de EBR completa y las cuatro de GPT.
pub const MAX_REGIONS: usize = 1 + MAX_EBR_CHAIN + 4;
/// Bytes de cada línea de `data` (128 dígitos hexadecimales).
pub const DATA_LINE_BYTES: usize = 64;
/// Longitud máxima de la descripción del origen.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;
/// Reintentos por sector al leer las regiones.
const READ_RETRIES: u32 = 1;

/// Error al leer o validar una copia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// El fichero supera [`MAX_FILE_BYTES`].
    TooLarge { size: u64 },
    /// No es UTF-8 válido.
    Encoding,
    /// JSON mal formado (fichero truncado, editado a mano...).
    Syntax {
        line: usize,
        column: usize,
        message: String,
    },
    /// No es una copia del particionado de MemRescue (`format`).
    NotABackup,
    /// Versión del formato no admitida.
    UnsupportedVersion(String),
    /// Campo ausente, desconocido, de otro tipo o fuera de rango; `path` es su ruta.
    Invalid { path: String, message: String },
    /// Suma SHA-256 que no coincide (cuerpo o región).
    Integrity(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupError::TooLarge { size } => write!(
                f,
                "copia del particionado demasiado grande: {size} bytes (máximo {MAX_FILE_BYTES})"
            ),
            BackupError::Encoding => write!(f, "la copia del particionado no es texto UTF-8"),
            BackupError::Syntax {
                line,
                column,
                message,
            } => write!(
                f,
                "copia del particionado mal formada (línea {line}, columna {column}): {message}"
            ),
            BackupError::NotABackup => write!(
                f,
                "el fichero no es una copia del particionado de MemRescue (format distinto de {FORMAT})"
            ),
            BackupError::UnsupportedVersion(v) => write!(
                f,
                "versión de la copia del particionado no admitida: {v} (se admite la {VERSION})"
            ),
            BackupError::Invalid { path, message } => {
                write!(f, "copia del particionado no válida en {path}: {message}")
            }
            BackupError::Integrity(m) => {
                write!(f, "la copia del particionado está alterada o dañada: {m}")
            }
        }
    }
}

impl std::error::Error for BackupError {}

/// Identidad del origen de la copia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// Descripción legible (informativa; no se compara).
    pub description: String,
    pub size_bytes: u64,
    /// Tamaño de sector lógico con que se abrió.
    pub sector_size: u32,
    pub sector_count: u64,
}

/// Estructura a la que pertenece una región de bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionKind {
    /// Sector de la LBA 0 (MBR, MBR protector o lo que haya).
    Mbr,
    /// Un EBR de la cadena de particiones lógicas.
    Ebr,
    GptPrimaryHeader,
    GptPrimaryEntries,
    GptBackupHeader,
    GptBackupEntries,
}

impl RegionKind {
    pub const ALL: [RegionKind; 6] = [
        RegionKind::Mbr,
        RegionKind::Ebr,
        RegionKind::GptPrimaryHeader,
        RegionKind::GptPrimaryEntries,
        RegionKind::GptBackupHeader,
        RegionKind::GptBackupEntries,
    ];

    /// Código estable del formato.
    pub fn code(self) -> &'static str {
        match self {
            RegionKind::Mbr => "mbr",
            RegionKind::Ebr => "ebr",
            RegionKind::GptPrimaryHeader => "gpt_primary_header",
            RegionKind::GptPrimaryEntries => "gpt_primary_entries",
            RegionKind::GptBackupHeader => "gpt_backup_header",
            RegionKind::GptBackupEntries => "gpt_backup_entries",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RegionKind::Mbr => "sector 0 (MBR)",
            RegionKind::Ebr => "EBR",
            RegionKind::GptPrimaryHeader => "cabecera GPT primaria",
            RegionKind::GptPrimaryEntries => "entradas GPT primarias",
            RegionKind::GptBackupHeader => "cabecera GPT de respaldo",
            RegionKind::GptBackupEntries => "entradas GPT de respaldo",
        }
    }
}

/// Contenido de una región.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionContent {
    /// Todos los sectores se leyeron con fiabilidad: estos son sus bytes.
    Data(Vec<u8>),
    /// Algún sector no es fiable (ilegible o saltado): sus LBA del dispositivo. No se guarda
    /// contenido (el relleno nunca son datos).
    Unreadable(Vec<u64>),
}

/// Bytes de una estructura de tabla tal como estaban en el origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRegion {
    pub kind: RegionKind,
    /// Primer byte en el origen.
    pub offset: u64,
    pub length: u64,
    pub content: RegionContent,
}

impl RawRegion {
    /// SHA-256 del contenido (en hexadecimal en minúsculas), si se leyó.
    pub fn sha256(&self) -> Option<String> {
        match &self.content {
            RegionContent::Data(d) => Some(sha256(d)),
            RegionContent::Unreadable(_) => None,
        }
    }
}

/// Copia del particionado de un origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutBackup {
    /// Programa y versión que la escribió (`MemRescue 0.2.0`).
    pub created_by: String,
    pub source: SourceIdentity,
    /// Particionado interpretado completo.
    pub layout: PartitionTable,
    /// Bytes de los sectores de tabla, en el orden en que se leyeron.
    pub regions: Vec<RawRegion>,
    /// Resultado de la búsqueda exprés, si se incluyó.
    pub search: Option<SearchResult>,
}

pub(crate) fn sha256(data: &[u8]) -> String {
    let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
    h.update(data);
    h.finish().pop().map(|(_, s)| s).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Captura
// ---------------------------------------------------------------------------------------------

/// Posiciones (en bytes del origen) de las estructuras de tabla de un particionado: LBA 0, EBR
/// leídos (`ebr_lbas`, en sectores del dispositivo) y, si hay lectura GPT, cabecera y tabla de
/// entradas de cada copia en la imagen (las que quedan fuera de la imagen se omiten).
fn region_positions(
    dev_sector: u64,
    dev_size: u64,
    ebr_lbas: &[u64],
    gpt: Option<&GptRead>,
) -> Vec<(RegionKind, u64, u64)> {
    let mut out = vec![(RegionKind::Mbr, 0, dev_sector)];
    for &lba in ebr_lbas {
        if let Some(off) = lba.checked_mul(dev_sector) {
            out.push((RegionKind::Ebr, off, dev_sector));
        }
    }
    if let Some(g) = gpt {
        let ss = u64::from(g.sector_size);
        for (copy, report) in [(GptCopy::Primary, &g.primary), (GptCopy::Backup, &g.backup)] {
            let (hk, ek) = match copy {
                GptCopy::Primary => (RegionKind::GptPrimaryHeader, RegionKind::GptPrimaryEntries),
                GptCopy::Backup => (RegionKind::GptBackupHeader, RegionKind::GptBackupEntries),
            };
            let Some(image_lba) = report.image_lba else {
                continue;
            };
            if let Some(off) = image_lba.checked_mul(ss) {
                out.push((hk, off, ss));
            }
            // La tabla de entradas, con el desplazamiento de su propia cabecera.
            let Some(h) = report.fields else {
                continue;
            };
            let bytes = h.array_bytes();
            if bytes == 0 || bytes > MAX_ENTRY_ARRAY_BYTES {
                continue;
            }
            let delta = i128::from(report.disk_lba) - i128::from(image_lba);
            let Ok(entries_image) = u64::try_from(i128::from(h.entries_lba) - delta) else {
                continue;
            };
            let len = bytes.div_ceil(ss) * ss;
            if let Some(off) = entries_image.checked_mul(ss) {
                out.push((ek, off, len));
            }
        }
    }
    out.retain(|&(_, off, len)| off.checked_add(len).is_some_and(|end| end <= dev_size));
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(*r));
    out
}

/// Lee `[offset, offset + length)` del origen con los sectores que lo cubren; nunca entrega el
/// relleno de un sector no fiable. `None` si queda fuera del origen.
fn read_region(
    dev: &(impl BlockDevice + ?Sized),
    offset: u64,
    length: u64,
) -> mr_io::Result<Option<RegionContent>> {
    let ss = u64::from(dev.sector_size());
    let Some(end) = offset.checked_add(length) else {
        return Ok(None);
    };
    if length == 0 || length > MAX_REGION_BYTES || end > dev.sector_count() * ss {
        return Ok(None);
    }
    let first = offset / ss;
    let count = end.div_ceil(ss) - first;
    let t = read_trusted(dev, first, count, READ_RETRIES)?;
    let bad: Vec<u64> = t.untrusted().collect();
    if !bad.is_empty() {
        return Ok(Some(RegionContent::Unreadable(bad)));
    }
    let all = t.all().unwrap_or_default();
    let at = (offset - first * ss) as usize;
    Ok(Some(RegionContent::Data(
        all[at..at + length as usize].to_vec(),
    )))
}

fn read_regions(
    dev: &(impl BlockDevice + ?Sized),
    positions: &[(RegionKind, u64, u64)],
) -> mr_io::Result<Vec<RawRegion>> {
    let mut out = Vec::new();
    for &(kind, offset, length) in positions {
        if let Some(content) = read_region(dev, offset, length)? {
            out.push(RawRegion {
                kind,
                offset,
                length,
                content,
            });
        }
    }
    Ok(out)
}

fn positions_of(
    dev: &(impl BlockDevice + ?Sized),
    layout: &PartitionTable,
) -> mr_io::Result<Vec<(RegionKind, u64, u64)>> {
    // La cadena de EBR no queda en la tabla interpretada: se vuelve a recorrer (solo lectura).
    let ebr_lbas = match layout.scheme {
        Scheme::Mbr | Scheme::Hybrid => mbr::read_mbr(dev)?.ebr_lbas,
        _ => Vec::new(),
    };
    Ok(region_positions(
        u64::from(dev.sector_size()),
        dev.sector_count() * u64::from(dev.sector_size()),
        &ebr_lbas,
        layout.gpt.as_ref(),
    ))
}

/// Captura la copia del particionado de `dev`: lee la tabla ([`crate::read_partition_table`]) y
/// los bytes de sus sectores de tabla. `search`, si se da, debe proceder del mismo origen.
pub fn capture(
    dev: &(impl BlockDevice + ?Sized),
    description: &str,
    search: Option<&SearchResult>,
) -> mr_io::Result<LayoutBackup> {
    let layout = crate::read_partition_table(dev)?;
    let positions = positions_of(dev, &layout)?;
    let regions = read_regions(dev, &positions)?;
    Ok(LayoutBackup {
        created_by: format!("MemRescue {}", mr_common::ENGINE_VERSION),
        source: SourceIdentity {
            description: description.chars().take(MAX_DESCRIPTION_CHARS).collect(),
            size_bytes: dev.size(),
            sector_size: dev.sector_size(),
            sector_count: dev.sector_count(),
        },
        layout,
        regions,
        search: search.cloned(),
    })
}

// ---------------------------------------------------------------------------------------------
// Escritura y lectura
// ---------------------------------------------------------------------------------------------

impl Codec for SourceIdentity {
    fn enc(&self) -> Json {
        Json::Obj(vec![
            ("description".into(), self.description.enc()),
            ("size_bytes".into(), self.size_bytes.enc()),
            ("sector_size".into(), self.sector_size.enc()),
            ("sector_count".into(), self.sector_count.enc()),
        ])
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let r = Self {
            description: o.take("description")?,
            size_bytes: o.take("size_bytes")?,
            sector_size: o.take("sector_size")?,
            sector_count: o.take("sector_count")?,
        };
        o.finish()?;
        Ok(r)
    }
}

impl Codec for RegionKind {
    fn enc(&self) -> Json {
        Json::Str(self.code().to_string())
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let s = codec::str_of(v, at)?;
        RegionKind::ALL
            .into_iter()
            .find(|k| k.code() == s)
            .ok_or_else(|| invalid(at, format!("región desconocida: {s}")))
    }
}

/// `{"kind", "offset", "length", "state": "data", "sha256", "data": [líneas hex]}` o
/// `{"kind", "offset", "length", "state": "unreadable", "unreadable_lbas": [...]}`.
impl Codec for RawRegion {
    fn enc(&self) -> Json {
        let mut out = vec![
            ("kind".to_string(), self.kind.enc()),
            ("offset".to_string(), self.offset.enc()),
            ("length".to_string(), self.length.enc()),
        ];
        match &self.content {
            RegionContent::Data(d) => {
                out.push(("state".into(), Json::Str("data".into())));
                out.push(("sha256".into(), Json::Str(sha256(d))));
                let lines = d
                    .chunks(DATA_LINE_BYTES)
                    .map(|c| Json::Str(hex_encode(c)))
                    .collect();
                out.push(("data".into(), Json::Arr(lines)));
            }
            RegionContent::Unreadable(lbas) => {
                out.push(("state".into(), Json::Str("unreadable".into())));
                out.push(("unreadable_lbas".into(), lbas.enc()));
            }
        }
        Json::Obj(out)
    }

    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let kind: RegionKind = o.take("kind")?;
        let offset = o.take("offset")?;
        let length: u64 = o.take("length")?;
        if length == 0 || length > MAX_REGION_BYTES {
            return Err(invalid(
                at,
                format!("longitud de región fuera de límites: {length}"),
            ));
        }
        let state: String = o.take("state")?;
        let content = match state.as_str() {
            "data" => {
                let declared: String = o.take("sha256")?;
                let (lines, lat) = o.raw("data")?;
                let Json::Arr(lines) = lines else {
                    return Err(invalid(&lat, "se esperaba una lista de líneas"));
                };
                let mut data = Vec::with_capacity(length as usize);
                for (i, l) in lines.iter().enumerate() {
                    let lat = format!("{lat}[{i}]");
                    let s = codec::str_of(l, &lat)?;
                    let last = i + 1 == lines.len();
                    let ok_len = if last {
                        !s.is_empty() && s.len() <= DATA_LINE_BYTES * 2
                    } else {
                        s.len() == DATA_LINE_BYTES * 2
                    };
                    let bytes = hex_decode(s).filter(|_| ok_len).ok_or_else(|| {
                        invalid(
                            &lat,
                            "línea de datos no válida (hexadecimal en minúsculas, 64 bytes por línea)",
                        )
                    })?;
                    data.extend_from_slice(&bytes);
                    if data.len() as u64 > length {
                        break;
                    }
                }
                if data.len() as u64 != length {
                    return Err(invalid(
                        at,
                        format!("la región declara {length} bytes y contiene {}", data.len()),
                    ));
                }
                let actual = sha256(&data);
                if actual != declared {
                    return Err(BackupError::Integrity(format!(
                        "la región {} ({} en el byte {offset}) no coincide con su SHA-256 (declarada {declared}, calculada {actual})",
                        at, kind.label()
                    )));
                }
                RegionContent::Data(data)
            }
            "unreadable" => {
                let lbas: Vec<u64> = o.take("unreadable_lbas")?;
                if lbas.is_empty() {
                    return Err(invalid(at, "región ilegible sin sectores ilegibles"));
                }
                RegionContent::Unreadable(lbas)
            }
            other => {
                return Err(invalid(
                    at,
                    format!("estado de región desconocido: {other}"),
                ))
            }
        };
        o.finish()?;
        Ok(RawRegion {
            kind,
            offset,
            length,
            content,
        })
    }
}

impl LayoutBackup {
    fn body(&self) -> Json {
        Json::Obj(vec![
            ("created_by".into(), self.created_by.enc()),
            ("source".into(), self.source.enc()),
            ("layout".into(), self.layout.enc()),
            ("regions".into(), self.regions.enc()),
            ("search".into(), self.search.enc()),
        ])
    }

    /// Documento completo (UTF-8, saltos de línea `\n`, terminado en salto de línea).
    pub fn to_text(&self) -> String {
        let body = self.body();
        let doc = Json::Obj(vec![
            ("format".into(), Json::Str(FORMAT.into())),
            ("version".into(), Json::Int(i128::from(VERSION))),
            ("body_sha256".into(), Json::Str(body_sha256(&body))),
            ("body".into(), body),
        ]);
        let mut out = String::new();
        json::write_pretty(&mut out, &doc, 0);
        out.push('\n');
        out
    }

    /// Lee y valida una copia desde sus bytes (ver [`BackupError`] para cada rechazo).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BackupError> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(BackupError::TooLarge {
                size: bytes.len() as u64,
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| BackupError::Encoding)?;
        Self::from_text(text)
    }

    /// Lee y valida una copia desde su texto.
    pub fn from_text(text: &str) -> Result<Self, BackupError> {
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err(BackupError::TooLarge {
                size: text.len() as u64,
            });
        }
        let doc = json::parse(text)?;
        let mut o = Obj::new(&doc, "$")?;
        let (format, _) = o.raw("format").map_err(|_| BackupError::NotABackup)?;
        if *format != Json::Str(FORMAT.into()) {
            return Err(BackupError::NotABackup);
        }
        let (version, _) = o.raw("version")?;
        match version {
            Json::Int(v) if *v == i128::from(VERSION) => {}
            Json::Int(v) => return Err(BackupError::UnsupportedVersion(v.to_string())),
            _ => return Err(BackupError::UnsupportedVersion("(no es un entero)".into())),
        }
        let declared: String = o.take("body_sha256")?;
        let (body, at) = o.raw("body")?;
        o.finish()?;
        let actual = body_sha256(body);
        if actual != declared {
            return Err(BackupError::Integrity(format!(
                "el cuerpo no coincide con body_sha256 (declarada {declared}, calculada {actual})"
            )));
        }
        let mut b = Obj::new(body, &at)?;
        let backup = LayoutBackup {
            created_by: b.take("created_by")?,
            source: b.take("source")?,
            layout: b.take("layout")?,
            regions: b.take("regions")?,
            search: b.take("search")?,
        };
        b.finish()?;
        backup.validate()?;
        Ok(backup)
    }

    /// Coherencia del contenido (límites y relaciones que el JSON no expresa).
    fn validate(&self) -> Result<(), BackupError> {
        let s = &self.source;
        let pow2 = |v: u32| v.is_power_of_two() && (512..=65536).contains(&v);
        if !pow2(s.sector_size) {
            return Err(invalid(
                "$.body.source.sector_size",
                "tamaño de sector no admitido (potencia de 2 entre 512 y 65536)",
            ));
        }
        if s.size_bytes / u64::from(s.sector_size) != s.sector_count {
            return Err(invalid(
                "$.body.source.sector_count",
                "no corresponde a size_bytes / sector_size",
            ));
        }
        if s.description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid(
                "$.body.source.description",
                "descripción demasiado larga",
            ));
        }
        if !pow2(self.layout.sector_size) {
            return Err(invalid(
                "$.body.layout.sector_size",
                "tamaño de sector no admitido (potencia de 2 entre 512 y 65536)",
            ));
        }
        if self.regions.len() > MAX_REGIONS {
            return Err(invalid("$.body.regions", "demasiadas regiones"));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, r) in self.regions.iter().enumerate() {
            let at = format!("$.body.regions[{i}]");
            if !r
                .offset
                .checked_add(r.length)
                .is_some_and(|e| e <= s.size_bytes)
            {
                return Err(invalid(&at, "la región queda fuera del origen"));
            }
            if !seen.insert((r.kind, r.offset, r.length)) {
                return Err(invalid(&at, "región repetida"));
            }
            if let RegionContent::Unreadable(lbas) = &r.content {
                let ss = u64::from(s.sector_size);
                let first = r.offset / ss;
                let end = (r.offset + r.length).div_ceil(ss);
                if lbas.len() as u64 > end - first || lbas.iter().any(|l| !(first..end).contains(l))
                {
                    return Err(invalid(
                        &at,
                        "sectores ilegibles fuera de la región o repetidos",
                    ));
                }
            }
        }
        if let Some(r) = &self.search {
            if r.sector_size != s.sector_size || r.sectors != s.sector_count {
                return Err(invalid(
                    "$.body.search",
                    "la búsqueda se hizo sobre otro origen o con otro tamaño de sector",
                ));
            }
            r.options
                .validate()
                .map_err(|m| invalid("$.body.search.options", m))?;
        }
        Ok(())
    }
}

/// SHA-256 de la forma canónica compacta del cuerpo.
fn body_sha256(body: &Json) -> String {
    let mut c = String::new();
    json::write_compact(&mut c, body);
    sha256(c.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// Comparación con un origen (solo lectura)
// ---------------------------------------------------------------------------------------------

/// Estado de una región en un lado de la comparación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionStatus {
    /// Leída: SHA-256 de su contenido.
    Data(String),
    /// Algún sector no es fiable.
    Unreadable(Vec<u64>),
    /// No existe en el origen (queda fuera) o no está en la copia.
    Absent,
}

impl RegionStatus {
    fn of(c: Option<&RegionContent>) -> Self {
        match c {
            Some(RegionContent::Data(d)) => RegionStatus::Data(sha256(d)),
            Some(RegionContent::Unreadable(l)) => RegionStatus::Unreadable(l.clone()),
            None => RegionStatus::Absent,
        }
    }
}

impl fmt::Display for RegionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionStatus::Data(h) => write!(f, "SHA-256 {h}"),
            RegionStatus::Unreadable(l) => write!(f, "ilegible (LBA {l:?})"),
            RegionStatus::Absent => write!(f, "ausente"),
        }
    }
}

/// Una diferencia entre la copia y el origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// Tamaño del origen distinto.
    SourceSize {
        backup: u64,
        current: u64,
    },
    /// Tamaño de sector con que se abrió distinto.
    SectorSize {
        backup: u32,
        current: u32,
    },
    /// Los bytes de una región de tabla ya no son los mismos (o la región aparece o desaparece).
    Region {
        kind: RegionKind,
        offset: u64,
        length: u64,
        backup: RegionStatus,
        current: RegionStatus,
    },
    Scheme {
        backup: Scheme,
        current: Scheme,
    },
    /// Tamaño de sector con que se interpretó la tabla.
    TableSectorSize {
        backup: u32,
        current: u32,
    },
    GptSelected {
        backup: Option<GptCopy>,
        current: Option<GptCopy>,
    },
    Shift {
        backup: i64,
        current: i64,
    },
    PartitionRemoved {
        backup: Box<Partition>,
    },
    PartitionAdded {
        current: Box<Partition>,
    },
    /// Misma entrada de tabla con otro contenido; `fields` = campos que difieren.
    PartitionChanged {
        backup: Box<Partition>,
        current: Box<Partition>,
        fields: Vec<&'static str>,
    },
    AnomalyRemoved(Anomaly),
    AnomalyAdded(Anomaly),
}

impl Difference {
    /// Código estable (JSON de la CLI).
    pub fn code(&self) -> &'static str {
        match self {
            Difference::SourceSize { .. } => "source_size",
            Difference::SectorSize { .. } => "sector_size",
            Difference::Region { .. } => "region_changed",
            Difference::Scheme { .. } => "scheme_changed",
            Difference::TableSectorSize { .. } => "table_sector_size",
            Difference::GptSelected { .. } => "gpt_selected_copy",
            Difference::Shift { .. } => "image_shift",
            Difference::PartitionRemoved { .. } => "partition_removed",
            Difference::PartitionAdded { .. } => "partition_added",
            Difference::PartitionChanged { .. } => "partition_changed",
            Difference::AnomalyRemoved(_) => "anomaly_removed",
            Difference::AnomalyAdded(_) => "anomaly_added",
        }
    }
}

fn origin_text(o: &Origin) -> String {
    match *o {
        Origin::Primary { slot } => format!("entrada primaria {slot}"),
        Origin::Logical { ebr_lba, slot } => format!("entrada {slot} del EBR de la LBA {ebr_lba}"),
        Origin::Gpt { index } => format!("entrada GPT {index}"),
    }
}

fn scheme_text(s: Scheme) -> &'static str {
    match s {
        Scheme::None => "sin tabla",
        Scheme::Mbr => "MBR",
        Scheme::Gpt => "GPT",
        Scheme::Hybrid => "MBR híbrido con GPT",
    }
}

fn copy_text(c: Option<GptCopy>) -> &'static str {
    match c {
        Some(GptCopy::Primary) => "primaria",
        Some(GptCopy::Backup) => "de respaldo",
        None => "ninguna",
    }
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Difference::SourceSize { backup, current } => write!(
                f,
                "tamaño del origen: {backup} bytes en la copia, {current} bytes ahora"
            ),
            Difference::SectorSize { backup, current } => write!(
                f,
                "tamaño de sector: {backup} B en la copia, {current} B ahora"
            ),
            Difference::Region {
                kind,
                offset,
                length,
                backup,
                current,
            } => write!(
                f,
                "{} (byte {offset}, {length} bytes): copia {backup}, origen {current}",
                kind.label()
            ),
            Difference::Scheme { backup, current } => {
                write!(
                    f,
                    "esquema: {} en la copia, {} ahora",
                    scheme_text(*backup),
                    scheme_text(*current)
                )
            }
            Difference::TableSectorSize { backup, current } => write!(
                f,
                "la tabla se interpretó con sectores de {backup} B y ahora con {current} B"
            ),
            Difference::GptSelected { backup, current } => write!(
                f,
                "copia GPT en uso: {} en la copia, {} ahora",
                copy_text(*backup),
                copy_text(*current)
            ),
            Difference::Shift { backup, current } => write!(
                f,
                "desplazamiento de la imagen: {backup} en la copia, {current} ahora"
            ),
            Difference::PartitionRemoved { backup } => write!(
                f,
                "partición de la copia que ya no está: {} (LBA {}, {} sectores)",
                origin_text(&backup.origin),
                backup.start_lba,
                backup.sector_count
            ),
            Difference::PartitionAdded { current } => write!(
                f,
                "partición nueva: {} (LBA {}, {} sectores)",
                origin_text(&current.origin),
                current.start_lba,
                current.sector_count
            ),
            Difference::PartitionChanged {
                backup,
                current,
                fields,
            } => write!(
                f,
                "{} cambiada ({}): LBA {} y {} sectores en la copia, LBA {} y {} sectores ahora",
                origin_text(&backup.origin),
                fields.join(", "),
                backup.start_lba,
                backup.sector_count,
                current.start_lba,
                current.sector_count
            ),
            Difference::AnomalyRemoved(a) => {
                write!(f, "anomalía que ya no aparece: {} ({a})", a.code())
            }
            Difference::AnomalyAdded(a) => write!(f, "anomalía nueva: {} ({a})", a.code()),
        }
    }
}

/// Resultado de comparar una copia con un origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// Copia tomada ahora del origen (con las mismas reglas que la guardada).
    pub current: LayoutBackup,
    pub differences: Vec<Difference>,
}

impl Comparison {
    /// El origen conserva exactamente el particionado y los bytes de tabla de la copia.
    pub fn identical(&self) -> bool {
        self.differences.is_empty()
    }
}

fn changed_fields(
    a: &Partition,
    b: &Partition,
    ga: Option<&gpt::GptPartition>,
    gb: Option<&gpt::GptPartition>,
) -> Vec<&'static str> {
    let mut v = Vec::new();
    let mut check = |differs: bool, name| {
        if differs {
            v.push(name);
        }
    };
    check(a.start_lba != b.start_lba, "inicio");
    check(a.sector_count != b.sector_count, "tamaño");
    check(a.kind != b.kind, "tipo");
    check(a.name != b.name, "nombre");
    check(a.active != b.active, "activa");
    check(a.confidence != b.confidence, "confianza");
    check(a.anomalies != b.anomalies, "anomalías");
    check(
        ga.map(|g| g.unique_guid) != gb.map(|g| g.unique_guid),
        "GUID único",
    );
    check(
        ga.map(|g| g.attributes) != gb.map(|g| g.attributes),
        "atributos",
    );
    v
}

/// Compara la copia con el origen `dev` **sin escribir nada**: identidad del origen, bytes de
/// cada región de la copia releídos en la misma posición, regiones que ahora existen y no están
/// en la copia, y el particionado interpretado (esquema, copia GPT, desplazamiento, particiones
/// por entrada de tabla y anomalías de la tabla). La búsqueda exprés no se repite.
pub fn compare(
    backup: &LayoutBackup,
    dev: &(impl BlockDevice + ?Sized),
) -> mr_io::Result<Comparison> {
    let current = capture(dev, &backup.source.description, None)?;
    let mut d = Vec::new();
    let (bs, cs) = (&backup.source, &current.source);
    if bs.size_bytes != cs.size_bytes {
        d.push(Difference::SourceSize {
            backup: bs.size_bytes,
            current: cs.size_bytes,
        });
    }
    if bs.sector_size != cs.sector_size {
        d.push(Difference::SectorSize {
            backup: bs.sector_size,
            current: cs.sector_size,
        });
    }

    // Bytes: cada región de la copia en la misma posición del origen.
    for r in &backup.regions {
        let now = read_region(dev, r.offset, r.length)?;
        if now.as_ref() != Some(&r.content) {
            d.push(Difference::Region {
                kind: r.kind,
                offset: r.offset,
                length: r.length,
                backup: RegionStatus::of(Some(&r.content)),
                current: RegionStatus::of(now.as_ref()),
            });
        }
    }
    for r in &current.regions {
        let known = backup
            .regions
            .iter()
            .any(|b| (b.kind, b.offset, b.length) == (r.kind, r.offset, r.length));
        if !known {
            d.push(Difference::Region {
                kind: r.kind,
                offset: r.offset,
                length: r.length,
                backup: RegionStatus::Absent,
                current: RegionStatus::of(Some(&r.content)),
            });
        }
    }

    // Particionado interpretado.
    let (bl, cl) = (&backup.layout, &current.layout);
    if bl.scheme != cl.scheme {
        d.push(Difference::Scheme {
            backup: bl.scheme,
            current: cl.scheme,
        });
    }
    if bl.sector_size != cl.sector_size {
        d.push(Difference::TableSectorSize {
            backup: bl.sector_size,
            current: cl.sector_size,
        });
    }
    let sel = |t: &PartitionTable| t.gpt.as_ref().and_then(|g| g.selected);
    if sel(bl) != sel(cl) {
        d.push(Difference::GptSelected {
            backup: sel(bl),
            current: sel(cl),
        });
    }
    if bl.shift() != cl.shift() {
        d.push(Difference::Shift {
            backup: bl.shift(),
            current: cl.shift(),
        });
    }
    let mut matched = vec![false; cl.partitions.len()];
    for p in &bl.partitions {
        let found = cl
            .partitions
            .iter()
            .enumerate()
            .find(|(i, q)| !matched[*i] && q.origin == p.origin);
        match found {
            Some((i, q)) => {
                matched[i] = true;
                let fields = changed_fields(p, q, bl.gpt_entry(p), cl.gpt_entry(q));
                if !fields.is_empty() {
                    d.push(Difference::PartitionChanged {
                        backup: Box::new(p.clone()),
                        current: Box::new(q.clone()),
                        fields,
                    });
                }
            }
            None => d.push(Difference::PartitionRemoved {
                backup: Box::new(p.clone()),
            }),
        }
    }
    for (i, q) in cl.partitions.iter().enumerate() {
        if !matched[i] {
            d.push(Difference::PartitionAdded {
                current: Box::new(q.clone()),
            });
        }
    }
    let mut pending: Vec<&Anomaly> = cl.anomalies.iter().collect();
    for a in &bl.anomalies {
        match pending.iter().position(|x| *x == a) {
            Some(i) => {
                pending.remove(i);
            }
            None => d.push(Difference::AnomalyRemoved(a.clone())),
        }
    }
    d.extend(pending.into_iter().cloned().map(Difference::AnomalyAdded));
    Ok(Comparison {
        current,
        differences: d,
    })
}
