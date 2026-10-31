//! Motor de recuperación (#26, M4): copia ficheros y directorios del árbol virtual (`mr-vfs`) a un
//! destino seguro, con el estado de cada tramo de bytes, conflictos de nombre, rutas largas,
//! flujos alternativos, fechas y un diario por fichero. Diseño en
//! `docs/investigacion/dmde-fase2/INV-01-motor-de-recuperacion.md` §7.4 a §7.10, comportamiento en
//! `docs/formatos/recuperacion.md`, diario en `docs/formatos/diario-recuperacion.md` y valores por
//! defecto en ADR-029 (adenda de #26).
//!
//! Invariantes (comprobadas por las pruebas):
//! 1. El origen nunca se escribe y el destino nunca es el origen, uno de sus segmentos o ficheros
//!    externos, ni una ruta de dispositivo ([`dest::prepare_root`], [`dest::OutputSink`]).
//! 2. Nada no fiable se presenta como íntegro: cada flujo registra sus tramos no fiables con su
//!    LBA y su relleno; el hash de un flujo con tramos no fiables lleva el ámbito
//!    `escrito_con_relleno` y nunca se presenta como hash del contenido.
//! 3. Nunca se sustituye un fichero existente sin [`ConflictPolicy::Overwrite`] confirmada
//!    ([`RecoverOptions::overwrite_confirmed`]) o una respuesta explícita a la pregunta.
//! 4. Ningún nombre sale del destino ([`names`], [`dest::check_contained`]).
//! 5. Atomicidad por fichero: un nombre final solo aparece completo (temporal y publicación de
//!    `mr_api::create_output`); la cancelación descarta el fichero en curso.
//! 6. Memoria acotada: trozos de 1 MiB, listas de tramos acotadas con recuentos exactos, diario
//!    escrito en flujo; lineal en la profundidad y en los nombres de un mismo directorio.
//! 7. Determinismo: mismo árbol, selección y opciones dan los mismos nombres, contenidos y diario
//!    (salvo fechas de ejecución, identificador y rutas absolutas).

pub mod dest;
mod engine;
pub mod journal;
pub mod json;
pub mod mem;
pub mod names;
pub mod source;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use mr_vfs::filter::Filter;
use mr_vfs::{DataState, NodeId, VfsError};
use thiserror::Error;

pub use dest::{Capabilities, CreateError, OutputSink, PendingFile};
pub use engine::recover;
pub use journal::{JOURNAL_FOLDER, JOURNAL_VERSION};
pub use source::{ContentSource, NtfsContent, SourceError, StreamDesc, StreamReader};

/// Patrón de relleno por defecto de los tramos no fiables (`BAD!`, ADR-021).
pub const DEFAULT_FILL: [u8; 4] = mr_fs_ntfs::stream::DEFAULT_FILL;
/// Tamaño de cada parte al dividir automáticamente para FAT32 (4 GiB − 1 MiB).
pub const AUTO_PART_SIZE: u64 = (4 << 30) - (1 << 20);
/// Máximo de renombrados de un mismo nombre por defecto.
pub const DEFAULT_RENAME_LIMIT: u32 = 1000;

/// Política de conflictos de nombre (INV-01 §7.7). Los conflictos entre entradas de la propia
/// recuperación (mayúsculas, versión viva y borrada, nombres que el escape iguala) se renombran
/// siempre: ninguna entrada seleccionada sustituye ni omite a otra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Pregunta al observador ([`RecoverObserver::conflict`]).
    Ask,
    /// `nombre (2).ext`, `(3)`... (` (borrado)` en los borrados).
    Rename,
    /// No se escribe; un directorio existente no se usa y su contenido se omite.
    Skip,
    /// Fichero: se omite si el existente tiene el mismo tamaño y SHA-256, si no se renombra.
    /// Directorio: se reutiliza (fusión). Reanudar = repetir con esta política.
    SkipIfIdentical,
    /// Sustitución atómica de ficheros existentes (solo con confirmación explícita);
    /// directorios: fusión.
    Overwrite,
}

impl ConflictPolicy {
    pub fn code(self) -> &'static str {
        match self {
            ConflictPolicy::Ask => "preguntar",
            ConflictPolicy::Rename => "renombrar",
            ConflictPolicy::Skip => "omitir",
            ConflictPolicy::SkipIfIdentical => "omitir_si_identico",
            ConflictPolicy::Overwrite => "sobrescribir",
        }
    }

    pub fn from_code(s: &str) -> Option<Self> {
        [
            ConflictPolicy::Ask,
            ConflictPolicy::Rename,
            ConflictPolicy::Skip,
            ConflictPolicy::SkipIfIdentical,
            ConflictPolicy::Overwrite,
        ]
        .into_iter()
        .find(|p| p.code() == s)
    }
}

/// Respuesta a una pregunta de conflicto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    Rename,
    Skip,
    /// En directorios equivale a fusionar.
    SkipIfIdentical,
    /// Sustituir (la respuesta es la confirmación explícita); en directorios, fusionar.
    Overwrite,
    /// Cancela la recuperación.
    Cancel,
}

/// Pregunta de conflicto (política [`ConflictPolicy::Ask`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictQuestion {
    pub node: NodeId,
    pub directory: bool,
    pub deleted: bool,
    /// Ruta relativa pedida en el destino (`/` como separador).
    pub requested: String,
    /// Ruta del objeto que ya existe.
    pub existing: PathBuf,
    pub existing_is_dir: bool,
    pub existing_size: Option<u64>,
    /// Tamaño del fichero que se recupera (0 en directorios).
    pub new_size: u64,
}

/// Respuesta del observador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictAnswer {
    pub decision: ConflictDecision,
    /// Aplicar la misma decisión a los conflictos siguientes del mismo tipo (fichero o
    /// directorio).
    pub apply_to_all: bool,
}

/// División de ficheros grandes (INV-01 §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitPolicy {
    /// Solo si el destino no admite el tamaño (FAT32: más de 4 GiB − 1), en partes de
    /// [`AUTO_PART_SIZE`].
    Auto,
    /// Nunca: un fichero que no cabe es un error de destino.
    Never,
    /// Siempre que el flujo principal supere este tamaño, en partes de este tamaño.
    Size(u64),
}

/// Opciones de la recuperación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverOptions {
    /// Carpeta de destino (se crea si no existe).
    pub destination: PathBuf,
    /// Diario (por defecto `<destino>/MemRescue-registro/recuperacion-<fecha>-<id>.jsonl`).
    pub journal: Option<PathBuf>,
    pub conflict: ConflictPolicy,
    /// Confirmación explícita de [`ConflictPolicy::Overwrite`] (sin ella se rechaza).
    pub overwrite_confirmed: bool,
    /// Estados de los datos que se escriben (máscara de [`DataState::bit`]); los demás se omiten
    /// y se anotan. Por defecto [`default_write_states`].
    pub write_states: u32,
    /// Escribir los bytes actuales del disco en los tramos `possibly_overwritten` (por defecto se
    /// escribe el relleno). Siguen siendo no fiables.
    pub include_reallocated_bytes: bool,
    /// Patrón de relleno (regla de ADR-021: no cuatro bytes iguales).
    pub fill: [u8; 4],
    /// Escribir los flujos alternativos.
    pub alternate_streams: bool,
    /// Fijar las fechas de creación, modificación y acceso.
    pub times: bool,
    /// Aplicar el atributo de solo lectura (al final).
    pub attributes: bool,
    /// Fichero lateral `.mrbad` con los tramos no fiables de cada fichero incompleto.
    pub mark_incomplete: bool,
    pub split: SplitPolicy,
    /// Renombrados máximos de un mismo nombre; después se omite y se anota.
    pub rename_limit: u32,
    /// Crear también los directorios que no reciben ningún fichero.
    pub create_empty_dirs: bool,
    /// Capacidades del destino fijadas (pruebas: destino sin flujos o FAT32); `None` = sonda.
    pub capabilities: Option<Capabilities>,
}

/// Estados que se escriben por defecto (adenda de ADR-029): `integro`,
/// `integro_no_verificable` y `parcial` (con relleno y marcado). `sobrescrito`, `sin_datos`,
/// `no_disponible` y `cifrado` se omiten salvo opción explícita.
pub fn default_write_states() -> u32 {
    DataState::Integro.bit() | DataState::IntegroNoVerificable.bit() | DataState::Parcial.bit()
}

impl RecoverOptions {
    pub fn new(destination: impl Into<PathBuf>) -> Self {
        RecoverOptions {
            destination: destination.into(),
            journal: None,
            conflict: ConflictPolicy::Rename,
            overwrite_confirmed: false,
            write_states: default_write_states(),
            include_reallocated_bytes: false,
            fill: DEFAULT_FILL,
            alternate_streams: true,
            times: true,
            attributes: true,
            mark_incomplete: true,
            split: SplitPolicy::Auto,
            rename_limit: DEFAULT_RENAME_LIMIT,
            create_empty_dirs: false,
            capabilities: None,
        }
    }
}

/// Qué se recupera: nodos seleccionados (con sus descendientes) y el filtro que se aplica dentro
/// de ellos. Un nodo seleccionado que cuelga de otro seleccionado se ignora (ya está incluido).
/// La raíz seleccionada vuelca su contenido directamente en el destino; cualquier otro directorio
/// seleccionado se crea en el destino con su nombre.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub nodes: Vec<NodeId>,
    /// Filtro (categorías, estados, tamaño, fechas...). Las reglas de nombre van en `masks`.
    pub filter: Filter,
    /// Reglas de nombre `regla;regla;!exclusión` (ver `mr_vfs::filter`).
    pub masks: String,
}

/// Datos del origen para el diario.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInfo {
    pub path: String,
    pub description: String,
    pub size: u64,
    pub sector_size: u32,
    /// LBA del origen donde empieza el volumen (partición o volumen encontrado), en sectores del
    /// origen; `None` si el volumen empieza en el sector 0.
    pub volume_start_lba: Option<u64>,
}

/// Progreso.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    /// Bytes escritos o leídos de los flujos principales (el total sale de los metadatos de los
    /// ficheros que se van a escribir, sin leer datos).
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub untrusted_bytes: u64,
    pub errors: u64,
    /// Ruta relativa en curso.
    pub current: String,
}

/// Observador: cancelación, progreso y preguntas de conflicto.
pub trait RecoverObserver: Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _p: &Progress) {}
    /// Solo con [`ConflictPolicy::Ask`]. Por defecto, renombrar.
    fn conflict(&self, _q: &ConflictQuestion) -> ConflictAnswer {
        ConflictAnswer {
            decision: ConflictDecision::Rename,
            apply_to_all: false,
        }
    }
}

/// Observador que no cancela, no informa y renombra.
pub struct NoObserver;
impl RecoverObserver for NoObserver {}

/// Estado final.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Complete,
    Cancelled,
    /// Se detuvo por un error que impide seguir (destino lleno, diario que no se puede escribir).
    Failed,
}

impl RunStatus {
    pub fn code(self) -> &'static str {
        match self {
            RunStatus::Complete => "completa",
            RunStatus::Cancelled => "cancelada",
            RunStatus::Failed => "fallida",
        }
    }
}

/// Totales de una recuperación.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub files_planned: u64,
    pub dirs_planned: u64,
    pub files_written: u64,
    pub dirs_created: u64,
    pub dirs_merged: u64,
    pub bytes_written: u64,
    pub untrusted_bytes: u64,
    pub filled_bytes: u64,
    /// Ficheros escritos por estado final (índice = valor de [`DataState`]).
    pub written_by_state: [u64; 8],
    pub skipped_state: u64,
    pub skipped_conflict: u64,
    pub skipped_identical: u64,
    pub unsupported: u64,
    pub errors: u64,
    pub renamed: u64,
    pub long_paths: u64,
    pub marked_incomplete: u64,
    pub cancelled_files: u64,
}

impl Totals {
    pub fn written(&self, d: DataState) -> u64 {
        self.written_by_state[d as usize]
    }

    /// Ficheros escritos que no son `integro`.
    pub fn written_not_intact(&self) -> u64 {
        self.files_written - self.written(DataState::Integro)
    }
}

/// Resultado de una recuperación (el detalle por fichero está en el diario).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverSummary {
    pub status: RunStatus,
    /// Motivo de una ejecución fallida.
    pub reason: Option<String>,
    pub journal: PathBuf,
    /// Raíz de destino (forma para mostrar).
    pub destination: String,
    pub capabilities: Capabilities,
    pub totals: Totals,
}

impl RecoverSummary {
    /// Todo lo seleccionado se escribió y es `integro` (o ya estaba, idéntico): ni errores, ni
    /// omitidos por estado o por conflicto, ni ficheros no íntegros.
    pub fn all_intact(&self) -> bool {
        let t = &self.totals;
        self.status == RunStatus::Complete
            && t.errors == 0
            && t.skipped_state == 0
            && t.unsupported == 0
            && t.written_not_intact() == 0
    }

    /// Código de salida de la CLI: 0 si todo es íntegro, 3 si algún fichero no lo es (escrito
    /// con tramos no fiables o sin procedencia verificable, u omitido por su estado), 1 si hubo
    /// errores, se canceló o falló.
    pub fn exit_code(&self) -> i32 {
        if self.status != RunStatus::Complete || self.totals.errors > 0 {
            1
        } else if self.all_intact() {
            0
        } else {
            3
        }
    }
}

/// Error que impide empezar (o seguir con) la recuperación.
#[derive(Debug, Error)]
pub enum RecoverError {
    #[error("opciones de recuperación no válidas: {0}")]
    InvalidOptions(String),
    #[error("destino no válido: {0}")]
    Destination(String),
    #[error("diario de la recuperación: {0}")]
    Journal(String),
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error("operación cancelada")]
    Cancelled,
}

impl RecoverError {
    pub fn code(&self) -> &'static str {
        match self {
            RecoverError::InvalidOptions(_) => "recover_invalid_options",
            RecoverError::Destination(_) => "recover_destination_rejected",
            RecoverError::Journal(_) => "recover_journal",
            RecoverError::Vfs(e) => e.code(),
            RecoverError::Cancelled => "cancelled",
        }
    }
}
