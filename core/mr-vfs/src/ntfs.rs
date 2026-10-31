//! Construcción del árbol virtual desde un volumen NTFS (INV-01 §7.2, «Fusión»).
//!
//! 1. **Vivos**: árbol de `mr-fs-ntfs` (índices `$I30` contrastados con la MFT, #22), con los
//!    enlaces duros como varias entradas que comparten registro. Una entrada que un índice vivo
//!    cita pero cuyo registro está dañado, es ilegible o es una referencia no válida (ADR-025,
//!    ADR-028) queda como `vivo` con datos `no_disponible` y [`flags::RECORD_DAMAGED`]; nunca se
//!    inventa su contenido.
//! 2. **Metadatos**: los ficheros del sistema de la raíz (registros 0 a 15, con `$Extend` y su
//!    contenido) se mueven a la carpeta sintética `[Metadatos]`.
//! 3. **Huérfanos en uso**: registros en uso no alcanzables desde la raíz, en `[Huérfanos]`,
//!    anidados bajo su directorio si este también es huérfano y, si no, agrupados por el registro
//!    padre que citan (`[Registro N]`).
//! 4. **Borrados** (#23, ADR-027): bajo su padre si el padre es un directorio vivo con la **misma
//!    secuencia** que cita `$FILE_NAME`, o un directorio borrado coherente (que a su vez se coloca
//!    igual); si el padre no existe, es ilegible o se reutilizó, en `[Borrados sin carpeta]`
//!    agrupados por el registro padre citado, con categoría `huerfano`. Los ciclos entre
//!    directorios borrados se cortan ([`flags::CYCLE_CUT`]).
//! 5. **Entradas de índice residuales** del árbol vivo (registro libre o reutilizado) que el
//!    recorrido de borrados no encontró: borradas, con nombre `residual` y datos `sin_datos`
//!    (no queda de dónde leerlos). Las que sí encontró añaden [`flags::FROM_INDEX`] a su entrada.
//!
//! **Estado de los datos** (INV-01 §7.3), sin leer datos: en vivos y huérfanos en uso, por la
//! disposición del flujo principal (cifrado, error tipado, tramos sin mapa); en borrados, por la
//! recuperabilidad conservadora de ADR-027 (`full` → `integro_no_verificable`, `partial` →
//! `parcial`, `none` → `sobrescrito` si algún clúster está en duda y `sin_datos` si no).
//!
//! **Orígenes de los borrados** ([`EntryOrigin`], #23): la MFT da [`flags::FROM_MFT`] y las
//! entradas de índice residuales (parte en uso caducada, espacio libre de un nodo, nodo libre,
//! índice de un directorio borrado) [`flags::FROM_INDEX`]; una entrada puede tener varias. El
//! nombre es `registro` si sale de un registro no en uso y `residual` si solo lo dan los índices;
//! en estas, los datos son `sin_datos` (registro reutilizado o libre) o `no_disponible` (registro
//! no interpretable). La enumeración no es exhaustiva: un origen futuro cuenta como índice.
//!
//! Cancelable entre fases y dentro de cada una; el progreso va en milésimas por fase (árbol,
//! evaluación de los vivos y borrados: total 3000).

use std::collections::{HashMap, HashSet};

use mr_fs_ntfs::attr::types;
use mr_fs_ntfs::deleted::{
    scan_deleted, DeletedEntry, DeletedStatus, EntryOrigin, OrphanReason as DeletedOrphanReason,
    ParentState, Reason, Recoverability, ScanObserver, ScanOptions,
};
use mr_fs_ntfs::dir::{DirEntry, TreeIssue, TreeObserver, TreeOptions, TreeStatus, ROOT_RECORD};
use mr_fs_ntfs::file::{SegmentKind, StreamError};
use mr_fs_ntfs::values::{Namespace, Times as NtfsTimes};
use mr_fs_ntfs::{EntryState, FileReference, NtfsError, NtfsVolume};

use crate::arena::{Limits, NewNode, Vfs, VfsBuilder};
use crate::name::{self, units};
use crate::{
    flags, Category, DataState, FsRef, NameReliability, NodeId, NodeKind, Result, Times, VfsError,
};

/// Carpeta sintética de los ficheros del sistema.
pub const METADATA_FOLDER: &str = "[Metadatos]";
/// Carpeta sintética de los registros en uso no alcanzables desde la raíz.
pub const ORPHANS_FOLDER: &str = "[Huérfanos]";
/// Carpeta sintética de los borrados cuyo padre ya no existe.
pub const LOOSE_DELETED_FOLDER: &str = "[Borrados sin carpeta]";

/// Nombre de la carpeta sintética que agrupa las entradas que citan el padre `record`.
pub fn parent_group_name(record: u64) -> String {
    format!("[Registro {record}]")
}

/// Unidades por fase del progreso.
pub const PHASE: u64 = 1000;

/// Opciones de la construcción.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtfsBuildOptions {
    pub limits: Limits,
    /// Recorrer la MFT en busca de borrados (y añadir las entradas de índice residuales).
    pub deleted: bool,
    /// Buscar registros en uso no alcanzables desde la raíz.
    pub orphans: bool,
    /// Incluir los ficheros del sistema en `[Metadatos]` (si no, se omiten).
    pub metadata: bool,
}

impl Default for NtfsBuildOptions {
    fn default() -> Self {
        NtfsBuildOptions {
            limits: Limits::default(),
            deleted: true,
            orphans: true,
            metadata: true,
        }
    }
}

/// Cancelación y progreso de la construcción.
pub trait BuildObserver {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _done: u64, _total: u64) {}
}

/// Observador que no cancela ni informa.
pub struct NoBuildObserver;

impl BuildObserver for NoBuildObserver {}

/// Traduce el progreso de una fase de `mr-fs-ntfs` al de la construcción.
struct Phase<'a> {
    obs: &'a dyn BuildObserver,
    index: u64,
}

impl Phase<'_> {
    fn report(&self, done: u64, total: u64) {
        let part = if total == 0 {
            0
        } else {
            (u128::from(done.min(total)) * u128::from(PHASE) / u128::from(total)) as u64
        };
        self.obs.progress(self.index * PHASE + part, 3 * PHASE);
    }
}

impl TreeObserver for Phase<'_> {
    fn cancelled(&self) -> bool {
        self.obs.cancelled()
    }
    fn progress(&self, done: u64, total: u64) {
        self.report(done, total);
    }
}

impl ScanObserver for Phase<'_> {
    fn cancelled(&self) -> bool {
        self.obs.cancelled()
    }
    fn progress(&self, done: u64, total: u64) {
        self.report(done, total);
    }
}

fn times(t: &NtfsTimes) -> Times {
    Times {
        created: t.created,
        modified: t.modified,
        changed: t.mft_modified,
        accessed: t.accessed,
    }
}

fn namespace(n: Namespace) -> u8 {
    match n {
        Namespace::Posix => 0,
        Namespace::Win32 => 1,
        Namespace::Dos => 2,
        Namespace::Win32AndDos => 3,
    }
}

fn fsref(r: FileReference, parent: FileReference) -> FsRef {
    FsRef {
        record: r.record,
        sequence: r.sequence,
        parent_record: parent.record,
        parent_sequence: parent.sequence,
    }
}

/// Secuencia del registro frente a la referenciada (un registro liberado lleva una más).
fn seq_matches(found: u16, referenced: u16) -> bool {
    found == referenced || found == referenced.wrapping_add(1)
}

/// Estado de los datos de un registro en uso por la disposición de su flujo principal, con los
/// indicadores de anomalías. Solo los errores de E/S son errores.
fn assess_in_use(vol: &NtfsVolume, record: u64, size: u64) -> Result<(DataState, u32)> {
    let f = match vol.file(record) {
        Ok(f) => f,
        Err(NtfsError::Io(e)) => return Err(VfsError::Ntfs(NtfsError::Io(e))),
        Err(_) => return Ok((DataState::NoDisponible, flags::RECORD_DAMAGED)),
    };
    let mut fl = 0;
    if !f.anomalies.is_empty() {
        fl |= flags::ANOMALIES;
    }
    let state = match f.stream_layout(types::DATA, "", vol.geometry()) {
        Err(StreamError::NotFound { .. }) if size == 0 => DataState::Integro,
        Err(StreamError::NotFound { .. }) => DataState::SinDatos,
        Err(StreamError::Encrypted) => DataState::Cifrado,
        Err(_) => DataState::NoDisponible,
        Ok(l) => {
            if !l.issues.is_empty() || l.segments.iter().any(|s| s.kind == SegmentKind::Unmapped) {
                DataState::Parcial
            } else {
                DataState::Integro
            }
        }
    };
    Ok((state, fl))
}

/// Indicador de origen de una fuente de un borrado. `EntryOrigin` no es exhaustiva: una fuente
/// futura cuenta como índice.
fn origin_flag(o: EntryOrigin) -> u32 {
    match o {
        EntryOrigin::MftRecord => flags::FROM_MFT,
        EntryOrigin::IndexStale
        | EntryOrigin::IndexSlack
        | EntryOrigin::IndexFreeNode
        | EntryOrigin::DeletedDirectoryIndex => flags::FROM_INDEX,
        _ => flags::FROM_INDEX,
    }
}

/// Estado de los datos de un borrado (ver el módulo).
fn deleted_state(e: &DeletedEntry) -> Option<DataState> {
    if e.is_directory {
        return None;
    }
    match e.status {
        DeletedStatus::Deleted => {}
        // Solo índice: el registro es de otro fichero o está libre sin atributos: no hay de dónde
        // leer los datos.
        DeletedStatus::RecordReused { .. } | DeletedStatus::RecordFree { .. } => {
            return Some(DataState::SinDatos)
        }
        DeletedStatus::RecordUnusable { .. } => return Some(DataState::NoDisponible),
    }
    let Some(s) = e.main_stream() else {
        return Some(DataState::SinDatos);
    };
    for r in &s.reasons {
        if let Reason::Stream(err) = r {
            return Some(if *err == StreamError::Encrypted {
                DataState::Cifrado
            } else {
                DataState::NoDisponible
            });
        }
    }
    let original = e.names.iter().map(|n| n.data_size).max().unwrap_or(0);
    if s.size == 0 && original > 0 {
        return Some(DataState::SinDatos);
    }
    Some(match s.recoverability {
        Recoverability::Full => DataState::IntegroNoVerificable,
        Recoverability::Partial => DataState::Parcial,
        Recoverability::None if s.possibly_overwritten() > 0 => DataState::Sobrescrito,
        Recoverability::None => DataState::SinDatos,
    })
}

/// Unidades exactas del nombre `lossy` (con U+FFFD) del registro: se vuelve a leer su
/// `$FILE_NAME`. Si no se encuentra, las unidades de la forma visible.
fn exact_units(vol: &NtfsVolume, record: u64, lossy: &str, parent: FileReference) -> Vec<u16> {
    if !lossy.contains(char::REPLACEMENT_CHARACTER) {
        return units(lossy);
    }
    vol.file(record)
        .ok()
        .and_then(|f| {
            let names = f.file_names_with_units();
            names
                .iter()
                .find(|(n, _)| n.name == lossy && n.parent == parent)
                .or_else(|| names.iter().find(|(n, _)| n.name == lossy))
                .map(|(_, u)| u.clone())
        })
        .unwrap_or_else(|| units(lossy))
}

/// Estado mutable de la construcción.
struct Ctx<'v> {
    vol: &'v NtfsVolume,
    b: VfsBuilder,
    /// Directorios vivos por registro: (secuencia, nodo).
    live_dirs: HashMap<u64, (u16, NodeId)>,
    /// Directorios huérfanos en uso por registro.
    orphan_dirs: HashMap<u64, (u16, NodeId)>,
    folders: HashMap<&'static str, NodeId>,
    groups: HashMap<(&'static str, u64), NodeId>,
}

impl Ctx<'_> {
    fn folder(&mut self, name: &'static str, cat: Category) -> Result<NodeId> {
        if let Some(n) = self.folders.get(name) {
            return Ok(*n);
        }
        let u = units(name);
        let id = self.b.push(NodeId::ROOT, NewNode::synthetic(&u, cat))?;
        self.folders.insert(name, id);
        Ok(id)
    }

    /// `[Registro N]` dentro de la carpeta `top`.
    fn group(&mut self, top: &'static str, record: u64) -> Result<NodeId> {
        if let Some(n) = self.groups.get(&(top, record)) {
            return Ok(*n);
        }
        let parent = self.folder(top, Category::Orphan)?;
        let u = units(&parent_group_name(record));
        let id = self
            .b
            .push(parent, NewNode::synthetic(&u, Category::Orphan))?;
        self.groups.insert((top, record), id);
        Ok(id)
    }

    /// Categoría de un hijo de `parent` que por sí mismo sería `own`: hereda `huerfano` y
    /// `metadatos` del padre.
    fn inherit(&self, parent: NodeId, own: Category) -> Category {
        match self.b.category(parent) {
            Some(c @ (Category::Orphan | Category::Metadata)) => c,
            _ => own,
        }
    }
}

/// Construye el árbol virtual del volumen. `Ok(None)` si se canceló.
pub fn build_ntfs(
    vol: &NtfsVolume,
    opts: &NtfsBuildOptions,
    obs: &dyn BuildObserver,
) -> Result<Option<Vfs>> {
    // ---- Fase 1: árbol vivo ----
    let tree_opts = TreeOptions {
        max_depth: usize::from(opts.limits.max_depth)
            .saturating_sub(4)
            .clamp(1, mr_fs_ntfs::dir::MAX_PATH_DEPTH),
        max_nodes: opts.limits.max_nodes as usize,
        orphans: opts.orphans,
    };
    let tree = vol.tree(&tree_opts, &Phase { obs, index: 0 })?;
    match tree.status {
        TreeStatus::Cancelled => return Ok(None),
        TreeStatus::Limit => {
            return Err(VfsError::LimitExceeded {
                what: "nodos",
                limit: u64::from(opts.limits.max_nodes),
            })
        }
        TreeStatus::Complete => {}
    }
    let root_entry = &tree.nodes[0].entry;
    let root = NewNode {
        name: &[],
        kind: NodeKind::Directory,
        category: Category::Live,
        name_reliability: NameReliability::Structure,
        data: None,
        flags: flags::FROM_INDEX,
        namespace: None,
        size: 0,
        allocated_size: 0,
        times: times(&root_entry.times),
        attributes: root_entry.file_attributes,
        origin: Some(fsref(root_entry.reference, root_entry.reference)),
    };
    let mut cx = Ctx {
        vol,
        b: VfsBuilder::new(opts.limits, root)?,
        live_dirs: HashMap::from([(ROOT_RECORD, (root_entry.reference.sequence, NodeId::ROOT))]),
        orphan_dirs: HashMap::new(),
        folders: HashMap::new(),
        groups: HashMap::new(),
    };
    let mut map: Vec<Option<NodeId>> = vec![None; tree.nodes.len()];
    map[0] = Some(NodeId::ROOT);
    // Entradas residuales del índice: (nodo padre, índice en el árbol).
    let mut stale: Vec<(NodeId, usize)> = Vec::new();
    // Ficheros por evaluar: (nodo, registro, tamaño).
    let mut pending: Vec<(NodeId, u64, u64)> = Vec::new();
    let mut by_record: HashMap<u64, NodeId> = HashMap::new();
    for (i, t) in tree.nodes.iter().enumerate().skip(1) {
        let Some(tp) = t.parent.and_then(|p| map[p]) else {
            continue;
        };
        let e = &t.entry;
        let to_metadata = tp == NodeId::ROOT && e.system;
        if to_metadata && !opts.metadata {
            continue;
        }
        if matches!(
            e.state,
            EntryState::NotInUse { .. } | EntryState::Reused { .. }
        ) {
            stale.push((tp, i));
            continue;
        }
        let parent = if to_metadata {
            cx.folder(METADATA_FOLDER, Category::Metadata)?
        } else {
            tp
        };
        let own = if to_metadata {
            Category::Metadata
        } else {
            Category::Live
        };
        let id = push_dir_entry(&mut cx, parent, e, own, &mut pending)?;
        map[i] = Some(id);
        by_record.entry(e.record()).or_insert(id);
        if e.is_directory && e.state.is_live() {
            cx.live_dirs
                .entry(e.record())
                .or_insert((e.reference.sequence, id));
        }
    }
    for a in &tree.anomalies {
        let node = match &a.issue {
            TreeIssue::Loop { record } | TreeIssue::Depth { record } => {
                by_record.get(record).copied()
            }
            _ => by_record
                .get(&a.directory)
                .copied()
                .or_else(|| (a.directory == ROOT_RECORD).then_some(NodeId::ROOT)),
        };
        if let Some(n) = node {
            if !matches!(a.issue, TreeIssue::Index(_)) {
                cx.b.add_flags(n, flags::DIR_INCOMPLETE);
            }
        }
        let text = format!("{}: {}", a.path, a.issue);
        cx.b.note(a.issue.code(), text, node);
    }
    // ---- Huérfanos en uso ----
    place_orphans(&mut cx, &tree.orphans, &mut pending)?;
    // ---- Fase 2: evaluación de los datos de los vivos ----
    let mut cache: HashMap<u64, (DataState, u32)> = HashMap::new();
    let phase = Phase { obs, index: 1 };
    let total = pending.len() as u64;
    for (k, (id, record, size)) in pending.iter().enumerate() {
        if k % 256 == 0 {
            if obs.cancelled() {
                return Ok(None);
            }
            phase.report(k as u64, total);
        }
        let (d, fl) = match cache.get(record) {
            Some(v) => *v,
            None => {
                let v = assess_in_use(vol, *record, *size)?;
                cache.insert(*record, v);
                v
            }
        };
        cx.b.set_data(*id, Some(d));
        cx.b.add_flags(*id, fl);
    }
    drop(cache);
    phase.report(total, total);
    // ---- Fase 3: borrados ----
    if opts.deleted {
        let scan = scan_deleted(vol, &ScanOptions::default(), &Phase { obs, index: 2 })?;
        let Some(scan) = scan else {
            return Ok(None);
        };
        if scan.entries_truncated {
            cx.b.set_partial();
            cx.b.note(
                "deleted_entries_truncated",
                "se alcanzó el máximo de entradas borradas: la lista no es completa".into(),
                None,
            );
        }
        if scan.claims_truncated {
            cx.b.note(
                "deleted_claims_truncated",
                "propietarios de clústeres incompletos: la evaluación de algunos borrados es incompleta"
                    .into(),
                None,
            );
        }
        if scan.directories_truncated {
            cx.b.note(
                "deleted_directories_truncated",
                "demasiados directorios: algunas rutas de borrados no se resuelven".into(),
                None,
            );
        }
        if let Some(b) = &scan.bitmap_issue {
            cx.b.note(b.code(), format!("$Bitmap: {b}"), None);
        }
        for p in &scan.problems {
            cx.b.note(
                p.state.code(),
                format!(
                    "registro {} {}{}",
                    p.record,
                    p.state.label(),
                    p.detail
                        .as_ref()
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default()
                ),
                None,
            );
        }
        let placed = place_deleted(&mut cx, &scan.entries, scan.claims_truncated)?;
        // Entradas residuales del árbol vivo.
        for (tp, i) in stale {
            let e = &tree.nodes[i].entry;
            if let Some(nodes) = placed.get(&e.record()) {
                for n in nodes {
                    if cx
                        .b
                        .origin(*n)
                        .is_some_and(|o| o.parent_record == e.parent.record)
                    {
                        cx.b.add_flags(*n, flags::FROM_INDEX);
                    }
                }
                continue;
            }
            push_stale(&mut cx, tp, e)?;
        }
    }
    obs.progress(3 * PHASE, 3 * PHASE);
    Ok(Some(cx.b.finish()))
}

/// Añade una entrada del árbol vivo (no residual).
fn push_dir_entry(
    cx: &mut Ctx<'_>,
    parent: NodeId,
    e: &DirEntry,
    own: Category,
    pending: &mut Vec<(NodeId, u64, u64)>,
) -> Result<NodeId> {
    let category = cx.inherit(parent, own);
    let damaged = !e.state.is_live();
    let mut fl = flags::FROM_INDEX;
    for (on, bit) in [
        (e.system, flags::SYSTEM),
        (e.name_lossy, flags::NAME_LOSSY),
        (e.times_from_index, flags::TIMES_FROM_INDEX),
        (damaged, flags::RECORD_DAMAGED),
        (e.link_count > 1, flags::HARD_LINK),
    ] {
        if on {
            fl |= bit;
        }
    }
    let kind = if e.is_directory {
        NodeKind::Directory
    } else {
        NodeKind::File
    };
    // En los ficheros con registro utilizable es provisional: se evalúa después por la
    // disposición del flujo.
    let data = (kind == NodeKind::File).then_some(DataState::NoDisponible);
    let id = cx.b.push(
        parent,
        NewNode {
            name: &e.name_units,
            kind,
            category,
            name_reliability: NameReliability::Structure,
            data,
            flags: fl,
            namespace: Some(namespace(e.namespace)),
            size: e.size,
            allocated_size: e.allocated_size,
            times: times(&e.times),
            attributes: e.file_attributes,
            origin: Some(fsref(e.reference, e.parent)),
        },
    )?;
    if kind == NodeKind::File && !damaged {
        pending.push((id, e.record(), e.size));
    }
    Ok(id)
}

/// Entrada de índice residual que el recorrido de borrados no encontró.
fn push_stale(cx: &mut Ctx<'_>, parent: NodeId, e: &DirEntry) -> Result<NodeId> {
    let category = cx.inherit(parent, Category::Deleted);
    let mut fl = flags::FROM_INDEX | flags::DELETED;
    if e.name_lossy {
        fl |= flags::NAME_LOSSY;
    }
    if e.times_from_index {
        fl |= flags::TIMES_FROM_INDEX;
    }
    if e.system {
        fl |= flags::SYSTEM;
    }
    let kind = if e.is_directory {
        NodeKind::Directory
    } else {
        NodeKind::File
    };
    cx.b.push(
        parent,
        NewNode {
            name: &e.name_units,
            kind,
            category,
            name_reliability: NameReliability::Residual,
            data: (kind == NodeKind::File).then_some(DataState::SinDatos),
            flags: fl,
            namespace: Some(namespace(e.namespace)),
            size: e.size,
            allocated_size: e.allocated_size,
            times: times(&e.times),
            attributes: e.file_attributes,
            origin: Some(fsref(e.reference, e.parent)),
        },
    )
}

/// Dónde se cuelga una entrada.
#[derive(Debug, Clone, Copy)]
enum Anchor {
    /// Nodo ya existente.
    Node(NodeId),
    /// Otra entrada del mismo grupo (directorio), que se coloca antes.
    Item(usize),
    /// Sin padre válido: carpeta sintética agrupada por el registro padre citado.
    Loose,
}

/// Orden de colocación con los padres antes que los hijos; corta los ciclos (devuelve qué
/// entradas se cortaron). `anchors` se modifica en los cortes.
fn placement_order(anchors: &mut [Anchor]) -> (Vec<usize>, Vec<bool>) {
    let n = anchors.len();
    let mut done = vec![false; n];
    let mut on_stack = vec![false; n];
    let mut cut = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for i in 0..n {
        if done[i] {
            continue;
        }
        let mut stack = vec![i];
        on_stack[i] = true;
        while let Some(&cur) = stack.last() {
            if let Anchor::Item(j) = anchors[cur] {
                if !done[j] {
                    if on_stack[j] {
                        anchors[cur] = Anchor::Loose;
                        cut[cur] = true;
                    } else {
                        on_stack[j] = true;
                        stack.push(j);
                    }
                    continue;
                }
            }
            stack.pop();
            on_stack[cur] = false;
            if !done[cur] {
                done[cur] = true;
                order.push(cur);
            }
        }
    }
    (order, cut)
}

/// Registros en uso no alcanzables desde la raíz, en `[Huérfanos]`.
fn place_orphans(
    cx: &mut Ctx<'_>,
    orphans: &[mr_fs_ntfs::Orphan],
    pending: &mut Vec<(NodeId, u64, u64)>,
) -> Result<()> {
    if orphans.is_empty() {
        return Ok(());
    }
    let dirs: HashMap<u64, usize> = orphans
        .iter()
        .enumerate()
        .filter(|(_, o)| o.is_directory)
        .map(|(i, o)| (o.record, i))
        .collect();
    let mut anchors: Vec<Anchor> = orphans
        .iter()
        .enumerate()
        .map(|(i, o)| match dirs.get(&o.parent.record) {
            Some(&j) if j != i && orphans[j].sequence == o.parent.sequence => Anchor::Item(j),
            _ => Anchor::Loose,
        })
        .collect();
    let (order, cut) = placement_order(&mut anchors);
    let mut nodes: Vec<Option<NodeId>> = vec![None; orphans.len()];
    for i in order {
        let o = &orphans[i];
        let parent = match anchors[i] {
            Anchor::Item(j) => nodes[j].ok_or(VfsError::NoSuchNode(u32::MAX))?,
            Anchor::Node(n) => n,
            Anchor::Loose => cx.group(ORPHANS_FOLDER, o.parent.record)?,
        };
        let file = match cx.vol.file(o.record) {
            Ok(f) => Some(f),
            Err(NtfsError::Io(e)) => return Err(VfsError::Ntfs(NtfsError::Io(e))),
            Err(_) => None,
        };
        let (fname, name_units) = match file.as_ref().and_then(|f| {
            f.file_names_with_units()
                .into_iter()
                .find(|(n, _)| n.name == o.name && n.parent == o.parent)
        }) {
            Some((n, u)) => (Some(n), u),
            None => (None, units(&o.name)),
        };
        let si = file.as_ref().and_then(|f| f.standard_information());
        let size = match &file {
            _ if o.is_directory => 0,
            Some(f) => f
                .stream_layout(types::DATA, "", cx.vol.geometry())
                .map_or_else(
                    |_| fname.as_ref().map_or(0, |n| n.data_size),
                    |l| l.data_size,
                ),
            None => 0,
        };
        let mut fl = flags::FROM_MFT;
        if name::is_lossy(&name_units) {
            fl |= flags::NAME_LOSSY;
        }
        if cut[i] {
            fl |= flags::CYCLE_CUT;
        }
        if file.as_ref().is_some_and(|f| f.header.link_count > 1) {
            fl |= flags::HARD_LINK;
        }
        let kind = if o.is_directory {
            NodeKind::Directory
        } else {
            NodeKind::File
        };
        let t = si
            .map(|s| times(&s.times))
            .or_else(|| fname.as_ref().map(|n| times(&n.times)))
            .unwrap_or_default();
        let attributes = si
            .map(|s| s.file_attributes)
            .or_else(|| fname.as_ref().map(|n| n.file_attributes))
            .unwrap_or(0);
        let id = cx.b.push(
            parent,
            NewNode {
                name: &name_units,
                kind,
                category: Category::Orphan,
                name_reliability: NameReliability::Structure,
                data: (kind == NodeKind::File).then_some(DataState::NoDisponible),
                flags: fl,
                namespace: fname.as_ref().map(|n| namespace(n.namespace)),
                size,
                allocated_size: 0,
                times: t,
                attributes,
                origin: Some(fsref(
                    FileReference {
                        record: o.record,
                        sequence: o.sequence,
                    },
                    o.parent,
                )),
            },
        )?;
        nodes[i] = Some(id);
        if o.is_directory {
            cx.orphan_dirs.insert(o.record, (o.sequence, id));
        } else {
            pending.push((id, o.record, size));
        }
    }
    Ok(())
}

/// Borrados del recorrido de la MFT. Devuelve los nodos creados por registro.
fn place_deleted(
    cx: &mut Ctx<'_>,
    entries: &[DeletedEntry],
    claims_truncated: bool,
) -> Result<HashMap<u64, Vec<NodeId>>> {
    let dirs: HashMap<u64, usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_directory)
        .map(|(i, e)| (e.record, i))
        .collect();
    let live = |m: &HashMap<u64, (u16, NodeId)>, p: FileReference| {
        m.get(&p.record)
            .filter(|(s, _)| *s == p.sequence)
            .map(|(_, n)| *n)
    };
    let mut anchors: Vec<Anchor> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| match e.parent_state {
            ParentState::Live => live(&cx.live_dirs, e.parent)
                .or_else(|| live(&cx.orphan_dirs, e.parent))
                .map_or(Anchor::Loose, Anchor::Node),
            ParentState::Deleted => match dirs.get(&e.parent.record) {
                Some(&j) if j != i && seq_matches(entries[j].sequence, e.parent.sequence) => {
                    Anchor::Item(j)
                }
                _ => Anchor::Loose,
            },
            ParentState::Orphan(_) => Anchor::Loose,
        })
        .collect();
    let (order, cut) = placement_order(&mut anchors);
    let mut nodes: Vec<Option<NodeId>> = vec![None; entries.len()];
    let mut placed: HashMap<u64, Vec<NodeId>> = HashMap::new();
    let mut seen: HashSet<u64> = HashSet::new();
    for i in order {
        let e = &entries[i];
        let parent = match anchors[i] {
            Anchor::Item(j) => nodes[j].ok_or(VfsError::NoSuchNode(u32::MAX))?,
            Anchor::Node(n) => n,
            Anchor::Loose => cx.group(LOOSE_DELETED_FOLDER, e.parent.record)?,
        };
        let category = cx.inherit(parent, Category::Deleted);
        let mut fl = flags::DELETED;
        // Orígenes: la MFT y las entradas de índice residuales (#23). El nombre es `registro` si
        // sale de un registro no en uso y `residual` si solo lo dan los índices.
        for o in &e.origins {
            fl |= origin_flag(*o);
        }
        let name_reliability = if e.status == DeletedStatus::Deleted && e.from_mft() {
            NameReliability::Record
        } else {
            NameReliability::Residual
        };
        if matches!(
            e.parent_state,
            ParentState::Orphan(DeletedOrphanReason::SequenceMismatch { .. })
        ) {
            fl |= flags::PARENT_REUSED;
        }
        if cut[i] {
            fl |= flags::CYCLE_CUT;
        }
        if !e.anomalies.is_empty() {
            fl |= flags::ANOMALIES;
        }
        if claims_truncated || e.streams.iter().any(|s| s.ranges_truncated) {
            fl |= flags::ASSESSMENT_TRUNCATED;
        }
        if e.names
            .iter()
            .filter(|n| n.namespace != Namespace::Dos)
            .count()
            > 1
        {
            fl |= flags::HARD_LINK;
        }
        // Solo se vuelve a leer el registro si el nombre sale de él (en una entrada solo residual
        // el registro es de otro fichero o no es interpretable).
        let name_units = if e.status == DeletedStatus::Deleted {
            exact_units(cx.vol, e.record, &e.name, e.parent)
        } else {
            units(&e.name)
        };
        if name::is_lossy(&name_units) {
            fl |= flags::NAME_LOSSY;
        }
        let attributes = e
            .names
            .iter()
            .find(|n| n.name == e.name && n.namespace == e.namespace)
            .map_or(0, |n| n.file_attributes);
        let kind = if e.is_directory {
            NodeKind::Directory
        } else {
            NodeKind::File
        };
        let id = cx.b.push(
            parent,
            NewNode {
                name: &name_units,
                kind,
                category,
                name_reliability,
                data: deleted_state(e),
                flags: fl,
                namespace: Some(namespace(e.namespace)),
                size: e.size,
                allocated_size: e.allocated_size,
                times: times(&e.times),
                attributes,
                origin: Some(fsref(
                    FileReference {
                        record: e.record,
                        sequence: e.sequence,
                    },
                    e.parent,
                )),
            },
        )?;
        nodes[i] = Some(id);
        placed.entry(e.record).or_default().push(id);
        if !seen.insert(e.record) {
            cx.b.note(
                "deleted_duplicate_record",
                format!("el registro {} aparece más de una vez", e.record),
                Some(id),
            );
        }
    }
    Ok(placed)
}
