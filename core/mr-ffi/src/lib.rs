//! Interfaz C de `memrescue_core.dll`. Contrato en `include/memrescue_core.h` y `docs/API_FFI.md`.
//!
//! Todo el `unsafe` del motor vive aquí y se limita a: leer punteros de entrada, escribir en
//! búferes del llamador y escribir estructuras de salida. Cada función exportada captura pánicos.

use mr_api::task::{Task, TaskState};
use mr_api::{ApiError, HashOptions, HashResult};
use mr_common::HashAlgorithm;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const ABI_VERSION: u32 = 1;

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

fn api_error_code(e: &ApiError) -> i32 {
    match e {
        ApiError::Io(mr_io::IoError::DevicePathRejected(_)) => MR_ERR_PATH_REJECTED,
        ApiError::Io(_) => MR_ERR_IO,
        ApiError::Store(_) => MR_ERR_STORE,
        ApiError::Cancelled => MR_ERR_CANCELLED,
        ApiError::InvalidArgument(_) => MR_ERR_INVALID_ARGUMENT,
    }
}

fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => fail(MR_ERR_PANIC, "error interno del motor (pánico capturado)"),
    }
}

// ---------- Registro de tareas ----------

enum Entry {
    Hash {
        task: Arc<Task<HashResult>>,
        outcome: Option<Result<HashResult, (i32, String)>>,
    },
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static R: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_handle() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

fn with_entry<R>(h: u64, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    map.get_mut(&h).map(f)
}

// ---------- Utilidades de punteros ----------

/// # Safety
/// `ptr` debe apuntar a `len` bytes legibles, o ser nulo con `len == 0`.
unsafe fn read_utf8<'a>(ptr: *const u8, len: usize) -> Result<&'a str, i32> {
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
    // SAFETY: garantizado por el llamador según el contrato.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes)
        .map_err(|_| fail(MR_ERR_INVALID_ARGUMENT, "la cadena no es UTF-8 válido"))
}

/// Escribe `s` + terminador en `buf` si cabe.
///
/// # Safety
/// `buf` debe tener `cap` bytes escribibles (o ser nulo con `cap == 0`); `out_len` debe ser válido o nulo.
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

// ---------- Funciones exportadas ----------

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

/// # Safety
/// `path` debe ser UTF-8 de `path_len` bytes y `out` un puntero válido a `MrImageInfo`.
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
        match mr_api::image_info(p, sector_size) {
            Ok(i) => {
                // SAFETY: `out` no es nulo y apunta a una estructura válida por contrato.
                unsafe {
                    *out = MrImageInfo {
                        size_bytes: i.size_bytes,
                        sector_count: i.sector_count,
                        trailing_bytes: i.trailing_bytes,
                        sector_size: i.sector_size,
                        reserved: 0,
                    };
                }
                MR_OK
            }
            Err(e) => fail(api_error_code(&e), e.to_string()),
        }
    })
}

fn algorithms_from_mask(mask: u32) -> Vec<HashAlgorithm> {
    HashAlgorithm::ALL
        .into_iter()
        .filter(|a| mask & mask_bit(*a) != 0)
        .collect()
}

fn mask_bit(a: HashAlgorithm) -> u32 {
    match a {
        HashAlgorithm::Crc32 => 1,
        HashAlgorithm::Md5 => 2,
        HashAlgorithm::Sha1 => 4,
        HashAlgorithm::Sha256 => 8,
    }
}

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
        let algorithms = algorithms_from_mask(algorithms_mask);
        if algorithms.is_empty() || algorithms_mask & !0xF != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "máscara de algoritmos no válida");
        }
        let opts = HashOptions {
            algorithms,
            store_path: (!s.is_empty()).then(|| PathBuf::from(s)),
            ..HashOptions::default()
        };
        match mr_api::start_hash_task(PathBuf::from(p), sector_size, opts) {
            Ok(task) => {
                let h = next_handle();
                registry().lock().unwrap_or_else(|e| e.into_inner()).insert(
                    h,
                    Entry::Hash {
                        task,
                        outcome: None,
                    },
                );
                // SAFETY: `out_task` no es nulo.
                unsafe { *out_task = h };
                MR_OK
            }
            Err(e) => fail(api_error_code(&e), e.to_string()),
        }
    })
}

/// # Safety
/// `out` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_task_status_get(task: u64, out: *mut MrTaskStatus) -> i32 {
    guard(|| {
        if out.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let st = with_entry(task, |e| match e {
            Entry::Hash { task, .. } => {
                let (done, total) = task.progress();
                MrTaskStatus {
                    state: task.state() as u32,
                    reserved: 0,
                    done,
                    total,
                }
            }
        });
        match st {
            Some(s) => {
                // SAFETY: `out` no es nulo.
                unsafe { *out = s };
                MR_OK
            }
            None => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de tarea no válido: {task}"),
            ),
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_task_cancel(task: u64) -> i32 {
    guard(|| {
        match with_entry(task, |e| match e {
            Entry::Hash { task, .. } => task.cancel(),
        }) {
            Some(()) => MR_OK,
            None => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de tarea no válido: {task}"),
            ),
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_task_free(task: u64) -> i32 {
    guard(|| {
        let removed = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&task);
        match removed {
            Some(Entry::Hash { task, .. }) => {
                // Si sigue en marcha se cancela; el hilo termina por su cuenta.
                task.cancel();
                MR_OK
            }
            None => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de tarea no válido: {task}"),
            ),
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
            Entry::Hash { task, outcome } => {
                if outcome.is_none() {
                    if task.state() == TaskState::Running {
                        return Err((MR_ERR_NOT_FINISHED, "la tarea no ha terminado".to_string()));
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
                    Some(Ok(r)) => r
                        .digests
                        .iter()
                        .find(|(a, _)| *a == alg)
                        .map(|(_, h)| h.clone())
                        .ok_or((
                            MR_ERR_INVALID_ARGUMENT,
                            format!("la tarea no calculó {alg}"),
                        )),
                    Some(Err((c, m))) => Err((*c, m.clone())),
                    None => Err((MR_ERR_NOT_FINISHED, "resultado no disponible".to_string())),
                }
            }
        });
        match lookup {
            None => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de tarea no válido: {task}"),
            ),
            Some(Err((c, m))) => fail(c, m),
            // SAFETY: contrato del llamador.
            Some(Ok(hex)) => unsafe { write_str(&hex, buf, cap, out_len) },
        }
    })
}

#[cfg(test)]
mod tests;
