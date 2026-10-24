//! MBR y cadenas EBR (issue #12).
//!
//! Disposición clásica (pública): tabla de 4 entradas de 16 bytes en 0x1BE y firma `55 AA` en 0x1FE.
//! Cada entrada: indicador de arranque (0x00 o 0x80), CHS inicial, tipo, CHS final, LBA inicial (u32)
//! y número de sectores (u32). Las particiones lógicas se encadenan con EBR: en cada EBR la entrada de
//! datos es relativa al **propio EBR** y la de enlace (tipo extendido) al **inicio de la extendida**;
//! las entradas del MBR son relativas a la posición de la tabla. Las CHS no se usan.
//!
//! En lugar de aceptar o rechazar, cada partición lleva su [`Confidence`] y las anomalías tipadas
//! que la justifican ([`MbrAnomaly`]). Reglas (INV-05 §2.3 como hechos observados y §5 como diseño
//! propio):
//! - En la LBA 0 la tabla se lee aunque falte la firma, pero entonces se exige que sea plausible:
//!   alguna entrada que empiece dentro del disco, indicadores de arranque válidos en las entradas
//!   usadas y como mucho una extendida. Si no, no hay tabla ([`MbrAnomaly::NoTable`]).
//! - Fuera de la LBA 0 (EBR o tabla desplazada) sin firma solo se acepta una tabla plausible y se
//!   anota la falta de firma.
//! - `Invalid`: indicador no válido en una entrada usada, varias extendidas en la misma tabla,
//!   entrada que empieza fuera del disco. `Suspicious`: varias activas, tipo desconocido, entrada
//!   que termina fuera del disco o solapada, firma ausente, problemas de la cadena de EBR.
//!
//! Todas las lecturas pasan por [`read_trusted`]: un sector ilegible produce
//! [`Anomaly::UnreadableSector`] y nunca se interpreta. Aritmética comprobada o saturada (un valor
//! imposible queda fuera del disco y se informa), límites explícitos ([`MAX_EBR_CHAIN`]) y salida
//! acotada: ningún dato del origen puede provocar un pánico, un bucle ni listas desmesuradas.

use crate::sector::read_trusted;
use crate::{Anomaly, Confidence, Origin, Partition, PartitionKind, Scheme, Severity};
use mr_io::BlockDevice;
use std::collections::HashSet;
use std::fmt;

/// Desplazamiento de la tabla de entradas dentro del sector.
pub const TABLE_OFFSET: usize = 0x1BE;
/// Tamaño de cada entrada.
pub const ENTRY_SIZE: usize = 16;
/// Desplazamiento de la firma `55 AA`.
pub const SIGNATURE_OFFSET: usize = 0x1FE;
/// Tipo de la entrada del MBR protector de GPT (UEFI).
pub const TYPE_GPT_PROTECTIVE: u8 = 0xEE;
/// Máximo de EBR que se recorren en una cadena (límite explícito contra cadenas maliciosas).
pub const MAX_EBR_CHAIN: usize = 1024;
/// Reintentos por sector en las lecturas de tablas.
pub const READ_RETRIES: u32 = 1;

/// `true` si el tipo describe una partición extendida (contenedor de EBR): 0x05 (CHS), 0x0F (LBA) y
/// 0x85 (Linux). INV-05 §4: 0x85 se acepta también como extendida y como enlace.
pub fn is_extended(kind: u8) -> bool {
    matches!(kind, 0x05 | 0x0F | 0x85)
}

/// Nombre legible de un tipo de partición MBR, o `None` si no figura en la lista. Lista propia
/// elaborada a partir de identificadores documentados públicamente; los tipos que no figuran se
/// informan como desconocidos ([`MbrAnomaly::UnknownType`]).
pub fn type_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0x01 => "FAT12",
        0x04 => "FAT16 (< 32 MiB)",
        0x05 => "Extendida (CHS)",
        0x06 => "FAT16",
        0x07 => "NTFS / exFAT / HPFS",
        0x0A => "Gestor de arranque de OS/2",
        0x0B => "FAT32 (CHS)",
        0x0C => "FAT32 (LBA)",
        0x0E => "FAT16 (LBA)",
        0x0F => "Extendida (LBA)",
        0x11 => "FAT12 oculta",
        0x12 => "Utilidades o recuperación del fabricante",
        0x14 => "FAT16 oculta (< 32 MiB)",
        0x16 => "FAT16 oculta",
        0x17 => "NTFS / exFAT oculta",
        0x1B => "FAT32 oculta (CHS)",
        0x1C => "FAT32 oculta (LBA)",
        0x1E => "FAT16 oculta (LBA)",
        0x27 => "Entorno de recuperación de Windows",
        0x39 => "Plan 9",
        0x42 => "Disco dinámico de Windows (LDM)",
        0x82 => "Intercambio de Linux / Solaris",
        0x83 => "Linux",
        0x84 => "Hibernación",
        0x85 => "Extendida de Linux",
        0x86 => "Conjunto de volúmenes FAT16",
        0x87 => "Conjunto de volúmenes NTFS",
        0x88 => "Tabla de particiones en texto de Linux",
        0x8E => "Linux LVM",
        0x9F => "BSD/OS",
        0xA0 => "Hibernación (portátiles)",
        0xA5 => "FreeBSD",
        0xA6 => "OpenBSD",
        0xA8 => "UFS de macOS",
        0xA9 => "NetBSD",
        0xAB => "Arranque de macOS",
        0xAF => "HFS / HFS+",
        0xB7 => "BSDI",
        0xB8 => "Intercambio de BSDI",
        0xBE => "Arranque de Solaris",
        0xBF => "Solaris",
        0xC1 => "FAT12 protegida (DR-DOS)",
        0xC4 => "FAT16 protegida (DR-DOS)",
        0xC6 => "FAT16 protegida (DR-DOS)",
        0xDA => "Datos sin sistema de archivos",
        0xDE => "Utilidades de Dell",
        0xEB => "BeOS BFS",
        0xEE => "GPT (MBR protector)",
        0xEF => "Sistema EFI (FAT)",
        0xFB => "VMware VMFS",
        0xFC => "Intercambio de VMware",
        0xFD => "RAID de Linux",
        _ => return None,
    })
}

/// Por qué un sector no se interpreta como tabla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoTableReason {
    /// Sin entradas usadas ni firma (p. ej. sector a cero).
    Blank,
    /// Ninguna entrada usada empieza dentro del disco.
    NoEntryInsideDisk,
    /// Una entrada usada tiene un indicador de arranque distinto de 0x00 y 0x80.
    InvalidBootIndicator,
    /// Más de una entrada de tipo extendido.
    MultipleExtended,
    /// El sector es una cabecera GPT (`EFI PART`): imagen probablemente desplazada.
    GptHeader,
    /// La tabla pedida está fuera del origen o el sector es demasiado corto.
    OutsideDevice,
}

impl NoTableReason {
    fn text(self) -> &'static str {
        match self {
            Self::Blank => "sin entradas ni firma",
            Self::NoEntryInsideDisk => "ninguna entrada empieza dentro del disco",
            Self::InvalidBootIndicator => "indicador de arranque no válido en una entrada usada",
            Self::MultipleExtended => "más de una entrada extendida",
            Self::GptHeader => "el sector es una cabecera GPT (¿imagen desplazada?)",
            Self::OutsideDevice => "fuera del origen",
        }
    }
}

/// Anomalías de MBR/EBR. `lba` es siempre la del sector de tabla (MBR o EBR) donde se detectó.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MbrAnomaly {
    /// El sector principal no contiene una tabla reconocible.
    NoTable { lba: u64, reason: NoTableReason },
    /// Un EBR de la cadena no es una tabla utilizable; la cadena termina ahí.
    EbrInvalid { lba: u64, reason: NoTableReason },
    /// Firma presente sin ninguna entrada usada.
    Empty { lba: u64 },
    /// Tabla leída sin la firma `55 AA`.
    SignatureMissing { lba: u64 },
    /// El sector es el de arranque de un volumen (disco sin particionar).
    Superfloppy { lba: u64, fs: &'static str },
    /// Indicador de arranque distinto de 0x00 y 0x80 (`used`: la entrada tiene tipo).
    InvalidBootIndicator {
        lba: u64,
        slot: u8,
        value: u8,
        used: bool,
    },
    /// Más de una entrada activa (0x80) en el MBR.
    MultipleActive { lba: u64, slots: Vec<u8> },
    /// Más de una entrada extendida (o de enlace) en la misma tabla.
    MultipleExtended { lba: u64, slots: Vec<u8> },
    /// Entrada con tipo y tamaño 0 (no se lista como partición).
    ZeroLength { lba: u64, slot: u8, kind: u8 },
    /// La entrada empieza fuera del disco.
    OutsideDisk {
        lba: u64,
        origin: Origin,
        start: u64,
    },
    /// La entrada empieza dentro y termina fuera del disco (`end` exclusivo).
    PartiallyOutsideDisk {
        lba: u64,
        origin: Origin,
        end: u64,
        disk_sectors: u64,
    },
    /// Dos entradas se solapan.
    Overlap {
        lba: u64,
        first: Origin,
        second: Origin,
    },
    /// Tipo que no figura en la lista de tipos conocidos.
    UnknownType { lba: u64, origin: Origin, kind: u8 },
    /// Partición lógica que no cabe en su partición extendida.
    LogicalOutsideExtended { lba: u64, origin: Origin },
    /// EBR con más de una entrada de datos (todas se listan).
    ExtraDataEntries { lba: u64, count: u8 },
    /// El enlace del EBR vuelve a un EBR ya visitado (`target`).
    EbrLoop { lba: u64, target: u64 },
    /// El enlace del EBR apunta fuera del disco.
    LinkOutsideDisk { lba: u64, target: u64 },
    /// El enlace del EBR apunta fuera de la partición extendida.
    LinkOutsideExtended { lba: u64, target: u64 },
    /// Se alcanzó [`MAX_EBR_CHAIN`] EBR; el resto de la cadena no se recorre.
    ChainLimit { lba: u64, limit: usize },
    /// MBR protector cuya entrada 0xEE no empieza en la LBA 1.
    ProtectiveStart { lba: u64, start: u64 },
    /// MBR protector cuyo tamaño no cubre el disco (ni es 0xFFFFFFFF).
    ProtectiveSizeMismatch {
        lba: u64,
        declared: u64,
        expected: u64,
    },
    /// MBR híbrido: entrada 0xEE junto a entradas que describen particiones.
    Hybrid { lba: u64 },
    /// Entrada `slot` de un MBR híbrido que no coincide (inicio y tamaño) con ninguna partición GPT.
    HybridMismatch { lba: u64, slot: u8 },
}

impl MbrAnomaly {
    /// Sector de tabla donde se detectó.
    pub fn lba(&self) -> u64 {
        match *self {
            Self::NoTable { lba, .. }
            | Self::EbrInvalid { lba, .. }
            | Self::Empty { lba }
            | Self::SignatureMissing { lba }
            | Self::Superfloppy { lba, .. }
            | Self::InvalidBootIndicator { lba, .. }
            | Self::MultipleActive { lba, .. }
            | Self::MultipleExtended { lba, .. }
            | Self::ZeroLength { lba, .. }
            | Self::OutsideDisk { lba, .. }
            | Self::PartiallyOutsideDisk { lba, .. }
            | Self::Overlap { lba, .. }
            | Self::UnknownType { lba, .. }
            | Self::LogicalOutsideExtended { lba, .. }
            | Self::ExtraDataEntries { lba, .. }
            | Self::EbrLoop { lba, .. }
            | Self::LinkOutsideDisk { lba, .. }
            | Self::LinkOutsideExtended { lba, .. }
            | Self::ChainLimit { lba, .. }
            | Self::ProtectiveStart { lba, .. }
            | Self::ProtectiveSizeMismatch { lba, .. }
            | Self::Hybrid { lba }
            | Self::HybridMismatch { lba, .. } => lba,
        }
    }

    /// Código estable (JSON de la CLI). Coincide con el del manifiesto del corpus cuando existe.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoTable { .. } => "mbr_missing",
            Self::EbrInvalid { .. } => "ebr_invalid",
            Self::Empty { .. } => "mbr_empty",
            Self::SignatureMissing { .. } => "mbr_signature_missing",
            Self::Superfloppy { .. } => "superfloppy",
            Self::InvalidBootIndicator { .. } => "mbr_invalid_boot_indicator",
            Self::MultipleActive { .. } => "mbr_multiple_active",
            Self::MultipleExtended { .. } => "mbr_multiple_extended",
            Self::ZeroLength { .. } => "mbr_zero_length",
            Self::OutsideDisk { .. } => "mbr_outside_disk",
            Self::PartiallyOutsideDisk { .. } => "mbr_partially_outside_disk",
            Self::Overlap { .. } => "mbr_overlap",
            Self::UnknownType { .. } => "mbr_unknown_type",
            Self::LogicalOutsideExtended { .. } => "ebr_logical_outside_extended",
            Self::ExtraDataEntries { .. } => "ebr_extra_entries",
            Self::EbrLoop { .. } => "ebr_loop",
            Self::LinkOutsideDisk { .. } => "ebr_link_outside_disk",
            Self::LinkOutsideExtended { .. } => "ebr_link_outside_extended",
            Self::ChainLimit { .. } => "ebr_chain_limit",
            Self::ProtectiveStart { .. } => "protective_mbr_start",
            Self::ProtectiveSizeMismatch { .. } => "protective_mbr_size_mismatch",
            Self::Hybrid { .. } => "hybrid_mbr",
            Self::HybridMismatch { .. } => "hybrid_mbr_mismatch",
        }
    }

    /// Gravedad: nivel de confianza que deja a la partición o tabla afectada. La ausencia de tabla
    /// es informativa (no hay nada que desconfiar); un EBR inutilizable, en cambio, rompe una
    /// cadena declarada.
    pub fn severity(&self) -> Severity {
        match self {
            Self::NoTable { .. }
            | Self::Empty { .. }
            | Self::Superfloppy { .. }
            | Self::Hybrid { .. }
            | Self::InvalidBootIndicator { used: false, .. } => Severity::Notice,
            Self::InvalidBootIndicator { used: true, .. }
            | Self::MultipleExtended { .. }
            | Self::OutsideDisk { .. } => Severity::Invalid,
            _ => Severity::Suspicious,
        }
    }

    /// LBA de destino (enlaces y bucles de EBR).
    pub fn target(&self) -> Option<u64> {
        match *self {
            Self::EbrLoop { target, .. }
            | Self::LinkOutsideDisk { target, .. }
            | Self::LinkOutsideExtended { target, .. } => Some(target),
            _ => None,
        }
    }

    /// Valor asociado: tipo, tamaño declarado del MBR protector, indicador, LBA fuera del disco...
    pub fn value(&self) -> Option<u64> {
        match *self {
            Self::UnknownType { kind, .. } | Self::ZeroLength { kind, .. } => Some(u64::from(kind)),
            Self::ProtectiveSizeMismatch { declared, .. } => Some(declared),
            Self::InvalidBootIndicator { value, .. } => Some(u64::from(value)),
            Self::ProtectiveStart { start, .. } | Self::OutsideDisk { start, .. } => Some(start),
            Self::PartiallyOutsideDisk { end, .. } => Some(end),
            Self::ChainLimit { limit, .. } => Some(limit as u64),
            _ => None,
        }
    }

    /// Particiones (por su origen) a las que se refiere la anomalía.
    pub fn origins(&self) -> Vec<Origin> {
        match self {
            Self::OutsideDisk { origin, .. }
            | Self::PartiallyOutsideDisk { origin, .. }
            | Self::UnknownType { origin, .. }
            | Self::LogicalOutsideExtended { origin, .. } => vec![*origin],
            Self::Overlap { first, second, .. } => vec![*first, *second],
            Self::MultipleActive { slots, .. } => {
                slots.iter().map(|&slot| Origin::Primary { slot }).collect()
            }
            _ => Vec::new(),
        }
    }
}

impl fmt::Display for MbrAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lba = self.lba();
        match self {
            Self::NoTable { reason, .. } => write!(
                f,
                "no hay tabla de particiones reconocible en la LBA {lba}: {}",
                reason.text()
            ),
            Self::EbrInvalid { reason, .. } => write!(
                f,
                "el EBR de la LBA {lba} no es una tabla utilizable: {}",
                reason.text()
            ),
            Self::Empty { .. } => write!(f, "tabla en la LBA {lba} con firma pero sin entradas"),
            Self::SignatureMissing { .. } => {
                write!(f, "tabla en la LBA {lba} sin la firma 55 AA")
            }
            Self::Superfloppy { fs, .. } => write!(
                f,
                "la LBA {lba} es el sector de arranque de un volumen {fs} (disco sin particionar)"
            ),
            Self::InvalidBootIndicator {
                slot, value, used, ..
            } => write!(
                f,
                "entrada {} de la tabla de la LBA {lba}: indicador de arranque 0x{value:02X} no válido{}",
                u32::from(*slot) + 1,
                if *used { "" } else { " (entrada vacía)" }
            ),
            Self::MultipleActive { slots, .. } => write!(
                f,
                "tabla de la LBA {lba}: {} entradas activas ({})",
                slots.len(),
                slot_list(slots)
            ),
            Self::MultipleExtended { slots, .. } => write!(
                f,
                "tabla de la LBA {lba}: {} entradas extendidas o de enlace ({})",
                slots.len(),
                slot_list(slots)
            ),
            Self::ZeroLength { slot, kind, .. } => write!(
                f,
                "entrada {} de la tabla de la LBA {lba}: tipo 0x{kind:02X} con tamaño 0",
                u32::from(*slot) + 1
            ),
            Self::OutsideDisk { origin, start, .. } => write!(
                f,
                "{} empieza en la LBA {start}, fuera del disco",
                origin_text(origin)
            ),
            Self::PartiallyOutsideDisk {
                origin,
                end,
                disk_sectors,
                ..
            } => write!(
                f,
                "{} termina en la LBA {}, fuera del disco ({disk_sectors} sectores)",
                origin_text(origin),
                end.saturating_sub(1)
            ),
            Self::Overlap { first, second, .. } => write!(
                f,
                "{} se solapa con {}",
                origin_text(first),
                origin_text(second)
            ),
            Self::UnknownType { origin, kind, .. } => {
                write!(f, "{}: tipo 0x{kind:02X} desconocido", origin_text(origin))
            }
            Self::LogicalOutsideExtended { origin, .. } => write!(
                f,
                "{} no cabe en su partición extendida",
                origin_text(origin)
            ),
            Self::ExtraDataEntries { count, .. } => {
                write!(f, "el EBR de la LBA {lba} tiene {count} entradas de datos")
            }
            Self::EbrLoop { target, .. } => write!(
                f,
                "el enlace del EBR de la LBA {lba} vuelve al EBR de la LBA {target} (bucle)"
            ),
            Self::LinkOutsideDisk { target, .. } => write!(
                f,
                "el enlace del EBR de la LBA {lba} apunta a la LBA {target}, fuera del disco"
            ),
            Self::LinkOutsideExtended { target, .. } => write!(
                f,
                "el enlace del EBR de la LBA {lba} apunta a la LBA {target}, fuera de la extendida"
            ),
            Self::ChainLimit { limit, .. } => write!(
                f,
                "cadena de EBR interrumpida en la LBA {lba}: límite de {limit} EBR"
            ),
            Self::ProtectiveStart { start, .. } => write!(
                f,
                "MBR protector: la entrada 0xEE empieza en la LBA {start} en lugar de la 1"
            ),
            Self::ProtectiveSizeMismatch {
                declared, expected, ..
            } => write!(
                f,
                "MBR protector: declara {declared} sectores y el disco tiene {expected} tras la tabla"
            ),
            Self::Hybrid { .. } => write!(
                f,
                "MBR híbrido: la entrada 0xEE convive con entradas que describen particiones"
            ),
            Self::HybridMismatch { slot, .. } => write!(
                f,
                "MBR híbrido: la entrada {} no coincide con ninguna partición GPT",
                u32::from(*slot) + 1
            ),
        }
    }
}

fn slot_list(slots: &[u8]) -> String {
    slots
        .iter()
        .map(|&s| (u32::from(s) + 1).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn origin_text(o: &Origin) -> String {
    match *o {
        Origin::Primary { slot } => format!("la entrada {} del MBR", u32::from(slot) + 1),
        Origin::Logical { ebr_lba, slot } => {
            format!(
                "la entrada {} del EBR de la LBA {ebr_lba}",
                u32::from(slot) + 1
            )
        }
        Origin::Gpt { index } => format!("la entrada GPT {}", u64::from(index) + 1),
        Origin::Apm { index } => format!("la entrada APM {index}"),
    }
}

/// Entrada tal como está en el disco (sin CHS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawEntry {
    pub boot: u8,
    pub kind: u8,
    pub start: u32,
    pub sectors: u32,
}

impl RawEntry {
    /// Una entrada con tipo 0 está libre (convención universal).
    pub fn is_used(&self) -> bool {
        self.kind != 0
    }

    /// Indicador 0x00 o 0x80.
    pub fn boot_valid(&self) -> bool {
        self.boot & 0x7F == 0
    }
}

/// Las 4 entradas y la firma de un sector de tabla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawTable {
    pub entries: [RawEntry; 4],
    pub signature: bool,
}

/// Lee las entradas y la firma de un sector de al menos 512 bytes, sin validar nada. `None` si es
/// más corto.
pub fn parse_raw(sector: &[u8]) -> Option<RawTable> {
    let sig = sector.get(SIGNATURE_OFFSET..SIGNATURE_OFFSET + 2)?;
    let mut entries = [RawEntry::default(); 4];
    for (i, e) in entries.iter_mut().enumerate() {
        let at = TABLE_OFFSET + i * ENTRY_SIZE;
        let b = sector.get(at..at + ENTRY_SIZE)?;
        *e = RawEntry {
            boot: b[0],
            kind: b[4],
            start: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            sectors: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        };
    }
    Some(RawTable {
        entries,
        signature: sig == [0x55, 0xAA],
    })
}

/// Sistema de archivos cuyo sector de arranque es `sector`, si se reconoce: salto x86 al inicio e
/// identificador en la posición que fijan las especificaciones públicas de NTFS, exFAT y FAT.
pub fn volume_boot_kind(sector: &[u8]) -> Option<&'static str> {
    let jump = sector.first() == Some(&0xE9)
        || (sector.first() == Some(&0xEB) && sector.get(2) == Some(&0x90));
    if !jump {
        return None;
    }
    let at = |o: usize, s: &[u8]| sector.get(o..o + s.len()) == Some(s);
    if at(3, b"NTFS    ") {
        Some("NTFS")
    } else if at(3, b"EXFAT   ") {
        Some("exFAT")
    } else if at(0x52, b"FAT32   ") {
        Some("FAT32")
    } else if at(0x36, b"FAT12   ") || at(0x36, b"FAT16   ") || at(0x36, b"FAT     ") {
        Some("FAT")
    } else {
        None
    }
}

/// Resultado de leer un MBR (y su cadena de EBR).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbrScan {
    /// `None` (sin tabla), `Mbr`, `Gpt` (MBR protector) o `Hybrid`.
    pub scheme: Scheme,
    /// LBA de la tabla leída.
    pub table_lba: u64,
    /// Primarias en orden de entrada y después lógicas en orden de cadena. En un híbrido, las
    /// entradas distintas de 0xEE; en un MBR protector, ninguna.
    pub partitions: Vec<Partition>,
    /// Anomalías no atribuibles a una sola partición (las de cada partición van en ella).
    pub anomalies: Vec<Anomaly>,
    /// Partición extendida recorrida: `(LBA inicial, sectores, tipo)`.
    pub extended: Option<(u64, u64, u8)>,
    /// EBR leídos, en orden de cadena y cada uno una vez.
    pub ebr_lbas: Vec<u64>,
}

/// Lee el MBR de la LBA 0 y, si hay una extendida, su cadena de EBR.
pub fn read_mbr(dev: &(impl BlockDevice + ?Sized)) -> mr_io::Result<MbrScan> {
    read_mbr_at(dev, 0)
}

/// Lee una tabla MBR situada en `table_lba` (p. ej. un MBR desplazado en una imagen con sectores
/// añadidos al inicio). Las entradas primarias son relativas a `table_lba` y se devuelven en LBA
/// del origen.
pub fn read_mbr_at(dev: &(impl BlockDevice + ?Sized), table_lba: u64) -> mr_io::Result<MbrScan> {
    let mut w = Walker {
        dev,
        total: dev.sector_count(),
        table_lba,
        partitions: Vec::new(),
        anomalies: Vec::new(),
        extended: None,
        ebr_lbas: Vec::new(),
    };
    let scheme = if table_lba >= w.total {
        w.no_table(NoTableReason::OutsideDevice)
    } else {
        match trusted_sector(dev, table_lba)? {
            Some(sector) => w.primary_table(&sector)?,
            None => {
                w.anomalies
                    .push(Anomaly::UnreadableSector { lba: table_lba });
                Scheme::None
            }
        }
    };
    w.check_overlaps();
    let mut scan = MbrScan {
        scheme,
        table_lba,
        partitions: w.partitions,
        anomalies: w.anomalies,
        extended: w.extended,
        ebr_lbas: w.ebr_lbas,
    };
    assign_confidence(&mut scan);
    Ok(scan)
}

/// Contenido de un sector si es fiable; `None` si es ilegible o se saltó (nunca su relleno).
fn trusted_sector(dev: &(impl BlockDevice + ?Sized), lba: u64) -> mr_io::Result<Option<Vec<u8>>> {
    let t = read_trusted(dev, lba, 1, READ_RETRIES)?;
    Ok(t.sector(lba).map(<[u8]>::to_vec))
}

/// Entradas usadas de una tabla con su posición.
fn used_entries(raw: &RawTable) -> Vec<(u8, RawEntry)> {
    (0u8..)
        .zip(raw.entries)
        .filter(|(_, e)| e.is_used())
        .collect()
}

/// LBA absoluta de una entrada relativa a `base` (saturada: un valor imposible queda fuera del
/// disco y se informa como tal).
fn start_of(base: u64, rel: u32) -> u64 {
    base.saturating_add(u64::from(rel))
}

/// Estado del recorrido de una tabla y su cadena.
struct Walker<'a, D: BlockDevice + ?Sized> {
    dev: &'a D,
    total: u64,
    table_lba: u64,
    partitions: Vec<Partition>,
    anomalies: Vec<Anomaly>,
    extended: Option<(u64, u64, u8)>,
    ebr_lbas: Vec<u64>,
}

impl<D: BlockDevice + ?Sized> Walker<'_, D> {
    fn push(&mut self, a: MbrAnomaly) {
        self.anomalies.push(Anomaly::Mbr(a));
    }

    fn no_table(&mut self, reason: NoTableReason) -> Scheme {
        let lba = self.table_lba;
        self.push(MbrAnomaly::NoTable { lba, reason });
        Scheme::None
    }

    /// Interpreta el sector principal y decide el esquema.
    fn primary_table(&mut self, sector: &[u8]) -> mr_io::Result<Scheme> {
        let lba = self.table_lba;
        if let Some(fs) = volume_boot_kind(sector) {
            self.push(MbrAnomaly::Superfloppy { lba, fs });
            return Ok(Scheme::None);
        }
        if sector.starts_with(b"EFI PART") {
            return Ok(self.no_table(NoTableReason::GptHeader));
        }
        let Some(raw) = parse_raw(sector) else {
            return Ok(self.no_table(NoTableReason::OutsideDevice));
        };
        let used = used_entries(&raw);
        if used.is_empty() {
            if raw.signature {
                self.push(MbrAnomaly::Empty { lba });
                return Ok(Scheme::None);
            }
            return Ok(self.no_table(NoTableReason::Blank));
        }
        let bad_boot = used.iter().any(|(_, e)| !e.boot_valid());
        let ext_slots: Vec<u8> = used
            .iter()
            .filter(|(_, e)| is_extended(e.kind))
            .map(|&(s, _)| s)
            .collect();
        let inside = used
            .iter()
            .any(|(_, e)| start_of(lba, e.start) < self.total);
        if !raw.signature {
            // Sin firma solo se acepta una tabla plausible (INV-05 §2.3, reglas 1 a 6).
            if bad_boot {
                return Ok(self.no_table(NoTableReason::InvalidBootIndicator));
            }
            if ext_slots.len() > 1 {
                return Ok(self.no_table(NoTableReason::MultipleExtended));
            }
            if !inside {
                return Ok(self.no_table(NoTableReason::NoEntryInsideDisk));
            }
        } else if !inside && lba != 0 {
            return Ok(self.no_table(NoTableReason::NoEntryInsideDisk));
        }

        let protective: Vec<(u8, RawEntry)> = used
            .iter()
            .copied()
            .filter(|(_, e)| e.kind == TYPE_GPT_PROTECTIVE)
            .collect();
        let scheme = if protective.is_empty() {
            Scheme::Mbr
        } else if protective.len() == used.len() {
            Scheme::Gpt
        } else {
            Scheme::Hybrid
        };
        if !raw.signature {
            self.push(MbrAnomaly::SignatureMissing { lba });
        }
        // Indicadores en entradas libres: solo se anotan (INV-05 §2.3, regla 1).
        for (slot, e) in (0u8..).zip(raw.entries) {
            if !e.is_used() && e.boot != 0 {
                self.push(MbrAnomaly::InvalidBootIndicator {
                    lba,
                    slot,
                    value: e.boot,
                    used: false,
                });
            }
        }
        if scheme == Scheme::Gpt {
            self.protective_checks(protective[0].1);
            return Ok(scheme);
        }
        if scheme == Scheme::Hybrid {
            self.push(MbrAnomaly::Hybrid { lba });
        }
        let active: Vec<u8> = used
            .iter()
            .filter(|(_, e)| e.boot == 0x80 && e.kind != TYPE_GPT_PROTECTIVE)
            .map(|&(s, _)| s)
            .collect();
        if active.len() > 1 {
            self.push(MbrAnomaly::MultipleActive { lba, slots: active });
        }
        if ext_slots.len() > 1 {
            self.push(MbrAnomaly::MultipleExtended {
                lba,
                slots: ext_slots,
            });
        }
        let mut container: Option<(u8, u64, u64, u8)> = None;
        for &(slot, e) in &used {
            if e.kind == TYPE_GPT_PROTECTIVE {
                continue;
            }
            let origin = Origin::Primary { slot };
            if !is_extended(e.kind) {
                self.add_entry(lba, origin, e, None);
                continue;
            }
            if !e.boot_valid() {
                self.push(MbrAnomaly::InvalidBootIndicator {
                    lba,
                    slot,
                    value: e.boot,
                    used: true,
                });
            }
            if e.sectors == 0 {
                self.push(MbrAnomaly::ZeroLength {
                    lba,
                    slot,
                    kind: e.kind,
                });
                continue;
            }
            let start = start_of(lba, e.start);
            let sectors = u64::from(e.sectors);
            let mut out = Vec::new();
            self.bounds(lba, origin, start, sectors, &mut out);
            self.anomalies.extend(out);
            // Solo se recorre la primera extendida (con varias, la tabla ya es inválida).
            if container.is_none() {
                container = Some((slot, start, sectors, e.kind));
            }
        }
        if let Some((slot, start, sectors, kind)) = container {
            self.extended = Some((start, sectors, kind));
            self.container_overlaps(slot, start, sectors);
            if start < self.total {
                self.walk_chain(start, sectors)?;
            }
        }
        Ok(scheme)
    }

    /// Comprobaciones del MBR protector (UEFI): entrada 0xEE desde la LBA 1 que cubre el disco
    /// (o 0xFFFFFFFF si no cabe en 32 bits).
    fn protective_checks(&mut self, e: RawEntry) {
        let lba = self.table_lba;
        let start = u64::from(e.start);
        if start != 1 {
            self.push(MbrAnomaly::ProtectiveStart { lba, start });
        }
        let expected = self
            .total
            .saturating_sub(lba)
            .saturating_sub(1)
            .min(u64::from(u32::MAX));
        let declared = u64::from(e.sectors);
        if declared != expected && declared != u64::from(u32::MAX) {
            self.push(MbrAnomaly::ProtectiveSizeMismatch {
                lba,
                declared,
                expected,
            });
        }
    }

    /// Límites del disco de una entrada; las anomalías van a `out`.
    fn bounds(&self, lba: u64, origin: Origin, start: u64, sectors: u64, out: &mut Vec<Anomaly>) {
        if start >= self.total {
            out.push(Anomaly::Mbr(MbrAnomaly::OutsideDisk { lba, origin, start }));
            return;
        }
        let end = start.saturating_add(sectors);
        if end > self.total {
            out.push(Anomaly::Mbr(MbrAnomaly::PartiallyOutsideDisk {
                lba,
                origin,
                end,
                disk_sectors: self.total,
            }));
        }
    }

    /// Añade una partición de datos de la tabla `lba` (relativa a `lba`). `extended`: límites de
    /// la extendida, para las lógicas.
    fn add_entry(&mut self, lba: u64, origin: Origin, e: RawEntry, extended: Option<(u64, u64)>) {
        let slot = match origin {
            Origin::Primary { slot } | Origin::Logical { slot, .. } => slot,
            Origin::Gpt { .. } | Origin::Apm { .. } => 0,
        };
        if e.sectors == 0 {
            self.push(MbrAnomaly::ZeroLength {
                lba,
                slot,
                kind: e.kind,
            });
            return;
        }
        let start = start_of(lba, e.start);
        let sectors = u64::from(e.sectors);
        let mut own = Vec::new();
        if !e.boot_valid() {
            own.push(Anomaly::Mbr(MbrAnomaly::InvalidBootIndicator {
                lba,
                slot,
                value: e.boot,
                used: true,
            }));
        }
        self.bounds(lba, origin, start, sectors, &mut own);
        if type_name(e.kind).is_none() {
            own.push(Anomaly::Mbr(MbrAnomaly::UnknownType {
                lba,
                origin,
                kind: e.kind,
            }));
        }
        if let Some((ext_start, ext_sectors)) = extended {
            let ext_end = ext_start.saturating_add(ext_sectors);
            if start < ext_start || start.saturating_add(sectors) > ext_end {
                own.push(Anomaly::Mbr(MbrAnomaly::LogicalOutsideExtended {
                    lba,
                    origin,
                }));
            }
        }
        self.partitions.push(Partition {
            origin,
            start_lba: start,
            sector_count: sectors,
            kind: PartitionKind::Mbr(e.kind),
            name: None,
            active: e.boot == 0x80,
            confidence: Confidence::Valid,
            anomalies: own,
        });
    }

    /// La extendida no debe solaparse con las primarias.
    fn container_overlaps(&mut self, slot: u8, start: u64, sectors: u64) {
        let end = start.saturating_add(sectors);
        let lba = self.table_lba;
        let hits: Vec<Origin> = self
            .partitions
            .iter()
            .filter(|p| p.start_lba < end && start < p.start_lba.saturating_add(p.sector_count))
            .map(|p| p.origin)
            .collect();
        for o in hits {
            self.push(MbrAnomaly::Overlap {
                lba,
                first: o,
                second: Origin::Primary { slot },
            });
        }
    }

    /// Recorre la cadena de EBR de la extendida `[ext_start, ext_start + ext_sectors)`.
    fn walk_chain(&mut self, ext_start: u64, ext_sectors: u64) -> mr_io::Result<()> {
        let ext_end = ext_start.saturating_add(ext_sectors);
        let mut visited = HashSet::new();
        let mut current = ext_start;
        loop {
            if self.ebr_lbas.len() >= MAX_EBR_CHAIN {
                self.push(MbrAnomaly::ChainLimit {
                    lba: current,
                    limit: MAX_EBR_CHAIN,
                });
                return Ok(());
            }
            visited.insert(current);
            self.ebr_lbas.push(current);
            let Some(sector) = trusted_sector(self.dev, current)? else {
                self.anomalies
                    .push(Anomaly::UnreadableSector { lba: current });
                return Ok(());
            };
            let Some(next) = self.ebr(current, &sector, ext_start, ext_sectors) else {
                return Ok(());
            };
            let anomaly = if visited.contains(&next) {
                MbrAnomaly::EbrLoop {
                    lba: current,
                    target: next,
                }
            } else if next >= self.total {
                MbrAnomaly::LinkOutsideDisk {
                    lba: current,
                    target: next,
                }
            } else if next < ext_start || next >= ext_end {
                MbrAnomaly::LinkOutsideExtended {
                    lba: current,
                    target: next,
                }
            } else {
                current = next;
                continue;
            };
            self.push(anomaly);
            return Ok(());
        }
    }

    /// Interpreta un EBR: añade sus lógicas y devuelve la LBA del siguiente EBR, si lo hay.
    fn ebr(&mut self, lba: u64, sector: &[u8], ext_start: u64, ext_sectors: u64) -> Option<u64> {
        let Some(raw) = parse_raw(sector) else {
            self.push(MbrAnomaly::EbrInvalid {
                lba,
                reason: NoTableReason::OutsideDevice,
            });
            return None;
        };
        let used = used_entries(&raw);
        if used.is_empty() {
            if !raw.signature {
                self.push(MbrAnomaly::EbrInvalid {
                    lba,
                    reason: NoTableReason::Blank,
                });
            }
            // Con firma y sin entradas: extendida sin lógicas, no es una anomalía.
            return None;
        }
        let links: Vec<(u8, RawEntry)> = used
            .iter()
            .copied()
            .filter(|(_, e)| is_extended(e.kind))
            .collect();
        if !raw.signature {
            if used.iter().any(|(_, e)| !e.boot_valid()) {
                self.push(MbrAnomaly::EbrInvalid {
                    lba,
                    reason: NoTableReason::InvalidBootIndicator,
                });
                return None;
            }
            self.push(MbrAnomaly::SignatureMissing { lba });
        }
        if links.len() > 1 {
            // INV-05 §2.3, regla 3: con más de un enlace el sector no es un EBR utilizable.
            self.push(MbrAnomaly::MultipleExtended {
                lba,
                slots: links.iter().map(|&(s, _)| s).collect(),
            });
            return None;
        }
        let data: Vec<(u8, RawEntry)> = used
            .iter()
            .copied()
            .filter(|(_, e)| !is_extended(e.kind))
            .collect();
        if data.len() > 1 {
            self.push(MbrAnomaly::ExtraDataEntries {
                lba,
                count: u8::try_from(data.len()).unwrap_or(u8::MAX),
            });
        }
        for (slot, e) in data {
            // Entrada de datos: relativa al propio EBR.
            let origin = Origin::Logical { ebr_lba: lba, slot };
            self.add_entry(lba, origin, e, Some((ext_start, ext_sectors)));
        }
        let &(slot, link) = links.first()?;
        if !link.boot_valid() {
            self.push(MbrAnomaly::InvalidBootIndicator {
                lba,
                slot,
                value: link.boot,
                used: true,
            });
        }
        // Enlace: relativo al inicio de la extendida.
        Some(start_of(ext_start, link.start))
    }

    /// Solapes entre particiones listadas con un barrido ordenado: como mucho una anomalía por
    /// partición, así la salida queda acotada aunque haya muchas entradas solapadas.
    fn check_overlaps(&mut self) {
        let mut spans: Vec<(u64, u64, Origin, u64)> = self
            .partitions
            .iter()
            .filter(|p| p.start_lba < self.total)
            .map(|p| {
                let table = match p.origin {
                    Origin::Logical { ebr_lba, .. } => ebr_lba,
                    _ => self.table_lba,
                };
                (
                    p.start_lba,
                    p.start_lba.saturating_add(p.sector_count),
                    p.origin,
                    table,
                )
            })
            .collect();
        spans.sort_by_key(|s| (s.0, s.1));
        let mut holder: Option<(u64, Origin)> = None;
        let mut found = Vec::new();
        for (start, end, origin, table) in spans {
            match holder {
                Some((hend, horigin)) if start < hend => {
                    found.push(MbrAnomaly::Overlap {
                        lba: table,
                        first: horigin,
                        second: origin,
                    });
                    if end > hend {
                        holder = Some((end, origin));
                    }
                }
                _ => holder = Some((end, origin)),
            }
        }
        for a in found {
            self.push(a);
        }
    }
}

/// Confianza de cada partición: sus propias anomalías, las de la tabla que la mencionan y la falta
/// de firma de la tabla de la que procede.
fn assign_confidence(scan: &mut MbrScan) {
    for p in &mut scan.partitions {
        let own_table = match p.origin {
            Origin::Logical { ebr_lba, .. } => ebr_lba,
            _ => scan.table_lba,
        };
        let related = scan.anomalies.iter().filter(|a| match a {
            Anomaly::Mbr(MbrAnomaly::SignatureMissing { lba }) => *lba == own_table,
            Anomaly::Mbr(m) => m.origins().contains(&p.origin),
            _ => false,
        });
        p.confidence = confidence_of(p.anomalies.iter().chain(related));
    }
}

/// Confianza resultante de un conjunto de anomalías: decide la peor gravedad.
pub fn confidence_of<'a>(anomalies: impl IntoIterator<Item = &'a Anomaly>) -> Confidence {
    let mut suspicious = Vec::new();
    let mut invalid = Vec::new();
    let mut fixes = Vec::new();
    for a in anomalies {
        match a.severity() {
            Severity::Notice => {}
            Severity::Compensated => fixes.push(a.to_string()),
            Severity::Suspicious => suspicious.push(a.to_string()),
            Severity::Invalid => invalid.push(a.to_string()),
        }
    }
    if !invalid.is_empty() {
        Confidence::Invalid(invalid.join("; "))
    } else if !suspicious.is_empty() {
        Confidence::Suspicious(suspicious)
    } else if !fixes.is_empty() {
        Confidence::ValidWithFixes(fixes)
    } else {
        Confidence::Valid
    }
}

#[cfg(test)]
mod tests;
