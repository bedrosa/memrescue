//! Búsqueda exprés de particiones perdidas (issue #15, PT-05 y PT-06).
//!
//! Diseño propio de MemRescue descrito en
//! `docs/investigacion/dmde-fase2/INV-05b-generacion-de-candidatos.md` §8 y en
//! `docs/formatos/busqueda-particiones.md` (ADR-023). Resumen:
//!
//! - **Candidatos por niveles** ([`Tier`]): P0 declarados por la tabla (y el byte 0), P1 derivados
//!   de lo ya confirmado (encadenado tras cada volumen, inicio deducido de una copia, entradas de
//!   tablas encontradas y posiciones corregidas por un desplazamiento aceptado), P2 rejilla inicial
//!   (63, cilindros CHS y múltiplos de 1 MiB), P3 huecos sin explicar y P4 rejilla final (últimos
//!   sectores y finales de cada MiB). Cola ordenada por `(nivel, posición, orden de inserción)` y
//!   deduplicada por posición.
//! - **Presupuesto acotado** ([`SearchOptions`]): lecturas y bytes máximos que no crecen con el
//!   tamaño del disco; reservas por nivel; estado [`SearchStatus::BudgetExhausted`] con los
//!   candidatos pendientes por nivel. Nunca se recorre el disco entero.
//! - **Lectura tolerante**: un sector ilegible o saltado nunca es evidencia ni llega a un
//!   reconocedor; el indicador correspondiente queda [`Check::Unverifiable`] («?»).
//! - **Evidencia y confianza** (ADR-003): principal, copia coherente, estructuras básicas, entrada
//!   de tabla, autoubicación y alineación; la confianza sale de reglas explicables
//!   ([`FoundVolume::confidence`]).
//! - **Desplazamiento de la imagen**: hipótesis aceptada solo con dos evidencias independientes que
//!   coinciden; se informa ([`SearchAnomaly::ImageShifted`]) y nunca se aplica en silencio.
//! - **Determinista**, cancelable antes de cada lectura y con progreso respecto al presupuesto.

mod fs;
mod reader;
mod tables;
#[cfg(test)]
mod tests;

pub use reader::ReadStats;
pub use tables::{FoundEntry, FoundTable, FoundTableKind};

use crate::{Confidence, Origin, PartitionTable, Severity};
use fs::{Boot, RECOGNIZERS};
use mr_io::BlockDevice;
use reader::{Bytes, Reader, Stop};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// 1 MiB en bytes.
pub const MIB: u64 = 1 << 20;
/// Bytes de un cilindro CHS con 255 cabezas y 63 sectores de 512 B.
const CYLINDER: u64 = 16065 * 512;
/// Bytes de una pista CHS (63 sectores de 512 B).
const TRACK: u64 = 63 * 512;

/// Familias de reconocedores ([`SearchOptions::families`]).
pub mod family {
    pub const NTFS: u32 = 1;
    /// FAT12, FAT16 y FAT32.
    pub const FAT: u32 = 2;
    pub const EXFAT: u32 = 4;
    /// Tablas MBR/EBR encontradas en posiciones candidatas (modo estricto).
    pub const MBR: u32 = 8;
    /// Cabeceras GPT encontradas en posiciones candidatas.
    pub const GPT: u32 = 16;
    pub const ALL: u32 = NTFS | FAT | EXFAT | MBR | GPT;
}

pub const DEFAULT_MAX_READS: u64 = 4096;
pub const DEFAULT_MAX_BYTES: u64 = 32 * MIB;
pub const DEFAULT_MAX_VOLUMES: u32 = 256;
pub const DEFAULT_PROBE_WINDOW: u32 = 8192;
pub const DEFAULT_START_GRID: u64 = 64 * MIB;
pub const DEFAULT_END_GRID: u64 = 16 * MIB;
pub const DEFAULT_CYLINDERS: u32 = 64;
pub const DEFAULT_PROBES_PER_GAP: u32 = 8;
/// Límites superiores de las opciones (protegen memoria y tiempo ante valores absurdos).
pub const LIMIT_MAX_READS: u64 = 1 << 20;
pub const LIMIT_MAX_BYTES: u64 = 4096 * MIB;
pub const LIMIT_MAX_VOLUMES: u32 = 4096;
pub const LIMIT_PROBE_WINDOW: u32 = 64 * 1024;
pub const LIMIT_GRID: u64 = 1 << 40;
pub const LIMIT_CYLINDERS: u32 = 4096;
pub const LIMIT_PROBES_PER_GAP: u32 = 64;
/// Bytes máximos del array de entradas de una cabecera GPT encontrada que se leen.
const MAX_FOUND_GPT_ARRAY: u64 = 64 * 1024;

/// Opciones de la búsqueda (INV-05b §8.2; valores por defecto documentados en
/// `docs/formatos/busqueda-particiones.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    /// Lecturas físicas totales (los aciertos de caché no cuentan).
    pub max_reads: u64,
    /// Bytes leídos totales.
    pub max_bytes: u64,
    /// Volúmenes confirmados tras los que se deja de buscar.
    pub max_volumes: u32,
    /// Bytes leídos en cada candidato (múltiplo de 512).
    pub probe_window: u32,
    /// Ventana inicial explorada con múltiplos de 1 MiB.
    pub start_grid_bytes: u64,
    /// Ventana final explorada con los finales de cada MiB.
    pub end_grid_bytes: u64,
    /// Límites de cilindro CHS explorados (`k × 16065` y `+ 63`).
    pub cylinders: u32,
    /// Candidatos por hueco sin explicar.
    pub probes_per_gap: u32,
    /// Reconocedores activos ([`family`]).
    pub families: u32,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_reads: DEFAULT_MAX_READS,
            max_bytes: DEFAULT_MAX_BYTES,
            max_volumes: DEFAULT_MAX_VOLUMES,
            probe_window: DEFAULT_PROBE_WINDOW,
            start_grid_bytes: DEFAULT_START_GRID,
            end_grid_bytes: DEFAULT_END_GRID,
            cylinders: DEFAULT_CYLINDERS,
            probes_per_gap: DEFAULT_PROBES_PER_GAP,
            families: family::ALL,
        }
    }
}

impl SearchOptions {
    /// Comprueba los límites de cada opción.
    pub fn validate(&self) -> Result<(), String> {
        let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
        check(
            (1..=LIMIT_MAX_READS).contains(&self.max_reads),
            "max_reads debe estar entre 1 y 1048576",
        )?;
        check(
            (512..=LIMIT_MAX_BYTES).contains(&self.max_bytes),
            "max_bytes debe estar entre 512 y 4 GiB",
        )?;
        check(
            (1..=LIMIT_MAX_VOLUMES).contains(&self.max_volumes),
            "max_volumes debe estar entre 1 y 4096",
        )?;
        check(
            (512..=LIMIT_PROBE_WINDOW).contains(&self.probe_window)
                && self.probe_window.is_multiple_of(512),
            "probe_window debe ser un múltiplo de 512 entre 512 y 65536",
        )?;
        check(
            self.start_grid_bytes <= LIMIT_GRID && self.end_grid_bytes <= LIMIT_GRID,
            "las ventanas de rejilla no pueden superar 1 TiB",
        )?;
        check(
            self.cylinders <= LIMIT_CYLINDERS,
            "cylinders no puede superar 4096",
        )?;
        check(
            self.probes_per_gap <= LIMIT_PROBES_PER_GAP,
            "probes_per_gap no puede superar 64",
        )?;
        check(
            self.families != 0 && self.families & !family::ALL == 0,
            "familias de reconocedores no válidas",
        )
    }
}

/// Nivel de un candidato (INV-05b §8.4). El orden es el de prioridad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// P0: declarados por la tabla del origen y el byte 0.
    Declared,
    /// P1: derivados de lo ya confirmado.
    Derived,
    /// P2: rejilla inicial (CHS y 1 MiB).
    StartGrid,
    /// P3: huecos sin explicar.
    Gaps,
    /// P4: rejilla final.
    EndGrid,
}

impl Tier {
    pub const ALL: [Tier; 5] = [
        Tier::Declared,
        Tier::Derived,
        Tier::StartGrid,
        Tier::Gaps,
        Tier::EndGrid,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// `true` en los niveles de rejilla y huecos (P2 a P4), sujetos a cupo.
    pub fn is_grid(self) -> bool {
        self >= Tier::StartGrid
    }

    /// Código estable: `p0` a `p4`.
    pub fn code(self) -> &'static str {
        ["p0", "p1", "p2", "p3", "p4"][self.index()]
    }

    pub fn label(self) -> &'static str {
        [
            "declarado por la tabla",
            "derivado",
            "rejilla inicial",
            "hueco",
            "rejilla final",
        ][self.index()]
    }
}

/// Sistema de archivos de un volumen encontrado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FsKind {
    Ntfs,
    Fat12,
    Fat16,
    Fat32,
    ExFat,
}

impl FsKind {
    /// Código estable (`ntfs`, `fat12`, `fat16`, `fat32`, `exfat`), como en el manifiesto del corpus.
    pub fn code(self) -> &'static str {
        match self {
            FsKind::Ntfs => "ntfs",
            FsKind::Fat12 => "fat12",
            FsKind::Fat16 => "fat16",
            FsKind::Fat32 => "fat32",
            FsKind::ExFat => "exfat",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            FsKind::Ntfs => "NTFS",
            FsKind::Fat12 => "FAT12",
            FsKind::Fat16 => "FAT16",
            FsKind::Fat32 => "FAT32",
            FsKind::ExFat => "exFAT",
        }
    }

    /// Tipo de entrada MBR que le corresponde (para una futura reinserción, PT-08).
    pub fn suggested_mbr_type(self) -> u8 {
        match self {
            FsKind::Ntfs | FsKind::ExFat => 0x07,
            FsKind::Fat12 => 0x01,
            FsKind::Fat16 => 0x0E,
            FsKind::Fat32 => 0x0C,
        }
    }

    /// Dígitos hexadecimales con que se muestra el número de serie.
    pub fn serial_digits(self) -> usize {
        if self == FsKind::Ntfs {
            16
        } else {
            8
        }
    }
}

/// Resultado triestado de una comprobación (más «fuera de la imagen» y «no aplica»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    /// Presente y válida.
    Yes,
    /// Ausente o dañada (los sectores se leyeron).
    No,
    /// No verificable: algún sector necesario es ilegible o se saltó.
    Unverifiable,
    /// La posición cae fuera de la imagen.
    Outside,
    /// No aplica (el sistema de archivos no tiene esa estructura, o no hay dato que comparar).
    NotApplicable,
}

impl Check {
    /// Símbolo del indicador: `+` sí, `x` no, `?` no verificable, `o` fuera de la imagen, `-` no
    /// aplica.
    pub fn symbol(self) -> char {
        match self {
            Check::Yes => '+',
            Check::No => 'x',
            Check::Unverifiable => '?',
            Check::Outside => 'o',
            Check::NotApplicable => '-',
        }
    }

    /// Código estable (JSON).
    pub fn code(self) -> &'static str {
        match self {
            Check::Yes => "yes",
            Check::No => "no",
            Check::Unverifiable => "unverifiable",
            Check::Outside => "outside_image",
            Check::NotApplicable => "not_applicable",
        }
    }

    /// Código numérico estable (ABI): 1 a 5.
    pub fn number(self) -> u32 {
        match self {
            Check::Yes => 1,
            Check::No => 2,
            Check::Unverifiable => 3,
            Check::Outside => 4,
            Check::NotApplicable => 5,
        }
    }

    /// Orden de preferencia al combinar varias posiciones posibles de una copia.
    fn rank(self) -> u8 {
        match self {
            Check::Yes => 4,
            Check::Unverifiable => 3,
            Check::No => 2,
            Check::Outside => 1,
            Check::NotApplicable => 0,
        }
    }
}

/// Anomalía de un volumen encontrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeAnomaly {
    /// La estructura principal se leyó y no es válida: candidata a restaurar desde la copia (PT-08).
    PrimaryDamaged { byte: i64 },
    /// La estructura principal está en un sector ilegible.
    PrimaryUnreadable { lba: u64 },
    /// El volumen empieza antes del inicio de la imagen: parcial, no se puede abrir.
    PrimaryOutsideImage { byte: i64 },
    /// No hay copia válida en su posición. `compensated`: el volumen está confirmado por otras
    /// estructuras (principal, estructuras básicas y entrada de tabla).
    BackupMissing { byte: i64, compensated: bool },
    /// La copia está en un sector ilegible.
    BackupUnreadable { lba: u64, compensated: bool },
    /// La copia cae fuera de la imagen.
    BackupOutsideImage { byte: i64, compensated: bool },
    /// Principal y copia válidas pero con campos clave distintos.
    BackupMismatch { byte: i64 },
    /// El volumen termina después del final de la imagen.
    BeyondImageEnd { missing_bytes: u64 },
    /// El sistema de archivos usa otro tamaño de sector que el dispositivo.
    SectorSizeMismatch { fs: u32, device: u32 },
    /// El inicio no es múltiplo del sector del dispositivo: no se puede abrir como rango.
    Unaligned { byte: i64 },
    /// El inicio declarado por el propio volumen difiere en el desplazamiento aceptado.
    ShiftDetected { delta_bytes: i64 },
    /// El inicio declarado difiere sin que haya un desplazamiento aceptado (informativa).
    SelfLocationMismatch {
        declared_byte: i64,
        actual_byte: i64,
    },
    /// Solapa con un volumen válido.
    Overlap { other_start: i64 },
    /// Solo la estructura principal, sin copia ni estructuras que la confirmen.
    Unconfirmed,
}

impl VolumeAnomaly {
    /// Código estable (JSON de la CLI, ABI textual).
    pub fn code(&self) -> &'static str {
        use VolumeAnomaly::*;
        match self {
            PrimaryDamaged { .. } => "found_primary_damaged",
            PrimaryUnreadable { .. } => "found_primary_unreadable",
            PrimaryOutsideImage { .. } => "found_primary_outside_image",
            BackupMissing { .. } => "found_backup_missing",
            BackupUnreadable { .. } => "found_backup_unreadable",
            BackupOutsideImage { .. } => "found_backup_outside_image",
            BackupMismatch { .. } => "found_backup_mismatch",
            BeyondImageEnd { .. } => "found_beyond_image_end",
            SectorSizeMismatch { .. } => "found_sector_size_mismatch",
            Unaligned { .. } => "found_unaligned",
            ShiftDetected { .. } => "found_shift_detected",
            SelfLocationMismatch { .. } => "found_self_location_mismatch",
            Overlap { .. } => "found_overlap",
            Unconfirmed => "found_unconfirmed",
        }
    }

    /// Código numérico estable (ABI, bloque 301 a 314).
    pub fn number(&self) -> u32 {
        use VolumeAnomaly::*;
        match self {
            PrimaryDamaged { .. } => 301,
            PrimaryUnreadable { .. } => 302,
            PrimaryOutsideImage { .. } => 303,
            BackupMissing { .. } => 304,
            BackupUnreadable { .. } => 305,
            BackupOutsideImage { .. } => 306,
            BackupMismatch { .. } => 307,
            BeyondImageEnd { .. } => 308,
            SectorSizeMismatch { .. } => 309,
            Unaligned { .. } => 310,
            ShiftDetected { .. } => 311,
            SelfLocationMismatch { .. } => 312,
            Overlap { .. } => 313,
            Unconfirmed => 314,
        }
    }

    pub fn severity(&self) -> Severity {
        use VolumeAnomaly::*;
        match self {
            SelfLocationMismatch { .. } => Severity::Notice,
            BackupMissing { compensated, .. }
            | BackupUnreadable { compensated, .. }
            | BackupOutsideImage { compensated, .. } => {
                if *compensated {
                    Severity::Notice
                } else {
                    Severity::Suspicious
                }
            }
            _ => Severity::Suspicious,
        }
    }

    /// Posición en la imagen, en bytes, si la tiene.
    pub fn byte(&self) -> Option<i64> {
        use VolumeAnomaly::*;
        match *self {
            PrimaryDamaged { byte }
            | PrimaryOutsideImage { byte }
            | BackupMissing { byte, .. }
            | BackupOutsideImage { byte, .. }
            | BackupMismatch { byte }
            | Unaligned { byte } => Some(byte),
            SelfLocationMismatch { actual_byte, .. } => Some(actual_byte),
            Overlap { other_start } => Some(other_start),
            _ => None,
        }
    }

    /// LBA del dispositivo (sector ilegible), si la tiene.
    pub fn lba(&self) -> Option<u64> {
        match *self {
            VolumeAnomaly::PrimaryUnreadable { lba }
            | VolumeAnomaly::BackupUnreadable { lba, .. } => Some(lba),
            _ => None,
        }
    }

    /// Valor asociado (con signo), si lo hay.
    pub fn value(&self) -> Option<i64> {
        use VolumeAnomaly::*;
        match *self {
            BeyondImageEnd { missing_bytes } => i64::try_from(missing_bytes).ok(),
            SectorSizeMismatch { fs, .. } => Some(i64::from(fs)),
            ShiftDetected { delta_bytes } => Some(delta_bytes),
            SelfLocationMismatch { declared_byte, .. } => Some(declared_byte),
            _ => None,
        }
    }
}

impl fmt::Display for VolumeAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use VolumeAnomaly::*;
        match self {
            PrimaryDamaged { byte } => write!(
                f,
                "estructura principal dañada en el byte {byte}; la copia es válida (restaurable desde la copia)"
            ),
            PrimaryUnreadable { lba } => write!(
                f,
                "estructura principal no verificable: el sector {lba} es ilegible"
            ),
            PrimaryOutsideImage { byte } => write!(
                f,
                "el volumen empieza en el byte {byte}, antes del inicio de la imagen: volumen parcial que no se puede abrir"
            ),
            BackupMissing { byte, .. } => {
                write!(f, "sin copia válida de la estructura en el byte {byte}")
            }
            BackupUnreadable { lba, .. } => {
                write!(f, "copia no verificable: el sector {lba} es ilegible")
            }
            BackupOutsideImage { byte, .. } => {
                write!(f, "la copia (byte {byte}) cae fuera de la imagen")
            }
            BackupMismatch { byte } => write!(
                f,
                "la copia del byte {byte} es válida pero no coincide con la principal"
            ),
            BeyondImageEnd { missing_bytes } => write!(
                f,
                "el volumen termina después del final de la imagen (faltan {missing_bytes} bytes)"
            ),
            SectorSizeMismatch { fs, device } => write!(
                f,
                "el sistema de archivos usa sectores de {fs} B y el origen está abierto con {device} B"
            ),
            Unaligned { byte } => write!(
                f,
                "el inicio (byte {byte}) no es múltiplo del sector del origen: no se puede abrir como rango"
            ),
            ShiftDetected { delta_bytes } => write!(
                f,
                "el volumen declara otro inicio: coincide con el desplazamiento detectado de la imagen ({delta_bytes} bytes); abrirlo exige aplicarlo explícitamente"
            ),
            SelfLocationMismatch {
                declared_byte,
                actual_byte,
            } => write!(
                f,
                "el volumen declara empezar en el byte {declared_byte} y está en el {actual_byte} (sectores ocultos relativos o no rellenados)"
            ),
            Overlap { other_start } => write!(
                f,
                "solapa con el volumen válido que empieza en el byte {other_start}"
            ),
            Unconfirmed => write!(
                f,
                "solo la estructura principal: sin copia ni estructuras básicas que la confirmen"
            ),
        }
    }
}

/// Anomalía de la imagen detectada por la búsqueda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAnomaly {
    /// Hipótesis de desplazamiento aceptada: `LBA imagen = LBA disco + delta / sector`.
    /// `evidence` = evidencias independientes que coinciden.
    ImageShifted {
        delta_bytes: i64,
        sector_size: u32,
        evidence: u32,
    },
    /// Tabla MBR del disco encontrada en `lba` de la imagen (imagen desplazada).
    MbrDisplaced { lba: u64 },
}

impl SearchAnomaly {
    /// Código estable, como en el manifiesto del corpus.
    pub fn code(&self) -> &'static str {
        match self {
            SearchAnomaly::ImageShifted { .. } => "image_shifted",
            SearchAnomaly::MbrDisplaced { .. } => "mbr_displaced",
        }
    }

    /// Código numérico estable (ABI).
    pub fn number(&self) -> u32 {
        match self {
            SearchAnomaly::ImageShifted { .. } => 320,
            SearchAnomaly::MbrDisplaced { .. } => 321,
        }
    }

    pub fn severity(&self) -> Severity {
        Severity::Suspicious
    }

    /// LBA de la imagen: la de la LBA 0 del disco (con signo) o la de la tabla desplazada.
    pub fn lba(&self) -> i64 {
        match *self {
            SearchAnomaly::ImageShifted {
                delta_bytes,
                sector_size,
                ..
            } => delta_bytes.div_euclid(i64::from(sector_size)),
            SearchAnomaly::MbrDisplaced { lba } => lba as i64,
        }
    }

    /// `ImageShifted`: sectores del disco que faltan al inicio de la imagen (negativo: sobran),
    /// como `leading_sectors_missing` del manifiesto.
    pub fn value(&self) -> Option<i64> {
        match self {
            SearchAnomaly::ImageShifted { .. } => Some(-self.lba()),
            SearchAnomaly::MbrDisplaced { .. } => None,
        }
    }
}

impl fmt::Display for SearchAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchAnomaly::ImageShifted { evidence, .. } => {
                let lba = self.lba();
                if lba >= 0 {
                    write!(
                        f,
                        "imagen desplazada: la LBA 0 del disco está en la LBA {lba} de la imagen ({lba} sectores añadidos al inicio; {evidence} evidencias independientes); no se aplica en silencio"
                    )
                } else {
                    write!(
                        f,
                        "imagen desplazada: faltan {} sectores del disco al inicio de la imagen ({evidence} evidencias independientes); no se aplica en silencio",
                        -lba
                    )
                }
            }
            SearchAnomaly::MbrDisplaced { lba } => write!(
                f,
                "MBR del disco encontrado en la LBA {lba} de la imagen (imagen desplazada)"
            ),
        }
    }
}

/// Volumen encontrado por la búsqueda, con su evidencia (indicadores) y su confianza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundVolume {
    pub fs: FsKind,
    /// Primer byte en la imagen (negativo si empieza antes del inicio de la imagen).
    pub start_byte: i64,
    pub length_bytes: u64,
    /// Bytes por sector del propio sistema de archivos.
    pub fs_sector_size: u32,
    /// Indicador P: estructura principal (sector de arranque).
    pub primary: Check,
    /// Indicador C: copia válida y coherente con la principal.
    pub backup: Check,
    /// Indicador S: estructuras básicas (registro 0 de `$MFT`, primera entrada de la FAT).
    pub structures: Check,
    /// Indicador E: una entrada de la tabla del origen (o de una tabla encontrada) lo describe.
    pub entry: Check,
    /// Indicador U: el inicio declarado por el propio volumen coincide con su posición.
    pub self_location: Check,
    /// Indicador A: inicio alineado a 1 MiB o a una convención CHS.
    pub aligned: Check,
    /// Posición de la estructura principal (bytes de la imagen).
    pub primary_byte: i64,
    /// Posición de la copia usada o esperada (bytes de la imagen), si el FS tiene copia.
    pub backup_byte: Option<i64>,
    /// Inicio declarado por el volumen en sus propios sectores (0 = no declarado).
    pub declared_start: u64,
    pub serial: u64,
    /// Nivel del candidato que lo encontró y descripción del origen.
    pub tier: Tier,
    pub origin: String,
    /// Partición de la tabla del origen que lo describe (índice en `PartitionTable::partitions`).
    pub table_partition: Option<usize>,
    /// Tabla encontrada que lo describe (índice en [`SearchResult::tables`]).
    pub found_table: Option<usize>,
    pub confidence: Confidence,
    pub anomalies: Vec<VolumeAnomaly>,
    /// Puntuación para ordenar y desempatar (no decide la confianza).
    pub score: u32,
}

impl FoundVolume {
    pub fn end_byte(&self) -> i64 {
        self.start_byte.saturating_add(self.length_bytes as i64)
    }

    /// Empieza antes del inicio de la imagen: no se puede abrir.
    pub fn partial(&self) -> bool {
        self.start_byte < 0
    }

    /// Lo describe una entrada de la tabla del origen: es esa partición, no una encontrada.
    pub fn listed(&self) -> bool {
        self.table_partition.is_some()
    }

    /// Abrirlo exige pedir explícitamente que se aplique el desplazamiento detectado.
    pub fn requires_shift(&self) -> bool {
        self.anomalies
            .iter()
            .any(|a| matches!(a, VolumeAnomaly::ShiftDetected { .. }))
    }

    /// Primera LBA del dispositivo (redondeo hacia abajo; negativa si es parcial).
    pub fn start_lba(&self, sector_size: u32) -> i64 {
        self.start_byte.div_euclid(i64::from(sector_size))
    }

    /// Sectores del dispositivo que ocupa (redondeo hacia arriba).
    pub fn sector_count(&self, sector_size: u32) -> u64 {
        self.length_bytes.div_ceil(u64::from(sector_size))
    }

    /// Indicadores compactos: `E` entrada, `P` principal, `C` copia, `S` estructuras, `U`
    /// autoubicación, `A` alineación, cada uno con su símbolo ([`Check::symbol`]).
    pub fn indicators(&self) -> String {
        [
            ('E', self.entry),
            ('P', self.primary),
            ('C', self.backup),
            ('S', self.structures),
            ('U', self.self_location),
            ('A', self.aligned),
        ]
        .iter()
        .map(|(l, c)| format!("{l}{}", c.symbol()))
        .collect::<Vec<_>>()
        .join(" ")
    }

    /// Copias válidas, como `evidence` del manifiesto del corpus (`boot`, `backup_boot`).
    pub fn evidence(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.primary == Check::Yes {
            v.push("boot");
        }
        if self.backup == Check::Yes {
            v.push("backup_boot");
        }
        v
    }
}

/// Estado final de la búsqueda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStatus {
    /// Todos los candidatos se sondearon.
    Complete,
    /// Se agotó el presupuesto (o el cupo de un nivel, o `max_volumes`): resultado parcial.
    BudgetExhausted,
    /// Cancelada por el usuario: resultado parcial.
    Cancelled,
}

impl SearchStatus {
    pub fn code(self) -> &'static str {
        match self {
            SearchStatus::Complete => "complete",
            SearchStatus::BudgetExhausted => "budget_exhausted",
            SearchStatus::Cancelled => "cancelled",
        }
    }

    /// Código numérico estable (ABI): 1 a 3.
    pub fn number(self) -> u32 {
        match self {
            SearchStatus::Complete => 1,
            SearchStatus::BudgetExhausted => 2,
            SearchStatus::Cancelled => 3,
        }
    }
}

/// Resultado de [`search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub status: SearchStatus,
    /// Tamaño de sector y sectores del dispositivo examinado.
    pub sector_size: u32,
    pub sectors: u64,
    /// Volúmenes por posición ascendente y, a igualdad, por puntuación.
    pub volumes: Vec<FoundVolume>,
    /// Tablas encontradas fuera de la posición de la tabla principal.
    pub tables: Vec<FoundTable>,
    pub anomalies: Vec<SearchAnomaly>,
    /// Desplazamiento aceptado (bytes, `posición imagen - posición disco`).
    pub shift_bytes: Option<i64>,
    pub stats: ReadStats,
    /// Candidatos sondeados.
    pub probed: u64,
    /// Candidatos que quedaron sin sondear, por nivel (P0 a P4).
    pub pending: [u64; 5],
    /// Se alcanzó `max_volumes`.
    pub volume_limit_reached: bool,
    pub options: SearchOptions,
}

/// Progreso de la búsqueda (se emite tras cada lectura física).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchProgress {
    pub reads: u64,
    pub max_reads: u64,
    pub bytes: u64,
    pub tier: Tier,
    /// Byte del candidato en curso.
    pub candidate_byte: i64,
    pub volumes: u64,
}

/// Observador de la búsqueda: cancelación (consultada antes de cada lectura física) y progreso.
pub trait SearchObserver {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _p: &SearchProgress) {}
}

/// Observador que no cancela ni informa.
pub struct NoObserver;

impl SearchObserver for NoObserver {}

/// Busca volúmenes perdidos en `dev`.
///
/// `table` es la tabla leída con [`crate::read_partition_table`] (sus particiones, aunque sean
/// dudosas, son candidatos P0, y los volúmenes que coinciden con una entrada se marcan con ella en
/// vez de duplicarse). Sus lecturas no cuentan para el presupuesto. Solo falla con opciones no
/// válidas; los errores de lectura se tratan como sectores ilegibles.
pub fn search(
    dev: &dyn BlockDevice,
    table: Option<&PartitionTable>,
    opts: &SearchOptions,
    observer: &dyn SearchObserver,
) -> Result<SearchResult, String> {
    opts.validate()?;
    let cache = u64::from(opts.probe_window) * 64 + MIB;
    let rd = Reader::new(dev, opts.max_reads, opts.max_bytes, cache, observer);
    let mut e = Engine::new(rd, table, *opts);
    e.run();
    Ok(e.finish())
}

// ---------------------------------------------------------------------------------------------
// Motor
// ---------------------------------------------------------------------------------------------

/// Clase de candidato: inicio de volumen, final de volumen (copia al final) o tabla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Start,
    End,
    /// Sector de tabla; `ext_base` = LBA del inicio de la extendida si es un EBR.
    Table {
        ext_base: Option<u64>,
    },
}

#[derive(Debug, Clone)]
struct Candidate {
    pos: i128,
    tier: Tier,
    kind: Kind,
    origin: String,
}

/// Partición de la tabla del origen en coordenadas de la imagen.
struct TablePart {
    start: i128,
    len: i128,
    valid: bool,
}

/// Hipótesis evaluada.
#[derive(Debug, Clone)]
struct Eval {
    rec: usize,
    start: i128,
    primary: Check,
    primary_bad: Option<u64>,
    backup: Check,
    backup_pos: Option<i128>,
    backup_bad: Option<u64>,
    mismatch: bool,
    boot: Boot,
    length: u64,
}

impl Eval {
    fn score(&self) -> i32 {
        let b = match self.primary {
            Check::Yes => 4,
            Check::Unverifiable => 1,
            _ => 0,
        };
        let c = i32::from(self.backup == Check::Yes) * 3;
        b + c
    }

    /// La mejor evidencia posible: no hace falta evaluar otras hipótesis.
    fn complete(&self) -> bool {
        self.primary == Check::Yes && matches!(self.backup, Check::Yes | Check::NotApplicable)
    }
}

/// Evidencia de desplazamiento: `(delta en bytes, fuente)`.
type ShiftEvidence = (i128, u64);

struct Engine<'a> {
    rd: Reader<'a>,
    opts: SearchOptions,
    table: Option<&'a PartitionTable>,
    parts: Vec<TablePart>,
    /// Posiciones de las tablas que ya leyó el lector de la tabla principal (copias GPT y EBR de
    /// la cadena): no son tablas «encontradas».
    main_tables: Vec<i128>,
    queue: BTreeSet<(Tier, i128, u64)>,
    cands: BTreeMap<u64, Candidate>,
    seen: BTreeSet<i128>,
    seen_tables: BTreeSet<i128>,
    seq: u64,
    volumes: Vec<FoundVolume>,
    /// Estructuras ya explicadas por un volumen aceptado: `(reconocedor, posición)`.
    explained: BTreeSet<(usize, i128)>,
    evaluated: BTreeSet<(usize, i128)>,
    tables: Vec<FoundTable>,
    anomalies: Vec<SearchAnomaly>,
    evidence: Vec<ShiftEvidence>,
    shift: Option<i128>,
    gaps_dirty: bool,
    capped: [bool; 5],
    pending: [u64; 5],
    probed: u64,
    status: SearchStatus,
    volume_limit: bool,
}

/// `v` redondeado hacia arriba a un múltiplo de `m` (> 0).
fn align_up(v: i128, m: i128) -> i128 {
    v.div_euclid(m) * m + if v.rem_euclid(m) == 0 { 0 } else { m }
}

fn align_down(v: i128, m: i128) -> i128 {
    v.div_euclid(m) * m
}

fn to_i64(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

fn bytes_text(pos: i128, ss: u64) -> String {
    format!("LBA {}", pos.div_euclid(i128::from(ss)))
}

impl<'a> Engine<'a> {
    fn new(rd: Reader<'a>, table: Option<&'a PartitionTable>, opts: SearchOptions) -> Self {
        let mut parts = Vec::new();
        let mut main_tables = Vec::new();
        if let Some(t) = table {
            let ts = i128::from(t.sector_size);
            for p in &t.partitions {
                if let Origin::Logical { ebr_lba, .. } = p.origin {
                    main_tables.push(i128::from(ebr_lba) * ts);
                }
                let shift = match p.origin {
                    Origin::Gpt { .. } => i128::from(t.shift()),
                    _ => 0,
                };
                parts.push(TablePart {
                    start: (i128::from(p.start_lba) - shift) * ts,
                    len: i128::from(p.sector_count) * ts,
                    valid: p.confidence == Confidence::Valid,
                });
            }
            if let Some(g) = &t.gpt {
                let gs = i128::from(g.sector_size);
                for c in [&g.primary, &g.backup] {
                    if let Some(l) = c.image_lba {
                        main_tables.push(i128::from(l) * gs);
                    }
                }
            }
        }
        Self {
            rd,
            opts,
            table,
            parts,
            main_tables,
            queue: BTreeSet::new(),
            cands: BTreeMap::new(),
            seen: BTreeSet::new(),
            seen_tables: BTreeSet::new(),
            seq: 0,
            volumes: Vec::new(),
            explained: BTreeSet::new(),
            evaluated: BTreeSet::new(),
            tables: Vec::new(),
            anomalies: Vec::new(),
            evidence: Vec::new(),
            shift: None,
            gaps_dirty: true,
            capped: [false; 5],
            pending: [0; 5],
            probed: 0,
            status: SearchStatus::Complete,
            volume_limit: false,
        }
    }

    fn ss(&self) -> i128 {
        i128::from(self.rd.sector_size())
    }

    fn size(&self) -> i128 {
        i128::from(self.rd.size())
    }

    /// Encola un candidato si su posición no se ha encolado antes. Los de rejilla deben estar
    /// alineados al sector del dispositivo y dentro de la imagen.
    fn push(&mut self, pos: i128, tier: Tier, kind: Kind, origin: impl Into<String>) {
        if tier.is_grid() && (pos < 0 || pos >= self.size() || pos.rem_euclid(self.ss()) != 0) {
            return;
        }
        if matches!(kind, Kind::Table { .. }) {
            // Una tabla en una posición ya encolada como volumen se prueba igualmente.
            if !self.seen_tables.insert(pos) {
                return;
            }
        } else if !self.seen.insert(pos) {
            return;
        }
        self.seq += 1;
        self.queue.insert((tier, pos, self.seq));
        self.cands.insert(
            self.seq,
            Candidate {
                pos,
                tier,
                kind,
                origin: origin.into(),
            },
        );
    }

    fn seed_candidates(&mut self) {
        let ss = self.ss();
        let size = self.size();
        // P0: byte 0 y lo declarado por la tabla.
        self.push(0, Tier::Declared, Kind::Start, "byte 0 del origen");
        let declared: Vec<(i128, i128)> = self.parts.iter().map(|p| (p.start, p.len)).collect();
        for (i, (start, len)) in declared.into_iter().enumerate() {
            self.push(
                start,
                Tier::Declared,
                Kind::Start,
                format!("inicio de la partición {} de la tabla", i + 1),
            );
            self.push(
                start + len - ss,
                Tier::Declared,
                Kind::End,
                format!("último sector de la partición {} de la tabla", i + 1),
            );
        }
        // P2: rejilla inicial.
        let mib = i128::from(MIB);
        self.push(
            i128::from(TRACK),
            Tier::StartGrid,
            Kind::Start,
            "LBA 63 (CHS)",
        );
        for k in 1..=i128::from(self.opts.cylinders) {
            let c = k * i128::from(CYLINDER);
            if c >= size {
                break;
            }
            self.push(c, Tier::StartGrid, Kind::Start, "límite de cilindro CHS");
            self.push(
                c + i128::from(TRACK),
                Tier::StartGrid,
                Kind::Start,
                "límite de cilindro CHS + 63",
            );
        }
        let grid = i128::from(self.opts.start_grid_bytes).min(size);
        let mut m = 0;
        while m <= grid && m < size {
            self.push(m, Tier::StartGrid, Kind::Start, "múltiplo de 1 MiB");
            m += mib;
        }
        // P4: rejilla final.
        for s in [ss, 512, 4096] {
            self.push(
                size - s,
                Tier::EndGrid,
                Kind::End,
                "último sector del origen",
            );
        }
        let window = i128::from(self.opts.end_grid_bytes);
        let mut k = 1;
        while k * mib <= window && k * mib < size {
            self.push(
                size - k * mib - ss,
                Tier::EndGrid,
                Kind::End,
                "final de MiB contado desde el final del origen",
            );
            self.push(
                align_down(size, mib) - (k - 1) * mib - ss,
                Tier::EndGrid,
                Kind::End,
                "final de MiB contado desde el inicio del origen",
            );
            k += 1;
        }
    }

    /// P3: candidatos de los huecos que no explica ningún volumen confirmado.
    fn push_gaps(&mut self) {
        let ss = self.ss();
        let size = self.size();
        let mib = i128::from(MIB);
        let mut spans: Vec<(i128, i128)> = self
            .volumes
            .iter()
            .map(|v| {
                (
                    i128::from(v.start_byte).max(0),
                    i128::from(v.end_byte()).min(size),
                )
            })
            .filter(|(a, b)| a < b)
            .collect();
        spans.sort_unstable();
        let mut gaps = Vec::new();
        let mut at = 0;
        for (a, b) in spans {
            if a > at {
                gaps.push((at, a));
            }
            at = at.max(b);
        }
        if at < size {
            gaps.push((at, size));
        }
        for (a, b) in gaps {
            if b - a < ss {
                continue;
            }
            let cyl = align_up(a, i128::from(CYLINDER));
            let tail = align_down(b, mib);
            let list = [
                (a, Kind::Start),
                (align_up(a, mib), Kind::Start),
                (cyl, Kind::Start),
                (cyl + i128::from(TRACK), Kind::Start),
                (b - ss, Kind::End),
                (tail - ss, Kind::End),
                (tail - mib - ss, Kind::End),
            ];
            let mut n = 0;
            for (pos, kind) in list {
                if n >= self.opts.probes_per_gap {
                    break;
                }
                if pos < a || pos >= b {
                    continue;
                }
                let origin = format!(
                    "hueco sin explicar {}-{}",
                    bytes_text(a, self.rd.sector_size()),
                    bytes_text(b - 1, self.rd.sector_size())
                );
                self.push(pos, Tier::Gaps, kind, origin);
                n += 1;
            }
        }
    }

    fn run(&mut self) {
        self.seed_candidates();
        loop {
            if self.volumes.len() >= self.opts.max_volumes as usize {
                self.volume_limit = true;
                self.status = SearchStatus::BudgetExhausted;
                break;
            }
            if self.gaps_dirty {
                let next = self.queue.first().map(|k| k.0);
                if next.is_none_or(|t| t >= Tier::Gaps) {
                    self.gaps_dirty = false;
                    self.push_gaps();
                    continue;
                }
            }
            let Some(key) = self.queue.pop_first() else {
                break;
            };
            let Some(c) = self.cands.remove(&key.2) else {
                continue;
            };
            let t = c.tier.index();
            if self.capped[t] {
                self.pending[t] += 1;
                continue;
            }
            if c.tier.is_grid() && self.inside_valid_volume(c.pos) {
                continue;
            }
            self.rd.tier = c.tier;
            self.rd.candidate = to_i64(c.pos);
            self.probed += 1;
            match self.probe(&c) {
                Ok(()) => {}
                Err(Stop::TierCap) => {
                    self.capped[t] = true;
                    self.pending[t] += 1;
                    self.status = SearchStatus::BudgetExhausted;
                }
                Err(stop) => {
                    self.status = if stop == Stop::Cancelled {
                        SearchStatus::Cancelled
                    } else {
                        SearchStatus::BudgetExhausted
                    };
                    self.pending[t] += 1;
                    for (tier, _, _) in std::mem::take(&mut self.queue) {
                        self.pending[tier.index()] += 1;
                    }
                    break;
                }
            }
        }
    }

    /// `true` si `pos` cae estrictamente dentro de un volumen ya confirmado como válido.
    fn inside_valid_volume(&self, pos: i128) -> bool {
        self.volumes.iter().any(|v| {
            provisional_valid(v) && pos > i128::from(v.start_byte) && pos < i128::from(v.end_byte())
        })
    }

    /// `true` si `pos` cae estrictamente dentro de una partición válida de la tabla o de un volumen
    /// válido: ahí no se buscan tablas (un sector con aspecto de MBR dentro de datos es un señuelo).
    fn inside_valid_data(&self, pos: i128) -> bool {
        self.inside_valid_volume(pos)
            || self
                .parts
                .iter()
                .any(|p| p.valid && pos > p.start && pos < p.start + p.len)
    }

    fn active(&self, i: usize) -> bool {
        self.opts.families & RECOGNIZERS[i].family() != 0
    }

    fn probe(&mut self, c: &Candidate) -> Result<(), Stop> {
        if c.pos < 0 || c.pos >= self.size() {
            return Ok(());
        }
        if let Kind::Table { ext_base } = c.kind {
            return self.probe_table(c, ext_base);
        }
        let window = (i128::from(self.opts.probe_window)).min(self.size() - c.pos);
        // Una lectura de la ventana completa: los reconocedores comparten sus sectores.
        self.rd.get(c.pos, window as usize)?;
        let mut found = false;
        for (ri, rec) in RECOGNIZERS.iter().enumerate() {
            if !self.active(ri) {
                continue;
            }
            let q = c.pos + i128::from(rec.offset());
            if self.explained.contains(&(ri, q)) {
                found = true;
                continue;
            }
            if let Bytes::Data(d) = self.rd.get(q, 512)? {
                if let Some(b) = rec.parse(&d) {
                    found = true;
                    self.seed_hypotheses(ri, q, b, c)?;
                }
            }
        }
        // Copias cercanas dentro de la ventana (FAT32 en el sector 6, exFAT en el 12) y, para los
        // candidatos declarados, las mismas con sectores de 4096 B (INV-05b §8.6, pasos 1 y 5).
        let mut near: Vec<i128> = [6 * 512, 12 * 512]
            .into_iter()
            .filter(|o| o + 512 <= window)
            .collect();
        if !found && c.tier == Tier::Declared && self.ss() == 512 {
            near.extend([6 * 4096, 12 * 4096]);
        }
        for off in near {
            for (ri, rec) in RECOGNIZERS.iter().enumerate() {
                let q = c.pos + off;
                if !self.active(ri) || self.explained.contains(&(ri, q)) {
                    continue;
                }
                let Bytes::Data(d) = self.rd.get(q, 512)? else {
                    continue;
                };
                let Some(b) = rec.parse(&d) else {
                    continue;
                };
                if !b.backups.iter().any(|(o, _)| i128::from(*o) == off) {
                    continue;
                }
                found = true;
                if self.evaluated.insert((ri, c.pos)) {
                    if let Some(e) = self.evaluate(ri, c.pos, Some((q, &b)))? {
                        self.accept(e, c)?;
                    }
                }
            }
        }
        if !found {
            self.probe_table(c, None)?;
        }
        Ok(())
    }

    /// Hipótesis a partir de una estructura encontrada en `q`: que sea la principal (inicio en `q`)
    /// o una de sus copias (inicio en `q - desplazamiento de la copia`). Se elige la de mejor
    /// evidencia, prefiriendo la principal en candidatos de inicio y la copia en los de final.
    fn seed_hypotheses(&mut self, ri: usize, q: i128, b: Boot, c: &Candidate) -> Result<(), Stop> {
        let off = i128::from(RECOGNIZERS[ri].offset());
        let mut hyps: Vec<(i128, bool)> = vec![(q - off, true)];
        hyps.extend(b.backups.iter().map(|(o, _)| (q - i128::from(*o), false)));
        let prefer_primary = c.kind != Kind::End;
        hyps.sort_by_key(|(_, primary)| *primary != prefer_primary);
        let mut best: Option<(i32, Eval)> = None;
        for (start, primary) in hyps {
            if !self.evaluated.insert((ri, start)) {
                continue;
            }
            let Some(e) = self.evaluate(ri, start, Some((q, &b)))? else {
                continue;
            };
            let mut score = e.score();
            if primary == prefer_primary {
                score += 2;
            }
            if e.start + i128::from(e.length) > self.size() {
                score -= 1;
            }
            if e.complete() && primary == prefer_primary {
                best = Some((score, e));
                break;
            }
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, e));
            }
        }
        if let Some((_, e)) = best {
            self.accept(e, c)?;
        }
        Ok(())
    }

    /// Lee y comprueba una copia de la estructura en `pos`.
    fn read_copy(
        &mut self,
        ri: usize,
        pos: i128,
    ) -> Result<(Check, Option<Boot>, Option<u64>), Stop> {
        let rec = RECOGNIZERS[ri];
        if pos < 0 {
            return Ok((Check::Outside, None, None));
        }
        let d = match self.rd.get(pos, 512)? {
            Bytes::Outside => return Ok((Check::Outside, None, None)),
            Bytes::Unreadable(l) => return Ok((Check::Unverifiable, None, Some(l))),
            Bytes::Data(d) => d,
        };
        let Some(b) = rec.parse(&d) else {
            return Ok((Check::No, None, None));
        };
        let n = rec.copy_len(&b);
        if n > 512 {
            match self.rd.get(pos, n as usize)? {
                Bytes::Data(r) if rec.copy_ok(&b, &r) => {}
                Bytes::Unreadable(l) => return Ok((Check::Unverifiable, None, Some(l))),
                _ => return Ok((Check::No, None, None)),
            }
        }
        Ok((Check::Yes, Some(b), None))
    }

    /// Evalúa la hipótesis «volumen de `RECOGNIZERS[ri]` que empieza en `start`». `seed` es la
    /// estructura ya encontrada (posición y campos), que sirve de referencia si la principal no es
    /// válida. Devuelve `None` si ni la principal ni ninguna copia son válidas.
    fn evaluate(
        &mut self,
        ri: usize,
        start: i128,
        seed: Option<(i128, &Boot)>,
    ) -> Result<Option<Eval>, Stop> {
        let rec = RECOGNIZERS[ri];
        let ppos = start + i128::from(rec.offset());
        let (primary, pboot, primary_bad) = self.read_copy(ri, ppos)?;
        // Sin principal válida, la referencia es la estructura encontrada si está en la posición
        // de la principal (sector legible pero región dañada) o en la de una de sus copias.
        let reference = match (&pboot, seed) {
            (Some(b), _) => b.clone(),
            (None, Some((q, b)))
                if q == ppos || b.backups.iter().any(|(o, _)| start + i128::from(*o) == q) =>
            {
                b.clone()
            }
            _ => return Ok(None),
        };
        let mut backup = Check::NotApplicable;
        let mut backup_pos = None;
        let mut backup_bad = None;
        let mut mismatch = false;
        let mut length = reference.length;
        for &(o, len) in &reference.backups {
            let pos = start + i128::from(o);
            let (mut state, cboot, bad) = self.read_copy(ri, pos)?;
            let mut this_mismatch = false;
            if let (Some(p), Some(cb)) = (&pboot, &cboot) {
                if !rec.consistent(p, cb) {
                    state = Check::No;
                    this_mismatch = true;
                }
            }
            if backup == Check::NotApplicable || state.rank() > backup.rank() {
                backup = state;
                backup_pos = Some(pos);
                backup_bad = bad;
                mismatch = this_mismatch;
                if state == Check::Yes {
                    length = len;
                    break;
                }
            }
        }
        if primary != Check::Yes && backup != Check::Yes {
            return Ok(None);
        }
        Ok(Some(Eval {
            rec: ri,
            start,
            primary,
            primary_bad,
            backup,
            backup_pos,
            backup_bad,
            mismatch,
            boot: reference,
            length,
        }))
    }

    fn structures(&mut self, e: &Eval) -> Result<Check, Stop> {
        let rec = RECOGNIZERS[e.rec];
        let Some((off, len)) = rec.structures(&e.boot) else {
            return Ok(Check::NotApplicable);
        };
        Ok(
            match self.rd.get(e.start + i128::from(off), len as usize)? {
                Bytes::Data(d) if rec.structures_ok(&e.boot, &d) => Check::Yes,
                Bytes::Data(_) => Check::No,
                Bytes::Unreadable(_) => Check::Unverifiable,
                Bytes::Outside => Check::Outside,
            },
        )
    }

    /// Acepta un volumen: lo registra, marca sus estructuras como explicadas, encadena el siguiente
    /// candidato y actualiza la hipótesis de desplazamiento.
    fn accept(&mut self, e: Eval, c: &Candidate) -> Result<(), Stop> {
        let fs = e.boot.fs;
        if self
            .volumes
            .iter()
            .any(|v| v.fs == fs && i128::from(v.start_byte) == e.start)
        {
            return Ok(());
        }
        let structures = if e.primary == Check::Yes || e.backup == Check::Yes {
            self.structures(&e)?
        } else {
            Check::NotApplicable
        };
        let off = i128::from(RECOGNIZERS[e.rec].offset());
        if e.primary == Check::Yes {
            self.explained.insert((e.rec, e.start + off));
        }
        if let (Check::Yes, Some(p)) = (e.backup, e.backup_pos) {
            self.explained.insert((e.rec, p));
        }
        let mut anomalies = Vec::new();
        if let Some(l) = e.primary_bad {
            anomalies.push(VolumeAnomaly::PrimaryUnreadable { lba: l });
        }
        if let (true, Some(p)) = (e.mismatch, e.backup_pos) {
            anomalies.push(VolumeAnomaly::BackupMismatch { byte: to_i64(p) });
        }
        let v = FoundVolume {
            fs,
            start_byte: to_i64(e.start),
            length_bytes: e.length,
            fs_sector_size: e.boot.sector_size,
            primary: e.primary,
            backup: e.backup,
            structures,
            entry: Check::No,
            self_location: Check::NotApplicable,
            aligned: Check::No,
            primary_byte: to_i64(e.start + off),
            backup_byte: e.backup_pos.map(to_i64),
            declared_start: e.boot.declared_start,
            serial: e.boot.serial,
            tier: c.tier,
            origin: format!(
                "{} ({}, {})",
                c.origin,
                c.tier.code().to_uppercase(),
                bytes_text(c.pos, self.rd.sector_size())
            ),
            table_partition: None,
            found_table: None,
            confidence: Confidence::Valid,
            anomalies: {
                if let Some(l) = e.backup_bad {
                    anomalies.push(VolumeAnomaly::BackupUnreadable {
                        lba: l,
                        compensated: false,
                    });
                }
                anomalies
            },
            score: 0,
        };
        let end = i128::from(v.end_byte());
        let declared = v.declared_start;
        let fs_ss = i128::from(v.fs_sector_size);
        let start = e.start;
        self.volumes.push(v);
        self.rd.volumes = self.volumes.len() as u64;
        self.gaps_dirty = true;
        // P1: encadenado tras el final del volumen.
        let n = self.volumes.len();
        let label = format!("tras el volumen {n} ({})", fs.name());
        let mib = i128::from(MIB);
        for (pos, what) in [
            (end, "final exacto"),
            (align_up(end, mib), "final alineado a 1 MiB"),
            (
                align_up(end, i128::from(CYLINDER)),
                "final alineado a cilindro",
            ),
            (
                align_up(end, i128::from(CYLINDER)) + i128::from(TRACK),
                "final alineado a cilindro + 63",
            ),
        ] {
            if pos < self.size() {
                self.push(pos, Tier::Derived, Kind::Start, format!("{label}, {what}"));
            }
        }
        // Evidencia de desplazamiento: el volumen declara otro inicio.
        if declared != 0 {
            let delta = start - i128::from(declared) * fs_ss;
            if delta != 0 {
                self.evidence.push((delta, 1000 + n as u64));
                self.consider_shift();
            }
        }
        Ok(())
    }

    /// Sondea tablas en la posición del candidato: cabecera GPT y MBR/EBR en modo estricto.
    fn probe_table(&mut self, c: &Candidate, ext_base: Option<u64>) -> Result<(), Stop> {
        let ss = self.ss();
        if c.pos < 0 || c.pos.rem_euclid(ss) != 0 {
            return Ok(());
        }
        let explicit = matches!(c.kind, Kind::Table { .. });
        if !explicit && self.inside_valid_data(c.pos) {
            return Ok(());
        }
        let lba = (c.pos / ss) as u64;
        if self.main_tables.contains(&c.pos) || self.tables.iter().any(|t| t.lba == lba) {
            return Ok(());
        }
        let Bytes::Data(d) = self.rd.get(c.pos, ss as usize)? else {
            return Ok(());
        };
        if self.opts.families & family::GPT != 0 && d.starts_with(crate::gpt::SIGNATURE) {
            if let Some(h) = tables::gpt_header(&d) {
                return self.found_gpt(c.pos, lba, h);
            }
        }
        if self.opts.families & family::MBR == 0 || (c.pos == 0 && ext_base.is_none()) {
            return Ok(());
        }
        let Some(entries) =
            tables::strict_mbr(&d, c.pos as u64, ext_base, ss as u64, self.rd.sectors())
        else {
            return Ok(());
        };
        let t = self.tables.len();
        self.tables.push(FoundTable {
            kind: FoundTableKind::Mbr,
            lba,
            declared_lba: 0,
            entries: entries.clone(),
        });
        if self.shift == Some(c.pos) && ext_base.is_none() {
            self.anomalies.push(SearchAnomaly::MbrDisplaced { lba });
        }
        for e in entries {
            let start = i128::from(e.start_byte);
            if e.extended {
                let base = ext_base.unwrap_or((start / ss) as u64);
                self.push(
                    start,
                    Tier::Derived,
                    Kind::Table {
                        ext_base: Some(base),
                    },
                    format!("enlace de la tabla encontrada en la LBA {lba}"),
                );
            } else {
                let what = format!("entrada de la tabla encontrada {} (LBA {lba})", t + 1);
                self.push(start, Tier::Derived, Kind::Start, what.clone());
                self.push(
                    start + i128::from(e.length_bytes) - ss,
                    Tier::Derived,
                    Kind::End,
                    what,
                );
            }
        }
        Ok(())
    }

    /// Cabecera GPT encontrada fuera de las posiciones de la tabla principal: evidencia de
    /// desplazamiento y, si su array de entradas es legible y correcto, candidatos P1.
    fn found_gpt(&mut self, pos: i128, lba: u64, h: crate::gpt::GptHeader) -> Result<(), Stop> {
        let ss = self.ss();
        let delta = pos - i128::from(h.my_lba) * ss;
        let t = self.tables.len();
        let mut entries = Vec::new();
        let bytes = h.array_bytes();
        if bytes > 0 && bytes <= MAX_FOUND_GPT_ARRAY {
            let at = i128::from(h.entries_lba) * ss + delta;
            if let Bytes::Data(d) = self.rd.get(at, bytes as usize)? {
                if crc32fast::hash(&d) == h.entries_crc32 {
                    for (first, last) in tables::gpt_entries(&h, &d) {
                        let start = i128::from(first) * ss + delta;
                        let len = (i128::from(last) - i128::from(first) + 1) * ss;
                        entries.push(FoundEntry {
                            start_byte: to_i64(start),
                            length_bytes: u64::try_from(len).unwrap_or(0),
                            mbr_type: 0,
                            extended: false,
                        });
                        let what = format!("entrada GPT de la cabecera encontrada en la LBA {lba}");
                        self.push(start, Tier::Derived, Kind::Start, what.clone());
                        self.push(start + len - ss, Tier::Derived, Kind::End, what);
                    }
                }
            }
        }
        self.tables.push(FoundTable {
            kind: FoundTableKind::Gpt,
            lba,
            declared_lba: h.my_lba,
            entries,
        });
        if delta != 0 {
            self.evidence.push((delta, 2_000_000 + t as u64));
            self.consider_shift();
        }
        Ok(())
    }

    /// Acepta la hipótesis de desplazamiento si dos evidencias independientes coinciden en un
    /// `delta` distinto de 0 y ninguna entrada de la tabla es coherente con `delta = 0`.
    fn consider_shift(&mut self) {
        if self.shift.is_some() {
            return;
        }
        let mut evidence = self.evidence.clone();
        if let Some(t) = self.table {
            if t.shift() != 0 {
                if let Some(g) = &t.gpt {
                    evidence.push((-i128::from(t.shift()) * i128::from(g.sector_size), 1));
                }
            }
        }
        let coherent_zero = self.parts.iter().any(|p| {
            self.volumes
                .iter()
                .any(|v| i128::from(v.start_byte) == p.start)
        });
        if coherent_zero {
            return;
        }
        let mut by_delta: BTreeMap<i128, BTreeSet<u64>> = BTreeMap::new();
        for (d, src) in evidence {
            by_delta.entry(d).or_default().insert(src);
        }
        let best = by_delta
            .iter()
            .filter(|(_, s)| s.len() >= 2)
            .max_by(|a, b| {
                a.1.len()
                    .cmp(&b.1.len())
                    .then(b.0.abs().cmp(&a.0.abs()))
                    .then(b.0.cmp(a.0))
            })
            .map(|(d, s)| (*d, s.len()));
        let Some((delta, n)) = best else {
            return;
        };
        self.shift = Some(delta);
        self.anomalies.push(SearchAnomaly::ImageShifted {
            delta_bytes: to_i64(delta),
            sector_size: self.rd.sector_size() as u32,
            evidence: n as u32,
        });
        // P1: posiciones corregidas por el desplazamiento (nunca se reescribe nada).
        let ss = self.ss();
        let declared: Vec<(i128, i128)> = self.parts.iter().map(|p| (p.start, p.len)).collect();
        for (i, (start, len)) in declared.into_iter().enumerate() {
            let what = format!(
                "partición {} de la tabla corregida por el desplazamiento",
                i + 1
            );
            self.push(start + delta, Tier::Derived, Kind::Start, what.clone());
            self.push(start + len - ss + delta, Tier::Derived, Kind::End, what);
        }
        self.push(
            delta,
            Tier::Derived,
            Kind::Table { ext_base: None },
            "MBR del disco según el desplazamiento",
        );
        self.push(
            delta + ss,
            Tier::Derived,
            Kind::Table { ext_base: None },
            "cabecera GPT del disco según el desplazamiento",
        );
    }

    /// Une el resultado con la tabla, calcula indicadores, anomalías y confianza, y ordena.
    fn finish(mut self) -> SearchResult {
        let ss = self.rd.sector_size();
        let size = self.rd.size();
        let mut volumes = std::mem::take(&mut self.volumes);
        for v in &mut volumes {
            self.complete_volume(v, ss, size);
        }
        // Solapes: el sospechoso que solapa con uno válido lo indica (no se oculta).
        let valid: Vec<(i64, i64)> = volumes
            .iter()
            .filter(|v| v.confidence == Confidence::Valid)
            .map(|v| (v.start_byte, v.end_byte()))
            .collect();
        for v in &mut volumes {
            if v.confidence == Confidence::Valid {
                continue;
            }
            if let Some((s, _)) = valid
                .iter()
                .find(|(s, e)| *s < v.end_byte() && v.start_byte < *e)
            {
                v.anomalies.push(VolumeAnomaly::Overlap { other_start: *s });
                v.confidence = volume_confidence(v);
            }
        }
        volumes.sort_by(|a, b| {
            a.start_byte
                .cmp(&b.start_byte)
                .then(b.score.cmp(&a.score))
                .then(a.fs.cmp(&b.fs))
        });
        SearchResult {
            status: self.status,
            sector_size: ss as u32,
            sectors: self.rd.sectors(),
            volumes,
            tables: self.tables,
            anomalies: self.anomalies,
            shift_bytes: self.shift.map(to_i64),
            stats: self.rd.stats.clone(),
            probed: self.probed,
            pending: self.pending,
            volume_limit_reached: self.volume_limit,
            options: self.opts,
        }
    }

    fn complete_volume(&self, v: &mut FoundVolume, ss: u64, size: u64) {
        let start = i128::from(v.start_byte);
        let len = i128::from(v.length_bytes);
        // E: entrada de la tabla del origen o de una tabla encontrada.
        v.table_partition = self
            .parts
            .iter()
            .position(|p| p.start == start && p.len >= len);
        v.found_table = self.tables.iter().position(|t| {
            t.entries.iter().any(|e| {
                !e.extended
                    && i128::from(e.start_byte) == start
                    && i128::from(e.length_bytes) >= len
            })
        });
        v.entry = if v.table_partition.is_some() || v.found_table.is_some() {
            Check::Yes
        } else {
            Check::No
        };
        // U: autoubicación.
        let declared = i128::from(v.declared_start) * i128::from(v.fs_sector_size);
        v.self_location = if v.declared_start == 0 {
            Check::NotApplicable
        } else if declared == start {
            Check::Yes
        } else {
            Check::No
        };
        if v.self_location == Check::No {
            if self.shift == Some(start - declared) {
                v.anomalies.push(VolumeAnomaly::ShiftDetected {
                    delta_bytes: to_i64(start - declared),
                });
            } else {
                v.anomalies.push(VolumeAnomaly::SelfLocationMismatch {
                    declared_byte: to_i64(declared),
                    actual_byte: v.start_byte,
                });
            }
        }
        // A: alineación (1 MiB o convención CHS en sectores de 512 B).
        let lba512 = start.div_euclid(512);
        v.aligned = if start >= 0
            && (start % i128::from(MIB) == 0
                || (start % 512 == 0 && (lba512 % 16065 == 0 || lba512 % 16065 == 63)))
        {
            Check::Yes
        } else {
            Check::No
        };
        // Anomalías de evidencia.
        let b = v.primary == Check::Yes;
        let f = v.structures == Check::Yes;
        let e = v.entry == Check::Yes;
        let compensated = b && f && e;
        match v.primary {
            Check::No if v.backup == Check::Yes => v.anomalies.insert(
                0,
                VolumeAnomaly::PrimaryDamaged {
                    byte: v.primary_byte,
                },
            ),
            Check::Outside => v.anomalies.insert(
                0,
                VolumeAnomaly::PrimaryOutsideImage {
                    byte: v.primary_byte,
                },
            ),
            _ => {}
        }
        let backup_byte = v.backup_byte.unwrap_or(v.end_byte());
        match v.backup {
            Check::No
                if !v
                    .anomalies
                    .iter()
                    .any(|a| matches!(a, VolumeAnomaly::BackupMismatch { .. })) =>
            {
                v.anomalies.push(VolumeAnomaly::BackupMissing {
                    byte: backup_byte,
                    compensated,
                })
            }
            Check::Outside => v.anomalies.push(VolumeAnomaly::BackupOutsideImage {
                byte: backup_byte,
                compensated,
            }),
            _ => {}
        }
        for a in &mut v.anomalies {
            if let VolumeAnomaly::BackupUnreadable { compensated: c, .. } = a {
                *c = compensated;
            }
        }
        let end = i128::from(v.end_byte());
        if end > i128::from(size) {
            v.anomalies.push(VolumeAnomaly::BeyondImageEnd {
                missing_bytes: u64::try_from(end - i128::from(size)).unwrap_or(u64::MAX),
            });
        }
        if u64::from(v.fs_sector_size) != ss {
            v.anomalies.push(VolumeAnomaly::SectorSizeMismatch {
                fs: v.fs_sector_size,
                device: ss as u32,
            });
        }
        if start >= 0 && start % i128::from(ss) != 0 {
            v.anomalies
                .push(VolumeAnomaly::Unaligned { byte: v.start_byte });
        }
        let base_valid = (b && v.backup == Check::Yes)
            || (b && v.backup == Check::NotApplicable && f)
            || (b && f && e);
        if !base_valid
            && !v
                .anomalies
                .iter()
                .any(|a| a.severity() >= Severity::Suspicious)
        {
            v.anomalies.push(VolumeAnomaly::Unconfirmed);
        }
        v.confidence = volume_confidence(v);
        v.score = [
            (b, 4),
            (v.backup == Check::Yes, 3),
            (f, 2),
            (e, 2),
            (v.self_location == Check::Yes, 1),
            (v.aligned == Check::Yes, 1),
        ]
        .iter()
        .filter(|(c, _)| *c)
        .map(|(_, w)| w)
        .sum();
    }
}

/// Válido provisional durante la búsqueda (para descartar candidatos de rejilla interiores).
fn provisional_valid(v: &FoundVolume) -> bool {
    v.primary == Check::Yes
        && (v.backup == Check::Yes
            || (v.backup == Check::NotApplicable && v.structures == Check::Yes))
}

/// Confianza de un volumen (ADR-003): válido si no tiene ninguna anomalía sospechosa; si no,
/// sospechoso con sus motivos. Los volúmenes sin principal ni copia válidas no se informan.
fn volume_confidence(v: &FoundVolume) -> Confidence {
    let reasons: Vec<String> = v
        .anomalies
        .iter()
        .filter(|a| a.severity() >= Severity::Suspicious)
        .map(ToString::to_string)
        .collect();
    if reasons.is_empty() {
        Confidence::Valid
    } else {
        Confidence::Suspicious(reasons)
    }
}
