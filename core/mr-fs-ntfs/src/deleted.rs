//! Ficheros borrados a partir de la MFT y su recuperabilidad (#23, ADR-027).
//!
//! [`scan_deleted`] recorre la MFT dos veces con memoria acotada:
//!
//! 1. **Descubrimiento.** Cada registro **base** que no está en uso y conserva un registro FILE
//!    válido con algún `$FILE_NAME` es un fichero (o directorio) borrado; los registros no en uso
//!    sin `$FILE_NAME` son registros libres (solo se cuentan). Las extensiones no en uso no son
//!    entradas: se unen a su base por la `$ATTRIBUTE_LIST` (con la holgura de secuencia de un
//!    registro liberado, ver `docs/formatos/ntfs.md`) y, si la lista del base falta o no se puede
//!    leer, por su referencia al base (número y secuencia, o secuencia + 1). Los registros dañados,
//!    ilegibles o fuera del origen se informan como tales ([`DeletedScan::problems`]), nunca como
//!    borrados. De cada entrada: nombre (`$FILE_NAME` no DOS si lo hay), padre (número y
//!    secuencia) y su estado ([`ParentState`]: vivo, borrado o huérfano), ruta por la cadena de
//!    padres de la MFT si llega a la raíz, tamaños, fechas e indicador de directorio.
//! 2. **Propietarios.** Con los clústeres de los flujos borrados como consulta, se recorren los
//!    registros en uso (base y extensiones) y se guardan solo las intersecciones de sus runs no
//!    residentes (de cualquier tipo de atributo) con esa consulta ([`ClaimIndex`], acotado a
//!    [`ScanOptions::max_claims`]). Lo mismo con los runs de los demás borrados.
//!
//! **Clasificación de cada clúster** de un flujo borrado (en este orden): asignado a un fichero
//! vivo según sus runs ([`ClusterState::Owned`], aunque `$Bitmap` diga libre), asignado en
//! `$Bitmap` sin propietario conocido ([`ClusterState::Allocated`]), asignación desconocida
//! ([`ClusterState::Unknown`]: bit ilegible, mapa corto o `$Bitmap` inutilizable), libre pero
//! reclamado también por otro borrado ([`ClusterState::SharedWithDeleted`]: no se sabe cuál lo
//! escribió el último) o libre ([`ClusterState::Free`]). Todo lo que no es libre es
//! "posiblemente sobrescrito".
//!
//! **Recuperabilidad** ([`Recoverability`]), conservadora: `full` solo si todos los clústeres con
//! contenido están libres y no hay tramos sin mapa (o el flujo es residente y el registro es
//! válido, o vacío); `none` si ningún clúster con contenido está libre ni es disperso; `partial` en
//! los demás casos; un flujo cuya disposición no se puede usar (cifrado, extensiones
//! incoherentes...) es `none`. Nunca se dice `full` si algún clúster tiene otro dueño.
//!
//! **Pendiente** (tras #22): las entradas `INDX` residuales (entradas de índice de directorio
//! en el espacio libre de los bloques `INDX`) serán otra fuente de entradas
//! ([`EntryOrigin`] tendrá una variante para ellas); las que coincidan con un registro de esta
//! lista añadirán nombres o rutas, y las que apunten a un registro reutilizado se clasificarán con
//! [`assess_layout`] cuando haya runs o como `none` si solo queda el `$FILE_NAME` del índice.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::attr::{types, AttrBody};
use crate::bitmap::{BitmapIssue, ClusterAlloc, ClusterBitmap};
use crate::file::{FileAnomaly, Geometry, NtfsFile, SegmentKind, StreamError, StreamLayout};
use crate::mft::{RecordOutcome, RecordState};
use crate::record::{FileRecord, FileReference};
use crate::runs::decode_runs;
use crate::values::{FileName, Namespace, Times};
use crate::{NtfsError, NtfsVolume};

/// Registro de la raíz del volumen.
pub const ROOT_RECORD: u64 = 5;
/// Máximo de entradas borradas por defecto.
pub const MAX_ENTRIES: usize = 1_000_000;
/// Máximo de intersecciones guardadas en cada índice de propietarios por defecto (24 B cada una).
pub const MAX_CLAIMS: usize = 4_000_000;
/// Máximo de directorios recordados para las rutas.
pub const MAX_DIRECTORIES: usize = 2_000_000;
/// Máximo de registros no interpretables listados (los recuentos son exactos).
pub const MAX_PROBLEMS: usize = 10_000;
/// Máximo de tramos de clústeres guardados por flujo por defecto (los recuentos son exactos).
pub const MAX_RANGES_PER_STREAM: usize = 4096;
/// Profundidad máxima de una ruta (evita ciclos en la cadena de padres).
pub const MAX_PATH_DEPTH: usize = 1024;
/// Máximo de registros de extensión no en uso recordados para unirlos a su base.
pub const MAX_LOOSE_EXTENSIONS: usize = 1_000_000;

/// Opciones de [`scan_deleted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// Solo se conserva la entrada de este registro (recuperación de un fichero); los demás
    /// borrados se recorren igual para detectar clústeres compartidos.
    pub focus: Option<u64>,
    pub max_entries: usize,
    pub max_claims: usize,
    pub max_ranges_per_stream: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            focus: None,
            max_entries: MAX_ENTRIES,
            max_claims: MAX_CLAIMS,
            max_ranges_per_stream: MAX_RANGES_PER_STREAM,
        }
    }
}

/// Observador del recorrido: cancelación cooperativa y progreso (registros leídos de las dos
/// pasadas más entradas evaluadas).
pub trait ScanObserver {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _done: u64, _total: u64) {}
}

/// Observador que no cancela ni informa.
pub struct NoObserver;

impl ScanObserver for NoObserver {}

/// Índice acotado de quién reclama clústeres: solo guarda las intersecciones de los tramos
/// añadidos con la consulta (los clústeres de los flujos borrados).
#[derive(Debug, Clone, Default)]
pub struct ClaimIndex {
    /// Tramos `[inicio, fin)` de la consulta, ordenados y sin solaparse.
    queries: Vec<(u64, u64)>,
    claims: Vec<Claim>,
    limit: usize,
    truncated: bool,
}

/// Clústeres `[start, end)` reclamados por el registro base `owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Claim {
    pub start: u64,
    pub end: u64,
    pub owner: u64,
}

impl ClaimIndex {
    /// Índice para la consulta `(lcn, clústeres)` con como mucho `limit` intersecciones.
    pub fn new(query: &[(u64, u64)], limit: usize) -> Self {
        let mut q: Vec<(u64, u64)> = query
            .iter()
            .filter(|r| r.1 > 0)
            .map(|&(l, n)| (l, l.saturating_add(n)))
            .collect();
        q.sort_unstable();
        let mut queries: Vec<(u64, u64)> = Vec::with_capacity(q.len());
        for (a, b) in q {
            match queries.last_mut() {
                Some((_, e)) if *e >= a => *e = (*e).max(b),
                _ => queries.push((a, b)),
            }
        }
        ClaimIndex {
            queries,
            claims: Vec::new(),
            limit,
            truncated: false,
        }
    }

    /// Añade los clústeres `[lcn, lcn + n)` de `owner` (solo lo que corta la consulta).
    pub fn add(&mut self, lcn: u64, n: u64, owner: u64) {
        if n == 0 {
            return;
        }
        let end = lcn.saturating_add(n);
        let mut i = self.queries.partition_point(|q| q.1 <= lcn);
        while let Some(&(qa, qb)) = self.queries.get(i) {
            if qa >= end {
                break;
            }
            let (a, b) = (lcn.max(qa), end.min(qb));
            if a < b {
                if self.claims.len() >= self.limit {
                    self.truncated = true;
                    return;
                }
                self.claims.push(Claim {
                    start: a,
                    end: b,
                    owner,
                });
            }
            i += 1;
        }
    }

    /// Ordena el índice (llamar tras el último [`ClaimIndex::add`]).
    pub fn finish(&mut self) {
        self.claims.sort_unstable();
        self.claims.dedup();
    }

    /// `true` si se descartaron intersecciones por el límite (los propietarios quedan
    /// incompletos; la asignación sigue saliendo de `$Bitmap`).
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    /// Reclamaciones que cortan `[lcn, lcn + n)`, recortadas a ese tramo y sin las de `exclude`.
    pub fn overlapping(&self, lcn: u64, n: u64, exclude: Option<u64>) -> Vec<Claim> {
        let end = lcn.saturating_add(n);
        // Cada reclamación está dentro de un tramo de la consulta: se empieza en el tramo que
        // contiene `lcn` (o el siguiente).
        let qi = self.queries.partition_point(|q| q.1 <= lcn);
        let from = self.queries.get(qi).map_or(lcn, |q| q.0.min(lcn));
        let mut i = self.claims.partition_point(|c| c.start < from);
        let mut out = Vec::new();
        while let Some(c) = self.claims.get(i) {
            if c.start >= end {
                break;
            }
            if c.end > lcn && Some(c.owner) != exclude {
                out.push(Claim {
                    start: c.start.max(lcn),
                    end: c.end.min(end),
                    owner: c.owner,
                });
            }
            i += 1;
        }
        out
    }
}

/// Estado de un tramo de clústeres de un flujo borrado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClusterState {
    /// Libre en `$Bitmap` y sin otro dueño: contenido original salvo que se escribiera sin
    /// asignar (no detectable).
    Free,
    /// En los runs de un fichero vivo (registro base `owner`).
    Owned { owner: u64 },
    /// Asignado en `$Bitmap` sin propietario conocido.
    Allocated,
    /// Asignación desconocida (bit no fiable, mapa corto o `$Bitmap` inutilizable).
    Unknown,
    /// Libre, pero también en los runs de otro fichero borrado (`record`).
    SharedWithDeleted { record: u64 },
}

impl ClusterState {
    pub fn code(self) -> &'static str {
        match self {
            ClusterState::Free => "free",
            ClusterState::Owned { .. } => "owned",
            ClusterState::Allocated => "allocated",
            ClusterState::Unknown => "unknown",
            ClusterState::SharedWithDeleted { .. } => "shared_with_deleted",
        }
    }

    /// `true` si el contenido puede no ser el original.
    pub fn is_suspect(self) -> bool {
        self != ClusterState::Free
    }
}

/// Tramo de clústeres de un flujo con su estado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterRange {
    pub vcn: u64,
    pub lcn: u64,
    pub length: u64,
    pub state: ClusterState,
}

/// Recuperabilidad de un flujo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Recoverability {
    Full,
    Partial,
    None,
}

impl Recoverability {
    pub fn code(self) -> &'static str {
        match self {
            Recoverability::Full => "full",
            Recoverability::Partial => "partial",
            Recoverability::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Recoverability::Full => "íntegro",
            Recoverability::Partial => "PARCIAL",
            Recoverability::None => "NINGUNO",
        }
    }
}

/// Motivo de una clasificación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Contenido residente en el registro (válido): no depende de clústeres.
    Resident,
    /// Flujo vacío.
    Empty,
    /// Todos los clústeres con contenido están libres.
    AllFree { clusters: u64 },
    /// Clústeres en los runs de ficheros vivos.
    Owned { clusters: u64, owners: Vec<u64> },
    /// Clústeres asignados en `$Bitmap` sin propietario conocido.
    Allocated { clusters: u64 },
    /// Clústeres con asignación desconocida.
    Unknown { clusters: u64 },
    /// Clústeres libres reclamados también por otros borrados.
    SharedWithDeleted { clusters: u64, records: Vec<u64> },
    /// Clústeres sin mapa (extensión perdida o runs no válidos): no hay de dónde leerlos.
    Unmapped { clusters: u64 },
    /// La disposición del flujo no se puede usar.
    Stream(StreamError),
}

impl Reason {
    pub fn code(&self) -> &'static str {
        match self {
            Reason::Resident => "resident",
            Reason::Empty => "empty",
            Reason::AllFree { .. } => "all_free",
            Reason::Owned { .. } => "owned_by_live_file",
            Reason::Allocated { .. } => "allocated_in_bitmap",
            Reason::Unknown { .. } => "allocation_unknown",
            Reason::SharedWithDeleted { .. } => "shared_with_deleted",
            Reason::Unmapped { .. } => "unmapped",
            Reason::Stream(e) => e.code(),
        }
    }
}

fn list(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::Resident => write!(f, "contenido residente en el registro MFT"),
            Reason::Empty => write!(f, "flujo vacío"),
            Reason::AllFree { clusters } => {
                write!(f, "los {clusters} clústeres siguen libres en $Bitmap")
            }
            Reason::Owned { clusters, owners } => write!(
                f,
                "{clusters} clústeres en los runs de ficheros vivos (registros {})",
                list(owners)
            ),
            Reason::Allocated { clusters } => write!(
                f,
                "{clusters} clústeres asignados en $Bitmap sin propietario conocido"
            ),
            Reason::Unknown { clusters } => write!(
                f,
                "{clusters} clústeres con asignación desconocida ($Bitmap no fiable ahí)"
            ),
            Reason::SharedWithDeleted { clusters, records } => write!(
                f,
                "{clusters} clústeres libres reclamados también por otros borrados (registros {})",
                list(records)
            ),
            Reason::Unmapped { clusters } => {
                write!(
                    f,
                    "{clusters} clústeres sin mapa (no se sabe dónde estaban)"
                )
            }
            Reason::Stream(e) => write!(f, "flujo no utilizable: {e}"),
        }
    }
}

/// Evaluación de un flujo de un fichero borrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAssessment {
    /// `""` = flujo principal.
    pub name: String,
    pub size: u64,
    pub resident: bool,
    pub compressed: bool,
    /// Clústeres en disco con contenido (dentro del tamaño inicializado).
    pub clusters: u64,
    pub free: u64,
    pub owned: u64,
    pub allocated: u64,
    pub unknown: u64,
    pub shared: u64,
    pub unmapped: u64,
    pub sparse: u64,
    /// Tramos en disco con su estado (como mucho `max_ranges_per_stream`).
    pub ranges: Vec<ClusterRange>,
    pub ranges_truncated: bool,
    /// Registros base vivos dueños de algún clúster, sin repetir.
    pub owners: Vec<u64>,
    /// Otros borrados que reclaman algún clúster, sin repetir.
    pub shared_with: Vec<u64>,
    pub recoverability: Recoverability,
    pub reasons: Vec<Reason>,
}

impl StreamAssessment {
    /// Clústeres posiblemente sobrescritos (todo lo que no está libre).
    pub fn possibly_overwritten(&self) -> u64 {
        self.owned + self.allocated + self.unknown + self.shared
    }

    /// Tramos `(lcn, clústeres)` sospechosos (para [`crate::DataStream::with_suspect_clusters`]).
    pub fn suspect_clusters(&self) -> Vec<(u64, u64)> {
        self.ranges
            .iter()
            .filter(|r| r.state.is_suspect())
            .map(|r| (r.lcn, r.length))
            .collect()
    }
}

/// Clasifica `layout` con el mapa de bits y los índices de propietarios (vivos y borrados). Un
/// error de disposición es `none`. `record` es el propio registro (se excluye de los borrados).
#[allow(clippy::too_many_arguments)]
pub fn assess_layout(
    record: u64,
    name: &str,
    layout: Result<&StreamLayout, &StreamError>,
    bitmap: &mut ClusterBitmap<'_>,
    live: &ClaimIndex,
    deleted: &ClaimIndex,
    max_ranges: usize,
) -> Result<StreamAssessment, NtfsError> {
    let mut a = StreamAssessment {
        name: name.to_string(),
        size: 0,
        resident: false,
        compressed: false,
        clusters: 0,
        free: 0,
        owned: 0,
        allocated: 0,
        unknown: 0,
        shared: 0,
        unmapped: 0,
        sparse: 0,
        ranges: Vec::new(),
        ranges_truncated: false,
        owners: Vec::new(),
        shared_with: Vec::new(),
        recoverability: Recoverability::None,
        reasons: Vec::new(),
    };
    let l = match layout {
        Ok(l) => l,
        Err(e) => {
            a.reasons.push(Reason::Stream(e.clone()));
            return Ok(a);
        }
    };
    a.size = l.data_size;
    a.resident = l.is_resident();
    a.compressed = l.is_compressed();
    if l.is_resident() {
        a.recoverability = Recoverability::Full;
        a.reasons.push(Reason::Resident);
        return Ok(a);
    }
    let cluster = u64::from(l.cluster_size).max(1);
    // Clústeres con contenido: lo inicializado (comprimido: unidades enteras).
    let content = if l.is_compressed() {
        let cu = l.compression_unit_clusters.max(1);
        l.data_size.div_ceil(cluster).div_ceil(cu) * cu
    } else {
        l.initialized_size.min(l.data_size).div_ceil(cluster)
    };
    if content == 0 {
        a.recoverability = Recoverability::Full;
        a.reasons.push(Reason::Empty);
        return Ok(a);
    }
    let mut owners = BTreeSet::new();
    let mut shared_with = BTreeSet::new();
    for seg in &l.segments {
        if seg.vcn >= content {
            break;
        }
        let len = seg.length.min(content - seg.vcn);
        match seg.kind {
            SegmentKind::Sparse => a.sparse += len,
            SegmentKind::Unmapped => a.unmapped += len,
            SegmentKind::Disk(lcn) => {
                a.clusters += len;
                for (s_lcn, s_len, state) in
                    classify_clusters(record, lcn, len, bitmap, live, deleted)?
                {
                    match state {
                        ClusterState::Free => a.free += s_len,
                        ClusterState::Owned { owner } => {
                            a.owned += s_len;
                            owners.insert(owner);
                        }
                        ClusterState::Allocated => a.allocated += s_len,
                        ClusterState::Unknown => a.unknown += s_len,
                        ClusterState::SharedWithDeleted { record } => {
                            a.shared += s_len;
                            shared_with.insert(record);
                        }
                    }
                    let r = ClusterRange {
                        vcn: seg.vcn + (s_lcn - lcn),
                        lcn: s_lcn,
                        length: s_len,
                        state,
                    };
                    let full = a.ranges.len() >= max_ranges;
                    match a.ranges.last_mut() {
                        Some(p)
                            if p.state == r.state
                                && p.vcn + p.length == r.vcn
                                && p.lcn + p.length == r.lcn =>
                        {
                            p.length += r.length
                        }
                        _ if full => a.ranges_truncated = true,
                        _ => a.ranges.push(r),
                    }
                }
            }
        }
    }
    a.owners = owners.into_iter().collect();
    a.shared_with = shared_with.into_iter().collect();
    let suspect = a.possibly_overwritten();
    a.recoverability = if suspect == 0 && a.unmapped == 0 {
        Recoverability::Full
    } else if a.free == 0 && a.sparse == 0 {
        Recoverability::None
    } else {
        Recoverability::Partial
    };
    if a.recoverability == Recoverability::Full {
        a.reasons.push(Reason::AllFree {
            clusters: a.clusters,
        });
    }
    if a.owned > 0 {
        a.reasons.push(Reason::Owned {
            clusters: a.owned,
            owners: a.owners.clone(),
        });
    }
    if a.allocated > 0 {
        a.reasons.push(Reason::Allocated {
            clusters: a.allocated,
        });
    }
    if a.unknown > 0 {
        a.reasons.push(Reason::Unknown {
            clusters: a.unknown,
        });
    }
    if a.shared > 0 {
        a.reasons.push(Reason::SharedWithDeleted {
            clusters: a.shared,
            records: a.shared_with.clone(),
        });
    }
    if a.unmapped > 0 {
        a.reasons.push(Reason::Unmapped {
            clusters: a.unmapped,
        });
    }
    Ok(a)
}

/// Estado de cada clúster de `[lcn, lcn + n)` (tramos contiguos, mismo estado unido). Barrido
/// lineal sobre los puntos de corte (asignación, reclamaciones vivas y de otros borrados).
fn classify_clusters(
    record: u64,
    lcn: u64,
    n: u64,
    bitmap: &mut ClusterBitmap<'_>,
    live: &ClaimIndex,
    deleted: &ClaimIndex,
) -> Result<Vec<(u64, u64, ClusterState)>, NtfsError> {
    let alloc = bitmap.runs(lcn, n)?;
    let owned = live.overlapping(lcn, n, None);
    let others = deleted.overlapping(lcn, n, Some(record));
    let end = lcn.saturating_add(n);
    let mut cuts: Vec<u64> = vec![lcn, end];
    cuts.extend(alloc.iter().map(|r| r.0));
    for c in owned.iter().chain(&others) {
        cuts.push(c.start);
        cuts.push(c.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    // Reclamaciones activas en cada tramo elemental (ordenadas por inicio).
    struct Sweep<'c> {
        claims: &'c [Claim],
        next: usize,
        active: Vec<Claim>,
    }
    impl Sweep<'_> {
        fn at(&mut self, a: u64) -> Option<Claim> {
            while let Some(c) = self.claims.get(self.next) {
                if c.start > a {
                    break;
                }
                self.active.push(*c);
                self.next += 1;
            }
            self.active.retain(|c| c.end > a);
            // Todos los puntos de corte incluyen inicios y finales: una activa cubre el tramo.
            self.active.iter().min_by_key(|c| c.owner).copied()
        }
    }
    let mut live_sweep = Sweep {
        claims: &owned,
        next: 0,
        active: Vec::new(),
    };
    let mut other_sweep = Sweep {
        claims: &others,
        next: 0,
        active: Vec::new(),
    };
    let mut k = 0usize;
    let mut out: Vec<(u64, u64, ClusterState)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        while k < alloc.len() && alloc[k].0 + alloc[k].1 <= a {
            k += 1;
        }
        let bit = alloc
            .get(k)
            .filter(|r| r.0 <= a)
            .map_or(ClusterAlloc::Unknown, |r| r.2);
        let owner = live_sweep.at(a);
        let other = other_sweep.at(a);
        let state = if let Some(c) = owner {
            ClusterState::Owned { owner: c.owner }
        } else {
            match bit {
                ClusterAlloc::Allocated => ClusterState::Allocated,
                ClusterAlloc::Unknown => ClusterState::Unknown,
                ClusterAlloc::Free => match other {
                    Some(c) => ClusterState::SharedWithDeleted { record: c.owner },
                    None => ClusterState::Free,
                },
            }
        };
        match out.last_mut() {
            Some((pl, pn, ps)) if *ps == state && *pl + *pn == a => *pn += b - a,
            _ => out.push((a, b - a, state)),
        }
    }
    Ok(out)
}

/// Estado del directorio padre de una entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentState {
    /// En uso, directorio y con la misma secuencia.
    Live,
    /// Directorio no en uso con la misma secuencia o una más (borrado a la vez o antes).
    Deleted,
    /// El padre ya no es ese directorio.
    Orphan(OrphanReason),
}

/// Por qué una entrada es huérfana.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanReason {
    /// El registro del padre está fuera de la MFT.
    OutOfRange,
    /// El registro del padre no es interpretable (dañado, ilegible, vacío...).
    Unusable(RecordState),
    /// Otra secuencia: el registro se ha reutilizado.
    SequenceMismatch { found: u16 },
    /// El registro del padre no es un directorio (o no es un registro base).
    NotDirectory,
}

impl ParentState {
    pub fn code(self) -> &'static str {
        match self {
            ParentState::Live => "live",
            ParentState::Deleted => "deleted",
            ParentState::Orphan(r) => match r {
                OrphanReason::OutOfRange => "orphan_out_of_range",
                OrphanReason::Unusable(_) => "orphan_unusable",
                OrphanReason::SequenceMismatch { .. } => "orphan_sequence_mismatch",
                OrphanReason::NotDirectory => "orphan_not_directory",
            },
        }
    }

    pub fn is_orphan(self) -> bool {
        matches!(self, ParentState::Orphan(_))
    }
}

impl fmt::Display for ParentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParentState::Live => write!(f, "padre vivo"),
            ParentState::Deleted => write!(f, "padre borrado"),
            ParentState::Orphan(OrphanReason::OutOfRange) => {
                write!(f, "huérfano: el padre está fuera de la MFT")
            }
            ParentState::Orphan(OrphanReason::Unusable(s)) => {
                write!(
                    f,
                    "huérfano: el registro del padre no es utilizable ({})",
                    s.label()
                )
            }
            ParentState::Orphan(OrphanReason::SequenceMismatch { found }) => write!(
                f,
                "huérfano: el registro del padre se ha reutilizado (secuencia {found})"
            ),
            ParentState::Orphan(OrphanReason::NotDirectory) => {
                write!(f, "huérfano: el registro del padre no es un directorio")
            }
        }
    }
}

/// Origen de una entrada borrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryOrigin {
    /// Registro base de la MFT no en uso.
    MftRecord,
    // Pendiente (#23 tras #22): entrada `INDX` residual.
}

/// Fichero o directorio borrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedEntry {
    pub origin: EntryOrigin,
    pub record: u64,
    /// Secuencia actual del registro (el sistema de archivos la incrementa al liberarlo).
    pub sequence: u16,
    pub is_directory: bool,
    /// Nombre elegido (`$FILE_NAME` no DOS si lo hay).
    pub name: String,
    pub namespace: Namespace,
    /// Todos los nombres (enlaces y espacios de nombres).
    pub names: Vec<FileName>,
    pub parent: FileReference,
    pub parent_state: ParentState,
    /// Ruta desde la raíz por la cadena de padres de la MFT (`None` si no llega a la raíz).
    pub path: Option<String>,
    /// Tamaño de datos del flujo principal (o el del `$FILE_NAME` si no hay `$DATA`).
    pub size: u64,
    pub allocated_size: u64,
    /// Fechas de `$STANDARD_INFORMATION` (o del `$FILE_NAME` si falta).
    pub times: Times,
    /// Extensiones usadas por la `$ATTRIBUTE_LIST`.
    pub extensions: Vec<u64>,
    /// Extensiones unidas por su referencia al base (lista ausente o ilegible).
    pub backref_extensions: Vec<u64>,
    pub anomalies: Vec<FileAnomaly>,
    /// Flujos `$DATA` (el principal primero).
    pub streams: Vec<StreamAssessment>,
}

impl DeletedEntry {
    /// Flujo principal, si lo hay.
    pub fn main_stream(&self) -> Option<&StreamAssessment> {
        self.streams.iter().find(|s| s.name.is_empty())
    }

    /// Recuperabilidad del flujo principal (`None` sin `$DATA`, p. ej. directorios).
    pub fn recoverability(&self) -> Option<Recoverability> {
        self.main_stream().map(|s| s.recoverability)
    }

    /// Ruta para mostrar: la completa o `<huérfano>/nombre`.
    pub fn display_path(&self) -> String {
        self.path
            .clone()
            .unwrap_or_else(|| format!("<huérfano {}>/{}", self.parent.record, self.name))
    }
}

/// Dueño vivo de clústeres de un borrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerInfo {
    pub record: u64,
    pub sequence: u16,
    pub name: Option<String>,
    pub path: Option<String>,
}

impl OwnerInfo {
    pub fn display(&self) -> String {
        self.path
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| format!("registro {}", self.record))
    }
}

/// Registro que no se pudo interpretar (nunca es un borrado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordProblem {
    pub record: u64,
    pub state: RecordState,
    pub detail: Option<String>,
}

/// Resultado de [`scan_deleted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedScan {
    pub records: u64,
    /// Registros por estado (en uso, no en uso, dañados...).
    pub counts: BTreeMap<RecordState, u64>,
    /// Registros base no en uso sin `$FILE_NAME` (libres).
    pub free_records: u64,
    /// Registros de extensión no en uso.
    pub loose_extensions: u64,
    pub entries: Vec<DeletedEntry>,
    /// Se alcanzó [`ScanOptions::max_entries`].
    pub entries_truncated: bool,
    /// Registros dañados, ilegibles o fuera del origen (como mucho [`MAX_PROBLEMS`]).
    pub problems: Vec<RecordProblem>,
    pub problems_truncated: bool,
    pub bitmap_issue: Option<BitmapIssue>,
    /// Algún índice de propietarios se quedó sin sitio (propietarios incompletos).
    pub claims_truncated: bool,
    /// Directorios de más: algunas rutas no se resuelven.
    pub directories_truncated: bool,
    /// Dueños vivos nombrados en las evaluaciones.
    pub owners: BTreeMap<u64, OwnerInfo>,
}

impl DeletedScan {
    pub fn entry(&self, record: u64) -> Option<&DeletedEntry> {
        self.entries.iter().find(|e| e.record == record)
    }

    /// Texto del motivo de un flujo con los dueños por nombre (como el `reason` del manifiesto).
    pub fn summary(&self, s: &StreamAssessment) -> String {
        if s.recoverability == Recoverability::Full
            || s.reasons.iter().any(|r| matches!(r, Reason::Stream(_)))
        {
            return s
                .reasons
                .iter()
                .map(Reason::to_string)
                .collect::<Vec<_>>()
                .join("; ");
        }
        let mut parts = vec![format!(
            "{} de {} clústeres posiblemente sobrescritos",
            s.possibly_overwritten(),
            s.clusters
        )];
        if !s.owners.is_empty() {
            let names: Vec<String> = s
                .owners
                .iter()
                .map(|o| {
                    self.owners
                        .get(o)
                        .map_or_else(|| format!("registro {o}"), OwnerInfo::display)
                })
                .collect();
            parts[0].push_str(&format!(", asignados ahora a {}", names.join(", ")));
        }
        for r in &s.reasons {
            if !matches!(r, Reason::Owned { .. }) {
                parts.push(r.to_string());
            }
        }
        parts.join("; ")
    }
}

/// Directorio recordado para las rutas.
#[derive(Debug, Clone)]
struct DirInfo {
    sequence: u16,
    in_use: bool,
    parent: FileReference,
    name: Option<String>,
}

/// Nombre preferido: el primero que no es DOS (8.3); si solo hay DOS, ese.
fn best_name(names: &[FileName]) -> Option<&FileName> {
    names
        .iter()
        .find(|n| n.namespace != Namespace::Dos)
        .or_else(|| names.first())
}

/// `$FILE_NAME` válidos del propio registro.
fn record_names(rec: &FileRecord) -> Vec<FileName> {
    rec.attributes()
        .map_while(Result::ok)
        .filter(|a| a.type_code == types::FILE_NAME)
        .filter_map(|a| a.resident_value().and_then(|v| FileName::parse(v).ok()))
        .collect()
}

fn has_attribute_list(rec: &FileRecord) -> bool {
    rec.attributes()
        .map_while(Result::ok)
        .any(|a| a.type_code == types::ATTRIBUTE_LIST)
}

/// Secuencia del registro frente a la referenciada (un registro liberado lleva una más).
fn seq_matches(found: u16, referenced: u16, in_use: bool) -> bool {
    found == referenced || (!in_use && found == referenced.wrapping_add(1))
}

/// Estado del padre `p` de una entrada.
fn parent_state(
    vol: &NtfsVolume,
    dirs: &HashMap<u64, DirInfo>,
    p: FileReference,
) -> Result<ParentState, NtfsError> {
    if let Some(d) = dirs.get(&p.record) {
        return Ok(if d.in_use && d.sequence == p.sequence {
            ParentState::Live
        } else if !d.in_use && seq_matches(d.sequence, p.sequence, false) {
            ParentState::Deleted
        } else {
            ParentState::Orphan(OrphanReason::SequenceMismatch { found: d.sequence })
        });
    }
    if p.record >= vol.record_count() {
        return Ok(ParentState::Orphan(OrphanReason::OutOfRange));
    }
    let o = vol.read_record(p.record)?;
    Ok(match o {
        RecordOutcome::Parsed(r) => {
            if r.header().sequence != p.sequence
                && !seq_matches(r.header().sequence, p.sequence, r.in_use())
            {
                ParentState::Orphan(OrphanReason::SequenceMismatch {
                    found: r.header().sequence,
                })
            } else {
                ParentState::Orphan(OrphanReason::NotDirectory)
            }
        }
        o => ParentState::Orphan(OrphanReason::Unusable(o.state())),
    })
}

/// Ruta de `name` bajo `parent` por la cadena de directorios (vivos o borrados con la secuencia
/// coherente); `None` si no llega a la raíz.
fn build_path(dirs: &HashMap<u64, DirInfo>, parent: FileReference, name: &str) -> Option<String> {
    let mut parts = vec![name.to_string()];
    let mut cur = parent;
    for _ in 0..MAX_PATH_DEPTH {
        let d = dirs.get(&cur.record)?;
        if !seq_matches(d.sequence, cur.sequence, d.in_use) {
            return None;
        }
        if cur.record == ROOT_RECORD {
            parts.reverse();
            return Some(format!("/{}", parts.join("/")));
        }
        parts.push(d.name.clone()?);
        cur = d.parent;
    }
    None
}

/// Entrada en preparación (con las disposiciones de sus flujos).
struct Pending {
    entry: DeletedEntry,
    layouts: Vec<(String, Result<StreamLayout, StreamError>)>,
}

/// Tramos en disco `(lcn, clústeres)` de una disposición (todo lo asignado, no solo el contenido:
/// para detectar clústeres compartidos basta con lo que el registro reclama).
fn disk_extents(l: &StreamLayout) -> impl Iterator<Item = (u64, u64)> + '_ {
    l.segments.iter().filter_map(|s| match s.kind {
        SegmentKind::Disk(lcn) => Some((lcn, s.length)),
        _ => None,
    })
}

/// Une a `f` las extensiones no en uso que apuntan a él cuando su `$ATTRIBUTE_LIST` falta o no
/// se pudo leer. Devuelve las que se han unido.
pub(crate) fn attach_backrefs(
    vol: &NtfsVolume,
    f: &mut NtfsFile,
    candidates: &[(u64, u16)],
) -> Vec<u64> {
    let list_ok = !f.attribute_list.is_empty()
        && !f
            .anomalies
            .iter()
            .any(|a| matches!(a, FileAnomaly::AttributeListUnreadable { .. }));
    if list_ok {
        return Vec::new();
    }
    let mut used = Vec::new();
    for &(ext, base_seq) in candidates {
        if f.extensions.contains(&ext) || !seq_matches(f.header.sequence, base_seq, false) {
            continue;
        }
        let Ok(RecordOutcome::Parsed(r)) = vol.read_record(ext) else {
            continue;
        };
        if r.in_use() || has_attribute_list(&r) {
            continue;
        }
        let (attrs, walk) = crate::file::record_attributes(&r);
        if let Some(w) = walk {
            f.anomalies.push(w);
        }
        f.attributes.extend(attrs);
        f.extensions.push(ext);
        used.push(ext);
    }
    used
}

fn pending_entry(
    vol: &NtfsVolume,
    g: Geometry,
    dirs: &HashMap<u64, DirInfo>,
    mut f: NtfsFile,
    backrefs: &[(u64, u16)],
) -> Result<Option<Pending>, NtfsError> {
    let backref_extensions = attach_backrefs(vol, &mut f, backrefs);
    let names = f.file_names();
    let Some(best) = best_name(&names).cloned() else {
        return Ok(None);
    };
    let parent = best.parent;
    let parent_state = parent_state(vol, dirs, parent)?;
    let path = if parent_state.is_orphan() {
        None
    } else {
        build_path(dirs, parent, &best.name)
    };
    let layouts: Vec<(String, Result<StreamLayout, StreamError>)> = f
        .data_streams()
        .into_iter()
        .map(|n| {
            let l = f.stream_layout(types::DATA, &n, g);
            (n, l)
        })
        .collect();
    let main = layouts
        .iter()
        .find(|(n, _)| n.is_empty())
        .and_then(|(_, l)| l.as_ref().ok());
    let (size, allocated_size) = main.map_or((best.data_size, best.allocated_size), |l| {
        (l.data_size, l.allocated_size)
    });
    let times = f.standard_information().map_or(best.times, |si| si.times);
    let mut anomalies = f.anomalies.clone();
    anomalies.extend(f.value_anomalies());
    Ok(Some(Pending {
        entry: DeletedEntry {
            origin: EntryOrigin::MftRecord,
            record: f.number,
            sequence: f.header.sequence,
            is_directory: f.is_directory(),
            name: best.name.clone(),
            namespace: best.namespace,
            names,
            parent,
            parent_state,
            path,
            size,
            allocated_size,
            times,
            extensions: f.extensions.clone(),
            backref_extensions,
            anomalies,
            streams: Vec::new(),
        },
        layouts,
    }))
}

/// Progreso y cancelación.
struct Progress<'o> {
    obs: &'o dyn ScanObserver,
    done: u64,
    total: u64,
}

impl Progress<'_> {
    fn step(&mut self, n: u64) -> bool {
        self.done += n;
        if self.done.is_multiple_of(64) || n > 1 {
            self.obs.progress(self.done, self.total);
        }
        !self.obs.cancelled()
    }
}

/// Recorre la MFT y devuelve los borrados con su recuperabilidad (ver el módulo). `Ok(None)` si
/// el observador pidió cancelar. Un error de E/S no recuperable termina el recorrido.
pub fn scan_deleted(
    vol: &NtfsVolume,
    opts: &ScanOptions,
    obs: &dyn ScanObserver,
) -> Result<Option<DeletedScan>, NtfsError> {
    let count = vol.record_count();
    let g = vol.geometry();
    let mut p = Progress {
        obs,
        done: 0,
        total: count.saturating_mul(2),
    };
    let mut counts: BTreeMap<RecordState, u64> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut problems_truncated = false;
    let mut candidates: Vec<u64> = Vec::new();
    let mut entries_truncated = false;
    let mut free_records = 0u64;
    let mut loose: HashMap<u64, Vec<(u64, u16)>> = HashMap::new();
    let mut loose_extensions = 0u64;
    let mut loose_kept = 0usize;
    let mut dirs: HashMap<u64, DirInfo> = HashMap::new();
    let mut directories_truncated = false;

    // Pasada 1: estados, candidatos, extensiones sueltas y directorios.
    for item in vol.records() {
        let (n, o) = item?;
        *counts.entry(o.state()).or_default() += 1;
        match &o {
            RecordOutcome::Parsed(r) => {
                let h = r.header();
                if h.is_base() {
                    let names = if h.is_directory() || !h.in_use() {
                        record_names(r)
                    } else {
                        Vec::new()
                    };
                    if h.is_directory() {
                        if dirs.len() < MAX_DIRECTORIES {
                            dirs.insert(
                                n,
                                DirInfo {
                                    sequence: h.sequence,
                                    in_use: h.in_use(),
                                    parent: best_name(&names)
                                        .map_or(FileReference::from_u64(0), |x| x.parent),
                                    name: best_name(&names).map(|x| x.name.clone()),
                                },
                            );
                        } else {
                            directories_truncated = true;
                        }
                    }
                    if !h.in_use() {
                        if names.is_empty() && !has_attribute_list(r) {
                            free_records += 1;
                        } else if candidates.len() < opts.max_entries {
                            candidates.push(n);
                        } else {
                            entries_truncated = true;
                        }
                    }
                } else if !h.in_use() {
                    loose_extensions += 1;
                    if loose_kept < MAX_LOOSE_EXTENSIONS {
                        loose_kept += 1;
                        loose
                            .entry(h.base_record.record)
                            .or_default()
                            .push((n, h.base_record.sequence));
                    }
                }
            }
            RecordOutcome::Damaged(_)
            | RecordOutcome::Unreadable { .. }
            | RecordOutcome::OutsideSource => {
                if problems.len() < MAX_PROBLEMS {
                    problems.push(RecordProblem {
                        record: n,
                        state: o.state(),
                        detail: o.detail(),
                    });
                } else {
                    problems_truncated = true;
                }
            }
            _ => {}
        }
        if !p.step(1) {
            return Ok(None);
        }
    }

    // Entradas: cada candidato reunido con sus extensiones.
    p.total = p.total.saturating_add(candidates.len() as u64 * 2);
    let mut pending: Vec<Pending> = Vec::new();
    let mut query: Vec<(u64, u64)> = Vec::new();
    let mut deleted_extents: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
    for &n in &candidates {
        let f = match vol.file(n) {
            Ok(f) => f,
            Err(NtfsError::Io(e)) => return Err(NtfsError::Io(e)),
            Err(e) => {
                // Leído bien en la pasada 1 y ahora no: se informa como problema.
                if problems.len() < MAX_PROBLEMS {
                    problems.push(RecordProblem {
                        record: n,
                        state: RecordState::NotInUse,
                        detail: Some(e.to_string()),
                    });
                }
                continue;
            }
        };
        let backrefs = loose.get(&n).map_or(&[][..], Vec::as_slice);
        match pending_entry(vol, g, &dirs, f, backrefs)? {
            Some(pe) => {
                let ext: Vec<(u64, u64)> = pe
                    .layouts
                    .iter()
                    .filter_map(|(_, l)| l.as_ref().ok())
                    .flat_map(disk_extents)
                    .collect();
                query.extend(&ext);
                deleted_extents.push((n, ext));
                if opts.focus.is_none_or(|f| f == n) {
                    pending.push(pe);
                }
            }
            None => free_records += 1,
        }
        if !p.step(1) {
            return Ok(None);
        }
    }

    // Pasada 2: propietarios vivos de los clústeres de los borrados.
    let mut live = ClaimIndex::new(&query, opts.max_claims);
    let mut deleted = ClaimIndex::new(&query, opts.max_claims);
    for (n, ext) in &deleted_extents {
        for &(l, c) in ext {
            deleted.add(l, c, *n);
        }
    }
    drop(deleted_extents);
    if query.is_empty() {
        p.done += count;
    } else {
        for item in vol.records() {
            let (n, o) = item?;
            if let RecordOutcome::Parsed(r) = &o {
                if r.in_use() {
                    let owner = if r.header().is_base() {
                        n
                    } else {
                        r.header().base_record.record
                    };
                    for a in r.attributes().map_while(Result::ok) {
                        if let AttrBody::NonResident(nr) = &a.body {
                            if let Ok(runs) =
                                decode_runs(nr.mapping_pairs, nr.start_vcn, g.total_clusters)
                            {
                                for run in runs {
                                    if let Some(l) = run.lcn {
                                        live.add(l, run.length, owner);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !p.step(1) {
                return Ok(None);
            }
        }
    }
    live.finish();
    deleted.finish();

    // Evaluación de cada flujo.
    let mut bitmap = ClusterBitmap::open(vol);
    let mut owner_ids: BTreeSet<u64> = BTreeSet::new();
    let mut entries = Vec::with_capacity(pending.len());
    for pe in pending {
        let Pending { mut entry, layouts } = pe;
        for (name, l) in &layouts {
            let a = assess_layout(
                entry.record,
                name,
                l.as_ref(),
                &mut bitmap,
                &live,
                &deleted,
                opts.max_ranges_per_stream,
            )?;
            owner_ids.extend(a.owners.iter().copied());
            entry.streams.push(a);
        }
        entries.push(entry);
        if !p.step(1) {
            return Ok(None);
        }
    }

    // Nombres de los dueños.
    let mut owners = BTreeMap::new();
    for o in owner_ids {
        let info = match vol.file(o) {
            Ok(f) => {
                let names = f.file_names();
                let best = best_name(&names);
                OwnerInfo {
                    record: o,
                    sequence: f.header.sequence,
                    name: best.map(|b| b.name.clone()),
                    path: best.and_then(|b| build_path(&dirs, b.parent, &b.name)),
                }
            }
            Err(NtfsError::Io(e)) => return Err(NtfsError::Io(e)),
            Err(_) => OwnerInfo {
                record: o,
                sequence: 0,
                name: None,
                path: None,
            },
        };
        owners.insert(o, info);
    }
    obs.progress(p.total, p.total);
    Ok(Some(DeletedScan {
        records: count,
        counts,
        free_records,
        loose_extensions,
        entries,
        entries_truncated,
        problems,
        problems_truncated,
        bitmap_issue: bitmap.issue().cloned(),
        claims_truncated: live.truncated() || deleted.truncated(),
        directories_truncated,
        owners,
    }))
}

#[cfg(test)]
#[path = "deleted_tests.rs"]
mod tests;
