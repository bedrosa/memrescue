//! Sistema de archivos virtual (#25, M4): árbol unificado de ficheros vivos, borrados y huérfanos
//! de un volumen, con el estado de cada entrada y la fiabilidad de sus datos. Base de la
//! recuperación (#26) y de sus informes (#27). Diseño en
//! `docs/investigacion/dmde-fase2/INV-01-motor-de-recuperacion.md` §7.2 y §7.3, formato y límites en
//! `docs/formatos/vfs.md`, decisiones por defecto en ADR-029.
//!
//! - [`arena`]: arena compacta de nodos (identificadores `u32` estables, sin punteros, nombres
//!   UTF-16 originales en un búfer compartido), límites con error tipado y orden determinista de
//!   los hijos.
//! - [`name`]: forma visible de los nombres (UTF-8 con sustitución), escape para mostrar rutas sin
//!   ambigüedad y comparación sin distinguir mayúsculas.
//! - [`filter`]: filtros puros (categoría, estado de los datos, máscaras de nombre con `;` y
//!   exclusión `!`, tamaño y fecha) y consultas sobre el árbol.
//! - [`ntfs`]: construcción desde un volumen NTFS (árbol vivo de `mr-fs-ntfs`, borrados de la MFT,
//!   huérfanos y ficheros de metadatos), cancelable y con progreso.
//!
//! El núcleo (arena, nombres y filtros) no conoce NTFS: cada nodo guarda una referencia genérica
//! al objeto del sistema de archivos ([`FsRef`]: registro y secuencia) con la que la recuperación
//! vuelve a abrirlo. Solo [`ntfs`] traduce las estructuras de `mr-fs-ntfs`.

pub mod arena;
pub mod filter;
pub mod name;
pub mod ntfs;

#[cfg(test)]
mod tests;

pub use arena::{Limits, NewNode, NodeInfo, Note, Stats, Vfs, VfsBuilder};
pub use filter::{query, Filter, NameRules, TimeField};
pub use ntfs::{build_ntfs, BuildObserver, NoBuildObserver, NtfsBuildOptions};

use thiserror::Error;

/// Identificador estable de un nodo: su posición en la arena. La raíz es el 0 y el padre de un
/// nodo siempre tiene un identificador menor que el suyo (el árbol no puede tener ciclos).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const ROOT: NodeId = NodeId(0);

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Tipo de nodo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum NodeKind {
    File = 1,
    Directory = 2,
    /// Carpeta propia de MemRescue (`[Metadatos]`, `[Huérfanos]`...): no existe en el origen.
    Synthetic = 3,
}

impl NodeKind {
    pub fn code(self) -> &'static str {
        match self {
            NodeKind::File => "fichero",
            NodeKind::Directory => "directorio",
            NodeKind::Synthetic => "sintetico",
        }
    }

    pub fn is_container(self) -> bool {
        self != NodeKind::File
    }

    fn from_u8(v: u8) -> NodeKind {
        match v {
            1 => NodeKind::File,
            2 => NodeKind::Directory,
            _ => NodeKind::Synthetic,
        }
    }
}

/// Categoría de una entrada (INV-01 §7.2). `Found` (M6) y `Raw` (M7) están reservadas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Category {
    /// Alcanzable desde la raíz por índices vivos.
    Live = 1,
    /// Registro no en uso colocado bajo su padre (vivo o borrado con la misma secuencia).
    Deleted = 2,
    /// Sin padre válido: registro en uso no alcanzable desde la raíz, o borrado cuyo padre no
    /// existe, es ilegible o se reutilizó (otra secuencia).
    Orphan = 3,
    /// Ficheros del sistema (`$MFT`, `$Bitmap`, `$Extend`...).
    Metadata = 4,
    /// Reservada: ficheros de volúmenes encontrados por el escaneo completo (M6).
    Found = 5,
    /// Reservada: ficheros por firma (M7).
    Raw = 6,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Live,
        Category::Deleted,
        Category::Orphan,
        Category::Metadata,
        Category::Found,
        Category::Raw,
    ];

    /// Código estable (también en la CLI, el JSON y la ABI).
    pub fn code(self) -> &'static str {
        match self {
            Category::Live => "vivo",
            Category::Deleted => "borrado",
            Category::Orphan => "huerfano",
            Category::Metadata => "metadatos",
            Category::Found => "encontrado",
            Category::Raw => "raw",
        }
    }

    pub fn from_code(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.code() == s)
    }

    pub fn bit(self) -> u32 {
        1 << (self as u8)
    }

    fn from_u8(v: u8) -> Category {
        Category::ALL
            .into_iter()
            .find(|c| *c as u8 == v)
            .unwrap_or(Category::Live)
    }
}

/// De dónde sale el nombre de una entrada (INV-01 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum NameReliability {
    /// Índice vivo o `$FILE_NAME` de un registro en uso.
    Structure = 1,
    /// Entrada de índice residual (no confirmada por un registro en uso).
    Residual = 2,
    /// Solo el `$FILE_NAME` de un registro no en uso.
    Record = 3,
    /// Nombre propio de MemRescue (carpetas sintéticas).
    Synthetic = 4,
}

impl NameReliability {
    pub fn code(self) -> &'static str {
        match self {
            NameReliability::Structure => "estructura",
            NameReliability::Residual => "residual",
            NameReliability::Record => "registro",
            NameReliability::Synthetic => "sintetico",
        }
    }

    fn from_u8(v: u8) -> NameReliability {
        match v {
            1 => NameReliability::Structure,
            2 => NameReliability::Residual,
            3 => NameReliability::Record,
            _ => NameReliability::Synthetic,
        }
    }
}

/// Resumen de la fiabilidad de los datos de un fichero (INV-01 §7.3). Solo `Integro` se
/// presenta como íntegro. Es el estado conocido por los metadatos, sin leer los datos: una lectura
/// posterior (recuperación, #26) puede encontrar sectores ilegibles y rebajarlo a `Parcial`, nunca
/// subirlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum DataState {
    /// Vivo y sin tramos no fiables conocidos.
    Integro = 1,
    /// Borrado con todos sus clústeres libres: sin errores conocidos, pero nada prueba que no se
    /// reutilizaran y liberaran después.
    IntegroNoVerificable = 2,
    /// Algún tramo no fiable (sin mapa, posiblemente sobrescrito...).
    Parcial = 3,
    /// Todos los clústeres con contenido están asignados a otro fichero o en duda.
    Sobrescrito = 4,
    /// No hay de dónde leer los datos (sin runs, sin `$DATA`, tamaño perdido).
    SinDatos = 5,
    /// Registro dañado o ilegible, o flujo con un error tipado.
    NoDisponible = 6,
    /// Flujo cifrado (EFS): el contenido en disco no es el del fichero.
    Cifrado = 7,
}

impl DataState {
    pub const ALL: [DataState; 7] = [
        DataState::Integro,
        DataState::IntegroNoVerificable,
        DataState::Parcial,
        DataState::Sobrescrito,
        DataState::SinDatos,
        DataState::NoDisponible,
        DataState::Cifrado,
    ];

    /// Código estable (también en la CLI, el JSON y la ABI).
    pub fn code(self) -> &'static str {
        match self {
            DataState::Integro => "integro",
            DataState::IntegroNoVerificable => "integro_no_verificable",
            DataState::Parcial => "parcial",
            DataState::Sobrescrito => "sobrescrito",
            DataState::SinDatos => "sin_datos",
            DataState::NoDisponible => "no_disponible",
            DataState::Cifrado => "cifrado",
        }
    }

    pub fn from_code(s: &str) -> Option<DataState> {
        DataState::ALL.into_iter().find(|d| d.code() == s)
    }

    /// Texto para mostrar.
    pub fn label(self) -> &'static str {
        match self {
            DataState::Integro => "íntegro",
            DataState::IntegroNoVerificable => "sin errores, procedencia no verificable",
            DataState::Parcial => "PARCIAL",
            DataState::Sobrescrito => "SOBRESCRITO",
            DataState::SinDatos => "SIN DATOS",
            DataState::NoDisponible => "NO DISPONIBLE",
            DataState::Cifrado => "CIFRADO",
        }
    }

    /// Solo `Integro` es íntegro.
    pub fn is_intact(self) -> bool {
        self == DataState::Integro
    }

    pub fn bit(self) -> u32 {
        1 << (self as u8)
    }

    fn from_u8(v: u8) -> Option<DataState> {
        DataState::ALL.into_iter().find(|d| *d as u8 == v)
    }
}

/// Indicadores de un nodo ([`NodeInfo::flags`]).
pub mod flags {
    /// Entrada borrada (registro no en uso o entrada de índice residual).
    pub const DELETED: u32 = 1;
    /// Fichero del sistema.
    pub const SYSTEM: u32 = 2;
    /// El nombre tiene unidades UTF-16 sueltas (la forma visible lleva U+FFFD).
    pub const NAME_LOSSY: u32 = 4;
    /// Fechas y atributos de la clave del índice (el registro no se pudo usar).
    pub const TIMES_FROM_INDEX: u32 = 8;
    /// Origen: entrada de un índice de directorio (vivo o residual).
    pub const FROM_INDEX: u32 = 16;
    /// Origen: registro de la MFT encontrado al recorrerla.
    pub const FROM_MFT: u32 = 32;
    /// Registro dañado, ilegible o referencia no válida (anomalía).
    pub const RECORD_DAMAGED: u32 = 64;
    /// El padre que cita el nombre se reutilizó (otra secuencia): huérfano.
    pub const PARENT_REUSED: u32 = 128;
    /// Las referencias al padre formaban un ciclo: se cortó aquí.
    pub const CYCLE_CUT: u32 = 256;
    /// Otro hermano tiene el mismo nombre sin distinguir mayúsculas.
    pub const DUPLICATE_NAME: u32 = 512;
    /// El fichero tiene anomalías de estructura (atributos, extensiones...).
    pub const ANOMALIES: u32 = 1024;
    /// La evaluación de los datos no es completa (tramos o propietarios truncados).
    pub const ASSESSMENT_TRUNCATED: u32 = 2048;
    /// Directorio no listado del todo (índice ilegible, ciclo o profundidad máxima).
    pub const DIR_INCOMPLETE: u32 = 4096;
    /// El registro tiene varios nombres (enlaces duros).
    pub const HARD_LINK: u32 = 8192;
    /// Todos los indicadores conocidos.
    pub const ALL: u32 = 0x3FFF;

    /// Códigos estables de los indicadores, en orden de bit.
    pub const NAMES: [(u32, &str); 14] = [
        (DELETED, "deleted"),
        (SYSTEM, "system"),
        (NAME_LOSSY, "name_lossy"),
        (TIMES_FROM_INDEX, "times_from_index"),
        (FROM_INDEX, "from_index"),
        (FROM_MFT, "from_mft"),
        (RECORD_DAMAGED, "record_damaged"),
        (PARENT_REUSED, "parent_reused"),
        (CYCLE_CUT, "cycle_cut"),
        (DUPLICATE_NAME, "duplicate_name"),
        (ANOMALIES, "anomalies"),
        (ASSESSMENT_TRUNCATED, "assessment_truncated"),
        (DIR_INCOMPLETE, "dir_incomplete"),
        (HARD_LINK, "hard_link"),
    ];

    /// Códigos de los indicadores activos.
    pub fn names(f: u32) -> Vec<&'static str> {
        NAMES
            .iter()
            .filter(|(b, _)| f & b != 0)
            .map(|(_, n)| *n)
            .collect()
    }
}

/// Fechas (FILETIME, 100 ns desde 1601, UTC; 0 si se desconocen).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Times {
    pub created: u64,
    pub modified: u64,
    /// Cambio del registro (MFT).
    pub changed: u64,
    pub accessed: u64,
}

/// Referencia al objeto del sistema de archivos con la que se vuelve a abrir (NTFS: registro
/// base de la MFT y su secuencia) y la del padre que cita su nombre.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FsRef {
    pub record: u64,
    pub sequence: u16,
    pub parent_record: u64,
    pub parent_sequence: u16,
}

/// Error del sistema de archivos virtual.
#[derive(Debug, Error)]
pub enum VfsError {
    /// Se superó un límite de la arena (memoria acotada).
    #[error("se superó el límite de {what} del árbol virtual ({limit})")]
    LimitExceeded { what: &'static str, limit: u64 },
    /// Filtro mal formado.
    #[error("filtro no válido: {0}")]
    InvalidFilter(String),
    /// Nodo inexistente.
    #[error("el nodo {0} no existe")]
    NoSuchNode(u32),
    /// El nodo es un fichero: no puede tener hijos.
    #[error("el nodo {0} es un fichero: no puede tener hijos")]
    NotAContainer(u32),
    /// Error del lector del sistema de archivos.
    #[error(transparent)]
    Ntfs(#[from] mr_fs_ntfs::NtfsError),
}

impl VfsError {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            VfsError::LimitExceeded { .. } => "vfs_limit_exceeded",
            VfsError::InvalidFilter(_) => "vfs_invalid_filter",
            VfsError::NoSuchNode(_) => "vfs_no_such_node",
            VfsError::NotAContainer(_) => "vfs_not_a_container",
            VfsError::Ntfs(e) => e.code(),
        }
    }
}

pub type Result<T> = std::result::Result<T, VfsError>;
