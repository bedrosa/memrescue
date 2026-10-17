//! Interfaz C de `memrescue_core.dll`. Contrato en `include/memrescue_core.h` y `docs/API_FFI.md`.
//!
//! Todo el `unsafe` del motor vive aquí y se limita a: leer punteros de entrada, escribir en
//! búferes del llamador y escribir estructuras de salida. Cada función exportada captura pánicos.
//! Orígenes y tareas viven en un registro indexado por handles numéricos (nunca punteros).

use mr_api::task::{Task, TaskState};
use mr_api::{ApiError, HashOptions, HashResult, Source, VerifyOptions};
use mr_common::HashAlgorithm;
use mr_io::resilient::{OnError, ReadPolicy, ReadReport};
use mr_io::{BlockDevice, ImageFile, IoError};
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const ABI_VERSION: u32 = 2;
pub const MAX_STRING_BYTES: usize = 65536;

pub const MR_OK: i32 = 0;
pub const MR_ERR_INVALID_ARGUMENT: i32 = 1;
pub const MR_ERR_IO: i32 = 2;
pub const MR_ERR_PATH_REJECTED: i32 = 3;
pub const MR_ERR_STORE: i32 = 4;
pub const MR_ERR_CANCELLED: i32 = 5;
pub const MR_ERR_NOT_FINISHED: i32 = 6;
pub const MR_ERR_INVALID_HANDLE: i32 = 7;
pub const MR_ERR_BUFFER_TOO_SMALL: i32 = 8;
pub const MR_ERR_TASK_FAILED: i32 = 9;
pub const MR_ERR_UNSUPPORTED_FORMAT: i32 = 10;
pub const MR_ERR_OUT_OF_RANGE: i32 = 11;
pub const MR_ERR_PANIC: i32 = 99;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrImageInfo {
    pub size_bytes: u64,
    pub sector_count: u64,
    pub trailing_bytes: u64,
    pub sector_size: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrTaskStatus {
    pub state: u32,
    pub reserved: u32,
    pub done: u64,
    pub total: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVerifyReport {
    pub sectors_requested: u64,
    pub sectors_ok: u64,
    pub sectors_bad: u64,
    pub retry_reads: u64,
    pub bad_range_count: u64,
    pub bad_ranges_truncated: u32,
    pub reserved: u32,
}

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_last_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

fn fail(code: i32, msg: impl Into<String>) -> i32 {
    set_last_error(msg);
    code
}

fn io_error_code(e: &IoError) -> i32 {
    match e {
        IoError::DevicePathRejected(_) => MR_ERR_PATH_REJECTED,
        IoError::UnsupportedFormat(_) => MR_ERR_UNSUPPORTED_FORMAT,
        IoError::InvalidRange { .. }
        | IoError::OutOfBounds { .. }
        | IoError::RequestTooLarge { .. } => MR_ERR_OUT_OF_RANGE,
        IoError::InvalidSectorSize(_) => MR_ERR_INVALID_ARGUMENT,
        IoError::Cancelled => MR_ERR_CANCELLED,
        IoError::Io(_) | IoError::Read { .. } => MR_ERR_IO,
    }
}

fn api_error_code(e: &ApiError) -> i32 {
    match e {
        ApiError::Io(io) => io_error_code(io),
        ApiError::Store(_) => MR_ERR_STORE,
        ApiError::Cancelled => MR_ERR_CANCELLED,
        ApiError::InvalidArgument(_) => MR_ERR_INVALID_ARGUMENT,
        ApiError::Spawn(_) => MR_ERR_IO,
    }
}

fn api_fail(e: &ApiError) -> i32 {
    fail(api_error_code(e), e.to_string())
}

fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => fail(MR_ERR_PANIC, "error interno del motor (pánico capturado)"),
    }
}

// ---------- Registro de handles ----------

type Outcome<T> = Option<Result<T, (i32, String)>>;

enum Entry {
    Source(Source),
    Hash {
        task: Arc<Task<HashResult>>,
        outcome: Outcome<HashResult>,
    },
    Verify {
        task: Arc<Task<ReadReport>>,
        outcome: Outcome<ReadReport>,
    },
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static R: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn insert(e: Entry) -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    let h = N.fetch_add(1, Ordering::Relaxed);
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(h, e);
    h
}

fn with_entry<R>(h: u64, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    map.get_mut(&h).map(f)
}

fn source(h: u64) -> Result<Source, i32> {
    match with_entry(h, |e| match e {
        Entry::Source(s) => Some(Arc::clone(s)),
        _ => None,
    }) {
        Some(Some(s)) => Ok(s),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de origen no válido: {h}"),
        )),
    }
}

fn bad_task(h: u64) -> i32 {
    fail(
        MR_ERR_INVALID_HANDLE,
        format!("handle de tarea no válido: {h}"),
    )
}

/// Toma el resultado de una tarea terminada y lo guarda en `outcome` (una sola vez).
fn settle<T>(task: &Task<T>, outcome: &mut Outcome<T>) -> Result<(), (i32, String)> {
    if outcome.is_none() {
        if task.state() == TaskState::Running {
            return Err((MR_ERR_NOT_FINISHED, "la tarea no ha terminado".into()));
        }
        *outcome = task.try_take().map(|r| {
            r.map_err(|e| {
                let code = match api_error_code(&e) {
                    MR_ERR_CANCELLED => MR_ERR_CANCELLED,
                    _ => MR_ERR_TASK_FAILED,
                };
                (code, e.to_string())
            })
        });
    }
    match outcome {
        Some(Ok(_)) => Ok(()),
        Some(Err((c, m))) => Err((*c, m.clone())),
        None => Err((MR_ERR_NOT_FINISHED, "resultado no disponible".into())),
    }
}

// ---------- Utilidades de punteros ----------

/// # Safety
/// `ptr` debe apuntar a `len` bytes legibles, o ser nulo con `len == 0`.
unsafe fn read_utf8<'a>(ptr: *const u8, len: usize) -> Result<&'a str, i32> {
    if len > MAX_STRING_BYTES {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            format!("cadena demasiado larga ({len} bytes)"),
        ));
    }
    if ptr.is_null() {
        return if len == 0 {
            Ok("")
        } else {
            Err(fail(
                MR_ERR_INVALID_ARGUMENT,
                "puntero nulo con longitud distinta de cero",
            ))
        };
    }
    // SAFETY: garantizado por el llamador; `len` ≤ 64 KiB ≤ isize::MAX.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes)
        .map_err(|_| fail(MR_ERR_INVALID_ARGUMENT, "la cadena no es UTF-8 válido"))
}

/// Escribe `s` + terminador en `buf` si cabe.
///
/// # Safety
/// `buf` debe tener `cap` bytes escribibles (o ser nulo con `cap == 0`); `out_len` válido o nulo.
unsafe fn write_str(s: &str, buf: *mut u8, cap: usize, out_len: *mut usize) -> i32 {
    if !out_len.is_null() {
        // SAFETY: puntero válido por contrato.
        unsafe { *out_len = s.len() };
    }
    if buf.is_null() || cap < s.len() + 1 {
        return fail(MR_ERR_BUFFER_TOO_SMALL, "búfer insuficiente");
    }
    // SAFETY: `buf` tiene al menos `s.len() + 1` bytes escribibles.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    MR_OK
}

/// # Safety
/// `out` debe ser nulo o apuntar a un `T` escribible.
unsafe fn write_out<T>(out: *mut T, v: T) -> i32 {
    if out.is_null() {
        return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
    }
    // SAFETY: no nulo y válido por contrato.
    unsafe { out.write(v) };
    MR_OK
}

fn info_of(src: &ImageFile) -> MrImageInfo {
    MrImageInfo {
        size_bytes: src.size(),
        sector_count: src.sector_count(),
        trailing_bytes: src.trailing_bytes(),
        sector_size: src.sector_size(),
        reserved: 0,
    }
}

fn mask_bit(a: HashAlgorithm) -> u32 {
    match a {
        HashAlgorithm::Crc32 => 1,
        HashAlgorithm::Md5 => 2,
        HashAlgorithm::Sha1 => 4,
        HashAlgorithm::Sha256 => 8,
    }
}

fn hash_options(mask: u32, store: &str) -> Result<HashOptions, i32> {
    let algorithms: Vec<HashAlgorithm> = HashAlgorithm::ALL
        .into_iter()
        .filter(|a| mask & mask_bit(*a) != 0)
        .collect();
    if algorithms.is_empty() || mask & !0xF != 0 {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "máscara de algoritmos no válida",
        ));
    }
    Ok(HashOptions {
        algorithms,
        store_path: (!store.is_empty()).then(|| PathBuf::from(store)),
        ..HashOptions::default()
    })
}

// ---------- Funciones exportadas: generales ----------

#[no_mangle]
pub extern "C" fn mr_abi_version() -> u32 {
    ABI_VERSION
}

/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_engine_version(buf: *mut u8, cap: usize, out_len: *mut usize) -> i32 {
    // SAFETY: se delega el contrato al llamador.
    guard(|| unsafe { write_str(mr_api::ENGINE_VERSION, buf, cap, out_len) })
}

/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_last_error(buf: *mut u8, cap: usize, out_len: *mut usize) -> i32 {
    let msg = LAST_ERROR.with(|e| e.borrow().clone());
    // SAFETY: se delega el contrato al llamador.
    guard(|| unsafe { write_str(&msg, buf, cap, out_len) })
}

// ---------- Ficheros ----------

/// # Safety
/// `path` UTF-8 de `path_len` bytes; `out_format` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_image_probe(
    path: *const u8,
    path_len: usize,
    out_format: *mut u32,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        match mr_api::probe_format(p) {
            // SAFETY: contrato del llamador.
            Ok(f) => unsafe { write_out(out_format, f.code()) },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// `path` UTF-8 de `path_len` bytes y `out` un puntero válido a `MrImageInfo`.
#[no_mangle]
pub unsafe extern "C" fn mr_image_info_get(
    path: *const u8,
    path_len: usize,
    sector_size: u32,
    out: *mut MrImageInfo,
) -> i32 {
    guard(|| {
        if out.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        match mr_api::open_source(p, sector_size) {
            // SAFETY: `out` no es nulo.
            Ok(s) => unsafe { write_out(out, info_of(&s)) },
            Err(e) => api_fail(&e),
        }
    })
}

// ---------- Orígenes ----------

/// # Safety
/// `path` UTF-8 de `path_len` bytes; `out_source` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_open(
    path: *const u8,
    path_len: usize,
    sector_size: u32,
    flags: u32,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !1 != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones de apertura no válidas");
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let r = if flags & 1 != 0 {
            ImageFile::open_as_raw(p, sector_size)
        } else {
            ImageFile::open(p, sector_size)
        };
        match r {
            // SAFETY: `out_source` no es nulo.
            Ok(img) => unsafe { write_out(out_source, insert(Entry::Source(Arc::new(img)))) },
            Err(e) => fail(io_error_code(&e), e.to_string()),
        }
    })
}

/// # Safety
/// `out` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_info(source_h: u64, out: *mut MrImageInfo) -> i32 {
    guard(|| match source(source_h) {
        // SAFETY: contrato del llamador.
        Ok(s) => unsafe { write_out(out, info_of(&s)) },
        Err(c) => c,
    })
}

/// Lee `count` sectores desde `lba` en `buf` (al menos `count × sector_size` bytes).
///
/// # Safety
/// `buf` debe tener `cap` bytes escribibles; `out_len` válido o nulo.
#[no_mangle]
pub unsafe extern "C" fn mr_source_read(
    source_h: u64,
    lba: u64,
    count: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let s = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let len = match mr_io::sector_span(s.as_ref(), lba, u64::from(count)) {
            Ok((_, len)) => len,
            Err(e) => return fail(io_error_code(&e), e.to_string()),
        };
        if !out_len.is_null() {
            // SAFETY: puntero válido por contrato.
            unsafe { *out_len = len };
        }
        if buf.is_null() || cap < len {
            return fail(MR_ERR_BUFFER_TOO_SMALL, format!("se necesitan {len} bytes"));
        }
        // SAFETY: `buf` tiene al menos `len` bytes escribibles (cap ≥ len) y len ≤ 64 MiB.
        let out = unsafe { std::slice::from_raw_parts_mut(buf, len) };
        match s.read_sectors(lba, u64::from(count), out) {
            Ok(()) => MR_OK,
            Err(e) => fail(io_error_code(&e), e.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_source_close(source_h: u64) -> i32 {
    guard(|| {
        let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&source_h) {
            Some(Entry::Source(_)) => {
                // Las tareas que aún usan el origen conservan su propia referencia (Arc).
                map.remove(&source_h);
                MR_OK
            }
            _ => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de origen no válido: {source_h}"),
            ),
        }
    })
}

// ---------- Tareas: hash ----------

/// # Safety
/// Cadenas según contrato; `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_hash_start(
    path: *const u8,
    path_len: usize,
    sector_size: u32,
    algorithms_mask: u32,
    store_path: *const u8,
    store_len: usize,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        // SAFETY: contrato del llamador.
        let (p, s) = match unsafe { (read_utf8(path, path_len), read_utf8(store_path, store_len)) }
        {
            (Ok(p), Ok(s)) => (p, s),
            (Err(c), _) | (_, Err(c)) => return c,
        };
        let opts = match hash_options(algorithms_mask, s) {
            Ok(o) => o,
            Err(c) => return c,
        };
        match mr_api::start_hash_task(PathBuf::from(p), sector_size, opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::Hash {
                        task,
                        outcome: None,
                    }),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// Cadena según contrato; `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_hash_start(
    source_h: u64,
    algorithms_mask: u32,
    store_path: *const u8,
    store_len: usize,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let src = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        // SAFETY: contrato del llamador.
        let s = match unsafe { read_utf8(store_path, store_len) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let opts = match hash_options(algorithms_mask, s) {
            Ok(o) => o,
            Err(c) => return c,
        };
        match mr_api::start_hash_task_on(src, opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::Hash {
                        task,
                        outcome: None,
                    }),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_hash_result(
    task: u64,
    algorithm: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let Some(alg) = HashAlgorithm::ALL
            .into_iter()
            .find(|a| mask_bit(*a) == algorithm)
        else {
            return fail(MR_ERR_INVALID_ARGUMENT, "algoritmo no válido");
        };
        let lookup = with_entry(task, |e| match e {
            Entry::Hash { task, outcome } => settle(task, outcome).and_then(|()| match outcome {
                Some(Ok(r)) => r
                    .digests
                    .iter()
                    .find(|(a, _)| *a == alg)
                    .map(|(_, h)| h.clone())
                    .ok_or((
                        MR_ERR_INVALID_ARGUMENT,
                        format!("la tarea no calculó {alg}"),
                    )),
                _ => Err((MR_ERR_NOT_FINISHED, "resultado no disponible".into())),
            }),
            _ => Err((
                MR_ERR_INVALID_HANDLE,
                format!("la tarea {task} no es de hash"),
            )),
        });
        match lookup {
            None => bad_task(task),
            Some(Err((c, m))) => fail(c, m),
            // SAFETY: contrato del llamador.
            Some(Ok(hex)) => unsafe { write_str(&hex, buf, cap, out_len) },
        }
    })
}

// ---------- Tareas: verificación ----------

/// # Safety
/// `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_verify_start(
    source_h: u64,
    start_lba: u64,
    count: u64,
    retries: u32,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if retries > 100 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "demasiados reintentos (máximo 100)",
            );
        }
        let src = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let opts = VerifyOptions {
            start_lba,
            count: (count != 0).then_some(count),
            policy: ReadPolicy {
                retries,
                on_error: OnError::FillAndContinue,
                ..ReadPolicy::default()
            },
        };
        match mr_api::start_verify_task(src, opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::Verify {
                        task,
                        outcome: None,
                    }),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

fn with_verify<R>(task: u64, f: impl FnOnce(&ReadReport) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::Verify { task, outcome } => settle(task, outcome).map(|()| match outcome {
            Some(Ok(rep)) => Some(f(rep)),
            _ => None,
        }),
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es de verificación"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(Some(v))) => Ok(v),
        Some(Ok(None)) => Err(fail(MR_ERR_NOT_FINISHED, "resultado no disponible")),
    }
}

/// # Safety
/// `out` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_verify_result(task: u64, out: *mut MrVerifyReport) -> i32 {
    guard(|| {
        let rep = with_verify(task, |r| MrVerifyReport {
            sectors_requested: r.sectors_requested,
            sectors_ok: r.sectors_read_ok,
            sectors_bad: r.sectors_bad,
            retry_reads: r.retry_reads,
            bad_range_count: r.bad_ranges.len() as u64,
            bad_ranges_truncated: u32::from(r.bad_ranges_truncated),
            reserved: 0,
        });
        match rep {
            // SAFETY: contrato del llamador.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// # Safety
/// `out_lba` y `out_count` deben ser punteros válidos.
#[no_mangle]
pub unsafe extern "C" fn mr_verify_bad_range(
    task: u64,
    index: u64,
    out_lba: *mut u64,
    out_count: *mut u64,
) -> i32 {
    guard(|| {
        if out_lba.is_null() || out_count.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let r = with_verify(task, |rep| {
            usize::try_from(index)
                .ok()
                .and_then(|i| rep.bad_ranges.get(i).copied())
        });
        match r {
            Ok(Some((lba, count))) => {
                // SAFETY: punteros no nulos y válidos por contrato.
                unsafe {
                    out_lba.write(lba);
                    out_count.write(count);
                }
                MR_OK
            }
            Ok(None) => fail(
                MR_ERR_OUT_OF_RANGE,
                format!("índice de rango fuera de límites: {index}"),
            ),
            Err(c) => c,
        }
    })
}

// ---------- Tareas: comunes ----------

/// # Safety
/// `out` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_task_status_get(task: u64, out: *mut MrTaskStatus) -> i32 {
    guard(|| {
        if out.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let st = with_entry(task, |e| {
            let (state, (done, total)) = match e {
                Entry::Hash { task, .. } => (task.state(), task.progress()),
                Entry::Verify { task, .. } => (task.state(), task.progress()),
                Entry::Source(_) => return None,
            };
            Some(MrTaskStatus {
                state: state as u32,
                reserved: 0,
                done,
                total,
            })
        });
        match st.flatten() {
            // SAFETY: `out` no es nulo.
            Some(s) => unsafe { write_out(out, s) },
            None => bad_task(task),
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_task_cancel(task: u64) -> i32 {
    guard(|| {
        let cancelled = with_entry(task, |e| match e {
            Entry::Hash { task, .. } => {
                task.cancel();
                true
            }
            Entry::Verify { task, .. } => {
                task.cancel();
                true
            }
            Entry::Source(_) => false,
        });
        if cancelled == Some(true) {
            MR_OK
        } else {
            bad_task(task)
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_task_free(task: u64) -> i32 {
    guard(|| {
        let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&task) {
            Some(Entry::Hash { .. } | Entry::Verify { .. }) => {}
            _ => return bad_task(task),
        }
        // Si sigue en marcha se cancela; el hilo termina por su cuenta.
        match map.remove(&task) {
            Some(Entry::Hash { task, .. }) => task.cancel(),
            Some(Entry::Verify { task, .. }) => task.cancel(),
            _ => {}
        }
        MR_OK
    })
}

#[cfg(test)]
mod tests;
