//! Lector NTFS (M3, issues #20, #21 y #22): sector de arranque, localización de la MFT, registros FILE,
//! atributos, runs y lectura del contenido de los flujos (residentes, no residentes, dispersos,
//! comprimidos con LZNT1 y flujos alternativos).
//!
//! Solo lectura y tolerante a daños, a partir de la documentación pública del formato en disco de
//! NTFS (estructuras del sector de arranque, de los registros FILE con su secuencia de
//! actualización, de las cabeceras de atributo y de los *mapping pairs*). Detalle, límites y
//! tolerancias en `docs/formatos/ntfs.md`.
//!
//! - [`boot`]: sector de arranque con validación estricta y límites.
//! - [`record`]: registro FILE: secuencia de actualización (fixups) comprobada por tramo y cabecera.
//! - [`attr`]: iterador acotado de cabeceras de atributo.
//! - [`values`]: contenido tipado de `$STANDARD_INFORMATION`, `$FILE_NAME`, `$OBJECT_ID`,
//!   `$VOLUME_INFORMATION` y `$ATTRIBUTE_LIST`.
//! - [`runs`]: decodificación de *mapping pairs* con aritmética comprobada.
//! - [`file`]: fichero = registro base + extensiones (`$ATTRIBUTE_LIST`) y disposición de cada flujo.
//! - [`stream`]: lectura de un flujo con el estado de cada tramo (datos, disperso, sin inicializar,
//!   ilegible...).
//! - [`lznt1`]: descompresión LZNT1 ([MS-XCA]) con límites estrictos.
//! - [`index`] (#22): índices `$I30` (raíz e `INDX`) con límites y nodos dañados tipados.
//! - [`dir`] (#22): listado de directorios contrastado con la MFT, árbol, rutas y huérfanos.
//! - [`upcase`] (#22): `$UpCase` y colación de nombres.
//! - [`mft`]: disposición de la MFT, lectura de registros por número y enumeración.
//! - [`volume`]: [`NtfsVolume::open`]: arranque principal o copia, `$MFT`, `$MFTMirr`, etiqueta;
//!   [`NtfsVolume::file`] y [`NtfsVolume::open_stream`].
//! - [`bitmap`], [`deleted`], [`residual`] y [`recover`] (#23): mapa de asignación de clústeres
//!   (`$Bitmap`), ficheros borrados de la MFT y entradas residuales de los índices con su
//!   recuperabilidad, y recuperación con estado por tramo.
//!
//! Contrato común con `mr-part`: toda lectura pasa por `mr_part::sector::read_trusted` (un sector
//! ilegible nunca se interpreta), aritmética comprobada, límites explícitos, ningún pánico con datos
//! del origen y confianza (ADR-003, `mr_common::Confidence`) en lugar de aceptar o rechazar.

pub mod attr;
pub mod bitmap;
pub mod boot;
pub mod deleted;
pub mod dir;
pub mod file;
pub mod index;
pub mod lznt1;
pub mod mft;
mod read;
pub mod record;
pub mod recover;
pub mod residual;
pub mod runs;
pub mod stream;
pub mod upcase;
pub mod values;
pub mod volume;

#[cfg(test)]
mod dir_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod testvol;

pub use bitmap::{ClusterAlloc, ClusterBitmap};
pub use boot::{parse_boot, BootError, BootSector};
pub use deleted::{
    scan_deleted, DeletedEntry, DeletedScan, DeletedStatus, EntryOrigin, IndexRef, ParentState,
    Recoverability, ScanObserver, ScanOptions, StreamAssessment,
};
pub use dir::{
    DirEntry, DirListing, EntryState, Orphan, OrphanReason, Tree, TreeAnomaly, TreeIssue, TreeNode,
    TreeObserver, TreeOptions, TreeStatus,
};
pub use file::{FileAnomaly, FileAttribute, NtfsFile, StreamError, StreamIssue, StreamLayout};
pub use index::{IndexAnomaly, IndexIssue, NodeId};
pub use mft::{MftLayout, RecordOutcome, RecordState, Records};
pub use mr_common::Confidence;
pub use mr_part::Severity;
pub use record::{FileRecord, FileReference, RecordDamage, RecordHeader};
pub use recover::{recover_stream, RecoveryReport, SuspectRange};
pub use stream::{CopyReport, DataStream, RangeState, StreamChunk, StreamRange};
pub use volume::{
    BackupBoot, BackupState, BootProbe, BootSource, MirrorCheck, MirrorMismatch, NtfsAnomaly,
    NtfsVolume, RecordSource, VolumeInfo,
};

use thiserror::Error;

/// Error al abrir un volumen o leer un registro. Los daños de estructuras concretas no son errores:
/// se informan como anomalías o como estado del registro.
#[derive(Debug, Error)]
pub enum NtfsError {
    /// Fallo de E/S no recuperable (no un sector ilegible: esos se informan en el resultado).
    #[error(transparent)]
    Io(#[from] mr_io::IoError),
    /// Ni el arranque principal ni su copia son un arranque NTFS válido.
    #[error("no es un volumen NTFS: arranque principal: {primary}; copia: {backup}")]
    NotNtfs {
        primary: BootProbe,
        backup: BootProbe,
    },
    /// No se puede localizar la MFT (registro 0 inutilizable en `$MFT` y en `$MFTMirr`).
    #[error("no se puede localizar la MFT: en $MFT: {mft}; en $MFTMirr: {mirror}")]
    MftUnusable { mft: String, mirror: String },
    /// Número de registro fuera de la MFT.
    #[error("el registro {number} no existe (la MFT tiene {count})")]
    RecordOutOfRange { number: u64, count: u64 },
    /// El registro no es un registro FILE interpretable (dañado, ilegible, vacío...).
    #[error("el registro {number} no es utilizable ({}){}", state.label(), detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    RecordUnusable {
        number: u64,
        state: RecordState,
        detail: Option<String>,
    },
    /// El registro es una extensión de otro: los ficheros se abren por su registro base.
    #[error("el registro {number} es una extensión del registro {}", base.record)]
    NotBaseRecord { number: u64, base: FileReference },
    /// El flujo no se puede leer.
    #[error("registro {number}: {error}")]
    Stream { number: u64, error: StreamError },
    /// Lectura de un flujo mayor que el máximo por petición.
    #[error("lectura de {len} bytes (máximo {max} por petición)")]
    ReadTooLarge { len: usize, max: usize },
    /// El registro no tiene índice de nombres (`$I30`): no es un directorio.
    #[error("el registro {number} no es un directorio (no tiene índice $I30)")]
    NotADirectory { number: u64 },
    /// La raíz del índice `$I30` no es interpretable: el directorio no se puede listar.
    #[error("registro {number}: {issue}")]
    IndexRoot { number: u64, issue: IndexIssue },
    /// Ruta no encontrada.
    #[error("no existe {path}: no se encuentra «{component}»")]
    NotFound { path: String, component: String },
    /// Ruta mal formada.
    #[error("ruta no válida «{path}»: {reason}")]
    InvalidPath { path: String, reason: String },
    /// El fichero (o un antecesor) no cuelga de un directorio válido.
    #[error("el registro {record} está huérfano: {reason} (padre: registro {}, secuencia {})", parent.record, parent.sequence)]
    Orphan {
        record: u64,
        parent: FileReference,
        reason: OrphanReason,
    },
}

impl NtfsError {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            NtfsError::Io(_) => "io",
            NtfsError::NotNtfs { .. } => "not_ntfs",
            NtfsError::MftUnusable { .. } => "mft_unusable",
            NtfsError::RecordOutOfRange { .. } => "record_out_of_range",
            NtfsError::RecordUnusable { .. } => "record_unusable",
            NtfsError::NotBaseRecord { .. } => "not_base_record",
            NtfsError::Stream { error, .. } => error.code(),
            NtfsError::ReadTooLarge { .. } => "read_too_large",
            NtfsError::NotADirectory { .. } => "not_a_directory",
            NtfsError::IndexRoot { issue, .. } => issue.code(),
            NtfsError::NotFound { .. } => "not_found",
            NtfsError::InvalidPath { .. } => "invalid_path",
            NtfsError::Orphan { reason, .. } => reason.code(),
        }
    }
}
