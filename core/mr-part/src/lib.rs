//! Tablas de particiones (M2): lectura de solo lectura y tolerante a daños.
//!
//! Contrato común de los parsers (`mbr`, `gpt` y los que se añadan):
//! - Leen del origen **solo** a través de [`sector::read_trusted`]: un sector `Unreadable` o
//!   `Skipped` nunca se interpreta como estructura (su contenido es un patrón de relleno); produce
//!   [`Anomaly::UnreadableSector`] y la estructura afectada queda sin confirmar.
//! - No devuelven un booleano: cada partición y cada tabla llevan su [`Confidence`] (ADR-003) y la
//!   lista de anomalías encontradas, para servir igual en discos sanos y dañados.
//! - Aritmética comprobada (`checked_*`) en todos los cálculos de LBA, límites explícitos de
//!   enlaces, entradas y memoria, y ningún pánico con datos del origen.
//! - Basados en especificaciones públicas (UEFI para GPT, disposición clásica de MBR/EBR) y en
//!   `docs/investigacion/dmde-fase2/INV-05-mbr-y-busqueda-de-particiones.md`; plan en
//!   `docs/PLAN_M2.md`.

pub mod gpt;
pub mod mbr;
pub mod sector;

pub use mr_common::Confidence;

/// Esquema de particionado detectado en el origen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// Sin tabla reconocible.
    None,
    /// MBR clásico (con o sin particiones extendidas).
    Mbr,
    /// GPT con MBR protector.
    Gpt,
    /// MBR híbrido: GPT más entradas MBR que describen particiones reales.
    Hybrid,
}

/// De dónde procede una partición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Entrada `slot` (0 a 3) del MBR.
    Primary { slot: u8 },
    /// Entrada `slot` del EBR situado en `ebr_lba` (cadena de particiones lógicas).
    Logical { ebr_lba: u64, slot: u8 },
    /// Entrada `index` de la tabla de entradas GPT.
    Gpt { index: u32 },
}

/// Tipo declarado de la partición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    /// Byte de tipo de una entrada MBR/EBR.
    Mbr(u8),
    /// GUID de tipo de una entrada GPT, en el orden de bytes del disco.
    Gpt([u8; 16]),
}

/// Anomalía encontrada al leer una tabla. Las específicas de cada formato van en su módulo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// El sector `lba` no se pudo leer (o se saltó): no se usa como estructura.
    UnreadableSector {
        lba: u64,
    },
    Mbr(mbr::MbrAnomaly),
    Gpt(gpt::GptAnomaly),
}

/// Una partición declarada por una tabla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub origin: Origin,
    /// Primer sector, en sectores lógicos del origen.
    pub start_lba: u64,
    pub sector_count: u64,
    pub kind: PartitionKind,
    /// Nombre (GPT); `None` en MBR.
    pub name: Option<String>,
    /// Indicador de arranque (MBR) o atributo equivalente.
    pub active: bool,
    pub confidence: Confidence,
    pub anomalies: Vec<Anomaly>,
}

/// Resultado de leer el particionado de un origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTable {
    pub scheme: Scheme,
    /// Tamaño de sector lógico con que se interpretaron las LBA.
    pub sector_size: u32,
    pub partitions: Vec<Partition>,
    /// Anomalías de la tabla en conjunto (no atribuibles a una partición).
    pub anomalies: Vec<Anomaly>,
}
