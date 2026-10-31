//! Interfaz C de `memrescue_core.dll`. Contrato en `include/memrescue_core.h` y `docs/API_FFI.md`.
//!
//! Todo el `unsafe` del motor vive aquí y se limita a: leer punteros de entrada, escribir en
//! búferes del llamador y escribir estructuras de salida. Cada función exportada captura pánicos.
//! Orígenes y tareas viven en un registro indexado por handles numéricos (nunca punteros).

use mr_api::mr_part::{self, Anomaly, Confidence, Origin, PartitionKind, PartitionTable, Scheme};
use mr_api::task::{Task, TaskState};
use mr_api::{
    ApiError, HashOptions, HashResult, OpenOptions, PartitionSearch, SearchOptions, Source,
    VerifyOptions,
};
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
/// Ruta NTFS que no existe (#22).
pub const MR_ERR_NOT_FOUND: i32 = 15;
/// La salida ya existe y no se pidió sustituirla (#22).
pub const MR_ERR_ALREADY_EXISTS: i32 = 16;
/// Un fichero externo aprobado por el usuario no corresponde al contenedor (ADR-018, addendum).
pub const MR_ERR_EXTERNAL_MISMATCH: i32 = 17;

/// `mr_source_open`: leer como RAW aunque parezca un contenedor.
pub const MR_OPEN_FORCE_RAW: u32 = 1;
/// `mr_source_open`: abrir la imagen dividida que empieza en la ruta indicada.
pub const MR_OPEN_SPLIT: u32 = 2;
/// `mr_source_open`: modo tolerante para contenedores con estructuras de asignación parcialmente
/// dañadas (ADR-020). Compatible con la ABI 3: es un bit nuevo de `flags`.
pub const MR_OPEN_TOLERANT: u32 = 4;
/// `mr_source_open_ex`: aceptar ficheros externos cuyo vínculo de identidad con el contenedor no
/// coincide (petición explícita del usuario; la descripción del origen lo advierte).
pub const MR_OPEN_IGNORE_LINK_MISMATCH: u32 = 8;
/// Máximo de elementos de `MrOpenOptions::externals`.
pub const MR_MAX_EXTERNAL_FILES: u32 = 64;
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

// ---------- Particiones (ampliación compatible de la ABI 3, issue #12) ----------

/// `mr_partition_table_info.scheme`.
pub const MR_SCHEME_NONE: u32 = 0;
pub const MR_SCHEME_MBR: u32 = 1;
pub const MR_SCHEME_GPT: u32 = 2;
pub const MR_SCHEME_HYBRID: u32 = 3;
/// Apple Partition Map (#17): LBA en bloques del mapa (`sector_size`).
pub const MR_SCHEME_APM: u32 = 4;
/// Confianza de tablas y particiones.
pub const MR_CONFIDENCE_VALID: u32 = 1;
pub const MR_CONFIDENCE_VALID_WITH_FIXES: u32 = 2;
pub const MR_CONFIDENCE_SUSPICIOUS: u32 = 3;
pub const MR_CONFIDENCE_INVALID: u32 = 4;
/// Gravedad de una anomalía.
pub const MR_SEVERITY_NOTICE: u32 = 1;
pub const MR_SEVERITY_SUSPICIOUS: u32 = 2;
pub const MR_SEVERITY_INVALID: u32 = 3;
/// Daño compensado por una redundancia (copia GPT válida): confianza «válida con correcciones».
pub const MR_SEVERITY_COMPENSATED: u32 = 4;
/// `mr_partition_table_info.gpt_selected`.
pub const MR_GPT_NONE: u32 = 0;
pub const MR_GPT_PRIMARY: u32 = 1;
pub const MR_GPT_BACKUP: u32 = 2;
/// `mr_partition_table_info.gpt_flags`: hay lectura GPT (esquema GPT o híbrido).
pub const MR_GPT_PRESENT: u32 = 1;
/// `mr_partition_open_ex`: aplicar explícitamente el desplazamiento de la imagen a una partición GPT.
pub const MR_PARTITION_OPEN_APPLY_SHIFT: u32 = 1;
/// Origen de una partición.
pub const MR_ORIGIN_PRIMARY: u32 = 1;
pub const MR_ORIGIN_LOGICAL: u32 = 2;
pub const MR_ORIGIN_GPT: u32 = 3;
/// Entrada del Apple Partition Map (`slot` = número de entrada desde 1).
pub const MR_ORIGIN_APM: u32 = 4;
/// `partition_index` de las anomalías de la tabla (no de una partición).
pub const MR_PARTITION_TABLE: u32 = u32::MAX;
/// `mr_partition_anomaly.flags`: `target_lba` / `value` tienen significado.
pub const MR_ANOMALY_HAS_TARGET: u32 = 1;
pub const MR_ANOMALY_HAS_VALUE: u32 = 2;
/// `value` es un entero con signo en complemento a dos (desplazamiento de `image_shifted`).
pub const MR_ANOMALY_VALUE_SIGNED: u32 = 4;

/// Datos de una tabla de particiones (80 bytes). `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrPartitionTableInfo {
    pub struct_size: u32,
    /// `MR_SCHEME_*`.
    pub scheme: u32,
    /// Tamaño de sector con que se expresan las LBA de las particiones.
    pub sector_size: u32,
    /// `MR_CONFIDENCE_*` del conjunto (peor gravedad de todas las anomalías y de la GPT).
    pub confidence: u32,
    pub partition_count: u64,
    /// Anomalías de la tabla (`partition_index = MR_PARTITION_TABLE`).
    pub anomaly_count: u64,
    /// `MR_GPT_NONE` / `MR_GPT_PRIMARY` / `MR_GPT_BACKUP`: copia GPT en uso.
    pub gpt_selected: u32,
    /// `MR_GPT_PRESENT`.
    pub gpt_flags: u32,
    /// `LBA disco = LBA imagen + shift` (0 sin desplazamiento o sin GPT).
    pub shift: i64,
    /// Sectores del disco que faltan al final de la imagen (según la GPT).
    pub trailing_sectors_missing: u64,
    /// Tamaño de sector con que se interpretó la GPT (0 sin GPT).
    pub gpt_sector_size: u32,
    pub reserved: u32,
    /// GUID de disco de la cabecera elegida, en el orden del disco (ceros si no hay).
    pub disk_guid: [u8; 16],
}

/// Una partición (96 bytes). `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrPartitionInfo {
    pub struct_size: u32,
    /// `MR_ORIGIN_*`.
    pub origin: u32,
    /// Entrada en su tabla (0 a 3), índice GPT o número de entrada APM (desde 1).
    pub slot: u32,
    /// Tipo MBR (0 en GPT y APM).
    pub mbr_type: u32,
    /// EBR de una lógica (0 en las demás).
    pub ebr_lba: u64,
    pub start_lba: u64,
    pub sector_count: u64,
    /// 1 = activa (indicador 0x80).
    pub active: u32,
    /// `MR_CONFIDENCE_*`.
    pub confidence: u32,
    /// Anomalías propias de la partición.
    pub anomaly_count: u32,
    pub reserved: u32,
    /// GUID de tipo GPT en el orden del disco (ceros en MBR).
    pub gpt_type: [u8; 16],
    /// GUID único GPT en el orden del disco (ceros en MBR).
    pub unique_guid: [u8; 16],
    /// Atributos GPT de 64 bits; en APM, `pmPartStatus` (0 en MBR).
    pub attributes: u64,
}

/// Una anomalía (40 bytes). `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrPartitionAnomaly {
    pub struct_size: u32,
    /// `MR_ANOMALY_*` (código numérico estable).
    pub code: u32,
    /// `MR_SEVERITY_*`.
    pub severity: u32,
    /// `MR_ANOMALY_HAS_TARGET | MR_ANOMALY_HAS_VALUE`.
    pub flags: u32,
    /// Sector de tabla donde se detectó.
    pub lba: u64,
    pub target_lba: u64,
    pub value: u64,
}

/// Código numérico estable de una anomalía (`MR_ANOMALY_*` en la cabecera).
pub fn anomaly_code(a: &Anomaly) -> u32 {
    use mr_part::mbr::MbrAnomaly as M;
    match a {
        Anomaly::UnreadableSector { .. } => 1,
        Anomaly::Mbr(m) => match m {
            M::NoTable { .. } => 100,
            M::Empty { .. } => 101,
            M::SignatureMissing { .. } => 102,
            M::InvalidBootIndicator { .. } => 103,
            M::MultipleActive { .. } => 104,
            M::MultipleExtended { .. } => 105,
            M::OutsideDisk { .. } => 106,
            M::PartiallyOutsideDisk { .. } => 107,
            M::Overlap { .. } => 108,
            M::UnknownType { .. } => 109,
            M::ZeroLength { .. } => 110,
            M::EbrLoop { .. } => 111,
            M::LinkOutsideDisk { .. } => 112,
            M::LinkOutsideExtended { .. } => 113,
            M::ChainLimit { .. } => 114,
            M::LogicalOutsideExtended { .. } => 115,
            M::EbrInvalid { .. } => 116,
            M::ExtraDataEntries { .. } => 117,
            M::ProtectiveStart { .. } => 118,
            M::ProtectiveSizeMismatch { .. } => 119,
            M::Hybrid { .. } => 120,
            M::Superfloppy { .. } => 121,
            M::HybridMismatch { .. } => 122,
        },
        Anomaly::Gpt(g) => gpt_anomaly_code(g),
        Anomaly::Apm(a) => apm_anomaly_code(a),
    }
}

/// Códigos 400 a 414 (bloque reservado a APM, #17).
fn apm_anomaly_code(a: &mr_part::apm::ApmAnomaly) -> u32 {
    use mr_part::apm::ApmAnomaly as A;
    match a {
        A::DdmMissing => 400,
        A::BlockSizeInvalid { .. } => 401,
        A::BlockSizeMismatch { .. } => 402,
        A::DiskSizeMismatch { .. } => 403,
        A::MapMissing { .. } => 404,
        A::MapCountInvalid { .. } => 405,
        A::MapCountMismatch { .. } => 406,
        A::MapOutsideImage { .. } => 407,
        A::EntrySignature { .. } => 408,
        A::EntryZeroLength { .. } => 409,
        A::EntryOutsideDisk { .. } => 410,
        A::EntryPartiallyOutsideDisk { .. } => 411,
        A::EntryOverlap { .. } => 412,
        A::EntryDataOutside { .. } => 413,
        A::SectorSizeMismatch { .. } => 414,
    }
}

/// Códigos 201 a 241 (bloque reservado a GPT): las anomalías de una copia concreta usan un código
/// para la primaria (impar) y el siguiente para la de respaldo.
fn gpt_anomaly_code(g: &mr_part::gpt::GptAnomaly) -> u32 {
    use mr_part::gpt::{GptAnomaly as G, GptCopy};
    let per_copy = |base: u32, copy: &GptCopy| match copy {
        GptCopy::Primary => base,
        GptCopy::Backup => base + 1,
    };
    match g {
        G::HeaderMissing { copy, .. } => per_copy(201, copy),
        G::HeaderOutsideImage { copy, .. } => per_copy(203, copy),
        G::HeaderCrc { copy, .. } => per_copy(205, copy),
        G::HeaderSize { copy, .. } => per_copy(207, copy),
        G::RevisionUnsupported { copy, .. } => per_copy(209, copy),
        G::RevisionMinor { copy, .. } => per_copy(211, copy),
        G::MyLbaMismatch { copy, .. } => per_copy(213, copy),
        G::AlternateLbaMismatch { copy, .. } => per_copy(215, copy),
        G::UsableRangeInvalid { copy, .. } => per_copy(217, copy),
        G::EntriesLocationInvalid { copy, .. } => per_copy(219, copy),
        G::EntryArrayLimits { copy, .. } => per_copy(221, copy),
        G::EntriesCrc { copy, .. } => per_copy(223, copy),
        G::EntriesOutsideImage { copy, .. } => per_copy(225, copy),
        G::BackupInUse { .. } => 230,
        G::CopiesDisagree { .. } => 231,
        G::ImageShifted { .. } => 232,
        G::ImageTruncated { .. } => 233,
        G::UsableRangeOutsideImage { .. } => 234,
        G::SectorSizeMismatch { .. } => 235,
        G::EntryInverted { .. } => 236,
        G::EntryOutsideUsable { .. } => 237,
        G::EntryOutsideImage { .. } => 238,
        G::EntryOverlap { .. } => 239,
        G::DuplicateUniqueGuid { .. } => 240,
        G::NameInvalid { .. } => 241,
    }
}

fn confidence_code(c: &Confidence) -> u32 {
    match c {
        Confidence::Valid => MR_CONFIDENCE_VALID,
        Confidence::ValidWithFixes(_) => MR_CONFIDENCE_VALID_WITH_FIXES,
        Confidence::Suspicious(_) => MR_CONFIDENCE_SUSPICIOUS,
        Confidence::Invalid(_) => MR_CONFIDENCE_INVALID,
    }
}

fn severity_code(s: mr_part::Severity) -> u32 {
    match s {
        mr_part::Severity::Notice => MR_SEVERITY_NOTICE,
        mr_part::Severity::Compensated => MR_SEVERITY_COMPENSATED,
        mr_part::Severity::Suspicious => MR_SEVERITY_SUSPICIOUS,
        mr_part::Severity::Invalid => MR_SEVERITY_INVALID,
    }
}

fn scheme_code(s: Scheme) -> u32 {
    match s {
        Scheme::None => MR_SCHEME_NONE,
        Scheme::Mbr => MR_SCHEME_MBR,
        Scheme::Gpt => MR_SCHEME_GPT,
        Scheme::Hybrid => MR_SCHEME_HYBRID,
        Scheme::Apm => MR_SCHEME_APM,
    }
}

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

/// Cadena UTF-8 de entrada (puntero y longitud en bytes, sin terminador), 16 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MrStr {
    pub ptr: *const u8,
    pub len: usize,
}

/// Opciones de `mr_source_open_ex` (32 bytes). Mismo esquema de versión que `MrVerifyOptions`:
/// `struct_size` = `sizeof(mr_open_options)` de la cabecera del llamador.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MrOpenOptions {
    pub struct_size: u32,
    /// `MR_OPEN_FORCE_RAW | MR_OPEN_SPLIT | MR_OPEN_TOLERANT | MR_OPEN_IGNORE_LINK_MISMATCH`.
    pub flags: u32,
    /// Tamaño de sector lógico (0 = nativo).
    pub sector_size: u32,
    /// Número de elementos de `externals` (≤ 64).
    pub external_count: u32,
    /// Ficheros externos aprobados por el usuario, en orden (padre inmediato primero); nulo si
    /// `external_count` = 0.
    pub externals: *const MrStr,
    /// Reservado: debe ser 0.
    pub reserved: u64,
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
        IoError::ExternalMismatch { .. } => MR_ERR_EXTERNAL_MISMATCH,
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
        ApiError::Ntfs(mr_api::mr_fs_ntfs::NtfsError::Io(io)) => io_error_code(io),
        ApiError::Ntfs(mr_api::mr_fs_ntfs::NtfsError::RecordOutOfRange { .. }) => {
            MR_ERR_OUT_OF_RANGE
        }
        ApiError::Ntfs(mr_api::mr_fs_ntfs::NtfsError::NotFound { .. }) => MR_ERR_NOT_FOUND,
        ApiError::Ntfs(
            mr_api::mr_fs_ntfs::NtfsError::NotADirectory { .. }
            | mr_api::mr_fs_ntfs::NtfsError::InvalidPath { .. },
        ) => MR_ERR_INVALID_ARGUMENT,
        ApiError::OutputExists(_) => MR_ERR_ALREADY_EXISTS,
        ApiError::Ntfs(_) => MR_ERR_CORRUPT,
        ApiError::Vfs(v) => match v {
            mr_api::mr_vfs::VfsError::LimitExceeded { .. } => MR_ERR_OUT_OF_RANGE,
            mr_api::mr_vfs::VfsError::InvalidFilter(_)
            | mr_api::mr_vfs::VfsError::NoSuchNode(_)
            | mr_api::mr_vfs::VfsError::NotAContainer(_) => MR_ERR_INVALID_ARGUMENT,
            mr_api::mr_vfs::VfsError::Ntfs(mr_api::mr_fs_ntfs::NtfsError::Io(io)) => {
                io_error_code(io)
            }
            mr_api::mr_vfs::VfsError::Ntfs(_) => MR_ERR_CORRUPT,
        },
        ApiError::Cancelled => MR_ERR_CANCELLED,
        // Salida que no se puede comprobar frente al origen (#76): mismo código que «la salida es
        // el origen», sin cambiar la ABI; el mensaje da el motivo.
        ApiError::InvalidArgument(_) | ApiError::OutputNotVerifiable { .. } => {
            MR_ERR_INVALID_ARGUMENT
        }
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
    /// Tabla de particiones leída de un origen (que se conserva para abrir particiones).
    Partitions {
        parent: Source,
        table: Box<PartitionTable>,
    },
    /// Búsqueda de particiones perdidas (el origen se conserva para abrir los volúmenes).
    Search(Box<SearchEntry>),
    /// Volumen NTFS abierto (#22) con su origen.
    Ntfs(Box<ntfs::NtfsEntry>),
    /// Listado de un directorio NTFS.
    NtfsListing(Box<mr_api::mr_fs_ntfs::DirListing>),
    /// Árbol NTFS en segundo plano.
    NtfsTree(Box<ntfs::TreeEntry>),
    /// Copia de un fichero NTFS en segundo plano.
    NtfsCopy(Box<ntfs::CopyEntry>),
    /// Extensiones de un descriptor VMDK separado (#11); no abre ningún fichero.
    VmdkExtents(Box<mr_api::VmdkExtentPlan>),
    /// Búsqueda de borrados NTFS en segundo plano (#23).
    NtfsDeleted(Box<ntfs::DeletedEntryTask>),
    /// Recuperación de un borrado NTFS en segundo plano (#23).
    NtfsRecover(Box<ntfs::RecoverEntry>),
    /// Construcción del árbol virtual en segundo plano (#25).
    VfsBuild(Box<vfs::BuildEntry>),
    /// Árbol virtual terminado.
    Vfs(Arc<mr_api::mr_vfs::Vfs>),
    /// Resultado de una consulta sobre el árbol virtual.
    VfsQuery(Vec<mr_api::mr_vfs::NodeId>),
}

/// Tarea de búsqueda de particiones con su resultado y el origen en que se hizo.
struct SearchEntry {
    task: Arc<Task<PartitionSearch>>,
    outcome: Outcome<PartitionSearch>,
    parent: Source,
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

/// Abre un origen con opciones versionadas y la lista de **ficheros externos aprobados** por el
/// usuario (hoy, la cadena de padres de un disco diferencial VHD/VHDX, del inmediato al base).
/// `flags` admite los de `mr_source_open` más `MR_OPEN_IGNORE_LINK_MISMATCH`; con
/// `MR_OPEN_SPLIT` no se admiten ficheros externos. Ampliación compatible de la ABI 3.
///
/// # Safety
/// `path` UTF-8 de `path_len` bytes; `options` apunta a un `MrOpenOptions` con `struct_size`
/// escrito; `externals` con `external_count` elementos válidos (o nulo si es 0), cada uno con una
/// cadena UTF-8 legible; `out_source` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_open_ex(
    path: *const u8,
    path_len: usize,
    options: *const MrOpenOptions,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() || options.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero nulo");
        }
        // Se lee primero solo `struct_size`, para no leer más allá de una estructura más pequeña.
        // SAFETY: `options` no es nulo y contiene al menos su primer campo.
        let size = unsafe { options.cast::<u32>().read_unaligned() };
        let expected = std::mem::size_of::<MrOpenOptions>();
        if size as usize != expected {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("tamaño de mr_open_options no válido: {size} (se esperaba {expected})"),
            );
        }
        // SAFETY: el tamaño declarado coincide con el de la estructura.
        let o = unsafe { options.read_unaligned() };
        let known =
            MR_OPEN_FORCE_RAW | MR_OPEN_SPLIT | MR_OPEN_TOLERANT | MR_OPEN_IGNORE_LINK_MISMATCH;
        if o.flags & !known != 0 || o.reserved != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones de apertura no válidas");
        }
        let count = o.external_count as usize;
        if count > MR_MAX_EXTERNAL_FILES as usize {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("demasiados ficheros externos ({count})"),
            );
        }
        if count > 0 && o.externals.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "lista de ficheros externos nula");
        }
        if count > 0 && o.flags & MR_OPEN_SPLIT != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "una imagen dividida no admite ficheros externos",
            );
        }
        let mut paths: Vec<&str> = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: `externals` tiene `count` elementos por contrato.
            let s = unsafe { o.externals.add(i).read_unaligned() };
            // SAFETY: contrato del llamador sobre cada cadena.
            match unsafe { read_utf8(s.ptr, s.len) } {
                Ok(p) if !p.is_empty() => paths.push(p),
                Ok(_) => return fail(MR_ERR_INVALID_ARGUMENT, "ruta de fichero externo vacía"),
                Err(c) => return c,
            }
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let opts = OpenOptions {
            sector_size: o.sector_size,
            force_raw: o.flags & MR_OPEN_FORCE_RAW != 0,
            tolerant: o.flags & MR_OPEN_TOLERANT != 0,
        };
        let r = if o.flags & MR_OPEN_SPLIT != 0 {
            mr_api::open_split_source(p, opts)
        } else {
            mr_api::open_with_parents(p, opts, &paths, o.flags & MR_OPEN_IGNORE_LINK_MISMATCH != 0)
        };
        match r {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Indicaciones del contenedor sobre los ficheros externos que necesita (padre de un disco
/// diferencial VHD/VHDX), **sin abrir ningún otro fichero**: una línea `clave\tvalor` por
/// indicación (valores saneados: sin caracteres de control). Cadena vacía y `out_count` = 0 si
/// la imagen no necesita ficheros externos.
///
/// # Safety
/// `path` UTF-8 de `path_len` bytes; contrato de cadenas de salida; `out_count` válido o nulo.
#[no_mangle]
pub unsafe extern "C" fn mr_image_external_hints(
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
        let hints = match mr_api::external_hints(p) {
            Ok(h) => h,
            Err(e) => return api_fail(&e),
        };
        if !out_count.is_null() {
            // SAFETY: puntero válido por contrato.
            unsafe { *out_count = u32::try_from(hints.len()).unwrap_or(u32::MAX) };
        }
        let text = hints
            .iter()
            .map(|h| format!("{}\t{}", h.key, h.value))
            .collect::<Vec<_>>()
            .join("\n");
        // SAFETY: contrato del llamador.
        unsafe { write_str(&text, buf, cap, out_len) }
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

// ---------- Particiones ----------

/// Lee la tabla de particiones del origen y devuelve un handle para consultarla por índice. La
/// lectura es tolerante: un sector ilegible se informa como anomalía, no como error.
///
/// # Safety
/// `out_table` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_partitions(source_h: u64, out_table: *mut u64) -> i32 {
    guard(|| {
        if out_table.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let src = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        match mr_api::partitions(src.as_ref()) {
            // SAFETY: `out_table` no es nulo.
            Ok(table) => unsafe {
                write_out(
                    out_table,
                    insert(Entry::Partitions {
                        parent: src,
                        table: Box::new(table),
                    }),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Ejecuta `f` sobre una tabla de particiones registrada.
fn with_partitions<R>(h: u64, f: impl FnOnce(&Source, &PartitionTable) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::Partitions { parent, table } => Some(f(parent, table)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de tabla de particiones no válido: {h}"),
        )),
    }
}

/// Comprueba el `struct_size` que el llamador escribió en una estructura de salida versionada.
///
/// # Safety
/// `out` debe ser nulo o apuntar a una estructura escribible que empiece por `struct_size`.
unsafe fn check_struct_size<T>(out: *mut T, name: &str) -> Result<(), i32> {
    if out.is_null() {
        return Err(fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo"));
    }
    // SAFETY: no nulo; el primer campo de toda estructura versionada es `struct_size` (u32).
    let size = unsafe { out.cast::<u32>().read_unaligned() };
    let expected = std::mem::size_of::<T>();
    if size as usize != expected {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            format!("tamaño de {name} no válido: {size} (se esperaba {expected})"),
        ));
    }
    Ok(())
}

fn index_error(what: &str, index: u64) -> i32 {
    fail(
        MR_ERR_OUT_OF_RANGE,
        format!("índice de {what} fuera de límites: {index}"),
    )
}

/// Esquema, tamaño de sector, confianza y recuentos de la tabla.
///
/// # Safety
/// `out` debe apuntar a un `MrPartitionTableInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_partitions_info(table_h: u64, out: *mut MrPartitionTableInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_partition_table_info") } {
            return c;
        }
        match with_partitions(table_h, |_, t| {
            let g = t.gpt.as_ref();
            MrPartitionTableInfo {
                struct_size: std::mem::size_of::<MrPartitionTableInfo>() as u32,
                scheme: scheme_code(t.scheme),
                sector_size: t.sector_size,
                confidence: confidence_code(&t.confidence()),
                partition_count: t.partitions.len() as u64,
                anomaly_count: t.anomalies.len() as u64,
                gpt_selected: match g.and_then(|g| g.selected) {
                    None => MR_GPT_NONE,
                    Some(mr_part::gpt::GptCopy::Primary) => MR_GPT_PRIMARY,
                    Some(mr_part::gpt::GptCopy::Backup) => MR_GPT_BACKUP,
                },
                gpt_flags: if g.is_some() { MR_GPT_PRESENT } else { 0 },
                shift: t.shift(),
                trailing_sectors_missing: g.map_or(0, |g| g.trailing_sectors_missing),
                gpt_sector_size: g.map_or(0, |g| g.sector_size),
                reserved: 0,
                disk_guid: g.and_then(|g| g.header()).map_or([0; 16], |h| h.disk_guid),
            }
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Partición número `index` (orden: primarias y después lógicas en orden de cadena).
///
/// # Safety
/// `out` debe apuntar a un `MrPartitionInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_get(
    table_h: u64,
    index: u32,
    out: *mut MrPartitionInfo,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_partition_info") } {
            return c;
        }
        let r = with_partitions(table_h, |_, t| {
            let p = t.partitions.get(index as usize)?;
            let (origin, slot, ebr_lba) = match p.origin {
                Origin::Primary { slot } => (MR_ORIGIN_PRIMARY, u32::from(slot), 0),
                Origin::Logical { ebr_lba, slot } => (MR_ORIGIN_LOGICAL, u32::from(slot), ebr_lba),
                Origin::Gpt { index } => (MR_ORIGIN_GPT, index, 0),
                Origin::Apm { index } => (MR_ORIGIN_APM, index, 0),
            };
            let (mbr_type, gpt_type) = match p.kind {
                PartitionKind::Mbr(t) => (u32::from(t), [0u8; 16]),
                PartitionKind::Gpt(g) => (0, g),
                PartitionKind::Apm(_) => (0, [0u8; 16]),
            };
            let extra = t.gpt_entry(p);
            Some(MrPartitionInfo {
                struct_size: std::mem::size_of::<MrPartitionInfo>() as u32,
                origin,
                slot,
                mbr_type,
                ebr_lba,
                start_lba: p.start_lba,
                sector_count: p.sector_count,
                active: u32::from(p.active),
                confidence: confidence_code(&p.confidence),
                anomaly_count: u32::try_from(p.anomalies.len()).unwrap_or(u32::MAX),
                reserved: 0,
                gpt_type,
                unique_guid: extra.map_or([0; 16], |e| e.unique_guid),
                // En APM, `pmPartStatus`.
                attributes: extra.map_or_else(
                    || t.apm_entry(p).map_or(0, |a| u64::from(a.status)),
                    |e| e.attributes,
                ),
            })
        });
        match r {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("partición", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Nombre legible del tipo de la partición (cadena vacía si es desconocido).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_type_name(
    table_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_partitions(table_h, |_, t| {
            t.partitions
                .get(index as usize)
                .map(|p| p.kind.type_name().unwrap_or("").to_string())
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(name)) => unsafe { write_str(&name, buf, cap, out_len) },
            Ok(None) => index_error("partición", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Nombre GPT o APM de la partición (cadena vacía en MBR).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_name(
    table_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_partitions(table_h, |_, t| {
            t.partitions
                .get(index as usize)
                .map(|p| p.name.clone().unwrap_or_default())
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(name)) => unsafe { write_str(&name, buf, cap, out_len) },
            Ok(None) => index_error("partición", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Tipo APM de la partición tal como está en el mapa (`pmParType`, p. ej. `Apple_HFS`); cadena
/// vacía si no es una entrada APM (#17).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_apm_type(
    table_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_partitions(table_h, |_, t| {
            t.partitions.get(index as usize).map(|p| match p.kind {
                PartitionKind::Apm(k) => k.text(),
                _ => String::new(),
            })
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(kind)) => unsafe { write_str(&kind, buf, cap, out_len) },
            Ok(None) => index_error("partición", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Anomalía `anomaly_index` de la partición `partition_index` o, con `MR_PARTITION_TABLE`, de la
/// tabla.
fn anomaly_at(t: &PartitionTable, partition_index: u32, anomaly_index: u32) -> Option<&Anomaly> {
    let list = if partition_index == MR_PARTITION_TABLE {
        &t.anomalies
    } else {
        &t.partitions.get(partition_index as usize)?.anomalies
    };
    list.get(anomaly_index as usize)
}

/// Anomalía de una partición o de la tabla (`partition_index = MR_PARTITION_TABLE`).
///
/// # Safety
/// `out` debe apuntar a un `MrPartitionAnomaly` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_anomaly(
    table_h: u64,
    partition_index: u32,
    anomaly_index: u32,
    out: *mut MrPartitionAnomaly,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_partition_anomaly") } {
            return c;
        }
        let r = with_partitions(table_h, |_, t| {
            anomaly_at(t, partition_index, anomaly_index).map(|a| {
                let (target, value) = (a.target(), a.value());
                let mut flags = 0;
                if target.is_some() {
                    flags |= MR_ANOMALY_HAS_TARGET;
                }
                if value.is_some() {
                    flags |= MR_ANOMALY_HAS_VALUE;
                }
                if a.value_signed() {
                    flags |= MR_ANOMALY_VALUE_SIGNED;
                }
                MrPartitionAnomaly {
                    struct_size: std::mem::size_of::<MrPartitionAnomaly>() as u32,
                    code: anomaly_code(a),
                    severity: severity_code(a.severity()),
                    flags,
                    lba: a.lba().unwrap_or(0),
                    target_lba: target.unwrap_or(0),
                    value: value.unwrap_or(0),
                }
            })
        });
        match r {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("anomalía", u64::from(anomaly_index)),
            Err(c) => c,
        }
    })
}

/// Código textual estable y descripción legible de una anomalía, separados por un salto de línea:
/// `"<código>\n<descripción>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_anomaly_text(
    table_h: u64,
    partition_index: u32,
    anomaly_index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_partitions(table_h, |_, t| {
            anomaly_at(t, partition_index, anomaly_index).map(|a| format!("{}\n{a}", a.code()))
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(text)) => unsafe { write_str(&text, buf, cap, out_len) },
            Ok(None) => index_error("anomalía", u64::from(anomaly_index)),
            Err(c) => c,
        }
    })
}

/// Abre la partición `index` como origen independiente (vista acotada del origen de la tabla; si
/// termina fuera del origen se abre la parte existente). Una partición GPT de una imagen desplazada
/// se rechaza (`MR_ERR_INVALID_ARGUMENT`): use `mr_partition_open_ex` con
/// `MR_PARTITION_OPEN_APPLY_SHIFT`. Se cierra con `mr_source_close`.
///
/// # Safety
/// `out_source` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_open(table_h: u64, index: u32, out_source: *mut u64) -> i32 {
    // SAFETY: mismo contrato.
    unsafe { mr_partition_open_ex(table_h, index, 0, out_source) }
}

/// Como `mr_partition_open`; con `MR_PARTITION_OPEN_APPLY_SHIFT` una partición GPT de una imagen
/// desplazada se abre en `LBA disco - shift` y `mr_source_describe` indica el desplazamiento
/// aplicado. Otros bits → `MR_ERR_INVALID_ARGUMENT`.
///
/// # Safety
/// `out_source` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_open_ex(
    table_h: u64,
    index: u32,
    flags: u32,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_PARTITION_OPEN_APPLY_SHIFT != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones de apertura de partición no válidas",
            );
        }
        let r = with_partitions(table_h, |parent, t| {
            ((index as usize) < t.partitions.len()).then(|| (Arc::clone(parent), t.clone()))
        });
        let (parent, table) = match r {
            Ok(Some(v)) => v,
            Ok(None) => return index_error("partición", u64::from(index)),
            Err(c) => return c,
        };
        let apply = flags & MR_PARTITION_OPEN_APPLY_SHIFT != 0;
        match mr_api::open_partition(&parent, &table, index as usize, apply) {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Libera una tabla de particiones (los orígenes abiertos desde ella siguen siendo válidos).
#[no_mangle]
pub extern "C" fn mr_partitions_free(table_h: u64) -> i32 {
    guard(|| {
        let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&table_h) {
            Some(Entry::Partitions { .. }) => {
                map.remove(&table_h);
                MR_OK
            }
            _ => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de tabla de particiones no válido: {table_h}"),
            ),
        }
    })
}

// ---------- Búsqueda exprés de particiones perdidas (#15, ADR-023) ----------

/// Opciones de `mr_source_partition_search_start` (64 bytes). `struct_size` lo escribe el
/// llamador; un campo a 0 toma el valor por defecto documentado.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrPartitionSearchOptions {
    pub struct_size: u32,
    /// `MR_SEARCH_FAMILY_*`; 0 = todas.
    pub families: u32,
    /// Lecturas físicas máximas (0 = 4096).
    pub max_reads: u64,
    /// Bytes leídos máximos (0 = 32 MiB).
    pub max_bytes: u64,
    /// Ventana inicial con múltiplos de 1 MiB (0 = 64 MiB).
    pub start_grid_bytes: u64,
    /// Ventana final con finales de MiB (0 = 16 MiB).
    pub end_grid_bytes: u64,
    /// Volúmenes tras los que se deja de buscar (0 = 256).
    pub max_volumes: u32,
    /// Bytes leídos por candidato (0 = 8192; múltiplo de 512).
    pub probe_window: u32,
    /// Límites de cilindro CHS (0 = 64).
    pub cylinders: u32,
    /// Candidatos por hueco (0 = 8).
    pub probes_per_gap: u32,
    /// Reservado: debe ser 0.
    pub flags: u32,
    pub reserved: u32,
}

/// Resumen del resultado de una búsqueda (96 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrPartitionSearchInfo {
    pub struct_size: u32,
    /// `MR_SEARCH_COMPLETE` / `MR_SEARCH_BUDGET_EXHAUSTED` / `MR_SEARCH_CANCELLED`.
    pub status: u32,
    pub volume_count: u32,
    /// Tablas encontradas fuera de la posición de la tabla principal.
    pub table_count: u32,
    /// Anomalías de la búsqueda (`volume_index = MR_PARTITION_TABLE`).
    pub anomaly_count: u32,
    /// `MR_SEARCH_HAS_SHIFT | MR_SEARCH_VOLUME_LIMIT`.
    pub flags: u32,
    /// Desplazamiento aceptado en bytes (`posición imagen - posición disco`); 0 si no hay.
    pub shift_bytes: i64,
    pub reads: u64,
    pub max_reads: u64,
    pub bytes_read: u64,
    pub unverifiable_sectors: u64,
    /// Candidatos sondeados.
    pub probed: u64,
    /// Candidatos que quedaron sin sondear.
    pub pending: u64,
    pub sector_size: u32,
    pub reserved: u32,
    pub sectors: u64,
}

/// Un volumen encontrado (136 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrFoundVolume {
    pub struct_size: u32,
    /// `MR_FS_*`.
    pub fs: u32,
    /// Primer byte en la imagen (negativo: empieza antes de la imagen).
    pub start_byte: i64,
    pub length_bytes: u64,
    /// LBA del origen (redondeo hacia abajo; negativa si es parcial).
    pub start_lba: i64,
    pub sector_count: u64,
    pub fs_sector_size: u32,
    /// `MR_CONFIDENCE_*`.
    pub confidence: u32,
    /// Indicadores `MR_CHECK_*`: principal, copia, estructuras, entrada, autoubicación, alineación.
    pub primary: u32,
    pub backup: u32,
    pub structures: u32,
    pub entry: u32,
    pub self_location: u32,
    pub aligned: u32,
    /// Nivel del candidato que lo encontró (0 a 4 = P0 a P4).
    pub tier: u32,
    /// `MR_FOUND_*`.
    pub flags: u32,
    /// Partición de la tabla del origen que lo describe (`MR_PARTITION_TABLE` si ninguna).
    pub table_index: u32,
    /// Tabla encontrada que lo describe (`MR_PARTITION_TABLE` si ninguna).
    pub found_table_index: u32,
    pub anomaly_count: u32,
    pub suggested_mbr_type: u32,
    pub primary_byte: i64,
    /// Posición de la copia usada o esperada (válida con `MR_FOUND_HAS_BACKUP`).
    pub backup_byte: i64,
    /// Inicio declarado por el volumen en sus sectores (0 = no declarado).
    pub declared_start: u64,
    pub serial: u64,
    pub score: u32,
    pub reserved: u32,
}

/// Una tabla encontrada (32 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrFoundTable {
    pub struct_size: u32,
    /// `MR_FOUND_TABLE_MBR` / `MR_FOUND_TABLE_GPT`.
    pub kind: u32,
    pub lba: u64,
    /// `MyLBA` de una cabecera GPT (0 en MBR).
    pub declared_lba: u64,
    pub entry_count: u32,
    pub reserved: u32,
}

pub const MR_SEARCH_FAMILY_NTFS: u32 = mr_part::search::family::NTFS;
pub const MR_SEARCH_FAMILY_FAT: u32 = mr_part::search::family::FAT;
pub const MR_SEARCH_FAMILY_EXFAT: u32 = mr_part::search::family::EXFAT;
pub const MR_SEARCH_FAMILY_MBR: u32 = mr_part::search::family::MBR;
pub const MR_SEARCH_FAMILY_GPT: u32 = mr_part::search::family::GPT;
pub const MR_SEARCH_COMPLETE: u32 = 1;
pub const MR_SEARCH_BUDGET_EXHAUSTED: u32 = 2;
pub const MR_SEARCH_CANCELLED: u32 = 3;
pub const MR_SEARCH_HAS_SHIFT: u32 = 1;
pub const MR_SEARCH_VOLUME_LIMIT: u32 = 2;
pub const MR_FS_NTFS: u32 = 1;
pub const MR_FS_FAT12: u32 = 2;
pub const MR_FS_FAT16: u32 = 3;
pub const MR_FS_FAT32: u32 = 4;
pub const MR_FS_EXFAT: u32 = 5;
pub const MR_FOUND_PARTIAL: u32 = 1;
pub const MR_FOUND_LISTED: u32 = 2;
pub const MR_FOUND_REQUIRES_SHIFT: u32 = 4;
pub const MR_FOUND_HAS_BACKUP: u32 = 8;
pub const MR_FOUND_TABLE_MBR: u32 = 1;
pub const MR_FOUND_TABLE_GPT: u32 = 2;
/// `mr_partition_anomaly.flags` de una anomalía de búsqueda: `lba` es `int64_t`.
pub const MR_ANOMALY_LBA_SIGNED: u32 = 8;

fn fs_code(fs: mr_part::search::FsKind) -> u32 {
    use mr_part::search::FsKind::*;
    match fs {
        Ntfs => MR_FS_NTFS,
        Fat12 => MR_FS_FAT12,
        Fat16 => MR_FS_FAT16,
        Fat32 => MR_FS_FAT32,
        ExFat => MR_FS_EXFAT,
    }
}

/// Convierte las opciones C (0 = por defecto) y las valida.
fn search_options(o: &MrPartitionSearchOptions) -> Result<SearchOptions, i32> {
    if o.flags != 0 || o.reserved != 0 {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "opciones de búsqueda no válidas (flags y reserved deben ser 0)",
        ));
    }
    let d = SearchOptions::default();
    let pick64 = |v: u64, def: u64| if v == 0 { def } else { v };
    let pick32 = |v: u32, def: u32| if v == 0 { def } else { v };
    let opts = SearchOptions {
        max_reads: pick64(o.max_reads, d.max_reads),
        max_bytes: pick64(o.max_bytes, d.max_bytes),
        max_volumes: pick32(o.max_volumes, d.max_volumes),
        probe_window: pick32(o.probe_window, d.probe_window),
        start_grid_bytes: pick64(o.start_grid_bytes, d.start_grid_bytes),
        end_grid_bytes: pick64(o.end_grid_bytes, d.end_grid_bytes),
        cylinders: pick32(o.cylinders, d.cylinders),
        probes_per_gap: pick32(o.probes_per_gap, d.probes_per_gap),
        families: pick32(o.families, d.families),
    };
    opts.validate().map_err(|e| {
        fail(
            MR_ERR_INVALID_ARGUMENT,
            format!("opciones de búsqueda: {e}"),
        )
    })?;
    Ok(opts)
}

/// Lanza la búsqueda exprés de particiones perdidas sobre el origen (lee antes su tabla). Solo se
/// ejecuta a petición. `options` puede ser nulo (valores por defecto); si no, su `struct_size`
/// debe ser `sizeof(mr_partition_search_options)`. Progreso de la tarea: lecturas hechas respecto a
/// `max_reads`. Al cancelar, la tarea termina en estado cancelado **con** resultado parcial.
///
/// # Safety
/// `options` debe ser nulo o válido; `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_partition_search_start(
    source_h: u64,
    options: *const MrPartitionSearchOptions,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let opts = if options.is_null() {
            SearchOptions::default()
        } else {
            // SAFETY: no nulo; se lee primero solo `struct_size`.
            let size = unsafe { options.cast::<u32>().read_unaligned() };
            let expected = std::mem::size_of::<MrPartitionSearchOptions>();
            if size as usize != expected {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    format!(
                        "tamaño de mr_partition_search_options no válido: {size} (se esperaba {expected})"
                    ),
                );
            }
            // SAFETY: el tamaño declarado coincide con el de la estructura.
            let o = unsafe { options.read_unaligned() };
            match search_options(&o) {
                Ok(v) => v,
                Err(c) => return c,
            }
        };
        let src = match source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        match mr_api::start_partition_search_task(Arc::clone(&src), opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::Search(Box::new(SearchEntry {
                        task,
                        outcome: None,
                        parent: src,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Ejecuta `f` sobre el resultado de una búsqueda terminada (también si se canceló con
/// resultado parcial).
fn with_search<R>(task: u64, f: impl FnOnce(&Source, &PartitionSearch) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::Search(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
            Some(Ok(s)) => Some(f(&e.parent, s)),
            _ => None,
        }),
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es una búsqueda de particiones"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(Some(v))) => Ok(v),
        Some(Ok(None)) => Err(fail(MR_ERR_NOT_FINISHED, "resultado no disponible")),
    }
}

/// Resumen del resultado (estado, recuentos, presupuesto gastado, desplazamiento).
///
/// # Safety
/// `out` debe apuntar a un `MrPartitionSearchInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_partition_search_result(
    task: u64,
    out: *mut MrPartitionSearchInfo,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_partition_search_info") } {
            return c;
        }
        match with_search(task, |_, s| {
            let r = &s.result;
            let mut flags = 0;
            if r.shift_bytes.is_some() {
                flags |= MR_SEARCH_HAS_SHIFT;
            }
            if r.volume_limit_reached {
                flags |= MR_SEARCH_VOLUME_LIMIT;
            }
            MrPartitionSearchInfo {
                struct_size: std::mem::size_of::<MrPartitionSearchInfo>() as u32,
                status: r.status.number(),
                volume_count: r.volumes.len() as u32,
                table_count: r.tables.len() as u32,
                anomaly_count: r.anomalies.len() as u32,
                flags,
                shift_bytes: r.shift_bytes.unwrap_or(0),
                reads: r.stats.reads,
                max_reads: r.options.max_reads,
                bytes_read: r.stats.bytes,
                unverifiable_sectors: r.stats.unverifiable_sectors,
                probed: r.probed,
                pending: r.pending.iter().sum(),
                sector_size: r.sector_size,
                reserved: 0,
                sectors: r.sectors,
            }
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Volumen encontrado número `index` (orden por posición).
///
/// # Safety
/// `out` debe apuntar a un `MrFoundVolume` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_found_volume_get(
    task: u64,
    index: u32,
    out: *mut MrFoundVolume,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_found_volume") } {
            return c;
        }
        let r = with_search(task, |_, s| {
            let r = &s.result;
            let v = r.volumes.get(index as usize)?;
            let mut flags = 0;
            if v.partial() {
                flags |= MR_FOUND_PARTIAL;
            }
            if v.listed() {
                flags |= MR_FOUND_LISTED;
            }
            if v.requires_shift() {
                flags |= MR_FOUND_REQUIRES_SHIFT;
            }
            if v.backup_byte.is_some() {
                flags |= MR_FOUND_HAS_BACKUP;
            }
            let idx = |i: Option<usize>| i.map_or(MR_PARTITION_TABLE, |i| i as u32);
            Some(MrFoundVolume {
                struct_size: std::mem::size_of::<MrFoundVolume>() as u32,
                fs: fs_code(v.fs),
                start_byte: v.start_byte,
                length_bytes: v.length_bytes,
                start_lba: v.start_lba(r.sector_size),
                sector_count: v.sector_count(r.sector_size),
                fs_sector_size: v.fs_sector_size,
                confidence: confidence_code(&v.confidence),
                primary: v.primary.number(),
                backup: v.backup.number(),
                structures: v.structures.number(),
                entry: v.entry.number(),
                self_location: v.self_location.number(),
                aligned: v.aligned.number(),
                tier: v.tier.index() as u32,
                flags,
                table_index: idx(v.table_partition),
                found_table_index: idx(v.found_table),
                anomaly_count: u32::try_from(v.anomalies.len()).unwrap_or(u32::MAX),
                suggested_mbr_type: u32::from(v.fs.suggested_mbr_type()),
                primary_byte: v.primary_byte,
                backup_byte: v.backup_byte.unwrap_or(0),
                declared_start: v.declared_start,
                serial: v.serial,
                score: v.score,
                reserved: 0,
            })
        });
        match r {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("volumen encontrado", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Origen del volumen: qué candidato lo propuso (texto legible).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_found_volume_origin(
    task: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_search(task, |_, s| {
            s.result
                .volumes
                .get(index as usize)
                .map(|v| v.origin.clone())
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("volumen encontrado", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Anomalía de búsqueda como `(código, gravedad, lba con signo, valor con signo, texto)`.
type SearchAnomalyRow = (u32, mr_part::Severity, Option<i64>, Option<i64>, String);

fn search_anomaly_at(s: &PartitionSearch, vi: u32, ai: u32) -> Option<SearchAnomalyRow> {
    let r = &s.result;
    let ss = i64::from(r.sector_size);
    if vi == MR_PARTITION_TABLE {
        let a = r.anomalies.get(ai as usize)?;
        return Some((
            a.number(),
            a.severity(),
            Some(a.lba()),
            a.value(),
            format!("{}\n{a}", a.code()),
        ));
    }
    let a = r.volumes.get(vi as usize)?.anomalies.get(ai as usize)?;
    let lba = a
        .lba()
        .map(|l| l as i64)
        .or_else(|| a.byte().map(|b| b.div_euclid(ss)));
    Some((
        a.number(),
        a.severity(),
        lba,
        a.value(),
        format!("{}\n{a}", a.code()),
    ))
}

/// Anomalía `anomaly_index` del volumen `volume_index` o, con `MR_PARTITION_TABLE`, de la búsqueda
/// (`image_shifted`, `mbr_displaced`). `lba` es `int64_t` (`MR_ANOMALY_LBA_SIGNED`) y `value`
/// también (`MR_ANOMALY_VALUE_SIGNED`).
///
/// # Safety
/// `out` debe apuntar a un `MrPartitionAnomaly` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_found_volume_anomaly(
    task: u64,
    volume_index: u32,
    anomaly_index: u32,
    out: *mut MrPartitionAnomaly,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_partition_anomaly") } {
            return c;
        }
        let r = with_search(task, |_, s| {
            search_anomaly_at(s, volume_index, anomaly_index).map(|(code, sev, lba, value, _)| {
                let mut flags = MR_ANOMALY_LBA_SIGNED;
                if value.is_some() {
                    flags |= MR_ANOMALY_HAS_VALUE | MR_ANOMALY_VALUE_SIGNED;
                }
                MrPartitionAnomaly {
                    struct_size: std::mem::size_of::<MrPartitionAnomaly>() as u32,
                    code,
                    severity: severity_code(sev),
                    flags,
                    lba: lba.unwrap_or(0) as u64,
                    target_lba: 0,
                    value: value.unwrap_or(0) as u64,
                }
            })
        });
        match r {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("anomalía", u64::from(anomaly_index)),
            Err(c) => c,
        }
    })
}

/// `"<código>\n<descripción>"` de una anomalía de volumen o de la búsqueda.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_found_volume_anomaly_text(
    task: u64,
    volume_index: u32,
    anomaly_index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let r = with_search(task, |_, s| {
            search_anomaly_at(s, volume_index, anomaly_index).map(|a| a.4)
        });
        match r {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("anomalía", u64::from(anomaly_index)),
            Err(c) => c,
        }
    })
}

/// Tabla encontrada número `index`.
///
/// # Safety
/// `out` debe apuntar a un `MrFoundTable` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_found_table_get(task: u64, index: u32, out: *mut MrFoundTable) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_found_table") } {
            return c;
        }
        let r = with_search(task, |_, s| {
            s.result.tables.get(index as usize).map(|t| MrFoundTable {
                struct_size: std::mem::size_of::<MrFoundTable>() as u32,
                kind: match t.kind {
                    mr_part::search::FoundTableKind::Mbr => MR_FOUND_TABLE_MBR,
                    mr_part::search::FoundTableKind::Gpt => MR_FOUND_TABLE_GPT,
                },
                lba: t.lba,
                declared_lba: t.declared_lba,
                entry_count: t.entries.len() as u32,
                reserved: 0,
            })
        });
        match r {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("tabla encontrada", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Abre el volumen encontrado `index` como origen (vista acotada del origen de la búsqueda). Un
/// volumen parcial o desalineado se rechaza siempre; uno que depende del desplazamiento detectado
/// solo con `MR_PARTITION_OPEN_APPLY_SHIFT`. Se cierra con `mr_source_close`.
///
/// # Safety
/// `out_source` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_found_volume_open(
    task: u64,
    index: u32,
    flags: u32,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_PARTITION_OPEN_APPLY_SHIFT != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones de apertura de volumen no válidas",
            );
        }
        let r = with_search(task, |parent, s| {
            ((index as usize) < s.result.volumes.len())
                .then(|| (Arc::clone(parent), s.result.clone()))
        });
        let (parent, result) = match r {
            Ok(Some(v)) => v,
            Ok(None) => return index_error("volumen encontrado", u64::from(index)),
            Err(c) => return c,
        };
        let apply = flags & MR_PARTITION_OPEN_APPLY_SHIFT != 0;
        match mr_api::open_found_volume(&parent, &result, index as usize, apply) {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
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
                Entry::Search(e) => (e.task.state(), e.task.progress()),
                Entry::NtfsTree(e) => (e.task.state(), e.task.progress()),
                Entry::NtfsCopy(e) => (e.task.state(), e.task.progress()),
                Entry::NtfsDeleted(e) => (e.task.state(), e.task.progress()),
                Entry::NtfsRecover(e) => (e.task.state(), e.task.progress()),
                Entry::VfsBuild(e) => (e.task.state(), e.task.progress()),
                Entry::Source(_)
                | Entry::Partitions { .. }
                | Entry::Ntfs(_)
                | Entry::NtfsListing(_)
                | Entry::VmdkExtents(_)
                | Entry::Vfs(_)
                | Entry::VfsQuery(_) => return None,
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
            Entry::Search(e) => {
                e.task.cancel();
                true
            }
            Entry::NtfsTree(e) => {
                e.task.cancel();
                true
            }
            Entry::NtfsCopy(e) => {
                e.task.cancel();
                true
            }
            Entry::NtfsDeleted(e) => {
                e.task.cancel();
                true
            }
            Entry::NtfsRecover(e) => {
                e.task.cancel();
                true
            }
            Entry::VfsBuild(e) => {
                e.task.cancel();
                true
            }
            Entry::Source(_)
            | Entry::Partitions { .. }
            | Entry::Ntfs(_)
            | Entry::NtfsListing(_)
            | Entry::VmdkExtents(_)
            | Entry::Vfs(_)
            | Entry::VfsQuery(_) => false,
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
            Some(
                Entry::Hash { .. }
                | Entry::Verify { .. }
                | Entry::Search(_)
                | Entry::NtfsTree(_)
                | Entry::NtfsCopy(_)
                | Entry::NtfsDeleted(_)
                | Entry::NtfsRecover(_)
                | Entry::VfsBuild(_),
            ) => {}
            _ => return bad_task(task),
        }
        // Si sigue en marcha se cancela; el hilo termina por su cuenta.
        match map.remove(&task) {
            Some(Entry::Hash { task, .. }) => task.cancel(),
            Some(Entry::Verify { task, .. }) => task.cancel(),
            Some(Entry::Search(e)) => e.task.cancel(),
            Some(Entry::NtfsTree(e)) => e.task.cancel(),
            Some(Entry::NtfsCopy(e)) => e.task.cancel(),
            Some(Entry::NtfsDeleted(e)) => e.task.cancel(),
            Some(Entry::NtfsRecover(e)) => e.task.cancel(),
            Some(Entry::VfsBuild(e)) => e.task.cancel(),
            _ => {}
        }
        MR_OK
    })
}

mod ntfs;
pub use ntfs::*;
mod vfs;
pub use vfs::*;

#[cfg(test)]
mod ntfs_tests;
mod vmdk_extents;
pub use vmdk_extents::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod vfs_tests;
