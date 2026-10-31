//! Recuperación (#26, ampliación compatible de la ABI 3): tarea que copia una selección del árbol
//! virtual a una carpeta de destino con el diario JSON Lines, progreso detallado, preguntas de
//! conflicto que el llamador responde por sondeo (sin llamadas de vuelta, como el resto de la
//! ABI) e informe final. Opciones en una estructura versionada por `struct_size`.

use std::path::PathBuf;
use std::sync::Arc;

use mr_api::mr_recover::{
    ConflictAnswer, ConflictDecision, ConflictPolicy, RecoverOptions, RecoverSummary, RunStatus,
    Selection, SplitPolicy, DEFAULT_FILL,
};
use mr_api::mr_vfs::filter::{Filter, NameRules};
use mr_api::mr_vfs::{Category, DataState, NodeId};
use mr_api::{RecoverRequest, RecoverTask};

use super::{
    api_fail, bad_task, check_struct_size, fail, guard, insert, read_utf8, settle, with_entry,
    write_out, write_str, Entry, Outcome, MR_ERR_INVALID_ARGUMENT, MR_ERR_INVALID_HANDLE,
    MR_ERR_NOT_FOUND, MR_OK,
};

/// Tarea de recuperación con su resultado.
pub(crate) struct RecoverEntry {
    pub(crate) task: RecoverTask,
    outcome: Outcome<RecoverSummary>,
}

impl RecoverEntry {
    pub(crate) fn state_progress(&self) -> (mr_api::task::TaskState, (u64, u64)) {
        (self.task.state(), self.task.task().progress())
    }
}

pub const MR_RECOVER_CONFLICT_ASK: u32 = 1;
pub const MR_RECOVER_CONFLICT_RENAME: u32 = 2;
pub const MR_RECOVER_CONFLICT_SKIP: u32 = 3;
pub const MR_RECOVER_CONFLICT_SKIP_IDENTICAL: u32 = 4;
pub const MR_RECOVER_CONFLICT_OVERWRITE: u32 = 5;
/// Respuesta a una pregunta: cancelar la recuperación.
pub const MR_RECOVER_ANSWER_CANCEL: u32 = 6;

pub const MR_RECOVER_INCLUDE_REALLOCATED_BYTES: u32 = 1;
pub const MR_RECOVER_NO_ALTERNATE_STREAMS: u32 = 2;
pub const MR_RECOVER_NO_TIMES: u32 = 4;
pub const MR_RECOVER_NO_ATTRIBUTES: u32 = 8;
pub const MR_RECOVER_NO_MARK_INCOMPLETE: u32 = 16;
pub const MR_RECOVER_CREATE_EMPTY_DIRS: u32 = 32;
/// Confirmación explícita de `MR_RECOVER_CONFLICT_OVERWRITE`.
pub const MR_RECOVER_CONFIRM_OVERWRITE: u32 = 64;
/// Solo entradas borradas (filtro).
pub const MR_RECOVER_DELETED_ONLY: u32 = 128;

/// `split_size`: nunca dividir.
pub const MR_RECOVER_SPLIT_NEVER: u64 = u64::MAX;

pub const MR_RECOVER_PROGRESS_QUESTION: u32 = 1;

pub const MR_RECOVER_QUESTION_DIRECTORY: u32 = 1;
pub const MR_RECOVER_QUESTION_DELETED: u32 = 2;
pub const MR_RECOVER_QUESTION_EXISTING_IS_DIR: u32 = 4;
pub const MR_RECOVER_QUESTION_TEXT_REQUESTED: u32 = 0;
pub const MR_RECOVER_QUESTION_TEXT_EXISTING: u32 = 1;

pub const MR_RECOVER_STATUS_COMPLETE: u32 = 1;
pub const MR_RECOVER_STATUS_CANCELLED: u32 = 2;
pub const MR_RECOVER_STATUS_FAILED: u32 = 3;

pub const MR_RECOVER_CAP_NAMED_STREAMS: u32 = 1;
pub const MR_RECOVER_CAP_CASE_SENSITIVE: u32 = 2;
pub const MR_RECOVER_CAP_MAX_FILE_SIZE: u32 = 4;

pub const MR_RECOVER_TEXT_JOURNAL: u32 = 0;
pub const MR_RECOVER_TEXT_DESTINATION: u32 = 1;
pub const MR_RECOVER_TEXT_REASON: u32 = 2;

/// Opciones (48 bytes). Con `options` nulo o todo a 0 salvo `struct_size`: renombrar,
/// estados por defecto, categorías vivo, borrado y huérfano, división automática, `BAD!`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrRecoverOptions {
    pub struct_size: u32,
    /// `MR_RECOVER_*` (indicadores).
    pub flags: u32,
    /// `MR_RECOVER_CONFLICT_*` (0 = renombrar).
    pub conflict: u32,
    /// Estados que se escriben: máscara de `1 << MR_VFS_DATA_*` (0 = por defecto: íntegro,
    /// íntegro no verificable y parcial).
    pub write_states: u32,
    /// Filtro de categorías: máscara de `1 << MR_VFS_CAT_*` (0 = vivo, borrado y huérfano).
    pub categories: u32,
    /// Filtro de estados de los datos (0 = todos).
    pub data_states: u32,
    /// 0 = automática (FAT32), `MR_RECOVER_SPLIT_NEVER` o el tamaño de cada parte (≥ 4096).
    pub split_size: u64,
    /// Patrón de relleno (todo 0 = `BAD!`; regla de ADR-021).
    pub fill: [u8; 4],
    /// Renombrados máximos de un mismo nombre (0 = por defecto, 1000).
    pub rename_limit: u32,
    pub reserved: u64,
}

/// Progreso (56 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrRecoverProgress {
    pub struct_size: u32,
    /// `MR_RECOVER_PROGRESS_QUESTION` si hay una pregunta pendiente.
    pub flags: u32,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub untrusted_bytes: u64,
    pub errors: u64,
}

/// Pregunta de conflicto pendiente (40 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrRecoverQuestion {
    pub struct_size: u32,
    /// `MR_RECOVER_QUESTION_*`.
    pub flags: u32,
    /// Identificador que se pasa a `mr_recover_answer`.
    pub id: u64,
    pub node: u32,
    pub reserved: u32,
    /// Tamaño del existente (`UINT64_MAX` si es un directorio o se desconoce).
    pub existing_size: u64,
    pub new_size: u64,
}

/// Informe final (200 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrRecoverReport {
    pub struct_size: u32,
    /// `MR_RECOVER_STATUS_*`.
    pub status: u32,
    pub files_planned: u64,
    pub dirs_planned: u64,
    pub files_written: u64,
    pub dirs_created: u64,
    pub dirs_merged: u64,
    pub bytes_written: u64,
    pub untrusted_bytes: u64,
    pub filled_bytes: u64,
    pub written_integro: u64,
    pub written_integro_no_verificable: u64,
    pub written_parcial: u64,
    pub written_sobrescrito: u64,
    /// Escritos en otros estados (`sin_datos`, `no_disponible`, `cifrado` incluidos con opción).
    pub written_other: u64,
    pub skipped_state: u64,
    pub skipped_conflict: u64,
    pub skipped_identical: u64,
    pub unsupported: u64,
    pub errors: u64,
    pub renamed: u64,
    pub long_paths: u64,
    pub marked_incomplete: u64,
    pub cancelled_files: u64,
    /// `MR_RECOVER_CAP_*` del destino.
    pub capabilities: u32,
    /// Código de salida equivalente de la CLI (0 íntegro, 3 algo no íntegro, 1 errores).
    pub exit_code: u32,
    /// Con `MR_RECOVER_CAP_MAX_FILE_SIZE`.
    pub max_file_size: u64,
}

fn options_of(
    o: &MrRecoverOptions,
    dest: PathBuf,
    journal: Option<PathBuf>,
) -> Result<(RecoverOptions, u32, u32, bool), i32> {
    let known = MR_RECOVER_INCLUDE_REALLOCATED_BYTES
        | MR_RECOVER_NO_ALTERNATE_STREAMS
        | MR_RECOVER_NO_TIMES
        | MR_RECOVER_NO_ATTRIBUTES
        | MR_RECOVER_NO_MARK_INCOMPLETE
        | MR_RECOVER_CREATE_EMPTY_DIRS
        | MR_RECOVER_CONFIRM_OVERWRITE
        | MR_RECOVER_DELETED_ONLY;
    if o.flags & !known != 0 || o.reserved != 0 {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "opciones de recuperación no válidas (indicadores desconocidos o reservado no nulo)",
        ));
    }
    let conflict = match o.conflict {
        0 | MR_RECOVER_CONFLICT_RENAME => ConflictPolicy::Rename,
        MR_RECOVER_CONFLICT_ASK => ConflictPolicy::Ask,
        MR_RECOVER_CONFLICT_SKIP => ConflictPolicy::Skip,
        MR_RECOVER_CONFLICT_SKIP_IDENTICAL => ConflictPolicy::SkipIfIdentical,
        MR_RECOVER_CONFLICT_OVERWRITE => ConflictPolicy::Overwrite,
        c => {
            return Err(fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("política de conflictos no válida: {c}"),
            ))
        }
    };
    let mut r = RecoverOptions::new(dest);
    r.journal = journal;
    r.conflict = conflict;
    r.overwrite_confirmed = o.flags & MR_RECOVER_CONFIRM_OVERWRITE != 0;
    if o.write_states != 0 {
        r.write_states = o.write_states;
    }
    r.include_reallocated_bytes = o.flags & MR_RECOVER_INCLUDE_REALLOCATED_BYTES != 0;
    if o.fill != [0; 4] {
        r.fill = o.fill;
    } else {
        r.fill = DEFAULT_FILL;
    }
    r.alternate_streams = o.flags & MR_RECOVER_NO_ALTERNATE_STREAMS == 0;
    r.times = o.flags & MR_RECOVER_NO_TIMES == 0;
    r.attributes = o.flags & MR_RECOVER_NO_ATTRIBUTES == 0;
    r.mark_incomplete = o.flags & MR_RECOVER_NO_MARK_INCOMPLETE == 0;
    r.create_empty_dirs = o.flags & MR_RECOVER_CREATE_EMPTY_DIRS != 0;
    r.split = match o.split_size {
        0 => SplitPolicy::Auto,
        MR_RECOVER_SPLIT_NEVER => SplitPolicy::Never,
        n => SplitPolicy::Size(n),
    };
    if o.rename_limit != 0 {
        r.rename_limit = o.rename_limit;
    }
    let categories = if o.categories == 0 {
        Category::Live.bit() | Category::Deleted.bit() | Category::Orphan.bit()
    } else {
        o.categories
    };
    Ok((
        r,
        categories,
        o.data_states,
        o.flags & MR_RECOVER_DELETED_ONLY != 0,
    ))
}

/// Lanza la recuperación de los nodos `nodes` (identificadores del árbol `vfs`, construido desde
/// el volumen `volume`) en la carpeta `dest`. `masks`: reglas de nombre UTF-8 (puede ser vacía);
/// `journal`: diario (vacío = `<dest>/MemRescue-registro/...`). Opciones, destino (nunca el
/// origen, sus segmentos o ficheros externos, ni un dispositivo: `MR_ERR_PATH_REJECTED`) y nodos se
/// comprueban antes de lanzar.
///
/// # Safety
/// `nodes` con `node_count` elementos; cadenas con su longitud (o nulas con 0); `options` nulo o
/// con `struct_size` escrito; `out_task` válido.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mr_recover_start(
    volume_h: u64,
    vfs_h: u64,
    nodes: *const u32,
    node_count: usize,
    options: *const MrRecoverOptions,
    dest: *const u8,
    dest_len: usize,
    masks: *const u8,
    masks_len: usize,
    journal: *const u8,
    journal_len: usize,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() || (nodes.is_null() && node_count > 0) {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero nulo");
        }
        if node_count == 0 || node_count > 1 << 20 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "la selección debe tener entre 1 y 1048576 nodos",
            );
        }
        let o = if options.is_null() {
            MrRecoverOptions::default()
        } else {
            // SAFETY: no nulo; se lee primero solo `struct_size`.
            let size = unsafe { options.cast::<u32>().read_unaligned() };
            let expected = std::mem::size_of::<MrRecoverOptions>();
            if size as usize != expected {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    format!(
                        "tamaño de mr_recover_options no válido: {size} (se esperaba {expected})"
                    ),
                );
            }
            // SAFETY: el tamaño declarado coincide con el de la estructura.
            unsafe { options.read_unaligned() }
        };
        // SAFETY: contrato del llamador.
        let (d, m, j) = match unsafe {
            (
                read_utf8(dest, dest_len),
                read_utf8(masks, masks_len),
                read_utf8(journal, journal_len),
            )
        } {
            (Ok(d), Ok(m), Ok(j)) => (d, m, j),
            (Err(c), _, _) | (_, Err(c), _) | (_, _, Err(c)) => return c,
        };
        if d.is_empty() {
            return fail(MR_ERR_INVALID_ARGUMENT, "destino vacío");
        }
        let jp = (!j.is_empty()).then(|| PathBuf::from(j));
        let (opts, categories, data_states, deleted_only) =
            match options_of(&o, PathBuf::from(d), jp) {
                Ok(x) => x,
                Err(c) => return c,
            };
        if let Err(e) = NameRules::parse(m) {
            return api_fail(&mr_api::ApiError::Vfs(e));
        }
        // SAFETY: `nodes` tiene `node_count` elementos (comprobado no nulo).
        let ids: Vec<NodeId> = unsafe { std::slice::from_raw_parts(nodes, node_count) }
            .iter()
            .map(|n| NodeId(*n))
            .collect();
        let vfs = match with_entry(vfs_h, |e| match e {
            Entry::Vfs(v) => Some(Arc::clone(v)),
            _ => None,
        }) {
            Some(Some(v)) => v,
            _ => {
                return fail(
                    MR_ERR_INVALID_HANDLE,
                    format!("handle de árbol virtual no válido: {vfs_h}"),
                )
            }
        };
        if let Some(bad) = ids.iter().find(|n| !vfs.contains(**n)) {
            return super::index_error("nodo", u64::from(bad.0));
        }
        let (src, vol) = match super::ntfs::volume_with_source(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        let req = RecoverRequest {
            selection: Selection {
                nodes: ids,
                filter: Filter {
                    categories,
                    data_states,
                    deleted_only,
                    files_only: true,
                    ..Filter::default()
                },
                masks: m.to_string(),
            },
            options: opts,
        };
        if let Err(e) = req.selection.filter.validate() {
            return api_fail(&mr_api::ApiError::Vfs(e));
        }
        if req.options.conflict == ConflictPolicy::Overwrite && !req.options.overwrite_confirmed {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "MR_RECOVER_CONFLICT_OVERWRITE exige MR_RECOVER_CONFIRM_OVERWRITE",
            );
        }
        let f = req.options.fill;
        if f.iter().all(|b| *b == f[0]) {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "el patrón de relleno no puede tener sus cuatro bytes iguales (ADR-021)",
            );
        }
        match mr_api::start_recover_task(src, vol, vfs, req) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::Recover(Box::new(RecoverEntry {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

fn with_recover<R>(h: u64, f: impl FnOnce(&mut RecoverEntry) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::Recover(r) => Some(f(r)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(bad_task(h)),
    }
}

/// Progreso detallado (también con la tarea terminada).
///
/// # Safety
/// `out` debe apuntar a un `MrRecoverProgress` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_progress(task: u64, out: *mut MrRecoverProgress) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_recover_progress") } {
            return c;
        }
        match with_recover(task, |r| {
            let p = r.task.progress();
            MrRecoverProgress {
                struct_size: std::mem::size_of::<MrRecoverProgress>() as u32,
                flags: if r.task.pending_question().is_some() {
                    MR_RECOVER_PROGRESS_QUESTION
                } else {
                    0
                },
                files_done: p.files_done,
                files_total: p.files_total,
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                untrusted_bytes: p.untrusted_bytes,
                errors: p.errors,
            }
        }) {
            // SAFETY: `out` comprobado.
            Ok(p) => unsafe { write_out(out, p) },
            Err(c) => c,
        }
    })
}

/// Ruta relativa en curso (UTF-8; vacía al terminar).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_current(
    task: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| match with_recover(task, |r| r.task.progress().current) {
        // SAFETY: contrato del llamador.
        Ok(s) => unsafe { write_str(&s, buf, cap, out_len) },
        Err(c) => c,
    })
}

/// Pregunta de conflicto pendiente. Ninguna → `MR_ERR_NOT_FOUND`.
///
/// # Safety
/// `out` debe apuntar a un `MrRecoverQuestion` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_question(task: u64, out: *mut MrRecoverQuestion) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_recover_question") } {
            return c;
        }
        match with_recover(task, |r| r.task.pending_question()) {
            Ok(Some((id, q))) => {
                let mut flags = 0;
                if q.directory {
                    flags |= MR_RECOVER_QUESTION_DIRECTORY;
                }
                if q.deleted {
                    flags |= MR_RECOVER_QUESTION_DELETED;
                }
                if q.existing_is_dir {
                    flags |= MR_RECOVER_QUESTION_EXISTING_IS_DIR;
                }
                let v = MrRecoverQuestion {
                    struct_size: std::mem::size_of::<MrRecoverQuestion>() as u32,
                    flags,
                    id,
                    node: q.node.0,
                    reserved: 0,
                    existing_size: q.existing_size.unwrap_or(u64::MAX),
                    new_size: q.new_size,
                };
                // SAFETY: `out` comprobado.
                unsafe { write_out(out, v) }
            }
            Ok(None) => fail(MR_ERR_NOT_FOUND, "no hay ninguna pregunta pendiente"),
            Err(c) => c,
        }
    })
}

/// Texto de la pregunta pendiente: `MR_RECOVER_QUESTION_TEXT_REQUESTED` (ruta relativa pedida en
/// el destino) o `_EXISTING` (ruta del objeto que ya existe).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_question_text(
    task: u64,
    id: u64,
    which: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let q = match with_recover(task, |r| r.task.pending_question()) {
            Ok(Some((qid, q))) if qid == id => q,
            Ok(_) => return fail(MR_ERR_NOT_FOUND, "esa pregunta ya no está pendiente"),
            Err(c) => return c,
        };
        let s = match which {
            MR_RECOVER_QUESTION_TEXT_REQUESTED => q.requested,
            MR_RECOVER_QUESTION_TEXT_EXISTING => q.existing.to_string_lossy().into_owned(),
            _ => return fail(MR_ERR_INVALID_ARGUMENT, "texto de pregunta no válido"),
        };
        // SAFETY: contrato del llamador.
        unsafe { write_str(&s, buf, cap, out_len) }
    })
}

/// Responde la pregunta `id`: `MR_RECOVER_CONFLICT_RENAME`, `_SKIP`, `_SKIP_IDENTICAL`,
/// `_OVERWRITE` (la respuesta es la confirmación explícita; en directorios, fusionar) o
/// `MR_RECOVER_ANSWER_CANCEL`. `apply_to_all` ≠ 0: la misma decisión para los conflictos
/// siguientes del mismo tipo. Una pregunta que ya no está pendiente → `MR_ERR_NOT_FOUND`.
#[no_mangle]
pub extern "C" fn mr_recover_answer(task: u64, id: u64, decision: u32, apply_to_all: u32) -> i32 {
    guard(|| {
        let d = match decision {
            MR_RECOVER_CONFLICT_RENAME => ConflictDecision::Rename,
            MR_RECOVER_CONFLICT_SKIP => ConflictDecision::Skip,
            MR_RECOVER_CONFLICT_SKIP_IDENTICAL => ConflictDecision::SkipIfIdentical,
            MR_RECOVER_CONFLICT_OVERWRITE => ConflictDecision::Overwrite,
            MR_RECOVER_ANSWER_CANCEL => ConflictDecision::Cancel,
            _ => return fail(MR_ERR_INVALID_ARGUMENT, "decisión no válida"),
        };
        if apply_to_all > 1 {
            return fail(MR_ERR_INVALID_ARGUMENT, "apply_to_all debe ser 0 o 1");
        }
        match with_recover(task, |r| {
            r.task.answer(
                id,
                ConflictAnswer {
                    decision: d,
                    apply_to_all: apply_to_all == 1,
                },
            )
        }) {
            Ok(true) => MR_OK,
            Ok(false) => fail(MR_ERR_NOT_FOUND, "esa pregunta ya no está pendiente"),
            Err(c) => c,
        }
    })
}

fn settled(task: u64) -> Result<RecoverSummary, i32> {
    let r = with_entry(task, |e| match e {
        Entry::Recover(r) => {
            let t = Arc::clone(r.task.task());
            settle(&t, &mut r.outcome).map(|()| match &r.outcome {
                Some(Ok(s)) => s.clone(),
                _ => unreachable!("settle garantiza el resultado"),
            })
        }
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es una recuperación"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(s)) => Ok(s),
    }
}

/// Informe de una recuperación terminada (también cancelada: lo escrito hasta entonces).
///
/// # Safety
/// `out` debe apuntar a un `MrRecoverReport` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_result(task: u64, out: *mut MrRecoverReport) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_recover_report") } {
            return c;
        }
        let s = match settled(task) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let t = &s.totals;
        let mut caps = 0;
        if s.capabilities.named_streams {
            caps |= MR_RECOVER_CAP_NAMED_STREAMS;
        }
        if s.capabilities.case_sensitive {
            caps |= MR_RECOVER_CAP_CASE_SENSITIVE;
        }
        if s.capabilities.max_file_size.is_some() {
            caps |= MR_RECOVER_CAP_MAX_FILE_SIZE;
        }
        let main = t.written(DataState::Integro)
            + t.written(DataState::IntegroNoVerificable)
            + t.written(DataState::Parcial)
            + t.written(DataState::Sobrescrito);
        let r = MrRecoverReport {
            struct_size: std::mem::size_of::<MrRecoverReport>() as u32,
            status: match s.status {
                RunStatus::Complete => MR_RECOVER_STATUS_COMPLETE,
                RunStatus::Cancelled => MR_RECOVER_STATUS_CANCELLED,
                RunStatus::Failed => MR_RECOVER_STATUS_FAILED,
            },
            files_planned: t.files_planned,
            dirs_planned: t.dirs_planned,
            files_written: t.files_written,
            dirs_created: t.dirs_created,
            dirs_merged: t.dirs_merged,
            bytes_written: t.bytes_written,
            untrusted_bytes: t.untrusted_bytes,
            filled_bytes: t.filled_bytes,
            written_integro: t.written(DataState::Integro),
            written_integro_no_verificable: t.written(DataState::IntegroNoVerificable),
            written_parcial: t.written(DataState::Parcial),
            written_sobrescrito: t.written(DataState::Sobrescrito),
            written_other: t.files_written - main,
            skipped_state: t.skipped_state,
            skipped_conflict: t.skipped_conflict,
            skipped_identical: t.skipped_identical,
            unsupported: t.unsupported,
            errors: t.errors,
            renamed: t.renamed,
            long_paths: t.long_paths,
            marked_incomplete: t.marked_incomplete,
            cancelled_files: t.cancelled_files,
            capabilities: caps,
            exit_code: s.exit_code() as u32,
            max_file_size: s.capabilities.max_file_size.unwrap_or(0),
        };
        // SAFETY: `out` comprobado.
        unsafe { write_out(out, r) }
    })
}

/// Texto del informe: `MR_RECOVER_TEXT_JOURNAL` (ruta del diario), `_DESTINATION` (raíz) o
/// `_REASON` (motivo de una recuperación fallida; vacío si no lo hay).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_recover_result_text(
    task: u64,
    which: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let s = match settled(task) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let t = match which {
            MR_RECOVER_TEXT_JOURNAL => s.journal.to_string_lossy().into_owned(),
            MR_RECOVER_TEXT_DESTINATION => s.destination.clone(),
            MR_RECOVER_TEXT_REASON => s.reason.clone().unwrap_or_default(),
            _ => return fail(MR_ERR_INVALID_ARGUMENT, "texto de informe no válido"),
        };
        // SAFETY: contrato del llamador.
        unsafe { write_str(&t, buf, cap, out_len) }
    })
}
