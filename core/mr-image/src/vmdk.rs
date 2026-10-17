//! VMDK de extensión dispersa con descriptor incrustado (`monolithicSparse`), en **solo lectura**.
//!
//! Basado en la especificación pública de VMware «Virtual Disk Format 1.1». Estructura:
//! - Sector 0: cabecera de extensión dispersa (`KDMV`, little-endian).
//! - Descriptor de texto incrustado (`descriptorOffset`/`descriptorSize`, en sectores).
//! - Directorio de granos (GD) en `gdOffset`: una entrada `u32` por tabla de granos (GT) con el
//!   sector donde empieza la GT (0 = GT ausente → todo el tramo es disperso).
//! - Cada GT tiene `numGTEsPerGT` (512) entradas `u32` con el sector del grano
//!   (0 = no asignado; 1 con el indicador «zeroed-grain» = grano a cero).
//!
//! Decisiones:
//! - Se usa el GD **principal** (`gdOffset`); el redundante (`rgdOffset`) no se lee ni se compara.
//! - El GD y todas las GT presentes se cargan y validan al abrir (cada una acotada por
//!   [`crate::MAX_TABLE_BYTES`] y por el tamaño del fichero): una GT o un grano fuera del fichero,
//!   que solapa la cabecera, el descriptor, el GD, una GT u otro grano (incluidas las entradas
//!   duplicadas), o una entrada 1 sin el indicador «zeroed-grain», hace que la apertura falle con
//!   `Corrupt` en modo estricto.
//! - Modo tolerante ([`VmdkImage::open_with`], ADR-020): esas entradas se descartan. Una entrada de
//!   GD inválida descarta los granos de su GT; una entrada de GT inválida, solo su grano. Los granos
//!   descartados se leen con error y se informan como datos no fiables. La cabecera, el descriptor
//!   y la ubicación del GD siguen siendo críticos (`Corrupt` en ambos modos).
//! - Nunca se abren otros ficheros: padres, extensiones adicionales, descriptores separados y
//!   rutas del descriptor solo aparecen como texto (saneado y truncado) en los errores.
//! - Se rechazan con [`IoError::UnsupportedFeature`]: streamOptimized (compresión, marcadores o
//!   GD al final), `COWD` (ESX antiguo), versiones e indicadores desconocidos y descriptores de
//!   texto separados (monolithicFlat, twoGbMaxExtent*, vmfs…).

use crate::budget::{Budget, TABLE_CHUNK};
use crate::tolerant::{self, Damage, Hit, Zone};
use crate::MAX_TABLE_BYTES;
use mr_io::{Allocation, AllocationRun, BlockDevice, IoError, Result};
use std::fs::File;

const FORMAT: &str = "VMDK";
const SECTOR: u64 = 512;
/// La cabecera ocupa 79 bytes útiles; se lee el sector completo.
const HEADER_LEN: usize = 512;
const MAGIC: &[u8; 4] = b"KDMV";
/// Cabecera de los discos dispersos de ESX 2.x (formato «COWD»), no soportado.
const COWD_MAGIC: &[u8; 4] = b"COWD";
const TEXT_DESCRIPTOR_SIGNATURE: &[u8] = b"# disk descriptorfile";

const FLAG_NEWLINE_TEST: u32 = 1;
const FLAG_REDUNDANT_GT: u32 = 1 << 1;
const FLAG_ZEROED_GTE: u32 = 1 << 2;
const FLAG_COMPRESSED: u32 = 1 << 16;
const FLAG_MARKERS: u32 = 1 << 17;
const KNOWN_FLAGS: u32 =
    FLAG_NEWLINE_TEST | FLAG_REDUNDANT_GT | FLAG_ZEROED_GTE | FLAG_COMPRESSED | FLAG_MARKERS;

/// Entradas por tabla de granos exigidas por la especificación.
const GTES_PER_GT: u64 = 512;
const GT_BYTES: u64 = GTES_PER_GT * 4;
/// Entrada de GT que marca un grano a cero (con el indicador «zeroed-grain»).
const GTE_ZEROED: u32 = 1;
/// Marca interna (en las GT ya validadas) de un grano descartado en modo tolerante. El valor 1 nunca
/// es la ubicación de un grano válido: o es un grano a cero (se guarda como 0) o es inválido.
const GRAIN_DISCARDED: u32 = 1;
/// Marca interna de una entrada de GD sin GT (todo su tramo es disperso).
const GT_ABSENT: u32 = u32::MAX;
/// Marca interna de una entrada de GD descartada en modo tolerante (todos sus granos).
const GT_DISCARDED: u32 = u32::MAX - 1;
const MIN_GRAIN_SECTORS: u64 = 8;
/// Límite propio (512 MiB por grano) para acotar la aritmética; VMware usa 128 sectores.
const MAX_GRAIN_SECTORS: u64 = 1 << 20;
/// `gdOffset` que indica «GD al final del fichero» (streamOptimized).
const GD_AT_END: u64 = u64::MAX;
/// Tamaño máximo del descriptor (incrustado o separado) que se acepta.
pub const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
/// Longitud máxima de una línea del descriptor.
pub const MAX_DESCRIPTOR_LINE: usize = 4096;
/// Caracteres máximos de un texto procedente del fichero dentro de un mensaje de error.
const MAX_REFERENCE_CHARS: usize = 260;
const NO_PARENT_CID: &str = "ffffffff";

fn corrupt(detail: impl Into<String>) -> IoError {
    IoError::Corrupt {
        format: FORMAT,
        detail: detail.into(),
    }
}

fn unsupported(detail: impl Into<String>) -> IoError {
    IoError::UnsupportedFeature {
        format: FORMAT,
        detail: detail.into(),
    }
}

fn external(reference: &str) -> IoError {
    IoError::ExternalReference {
        format: FORMAT,
        reference: sanitize(reference),
    }
}

/// Convierte un texto del fichero en algo seguro para un mensaje: caracteres de control escapados
/// y longitud acotada. Nunca se interpreta como ruta.
fn sanitize(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= MAX_REFERENCE_CHARS {
            out.push('…');
            break;
        }
        if c.is_control() || c == '\u{feff}' {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Descriptor
// ---------------------------------------------------------------------------------------------

/// Modo de acceso de una extensión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentAccess {
    ReadWrite,
    ReadOnly,
    NoAccess,
}

/// Tipo de extensión declarado en el descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentType {
    Sparse,
    Flat,
    Zero,
    Vmfs,
    VmfsSparse,
    VmfsRdm,
    VmfsRaw,
    SeSparse,
}

impl ExtentType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "SPARSE" => Self::Sparse,
            "FLAT" => Self::Flat,
            "ZERO" => Self::Zero,
            "VMFS" => Self::Vmfs,
            "VMFSSPARSE" => Self::VmfsSparse,
            "VMFSRDM" => Self::VmfsRdm,
            "VMFSRAW" => Self::VmfsRaw,
            "SESPARSE" => Self::SeSparse,
            _ => return None,
        })
    }
}

/// Línea de extensión: `ACCESO SECTORES TIPO ["FICHERO" [DESPLAZAMIENTO]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmdkExtent {
    pub access: ExtentAccess,
    /// Tamaño de la extensión en sectores de 512 bytes.
    pub sectors: u64,
    pub kind: ExtentType,
    /// Nombre de fichero tal cual aparece (solo texto: nunca se abre).
    pub file_name: Option<String>,
    /// Desplazamiento en sectores dentro del fichero (extensiones FLAT).
    pub offset: Option<u64>,
}

/// Descriptor de disco VMDK interpretado.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmdkDescriptor {
    pub version: Option<String>,
    pub cid: Option<String>,
    pub parent_cid: Option<String>,
    pub create_type: Option<String>,
    pub parent_file_name_hint: Option<String>,
    pub extents: Vec<VmdkExtent>,
}

impl VmdkDescriptor {
    /// Referencia al disco padre, si existe (`parentCID` distinto de `ffffffff` o `parentFileNameHint`).
    pub fn parent_reference(&self) -> Option<String> {
        if let Some(hint) = &self.parent_file_name_hint {
            return Some(hint.clone());
        }
        match &self.parent_cid {
            Some(cid) if !cid.eq_ignore_ascii_case(NO_PARENT_CID) => {
                Some(format!("disco padre (parentCID={cid})"))
            }
            _ => None,
        }
    }
}

fn set_once(slot: &mut Option<String>, key: &str, value: &str, line: usize) -> Result<()> {
    if slot.is_some() {
        return Err(corrupt(format!(
            "descriptor: clave «{}» repetida (línea {line})",
            sanitize(key)
        )));
    }
    *slot = Some(value.to_string());
    Ok(())
}

fn unquote(v: &str) -> &str {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Interpreta una línea de extensión; `Ok(None)` si la línea no empieza por un modo de acceso.
fn parse_extent(line: &str, n: usize) -> Result<Option<VmdkExtent>> {
    let first = line.split_ascii_whitespace().next().unwrap_or("");
    let access = match first {
        "RW" => ExtentAccess::ReadWrite,
        "RDONLY" => ExtentAccess::ReadOnly,
        "NOACCESS" => ExtentAccess::NoAccess,
        _ => return Ok(None),
    };
    let bad = |what: &str| {
        corrupt(format!(
            "descriptor: extensión no válida en la línea {n}: {what}"
        ))
    };
    let rest = line[first.len()..].trim_start();
    let (size_tok, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let sectors: u64 = size_tok
        .parse()
        .map_err(|_| bad(&format!("tamaño «{}»", sanitize(size_tok))))?;
    let rest = rest.trim_start();
    let (type_tok, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let kind = ExtentType::parse(type_tok)
        .ok_or_else(|| bad(&format!("tipo desconocido «{}»", sanitize(type_tok))))?;
    let rest = rest.trim();
    let (file_name, rest) = if rest.is_empty() {
        (None, "")
    } else if let Some(q) = rest.strip_prefix('"') {
        let end = q
            .find('"')
            .ok_or_else(|| bad("nombre de fichero sin comillas de cierre"))?;
        (Some(q[..end].to_string()), q[end + 1..].trim())
    } else {
        return Err(bad("el nombre de fichero debe ir entre comillas"));
    };
    let offset = if rest.is_empty() {
        None
    } else {
        Some(
            rest.parse::<u64>()
                .map_err(|_| bad(&format!("desplazamiento «{}»", sanitize(rest))))?,
        )
    };
    match (kind, &file_name) {
        (ExtentType::Zero, Some(_)) => return Err(bad("una extensión ZERO no lleva fichero")),
        (ExtentType::Zero, None) => {}
        (_, None) => return Err(bad("falta el nombre de fichero")),
        _ => {}
    }
    Ok(Some(VmdkExtent {
        access,
        sectors,
        kind,
        file_name,
        offset,
    }))
}

/// Interpreta el texto de un descriptor VMDK (incrustado o separado). No valida la semántica
/// (tipo de disco, padre…): eso lo hace [`VmdkImage::open`]. Nunca abre los ficheros citados.
pub fn parse_descriptor(text: &str) -> Result<VmdkDescriptor> {
    if text.len() > MAX_DESCRIPTOR_BYTES {
        return Err(corrupt(format!(
            "descriptor demasiado grande ({} bytes; máximo {MAX_DESCRIPTOR_BYTES})",
            text.len()
        )));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut d = VmdkDescriptor::default();
    for (i, raw) in text.split('\n').enumerate() {
        let n = i + 1;
        if raw.len() > MAX_DESCRIPTOR_LINE {
            return Err(corrupt(format!(
                "descriptor: línea {n} demasiado larga ({} bytes; máximo {MAX_DESCRIPTOR_LINE})",
                raw.len()
            )));
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.chars().any(|c| c.is_control() && c != '\t') {
            return Err(corrupt(format!(
                "descriptor: carácter de control en la línea {n}: «{}»",
                sanitize(line)
            )));
        }
        if let Some(ext) = parse_extent(line, n)? {
            d.extents.push(ext);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(corrupt(format!(
                "descriptor: línea {n} no reconocida: «{}»",
                sanitize(line)
            )));
        };
        let key = k.trim();
        let value = unquote(v.trim());
        if key.eq_ignore_ascii_case("version") {
            set_once(&mut d.version, key, value, n)?;
        } else if key.eq_ignore_ascii_case("CID") {
            set_once(&mut d.cid, key, value, n)?;
        } else if key.eq_ignore_ascii_case("parentCID") {
            set_once(&mut d.parent_cid, key, value, n)?;
        } else if key.eq_ignore_ascii_case("createType") {
            set_once(&mut d.create_type, key, value, n)?;
        } else if key.eq_ignore_ascii_case("parentFileNameHint") {
            set_once(&mut d.parent_file_name_hint, key, value, n)?;
        } else if key.is_empty() {
            return Err(corrupt(format!("descriptor: clave vacía en la línea {n}")));
        }
        // Resto (ddb.*, encoding, isNativeSnapshot…): informativo, se ignora.
    }
    Ok(d)
}

/// Texto del descriptor: hasta el primer NUL, con conversión UTF-8 tolerante (los bytes no
/// válidos se sustituyen; solo se usan como texto).
fn descriptor_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// `true` si el nombre de extensión apunta claramente a otro fichero (ruta, unidad, UNC…).
fn names_other_file(name: &str) -> bool {
    name.is_empty()
        || name.contains(['/', '\\', ':'])
        || name == "."
        || name == ".."
        || name.trim() != name
}

// ---------------------------------------------------------------------------------------------
// Cabecera
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SparseHeader {
    version: u32,
    flags: u32,
    capacity: u64,
    grain_size: u64,
    descriptor_offset: u64,
    descriptor_size: u64,
    num_gtes_per_gt: u32,
    gd_offset: u64,
    unclean_shutdown: bool,
    newline_chars: [u8; 4],
    compress_algorithm: u16,
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn le_u64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

impl SparseHeader {
    /// `b` mide `HEADER_LEN` bytes y empieza por `KDMV`.
    fn parse(b: &[u8; HEADER_LEN]) -> Self {
        Self {
            version: le_u32(b, 4),
            flags: le_u32(b, 8),
            capacity: le_u64(b, 12),
            grain_size: le_u64(b, 20),
            descriptor_offset: le_u64(b, 28),
            descriptor_size: le_u64(b, 36),
            num_gtes_per_gt: le_u32(b, 44),
            // rgdOffset (48) no se usa.
            gd_offset: le_u64(b, 56),
            // overHead (64) no se usa.
            unclean_shutdown: b[72] != 0,
            newline_chars: [b[73], b[74], b[75], b[76]],
            compress_algorithm: u16::from_le_bytes([b[77], b[78]]),
        }
    }

    /// Validaciones que no dependen del tamaño del fichero.
    fn validate(&self) -> Result<()> {
        if !(1..=3).contains(&self.version) {
            return Err(unsupported(format!(
                "versión de cabecera dispersa {} desconocida (se admiten 1 a 3)",
                self.version
            )));
        }
        if self.flags & (FLAG_COMPRESSED | FLAG_MARKERS) != 0
            || self.gd_offset == GD_AT_END
            || self.compress_algorithm != 0
        {
            return Err(unsupported(
                "streamOptimized (granos comprimidos, marcadores o directorio al final)",
            ));
        }
        let unknown = self.flags & !KNOWN_FLAGS;
        if unknown != 0 {
            return Err(unsupported(format!(
                "indicadores de cabecera desconocidos 0x{unknown:08x}"
            )));
        }
        if self.flags & FLAG_NEWLINE_TEST != 0 && self.newline_chars != *b"\n \r\n" {
            return Err(corrupt(format!(
                "caracteres de prueba de fin de línea alterados ({:02x?}; se esperaba 0a 20 0d 0a): \
                 ¿fichero transferido en modo texto?",
                self.newline_chars
            )));
        }
        if !self.grain_size.is_power_of_two()
            || !(MIN_GRAIN_SECTORS..=MAX_GRAIN_SECTORS).contains(&self.grain_size)
        {
            return Err(corrupt(format!(
                "tamaño de grano no válido: {} sectores (potencia de 2 entre {MIN_GRAIN_SECTORS} y {MAX_GRAIN_SECTORS})",
                self.grain_size
            )));
        }
        if u64::from(self.num_gtes_per_gt) != GTES_PER_GT {
            return Err(corrupt(format!(
                "numGTEsPerGT = {} (la especificación exige {GTES_PER_GT})",
                self.num_gtes_per_gt
            )));
        }
        if self.capacity > u64::MAX / SECTOR {
            return Err(corrupt(format!(
                "capacidad imposible: {} sectores",
                self.capacity
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Imagen
// ---------------------------------------------------------------------------------------------

/// Detalle de un error `Corrupt` (sin repetir el prefijo del formato al volver a envolverlo).
fn detail(e: IoError) -> String {
    match e {
        IoError::Corrupt { detail, .. } => detail,
        other => other.to_string(),
    }
}

/// Lectura posicional completa; reintenta `Interrupted` y convierte el fin de fichero prematuro
/// (fichero acortado tras abrirlo) en error, nunca en datos inventados.
fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    mr_io::read_exact_at(file, offset, buf).map_err(|source| IoError::Read {
        offset,
        len: buf.len(),
        source,
    })
}

/// Ubicación de un grano.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grain {
    /// No asignado o marcado a cero: ceros por definición.
    Sparse,
    /// Desplazamiento en bytes del grano dentro del fichero.
    Data(u64),
    /// Descartado en modo tolerante: no fiable.
    Discarded,
}

/// Disco VMDK monolithicSparse abierto en solo lectura.
pub struct VmdkImage {
    file: File,
    header: SparseHeader,
    /// Tamaño virtual en bytes (`capacity × 512`).
    size: u64,
    /// Tamaño de grano en bytes.
    grain_bytes: u64,
    /// Número de granos virtuales (`ceil(capacity / grainSize)`).
    grains: u64,
    /// Por cada entrada del GD principal: ordinal de su GT en `gtes`, [`GT_ABSENT`] o
    /// [`GT_DISCARDED`].
    gt_slot: Vec<u32>,
    /// Entradas validadas de las GT presentes, `GTES_PER_GT` por GT: 0 = disperso (sin asignar o a
    /// cero), [`GRAIN_DISCARDED`] o el sector del grano en el fichero.
    gtes: Vec<u32>,
    descriptor: VmdkDescriptor,
    tolerant: bool,
    /// Granos descartados en modo tolerante.
    discarded: u64,
}

impl std::fmt::Debug for VmdkImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmdkImage")
            .field("size", &self.size)
            .field("grain_bytes", &self.grain_bytes)
            .field("gd_entries", &self.gt_slot.len())
            .finish()
    }
}

impl VmdkImage {
    /// Abre un VMDK monolithicSparse en modo estricto. `file` ya está admitido y abierto en solo
    /// lectura.
    pub fn open(file: File) -> Result<Self> {
        Self::open_with(file, false)
    }

    /// Como [`Self::open`]; con `tolerant`, las entradas de GD y GT inválidas o ambiguas no impiden
    /// abrir la imagen: sus granos se descartan (ver la documentación del módulo).
    pub fn open_with(file: File, tolerant: bool) -> Result<Self> {
        let file_len = file.metadata()?.len();
        let mut head = [0u8; HEADER_LEN];
        let head_len = file_len.min(HEADER_LEN as u64) as usize;
        read_exact_at(&file, 0, &mut head[..head_len])?;

        if head_len < 4 || head[..4] != *MAGIC {
            return Err(Self::reject_non_sparse(&file, file_len, &head[..head_len]));
        }
        if head_len < HEADER_LEN {
            return Err(corrupt(format!(
                "fichero demasiado pequeño para la cabecera ({file_len} bytes)"
            )));
        }
        let header = SparseHeader::parse(&head);
        header.validate()?;

        // Descriptor incrustado.
        if header.descriptor_offset == 0 {
            return Err(unsupported(
                "extensión dispersa sin descriptor incrustado: forma parte de un disco descrito \
                 por otro fichero (p. ej. twoGbMaxExtentSparse)",
            ));
        }
        let desc_bytes = header
            .descriptor_size
            .checked_mul(SECTOR)
            .filter(|&b| b > 0 && b <= MAX_DESCRIPTOR_BYTES as u64)
            .ok_or_else(|| {
                corrupt(format!(
                    "tamaño de descriptor no válido: {} sectores",
                    header.descriptor_size
                ))
            })?;
        let desc_off =
            Self::checked_region("descriptor", header.descriptor_offset, desc_bytes, file_len)?;
        let mut desc_raw = vec![0u8; desc_bytes as usize];
        read_exact_at(&file, desc_off, &mut desc_raw)?;
        let descriptor = parse_descriptor(&descriptor_text(&desc_raw))?;
        Self::validate_descriptor(&descriptor, header.capacity)?;

        // Directorio de granos.
        let grain_bytes = header.grain_size * SECTOR;
        let grains = header.capacity.div_ceil(header.grain_size);
        let gd_entries = grains.div_ceil(GTES_PER_GT);
        let gd_bytes = gd_entries
            .checked_mul(4)
            .filter(|&b| b <= MAX_TABLE_BYTES)
            .ok_or_else(|| {
                corrupt(format!(
                    "directorio de granos demasiado grande ({gd_entries} entradas; límite {MAX_TABLE_BYTES} bytes)"
                ))
            })?;
        if header.gd_offset == 0 && gd_entries > 0 {
            return Err(corrupt("gdOffset = 0"));
        }
        let mut budget = Budget::new(FORMAT);
        budget.add_tables(gd_bytes);
        let mut gd: Vec<u32> = budget.vec(gd_entries, "directorio de granos")?;
        let mut zones = vec![
            Zone::fixed(0, HEADER_LEN as u64),
            Zone::fixed(desc_off, desc_bytes),
        ];
        if gd_entries > 0 {
            let gd_off =
                Self::checked_region("directorio de granos", header.gd_offset, gd_bytes, file_len)?;
            if desc_off < gd_off + gd_bytes && gd_off < desc_off + desc_bytes {
                return Err(corrupt(
                    "el descriptor y el directorio de granos se solapan",
                ));
            }
            // Por trozos: el GD en bruto nunca se guarda entero.
            let mut buf = vec![0u8; TABLE_CHUNK.min(gd_bytes as usize)];
            let mut pos = 0u64;
            while pos < gd_bytes {
                let len = (gd_bytes - pos).min(buf.len() as u64) as usize;
                let chunk = &mut buf[..len];
                read_exact_at(&file, gd_off + pos, chunk)?;
                gd.extend(
                    chunk
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| u32::from_le_bytes(*c)),
                );
                pos += len as u64;
            }
            zones.push(Zone::fixed(gd_off, gd_bytes));
        }
        let size = header.capacity * SECTOR;
        let grain_span = |g: u64| -> u64 { grain_bytes.min(size - g * grain_bytes) };
        let (gt_slot, gtes, discarded) = Self::load_tables(
            &file,
            file_len,
            &header,
            gd,
            grains,
            grain_span,
            &zones,
            tolerant,
            &mut budget,
        )?;

        Ok(Self {
            file,
            size,
            grain_bytes,
            grains,
            gt_slot,
            gtes,
            descriptor,
            header,
            tolerant,
            discarded,
        })
    }

    /// Valida las GT del GD principal `gd` y sus granos, y devuelve `(gt_slot, gtes, descartados)`.
    /// `gd` se reutiliza como `gt_slot` (cada entrada se lee antes de sobrescribirla).
    ///
    /// Correspondencia inequívoca: cada GT (2 KiB) y cada grano (su parte útil) quedan enteros dentro
    /// del fichero y no solapan la cabecera, el descriptor, el GD, otra GT ni otro grano. Si un grano
    /// solapa una GT se descartan los dos (y con la GT, todos sus granos). En modo estricto la
    /// apertura falla en la primera entrada inválida.
    #[allow(clippy::too_many_arguments)]
    fn load_tables(
        file: &File,
        file_len: u64,
        header: &SparseHeader,
        gd: Vec<u32>,
        grains: u64,
        grain_span: impl Fn(u64) -> u64,
        zones: &[Zone],
        tolerant: bool,
        budget: &mut Budget,
    ) -> Result<(Vec<u32>, Vec<u32>, u64)> {
        // `[sector × 512, + bytes)` dentro del fichero (sin construir el mensaje de error).
        let fits = |sector: u32, bytes: u64| {
            (u64::from(sector) * SECTOR)
                .checked_add(bytes)
                .is_some_and(|end| end <= file_len)
        };

        // 1) Tablas de granos. Índices `u32`: el GD tiene como mucho MAX_TABLE_BYTES / 4 = 2^26
        //    entradas.
        let gd_entries = gd.len() as u64;
        let mut gt_damage = Damage::new(FORMAT, gd_entries, tolerant, budget)?;
        let mut in_file = 0u64;
        for (i, &gt) in gd.iter().enumerate() {
            if gt == 0 {
                continue;
            }
            if fits(gt, GT_BYTES) {
                in_file += 1;
            } else {
                let e = Self::checked_region(
                    &format!("tabla de granos {i}"),
                    u64::from(gt),
                    GT_BYTES,
                    file_len,
                )
                .err()
                .map(detail)
                .unwrap_or_default();
                gt_damage.add(i as u64, || e)?;
            }
        }
        let mut candidates: Vec<u32> = budget.vec(in_file, "lista de tablas de granos")?;
        candidates.extend(
            gd.iter()
                .enumerate()
                .filter(|&(_, &gt)| gt != 0 && fits(gt, GT_BYTES))
                .map(|(i, _)| i as u32),
        );
        let gt_span = |i: u32| -> (u64, u64) {
            let start = u64::from(gd[i as usize]) * SECTOR;
            (start, start + GT_BYTES)
        };
        tolerant::overlaps(&mut candidates, gt_span, &mut zones.to_vec(), |hit| {
            match hit {
            Hit::Entry(i) => gt_damage.add(u64::from(i), || {
                format!("la tabla de granos {i} solapa la cabecera, el descriptor, el directorio u otra tabla")
            }),
            Hit::Tag(_) => Ok(()),
        }
        })?;
        budget.release(candidates);

        let present = gd
            .iter()
            .enumerate()
            .filter(|&(i, &gt)| gt != 0 && !gt_damage.contains(i as u64))
            .count() as u64;
        let gt_table_bytes = present * GT_BYTES;
        if gt_table_bytes > MAX_TABLE_BYTES {
            return Err(unsupported(format!(
                "las tablas de granos ocupan {gt_table_bytes} bytes (límite de memoria {MAX_TABLE_BYTES})"
            )));
        }
        budget.add_tables(gt_table_bytes);

        // 2) Entradas de las GT válidas. Las posiciones en `gtes` (< 2^26, porque las GT ocupan
        //    como mucho MAX_TABLE_BYTES) identifican los granos en el registro de daños y en la
        //    detección de solapes; `slot_gd` da el índice del GD de cada GT presente.
        let mut gtes: Vec<u32> = budget.vec(present * GTES_PER_GT, "tablas de granos")?;
        let mut slot_gd: Vec<u32> = budget.vec(present, "índice de tablas de granos")?;
        let mut gt_zones: Vec<Zone> = budget.vec(
            zones.len() as u64 + present,
            "zonas de las tablas de granos",
        )?;
        gt_zones.extend_from_slice(zones);
        let mut grain_damage = Damage::new(FORMAT, present * GTES_PER_GT, tolerant, budget)?;
        let mut allocated = 0u64;
        let mut gt_slot = gd;
        let mut raw = [0u8; GT_BYTES as usize];
        for (i, slot) in gt_slot.iter_mut().enumerate() {
            let gt = *slot;
            if gt == 0 {
                *slot = GT_ABSENT;
                continue;
            }
            if gt_damage.contains(i as u64) {
                *slot = GT_DISCARDED;
                continue;
            }
            let start = u64::from(gt) * SECTOR;
            read_exact_at(file, start, &mut raw)?;
            *slot = slot_gd.len() as u32;
            slot_gd.push(i as u32);
            gt_zones.push(Zone {
                start,
                end: start + GT_BYTES,
                tag: Some(i as u64),
            });
            for (j, c) in raw.as_chunks::<4>().0.iter().enumerate() {
                let entry = u32::from_le_bytes(*c);
                let g = i as u64 * GTES_PER_GT + j as u64;
                let p = gtes.len() as u64;
                let value = if g >= grains || entry == 0 {
                    0
                } else if entry == GTE_ZEROED {
                    if header.flags & FLAG_ZEROED_GTE != 0 {
                        0
                    } else {
                        grain_damage.add(p, || {
                            format!(
                                "grano {g} apunta al sector 1 sin el indicador de granos a cero"
                            )
                        })?;
                        GRAIN_DISCARDED
                    }
                } else {
                    if fits(entry, grain_span(g)) {
                        allocated += 1;
                    } else {
                        let e = Self::checked_region(
                            &format!("grano {g}"),
                            u64::from(entry),
                            grain_span(g),
                            file_len,
                        )
                        .err()
                        .map(detail)
                        .unwrap_or_default();
                        grain_damage.add(p, || e)?;
                    }
                    entry
                };
                gtes.push(value);
            }
        }
        gt_damage.release(budget);

        // 3) Solapes de los granos entre sí y con las estructuras y las GT.
        let grain_of = |p: u32| -> u64 {
            u64::from(slot_gd[(u64::from(p) / GTES_PER_GT) as usize]) * GTES_PER_GT
                + u64::from(p) % GTES_PER_GT
        };
        // Candidatos: granos con ubicación (> GRAIN_DISCARDED) que no se han descartado ya.
        let mut candidates: Vec<u32> = budget.vec(allocated, "lista de granos asignados")?;
        candidates.extend(
            gtes.iter()
                .enumerate()
                .filter(|&(p, &v)| v > GRAIN_DISCARDED && !grain_damage.contains(p as u64))
                .map(|(p, _)| p as u32),
        );
        let span = |p: u32| -> (u64, u64) {
            let start = u64::from(gtes[p as usize]) * SECTOR;
            (start, start + grain_span(grain_of(p)))
        };
        // GT solapadas por un grano: como mucho una por GT presente.
        let mut bad_gts: Vec<u32> = Vec::new();
        tolerant::overlaps(&mut candidates, span, &mut gt_zones, |hit| match hit {
            Hit::Entry(p) => {
                let g = grain_of(p);
                grain_damage.add(u64::from(p), || {
                    format!("el grano {g} solapa las estructuras del contenedor, una tabla de granos u otro grano")
                })
            }
            Hit::Tag(i) => {
                if !tolerant {
                    return Err(corrupt(format!(
                        "la tabla de granos {i} se solapa con un grano"
                    )));
                }
                bad_gts.push(i as u32);
                Ok(())
            }
        })?;
        budget.release(candidates);
        budget.release(gt_zones);
        for p in grain_damage.iter() {
            gtes[p as usize] = GRAIN_DISCARDED;
        }
        for &i in &bad_gts {
            gt_slot[i as usize] = GT_DISCARDED;
        }

        // 4) Recuento de granos descartados (los de las GT descartadas cuentan una sola vez).
        let covered = |i: u64| GTES_PER_GT.min(grains.saturating_sub(i * GTES_PER_GT));
        let mut discarded: u64 = gt_slot
            .iter()
            .enumerate()
            .filter(|(_, &s)| s == GT_DISCARDED)
            .map(|(i, _)| covered(i as u64))
            .sum();
        discarded += grain_damage
            .iter()
            .filter(|&p| {
                let i = slot_gd[(p / GTES_PER_GT) as usize];
                gt_slot[i as usize] != GT_DISCARDED
            })
            .count() as u64;
        grain_damage.release(budget);
        budget.release(slot_gd);
        Ok((gt_slot, gtes, discarded))
    }

    /// Comprueba que `[sector × 512, + bytes)` está dentro del fichero y devuelve el desplazamiento.
    fn checked_region(what: &str, sector: u64, bytes: u64, file_len: u64) -> Result<u64> {
        sector
            .checked_mul(SECTOR)
            .and_then(|off| off.checked_add(bytes).map(|end| (off, end)))
            .filter(|&(_, end)| end <= file_len)
            .map(|(off, _)| off)
            .ok_or_else(|| {
                corrupt(format!(
                    "{what} fuera del fichero (sector {sector}, {bytes} bytes; el fichero mide {file_len})"
                ))
            })
    }

    /// Error para un fichero sin firma `KDMV`.
    fn reject_non_sparse(file: &File, file_len: u64, head: &[u8]) -> IoError {
        if head.len() >= 4 && head[..4] == *COWD_MAGIC {
            return unsupported("disco disperso de ESX antiguo (firma COWD)");
        }
        let body = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
        let n = TEXT_DESCRIPTOR_SIGNATURE.len();
        let is_text = body.len() >= n && body[..n].eq_ignore_ascii_case(TEXT_DESCRIPTOR_SIGNATURE);
        if !is_text {
            return corrupt("firma «KDMV» no encontrada");
        }
        // Descriptor de texto separado: se interpreta solo para informar; nunca se sigue.
        let len = file_len.min(MAX_DESCRIPTOR_BYTES as u64) as usize;
        let mut raw = vec![0u8; len];
        let mut detail = String::from("descriptor de texto separado");
        if read_exact_at(file, 0, &mut raw).is_ok() {
            if let Ok(d) = parse_descriptor(&descriptor_text(&raw)) {
                if let Some(t) = &d.create_type {
                    detail.push_str(&format!(" (createType «{}»)", sanitize(t)));
                }
                let names: Vec<String> = d
                    .extents
                    .iter()
                    .filter_map(|e| e.file_name.as_deref().map(sanitize))
                    .collect();
                if !names.is_empty() {
                    // Siempre referencia otros ficheros: se informa cuáles, sin abrirlos.
                    return IoError::ExternalReference {
                        format: "VMDK",
                        reference: names.join("», «"),
                    };
                }
            }
        }
        detail.push_str("; abra el fichero de extensión dispersa monolítico");
        unsupported(detail)
    }

    /// Semántica del descriptor incrustado de un monolithicSparse.
    fn validate_descriptor(d: &VmdkDescriptor, capacity: u64) -> Result<()> {
        if let Some(parent) = d.parent_reference() {
            return Err(external(&parent));
        }
        let create = d
            .create_type
            .as_deref()
            .ok_or_else(|| corrupt("descriptor sin createType"))?;
        if create.eq_ignore_ascii_case("streamOptimized") {
            return Err(unsupported("streamOptimized"));
        }
        if !create.eq_ignore_ascii_case("monolithicSparse") {
            return Err(unsupported(format!(
                "createType «{}» en una extensión dispersa (solo se admite monolithicSparse)",
                sanitize(create)
            )));
        }
        let ext = match d.extents.as_slice() {
            [] => return Err(corrupt("descriptor sin extensiones")),
            [one] => one,
            many => {
                // Las extensiones adicionales viven necesariamente en otros ficheros.
                let names: Vec<&str> = many
                    .iter()
                    .skip(1)
                    .filter_map(|e| e.file_name.as_deref())
                    .collect();
                if names.is_empty() {
                    return Err(unsupported(format!(
                        "{} extensiones en un monolithicSparse",
                        many.len()
                    )));
                }
                return Err(external(&names.join(", ")));
            }
        };
        let name = ext.file_name.as_deref().unwrap_or("");
        if ext.kind != ExtentType::Sparse {
            return match ext.file_name.as_deref() {
                Some(n) => Err(external(n)),
                None => Err(unsupported(format!(
                    "extensión de tipo {:?} en un monolithicSparse",
                    ext.kind
                ))),
            };
        }
        if names_other_file(name) {
            return Err(external(name));
        }
        if ext.access == ExtentAccess::NoAccess {
            return Err(unsupported("extensión NOACCESS"));
        }
        if ext.sectors != capacity {
            return Err(corrupt(format!(
                "la extensión del descriptor mide {} sectores y la cabecera declara {capacity}",
                ext.sectors
            )));
        }
        Ok(())
    }

    /// Descriptor incrustado interpretado.
    pub fn descriptor(&self) -> &VmdkDescriptor {
        &self.descriptor
    }

    /// Tamaño de grano en bytes.
    pub fn grain_size_bytes(&self) -> u64 {
        self.grain_bytes
    }

    /// Variante legible.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "VMDK monolithicSparse (cabecera v{}), granos de {} KiB",
            self.header.version,
            self.grain_bytes / 1024
        );
        if self.header.flags & FLAG_ZEROED_GTE != 0 {
            s.push_str(", granos a cero");
        }
        if self.header.unclean_shutdown {
            s.push_str(", cierre no limpio");
        }
        if self.tolerant {
            s.push_str(&tolerant::describe_suffix(
                self.discarded,
                "grano",
                "granos",
            ));
        }
        s
    }

    /// `true` si se abrió en modo tolerante.
    pub fn is_tolerant(&self) -> bool {
        self.tolerant
    }

    /// Granos descartados en modo tolerante (0 en modo estricto).
    pub fn discarded_blocks(&self) -> u64 {
        self.discarded
    }

    /// Ubicación validada del grano `g` (`g < grains`).
    fn resolve(&self, g: u64) -> Result<Grain> {
        let gd_idx = usize::try_from(g / GTES_PER_GT).map_err(|_| corrupt("índice de grano"))?;
        let slot = *self
            .gt_slot
            .get(gd_idx)
            .ok_or_else(|| corrupt(format!("grano {g} fuera del directorio")))?;
        let entry = match slot {
            GT_ABSENT => return Ok(Grain::Sparse),
            GT_DISCARDED => return Ok(Grain::Discarded),
            k => {
                let p = u64::from(k) * GTES_PER_GT + g % GTES_PER_GT;
                usize::try_from(p)
                    .ok()
                    .and_then(|p| self.gtes.get(p))
                    .copied()
                    .ok_or_else(|| corrupt(format!("grano {g} fuera de las tablas")))?
            }
        };
        Ok(match entry {
            0 => Grain::Sparse,
            GRAIN_DISCARDED => Grain::Discarded,
            // Validado al abrir: dentro del fichero y sin solapes.
            sector => Grain::Data(u64::from(sector) * SECTOR),
        })
    }
}

impl BlockDevice for VmdkImage {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        SECTOR as u32
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let oob = || IoError::OutOfBounds {
            offset,
            len: buf.len(),
            size: self.size,
        };
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&e| e <= self.size)
            .ok_or_else(oob)?;
        let mut pos = offset;
        let mut done = 0usize;
        while pos < end {
            let g = pos / self.grain_bytes;
            let within = pos % self.grain_bytes;
            let n = (self.grain_bytes - within).min(end - pos) as usize;
            let chunk = &mut buf[done..done + n];
            match self.resolve(g)? {
                Grain::Sparse => chunk.fill(0),
                Grain::Data(base) => read_exact_at(&self.file, base + within, chunk)?,
                Grain::Discarded => {
                    return Err(tolerant::discarded_read(FORMAT, "grano", g, pos, n));
                }
            }
            pos += n as u64;
            done += n;
        }
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        let end = match lba.checked_add(count) {
            Some(e) if count > 0 && e <= total => e,
            _ => return Err(IoError::InvalidRange { lba, count, total }),
        };
        let spg = self.header.grain_size;
        let mut runs: Vec<AllocationRun> = Vec::new();
        let mut s = lba;
        while s < end {
            let g = s / spg;
            if g >= self.grains {
                return Err(corrupt(format!("sector {s} fuera de los granos")));
            }
            let gd_idx = g / GTES_PER_GT;
            let slot = usize::try_from(gd_idx)
                .ok()
                .and_then(|i| self.gt_slot.get(i))
                .copied()
                .ok_or_else(|| corrupt(format!("grano {g} fuera del directorio")))?;
            let next_gt = (gd_idx + 1).saturating_mul(GTES_PER_GT).saturating_mul(spg);
            let (kind, span_end) = match slot {
                // GT ausente: los 512 granos que cubriría son dispersos.
                GT_ABSENT => (Allocation::Sparse, next_gt),
                // GT descartada: sus granos no son fiables; hay que «leerlos» para descubrirlo.
                GT_DISCARDED => (Allocation::Data, next_gt),
                _ => {
                    let kind = match self.resolve(g)? {
                        Grain::Sparse => Allocation::Sparse,
                        Grain::Data(_) | Grain::Discarded => Allocation::Data,
                    };
                    (kind, (g + 1).saturating_mul(spg))
                }
            };
            let e = span_end.min(end);
            match runs.last_mut() {
                Some((start, len, k)) if *k == kind && *start + *len == s => *len += e - s,
                _ => runs.push((s, e - s, kind)),
            }
            s = e;
        }
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const GRAIN: u64 = 16; // sectores (8 KiB) para imágenes de prueba pequeñas

    /// Descriptor monolithicSparse correcto para `capacity` sectores.
    fn descriptor(capacity: u64) -> String {
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=12345678\nparentCID=ffffffff\n\
             createType=\"monolithicSparse\"\n\n# Extent description\n\
             RW {capacity} SPARSE \"prueba.vmdk\"\n\n# The Disk Data Base\n#DDB\n\
             ddb.virtualHWVersion = \"4\"\n"
        )
    }

    /// Generador de imágenes sintéticas.
    struct Builder {
        version: u32,
        flags: u32,
        capacity: u64,
        grain: u64,
        gtes: u32,
        gd_offset: Option<u64>,
        newline: [u8; 4],
        descriptor: String,
        /// (índice de grano, byte de relleno); relleno 0 → entrada GTE_ZEROED.
        grains: Vec<(u64, u8)>,
        /// Entradas de GT forzadas: (índice de grano, valor).
        raw_gte: Vec<(u64, u32)>,
        /// Entradas de GD forzadas: (índice, valor).
        raw_gde: Vec<(usize, u32)>,
        truncate_to: Option<usize>,
    }

    impl Builder {
        fn new(capacity: u64) -> Self {
            Self {
                version: 1,
                flags: FLAG_NEWLINE_TEST | FLAG_ZEROED_GTE,
                capacity,
                grain: GRAIN,
                gtes: 512,
                gd_offset: None,
                newline: *b"\n \r\n",
                descriptor: descriptor(capacity),
                grains: Vec::new(),
                raw_gte: Vec::new(),
                raw_gde: Vec::new(),
                truncate_to: None,
            }
        }

        fn build(&self) -> Vec<u8> {
            let desc_sectors = 4u64;
            // El diseño usa siempre el grano de prueba; los valores forzados solo van en la cabecera.
            let layout_grain =
                if self.grain.is_power_of_two() && self.grain >= 8 && self.grain <= 1024 {
                    self.grain
                } else {
                    GRAIN
                };
            let layout_cap = self.capacity.min(1 << 24);
            let ngrains = layout_cap.div_ceil(layout_grain);
            let gd_entries = ngrains.div_ceil(512).max(1);
            let gd_sector = 1 + desc_sectors;
            let gd_sectors = (gd_entries * 4).div_ceil(512);
            let gt_first = gd_sector + gd_sectors;
            let gt_total_sectors = gd_entries * 4; // 2 KiB por GT
            let data_first = gt_first + gt_total_sectors;
            let mut gd: Vec<u32> = (0..gd_entries).map(|i| (gt_first + i * 4) as u32).collect();
            let mut gt = vec![0u32; (gd_entries * 512) as usize];
            let mut data = Vec::new();
            for &(g, fill) in &self.grains {
                if fill == 0 {
                    gt[g as usize] = GTE_ZEROED;
                } else {
                    gt[g as usize] = (data_first + (data.len() as u64) / 512) as u32;
                    data.extend(std::iter::repeat_n(fill, (layout_grain * 512) as usize));
                }
            }
            for &(g, v) in &self.raw_gte {
                gt[g as usize] = v;
            }
            for &(i, v) in &self.raw_gde {
                gd[i] = v;
            }
            let mut f = vec![0u8; (data_first * 512) as usize];
            f[0..4].copy_from_slice(MAGIC);
            f[4..8].copy_from_slice(&self.version.to_le_bytes());
            f[8..12].copy_from_slice(&self.flags.to_le_bytes());
            f[12..20].copy_from_slice(&self.capacity.to_le_bytes());
            f[20..28].copy_from_slice(&self.grain.to_le_bytes());
            f[28..36].copy_from_slice(&1u64.to_le_bytes());
            f[36..44].copy_from_slice(&desc_sectors.to_le_bytes());
            f[44..48].copy_from_slice(&self.gtes.to_le_bytes());
            f[48..56].copy_from_slice(&0u64.to_le_bytes());
            f[56..64].copy_from_slice(&self.gd_offset.unwrap_or(gd_sector).to_le_bytes());
            f[64..72].copy_from_slice(&data_first.to_le_bytes());
            f[73..77].copy_from_slice(&self.newline);
            let d = self.descriptor.as_bytes();
            let dl = d.len().min((desc_sectors * 512) as usize);
            f[512..512 + dl].copy_from_slice(&d[..dl]);
            let gdo = (gd_sector * 512) as usize;
            for (i, v) in gd.iter().enumerate() {
                f[gdo + i * 4..gdo + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            let gto = (gt_first * 512) as usize;
            for (i, v) in gt.iter().enumerate() {
                f[gto + i * 4..gto + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            f.extend_from_slice(&data);
            if let Some(t) = self.truncate_to {
                f.truncate(t);
            }
            f
        }

        fn open(&self) -> Result<VmdkImage> {
            open_bytes(&self.build())
        }
    }

    fn open_bytes(bytes: &[u8]) -> Result<VmdkImage> {
        let mut tmp = tempfile::tempfile().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        VmdkImage::open(tmp)
    }

    fn is_corrupt<T: std::fmt::Debug>(r: Result<T>) -> bool {
        matches!(r, Err(IoError::Corrupt { format: "VMDK", .. }))
    }
    fn is_unsupported<T: std::fmt::Debug>(r: Result<T>) -> bool {
        matches!(r, Err(IoError::UnsupportedFeature { format: "VMDK", .. }))
    }
    fn external_ref<T: std::fmt::Debug>(r: Result<T>) -> String {
        match r {
            Err(IoError::ExternalReference {
                format: "VMDK",
                reference,
            }) => reference,
            other => panic!("se esperaba ExternalReference: {other:?}"),
        }
    }

    // ---- descriptor ----

    #[test]
    fn parses_qemu_style_descriptor() {
        let d = parse_descriptor(&descriptor(5860)).unwrap();
        assert_eq!(d.version.as_deref(), Some("1"));
        assert_eq!(d.cid.as_deref(), Some("12345678"));
        assert_eq!(d.parent_cid.as_deref(), Some("ffffffff"));
        assert_eq!(d.create_type.as_deref(), Some("monolithicSparse"));
        assert_eq!(d.parent_reference(), None);
        assert_eq!(
            d.extents,
            vec![VmdkExtent {
                access: ExtentAccess::ReadWrite,
                sectors: 5860,
                kind: ExtentType::Sparse,
                file_name: Some("prueba.vmdk".into()),
                offset: None,
            }]
        );
    }

    #[test]
    fn parses_flat_zero_crlf_bom_and_spaces_in_names() {
        let t =
            "\u{feff}# Disk DescriptorFile\r\nversion=1\r\ncreateType=\"twoGbMaxExtentFlat\"\r\n\
                 RDONLY 100 FLAT \"mi disco-f001.vmdk\" 7\r\nNOACCESS 50 ZERO\r\n\
                 parentFileNameHint=\"base.vmdk\"\r\n";
        let d = parse_descriptor(t).unwrap();
        assert_eq!(d.extents.len(), 2);
        assert_eq!(d.extents[0].access, ExtentAccess::ReadOnly);
        assert_eq!(d.extents[0].kind, ExtentType::Flat);
        assert_eq!(
            d.extents[0].file_name.as_deref(),
            Some("mi disco-f001.vmdk")
        );
        assert_eq!(d.extents[0].offset, Some(7));
        assert_eq!(d.extents[1].kind, ExtentType::Zero);
        assert_eq!(d.extents[1].file_name, None);
        assert_eq!(d.parent_reference().as_deref(), Some("base.vmdk"));
    }

    #[test]
    fn malicious_descriptors_are_text_only() {
        for path in [
            r"..\..\Windows\System32\config\SAM",
            r"C:\Windows\win.ini",
            r"\\servidor\recurso\disco.vmdk",
            "/etc/passwd",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
        ] {
            // Como nombre de la extensión.
            let mut b = Builder::new(64);
            b.descriptor = descriptor(64).replace("prueba.vmdk", path);
            assert_eq!(external_ref(b.open()), path);
            // Como padre.
            let mut b = Builder::new(64);
            b.descriptor = descriptor(64) + &format!("parentFileNameHint=\"{path}\"\n");
            assert_eq!(external_ref(b.open()), path);
        }
    }

    #[test]
    fn huge_lines_and_odd_encodings() {
        let long = format!("createType=\"{}\"\n", "a".repeat(MAX_DESCRIPTOR_LINE + 1));
        assert!(is_corrupt(parse_descriptor(&long)));
        let huge = "#".repeat(MAX_DESCRIPTOR_BYTES + 1);
        assert!(is_corrupt(parse_descriptor(&huge)));
        // Caracteres de control y líneas basura.
        assert!(is_corrupt(parse_descriptor("createType=\"x\u{7}\"\n")));
        assert!(is_corrupt(parse_descriptor("basura sin igual\n")));
        assert!(is_corrupt(parse_descriptor("RW 10 SPARSE \"sin cierre\n")));
        assert!(is_corrupt(parse_descriptor("RW 10 SPARSE sin_comillas\n")));
        assert!(is_corrupt(parse_descriptor("RW -1 SPARSE \"a\"\n")));
        assert!(is_corrupt(parse_descriptor(
            "RW 99999999999999999999999 SPARSE \"a\"\n"
        )));
        assert!(is_corrupt(parse_descriptor("RW 10 RARO \"a\"\n")));
        assert!(is_corrupt(parse_descriptor("RW 10 SPARSE\n")));
        assert!(is_corrupt(parse_descriptor("RW 10 ZERO \"a\"\n")));
        assert!(is_corrupt(parse_descriptor("RW 10 FLAT \"a\" 1 2\n")));
        assert!(is_corrupt(parse_descriptor("createType=a\ncreateType=b\n")));
        assert!(is_corrupt(parse_descriptor("=x\n")));
        // UTF-8 no ASCII: se acepta como texto.
        let d =
            parse_descriptor("createType=\"monolithicSparse\"\nRW 8 SPARSE \"дискó 💾.vmdk\"\n")
                .unwrap();
        assert_eq!(d.extents[0].file_name.as_deref(), Some("дискó 💾.vmdk"));
        // Bytes no UTF-8 dentro de la imagen: conversión tolerante, sin pánico.
        let b = Builder::new(64);
        let mut raw = b.build();
        let pos = raw.windows(6).position(|w| w == b"prueba").unwrap();
        raw[pos + 2] = 0xff;
        raw[pos + 3] = 0xfe;
        let img = open_bytes(&raw).unwrap();
        assert!(img.descriptor().extents[0]
            .file_name
            .as_deref()
            .unwrap()
            .contains('\u{fffd}'));
        // Línea enorme dentro de la imagen: descriptor de 10 sectores reubicado al final.
        let mut raw = Builder::new(64).build();
        let sector = (raw.len() / 512) as u64;
        raw[28..36].copy_from_slice(&sector.to_le_bytes());
        raw[36..44].copy_from_slice(&10u64.to_le_bytes());
        let mut block =
            format!("{}ddb.x = \"{}\"\n", descriptor(64), "x".repeat(4500)).into_bytes();
        block.resize(5120, 0);
        raw.extend_from_slice(&block);
        assert!(is_corrupt(open_bytes(&raw)));
        // El mismo descriptor sin la línea enorme se acepta en esa posición.
        let mut raw2 = Builder::new(64).build();
        raw2[28..36].copy_from_slice(&sector.to_le_bytes());
        raw2[36..44].copy_from_slice(&10u64.to_le_bytes());
        let mut block = descriptor(64).into_bytes();
        block.resize(5120, 0);
        raw2.extend_from_slice(&block);
        assert!(open_bytes(&raw2).is_ok());
    }

    #[test]
    fn error_references_are_sanitized_and_bounded() {
        let mut b = Builder::new(64);
        b.descriptor = format!(
            "createType=\"monolithicSparse\"\nRW 64 SPARSE \"x\"\nparentFileNameHint=\"a\t{}\"\n",
            "b".repeat(1000)
        );
        let r = external_ref(b.open());
        assert!(!r.chars().any(|c| c.is_control()));
        assert!(r.chars().count() <= MAX_REFERENCE_CHARS + 2);
    }

    #[test]
    fn descriptor_semantics() {
        // Padre por parentCID.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("parentCID=ffffffff", "parentCID=a1b2c3d4");
        assert!(external_ref(b.open()).contains("a1b2c3d4"));
        // streamOptimized u otros tipos declarados en el descriptor.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("monolithicSparse", "streamOptimized");
        assert!(is_unsupported(b.open()));
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("monolithicSparse", "vmfsSparse");
        assert!(is_unsupported(b.open()));
        // Sin createType / sin extensiones.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("createType=\"monolithicSparse\"", "");
        assert!(is_corrupt(b.open()));
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("RW 64 SPARSE \"prueba.vmdk\"", "");
        assert!(is_corrupt(b.open()));
        // Tamaño de extensión distinto de la capacidad.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(65);
        assert!(is_corrupt(b.open()));
        // Extensión adicional → otro fichero.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64) + "RW 10 SPARSE \"otro-s002.vmdk\"\n";
        assert_eq!(external_ref(b.open()), "otro-s002.vmdk");
        // Extensión FLAT → otro fichero.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("SPARSE \"prueba.vmdk\"", "FLAT \"plano.vmdk\" 0");
        assert_eq!(external_ref(b.open()), "plano.vmdk");
        // Renombrar el fichero no importa: la extensión única es el propio fichero.
        let mut b = Builder::new(64);
        b.descriptor = descriptor(64).replace("prueba.vmdk", "otro nombre.vmdk");
        assert!(b.open().is_ok());
    }

    // ---- cabecera ----

    #[test]
    fn rejects_bad_headers() {
        let good = Builder::new(64).build();
        assert!(open_bytes(&good).is_ok());

        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(is_corrupt(open_bytes(&bad)));
        assert!(is_corrupt(open_bytes(&[])));
        assert!(is_corrupt(open_bytes(b"KDMV")));
        assert!(is_corrupt(open_bytes(&good[..300])));
        let mut cowd = good.clone();
        cowd[..4].copy_from_slice(b"COWD");
        assert!(is_unsupported(open_bytes(&cowd)));

        for v in [0, 4, 0xffff_ffff] {
            let mut b = Builder::new(64);
            b.version = v;
            assert!(is_unsupported(b.open()), "versión {v}");
        }
        for v in [2, 3] {
            let mut b = Builder::new(64);
            b.version = v;
            assert!(b.open().is_ok(), "versión {v}");
        }
        let mut b = Builder::new(64);
        b.flags |= 1 << 5;
        assert!(is_unsupported(b.open()));
        for g in [0, 1, 4, 7, 12, 100, 1 << 21, u64::MAX] {
            let mut b = Builder::new(64);
            b.grain = g;
            assert!(is_corrupt(b.open()), "grano {g}");
        }
        for n in [0, 128, 511, 513, 1024] {
            let mut b = Builder::new(64);
            b.gtes = n;
            assert!(is_corrupt(b.open()), "numGTEsPerGT {n}");
        }
    }

    #[test]
    fn newline_test_characters() {
        for nl in [*b"\r\n \r", *b"\n \n\n", *b"\n\0\r\n", [0; 4]] {
            let mut b = Builder::new(64);
            b.newline = nl;
            assert!(is_corrupt(b.open()));
            // Sin el indicador de prueba, los caracteres no se comprueban.
            b.flags &= !FLAG_NEWLINE_TEST;
            assert!(b.open().is_ok());
        }
    }

    #[test]
    fn stream_optimized_is_unsupported() {
        for flags in [
            FLAG_COMPRESSED,
            FLAG_MARKERS,
            FLAG_COMPRESSED | FLAG_MARKERS,
        ] {
            let mut b = Builder::new(64);
            b.flags |= flags;
            b.version = 3;
            assert!(is_unsupported(b.open()));
        }
        let mut b = Builder::new(64);
        b.gd_offset = Some(GD_AT_END);
        assert!(is_unsupported(b.open()));
        let mut raw = Builder::new(64).build();
        raw[77] = 1; // compressAlgorithm = deflate
        assert!(is_unsupported(open_bytes(&raw)));
    }

    #[test]
    fn text_descriptor_is_not_followed() {
        let t = "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\n\
                 createType=\"monolithicFlat\"\nRW 64 FLAT \"..\\..\\Windows\\win.ini\" 0\n";
        match open_bytes(t.as_bytes()) {
            Err(IoError::ExternalReference { reference, .. }) => {
                assert!(reference.contains(r"..\..\Windows\win.ini"));
            }
            other => panic!("{other:?}"),
        }
        // Basura de texto que no es descriptor.
        assert!(is_corrupt(open_bytes(b"hola, no soy un VMDK")));
        // Descriptor separado con contenido corrupto: igualmente no soportado, sin pánico.
        let mut t = b"\xEF\xBB\xBF# Disk DescriptorFile\n".to_vec();
        t.extend(std::iter::repeat_n(0xffu8, 5000));
        assert!(is_unsupported(open_bytes(&t)));
    }

    #[test]
    fn descriptor_location_is_validated() {
        let mut raw = Builder::new(64).build();
        raw[28..36].copy_from_slice(&0u64.to_le_bytes()); // sin descriptor incrustado
        assert!(is_unsupported(open_bytes(&raw)));
        let mut raw = Builder::new(64).build();
        raw[28..36].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(is_corrupt(open_bytes(&raw)));
        let mut raw = Builder::new(64).build();
        raw[36..44].copy_from_slice(&0u64.to_le_bytes());
        assert!(is_corrupt(open_bytes(&raw)));
        let mut raw = Builder::new(64).build();
        raw[36..44].copy_from_slice(&(u64::MAX / 256).to_le_bytes());
        assert!(is_corrupt(open_bytes(&raw)));
        let mut raw = Builder::new(64).build();
        raw[28..36].copy_from_slice(&1_000_000u64.to_le_bytes());
        assert!(is_corrupt(open_bytes(&raw)));
    }

    // ---- GD / GT ----

    #[test]
    fn gd_and_gt_outside_file() {
        let mut b = Builder::new(64);
        b.gd_offset = Some(1 << 40);
        assert!(is_corrupt(b.open()));
        let mut b = Builder::new(64);
        b.gd_offset = Some(u64::MAX - 1);
        assert!(is_corrupt(b.open()));
        let mut b = Builder::new(64);
        b.gd_offset = Some(0);
        assert!(is_corrupt(b.open()));
        let mut b = Builder::new(64);
        b.raw_gde = vec![(0, u32::MAX)];
        assert!(is_corrupt(b.open()));
        // Grano fuera del fichero: todas las GT se validan al abrir, así que el modo estricto lo
        // rechaza en la apertura (antes, al leerlo), sin pánico.
        let mut b = Builder::new(64);
        b.raw_gte = vec![(1, u32::MAX)];
        assert!(is_corrupt(b.open()));
        // Entrada 1 sin el indicador de granos a cero.
        let mut b = Builder::new(64);
        b.flags &= !FLAG_ZEROED_GTE;
        b.raw_gte = vec![(0, GTE_ZEROED)];
        assert!(is_corrupt(b.open()));
        // Fichero truncado: el grano queda fuera.
        let mut b = Builder::new(64);
        b.grains = vec![(0, 0xaa)];
        let full = b.build().len();
        b.truncate_to = Some(full - 512);
        assert!(is_corrupt(b.open()));
    }

    #[test]
    fn huge_capacity_without_huge_memory() {
        // GD gigantesco: se rechaza antes de reservar nada.
        for cap in [u64::MAX / 512, 1u64 << 50] {
            let mut b = Builder::new(64);
            b.capacity = cap;
            b.descriptor = descriptor(cap);
            assert!(is_corrupt(b.open()), "capacidad {cap}");
        }
        // Más allá de lo representable en bytes.
        let mut b = Builder::new(64);
        b.capacity = u64::MAX / 512 + 1;
        b.descriptor = descriptor(b.capacity);
        assert!(is_corrupt(b.open()));
        // 64 TiB con granos de 512 MiB: 2^17 granos → 256 entradas de GD; se abre y lee ceros.
        let cap = 1u64 << 37;
        let mut b = Builder::new(64);
        b.capacity = cap;
        b.grain = MAX_GRAIN_SECTORS;
        b.descriptor = descriptor(cap);
        let mut raw = b.build();
        // El diseño de prueba reserva un único GT; las demás entradas de GD quedan a 0 (ausentes).
        let gdo = 5 * 512;
        raw.resize(raw.len().max(gdo + 256 * 4 + 2048), 0);
        for i in 1..256 {
            raw[gdo + i * 4..gdo + i * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        }
        let img = open_bytes(&raw).unwrap();
        assert_eq!(img.size(), cap * 512);
        let mut buf = vec![0xffu8; 4096];
        img.read_at(img.size() - 4096, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 0));
        let runs = img.allocation(0, img.sector_count()).unwrap();
        assert_eq!(runs, vec![(0, cap, Allocation::Sparse)]);
    }

    // ---- lectura y asignación ----

    #[test]
    fn reads_across_allocated_unallocated_and_zeroed_grains() {
        // 601 granos (2 GT) y capacidad no múltiplo del grano.
        let cap = 600 * GRAIN + 5;
        let mut b = Builder::new(cap);
        b.grains = vec![
            (0, 0x11),
            (1, 0x22),
            (3, 0),
            (4, 0x44),
            (513, 0x55),
            (600, 0x66),
        ];
        let img = b.open().unwrap();
        assert_eq!(img.size(), cap * 512);
        assert_eq!(img.sector_size(), 512);
        assert_eq!(img.grain_size_bytes(), GRAIN * 512);
        assert!(img.describe().contains("monolithicSparse"));
        assert!(img.describe().contains("8 KiB"));

        let gb = (GRAIN * 512) as usize;
        let expected = |off: usize| -> u8 {
            match off / gb {
                0 => 0x11,
                1 => 0x22,
                4 => 0x44,
                513 => 0x55,
                600 => 0x66,
                _ => 0,
            }
        };
        let total = img.size() as usize;
        let mut all = vec![0xccu8; total];
        img.read_at(0, &mut all).unwrap();
        for (i, &x) in all.iter().enumerate() {
            assert_eq!(x, expected(i), "byte {i}");
        }
        // Lecturas desalineadas que cruzan granos.
        for &(off, len) in &[
            (gb - 3, 7),
            (gb * 2 - 1, gb + 2),
            (gb * 512 - 10, 30),
            (total - 5, 5),
        ] {
            let mut buf = vec![0xccu8; len];
            img.read_at(off as u64, &mut buf).unwrap();
            assert_eq!(&buf[..], &all[off..off + len]);
        }
        // Límites.
        let mut one = [0u8; 1];
        assert!(matches!(
            img.read_at(img.size(), &mut one),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            img.read_at(u64::MAX, &mut one),
            Err(IoError::OutOfBounds { .. })
        ));
        img.read_at(img.size(), &mut []).unwrap();

        // Asignación.
        let runs = img.allocation(0, cap).unwrap();
        assert_eq!(
            runs,
            vec![
                (0, 2 * GRAIN, Allocation::Data),
                (2 * GRAIN, 2 * GRAIN, Allocation::Sparse),
                (4 * GRAIN, GRAIN, Allocation::Data),
                (5 * GRAIN, 508 * GRAIN, Allocation::Sparse),
                (513 * GRAIN, GRAIN, Allocation::Data),
                (514 * GRAIN, 86 * GRAIN, Allocation::Sparse),
                (600 * GRAIN, 5, Allocation::Data),
            ]
        );
        assert_eq!(
            img.allocation(GRAIN + 3, GRAIN).unwrap(),
            vec![
                (GRAIN + 3, GRAIN - 3, Allocation::Data),
                (2 * GRAIN, 3, Allocation::Sparse)
            ]
        );
        assert!(matches!(
            img.allocation(0, 0),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            img.allocation(cap, 1),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            img.allocation(u64::MAX, 2),
            Err(IoError::InvalidRange { .. })
        ));
    }

    #[test]
    fn missing_gt_is_sparse() {
        let cap = 1100 * GRAIN;
        let mut b = Builder::new(cap);
        b.grains = vec![(1050, 0x77)];
        b.raw_gde = vec![(0, 0)];
        let img = b.open().unwrap();
        assert_eq!(
            img.allocation(0, cap).unwrap(),
            vec![
                (0, 1050 * GRAIN, Allocation::Sparse),
                (1050 * GRAIN, GRAIN, Allocation::Data),
                (1051 * GRAIN, 49 * GRAIN, Allocation::Sparse),
            ]
        );
        let mut buf = vec![0xffu8; 1024];
        img.read_at(0, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 0));
        img.read_at(1050 * GRAIN * 512, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 0x77));
    }

    // ---- modo tolerante (ADR-020) ----

    fn open_mode(bytes: &[u8], tolerant: bool) -> (tempfile::NamedTempFile, Result<VmdkImage>) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        let r = VmdkImage::open_with(File::open(tmp.path()).unwrap(), tolerant);
        (tmp, r)
    }

    /// Imagen de referencia: 601 granos (2 GT), capacidad no múltiplo del grano.
    fn tolerant_sample() -> Builder {
        let mut b = Builder::new(600 * GRAIN + 5);
        b.grains = vec![
            (0, 0x11),
            (1, 0x22),
            (3, 0),
            (4, 0x44),
            (513, 0x55),
            (600, 0x66),
        ];
        b
    }

    /// Entrada `i` del GD (en el sector 5 de las imágenes de prueba) y entrada de GT del grano `g`.
    fn gde(raw: &[u8], i: usize) -> u32 {
        le_u32(raw, 5 * 512 + 4 * i)
    }
    fn gte(raw: &[u8], g: u64) -> u32 {
        let gt = gde(raw, (g / GTES_PER_GT) as usize) as usize;
        le_u32(raw, gt * 512 + (g % GTES_PER_GT) as usize * 4)
    }

    #[test]
    fn tolerant_mode_discards_only_invalid_or_ambiguous_grains() {
        let base = tolerant_sample();
        let raw = base.build();
        let (_t, reference) = open_mode(&raw, false);
        let reference = reference.unwrap();
        let mut expect = vec![0u8; reference.size() as usize];
        reference.read_at(0, &mut expect).unwrap();
        let cap = 600 * GRAIN + 5;
        let g1 = gte(&raw, 1);
        let gt0 = gde(&raw, 0);
        let gt1 = gde(&raw, 1);
        // Granos de la GT 1 (512 a 600): hasta el final del disco.
        let second_gt = (512 * GRAIN, cap - 512 * GRAIN);
        type Edit = Box<dyn Fn(&mut Builder)>;
        // (caso, cambio, tramos de sectores dañados, granos descartados). 16 sectores por grano.
        type Case = (&'static str, Edit, Vec<(u64, u64)>, u64);
        let cases: Vec<Case> = vec![
            (
                "grano fuera del fichero",
                Box::new(|b| b.raw_gte = vec![(1, u32::MAX)]),
                vec![(GRAIN, GRAIN)],
                1,
            ),
            (
                // Sus 16 sectores cubren también las dos GT: se descarta todo el disco.
                "grano sobre el descriptor",
                Box::new(|b| b.raw_gte = vec![(4, 2)]),
                vec![(0, cap)],
                601,
            ),
            (
                // Sus 16 sectores cubren también las dos GT: se descarta todo el disco.
                "grano sobre el directorio",
                Box::new(|b| b.raw_gte = vec![(4, 5)]),
                vec![(0, cap)],
                601,
            ),
            (
                "grano duplicado",
                Box::new(move |b| b.raw_gte = vec![(4, g1)]),
                vec![(GRAIN, GRAIN), (4 * GRAIN, GRAIN)],
                2,
            ),
            (
                "grano solapado con otro",
                Box::new(move |b| b.raw_gte = vec![(4, g1 + 8)]),
                vec![(GRAIN, GRAIN), (4 * GRAIN, GRAIN)],
                2,
            ),
            (
                "entrada 1 sin el indicador de granos a cero",
                Box::new(|b| b.flags &= !FLAG_ZEROED_GTE),
                vec![(3 * GRAIN, GRAIN)],
                1,
            ),
            (
                "GT fuera del fichero",
                Box::new(|b| b.raw_gde = vec![(1, u32::MAX)]),
                vec![second_gt],
                89,
            ),
            (
                "GT sobre el descriptor",
                Box::new(|b| b.raw_gde = vec![(1, 1)]),
                vec![second_gt],
                89,
            ),
            (
                "GT duplicada",
                Box::new(move |b| b.raw_gde = vec![(1, gt0)]),
                vec![(0, cap)],
                601,
            ),
            (
                // El grano 4 cubre la GT 1 y el principio del grano 0: se descartan los tres.
                "grano sobre una GT",
                Box::new(move |b| b.raw_gte = vec![(4, gt1)]),
                vec![(0, GRAIN), (4 * GRAIN, GRAIN), second_gt],
                91,
            ),
        ];
        for (what, edit, bad, n) in cases {
            let mut b = tolerant_sample();
            edit(&mut b);
            let bytes = b.build();
            assert!(is_corrupt(open_mode(&bytes, false).1), "{what}");
            let (t, img) = open_mode(&bytes, true);
            let before = crate::tolerant::check::Snapshot::of(t.path());
            let img = img.unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(img.discarded_blocks(), n, "{what}");
            assert!(img.is_tolerant());
            let d = img.describe();
            assert!(
                d.ends_with(&crate::tolerant::describe_suffix(n, "grano", "granos")),
                "{what}: {d}"
            );
            crate::tolerant::check::tolerant_read_matches(&img, &expect, &bad, what);
            drop(img);
            assert_eq!(
                crate::tolerant::check::Snapshot::of(t.path()),
                before,
                "{what}"
            );
        }
        // Sin daños: idéntico al modo estricto.
        let (_t, img) = open_mode(&raw, true);
        let img = img.unwrap();
        assert_eq!(img.discarded_blocks(), 0);
        assert!(img
            .describe()
            .ends_with("; modo tolerante: 0 granos descartados (no fiables)"));
        assert!(!reference.describe().contains("tolerante"));
        crate::tolerant::check::tolerant_read_matches(&img, &expect, &[], "sin daños");
        assert_eq!(
            img.allocation(0, cap).unwrap(),
            reference.allocation(0, cap).unwrap()
        );
    }

    #[test]
    fn critical_structures_are_corrupt_in_both_modes() {
        let good = tolerant_sample().build();
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut raw = good.clone();
        raw[0] = b'X';
        cases.push(("firma", raw));
        let mut raw = good.clone();
        raw[28..36].copy_from_slice(&1_000_000u64.to_le_bytes());
        cases.push(("descriptor fuera del fichero", raw));
        let mut raw = good.clone();
        raw[56..64].copy_from_slice(&(1u64 << 40).to_le_bytes());
        cases.push(("GD fuera del fichero", raw));
        let mut raw = good.clone();
        raw[56..64].copy_from_slice(&3u64.to_le_bytes());
        cases.push(("GD sobre el descriptor", raw));
        let mut raw = good.clone();
        raw[20..28].copy_from_slice(&12u64.to_le_bytes());
        cases.push(("tamaño de grano", raw));
        let mut b = tolerant_sample();
        b.descriptor = descriptor(600 * GRAIN + 4);
        cases.push(("capacidad del descriptor", b.build()));
        for (what, bytes) in cases {
            for tolerant in [false, true] {
                let (t, r) = open_mode(&bytes, tolerant);
                assert!(is_corrupt(r), "{what} (tolerante: {tolerant})");
                assert!(std::fs::read(t.path()).unwrap() == bytes);
            }
        }
    }

    #[test]
    fn zero_capacity() {
        let mut b = Builder::new(0);
        b.descriptor = descriptor(0);
        let img = b.open().unwrap();
        assert_eq!(img.size(), 0);
        assert!(img.allocation(0, 1).is_err());
    }
}
