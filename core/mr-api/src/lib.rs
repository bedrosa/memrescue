//! API de alto nivel del motor. La consumen la CLI (directamente) y la GUI (a través de `mr-ffi`).
//!
//! Operaciones disponibles (todas de solo lectura):
//! - [`probe_format`]: identifica el formato de un fichero (RAW o contenedor aún no soportado).
//! - [`open_source`] / [`image_info`]: abre una imagen RAW y devuelve sus datos básicos.
//! - [`read_sectors`]: lectura estricta de un rango de sectores (visor hexadecimal).
//! - [`start_hash_task`]: hashes del fichero completo en segundo plano.
//! - [`start_verify_task`]: lectura completa tolerante a errores con informe de sectores ilegibles.

pub mod task;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_io::resilient::{read_range, OnError, ReadPolicy, ReadReport};
use mr_io::{BlockDevice, ImageFile, ImageFormat};
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
    #[error("no se pudo iniciar la tarea: {0}")]
    Spawn(std::io::Error),
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Origen abierto (en esta fase: imagen RAW en solo lectura). Se comparte entre lecturas y tareas.
pub type Source = Arc<ImageFile>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sector_size: u32,
    pub sector_count: u64,
    /// Bytes sobrantes al final que no completan un sector.
    pub trailing_bytes: u64,
}

impl ImageInfo {
    pub fn of(src: &ImageFile) -> Self {
        Self {
            path: src.path().to_path_buf(),
            size_bytes: src.size(),
            sector_size: src.sector_size(),
            sector_count: src.sector_count(),
            trailing_bytes: src.trailing_bytes(),
        }
    }
}

/// Identifica el formato del fichero sin abrirlo como imagen.
pub fn probe_format(path: impl AsRef<Path>) -> Result<ImageFormat> {
    Ok(mr_io::probe_format(path)?)
}

/// Abre una imagen RAW en solo lectura (rechaza dispositivos y contenedores no soportados).
pub fn open_source(path: impl AsRef<Path>, sector_size: u32) -> Result<Source> {
    Ok(Arc::new(ImageFile::open(path, sector_size)?))
}

/// Abre la imagen en solo lectura y devuelve sus datos básicos.
pub fn image_info(path: impl AsRef<Path>, sector_size: u32) -> Result<ImageInfo> {
    let src = open_source(path, sector_size)?;
    Ok(ImageInfo::of(&src))
}

/// Lee `count` sectores desde `lba` con validación estricta (rango, tamaño máximo por petición).
pub fn read_sectors(src: &dyn BlockDevice, lba: u64, count: u64) -> Result<Vec<u8>> {
    let (_, len) = mr_io::sector_span(src, lba, count)?;
    let mut buf = vec![0u8; len];
    src.read_sectors(lba, count, &mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------------------------
// Hashes
// ---------------------------------------------------------------------------------------------

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

/// Calcula hashes del contenido completo (incluidos los bytes finales que no completan un
/// sector). Es estricto: cualquier error de lectura aborta, porque un hash con datos rellenados
/// no representaría el origen.
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
    let chunk = opts.chunk_size.clamp(512, mr_io::MAX_READ_REQUEST);
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
    let src = open_source(&path, sector_size)?;
    start_hash_task_on(src, opts)
}

/// Hash de un origen ya abierto.
pub fn start_hash_task_on(src: Source, opts: HashOptions) -> Result<Arc<Task<HashResult>>> {
    Task::spawn(move |ctx| run_hash_job(&src, &opts, ctx))
}

fn run_hash_job(img: &ImageFile, opts: &HashOptions, ctx: &TaskContext) -> Result<HashResult> {
    let Some(db) = &opts.store_path else {
        return hash_image(img, opts, ctx);
    };
    let mut store = Store::open(db)?;
    let src = store.add_image_source(
        &img.path().display().to_string(),
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

// ---------------------------------------------------------------------------------------------
// Verificación de legibilidad
// ---------------------------------------------------------------------------------------------

/// Opciones de la verificación (lectura completa tolerante a errores).
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub start_lba: u64,
    /// `None` = hasta el último sector completo.
    pub count: Option<u64>,
    pub policy: ReadPolicy,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            start_lba: 0,
            count: None,
            policy: ReadPolicy {
                on_error: OnError::FillAndContinue,
                ..ReadPolicy::default()
            },
        }
    }
}

/// Lee el rango completo de sectores; el progreso se expresa en sectores.
pub fn verify(
    dev: &dyn BlockDevice,
    opts: &VerifyOptions,
    ctx: &TaskContext,
) -> Result<ReadReport> {
    let total = dev.sector_count();
    let count = match opts.count {
        Some(c) => c,
        None => total.checked_sub(opts.start_lba).ok_or_else(|| {
            ApiError::InvalidArgument(format!("LBA inicial {} fuera del origen", opts.start_lba))
        })?,
    };
    ctx.set_total(count);
    let r = read_range(
        dev,
        opts.start_lba,
        count,
        &opts.policy,
        |_, _| Ok(()),
        |done, _| {
            ctx.set_done(done);
            !ctx.is_cancelled()
        },
    );
    match r {
        Err(mr_io::IoError::Cancelled) => Err(ApiError::Cancelled),
        other => Ok(other?),
    }
}

/// Lanza la verificación de un origen en segundo plano.
pub fn start_verify_task(src: Source, opts: VerifyOptions) -> Result<Arc<Task<ReadReport>>> {
    Task::spawn(move |ctx| verify(src.as_ref(), &opts, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice};
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
    fn hash_is_strict_on_read_errors() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0u8; 8192], 512).unwrap()).with_bad([9]);
        let ctx = TaskContext::detached();
        let opts = HashOptions {
            chunk_size: 1024,
            ..HashOptions::default()
        };
        assert!(matches!(
            hash_image(&dev, &opts, &ctx),
            Err(ApiError::Io(mr_io::IoError::Read { offset: 4096, .. }))
        ));
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
        assert!(matches!(
            image_info(r"\\?\C:", 512),
            Err(ApiError::Io(mr_io::IoError::DevicePathRejected(_)))
        ));
    }

    #[test]
    fn read_sectors_reads_exact_ranges_for_all_sector_sizes() {
        for ss in mr_io::COMMON_SECTOR_SIZES {
            let data: Vec<u8> = (0..16 * ss).map(|i| (i / ss) as u8).collect();
            let dev = MemDevice::new(data, ss).unwrap();
            let b = read_sectors(&dev, 15, 1).unwrap();
            assert_eq!(b.len(), ss as usize);
            assert!(b.iter().all(|&x| x == 15));
            assert!(read_sectors(&dev, 16, 1).is_err());
            assert!(read_sectors(&dev, 0, 0).is_err());
        }
    }

    #[test]
    fn verify_reports_bad_sectors_and_progress_in_sectors() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0u8; 512 * 100], 512).unwrap())
            .with_bad([10, 11, 50]);
        let ctx = TaskContext::detached();
        let r = verify(&dev, &VerifyOptions::default(), &ctx).unwrap();
        assert_eq!((r.sectors_read_ok, r.sectors_bad), (97, 3));
        assert_eq!(r.bad_ranges, vec![(10, 2), (50, 1)]);
        assert_eq!((ctx.done(), ctx.total()), (100, 100));
    }

    #[test]
    fn verify_task_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("v.img");
        std::fs::write(&img, vec![0u8; 64 * 1024 * 1024]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let opts = VerifyOptions {
            policy: ReadPolicy {
                chunk_sectors: 1,
                on_error: OnError::FillAndContinue,
                ..ReadPolicy::default()
            },
            ..VerifyOptions::default()
        };
        let t = start_verify_task(src, opts).unwrap();
        t.cancel();
        assert!(matches!(t.wait(), Err(ApiError::Cancelled)));
        assert_eq!(t.state(), TaskState::Cancelled);
    }
}
