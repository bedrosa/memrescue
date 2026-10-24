//! Apple Partition Map (APM, issue #17, PT-04).
//!
//! Disposición pública (Apple, *Inside Macintosh: Devices*, «SCSI Manager», estructuras `Block0` y
//! `Partition`; todos los enteros en orden big-endian):
//! - Bloque 0: *Driver Descriptor Map* (DDM) con la firma `ER` (0x4552), el tamaño de bloque del
//!   dispositivo (`sbBlkSize`, u16 en +2) y el número de bloques (`sbBlkCount`, u32 en +4).
//! - Bloques 1 a N: una entrada del mapa por bloque, con la firma `PM` (0x504D) en +0, el número de
//!   entradas del mapa (`pmMapBlkCnt`, u32 en +4, igual en todas), el primer bloque
//!   (`pmPyPartStart`, u32 en +8) y el tamaño en bloques (`pmPartBlkCnt`, u32 en +12), el nombre
//!   (`pmPartName`, 32 bytes en +16) y el tipo (`pmParType`, 32 bytes en +48) como cadenas terminadas
//!   en NUL, el área de datos lógica (`pmLgDataStart` en +80 y `pmDataCnt` en +84, relativas a la
//!   partición) y los indicadores de estado (`pmPartStatus`, u32 en +88).
//!
//! Reglas propias (detalle en `docs/formatos/apm.md`):
//! - La entrada `i` está en el byte `i * B` del origen y las posiciones del mapa se expresan en
//!   bloques de `B` bytes, donde `B` es el tamaño de bloque del DDM si es una potencia de dos entre
//!   512 y 4096 y allí hay una entrada `PM`; si no, se prueba con el tamaño de sector del origen y con
//!   512 (y se anota la discrepancia). Las LBA de las particiones quedan en bloques de `B` y la tabla
//!   se interpreta con `B` como tamaño de sector: si difiere del sector del origen, abrir una
//!   partición exige reabrir el origen con ese tamaño (ADR-022, las LBA no se convierten en silencio).
//! - El número de entradas se toma de la primera entrada y se limita a [`MAX_MAP_ENTRIES`] y al
//!   tamaño del origen; las entradas sin firma no se listan.
//! - Se listan todas las entradas con tamaño mayor que 0 (también `Apple_partition_map` y
//!   `Apple_Free`, que forman parte del mapa), con su [`Confidence`] y sus anomalías.
//!
//! Todas las lecturas pasan por [`read_trusted`]: un sector ilegible produce
//! [`Anomaly::UnreadableSector`] y nunca se interpreta. Aritmética en `u64` sin desbordamiento
//! posible (los campos son de 32 bits) y salida acotada.

use crate::sector::read_trusted;
use crate::{Anomaly, Confidence, Origin, Partition, PartitionKind, Severity};
use mr_io::BlockDevice;
use std::fmt;

/// Firma del *Driver Descriptor Map* (bloque 0).
pub const DDM_SIGNATURE: &[u8; 2] = b"ER";
/// Firma de una entrada del mapa.
pub const ENTRY_SIGNATURE: &[u8; 2] = b"PM";
/// Bytes que se leen de cada entrada (el registro ocupa 512 bytes en todos los tamaños de bloque).
pub const ENTRY_BYTES: u64 = 512;
/// Tamaños de bloque admitidos (potencias de dos).
pub const MIN_BLOCK_SIZE: u32 = 512;
pub const MAX_BLOCK_SIZE: u32 = 4096;
/// Máximo de entradas del mapa que se leen (límite explícito; Apple reserva 63 por omisión).
pub const MAX_MAP_ENTRIES: u32 = 512;
/// `pmPartStatus`: partición de arranque (bit 31).
pub const STATUS_STARTUP: u32 = 0x8000_0000;
/// Reintentos por sector en las lecturas del mapa.
pub const READ_RETRIES: u32 = 1;

/// `true` si `b` es un tamaño de bloque APM admitido.
pub fn valid_block_size(b: u32) -> bool {
    (MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&b) && b.is_power_of_two()
}

/// Tipo de una entrada APM (`pmParType`, 32 bytes tal como están en el disco).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApmType(pub [u8; 32]);

impl ApmType {
    /// Texto del tipo hasta el primer NUL (ASCII imprimible; otros bytes como `?`).
    pub fn text(&self) -> String {
        decode_text(&self.0)
    }
}

/// Texto de una cadena APM de longitud fija: hasta el primer NUL, ASCII imprimible y `?` para los
/// demás bytes (los nombres Mac OS clásicos pueden estar en Mac Roman; no se adivina la codificación).
pub fn decode_text(raw: &[u8]) -> String {
    raw.iter()
        .take_while(|&&b| b != 0)
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

/// Nombre legible de un tipo APM, o `None` si no figura en la lista. Lista propia de los tipos
/// documentados públicamente por Apple; la comparación no distingue mayúsculas.
pub fn type_name(t: &ApmType) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("Apple_partition_map", "Mapa de particiones de Apple"),
        ("Apple_Free", "Espacio libre"),
        ("Apple_HFS", "HFS / HFS+"),
        ("Apple_HFSX", "HFSX"),
        ("Apple_MFS", "MFS"),
        ("Apple_UFS", "UFS de Apple"),
        ("Apple_Boot", "Arranque de Apple"),
        ("Apple_Bootstrap", "Arranque (NewWorld)"),
        ("Apple_Boot_RAID", "Arranque RAID de Apple"),
        ("Apple_RAID", "RAID de Apple"),
        ("Apple_APFS", "Apple APFS"),
        ("Apple_CoreStorage", "Core Storage de Apple"),
        ("Apple_Driver", "Controlador de Apple"),
        ("Apple_Driver43", "Controlador SCSI de Apple"),
        ("Apple_Driver43_CD", "Controlador SCSI de CD de Apple"),
        ("Apple_Driver_ATA", "Controlador ATA de Apple"),
        ("Apple_Driver_ATAPI", "Controlador ATAPI de Apple"),
        ("Apple_Driver_IOKit", "Controlador IOKit de Apple"),
        ("Apple_FWDriver", "Controlador FireWire de Apple"),
        ("Apple_Patches", "Parches de Apple"),
        ("Apple_PRODOS", "ProDOS"),
        ("Apple_Rhapsody_UFS", "UFS de Rhapsody"),
        ("Apple_Scratch", "Vacía (Apple_Scratch)"),
        ("Apple_Void", "Relleno (Apple_Void)"),
        ("Apple_UNIX_SVR2", "A/UX"),
    ];
    let text = t.text();
    KNOWN
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&text))
        .map(|(_, n)| *n)
}

/// Anomalías del Apple Partition Map. `lba` es la LBA del origen (en sectores del dispositivo) del
/// bloque donde se detectó; `index` es el número de entrada del mapa (desde 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApmAnomaly {
    /// El bloque 0 no tiene la firma `ER` (el mapa se encontró igualmente).
    DdmMissing,
    /// `sbBlkSize` no es una potencia de dos entre 512 y 4096.
    BlockSizeInvalid { declared: u32 },
    /// El mapa está en bloques de `used` bytes aunque el DDM declara `declared`.
    BlockSizeMismatch { declared: u32, used: u32 },
    /// `sbBlkCount` no coincide con los bloques del origen (`available`).
    DiskSizeMismatch { declared: u64, available: u64 },
    /// DDM válido sin ninguna entrada `PM` en el bloque 1.
    MapMissing { lba: u64 },
    /// `pmMapBlkCnt` de la primera entrada es 0 o supera [`MAX_MAP_ENTRIES`].
    MapCountInvalid { lba: u64, count: u32 },
    /// Una entrada declara un número de entradas distinto del de la primera.
    MapCountMismatch { lba: u64, index: u32, count: u32 },
    /// El mapa declara entradas más allá del final del origen; la primera que falta es `index`.
    MapOutsideImage { index: u32 },
    /// La entrada `index` no tiene la firma `PM`: no se lista.
    EntrySignature { lba: u64, index: u32 },
    /// Entrada con tamaño 0 (no se lista).
    EntryZeroLength { lba: u64, index: u32 },
    /// La entrada empieza fuera del disco.
    EntryOutsideDisk { lba: u64, index: u32, start: u64 },
    /// La entrada empieza dentro y termina fuera del disco (`end` exclusivo, en bloques).
    EntryPartiallyOutsideDisk { lba: u64, index: u32, end: u64 },
    /// Dos entradas se solapan.
    EntryOverlap { lba: u64, index: u32, other: u32 },
    /// El área de datos lógica (`pmLgDataStart` + `pmDataCnt`) sale de la partición.
    EntryDataOutside { lba: u64, index: u32 },
    /// El mapa usa bloques de `block_size` bytes y el origen sectores de `device` bytes.
    SectorSizeMismatch { block_size: u32, device: u32 },
}

impl ApmAnomaly {
    /// LBA del origen donde se detectó (`None` si no tiene posición en la imagen).
    pub fn lba(&self) -> Option<u64> {
        match *self {
            Self::DdmMissing
            | Self::BlockSizeInvalid { .. }
            | Self::BlockSizeMismatch { .. }
            | Self::DiskSizeMismatch { .. } => Some(0),
            Self::MapMissing { lba }
            | Self::MapCountInvalid { lba, .. }
            | Self::MapCountMismatch { lba, .. }
            | Self::EntrySignature { lba, .. }
            | Self::EntryZeroLength { lba, .. }
            | Self::EntryOutsideDisk { lba, .. }
            | Self::EntryPartiallyOutsideDisk { lba, .. }
            | Self::EntryOverlap { lba, .. }
            | Self::EntryDataOutside { lba, .. } => Some(lba),
            Self::MapOutsideImage { .. } | Self::SectorSizeMismatch { .. } => None,
        }
    }

    /// Código estable (JSON de la CLI). Coincide con el del manifiesto del corpus cuando existe.
    pub fn code(&self) -> &'static str {
        match self {
            Self::DdmMissing => "apm_ddm_missing",
            Self::BlockSizeInvalid { .. } => "apm_block_size_invalid",
            Self::BlockSizeMismatch { .. } => "apm_block_size_mismatch",
            Self::DiskSizeMismatch { .. } => "apm_disk_size_mismatch",
            Self::MapMissing { .. } => "apm_map_missing",
            Self::MapCountInvalid { .. } => "apm_map_count_invalid",
            Self::MapCountMismatch { .. } => "apm_map_count_mismatch",
            Self::MapOutsideImage { .. } => "apm_map_outside_image",
            Self::EntrySignature { .. } => "apm_entry_signature",
            Self::EntryZeroLength { .. } => "apm_entry_zero_length",
            Self::EntryOutsideDisk { .. } => "apm_entry_outside_disk",
            Self::EntryPartiallyOutsideDisk { .. } => "apm_entry_partially_outside_disk",
            Self::EntryOverlap { .. } => "apm_entry_overlap",
            Self::EntryDataOutside { .. } => "apm_entry_data_outside",
            Self::SectorSizeMismatch { .. } => "apm_sector_size_mismatch",
        }
    }

    /// Gravedad: inválida si el mapa falta o una entrada empieza fuera del disco; informativa si
    /// el tamaño de bloque difiere del sector (normal en CD) o el DDM declara menos bloques de los
    /// que hay; sospechosa en los demás casos.
    pub fn severity(&self) -> Severity {
        match self {
            Self::MapMissing { .. } | Self::EntryOutsideDisk { .. } => Severity::Invalid,
            Self::SectorSizeMismatch { .. } => Severity::Notice,
            Self::DiskSizeMismatch {
                declared,
                available,
            } if declared <= available => Severity::Notice,
            _ => Severity::Suspicious,
        }
    }

    /// Valor asociado: tamaño o número declarado, número de entrada o inicio fuera del disco.
    pub fn value(&self) -> Option<u64> {
        match *self {
            Self::DdmMissing | Self::MapMissing { .. } => None,
            Self::BlockSizeInvalid { declared } | Self::BlockSizeMismatch { declared, .. } => {
                Some(u64::from(declared))
            }
            Self::DiskSizeMismatch { declared, .. } => Some(declared),
            Self::MapCountInvalid { count, .. } | Self::MapCountMismatch { count, .. } => {
                Some(u64::from(count))
            }
            Self::MapOutsideImage { index }
            | Self::EntrySignature { index, .. }
            | Self::EntryZeroLength { index, .. }
            | Self::EntryOutsideDisk { index, .. }
            | Self::EntryPartiallyOutsideDisk { index, .. }
            | Self::EntryOverlap { index, .. }
            | Self::EntryDataOutside { index, .. } => Some(u64::from(index)),
            Self::SectorSizeMismatch { block_size, .. } => Some(u64::from(block_size)),
        }
    }
}

impl fmt::Display for ApmAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DdmMissing => write!(
                f,
                "APM: el bloque 0 no tiene la firma ER del Driver Descriptor Map"
            ),
            Self::BlockSizeInvalid { declared } => write!(
                f,
                "APM: el Driver Descriptor Map declara un tamaño de bloque de {declared} B (no admitido)"
            ),
            Self::BlockSizeMismatch { declared, used } => write!(
                f,
                "APM: el Driver Descriptor Map declara bloques de {declared} B y el mapa está en bloques de {used} B"
            ),
            Self::DiskSizeMismatch {
                declared,
                available,
            } => write!(
                f,
                "APM: el Driver Descriptor Map declara {declared} bloques y el origen tiene {available}"
            ),
            Self::MapMissing { lba } => write!(
                f,
                "APM: Driver Descriptor Map sin mapa de particiones (ninguna entrada PM en la LBA {lba})"
            ),
            Self::MapCountInvalid { lba, count } => write!(
                f,
                "APM: la entrada de la LBA {lba} declara {count} entradas en el mapa (fuera de 1..{MAX_MAP_ENTRIES})"
            ),
            Self::MapCountMismatch { index, count, .. } => write!(
                f,
                "APM: la entrada {index} declara {count} entradas en el mapa, distinto de la primera"
            ),
            Self::MapOutsideImage { index } => write!(
                f,
                "APM: el mapa declara entradas más allá del final del origen (desde la entrada {index})"
            ),
            Self::EntrySignature { lba, index } => write!(
                f,
                "APM: la entrada {index} (LBA {lba}) no tiene la firma PM: no se lista"
            ),
            Self::EntryZeroLength { index, .. } => {
                write!(f, "APM: la entrada {index} tiene tamaño 0")
            }
            Self::EntryOutsideDisk { index, start, .. } => write!(
                f,
                "APM: la entrada {index} empieza en el bloque {start}, fuera del disco"
            ),
            Self::EntryPartiallyOutsideDisk { index, end, .. } => write!(
                f,
                "APM: la entrada {index} termina en el bloque {}, fuera del disco",
                end.saturating_sub(1)
            ),
            Self::EntryOverlap { index, other, .. } => {
                write!(f, "APM: la entrada {index} se solapa con la entrada {other}")
            }
            Self::EntryDataOutside { index, .. } => write!(
                f,
                "APM: el área de datos de la entrada {index} sale de la partición"
            ),
            Self::SectorSizeMismatch { block_size, device } => write!(
                f,
                "APM: el mapa usa bloques de {block_size} B y el origen sectores de {device} B; \
                 las LBA están en bloques de {block_size} B"
            ),
        }
    }
}

/// Datos propios de una entrada APM listada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApmPartition {
    /// `origin` = `Origin::Apm { index }`, `kind` = `PartitionKind::Apm(tipo)`, LBA en bloques.
    pub partition: Partition,
    /// `pmPartStatus`.
    pub status: u32,
    /// `pmLgDataStart` y `pmDataCnt` (relativos a la partición, en bloques).
    pub data_start: u32,
    pub data_count: u32,
}

/// Resultado de leer un Apple Partition Map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApmRead {
    /// El bloque 0 tiene la firma `ER`.
    pub ddm: bool,
    /// `sbBlkSize` y `sbBlkCount` del DDM (0 sin DDM).
    pub ddm_block_size: u32,
    pub ddm_block_count: u32,
    /// Tamaño de bloque con que se interpreta el mapa (y en el que están las LBA).
    pub block_size: u32,
    /// Bloques de `block_size` bytes que tiene el origen.
    pub disk_blocks: u64,
    /// `pmMapBlkCnt` de la primera entrada (0 si no hay mapa).
    pub map_entries: u32,
    /// Hay al menos una entrada `PM` (si no, solo hay DDM).
    pub map_found: bool,
    pub partitions: Vec<ApmPartition>,
    /// Anomalías no atribuibles a una sola partición.
    pub anomalies: Vec<Anomaly>,
    pub confidence: Confidence,
}

/// Contenido de un tramo de bytes del origen.
enum Bytes {
    Data(Vec<u8>),
    /// Algún sector del tramo no es fiable: su primera LBA.
    Unreadable(u64),
    Outside,
}

/// Lee [`ENTRY_BYTES`] bytes desde el byte `offset` del origen (pueden abarcar varios sectores o
/// ser parte de uno).
fn read_bytes(dev: &(impl BlockDevice + ?Sized), offset: u64) -> mr_io::Result<Bytes> {
    let ss = u64::from(dev.sector_size()).max(1);
    let Some(end) = offset.checked_add(ENTRY_BYTES) else {
        return Ok(Bytes::Outside);
    };
    let first = offset / ss;
    let last = (end - 1) / ss;
    if last >= dev.sector_count() {
        return Ok(Bytes::Outside);
    }
    let t = read_trusted(dev, first, last - first + 1, READ_RETRIES)?;
    if let Some(bad) = t.untrusted().next() {
        return Ok(Bytes::Unreadable(bad));
    }
    let Some(all) = t.all() else {
        return Ok(Bytes::Outside);
    };
    let at = usize::try_from(offset - first * ss).unwrap_or(usize::MAX);
    Ok(all
        .get(at..at.saturating_add(ENTRY_BYTES as usize))
        .map_or(Bytes::Outside, |b| Bytes::Data(b.to_vec())))
}

fn be16(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_be_bytes([b[at], b[at + 1]]))
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Lee el Apple Partition Map del origen. `None` si no hay indicios de APM: ni entrada `PM` en el
/// bloque 1 (con el tamaño del DDM, el del sector o 512) ni un DDM con firma `ER` y tamaño de
/// bloque admitido. Con DDM y sin mapa devuelve una lectura sin particiones y
/// [`ApmAnomaly::MapMissing`].
pub fn read_apm(dev: &(impl BlockDevice + ?Sized)) -> mr_io::Result<Option<ApmRead>> {
    let ss = dev.sector_size();
    let total_bytes = dev.sector_count().saturating_mul(u64::from(ss));
    let mut anomalies = Vec::new();
    let block0 = read_bytes(dev, 0)?;
    let (ddm, ddm_bs, ddm_count) = match &block0 {
        Bytes::Data(d) if d.starts_with(DDM_SIGNATURE) => (true, be16(d, 2), be32(d, 4)),
        _ => (false, 0, 0),
    };
    let ddm_valid = ddm && valid_block_size(ddm_bs);
    let mut candidates: Vec<u32> = Vec::with_capacity(3);
    for c in [ddm_valid.then_some(ddm_bs), Some(ss), Some(MIN_BLOCK_SIZE)]
        .into_iter()
        .flatten()
    {
        if valid_block_size(c) && !candidates.contains(&c) {
            candidates.push(c);
        }
    }
    // Con varios candidatos con firma `PM` (el bloque 1 de 2048 B es la entrada 4 de un mapa de
    // 512 B) se prefiere el que describe el propio mapa: `Apple_partition_map` desde el bloque 1.
    let mut hits: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut unreadable_map: Option<u64> = None;
    for &c in &candidates {
        match read_bytes(dev, u64::from(c))? {
            Bytes::Data(d) if d.starts_with(ENTRY_SIGNATURE) => hits.push((c, d)),
            Bytes::Unreadable(lba) if c == candidates[0] => unreadable_map = Some(lba),
            _ => {}
        }
    }
    let self_describing = |d: &[u8]| {
        be32(d, 8) == 1 && decode_text(&d[48..80]).eq_ignore_ascii_case("Apple_partition_map")
    };
    let pick = hits
        .iter()
        .position(|(_, d)| self_describing(d))
        .unwrap_or(0);
    let found = (!hits.is_empty()).then(|| hits.swap_remove(pick));
    let Some((bs, first)) = found else {
        if !ddm_valid {
            return Ok(None);
        }
        if let Some(lba) = unreadable_map {
            anomalies.push(Anomaly::UnreadableSector { lba });
        }
        anomalies.push(Anomaly::Apm(ApmAnomaly::MapMissing {
            lba: u64::from(ddm_bs) / u64::from(ss.max(1)),
        }));
        let confidence = crate::mbr::confidence_of(anomalies.iter());
        return Ok(Some(ApmRead {
            ddm,
            ddm_block_size: ddm_bs,
            ddm_block_count: ddm_count,
            block_size: ddm_bs,
            disk_blocks: total_bytes / u64::from(ddm_bs),
            map_entries: 0,
            map_found: false,
            partitions: Vec::new(),
            anomalies,
            confidence,
        }));
    };
    let block = u64::from(bs);
    let disk_blocks = total_bytes / block;
    let lba_of = |index: u64| index * block / u64::from(ss.max(1));

    // Bloque 0.
    match (&block0, ddm) {
        (Bytes::Unreadable(lba), _) => anomalies.push(Anomaly::UnreadableSector { lba: *lba }),
        (_, false) => anomalies.push(Anomaly::Apm(ApmAnomaly::DdmMissing)),
        (_, true) if !valid_block_size(ddm_bs) => {
            anomalies.push(Anomaly::Apm(ApmAnomaly::BlockSizeInvalid {
                declared: ddm_bs,
            }))
        }
        (_, true) => {
            if ddm_bs != bs {
                anomalies.push(Anomaly::Apm(ApmAnomaly::BlockSizeMismatch {
                    declared: ddm_bs,
                    used: bs,
                }));
            }
            let declared = u64::from(ddm_count);
            let available = total_bytes / u64::from(ddm_bs);
            if declared != available {
                anomalies.push(Anomaly::Apm(ApmAnomaly::DiskSizeMismatch {
                    declared,
                    available,
                }));
            }
        }
    }
    if bs != ss {
        anomalies.push(Anomaly::Apm(ApmAnomaly::SectorSizeMismatch {
            block_size: bs,
            device: ss,
        }));
    }

    // Número de entradas: el de la primera, acotado al límite y al origen.
    let map_entries = be32(&first, 4);
    let mut count = map_entries;
    if map_entries == 0 || map_entries > MAX_MAP_ENTRIES {
        anomalies.push(Anomaly::Apm(ApmAnomaly::MapCountInvalid {
            lba: lba_of(1),
            count: map_entries,
        }));
        count = map_entries.clamp(1, MAX_MAP_ENTRIES);
    }
    // Última entrada que cabe en el origen (la primera se ha leído, así que es al menos la 1).
    let fit = u32::try_from(total_bytes.saturating_sub(ENTRY_BYTES) / block).unwrap_or(u32::MAX);
    if count > fit {
        anomalies.push(Anomaly::Apm(ApmAnomaly::MapOutsideImage {
            index: fit.saturating_add(1),
        }));
        count = fit;
    }

    let mut partitions = Vec::new();
    for index in 1..=count {
        let at = u64::from(index) * block;
        let lba = lba_of(u64::from(index));
        let entry = if index == 1 {
            first.clone()
        } else {
            match read_bytes(dev, at)? {
                Bytes::Data(d) => d,
                Bytes::Unreadable(bad) => {
                    anomalies.push(Anomaly::UnreadableSector { lba: bad });
                    continue;
                }
                Bytes::Outside => break,
            }
        };
        if !entry.starts_with(ENTRY_SIGNATURE) {
            anomalies.push(Anomaly::Apm(ApmAnomaly::EntrySignature { lba, index }));
            continue;
        }
        let declared = be32(&entry, 4);
        if index > 1 && declared != map_entries {
            anomalies.push(Anomaly::Apm(ApmAnomaly::MapCountMismatch {
                lba,
                index,
                count: declared,
            }));
        }
        let start = u64::from(be32(&entry, 8));
        let size = u64::from(be32(&entry, 12));
        if size == 0 {
            anomalies.push(Anomaly::Apm(ApmAnomaly::EntryZeroLength { lba, index }));
            continue;
        }
        let mut own = Vec::new();
        if start >= disk_blocks {
            own.push(Anomaly::Apm(ApmAnomaly::EntryOutsideDisk {
                lba,
                index,
                start,
            }));
        } else if start + size > disk_blocks {
            own.push(Anomaly::Apm(ApmAnomaly::EntryPartiallyOutsideDisk {
                lba,
                index,
                end: start + size,
            }));
        }
        let data_start = be32(&entry, 80);
        let data_count = be32(&entry, 84);
        if u64::from(data_start) + u64::from(data_count) > size {
            own.push(Anomaly::Apm(ApmAnomaly::EntryDataOutside { lba, index }));
        }
        let mut kind = [0u8; 32];
        kind.copy_from_slice(&entry[48..80]);
        let status = be32(&entry, 88);
        partitions.push(ApmPartition {
            partition: Partition {
                origin: Origin::Apm { index },
                start_lba: start,
                sector_count: size,
                kind: PartitionKind::Apm(ApmType(kind)),
                name: Some(decode_text(&entry[16..48])),
                active: status & STATUS_STARTUP != 0,
                confidence: Confidence::Valid,
                anomalies: own,
            },
            status,
            data_start,
            data_count,
        });
    }
    mark_overlaps(&mut partitions, |i| lba_of(u64::from(i)));
    for p in &mut partitions {
        p.partition.confidence = crate::mbr::confidence_of(p.partition.anomalies.iter());
    }
    let confidence = crate::mbr::confidence_of(
        anomalies
            .iter()
            .chain(partitions.iter().flat_map(|p| p.partition.anomalies.iter())),
    );
    Ok(Some(ApmRead {
        ddm,
        ddm_block_size: ddm_bs,
        ddm_block_count: ddm_count,
        block_size: bs,
        disk_blocks,
        map_entries,
        map_found: true,
        partitions,
        anomalies,
        confidence,
    }))
}

fn index_of(p: &ApmPartition) -> u32 {
    match p.partition.origin {
        Origin::Apm { index } => index,
        _ => 0,
    }
}

/// Barrido ordenado por inicio: cada entrada solapada recibe como mucho una anomalía (con la
/// entrada de mayor final vista hasta entonces), así la salida queda acotada.
fn mark_overlaps(parts: &mut [ApmPartition], lba_of: impl Fn(u32) -> u64) {
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| (parts[i].partition.start_lba, i));
    let mut reach: Option<(u64, usize)> = None;
    let mut found = Vec::new();
    for &i in &order {
        let p = &parts[i].partition;
        let end = p.start_lba.saturating_add(p.sector_count);
        if let Some((r_end, r)) = reach {
            if p.start_lba < r_end {
                found.push((i, r));
            }
        }
        if reach.is_none_or(|(r_end, _)| end > r_end) {
            reach = Some((end, i));
        }
    }
    for (i, r) in found {
        let (index, other) = (index_of(&parts[i]), index_of(&parts[r]));
        parts[i]
            .partition
            .anomalies
            .push(Anomaly::Apm(ApmAnomaly::EntryOverlap {
                lba: lba_of(index),
                index,
                other,
            }));
    }
}

#[cfg(test)]
mod tests;
