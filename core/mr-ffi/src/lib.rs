//! Interfaz C de `memrescue_core.dll`. Contrato en `include/memrescue_core.h` y `docs/API_FFI.md`.
//!
//! Todo el `unsafe` del motor vive aquí y se limita a: leer punteros de entrada, escribir en
//! búferes del llamador y escribir estructuras de salida. Cada función exportada captura pánicos.
//! Orígenes y tareas viven en un registro indexado por handles numéricos (nunca punteros).

use mr_api::task::{Task, TaskState};
use mr_api::{ApiError, HashOptions, HashResult, OpenOptions, Source, VerifyOptions};
use mr_common::HashAlgorithm;
use mr_io::resilient::{OnError, ReadPolicy, ReadReport};
use mr_io::{BlockDevice, IoError, SectorState};
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const ABI_VERSION: u32 = 3;
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
pub const MR_ERR_CORRUPT: i32 = 12;
pub const MR_ERR_EXTERNAL_REFERENCE: i32 = 13;
pub const MR_ERR_UNSUPPORTED_FEATURE: i32 = 14;

/// `mr_source_open`: leer como RAW aunque parezca un contenedor.
pub const MR_OPEN_FORCE_RAW: u32 = 1;
/// `mr_source_open`: abrir la imagen dividida que empieza en la ruta indicada.
pub const MR_OPEN_SPLIT: u32 = 2;
/// `mr_source_open`: modo tolerante para contenedores con estructuras de asignación parcialmente
/// dañadas (ADR-020). Compatible con la ABI 3: es un bit nuevo de `flags`.
pub const MR_OPEN_TOLERANT: u32 = 4;
/// `mr_source_verify_start`: leer hacia atrás tras un salto.
pub const MR_VERIFY_READ_BACK: u32 = 1;
/// Máximo de reintentos por sector aceptado en la frontera C.
pub const MAX_RETRIES: u32 = mr_io::resilient::MAX_RETRIES;
/// `mr_source_verify_start_ex`: modo «ignorar todo», ningún error se reintenta.
pub const MR_VERIFY_IGNORE_ALL_ERRORS: u32 = 2;
/// Máximo de códigos de error a omitir en `MrVerifyOptions::skip_codes`.
pub const MAX_SKIP_CODES: u32 = 256;
pub const MR_ERR_PANIC: i32 = 99;
/// `mr_read_options.flags`: tras un salto, leer hacia atrás hasta el siguiente error.
pub const MR_READ_READ_BACK: u32 = 1;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrImageInfo {
    pub size_bytes: u64,
    pub sector_count: u64,
    pub trailing_bytes: u64,
    pub sector_size: u32,
    /// Código de `ImageFormat` con el que se interpreta (1 = RAW…).
    pub format: u32,
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
    pub sectors_read: u64,
    pub sectors_recovered: u64,
    pub sectors_unreadable: u64,
    pub sectors_skipped: u64,
    pub sectors_sparse: u64,
    pub retry_reads: u64,
    /// Tramos no fiables (ilegibles o saltados) disponibles en `mr_verify_untrusted_range`.
    pub untrusted_range_count: u64,
    /// 1 si el mapa se truncó (los recuentos por estado siguen siendo exactos).
    pub ranges_truncated: u32,
    pub reserved: u32,
}

/// Opciones de `mr_source_verify_start_ex` (48 bytes). `struct_size` permite ampliar la estructura
/// en el futuro sin romper la ABI: el llamador escribe `sizeof(mr_verify_options)` de su cabecera.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MrVerifyOptions {
    pub struct_size: u32,
    /// `MR_VERIFY_READ_BACK | MR_VERIFY_IGNORE_ALL_ERRORS`.
    pub flags: u32,
    pub start_lba: u64,
    /// 0 = hasta el último sector.
    pub count: u64,
    /// Sectores que se saltan tras cada ilegible (0 = sin salto).
    pub jump_sectors: u64,
    /// Reintentos por sector (0–100).
    pub retries: u32,
    /// Número de elementos de `skip_codes` (≤ `MAX_SKIP_CODES`).
    pub skip_code_count: u32,
    /// Códigos de error del sistema que no se reintentan (positivos); nulo si `skip_code_count` = 0.
    pub skip_codes: *const i32,
}

/// Opciones de `mr_source_read_mapped_ex` (32 bytes). Mismo esquema de versión que
/// `MrVerifyOptions`: `struct_size` = `sizeof(mr_read_options)` de la cabecera del llamador.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MrReadOptions {
    pub struct_size: u32,
    /// `MR_READ_READ_BACK`; otros bits se rechazan.
    pub flags: u32,
    /// Sectores que se marcan saltados tras cada ilegible (0 = sin salto).
    pub jump_sectors: u64,
    /// Reintentos por sector (0–100).
    pub retries: u32,
    /// Relleno de los sectores ilegibles (estado 2), repetido cada 4 bytes.
    pub bad_fill: [u8; 4],
    /// Relleno de los sectores saltados (estado 3), repetido cada 4 bytes.
    pub skip_fill: [u8; 4],
    /// Reservado: debe ser 0.
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
        IoError::UnsupportedFeature { .. } => MR_ERR_UNSUPPORTED_FEATURE,
        IoError::ExternalReference { .. } => MR_ERR_EXTERNAL_REFERENCE,
        IoError::Corrupt { .. } => MR_ERR_CORRUPT,
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
        /// Índices materializados del informe (se calculan una vez al consultar el resultado),
        /// para que las consultas por índice sean O(1) bajo el bloqueo del registro.
        index: Option<VerifyIndex>,
    },
}

/// Tramos no fiables y códigos de error de un informe de verificación, en orden.
struct VerifyIndex {
    untrusted: Vec<(u64, u64, SectorState)>,
    codes: Vec<(i32, u64)>,
}

impl VerifyIndex {
    fn new(r: &ReadReport) -> Self {
        Self {
            untrusted: r
                .map
                .runs()
                .iter()
                .copied()
                .filter(|t| !t.2.is_trustworthy())
                .collect(),
            codes: r.error_codes.iter().map(|(&c, &n)| (c, n)).collect(),
        }
    }
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

fn info_of(src: &mr_api::OpenSource) -> MrImageInfo {
    let i = src.info();
    MrImageInfo {
        size_bytes: i.size_bytes,
        sector_count: i.sector_count,
        trailing_bytes: i.trailing_bytes,
        sector_size: i.sector_size,
        format: i.format.code(),
    }
}

fn state_code(s: SectorState) -> u8 {
    s as u8
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
        if flags & !(MR_OPEN_FORCE_RAW | MR_OPEN_SPLIT | MR_OPEN_TOLERANT) != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones de apertura no válidas");
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let opts = OpenOptions {
            sector_size,
            force_raw: flags & MR_OPEN_FORCE_RAW != 0,
            tolerant: flags & MR_OPEN_TOLERANT != 0,
        };
        let r = if flags & MR_OPEN_SPLIT != 0 {
            mr_api::open_split_source(p, opts)
        } else {
            mr_api::open_with(p, opts)
        };
        match r {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
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

/// Descripción legible del origen (variante del contenedor, segmentos, rango…).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_source_describe(
    source_h: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| match source(source_h) {
        // SAFETY: contrato del llamador.
        Ok(s) => unsafe { write_str(&s.info().description, buf, cap, out_len) },
        Err(c) => c,
    })
}

/// Lista (separada por saltos de línea) los segmentos de una imagen dividida, sin abrirlos.
///
/// # Safety
/// `path` UTF-8 de `path_len` bytes; contrato de cadenas de salida; `out_count` válido o nulo.
#[no_mangle]
pub unsafe extern "C" fn mr_split_discover(
    path: *const u8,
    path_len: usize,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
    out_count: *mut u32,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let segs = match mr_api::discover_split(p) {
            Ok(s) => s,
            Err(e) => return api_fail(&e),
        };
        if !out_count.is_null() {
            // SAFETY: puntero válido por contrato.
            unsafe { *out_count = u32::try_from(segs.len()).unwrap_or(u32::MAX) };
        }
        let text = segs
            .iter()
            .map(|s| s.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // SAFETY: contrato del llamador.
        unsafe { write_str(&text, buf, cap, out_len) }
    })
}

/// Abre `count` sectores de un origen desde `start_lba` como un origen nuevo (partición o rango).
///
/// # Safety
/// `out_source` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_open_range(
    source_h: u64,
    start_lba: u64,
    count: u64,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let parent = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        match mr_api::open_range(&parent, start_lba, count) {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Lectura tolerante para el visor: `buf` recibe los datos (relleno en los ilegibles) y `states`
/// un byte por sector con su estado (0 leído, 1 recuperado, 2 ilegible, 3 saltado, 4 disperso).
/// Devuelve `MR_OK` aunque haya sectores ilegibles: el llamador **debe** consultar `states`.
/// Rellenos por defecto (`BAD!`); sin salto.
///
/// # Safety
/// `buf` con `cap` bytes y `states` con `states_cap` bytes escribibles; `out_len` válido o nulo.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mr_source_read_mapped(
    source_h: u64,
    lba: u64,
    count: u32,
    retries: u32,
    buf: *mut u8,
    cap: usize,
    states: *mut u8,
    states_cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let opts = mr_api::MappedReadOptions {
            retries,
            ..mr_api::MappedReadOptions::default()
        };
        // SAFETY: mismo contrato que esta función.
        unsafe {
            read_mapped(
                source_h, lba, count, &opts, buf, cap, states, states_cap, out_len,
            )
        }
    })
}

/// Lectura tolerante con opciones: como `mr_source_read_mapped`, con salto tras cada ilegible,
/// lectura hacia atrás y **patrones de relleno configurables**. Los patrones no pueden tener sus
/// cuatro bytes iguales (`00 00 00 00` incluido) ni coincidir entre sí (`MR_ERR_INVALID_ARGUMENT`).
///
/// # Safety
/// `options` válido; `buf` con `cap` bytes y `states` con `states_cap` bytes escribibles; `out_len`
/// válido o nulo.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mr_source_read_mapped_ex(
    source_h: u64,
    lba: u64,
    count: u32,
    options: *const MrReadOptions,
    buf: *mut u8,
    cap: usize,
    states: *mut u8,
    states_cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        if options.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero nulo");
        }
        // Se lee primero solo `struct_size`, para no leer más allá de una estructura más pequeña.
        // SAFETY: `options` no es nulo y contiene al menos su primer campo.
        let size = unsafe { options.cast::<u32>().read_unaligned() };
        let expected = std::mem::size_of::<MrReadOptions>();
        if size as usize != expected {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("tamaño de mr_read_options no válido: {size} (se esperaba {expected})"),
            );
        }
        // SAFETY: el tamaño declarado coincide con el de la estructura.
        let o = unsafe { options.read_unaligned() };
        if o.flags & !MR_READ_READ_BACK != 0 || o.reserved != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones de lectura no válidas");
        }
        let opts = mr_api::MappedReadOptions {
            retries: o.retries,
            jump_sectors: o.jump_sectors,
            read_back: o.flags & MR_READ_READ_BACK != 0,
            bad_fill: o.bad_fill,
            skip_fill: o.skip_fill,
        };
        // SAFETY: mismo contrato que esta función.
        unsafe {
            read_mapped(
                source_h, lba, count, &opts, buf, cap, states, states_cap, out_len,
            )
        }
    })
}

/// Cuerpo común de las lecturas tolerantes.
///
/// # Safety
/// `buf` con `cap` bytes y `states` con `states_cap` bytes escribibles; `out_len` válido o nulo.
#[allow(clippy::too_many_arguments)]
unsafe fn read_mapped(
    source_h: u64,
    lba: u64,
    count: u32,
    opts: &mr_api::MappedReadOptions,
    buf: *mut u8,
    cap: usize,
    states: *mut u8,
    states_cap: usize,
    out_len: *mut usize,
) -> i32 {
    if opts.retries > MAX_RETRIES {
        return fail(
            MR_ERR_INVALID_ARGUMENT,
            "demasiados reintentos (máximo 100)",
        );
    }
    if let Err(e) = mr_io::resilient::validate_fill_patterns(opts.bad_fill, opts.skip_fill) {
        return fail(MR_ERR_INVALID_ARGUMENT, e.to_string());
    }
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
    let n = count as usize;
    if buf.is_null() || cap < len || states.is_null() || states_cap < n {
        return fail(
            MR_ERR_BUFFER_TOO_SMALL,
            format!("se necesitan {len} bytes de datos y {n} de estados"),
        );
    }
    let (data, st) = match mr_api::read_sectors_mapped_with(s.as_ref(), lba, u64::from(count), opts)
    {
        Ok(v) => v,
        Err(e) => return api_fail(&e),
    };
    if data.len() != len || st.len() != n {
        return fail(MR_ERR_IO, "lectura incompleta del motor");
    }
    // SAFETY: `buf` tiene ≥ len bytes y `states` ≥ n bytes escribibles (comprobado arriba).
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf, len);
        for (i, v) in st.iter().enumerate() {
            *states.add(i) = state_code(*v);
        }
    }
    MR_OK
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
    jump_sectors: u64,
    flags: u32,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_VERIFY_READ_BACK != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones de verificación no válidas",
            );
        }
        if retries > MAX_RETRIES {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "demasiados reintentos (máximo 100)",
            );
        }
        let opts = VerifyOptions {
            start_lba,
            count: (count != 0).then_some(count),
            policy: ReadPolicy {
                retries,
                on_error: OnError::FillAndContinue,
                jump_sectors,
                read_back: flags & MR_VERIFY_READ_BACK != 0,
                ..ReadPolicy::default()
            },
        };
        // SAFETY: `out_task` no es nulo.
        unsafe { spawn_verify(source_h, opts, out_task) }
    })
}

/// Lanza la verificación y escribe el handle de la tarea.
///
/// # Safety
/// `out_task` no es nulo y es válido para escritura.
unsafe fn spawn_verify(source_h: u64, opts: VerifyOptions, out_task: *mut u64) -> i32 {
    let src = match source(source_h) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match mr_api::start_verify_task(src, opts) {
        // SAFETY: contrato de la función.
        Ok(task) => unsafe {
            write_out(
                out_task,
                insert(Entry::Verify {
                    task,
                    outcome: None,
                    index: None,
                }),
            )
        },
        Err(e) => api_fail(&e),
    }
}

// ---------- Verificación extendida: política de omisión de errores (IO-09) ----------

/// Convierte y valida las opciones C de la verificación extendida.
///
/// # Safety
/// `o.skip_codes` debe apuntar a `o.skip_code_count` enteros válidos (o ser nulo con 0).
unsafe fn verify_options_ex(o: &MrVerifyOptions) -> Result<VerifyOptions, i32> {
    if o.flags & !(MR_VERIFY_READ_BACK | MR_VERIFY_IGNORE_ALL_ERRORS) != 0 {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "opciones de verificación no válidas",
        ));
    }
    if o.retries > MAX_RETRIES {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "demasiados reintentos (máximo 100)",
        ));
    }
    if o.skip_code_count > MAX_SKIP_CODES {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            format!("demasiados códigos de error a omitir (máximo {MAX_SKIP_CODES})"),
        ));
    }
    let codes: &[i32] = match (o.skip_codes.is_null(), o.skip_code_count) {
        (_, 0) => &[],
        (true, _) => {
            return Err(fail(
                MR_ERR_INVALID_ARGUMENT,
                "lista de códigos nula con recuento distinto de 0",
            ))
        }
        // SAFETY: puntero no nulo con `skip_code_count` elementos por contrato.
        (false, n) => unsafe { std::slice::from_raw_parts(o.skip_codes, n as usize) },
    };
    if let Some(c) = codes.iter().find(|&&c| c <= 0) {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            format!("código de error no válido: {c} (debe ser positivo)"),
        ));
    }
    let mut skip = codes.to_vec();
    skip.sort_unstable();
    skip.dedup();
    Ok(VerifyOptions {
        start_lba: o.start_lba,
        count: (o.count != 0).then_some(o.count),
        policy: ReadPolicy {
            retries: o.retries,
            on_error: OnError::FillAndContinue,
            jump_sectors: o.jump_sectors,
            read_back: o.flags & MR_VERIFY_READ_BACK != 0,
            skip_error_codes: skip,
            ignore_all_errors: o.flags & MR_VERIFY_IGNORE_ALL_ERRORS != 0,
            ..ReadPolicy::default()
        },
    })
}

/// Verificación con opciones extendidas: además de las de `mr_source_verify_start`, la política de
/// omisión de errores (códigos que no se reintentan y modo «ignorar todo»).
///
/// # Safety
/// `options` y `out_task` deben ser punteros válidos; `options.skip_codes` debe apuntar a
/// `options.skip_code_count` enteros (o ser nulo con recuento 0).
#[no_mangle]
pub unsafe extern "C" fn mr_source_verify_start_ex(
    source_h: u64,
    options: *const MrVerifyOptions,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() || options.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero nulo");
        }
        // Se lee primero solo `struct_size`, para no leer más allá de una estructura más pequeña.
        // SAFETY: `options` no es nulo y contiene al menos su primer campo.
        let size = unsafe { options.cast::<u32>().read_unaligned() };
        let expected = std::mem::size_of::<MrVerifyOptions>();
        if size as usize != expected {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("tamaño de mr_verify_options no válido: {size} (se esperaba {expected})"),
            );
        }
        // SAFETY: el tamaño declarado coincide con el de la estructura.
        let o = unsafe { options.read_unaligned() };
        // SAFETY: contrato del llamador sobre `skip_codes`.
        let opts = match unsafe { verify_options_ex(&o) } {
            Ok(v) => v,
            Err(c) => return c,
        };
        // SAFETY: `out_task` no es nulo.
        unsafe { spawn_verify(source_h, opts, out_task) }
    })
}

/// Resumen de la política de errores aplicada: sectores cuyo error se omitió (sin agotar
/// reintentos) y número de códigos de error del sistema distintos registrados.
///
/// # Safety
/// `out_omitted` y `out_code_count` deben ser punteros válidos.
#[no_mangle]
pub unsafe extern "C" fn mr_verify_error_summary(
    task: u64,
    out_omitted: *mut u64,
    out_code_count: *mut u32,
) -> i32 {
    guard(|| {
        if out_omitted.is_null() || out_code_count.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_verify(task, |r, _| {
            (
                r.omitted_errors,
                u32::try_from(r.error_codes.len()).unwrap_or(u32::MAX),
            )
        }) {
            Ok((omitted, n)) => {
                // SAFETY: punteros no nulos y válidos por contrato.
                unsafe {
                    out_omitted.write(omitted);
                    out_code_count.write(n);
                }
                MR_OK
            }
            Err(c) => c,
        }
    })
}

/// Código de error del sistema nº `index` (en orden creciente) y número de sectores ilegibles
/// cuyo último intento falló con él.
///
/// # Safety
/// `out_code` y `out_sectors` deben ser punteros válidos.
#[no_mangle]
pub unsafe extern "C" fn mr_verify_error_code(
    task: u64,
    index: u32,
    out_code: *mut i32,
    out_sectors: *mut u64,
) -> i32 {
    guard(|| {
        if out_code.is_null() || out_sectors.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_verify(task, |_, ix| {
            usize::try_from(index)
                .ok()
                .and_then(|i| ix.codes.get(i).copied())
        }) {
            Ok(Some((code, n))) => {
                // SAFETY: punteros no nulos y válidos por contrato.
                unsafe {
                    out_code.write(code);
                    out_sectors.write(n);
                }
                MR_OK
            }
            Ok(None) => fail(
                MR_ERR_OUT_OF_RANGE,
                format!("índice de código de error fuera de límites: {index}"),
            ),
            Err(c) => c,
        }
    })
}

// ---------- Fin de la verificación extendida ----------

/// Ejecuta `f` sobre el informe de una verificación terminada y sus índices materializados
/// (tramos no fiables y códigos de error), que se construyen una sola vez.
fn with_verify<R>(task: u64, f: impl FnOnce(&ReadReport, &VerifyIndex) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::Verify {
            task,
            outcome,
            index,
        } => settle(task, outcome).map(|()| match outcome {
            Some(Ok(rep)) => Some(f(rep, index.get_or_insert_with(|| VerifyIndex::new(rep)))),
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
        let rep = with_verify(task, |r, ix| MrVerifyReport {
            sectors_requested: r.sectors_requested,
            sectors_read: r.map.count(SectorState::Read),
            sectors_recovered: r.map.count(SectorState::Recovered),
            sectors_unreadable: r.map.count(SectorState::Unreadable),
            sectors_skipped: r.map.count(SectorState::Skipped),
            sectors_sparse: r.map.count(SectorState::Sparse),
            retry_reads: r.retry_reads,
            untrusted_range_count: ix.untrusted.len() as u64,
            ranges_truncated: u32::from(r.map.truncated()),
            reserved: 0,
        });
        match rep {
            // SAFETY: contrato del llamador.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Tramo no fiable número `index` (estado 2 = ilegible, 3 = saltado).
///
/// # Safety
/// `out_lba`, `out_count` y `out_state` deben ser punteros válidos.
#[no_mangle]
pub unsafe extern "C" fn mr_verify_untrusted_range(
    task: u64,
    index: u64,
    out_lba: *mut u64,
    out_count: *mut u64,
    out_state: *mut u32,
) -> i32 {
    guard(|| {
        if out_lba.is_null() || out_count.is_null() || out_state.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let r = with_verify(task, |_, ix| {
            usize::try_from(index)
                .ok()
                .and_then(|i| ix.untrusted.get(i).copied())
        });
        match r {
            Ok(Some((lba, count, state))) => {
                // SAFETY: punteros no nulos y válidos por contrato.
                unsafe {
                    out_lba.write(lba);
                    out_count.write(count);
                    out_state.write(u32::from(state_code(state)));
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
