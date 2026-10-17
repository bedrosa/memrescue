//! API de alto nivel del motor. La consumen la CLI (directamente) y la GUI (a través de `mr-ffi`).
//!
//! Operaciones disponibles en M0 (todas de solo lectura):
//! - [`image_info`]: tamaño y nº de sectores de un fichero de imagen.
//! - [`start_hash_task`]: hashes de una imagen en segundo plano con progreso y cancelación.

pub mod task;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_io::{BlockDevice, ImageFile};
use mr_store::{JobState, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use task::{Task, TaskContext};
use thiserror::Error;

pub use mr_common::ENGINE_VERSION;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Io(#[from] mr_io::IoError),
    #[error(transparent)]
    Store(#[from] mr_store::StoreError),
    #[error("operación cancelada")]
    Cancelled,
    #[error("argumento no válido: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sector_size: u32,
    pub sector_count: u64,
    /// Bytes sobrantes al final que no completan un sector.
    pub trailing_bytes: u64,
}

/// Abre la imagen en solo lectura y devuelve sus datos básicos.
pub fn image_info(path: impl AsRef<Path>, sector_size: u32) -> Result<ImageInfo> {
    let img = ImageFile::open(path.as_ref(), sector_size)?;
    Ok(ImageInfo {
        path: path.as_ref().to_path_buf(),
        size_bytes: img.size(),
        sector_size,
        sector_count: img.sector_count(),
        trailing_bytes: img.size() % u64::from(sector_size),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashResult {
    pub bytes: u64,
    pub digests: Vec<(HashAlgorithm, String)>,
}

/// Opciones del trabajo de hash.
#[derive(Debug, Clone)]
pub struct HashOptions {
    pub algorithms: Vec<HashAlgorithm>,
    /// Tamaño de lectura por bloque.
    pub chunk_size: usize,
    /// Si se indica, el trabajo se registra en esta base de datos de trabajo.
    pub store_path: Option<PathBuf>,
}

impl Default for HashOptions {
    fn default() -> Self {
        Self {
            algorithms: vec![HashAlgorithm::Sha256],
            chunk_size: 4 * 1024 * 1024,
            store_path: None,
        }
    }
}

/// Calcula hashes de forma síncrona (usado por la tarea y por pruebas).
pub fn hash_image(
    dev: &dyn BlockDevice,
    opts: &HashOptions,
    ctx: &TaskContext,
) -> Result<HashResult> {
    if opts.algorithms.is_empty() {
        return Err(ApiError::InvalidArgument(
            "no se indicó ningún algoritmo".into(),
        ));
    }
    let chunk = opts.chunk_size.clamp(512, 64 * 1024 * 1024);
    let total = dev.size();
    ctx.set_total(total);
    let mut hasher = MultiHasher::new(&opts.algorithms);
    let mut buf = vec![0u8; chunk];
    let mut pos = 0u64;
    while pos < total {
        if ctx.is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        let n = usize::try_from((total - pos).min(chunk as u64)).unwrap_or(chunk);
        dev.read_at(pos, &mut buf[..n])?;
        hasher.update(&buf[..n]);
        pos += n as u64;
        ctx.set_done(pos);
    }
    Ok(HashResult {
        bytes: total,
        digests: hasher.finish(),
    })
}

/// Lanza el cálculo de hashes de una imagen en segundo plano.
pub fn start_hash_task(
    path: PathBuf,
    sector_size: u32,
    opts: HashOptions,
) -> Result<Arc<Task<HashResult>>> {
    // Se abre aquí para devolver errores de ruta de forma síncrona.
    let img = ImageFile::open(&path, sector_size)?;
    Ok(Task::spawn(move |ctx| {
        run_hash_job(&img, &path, &opts, ctx)
    }))
}

fn run_hash_job(
    img: &ImageFile,
    path: &Path,
    opts: &HashOptions,
    ctx: &TaskContext,
) -> Result<HashResult> {
    let Some(db) = &opts.store_path else {
        return hash_image(img, opts, ctx);
    };
    let store = Store::open(db)?;
    let src = store.add_image_source(
        &path.display().to_string(),
        img.size(),
        img.sector_size(),
        None,
    )?;
    let algos: Vec<&str> = opts.algorithms.iter().map(|a| a.name()).collect();
    let params = format!(r#"{{"algorithms":["{}"]}}"#, algos.join("\",\""));
    let job = store.create_job("hash", Some(src), &params, img.size())?;
    store.set_state(job, JobState::Running, None)?;
    let res = hash_image(img, opts, ctx);
    match &res {
        Ok(r) => {
            store.set_progress(job, r.bytes, r.bytes)?;
            let pairs: Vec<String> = r
                .digests
                .iter()
                .map(|(a, h)| format!(r#""{a}":"{h}""#))
                .collect();
            store.complete(job, &format!("{{{}}}", pairs.join(",")))?;
        }
        Err(ApiError::Cancelled) => {
            store.set_progress(job, ctx.done(), ctx.total())?;
            store.set_state(job, JobState::Cancelled, None)?;
        }
        Err(e) => {
            store.set_state(job, JobState::Failed, Some(&e.to_string()))?;
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::MemDevice;
    use std::io::Write;
    use task::TaskState;

    #[test]
    fn hash_of_memory_device_matches_known_value() {
        let dev = MemDevice::new(vec![0u8; 1024], 512).unwrap();
        let ctx = TaskContext::detached();
        let r = hash_image(&dev, &HashOptions::default(), &ctx).unwrap();
        // SHA-256 de 1024 bytes a cero.
        assert_eq!(
            r.digests[0].1,
            "5f70bf18a086007016e948b04aed3b82103a36bea41755b6cddfaf10ace3c6ef"
        );
        assert_eq!(ctx.done(), 1024);
    }

    #[test]
    fn task_runs_reports_progress_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("x.img");
        std::fs::File::create(&img)
            .unwrap()
            .write_all(&vec![7u8; 3 * 1024 * 1024])
            .unwrap();
        let db = dir.path().join("w.mrdb");
        let opts = HashOptions {
            algorithms: vec![HashAlgorithm::Crc32, HashAlgorithm::Sha256],
            chunk_size: 1024 * 1024,
            store_path: Some(db.clone()),
        };
        let t = start_hash_task(img, 512, opts).unwrap();
        let r = t.wait().unwrap();
        assert_eq!(r.bytes, 3 * 1024 * 1024);
        assert_eq!(t.state(), TaskState::Completed);
        assert_eq!(t.progress(), (r.bytes, r.bytes));
        let store = Store::open(&db).unwrap();
        let jobs = store.jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Completed);
        assert!(jobs[0].result_json.as_deref().unwrap().contains("sha256"));
    }

    #[test]
    fn cancellation_stops_and_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("big.img");
        std::fs::File::create(&img)
            .unwrap()
            .write_all(&vec![1u8; 32 * 1024 * 1024])
            .unwrap();
        let db = dir.path().join("w.mrdb");
        let opts = HashOptions {
            algorithms: vec![HashAlgorithm::Sha256],
            chunk_size: 512,
            store_path: Some(db.clone()),
        };
        let t = start_hash_task(img, 512, opts).unwrap();
        t.cancel();
        let r = t.wait();
        assert!(matches!(r, Err(ApiError::Cancelled)));
        assert_eq!(t.state(), TaskState::Cancelled);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.jobs().unwrap()[0].state, JobState::Cancelled);
    }

    #[test]
    fn rejects_device_paths() {
        assert!(matches!(
            image_info(r"\\.\PhysicalDrive0", 512),
            Err(ApiError::Io(mr_io::IoError::DevicePathRejected(_)))
        ));
    }
}
