//! Recuperación de ficheros y directorios del árbol virtual (#26) sobre `mr_recover`, con la
//! política de salida común: cada fichero se escribe con [`crate::create_output`] (nunca el
//! origen, ni uno de sus segmentos o ficheros externos, ni un dispositivo; temporal en el mismo
//! directorio y publicación sin sustituir salvo petición explícita) y el destino nunca es ni está
//! dentro del origen.
//!
//! [`recover`] es síncrona (CLI); [`start_recover_task`] la lanza como tarea con progreso
//! (bytes de los flujos principales en `Task::progress`, detalle en [`RecoverTask::progress`]),
//! cancelación y **peticiones** de conflicto con la política `preguntar`: la tarea se detiene en
//! [`RecoverTask::pending_question`] hasta que el llamador responde con [`RecoverTask::answer`]
//! (o cancela). Sin respuesta en la llamada síncrona, se renombra.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use mr_fs_ntfs::NtfsVolume;
use mr_recover::{
    ConflictAnswer, ConflictDecision, ConflictQuestion, CreateError, NtfsContent, OutputSink,
    PendingFile, Progress, RecoverError, RecoverObserver, RecoverOptions, RecoverSummary,
    Selection, SourceInfo,
};
use mr_vfs::Vfs;

use crate::task::{Task, TaskContext, TaskState};
use crate::{ApiError, OutputFile, Result, Source};

/// Petición de recuperación: qué (selección y filtro) y cómo (opciones).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverRequest {
    pub selection: Selection,
    pub options: RecoverOptions,
}

/// Salidas con [`crate::create_output`] (ver el módulo).
pub struct ApiSink {
    src: Source,
    paths: Vec<PathBuf>,
}

impl ApiSink {
    pub fn new(src: &Source) -> Self {
        let i = src.info();
        let mut paths = Vec::new();
        for p in std::iter::once(&i.path)
            .chain(i.segments.iter())
            .chain(i.external_files.iter())
        {
            if p.as_os_str().is_empty() {
                continue;
            }
            paths.push(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        }
        ApiSink {
            src: Arc::clone(src),
            paths,
        }
    }
}

struct ApiPending(OutputFile);

impl std::io::Write for ApiPending {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

fn create_error(e: ApiError) -> CreateError {
    match e {
        ApiError::OutputExists(p) => CreateError::Exists(p),
        ApiError::Io(mr_io::IoError::Io(io)) => CreateError::Io(io),
        other => CreateError::Rejected(other.to_string()),
    }
}

impl PendingFile for ApiPending {
    fn temp_path(&self) -> &Path {
        self.0.temp_path()
    }
    fn commit(self: Box<Self>) -> std::result::Result<PathBuf, CreateError> {
        self.0.commit().map_err(create_error)
    }
}

/// Forma comparable de una ruta (minúsculas, sin `\\?\` ni separador final).
fn norm(p: &Path) -> String {
    let s = mr_recover::dest::display_path(p)
        .to_lowercase()
        .replace('/', "\\");
    s.trim_end_matches('\\').to_string()
}

impl OutputSink for ApiSink {
    fn create(
        &self,
        path: &Path,
        overwrite: bool,
    ) -> std::result::Result<Box<dyn PendingFile>, CreateError> {
        crate::create_output(&self.src, path, overwrite)
            .map(|f| Box::new(ApiPending(f)) as Box<dyn PendingFile>)
            .map_err(create_error)
    }

    fn is_source(&self, path: &Path) -> bool {
        let c = std::fs::canonicalize(path).unwrap_or_else(|_| {
            // Si no existe: la forma absoluta tal cual (un destino nuevo dentro de una ruta del
            // origen también se rechaza).
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
        });
        let c = norm(&c);
        self.paths.iter().any(|p| {
            let p = norm(p);
            c == p || c.starts_with(&format!("{p}\\"))
        })
    }

    fn source_paths(&self) -> Vec<PathBuf> {
        self.paths.clone()
    }
}

/// Datos del origen para el diario.
pub fn recover_source_info(src: &Source) -> SourceInfo {
    let i = src.info();
    SourceInfo {
        path: mr_recover::dest::display_path(&i.path),
        description: i.description.clone(),
        size: i.size_bytes,
        sector_size: i.sector_size,
        volume_start_lba: i.range.map(|r| r.0),
    }
}

fn map_error(e: RecoverError) -> ApiError {
    match e {
        RecoverError::Vfs(v) => ApiError::Vfs(v),
        RecoverError::Cancelled => ApiError::Cancelled,
        other => ApiError::InvalidArgument(other.to_string()),
    }
}

/// Comprobaciones previas que dan errores tipados (ruta de dispositivo).
fn precheck(req: &RecoverRequest) -> Result<()> {
    mr_io::path_policy::check_lexical(&req.options.destination.to_string_lossy())?;
    if let Some(j) = &req.options.journal {
        mr_io::path_policy::check_lexical(&j.to_string_lossy())?;
    }
    Ok(())
}

/// Observador sobre el contexto de una tarea (cancelación y progreso en bytes).
struct CtxObserver<'c> {
    ctx: &'c TaskContext,
    shared: Option<&'c Shared>,
}

impl RecoverObserver for CtxObserver<'_> {
    fn cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }

    fn progress(&self, p: &Progress) {
        self.ctx.set_total(p.bytes_total);
        self.ctx.set_done(p.bytes_done.min(p.bytes_total));
        if let Some(s) = self.shared {
            *s.progress.lock().unwrap_or_else(|x| x.into_inner()) = p.clone();
        }
    }

    fn conflict(&self, q: &ConflictQuestion) -> ConflictAnswer {
        let rename = ConflictAnswer {
            decision: ConflictDecision::Rename,
            apply_to_all: false,
        };
        let Some(s) = self.shared else {
            return rename;
        };
        let id = s.next.fetch_add(1, Ordering::Relaxed) + 1;
        *s.question.lock().unwrap_or_else(|x| x.into_inner()) = Some((id, q.clone()));
        let mut a = s.answer.lock().unwrap_or_else(|x| x.into_inner());
        loop {
            if let Some((aid, ans)) = *a {
                if aid == id {
                    *a = None;
                    *s.question.lock().unwrap_or_else(|x| x.into_inner()) = None;
                    return ans;
                }
            }
            if self.ctx.is_cancelled() {
                *s.question.lock().unwrap_or_else(|x| x.into_inner()) = None;
                return ConflictAnswer {
                    decision: ConflictDecision::Cancel,
                    apply_to_all: false,
                };
            }
            a =
                s.cv.wait_timeout(a, Duration::from_millis(50))
                    .unwrap_or_else(|x| x.into_inner())
                    .0;
        }
    }
}

/// Recupera la selección del árbol virtual del volumen `vol` (abierto desde `src`) de forma
/// síncrona. Errores previos (opciones, destino rechazado, diario) antes de escribir nada; los
/// errores de cada fichero quedan en el diario. Al cancelar, el resumen tiene estado
/// `cancelada` (no es un error).
pub fn recover(
    src: &Source,
    vol: &NtfsVolume,
    vfs: &Vfs,
    req: &RecoverRequest,
    ctx: &TaskContext,
) -> Result<RecoverSummary> {
    recover_observed(src, vol, vfs, req, ctx, None)
}

fn recover_observed(
    src: &Source,
    vol: &NtfsVolume,
    vfs: &Vfs,
    req: &RecoverRequest,
    ctx: &TaskContext,
    shared: Option<&Shared>,
) -> Result<RecoverSummary> {
    precheck(req)?;
    let sink = ApiSink::new(src);
    let content = NtfsContent::new(vol, req.options.fill);
    let obs = CtxObserver { ctx, shared };
    let info = recover_source_info(src);
    let r = mr_recover::recover(
        vfs,
        &content,
        &sink,
        &info,
        &req.selection,
        &req.options,
        &obs,
    )
    .map_err(map_error)?;
    if r.status == mr_recover::RunStatus::Cancelled {
        ctx.finish_cancelled_with_result();
    }
    Ok(r)
}

/// Estado compartido de una tarea de recuperación.
#[derive(Default)]
struct Shared {
    progress: Mutex<Progress>,
    question: Mutex<Option<(u64, ConflictQuestion)>>,
    answer: Mutex<Option<(u64, ConflictAnswer)>>,
    cv: Condvar,
    next: AtomicU64,
}

/// Recuperación en segundo plano.
pub struct RecoverTask {
    task: Arc<Task<RecoverSummary>>,
    shared: Arc<Shared>,
}

impl RecoverTask {
    pub fn task(&self) -> &Arc<Task<RecoverSummary>> {
        &self.task
    }

    pub fn state(&self) -> TaskState {
        self.task.state()
    }

    /// Progreso detallado (ficheros, bytes, ruta en curso, bytes no fiables, errores).
    pub fn progress(&self) -> Progress {
        self.shared
            .progress
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .clone()
    }

    /// Pregunta de conflicto pendiente (política `preguntar`) con su identificador.
    pub fn pending_question(&self) -> Option<(u64, ConflictQuestion)> {
        self.shared
            .question
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .clone()
    }

    /// Responde la pregunta `id`. `false` si no es la pendiente.
    pub fn answer(&self, id: u64, answer: ConflictAnswer) -> bool {
        let pending = self
            .shared
            .question
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .as_ref()
            .is_some_and(|(q, _)| *q == id);
        if !pending {
            return false;
        }
        *self.shared.answer.lock().unwrap_or_else(|x| x.into_inner()) = Some((id, answer));
        self.shared.cv.notify_all();
        true
    }

    pub fn cancel(&self) {
        self.task.cancel();
        self.shared.cv.notify_all();
    }

    pub fn wait(&self) -> Result<RecoverSummary> {
        self.task.wait()
    }
}

/// Lanza [`recover`] en segundo plano. Las opciones y el destino se comprueban antes de lanzar
/// (errores de dispositivo, origen y opciones al momento).
pub fn start_recover_task(
    src: Source,
    vol: Arc<NtfsVolume>,
    vfs: Arc<Vfs>,
    req: RecoverRequest,
) -> Result<RecoverTask> {
    precheck(&req)?;
    let sink = ApiSink::new(&src);
    if sink.is_source(&req.options.destination) {
        return Err(ApiError::InvalidArgument(format!(
            "el destino {} es el origen o está dentro de él: nunca se escribe en el origen",
            req.options.destination.display()
        )));
    }
    let shared = Arc::new(Shared::default());
    let sh = Arc::clone(&shared);
    let task = Task::spawn(move |ctx| recover_observed(&src, &vol, &vfs, &req, ctx, Some(&sh)))?;
    Ok(RecoverTask { task, shared })
}
