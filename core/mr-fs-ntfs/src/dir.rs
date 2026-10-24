//! Directorios NTFS: listado de un índice `$I30` contrastado con la MFT, árbol completo con
//! límites, resolución de rutas, ruta de un registro y huérfanos.
//!
//! **Contraste con la MFT.** Cada entrada del índice es la afirmación «el registro N, con la
//! secuencia S, se llama X en este directorio». Solo es una entrada viva ([`EntryState::Live`]) si
//! el registro N existe, es interpretable, es un registro base, está en uso y su secuencia actual es
//! S. Si no, la entrada se informa con su estado (registro libre, reutilizado, dañado o ilegible,
//! referencia no válida) y nunca se presenta como un fichero vivo; #23 las expondrá como entradas
//! residuales. Además se comprueba que el registro tenga ese nombre en este directorio
//! (`entry_name_not_in_record`) y que la clave diga estar en este directorio
//! (`entry_parent_mismatch`).
//!
//! **Nombres.** Un fichero con nombre largo y nombre corto 8.3 tiene dos entradas (espacios Win32
//! y DOS) o una sola (Win32 y DOS): las entradas DOS de un registro que tiene otra entrada no DOS en
//! el mismo directorio no se listan (el nombre corto queda en [`DirEntry::dos_name`]). Los enlaces
//! duros (varios `$FILE_NAME` en directorios distintos, o en el mismo) aparecen en cada directorio.
//! Los nombres se decodifican de UTF-16 con sustitución (U+FFFD) de las unidades sueltas
//! (sustitutos sin pareja), se marca `name_lossy` y las comparaciones usan las unidades originales.
//! La entrada «.» de la raíz (apunta a sí misma) no se lista.
//!
//! **Rutas.** `/` separa componentes (también se admite `\`); la comparación no distingue
//! mayúsculas según `$UpCase` (ver [`crate::upcase`]) y, entre varios nombres iguales sin
//! distinguir mayúsculas, se prefiere el que coincide exactamente. [`NtfsVolume::path_of`] sube por
//! las referencias al padre de `$FILE_NAME` con límite de profundidad y detección de ciclos; un
//! padre ausente, ilegible, libre, reutilizado o que no es un directorio deja el fichero huérfano.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;

use crate::attr::types;
use crate::file::{AttrValue, NtfsFile};
use crate::index::{has_i30, Find, IndexAnomaly, IndexEntry, IndexIssue, IndexReader};
use crate::mft::{RecordOutcome, RecordState};
use crate::record::FileReference;
use crate::upcase::Upcase;
use crate::values::{FileName, Namespace, Times};
use crate::{NtfsError, NtfsVolume};

/// Registro del directorio raíz.
pub const ROOT_RECORD: u64 = 5;
/// Registro de `$Extend`.
pub const EXTEND_RECORD: u64 = 11;
/// Primer registro de usuario (0 a 15 son ficheros del sistema o reservados).
pub const FIRST_USER_RECORD: u64 = 16;
/// Profundidad máxima de una ruta.
pub const MAX_PATH_DEPTH: usize = 1024;
/// Atributo de `$FILE_NAME` que marca un directorio (tiene índice de nombres).
pub const FILE_NAME_INDEX_PRESENT: u32 = 0x1000_0000;

/// Estado de una entrada de directorio tras contrastarla con la MFT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    /// El registro está en uso con la secuencia de la referencia.
    Live,
    /// El registro ya no está en uso (borrado o libre): entrada residual.
    NotInUse { found_sequence: u16 },
    /// El registro está en uso con otra secuencia: se reutilizó para otro fichero.
    Reused { found_sequence: u16 },
    /// El registro no es interpretable (dañado, ilegible, vacío...).
    RecordUnusable {
        state: RecordState,
        detail: Option<String>,
    },
    /// Referencia fuera de la MFT o a un registro de extensión.
    InvalidReference { detail: String },
}

impl EntryState {
    pub fn is_live(&self) -> bool {
        matches!(self, EntryState::Live)
    }

    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            EntryState::Live => "live",
            EntryState::NotInUse { .. } => "stale_not_in_use",
            EntryState::Reused { .. } => "stale_reused",
            EntryState::RecordUnusable { .. } => "record_unusable",
            EntryState::InvalidReference { .. } => "invalid_reference",
        }
    }
}

impl fmt::Display for EntryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryState::Live => write!(f, "vivo"),
            EntryState::NotInUse { found_sequence } => write!(
                f,
                "entrada residual: el registro no está en uso (secuencia actual {found_sequence})"
            ),
            EntryState::Reused { found_sequence } => write!(
                f,
                "entrada residual: el registro se reutilizó (secuencia actual {found_sequence})"
            ),
            EntryState::RecordUnusable { state, detail } => {
                write!(f, "registro {}", state.label())?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            EntryState::InvalidReference { detail } => write!(f, "referencia no válida: {detail}"),
        }
    }
}

/// Entrada de un directorio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Referencia del índice (número y secuencia).
    pub reference: FileReference,
    /// Directorio que la contiene.
    pub parent: FileReference,
    /// Nombre decodificado (U+FFFD en las unidades sueltas).
    pub name: String,
    /// Nombre en unidades UTF-16 tal como está en disco.
    pub name_units: Vec<u16>,
    /// El nombre tenía unidades UTF-16 no válidas.
    pub name_lossy: bool,
    pub namespace: Namespace,
    /// Nombre corto 8.3 del mismo registro en este directorio (si es otra entrada).
    pub dos_name: Option<String>,
    pub is_directory: bool,
    /// Tamaño de `$DATA` principal (0 en directorios).
    pub size: u64,
    pub allocated_size: u64,
    /// Fechas de `$STANDARD_INFORMATION` (o de la clave si el registro no es utilizable).
    pub times: Times,
    /// `true` si las fechas y los atributos salen de la clave del índice.
    pub times_from_index: bool,
    /// Atributos DOS.
    pub file_attributes: u32,
    /// Enlaces según la cabecera del registro (0 si no se pudo leer).
    pub link_count: u16,
    /// Secuencia actual del registro (si se pudo leer).
    pub record_sequence: Option<u16>,
    /// Fichero del sistema (registros 0-15 y contenido de `$Extend`).
    pub system: bool,
    pub state: EntryState,
}

impl DirEntry {
    pub fn record(&self) -> u64 {
        self.reference.record
    }
}

/// Listado de un directorio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    pub directory: FileReference,
    /// Entradas en orden de colación (vivas y no vivas: ver [`DirEntry::state`]).
    pub entries: Vec<DirEntry>,
    pub anomalies: Vec<IndexAnomaly>,
}

impl DirListing {
    /// Entradas vivas.
    pub fn live(&self) -> impl Iterator<Item = &DirEntry> {
        self.entries.iter().filter(|e| e.state.is_live())
    }

    /// `true` si algún nodo no se pudo leer por sectores no fiables.
    pub fn has_unreadable(&self) -> bool {
        self.anomalies.iter().any(|a| a.issue.is_unreadable())
            || self
                .entries
                .iter()
                .any(|e| matches!(&e.state, EntryState::RecordUnusable { state, .. } if *state == RecordState::Unreadable))
    }
}

/// Por qué un registro no cuelga de un directorio válido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanReason {
    /// Sin `$FILE_NAME` (no tiene nombre ni padre).
    NoName,
    ParentOutOfRange,
    ParentUnusable {
        state: RecordState,
    },
    ParentNotInUse,
    ParentReused {
        found_sequence: u16,
    },
    ParentNotDirectory,
    /// El padre es un directorio alcanzable pero su índice no lista el fichero.
    NotIndexed,
    /// El padre tampoco es alcanzable desde la raíz.
    ParentOrphan,
    /// Las referencias al padre forman un ciclo.
    Loop,
    /// Más de [`MAX_PATH_DEPTH`] niveles.
    TooDeep,
}

impl OrphanReason {
    pub fn code(&self) -> &'static str {
        match self {
            OrphanReason::NoName => "orphan_no_name",
            OrphanReason::ParentOutOfRange => "orphan_parent_out_of_range",
            OrphanReason::ParentUnusable { .. } => "orphan_parent_unusable",
            OrphanReason::ParentNotInUse => "orphan_parent_not_in_use",
            OrphanReason::ParentReused { .. } => "orphan_parent_reused",
            OrphanReason::ParentNotDirectory => "orphan_parent_not_directory",
            OrphanReason::NotIndexed => "orphan_not_indexed",
            OrphanReason::ParentOrphan => "orphan_parent_orphan",
            OrphanReason::Loop => "orphan_loop",
            OrphanReason::TooDeep => "orphan_too_deep",
        }
    }
}

impl fmt::Display for OrphanReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrphanReason::NoName => write!(f, "no tiene $FILE_NAME"),
            OrphanReason::ParentOutOfRange => write!(f, "el padre está fuera de la MFT"),
            OrphanReason::ParentUnusable { state } => {
                write!(
                    f,
                    "el registro del padre no es utilizable ({})",
                    state.label()
                )
            }
            OrphanReason::ParentNotInUse => write!(f, "el padre no está en uso"),
            OrphanReason::ParentReused { found_sequence } => write!(
                f,
                "el registro del padre se reutilizó (secuencia actual {found_sequence})"
            ),
            OrphanReason::ParentNotDirectory => write!(f, "el padre no es un directorio"),
            OrphanReason::NotIndexed => {
                write!(f, "el índice del padre no tiene una entrada viva para él")
            }
            OrphanReason::ParentOrphan => write!(f, "el padre tampoco cuelga de la raíz"),
            OrphanReason::Loop => write!(f, "las referencias al padre forman un ciclo"),
            OrphanReason::TooDeep => {
                write!(f, "más de {MAX_PATH_DEPTH} niveles hasta la raíz")
            }
        }
    }
}

/// Registro en uso que no se alcanza desde la raíz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub record: u64,
    pub sequence: u16,
    pub name: String,
    pub parent: FileReference,
    pub is_directory: bool,
    pub reason: OrphanReason,
}

/// Opciones del árbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeOptions {
    /// Profundidad máxima (directorios bajo la raíz).
    pub max_depth: usize,
    /// Nodos máximos del árbol.
    pub max_nodes: usize,
    /// Buscar registros en uso no alcanzados (recorre toda la MFT).
    pub orphans: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        TreeOptions {
            max_depth: 256,
            max_nodes: 10_000_000,
            orphans: true,
        }
    }
}

/// Cancelación y progreso del árbol.
pub trait TreeObserver {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _done: u64, _total: u64) {}
}

/// Observador que no cancela ni informa.
pub struct NoTreeObserver;

impl TreeObserver for NoTreeObserver {}

/// Cómo terminó el árbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeStatus {
    Complete,
    /// Se alcanzó `max_nodes`: árbol parcial.
    Limit,
    Cancelled,
}

impl TreeStatus {
    pub fn code(self) -> &'static str {
        match self {
            TreeStatus::Complete => "complete",
            TreeStatus::Limit => "limit",
            TreeStatus::Cancelled => "cancelled",
        }
    }
}

/// Nodo del árbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    /// Ruta desde la raíz (`/` para la raíz).
    pub path: String,
    /// Índice del nodo padre (`None` en la raíz).
    pub parent: Option<usize>,
    pub depth: u32,
    pub entry: DirEntry,
}

/// Problema del árbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeIssue {
    /// Anomalía del índice de un directorio.
    Index(IndexAnomaly),
    /// El directorio no se pudo listar.
    Unlistable { code: &'static str, detail: String },
    /// Directorio alcanzado por segunda vez (no se vuelve a recorrer).
    Loop { record: u64 },
    /// Directorio más profundo que `max_depth` (no se recorre).
    Depth { record: u64 },
}

impl TreeIssue {
    pub fn code(&self) -> &'static str {
        match self {
            TreeIssue::Index(a) => a.issue.code(),
            TreeIssue::Unlistable { code, .. } => code,
            TreeIssue::Loop { .. } => "tree_loop",
            TreeIssue::Depth { .. } => "tree_depth",
        }
    }
}

impl fmt::Display for TreeIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeIssue::Index(a) => a.fmt(f),
            TreeIssue::Unlistable { detail, .. } => {
                write!(f, "el directorio no se puede listar: {detail}")
            }
            TreeIssue::Loop { record } => write!(
                f,
                "el directorio {record} ya se había alcanzado por otra ruta: ciclo, no se recorre otra vez"
            ),
            TreeIssue::Depth { record } => write!(
                f,
                "el directorio {record} supera la profundidad máxima: no se recorre"
            ),
        }
    }
}

/// Problema del árbol localizado en un directorio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeAnomaly {
    pub directory: u64,
    pub path: String,
    pub issue: TreeIssue,
}

/// Árbol de directorios de un volumen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// Nodos en orden de recorrido en anchura; el 0 es la raíz.
    pub nodes: Vec<TreeNode>,
    pub anomalies: Vec<TreeAnomaly>,
    pub orphans: Vec<Orphan>,
    pub status: TreeStatus,
    /// Directorios listados.
    pub directories: u64,
}

/// Nombre preferido de un registro: el primer `$FILE_NAME` no DOS (o el DOS si no hay otro).
pub fn preferred_name(names: &[FileName]) -> Option<&FileName> {
    names
        .iter()
        .find(|n| n.namespace != Namespace::Dos)
        .or_else(|| names.first())
}

fn units_of(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Tamaños de `$DATA` principal según el registro.
fn data_sizes(file: &NtfsFile) -> Option<(u64, u64)> {
    file.attributes
        .iter()
        .filter(|a| a.type_code == types::DATA && a.name.is_empty())
        .find_map(|a| match &a.value {
            AttrValue::Resident { value, .. } => {
                Some((value.len() as u64, value.len().next_multiple_of(8) as u64))
            }
            AttrValue::NonResident(n) if n.start_vcn == 0 => Some((n.data_size, n.allocated_size)),
            AttrValue::NonResident(_) => None,
        })
}

/// Validación de un registro como destino de una referencia.
pub(crate) enum Target {
    /// Registro base interpretable (en uso o no).
    File(Box<NtfsFile>),
    Unusable {
        state: RecordState,
        detail: Option<String>,
    },
    Invalid(String),
}

impl NtfsVolume {
    /// Lee el registro `n` como destino de una referencia.
    pub(crate) fn target(&self, n: u64) -> Result<Target, NtfsError> {
        if n >= self.record_count() {
            return Ok(Target::Invalid(format!(
                "el registro {n} está fuera de la MFT ({} registros)",
                self.record_count()
            )));
        }
        let o: RecordOutcome = self.read_record(n)?;
        let state = o.state();
        let detail = o.detail();
        let Some(rec) = o.into_record() else {
            return Ok(Target::Unusable { state, detail });
        };
        if !rec.header().is_base() {
            return Ok(Target::Invalid(format!(
                "el registro {n} es una extensión del registro {}",
                rec.header().base_record.record
            )));
        }
        Ok(Target::File(Box::new(self.file_from_record(rec)?)))
    }

    /// Construye la entrada de directorio de `reference` llamada `key` en el directorio `dir`.
    fn make_entry(
        &self,
        dir: FileReference,
        reference: FileReference,
        key: &FileName,
        units: &[u16],
        system: bool,
        issues: &mut Vec<IndexIssue>,
    ) -> Result<DirEntry, NtfsError> {
        let target = self.target(reference.record)?;
        let lossy = char::decode_utf16(units.iter().copied()).any(|r| r.is_err());
        let mut e = DirEntry {
            reference,
            parent: dir,
            name: key.name.clone(),
            name_units: units.to_vec(),
            name_lossy: lossy,
            namespace: key.namespace,
            dos_name: None,
            is_directory: key.file_attributes & FILE_NAME_INDEX_PRESENT != 0,
            size: key.data_size,
            allocated_size: key.allocated_size,
            times: key.times,
            times_from_index: true,
            file_attributes: key.file_attributes,
            link_count: 0,
            record_sequence: None,
            system: system || reference.record < FIRST_USER_RECORD,
            state: EntryState::Live,
        };
        match target {
            Target::Invalid(detail) => e.state = EntryState::InvalidReference { detail },
            Target::Unusable { state, detail } => {
                e.state = EntryState::RecordUnusable { state, detail }
            }
            Target::File(f) => {
                let h = &f.header;
                e.record_sequence = Some(h.sequence);
                e.link_count = h.link_count;
                e.is_directory = has_i30(&f) || h.is_directory();
                if let Some(si) = f.standard_information() {
                    e.times = si.times;
                    e.file_attributes = si.file_attributes;
                    e.times_from_index = false;
                }
                if e.is_directory {
                    e.size = 0;
                    e.allocated_size = 0;
                } else if let Some((size, alloc)) = data_sizes(&f) {
                    e.size = size;
                    e.allocated_size = alloc;
                }
                e.state = if !h.in_use() {
                    EntryState::NotInUse {
                        found_sequence: h.sequence,
                    }
                } else if h.sequence != reference.sequence {
                    EntryState::Reused {
                        found_sequence: h.sequence,
                    }
                } else {
                    EntryState::Live
                };
                if e.state.is_live()
                    && !f
                        .file_names()
                        .iter()
                        .any(|n| n.parent.record == dir.record && n.name == key.name)
                {
                    issues.push(IndexIssue::EntryNameNotInRecord {
                        record: reference.record,
                    });
                }
            }
        }
        Ok(e)
    }

    /// Lista el directorio del registro base `record`: entradas de su índice `$I30` contrastadas
    /// con la MFT, en orden de colación, y las anomalías del índice.
    pub fn list_dir(&self, record: u64) -> Result<DirListing, NtfsError> {
        let system =
            (record < FIRST_USER_RECORD && record != ROOT_RECORD) || record == EXTEND_RECORD;
        self.list_dir_as(record, system)
    }

    fn list_dir_as(&self, record: u64, system: bool) -> Result<DirListing, NtfsError> {
        let file = self.file(record)?;
        self.list_file(&file, system)
    }

    fn list_file(&self, file: &NtfsFile, system: bool) -> Result<DirListing, NtfsError> {
        let mut reader = IndexReader::open(self, file)?;
        let up = self.upcase();
        let raw = reader.walk(&up);
        let dir = file.reference();
        let mut anomalies = std::mem::take(&mut reader.anomalies);
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut seen: HashSet<(u64, u16, Vec<u16>)> = HashSet::new();
        let children_system = system || dir.record == EXTEND_RECORD;
        for (node, e) in raw {
            let Some(key) = &e.key else { continue };
            if e.reference.record == dir.record {
                if key.units != [u16::from(b'.')] {
                    anomalies.push(IndexAnomaly {
                        node,
                        issue: IndexIssue::EntrySelfReference,
                    });
                }
                continue;
            }
            if !seen.insert((e.reference.record, e.reference.sequence, key.units.clone())) {
                anomalies.push(IndexAnomaly {
                    node,
                    issue: IndexIssue::EntryDuplicate {
                        record: e.reference.record,
                    },
                });
                continue;
            }
            if key.file_name.parent.record != dir.record {
                anomalies.push(IndexAnomaly {
                    node,
                    issue: IndexIssue::EntryParentMismatch {
                        record: e.reference.record,
                        parent: key.file_name.parent,
                    },
                });
            }
            let mut issues = Vec::new();
            let entry = self.make_entry(
                dir,
                e.reference,
                &key.file_name,
                &key.units,
                children_system,
                &mut issues,
            )?;
            anomalies.extend(issues.into_iter().map(|issue| IndexAnomaly { node, issue }));
            entries.push(entry);
        }
        // Nombres DOS duplicados de un nombre largo del mismo registro.
        let mut long: HashMap<FileReference, usize> = HashMap::new();
        for e in &entries {
            if e.namespace != Namespace::Dos {
                *long.entry(e.reference).or_default() += 1;
            }
        }
        let mut dos: HashMap<FileReference, String> = HashMap::new();
        entries.retain(|e| {
            if e.namespace == Namespace::Dos && long.contains_key(&e.reference) {
                dos.insert(e.reference, e.name.clone());
                false
            } else {
                true
            }
        });
        for e in &mut entries {
            if let Some(d) = dos.get(&e.reference) {
                e.dos_name = Some(d.clone());
            }
        }
        entries.sort_by(|a, b| up.compare_stable(&a.name_units, &b.name_units));
        Ok(DirListing {
            directory: dir,
            entries,
            anomalies,
        })
    }

    /// Entrada del propio registro `record` con el nombre `name` dentro de `parent` (la raíz o el
    /// destino de una búsqueda).
    fn self_entry(
        &self,
        record: u64,
        name: &str,
        parent: FileReference,
    ) -> Result<DirEntry, NtfsError> {
        // La secuencia esperada es la actual del registro (si se puede leer).
        let sequence = match self.read_record(record)?.record() {
            Some(r) => r.header().sequence,
            None => parent.sequence,
        };
        let fname = FileName {
            parent,
            times: Times::default(),
            allocated_size: 0,
            data_size: 0,
            file_attributes: FILE_NAME_INDEX_PRESENT,
            reparse_or_ea: 0,
            namespace: Namespace::Win32AndDos,
            name: name.to_string(),
        };
        let mut issues = Vec::new();
        let mut e = self.make_entry(
            parent,
            FileReference { record, sequence },
            &fname,
            &units_of(name),
            false,
            &mut issues,
        )?;
        e.system = false;
        Ok(e)
    }

    /// Busca `name` (sin distinguir mayúsculas) entre las entradas vivas del directorio `dir`.
    /// Baja por el árbol B según la colación y, si el camino está dañado o el nombre solo coincide
    /// sin distinguir mayúsculas, recorre el índice entero.
    pub fn lookup(&self, dir: u64, name: &str) -> Result<Option<DirEntry>, NtfsError> {
        let file = self.file(dir)?;
        let reader = IndexReader::open(self, &file)?;
        let up = self.upcase();
        let units = units_of(name);
        let dref = file.reference();
        match reader.find(&units, &up) {
            Find::NotFound => return Ok(None),
            Find::Found(found) => {
                for IndexEntry { reference, key, .. } in found {
                    let Some(k) = key else { continue };
                    if reference.record == dir || k.file_name.namespace == Namespace::Dos {
                        continue;
                    }
                    let mut issues = Vec::new();
                    let e = self.make_entry(
                        dref,
                        reference,
                        &k.file_name,
                        &k.units,
                        false,
                        &mut issues,
                    )?;
                    if e.state.is_live() && e.name_units == units {
                        return Ok(Some(e));
                    }
                }
                // Solo coincidencias sin distinguir mayúsculas (puede haber una exacta en otro
                // nodo), nombres 8.3 o entradas no vivas: se recorre el índice entero.
            }
            Find::Unsure => {}
        }
        let listing = self.list_file(&file, false)?;
        Ok(pick(&listing, &units, &up))
    }

    /// Resuelve una ruta absoluta (`/a/b/c`) desde la raíz.
    pub fn resolve_path(&self, path: &str) -> Result<DirEntry, NtfsError> {
        let invalid = |reason: &str| NtfsError::InvalidPath {
            path: path.to_string(),
            reason: reason.to_string(),
        };
        let norm = path.replace('\\', "/");
        if !norm.starts_with('/') {
            return Err(invalid("debe empezar por /"));
        }
        let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() > MAX_PATH_DEPTH {
            return Err(invalid("demasiados niveles"));
        }
        if parts.iter().any(|p| *p == "." || *p == "..") {
            return Err(invalid("no se admiten los componentes . ni .."));
        }
        let root = FileReference {
            record: ROOT_RECORD,
            sequence: ROOT_RECORD as u16,
        };
        let mut cur = self.self_entry(ROOT_RECORD, "", root)?;
        for (i, part) in parts.iter().enumerate() {
            if !cur.is_directory {
                return Err(NtfsError::NotADirectory {
                    number: cur.record(),
                });
            }
            match self.lookup(cur.record(), part)? {
                Some(e) => cur = e,
                None => {
                    return Err(NtfsError::NotFound {
                        path: format!("/{}", parts[..=i].join("/")),
                        component: part.to_string(),
                    })
                }
            }
        }
        Ok(cur)
    }

    /// Problema del padre `parent` de un registro, o `None` si es un directorio en uso con esa
    /// secuencia.
    fn parent_problem(&self, parent: FileReference) -> Result<Option<OrphanReason>, NtfsError> {
        Ok(match self.target(parent.record)? {
            Target::Invalid(_) => Some(OrphanReason::ParentOutOfRange),
            Target::Unusable { state, .. } => Some(OrphanReason::ParentUnusable { state }),
            Target::File(f) => {
                if !f.in_use() {
                    Some(OrphanReason::ParentNotInUse)
                } else if f.header.sequence != parent.sequence {
                    Some(OrphanReason::ParentReused {
                        found_sequence: f.header.sequence,
                    })
                } else if !has_i30(&f) {
                    Some(OrphanReason::ParentNotDirectory)
                } else {
                    None
                }
            }
        })
    }

    /// Ruta de un registro base subiendo por las referencias al padre de su `$FILE_NAME`
    /// preferido (no DOS). Vale también para registros no en uso; los antecesores deben ser
    /// directorios en uso con la secuencia de la referencia. Error [`NtfsError::Orphan`] si no
    /// cuelga de la raíz.
    pub fn path_of(&self, record: u64) -> Result<String, NtfsError> {
        let mut names: Vec<String> = Vec::new();
        let mut cur = record;
        let mut visited: HashSet<u64> = HashSet::new();
        loop {
            if cur == ROOT_RECORD {
                break;
            }
            if !visited.insert(cur) {
                return Err(NtfsError::Orphan {
                    record,
                    parent: FileReference {
                        record: cur,
                        sequence: 0,
                    },
                    reason: OrphanReason::Loop,
                });
            }
            if names.len() >= MAX_PATH_DEPTH {
                return Err(NtfsError::Orphan {
                    record,
                    parent: FileReference {
                        record: cur,
                        sequence: 0,
                    },
                    reason: OrphanReason::TooDeep,
                });
            }
            let f = self.file(cur)?;
            let fnames = f.file_names();
            let Some(n) = preferred_name(&fnames) else {
                return Err(NtfsError::Orphan {
                    record,
                    parent: FileReference {
                        record: cur,
                        sequence: 0,
                    },
                    reason: OrphanReason::NoName,
                });
            };
            if let Some(reason) = self.parent_problem(n.parent)? {
                return Err(NtfsError::Orphan {
                    record,
                    parent: n.parent,
                    reason,
                });
            }
            names.push(n.name.clone());
            cur = n.parent.record;
        }
        names.reverse();
        Ok(format!("/{}", names.join("/")))
    }

    /// Árbol completo desde la raíz (en anchura), con límites, ciclos y, si se pide, los
    /// registros en uso que no se alcanzan (huérfanos). Cancelable: con la cancelación devuelve el
    /// árbol parcial con `TreeStatus::Cancelled`.
    pub fn tree(&self, opts: &TreeOptions, obs: &dyn TreeObserver) -> Result<Tree, NtfsError> {
        let records = self.record_count();
        let total = if opts.orphans { records * 2 } else { records };
        let root_ref = FileReference {
            record: ROOT_RECORD,
            sequence: ROOT_RECORD as u16,
        };
        let root = self.self_entry(ROOT_RECORD, "", root_ref)?;
        let mut tree = Tree {
            nodes: vec![TreeNode {
                path: "/".into(),
                parent: None,
                depth: 0,
                entry: root,
            }],
            anomalies: Vec::new(),
            orphans: Vec::new(),
            status: TreeStatus::Complete,
            directories: 0,
        };
        let mut reached: HashSet<u64> = HashSet::from([ROOT_RECORD]);
        let mut listed: HashSet<u64> = HashSet::from([ROOT_RECORD]);
        let mut queue: VecDeque<usize> = VecDeque::from([0]);
        obs.progress(0, total);
        'dirs: while let Some(i) = queue.pop_front() {
            if obs.cancelled() {
                tree.status = TreeStatus::Cancelled;
                break;
            }
            let (record, path, depth, system) = {
                let n = &tree.nodes[i];
                (n.entry.record(), n.path.clone(), n.depth, n.entry.system)
            };
            let listing = match self.list_dir_as(record, system && record != ROOT_RECORD) {
                Ok(l) => l,
                Err(NtfsError::Io(e)) => return Err(NtfsError::Io(e)),
                Err(e) => {
                    tree.anomalies.push(TreeAnomaly {
                        directory: record,
                        path: path.clone(),
                        issue: TreeIssue::Unlistable {
                            code: e.code(),
                            detail: e.to_string(),
                        },
                    });
                    continue;
                }
            };
            tree.directories += 1;
            for a in listing.anomalies {
                tree.anomalies.push(TreeAnomaly {
                    directory: record,
                    path: path.clone(),
                    issue: TreeIssue::Index(a),
                });
            }
            for e in listing.entries {
                if tree.nodes.len() >= opts.max_nodes {
                    tree.status = TreeStatus::Limit;
                    break 'dirs;
                }
                let child_path = if path == "/" {
                    format!("/{}", e.name)
                } else {
                    format!("{path}/{}", e.name)
                };
                let live = e.state.is_live();
                let is_dir = e.is_directory;
                let r = e.record();
                if live {
                    reached.insert(r);
                }
                tree.nodes.push(TreeNode {
                    path: child_path.clone(),
                    parent: Some(i),
                    depth: depth + 1,
                    entry: e,
                });
                let idx = tree.nodes.len() - 1;
                if live && is_dir {
                    if !listed.insert(r) {
                        tree.anomalies.push(TreeAnomaly {
                            directory: record,
                            path: child_path,
                            issue: TreeIssue::Loop { record: r },
                        });
                    } else if depth as usize + 1 > opts.max_depth {
                        tree.anomalies.push(TreeAnomaly {
                            directory: record,
                            path: child_path,
                            issue: TreeIssue::Depth { record: r },
                        });
                    } else {
                        queue.push_back(idx);
                    }
                }
            }
            obs.progress((tree.nodes.len() as u64).min(records), total);
        }
        if tree.status != TreeStatus::Complete || !opts.orphans {
            return Ok(tree);
        }
        obs.progress(records, total);
        let mut candidates: BTreeMap<u64, ()> = BTreeMap::new();
        for item in self.records_in(FIRST_USER_RECORD..records) {
            let (n, o) = item?;
            if n % 64 == 0 {
                if obs.cancelled() {
                    tree.status = TreeStatus::Cancelled;
                    return Ok(tree);
                }
                obs.progress(records + n, total);
            }
            if let RecordOutcome::Parsed(rec) = &o {
                if rec.in_use() && rec.header().is_base() && !reached.contains(&n) {
                    candidates.insert(n, ());
                }
            }
        }
        for n in candidates.keys() {
            let f = match self.file(*n) {
                Ok(f) => f,
                Err(NtfsError::Io(e)) => return Err(NtfsError::Io(e)),
                Err(_) => continue,
            };
            let names = f.file_names();
            let Some(name) = preferred_name(&names) else {
                continue; // sin nombre: no es una entrada del espacio de nombres
            };
            let reason = match self.parent_problem(name.parent)? {
                Some(r) => r,
                None if listed.contains(&name.parent.record) => OrphanReason::NotIndexed,
                None => OrphanReason::ParentOrphan,
            };
            tree.orphans.push(Orphan {
                record: *n,
                sequence: f.header.sequence,
                name: name.name.clone(),
                parent: name.parent,
                is_directory: has_i30(&f),
                reason,
            });
        }
        obs.progress(total, total);
        Ok(tree)
    }
}

/// Elige la entrada viva que coincide con `units`: exacta primero, después sin distinguir
/// mayúsculas y, por último, por el nombre corto 8.3.
fn pick(listing: &DirListing, units: &[u16], up: &Upcase) -> Option<DirEntry> {
    let live: Vec<&DirEntry> = listing.live().collect();
    live.iter()
        .find(|e| e.name_units == units)
        .or_else(|| {
            live.iter()
                .find(|e| up.eq_ignore_case(&e.name_units, units))
        })
        .or_else(|| {
            live.iter().find(|e| {
                e.dos_name
                    .as_ref()
                    .is_some_and(|d| up.eq_ignore_case(&units_of(d), units))
            })
        })
        .map(|e| (*e).clone())
}
