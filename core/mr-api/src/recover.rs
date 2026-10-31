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
///
/// **Destino frente al origen** ([`ApiSink::destination_problem`], también en
/// [`OutputSink::is_source`]), con cierre ante la duda como `check_output_path`: la ruta se hace
/// absoluta y se resuelve por su **antecesor existente más profundo** (forma canónica de ese
/// antecesor más los componentes que aún no existen). Así `imagen\x\y` se compara con la forma
/// canónica de `imagen` aunque `canonicalize` falle (un componente intermedio es un fichero) y
/// aunque la ruta pedida use otra forma del mismo directorio (nombre corto 8.3, mayúsculas,
/// `.`/`..`, `\\?\`). Si ningún antecesor se puede resolver, el destino se rechaza. Cada ruta del
/// origen se guarda en su forma canónica y en su forma absoluta (si no se puede canonizar, solo la
/// absoluta).
pub struct ApiSink {
    src: Source,
    paths: Vec<PathBuf>,
}

/// Forma resuelta de una ruta para compararla con el origen: forma canónica del antecesor existente
/// más profundo seguida de los componentes que no existen, y si ese antecesor es un fichero con
/// componentes por debajo (la ruta estaría «dentro» de un fichero). `None` si ningún antecesor se
/// puede resolver.
fn resolve_existing_prefix(path: &Path) -> Option<(PathBuf, bool)> {
    let abs = std::path::absolute(path).ok()?;
    let mut base = abs.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&base) {
            let inside_file = !rest.is_empty() && !c.is_dir();
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return Some((out, inside_file));
        }
        let name = base.file_name()?.to_os_string();
        rest.push(name);
        if !base.pop() || base.as_os_str().is_empty() {
            return None;
        }
    }
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
            if let Ok(c) = std::fs::canonicalize(p) {
                paths.push(c);
            }
            if let Ok(a) = std::path::absolute(p) {
                paths.push(a);
            }
        }
        ApiSink {
            src: Arc::clone(src),
            paths,
        }
    }

    fn matches_source(&self, resolved: &Path) -> bool {
        let c = norm(resolved);
        self.paths.iter().any(|p| {
            let p = norm(p);
            c == p || c.starts_with(&format!("{p}\\"))
        })
    }

    /// Motivo por el que `dest` no puede ser un destino de recuperación, o `None` si puede: es o
    /// está dentro del origen, de uno de sus segmentos o de sus ficheros externos; está dentro de
    /// un fichero; o no se puede verificar (ningún antecesor resoluble).
    pub fn destination_problem(&self, dest: &Path) -> Option<String> {
        let Some((resolved, inside_file)) = resolve_existing_prefix(dest) else {
            return Some(format!(
                "no se puede verificar que el destino {} no sea el origen (ninguna parte de la \
                 ruta se puede resolver): se rechaza",
                dest.display()
            ));
        };
        if self.matches_source(&resolved) || self.matches_source(dest) {
            return Some(format!(
                "el destino {} es el origen o está dentro de él (o de uno de sus segmentos o \
                 ficheros externos): nunca se escribe en el origen",
                dest.display()
            ));
        }
        if inside_file {
            return Some(format!(
                "el destino {} está dentro de un fichero, no de una carpeta",
                dest.display()
            ));
        }
        None
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

    /// Cierre ante la duda: una ruta que no se puede resolver cuenta como origen.
    fn is_source(&self, path: &Path) -> bool {
        match resolve_existing_prefix(path) {
            Some((resolved, _)) => self.matches_source(&resolved) || self.matches_source(path),
            None => true,
        }
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
    if let Some(problem) = sink.destination_problem(&req.options.destination) {
        return Err(ApiError::InvalidArgument(problem));
    }
    if let Some(j) = &req.options.journal {
        if sink.is_source(j) {
            return Err(ApiError::InvalidArgument(format!(
                "el diario {} sería el origen o no se puede verificar: nunca se escribe en el origen",
                j.display()
            )));
        }
    }
    let shared = Arc::new(Shared::default());
    let sh = Arc::clone(&shared);
    let task = Task::spawn(move |ctx| recover_observed(&src, &vol, &vfs, &req, ctx, Some(&sh)))?;
    Ok(RecoverTask { task, shared })
}

#[cfg(test)]
mod tests {
    //! Regresión (CI de #26): el destino se comprobaba con la forma léxica de la ruta cuando
    //! `canonicalize` fallaba (un componente intermedio es la imagen), así que `imagen\x` escrito
    //! con otra forma del mismo directorio (nombre corto 8.3 del directorio temporal del ejecutor,
    //! `RUNNER~1`) no coincidía con la forma canónica del origen y se aceptaba. Estas pruebas no
    //! dependen de dónde esté el directorio temporal: usan variantes textuales de la misma ruta.
    use super::*;
    use crate::task::TaskContext;
    use mr_fs_ntfs::testvol::tree_volume;
    use mr_recover::Selection;
    use mr_vfs::{NodeId, NtfsBuildOptions};

    /// Primera letra de unidad que no existe (para una ruta cuyo antecesor no se puede resolver).
    fn missing_drive() -> Option<PathBuf> {
        (b'D'..=b'Z')
            .rev()
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .find(|p| !p.exists())
    }

    /// Forma corta 8.3 de una ruta, si el sistema la tiene (solo Windows; `None` si no).
    fn short_form(p: &Path) -> Option<PathBuf> {
        #[cfg(not(windows))]
        {
            let _ = p;
            return None;
        }
        #[cfg(windows)]
        let o = {
            use std::os::windows::process::CommandExt;
            std::process::Command::new("cmd")
                .raw_arg(format!("/c for %I in (\"{}\") do @echo %~sI", p.display()))
                .output()
                .ok()?
        };
        if !o.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty() && s != p.display().to_string()).then(|| PathBuf::from(s))
    }

    #[test]
    fn destination_inside_the_source_is_always_refused_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("arbol.img");
        std::fs::write(&img, tree_volume(true).image).unwrap();
        let src = crate::open_source(&img, 0).unwrap();
        let sink = ApiSink::new(&src);
        let upper = PathBuf::from(img.to_string_lossy().to_uppercase());
        let dotted = dir
            .path()
            .join(".")
            .join("sub")
            .join("..")
            .join("arbol.img");
        let verbatim = PathBuf::from(format!(
            r"\\?\{}",
            std::path::absolute(&img).unwrap().display()
        ));
        let mut bad = vec![
            img.clone(),
            img.join("x"),
            img.join("x").join("y"),
            upper.join("x"),
            dotted.join("x"),
            verbatim.join("x"),
        ];
        if let Some(short) = short_form(dir.path()) {
            bad.push(short.join("arbol.img").join("x"));
        }
        for d in &bad {
            assert!(sink.destination_problem(d).is_some(), "{d:?}");
            assert!(sink.is_source(d) || d == &img || sink.destination_problem(d).is_some());
        }
        // Sin nada que lo resuelva: se rechaza (cierre ante la duda).
        if let Some(drive) = missing_drive() {
            let d = drive.join("x").join("y");
            assert!(sink.destination_problem(&d).is_some(), "{d:?}");
            assert!(sink.is_source(&d));
        }
        // Un destino normal (aunque no exista todavía) se admite.
        assert_eq!(
            sink.destination_problem(&dir.path().join("nuevo").join("dir")),
            None
        );
        assert!(!sink.is_source(&dir.path().join("nuevo")));

        // `start_recover_task` rechaza todos antes de lanzar la tarea.
        let vol = Arc::new(crate::ntfs_open(&src).unwrap());
        let vfs =
            crate::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
                .unwrap();
        for d in &bad {
            let req = RecoverRequest {
                selection: Selection {
                    nodes: vec![NodeId::ROOT],
                    ..Selection::default()
                },
                options: RecoverOptions::new(d),
            };
            assert!(
                start_recover_task(Arc::clone(&src), Arc::clone(&vol), Arc::clone(&vfs), req)
                    .is_err(),
                "{d:?}"
            );
        }
        assert_eq!(std::fs::read(&img).unwrap(), tree_volume(true).image);
    }

    #[test]
    fn segments_of_a_split_image_are_refused_too() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = dir.path().join("disco.001");
        let s2 = dir.path().join("disco.002");
        std::fs::write(&s1, vec![0u8; 4096]).unwrap();
        std::fs::write(&s2, vec![0u8; 4096]).unwrap();
        let src = crate::open_split_source(&s1, crate::OpenOptions::default()).unwrap();
        let sink = ApiSink::new(&src);
        for d in [s2.clone(), s2.join("x"), s1.join("x").join("y")] {
            assert!(sink.destination_problem(&d).is_some(), "{d:?}");
        }
        assert_eq!(sink.destination_problem(&dir.path().join("ok")), None);
    }
}
