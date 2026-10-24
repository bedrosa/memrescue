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
pub mod search;
pub mod sector;
pub mod table;

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
    /// Lectura GPT completa (copias, copia elegida, desplazamiento, GUID únicos y atributos) cuando
    /// el esquema es GPT o híbrido; `None` en los demás.
    pub gpt: Option<gpt::GptRead>,
}

// ---------------------------------------------------------------------------------------------
// Añadido en #12 (MBR/EBR) e integración con GPT (#13): gravedad, códigos y textos de las
// anomalías, nombres de tipo y detección del esquema. Detalle en `table`.
// ---------------------------------------------------------------------------------------------

pub use table::{gpt_type_name, open_coordinates, read_partition_table, OpenCoordinates};

/// Gravedad de una anomalía: el nivel de [`Confidence`] que deja a lo que afecta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informativa (disco sin tabla, MBR híbrido...): no resta confianza.
    Notice,
    /// Daño compensado por una redundancia (p. ej. una copia GPT dañada cuando la otra es
    /// válida): la estructura es `ValidWithFixes`.
    Compensated,
    /// La estructura se puede usar con cautela.
    Suspicious,
    /// La estructura no es coherente.
    Invalid,
}

impl Anomaly {
    /// Sector de la imagen donde se detectó (`None` si no tiene posición en la imagen).
    pub fn lba(&self) -> Option<u64> {
        match self {
            Anomaly::UnreadableSector { lba } => Some(*lba),
            Anomaly::Mbr(m) => Some(m.lba()),
            Anomaly::Gpt(g) => table::gpt_lba(g),
        }
    }

    /// Código estable (JSON de la CLI). Coincide con el del manifiesto del corpus cuando existe.
    pub fn code(&self) -> &'static str {
        match self {
            Anomaly::UnreadableSector { .. } => "unreadable_sector",
            Anomaly::Mbr(m) => m.code(),
            Anomaly::Gpt(g) => table::gpt_code(g),
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Anomaly::UnreadableSector { .. } => Severity::Suspicious,
            Anomaly::Mbr(m) => m.severity(),
            Anomaly::Gpt(g) => table::gpt_severity(g),
        }
    }

    /// LBA de destino (enlaces de EBR, `MyLBA` esperada), si la hay.
    pub fn target(&self) -> Option<u64> {
        match self {
            Anomaly::Mbr(m) => m.target(),
            Anomaly::Gpt(g) => table::gpt_target(g),
            _ => None,
        }
    }

    /// Valor asociado (tipo, tamaño declarado, LBA del disco, índice de entrada...), si lo hay. En
    /// `image_shifted` es el desplazamiento con signo en complemento a dos ([`Anomaly::value_signed`]).
    pub fn value(&self) -> Option<u64> {
        match self {
            Anomaly::Mbr(m) => m.value(),
            Anomaly::Gpt(g) => table::gpt_value(g),
            _ => None,
        }
    }

    /// `true` si [`Anomaly::value`] debe leerse como entero con signo (`i64`).
    pub fn value_signed(&self) -> bool {
        matches!(self, Anomaly::Gpt(gpt::GptAnomaly::ImageShifted { .. }))
    }
}

impl std::fmt::Display for Anomaly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Anomaly::UnreadableSector { lba } => write!(
                f,
                "el sector {lba} no se pudo leer: no se interpreta como estructura"
            ),
            Anomaly::Mbr(m) => m.fmt(f),
            Anomaly::Gpt(g) => g.fmt(f),
        }
    }
}

impl PartitionKind {
    /// Nombre legible del tipo, si se conoce.
    pub fn type_name(&self) -> Option<&'static str> {
        match self {
            PartitionKind::Mbr(t) => mbr::type_name(*t),
            PartitionKind::Gpt(g) => gpt_type_name(g),
        }
    }
}

impl PartitionTable {
    /// Todas las anomalías: las de la tabla y las de cada partición, en ese orden.
    pub fn all_anomalies(&self) -> impl Iterator<Item = &Anomaly> {
        self.anomalies
            .iter()
            .chain(self.partitions.iter().flat_map(|p| p.anomalies.iter()))
    }

    /// Confianza del conjunto: la peor entre la gravedad de todas las anomalías y la confianza que
    /// el lector GPT da a la copia elegida (inválida si ninguna es utilizable).
    pub fn confidence(&self) -> Confidence {
        let own = mbr::confidence_of(self.all_anomalies());
        match &self.gpt {
            Some(g) if g.confidence.rank() < own.rank() => g.confidence.clone(),
            _ => own,
        }
    }

    /// Datos GPT propios de una partición (GUID único y atributos).
    pub fn gpt_entry(&self, p: &Partition) -> Option<&gpt::GptPartition> {
        let Origin::Gpt { index } = p.origin else {
            return None;
        };
        self.gpt
            .as_ref()?
            .partitions
            .iter()
            .find(|g| g.partition.origin == Origin::Gpt { index })
    }

    /// Desplazamiento de la imagen detectado por la GPT (`LBA disco = LBA imagen + shift`); 0 si no
    /// hay GPT o la imagen no está desplazada.
    pub fn shift(&self) -> i64 {
        self.gpt.as_ref().map_or(0, |g| g.shift)
    }
}

// ---------------------------------------------------------------------------------------------
// Añadido en #18: copia del particionado en formato propio (detalle en `backup`).
// ---------------------------------------------------------------------------------------------

pub mod backup;
