//! GPT (GUID Partition Table) con cabecera de respaldo y tolerancias (issue #13, PT-02 y PT-03).
//!
//! Basado solo en la especificación pública UEFI (capítulo 5, «GUID Partition Table (GPT) Disk
//! Layout»). Documentación del formato, reglas de selección, tolerancias y límites en
//! `docs/formatos/gpt.md`.
//!
//! Punto de entrada: [`read_gpt`]. Lee el origen **solo** con [`read_trusted`]: un sector
//! ilegible o saltado nunca se interpreta (produce [`Anomaly::UnreadableSector`] y la estructura
//! afectada queda en estado `Unreadable`).
//!
//! Coordenadas: las LBA **declaradas** por la GPT (campos de la cabecera y de las entradas, y
//! [`Partition::start_lba`]) son del disco original; las LBA donde se ha leído algo son de la
//! imagen. La relación es `LBA disco = LBA imagen + shift` ([`GptRead::shift`]); el desplazamiento
//! se detecta y se informa ([`GptAnomaly::ImageShifted`]), pero **nunca** se aplica en silencio a
//! las particiones. Todas las LBA se expresan en sectores de [`GptRead::sector_size`] bytes, salvo
//! las de [`Anomaly::UnreadableSector`], que son sectores del dispositivo.

use std::collections::HashMap;
use std::fmt;

use mr_io::{BlockDevice, Result};

use crate::sector::{read_trusted, MAX_TRUSTED_SECTORS};
use crate::{Anomaly, Confidence, Origin, Partition, PartitionKind};

/// Firma de la cabecera GPT.
pub const SIGNATURE: &[u8; 8] = b"EFI PART";
/// Revisión 1.0 de la cabecera.
pub const REVISION_1_0: u32 = 0x0001_0000;
/// Tamaño mínimo de la cabecera (los campos definidos por UEFI).
pub const MIN_HEADER_SIZE: u32 = 92;
/// Tamaño mínimo de una entrada (UEFI: 128 × 2^n).
pub const MIN_ENTRY_SIZE: u32 = 128;
/// Tamaño máximo de entrada que se admite (límite propio; UEFI no fija uno).
pub const MAX_ENTRY_SIZE: u32 = 4096;
/// Número máximo de entradas que se admite (límite propio).
pub const MAX_ENTRY_COUNT: u32 = 32768;
/// Tamaño máximo de la tabla de entradas que se lee a memoria (límite propio, 4 MiB).
pub const MAX_ENTRY_ARRAY_BYTES: u64 = 4 * 1024 * 1024;
/// Bit 2 de los atributos: «Legacy BIOS Bootable», el equivalente al indicador de arranque MBR.
pub const ATTR_LEGACY_BIOS_BOOTABLE: u64 = 1 << 2;
/// Tamaños de sector que se prueban si con el lógico del dispositivo no aparece ninguna firma.
pub const PROBED_SECTOR_SIZES: [u32; 2] = [512, 4096];

/// Reintentos por sector en las lecturas de la GPT.
const READ_RETRIES: u32 = 1;

/// Copia de la GPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GptCopy {
    /// Cabecera en la LBA 1 y entradas a continuación.
    Primary,
    /// Cabecera en la última LBA del disco y entradas justo antes.
    Backup,
}

impl GptCopy {
    fn name(self) -> &'static str {
        match self {
            GptCopy::Primary => "primaria",
            GptCopy::Backup => "de respaldo",
        }
    }
}

/// Estado de una cabecera GPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderState {
    /// Firma, tamaño, CRC-32, revisión y disposición correctos.
    Valid,
    /// Firma presente, pero CRC-32 o tamaño de cabecera incorrectos.
    Crc,
    /// CRC-32 correcta, pero campos incoherentes (revisión, MyLBA/AlternateLBA, rangos, límites).
    Invalid,
    /// Sin firma en la posición esperada.
    Missing,
    /// La posición esperada no existe en la imagen (recortada o desplazada).
    OutsideImage,
    /// El sector no se pudo leer: no se interpreta.
    Unreadable,
}

/// Estado de la tabla de entradas de una copia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntriesState {
    /// CRC-32 de la tabla igual a la declarada en la cabecera.
    Valid,
    /// CRC-32 distinta.
    Crc,
    /// La tabla queda (en parte) fuera de la imagen.
    OutsideImage,
    /// Algún sector de la tabla no se pudo leer: no se interpreta.
    Unreadable,
    /// No comprobada porque la cabecera no es válida.
    Unchecked,
}

/// Campos de una cabecera GPT (orden y significado de UEFI, tabla «GPT Header»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptHeader {
    pub revision: u32,
    pub header_size: u32,
    pub header_crc32: u32,
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    /// GUID de disco en el orden de bytes del disco.
    pub disk_guid: [u8; 16],
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc32: u32,
}

impl GptHeader {
    /// Interpreta los campos de un sector que empieza por la firma (sin validar nada más).
    pub fn parse(sec: &[u8]) -> Option<Self> {
        if sec.len() < MIN_HEADER_SIZE as usize || &sec[..8] != SIGNATURE {
            return None;
        }
        let mut disk_guid = [0u8; 16];
        disk_guid.copy_from_slice(&sec[56..72]);
        Some(Self {
            revision: le32(sec, 8),
            header_size: le32(sec, 12),
            header_crc32: le32(sec, 16),
            my_lba: le64(sec, 24),
            alternate_lba: le64(sec, 32),
            first_usable_lba: le64(sec, 40),
            last_usable_lba: le64(sec, 48),
            disk_guid,
            entries_lba: le64(sec, 72),
            entry_count: le32(sec, 80),
            entry_size: le32(sec, 84),
            entries_crc32: le32(sec, 88),
        })
    }

    /// Bytes de la tabla de entradas (sin desbordamiento posible: u32 × u32 cabe en u64).
    pub fn array_bytes(&self) -> u64 {
        u64::from(self.entry_count) * u64::from(self.entry_size)
    }
}

/// Qué campo difiere entre las dos copias válidas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptField {
    DiskGuid,
    UsableRange,
    EntryCount,
    EntrySize,
    EntriesCrc32,
    /// `AlternateLBA` de la primaria distinta de `MyLBA` de la copia.
    Location,
}

/// Anomalías específicas de GPT. Las LBA llamadas `lba` son de la imagen (donde se leyó); las
/// llamadas `disk_lba` son las declaradas (del disco original), porque su posición en la imagen
/// puede no existir. Correspondencia con los códigos del manifiesto del corpus en
/// `docs/formatos/gpt.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GptAnomaly {
    /// Sin firma `EFI PART` en la posición esperada.
    HeaderMissing { copy: GptCopy, lba: u64 },
    /// La posición de la cabecera no existe en la imagen.
    HeaderOutsideImage { copy: GptCopy, disk_lba: u64 },
    /// CRC-32 de la cabecera incorrecta.
    HeaderCrc {
        copy: GptCopy,
        lba: u64,
        stored: u32,
        computed: u32,
    },
    /// Tamaño de cabecera fuera de `[92, tamaño de sector]`.
    HeaderSize { copy: GptCopy, lba: u64, size: u32 },
    /// Revisión mayor distinta de 1: la cabecera no se usa.
    RevisionUnsupported {
        copy: GptCopy,
        lba: u64,
        revision: u32,
    },
    /// Revisión 1.x con x distinto de 0: se usa, pero se informa.
    RevisionMinor {
        copy: GptCopy,
        lba: u64,
        revision: u32,
    },
    /// `MyLBA` no coincide con la posición de la cabecera.
    MyLbaMismatch {
        copy: GptCopy,
        lba: u64,
        my_lba: u64,
        expected: u64,
    },
    /// `AlternateLBA` incoherente (la copia no apunta a la LBA 1 o la primaria apunta dentro del
    /// rango utilizable).
    AlternateLbaMismatch {
        copy: GptCopy,
        lba: u64,
        alternate_lba: u64,
    },
    /// `FirstUsableLBA > LastUsableLBA` o rango que pisa la cabecera.
    UsableRangeInvalid {
        copy: GptCopy,
        lba: u64,
        first: u64,
        last: u64,
    },
    /// Tabla de entradas que se solapa con la cabecera o con el rango utilizable.
    EntriesLocationInvalid {
        copy: GptCopy,
        lba: u64,
        entries_lba: u64,
    },
    /// Tamaño o número de entradas fuera de los límites (UEFI o [`MAX_ENTRY_SIZE`],
    /// [`MAX_ENTRY_COUNT`], [`MAX_ENTRY_ARRAY_BYTES`]). No se reserva memoria para la tabla.
    EntryArrayLimits {
        copy: GptCopy,
        lba: u64,
        entry_count: u32,
        entry_size: u32,
    },
    /// CRC-32 de la tabla de entradas incorrecta; `lba` = inicio de la tabla en la imagen.
    EntriesCrc {
        copy: GptCopy,
        lba: u64,
        stored: u32,
        computed: u32,
    },
    /// La tabla de entradas queda fuera de la imagen.
    EntriesOutsideImage { copy: GptCopy, disk_lba: u64 },
    /// Se usa la copia de respaldo porque la primaria no es utilizable (motivo: sus estados).
    BackupInUse {
        primary_header: HeaderState,
        primary_entries: EntriesState,
    },
    /// Las dos copias son válidas pero difieren en `field`.
    CopiesDisagree { field: GptField },
    /// La imagen está desplazada: `LBA disco = LBA imagen + offset` (`offset` > 0: faltan
    /// sectores al inicio; < 0: sobran). Las particiones conservan las LBA del disco.
    ImageShifted { offset: i64 },
    /// Faltan `missing` sectores al final de la imagen (según la cabecera elegida).
    ImageTruncated { missing: u64 },
    /// El rango utilizable de la cabecera elegida (leída en `lba`) no cabe en la imagen.
    UsableRangeOutsideImage { lba: u64, first: u64, last: u64 },
    /// La GPT se encontró con un tamaño de sector distinto del lógico del dispositivo.
    SectorSizeMismatch { device: u32, used: u32 },
    /// Entrada con `StartingLBA > EndingLBA`.
    EntryInverted { index: u32, first: u64, last: u64 },
    /// Entrada fuera de `[FirstUsableLBA, LastUsableLBA]`.
    EntryOutsideUsable { index: u32 },
    /// Entrada que empieza o acaba fuera de la imagen.
    EntryOutsideImage { index: u32 },
    /// Entradas `index` y `other` solapadas.
    EntryOverlap { index: u32, other: u32 },
    /// La entrada `index` repite el GUID único de `other`.
    DuplicateUniqueGuid { index: u32, other: u32 },
    /// Nombre con UTF-16 mal formado (se sustituye por U+FFFD).
    NameInvalid { index: u32 },
}

impl fmt::Display for GptAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use GptAnomaly::*;
        match self {
            HeaderMissing { copy, lba } => {
                write!(f, "cabecera GPT {} ausente en LBA {lba}", copy.name())
            }
            HeaderOutsideImage { copy, disk_lba } => write!(
                f,
                "cabecera GPT {} fuera de la imagen (LBA de disco {disk_lba})",
                copy.name()
            ),
            HeaderCrc {
                copy,
                lba,
                stored,
                computed,
            } => write!(
                f,
                "CRC-32 de la cabecera GPT {} incorrecta en LBA {lba} ({stored:08x}, calculada {computed:08x})",
                copy.name()
            ),
            HeaderSize { copy, lba, size } => write!(
                f,
                "tamaño de cabecera GPT {} no válido en LBA {lba}: {size}",
                copy.name()
            ),
            RevisionUnsupported {
                copy,
                lba,
                revision,
            } => write!(
                f,
                "revisión GPT {} no admitida en LBA {lba}: {revision:#010x}",
                copy.name()
            ),
            RevisionMinor {
                copy,
                lba,
                revision,
            } => write!(
                f,
                "revisión GPT {} {revision:#010x} en LBA {lba}",
                copy.name()
            ),
            MyLbaMismatch {
                copy,
                lba,
                my_lba,
                expected,
            } => write!(
                f,
                "MyLBA de la cabecera GPT {} en LBA {lba} es {my_lba}, se esperaba {expected}",
                copy.name()
            ),
            AlternateLbaMismatch {
                copy,
                lba,
                alternate_lba,
            } => write!(
                f,
                "AlternateLBA incoherente en la cabecera GPT {} de LBA {lba}: {alternate_lba}",
                copy.name()
            ),
            UsableRangeInvalid {
                copy,
                lba,
                first,
                last,
            } => write!(
                f,
                "rango utilizable incoherente en la cabecera GPT {} de LBA {lba}: {first}..{last}",
                copy.name()
            ),
            EntriesLocationInvalid {
                copy,
                lba,
                entries_lba,
            } => write!(
                f,
                "tabla de entradas de la cabecera GPT {} de LBA {lba} mal situada: {entries_lba}",
                copy.name()
            ),
            EntryArrayLimits {
                copy,
                lba,
                entry_count,
                entry_size,
            } => write!(
                f,
                "tabla de entradas GPT {} fuera de límites en LBA {lba}: {entry_count} × {entry_size} B",
                copy.name()
            ),
            EntriesCrc {
                copy,
                lba,
                stored,
                computed,
            } => write!(
                f,
                "CRC-32 de las entradas GPT {} incorrecta en LBA {lba} ({stored:08x}, calculada {computed:08x})",
                copy.name()
            ),
            EntriesOutsideImage { copy, disk_lba } => write!(
                f,
                "entradas GPT {} fuera de la imagen (LBA de disco {disk_lba})",
                copy.name()
            ),
            BackupInUse {
                primary_header,
                primary_entries,
            } => write!(
                f,
                "se usa la copia de respaldo (primaria: cabecera {primary_header:?}, entradas {primary_entries:?})"
            ),
            CopiesDisagree { field } => write!(f, "las copias GPT difieren en {field:?}"),
            ImageShifted { offset } => write!(
                f,
                "imagen desplazada: LBA de disco = LBA de imagen + {offset}"
            ),
            ImageTruncated { missing } => {
                write!(f, "faltan {missing} sectores al final de la imagen")
            }
            UsableRangeOutsideImage { lba, first, last } => write!(
                f,
                "el rango utilizable {first}..{last} de la cabecera leída en LBA {lba} no cabe en la imagen"
            ),
            SectorSizeMismatch { device, used } => write!(
                f,
                "GPT con sectores de {used} B en un dispositivo de {device} B"
            ),
            EntryInverted { index, first, last } => write!(
                f,
                "entrada {index}: inicio {first} posterior al final {last}"
            ),
            EntryOutsideUsable { index } => {
                write!(f, "entrada {index} fuera del rango utilizable")
            }
            EntryOutsideImage { index } => write!(f, "entrada {index} fuera de la imagen"),
            EntryOverlap { index, other } => {
                write!(f, "entradas {index} y {other} solapadas")
            }
            DuplicateUniqueGuid { index, other } => {
                write!(f, "entradas {index} y {other} con el mismo GUID único")
            }
            NameInvalid { index } => write!(f, "entrada {index}: nombre UTF-16 mal formado"),
        }
    }
}

/// Resultado del examen de una copia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyReport {
    pub header: HeaderState,
    pub entries: EntriesState,
    /// LBA declarada (del disco) de la cabecera. Para un respaldo buscado en el último sector sin
    /// cabecera válida, es esa LBA de la imagen.
    pub disk_lba: u64,
    /// LBA de la imagen donde se leyó la cabecera (`None` si queda fuera de la imagen).
    pub image_lba: Option<u64>,
    /// Campos de la cabecera si tiene firma y CRC-32 correcta (estados `Valid` e `Invalid`).
    pub fields: Option<GptHeader>,
}

impl CopyReport {
    /// Cabecera y entradas válidas.
    pub fn usable(&self) -> bool {
        self.header == HeaderState::Valid && self.entries == EntriesState::Valid
    }

    /// Hay firma `EFI PART` en la posición examinada.
    pub fn has_signature(&self) -> bool {
        matches!(
            self.header,
            HeaderState::Valid | HeaderState::Crc | HeaderState::Invalid
        )
    }
}

/// Una partición GPT con los datos que no caben en [`Partition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptPartition {
    /// `origin` = `Origin::Gpt { index }`, `kind` = `PartitionKind::Gpt(tipo)`, LBA del disco.
    pub partition: Partition,
    /// GUID único de la partición, en el orden de bytes del disco.
    pub unique_guid: [u8; 16],
    /// Atributos de 64 bits tal como están en la entrada.
    pub attributes: u64,
}

/// Resultado de [`read_gpt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptRead {
    /// Tamaño de sector con que se interpretó la GPT (todas las LBA están en estas unidades).
    pub sector_size: u32,
    /// Tamaño de sector lógico del dispositivo.
    pub device_sector_size: u32,
    pub primary: CopyReport,
    pub backup: CopyReport,
    /// Copia usada para las particiones (`None`: ninguna es utilizable).
    pub selected: Option<GptCopy>,
    /// `LBA disco = LBA imagen + shift` (sectores que faltan al inicio; negativo: sobran).
    pub shift: i64,
    /// Sectores del disco que faltan al final de la imagen.
    pub trailing_sectors_missing: u64,
    /// Entradas usadas de la copia elegida, en orden de índice.
    pub partitions: Vec<GptPartition>,
    /// Anomalías de la tabla en conjunto (las de cada partición van en la partición).
    pub anomalies: Vec<Anomaly>,
    /// Confianza en la tabla elegida.
    pub confidence: Confidence,
}

impl GptRead {
    /// Cabecera de la copia elegida.
    pub fn header(&self) -> Option<&GptHeader> {
        match self.selected? {
            GptCopy::Primary => self.primary.fields.as_ref(),
            GptCopy::Backup => self.backup.fields.as_ref(),
        }
    }

    /// `true` si alguna de las dos posiciones examinadas tiene la firma `EFI PART`.
    pub fn signature_found(&self) -> bool {
        self.primary.has_signature() || self.backup.has_signature()
    }

    /// Anomalías GPT de la tabla (sin las de sectores ilegibles).
    pub fn gpt_anomalies(&self) -> impl Iterator<Item = &GptAnomaly> + '_ {
        self.anomalies.iter().filter_map(|a| match a {
            Anomaly::Gpt(g) => Some(g),
            _ => None,
        })
    }

    /// LBA de la imagen que corresponde a una LBA del disco (`None` si cae antes del inicio de la
    /// imagen). No comprueba el final.
    pub fn image_lba(&self, disk_lba: u64) -> Option<u64> {
        u64::try_from(i128::from(disk_lba) - i128::from(self.shift)).ok()
    }
}

/// Lee la GPT del dispositivo.
///
/// - Lee la cabecera primaria en la LBA 1 y, si es válida, la de respaldo en su `AlternateLBA`; si
///   no, busca la de respaldo en la última LBA y, si su `MyLBA` no coincide con esa posición,
///   deduce el desplazamiento de la imagen ([`GptRead::shift`]) y vuelve a buscar la primaria donde
///   le corresponde (`1 - shift`).
/// - Usa la primaria si su cabecera y sus entradas son válidas; si no, la de respaldo
///   ([`GptAnomaly::BackupInUse`]); si tampoco, ninguna ([`GptRead::selected`] = `None`).
/// - Interpreta con el tamaño de sector del dispositivo y, si no aparece ninguna firma ni hay
///   sectores ilegibles en las posiciones examinadas, prueba los otros de [`PROBED_SECTOR_SIZES`];
///   si así encuentra una GPT utilizable la devuelve con [`GptAnomaly::SectorSizeMismatch`].
///
/// Devuelve error solo si falla la E/S de forma no recuperable; los sectores ilegibles se informan
/// como [`Anomaly::UnreadableSector`] y nunca se interpretan.
pub fn read_gpt(dev: &(impl BlockDevice + ?Sized)) -> Result<GptRead> {
    let device = dev.sector_size();
    let first = analyze(dev, device)?;
    if first.selected.is_some() || first.signature_found() || has_unreadable(&first) {
        return Ok(first);
    }
    for used in PROBED_SECTOR_SIZES {
        if used == device {
            continue;
        }
        let mut alt = analyze(dev, used)?;
        if alt.selected.is_some() {
            alt.anomalies
                .push(Anomaly::Gpt(GptAnomaly::SectorSizeMismatch {
                    device,
                    used,
                }));
            alt.confidence = table_confidence(&alt);
            return Ok(alt);
        }
    }
    Ok(first)
}

fn has_unreadable(r: &GptRead) -> bool {
    r.primary.header == HeaderState::Unreadable || r.backup.header == HeaderState::Unreadable
}

/// Lectura de sectores de `ss` bytes sobre un dispositivo con otro tamaño de sector lógico
/// (ambos potencias de 2), siempre a través de [`read_trusted`].
struct View<'a, D: ?Sized> {
    dev: &'a D,
    ss: u32,
    dss: u32,
    /// Sectores de `ss` bytes completos en el dispositivo.
    count: u64,
}

impl<'a, D: BlockDevice + ?Sized> View<'a, D> {
    fn new(dev: &'a D, ss: u32) -> Self {
        let dss = dev.sector_size();
        let bytes = dev.sector_count().saturating_mul(u64::from(dss));
        Self {
            dev,
            ss,
            dss,
            count: bytes / u64::from(ss),
        }
    }

    /// Lee `count` sectores de `ss` bytes desde `lba` (deben existir). `None` si alguno de los
    /// sectores del dispositivo implicados no es fiable (cada uno se añade como anomalía).
    fn read(&self, lba: u64, count: u64, anomalies: &mut Vec<Anomaly>) -> Result<Option<Vec<u8>>> {
        let ss = u64::from(self.ss);
        let dss = u64::from(self.dss);
        let range_err = || mr_io::IoError::InvalidRange {
            lba,
            count,
            total: self.count,
        };
        let start = lba.checked_mul(ss).ok_or_else(range_err)?;
        let len = count.checked_mul(ss).ok_or_else(range_err)?;
        let end = start.checked_add(len).ok_or_else(range_err)?;
        let dev_first = start / dss;
        let dev_end = end.div_ceil(dss);
        let total = usize::try_from((dev_end - dev_first) * dss).map_err(|_| range_err())?;
        let mut buf = Vec::with_capacity(total);
        let mut ok = true;
        let mut at = dev_first;
        while at < dev_end {
            let n = (dev_end - at).min(MAX_TRUSTED_SECTORS);
            let t = read_trusted(self.dev, at, n, READ_RETRIES)?;
            match t.all() {
                Some(d) if ok => buf.extend_from_slice(d),
                Some(_) => {}
                None => {
                    ok = false;
                    anomalies.extend(t.untrusted().map(|lba| Anomaly::UnreadableSector { lba }));
                }
            }
            at += n;
        }
        if !ok {
            return Ok(None);
        }
        let skip = usize::try_from(start - dev_first * dss).map_err(|_| range_err())?;
        let len = usize::try_from(len).map_err(|_| range_err())?;
        Ok(buf.get(skip..skip + len).map(<[u8]>::to_vec))
    }
}

/// Resultado del sondeo de una cabecera.
struct Probe {
    report: CopyReport,
    anomalies: Vec<Anomaly>,
}

impl Probe {
    fn done(report: CopyReport, anomalies: Vec<Anomaly>) -> Result<Self> {
        Ok(Self { report, anomalies })
    }
}

/// Qué se exige a la cabecera según su papel.
#[derive(Clone, Copy)]
enum Expect {
    /// Primaria: `MyLBA` = 1.
    Primary,
    /// Respaldo en una posición conocida: `MyLBA` = esa posición del disco, `AlternateLBA` = 1.
    BackupAt,
    /// Respaldo buscado en la última LBA: `AlternateLBA` = 1; `MyLBA` libre (da el desplazamiento).
    BackupFree,
}

/// LBA de la imagen de `disk_lba` si existe en la imagen.
fn image_of(disk_lba: u64, shift: i64, image_sectors: u64) -> Option<u64> {
    u64::try_from(i128::from(disk_lba) - i128::from(shift))
        .ok()
        .filter(|&l| l < image_sectors)
}

/// Examina la cabecera de `copy` que debe estar en la LBA del disco `disk_lba` (en la imagen,
/// `disk_lba - shift`).
fn probe_header<D: BlockDevice + ?Sized>(
    v: &View<'_, D>,
    copy: GptCopy,
    disk_lba: u64,
    shift: i64,
    expect: Expect,
) -> Result<Probe> {
    let mut an = Vec::new();
    let mut report = CopyReport {
        header: HeaderState::OutsideImage,
        entries: EntriesState::Unchecked,
        disk_lba,
        image_lba: None,
        fields: None,
    };
    let Some(lba) = image_of(disk_lba, shift, v.count) else {
        an.push(Anomaly::Gpt(GptAnomaly::HeaderOutsideImage {
            copy,
            disk_lba,
        }));
        return Probe::done(report, an);
    };
    report.image_lba = Some(lba);
    let Some(sec) = v.read(lba, 1, &mut an)? else {
        report.header = HeaderState::Unreadable;
        return Probe::done(report, an);
    };
    let Some(h) = GptHeader::parse(&sec) else {
        report.header = HeaderState::Missing;
        an.push(Anomaly::Gpt(GptAnomaly::HeaderMissing { copy, lba }));
        return Probe::done(report, an);
    };
    if h.header_size < MIN_HEADER_SIZE || h.header_size > v.ss {
        report.header = HeaderState::Crc;
        an.push(Anomaly::Gpt(GptAnomaly::HeaderSize {
            copy,
            lba,
            size: h.header_size,
        }));
        return Probe::done(report, an);
    }
    let computed = header_crc(&sec[..h.header_size as usize]);
    if computed != h.header_crc32 {
        report.header = HeaderState::Crc;
        an.push(Anomaly::Gpt(GptAnomaly::HeaderCrc {
            copy,
            lba,
            stored: h.header_crc32,
            computed,
        }));
        return Probe::done(report, an);
    }
    report.fields = Some(h);
    let problems = header_problems(&h, copy, lba, disk_lba, expect, v.ss);
    let fatal = problems
        .iter()
        .any(|p| !matches!(p, GptAnomaly::RevisionMinor { .. }));
    an.extend(problems.into_iter().map(Anomaly::Gpt));
    report.header = if fatal {
        HeaderState::Invalid
    } else {
        HeaderState::Valid
    };
    Probe::done(report, an)
}

/// Comprobaciones de coherencia de una cabecera con CRC-32 correcta (UEFI 5.3.2 y disposición de
/// 5.3.1). Todas salvo `RevisionMinor` invalidan la cabecera.
fn header_problems(
    h: &GptHeader,
    copy: GptCopy,
    lba: u64,
    disk_lba: u64,
    expect: Expect,
    ss: u32,
) -> Vec<GptAnomaly> {
    let mut p = Vec::new();
    if h.revision >> 16 != 1 {
        p.push(GptAnomaly::RevisionUnsupported {
            copy,
            lba,
            revision: h.revision,
        });
    } else if h.revision != REVISION_1_0 {
        p.push(GptAnomaly::RevisionMinor {
            copy,
            lba,
            revision: h.revision,
        });
    }
    let expected_my = match expect {
        Expect::Primary => Some(1),
        Expect::BackupAt => Some(disk_lba),
        Expect::BackupFree => None,
    };
    if let Some(e) = expected_my.filter(|&e| e != h.my_lba) {
        p.push(GptAnomaly::MyLbaMismatch {
            copy,
            lba,
            my_lba: h.my_lba,
            expected: e,
        });
    }
    let limits_ok = h.entry_size >= MIN_ENTRY_SIZE
        && h.entry_size <= MAX_ENTRY_SIZE
        && h.entry_size.is_power_of_two()
        && h.entry_count <= MAX_ENTRY_COUNT
        && h.array_bytes() <= MAX_ENTRY_ARRAY_BYTES;
    if !limits_ok {
        p.push(GptAnomaly::EntryArrayLimits {
            copy,
            lba,
            entry_count: h.entry_count,
            entry_size: h.entry_size,
        });
    }
    let (first, last) = (h.first_usable_lba, h.last_usable_lba);
    let entries_end = h
        .entries_lba
        .checked_add(h.array_bytes().div_ceil(u64::from(ss)));
    let (alternate_ok, range_ok, placed) = match expect {
        Expect::Primary => (
            h.alternate_lba > last && h.alternate_lba > 1,
            first <= last && first > h.my_lba,
            // Tabla después de la cabecera y antes del rango utilizable.
            h.entries_lba > h.my_lba && entries_end.is_some_and(|e| e <= first),
        ),
        Expect::BackupAt | Expect::BackupFree => (
            h.alternate_lba == 1,
            first <= last && last < h.my_lba && first > 1,
            // Tabla después del rango utilizable y antes de la cabecera.
            h.entries_lba > last && entries_end.is_some_and(|e| e <= h.my_lba),
        ),
    };
    if !alternate_ok {
        p.push(GptAnomaly::AlternateLbaMismatch {
            copy,
            lba,
            alternate_lba: h.alternate_lba,
        });
    }
    if !range_ok {
        p.push(GptAnomaly::UsableRangeInvalid {
            copy,
            lba,
            first,
            last,
        });
    }
    if limits_ok && !placed {
        p.push(GptAnomaly::EntriesLocationInvalid {
            copy,
            lba,
            entries_lba: h.entries_lba,
        });
    }
    p
}

/// CRC-32 de la cabecera con el propio campo de CRC (bytes 16 a 19) a cero.
fn header_crc(raw: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&raw[..16]);
    h.update(&[0u8; 4]);
    h.update(&raw[20..]);
    h.finalize()
}

/// Lee y comprueba la tabla de entradas de una copia con cabecera válida. Devuelve la tabla si su
/// CRC-32 es correcta.
fn check_entries<D: BlockDevice + ?Sized>(
    v: &View<'_, D>,
    copy: GptCopy,
    report: &mut CopyReport,
    shift: i64,
    an: &mut Vec<Anomaly>,
) -> Result<Option<Vec<u8>>> {
    let Some(h) = report
        .fields
        .filter(|_| report.header == HeaderState::Valid)
    else {
        report.entries = EntriesState::Unchecked;
        return Ok(None);
    };
    // `header_problems` ya limitó el tamaño a MAX_ENTRY_ARRAY_BYTES.
    let bytes = h.array_bytes();
    let sectors = bytes.div_ceil(u64::from(v.ss));
    let start = image_of(h.entries_lba, shift, v.count);
    let end_ok = |s: u64| s.checked_add(sectors).is_some_and(|e| e <= v.count);
    let Some(lba) = start.filter(|&s| end_ok(s)) else {
        report.entries = EntriesState::OutsideImage;
        an.push(Anomaly::Gpt(GptAnomaly::EntriesOutsideImage {
            copy,
            disk_lba: h.entries_lba,
        }));
        return Ok(None);
    };
    let raw = if sectors == 0 {
        Some(Vec::new())
    } else {
        v.read(lba, sectors, an)?
    };
    let Some(mut raw) = raw else {
        report.entries = EntriesState::Unreadable;
        return Ok(None);
    };
    raw.truncate(usize::try_from(bytes).unwrap_or(usize::MAX));
    let computed = crc32fast::hash(&raw);
    if computed != h.entries_crc32 {
        report.entries = EntriesState::Crc;
        an.push(Anomaly::Gpt(GptAnomaly::EntriesCrc {
            copy,
            lba,
            stored: h.entries_crc32,
            computed,
        }));
        return Ok(None);
    }
    report.entries = EntriesState::Valid;
    Ok(Some(raw))
}

/// Análisis completo con sectores de `ss` bytes.
fn analyze<D: BlockDevice + ?Sized>(dev: &D, ss: u32) -> Result<GptRead> {
    let v = View::new(dev, ss);
    let mut anomalies = Vec::new();
    let mut shift: i64 = 0;
    let mut p = probe_header(&v, GptCopy::Primary, 1, 0, Expect::Primary)?;
    let mut b;
    if p.report.header == HeaderState::Valid {
        let alt = p.report.fields.map_or(0, |h| h.alternate_lba);
        b = probe_header(&v, GptCopy::Backup, alt, 0, Expect::BackupAt)?;
    } else {
        let last = v.count.saturating_sub(1);
        b = probe_header(&v, GptCopy::Backup, last, 0, Expect::BackupFree)?;
        let my = b.report.fields.map(|h| h.my_lba);
        if let Some(my) = my.filter(|_| b.report.header == HeaderState::Valid) {
            b.report.disk_lba = my;
            if my != last {
                // my > último sector utilizable > 1 está garantizado; la resta cabe en i128.
                match i64::try_from(i128::from(my) - i128::from(last)) {
                    Ok(s) => {
                        shift = s;
                        // Los sectores ilegibles del primer intento son hechos de E/S: se
                        // conservan; el estado de la primaria se sustituye por el nuevo sondeo.
                        anomalies.extend(
                            p.anomalies
                                .drain(..)
                                .filter(|a| matches!(a, Anomaly::UnreadableSector { .. })),
                        );
                        p = probe_header(&v, GptCopy::Primary, 1, shift, Expect::Primary)?;
                    }
                    Err(_) => {
                        b.report.header = HeaderState::Invalid;
                        b.anomalies.push(Anomaly::Gpt(GptAnomaly::MyLbaMismatch {
                            copy: GptCopy::Backup,
                            lba: last,
                            my_lba: my,
                            expected: last,
                        }));
                    }
                }
            }
        }
    }
    anomalies.append(&mut p.anomalies);
    let primary_table = check_entries(&v, GptCopy::Primary, &mut p.report, shift, &mut anomalies)?;
    anomalies.append(&mut b.anomalies);
    let backup_table = check_entries(&v, GptCopy::Backup, &mut b.report, shift, &mut anomalies)?;
    let (primary, backup) = (p.report, b.report);

    let (selected, table) = if primary.usable() {
        (Some(GptCopy::Primary), primary_table)
    } else if backup.usable() {
        anomalies.push(Anomaly::Gpt(GptAnomaly::BackupInUse {
            primary_header: primary.header,
            primary_entries: primary.entries,
        }));
        (Some(GptCopy::Backup), backup_table)
    } else {
        (None, None)
    };

    if let (HeaderState::Valid, HeaderState::Valid, Some(a), Some(c)) =
        (primary.header, backup.header, primary.fields, backup.fields)
    {
        anomalies.extend(
            disagreements(&a, &c)
                .into_iter()
                .map(|field| Anomaly::Gpt(GptAnomaly::CopiesDisagree { field })),
        );
    }
    if shift != 0 {
        anomalies.push(Anomaly::Gpt(GptAnomaly::ImageShifted { offset: shift }));
    }

    let mut trailing = 0;
    let mut partitions = Vec::new();
    let chosen = match selected {
        Some(GptCopy::Primary) => Some(&primary),
        Some(GptCopy::Backup) => Some(&backup),
        None => None,
    };
    if let Some((rep, h)) = chosen.and_then(|r| r.fields.map(|h| (r, h))) {
        let last_image = i128::from(v.count) - 1;
        // Última LBA del disco según la cabecera elegida.
        let disk_last = if selected == Some(GptCopy::Primary) {
            h.alternate_lba
        } else {
            h.my_lba
        };
        let image_last = i128::from(disk_last) - i128::from(shift);
        if image_last > last_image {
            trailing = u64::try_from(image_last - last_image).unwrap_or(u64::MAX);
            anomalies.push(Anomaly::Gpt(GptAnomaly::ImageTruncated {
                missing: trailing,
            }));
        }
        if image_of(h.first_usable_lba, shift, v.count).is_none()
            || image_of(h.last_usable_lba, shift, v.count).is_none()
        {
            anomalies.push(Anomaly::Gpt(GptAnomaly::UsableRangeOutsideImage {
                lba: rep.image_lba.unwrap_or(0),
                first: h.first_usable_lba,
                last: h.last_usable_lba,
            }));
        }
        if let Some(t) = table.as_deref() {
            partitions = parse_entries(t, &h, shift, v.count);
        }
    }

    let mut read = GptRead {
        sector_size: ss,
        device_sector_size: dev.sector_size(),
        primary,
        backup,
        selected,
        shift,
        trailing_sectors_missing: trailing,
        partitions,
        anomalies,
        confidence: Confidence::Valid,
    };
    read.confidence = table_confidence(&read);
    Ok(read)
}

fn disagreements(a: &GptHeader, b: &GptHeader) -> Vec<GptField> {
    let mut d = Vec::new();
    if a.disk_guid != b.disk_guid {
        d.push(GptField::DiskGuid);
    }
    if (a.first_usable_lba, a.last_usable_lba) != (b.first_usable_lba, b.last_usable_lba) {
        d.push(GptField::UsableRange);
    }
    if a.entry_count != b.entry_count {
        d.push(GptField::EntryCount);
    }
    if a.entry_size != b.entry_size {
        d.push(GptField::EntrySize);
    }
    if a.entries_crc32 != b.entries_crc32 {
        d.push(GptField::EntriesCrc32);
    }
    if a.alternate_lba != b.my_lba {
        d.push(GptField::Location);
    }
    d
}

/// Confianza de la tabla: inválida sin copia utilizable; sospechosa con desplazamiento, truncado,
/// desacuerdo entre copias, tamaño de sector distinto o revisión menor desconocida; válida con
/// correcciones si solo se ha descartado (o no se ha podido leer) una copia; válida si no hay nada
/// que señalar.
fn table_confidence(r: &GptRead) -> Confidence {
    if r.selected.is_none() {
        return Confidence::Invalid("ninguna copia de la GPT es utilizable".into());
    }
    if r.anomalies.is_empty() {
        return Confidence::Valid;
    }
    let mut suspicious = Vec::new();
    let mut fixes = Vec::new();
    for a in &r.anomalies {
        let serious = matches!(
            a,
            Anomaly::Gpt(
                GptAnomaly::CopiesDisagree { .. }
                    | GptAnomaly::ImageShifted { .. }
                    | GptAnomaly::ImageTruncated { .. }
                    | GptAnomaly::UsableRangeOutsideImage { .. }
                    | GptAnomaly::SectorSizeMismatch { .. }
                    | GptAnomaly::RevisionMinor { .. }
            )
        );
        if serious {
            suspicious.push(describe(a));
        } else {
            fixes.push(describe(a));
        }
    }
    if suspicious.is_empty() {
        Confidence::ValidWithFixes(fixes)
    } else {
        suspicious.extend(fixes);
        Confidence::Suspicious(suspicious)
    }
}

fn describe(a: &Anomaly) -> String {
    match a {
        Anomaly::Gpt(g) => g.to_string(),
        Anomaly::UnreadableSector { lba } => format!("sector {lba} ilegible"),
        other => format!("{other:?}"),
    }
}

/// Interpreta las entradas usadas (GUID de tipo distinto de cero) de una tabla con CRC correcta.
fn parse_entries(table: &[u8], h: &GptHeader, shift: i64, image_sectors: u64) -> Vec<GptPartition> {
    let size = h.entry_size as usize;
    let mut out: Vec<GptPartition> = Vec::new();
    for (i, e) in table
        .chunks_exact(size)
        .take(h.entry_count as usize)
        .enumerate()
    {
        let mut type_guid = [0u8; 16];
        type_guid.copy_from_slice(&e[0..16]);
        if type_guid == [0; 16] {
            continue;
        }
        // i < MAX_ENTRY_COUNT, cabe en u32.
        let index = u32::try_from(i).unwrap_or(u32::MAX);
        let mut unique_guid = [0u8; 16];
        unique_guid.copy_from_slice(&e[16..32]);
        let first = le64(e, 32);
        let last = le64(e, 40);
        let attributes = le64(e, 48);
        let mut anomalies = Vec::new();
        let (name, name_ok) = decode_name(&e[56..128]);
        if !name_ok {
            anomalies.push(GptAnomaly::NameInvalid { index });
        }
        let sector_count = if first <= last {
            if first < h.first_usable_lba || last > h.last_usable_lba {
                anomalies.push(GptAnomaly::EntryOutsideUsable { index });
            }
            if image_of(first, shift, image_sectors).is_none()
                || image_of(last, shift, image_sectors).is_none()
            {
                anomalies.push(GptAnomaly::EntryOutsideImage { index });
            }
            (last - first).saturating_add(1)
        } else {
            anomalies.push(GptAnomaly::EntryInverted { index, first, last });
            0
        };
        out.push(GptPartition {
            partition: Partition {
                origin: Origin::Gpt { index },
                start_lba: first,
                sector_count,
                kind: PartitionKind::Gpt(type_guid),
                name: Some(name),
                active: attributes & ATTR_LEGACY_BIOS_BOOTABLE != 0,
                confidence: Confidence::Valid,
                anomalies: anomalies.into_iter().map(Anomaly::Gpt).collect(),
            },
            unique_guid,
            attributes,
        });
    }
    mark_overlaps(&mut out);
    mark_duplicate_guids(&mut out);
    for p in &mut out {
        p.partition.confidence = partition_confidence(&p.partition.anomalies);
    }
    out
}

/// Señala los solapes en las dos entradas de cada par. Barrido por inicio: cada entrada se compara
/// con la de mayor final vista hasta entonces (O(n log n); con varios solapes encadenados se
/// informa al menos uno por entrada solapada).
fn mark_overlaps(parts: &mut [GptPartition]) {
    let mut order: Vec<usize> = (0..parts.len())
        .filter(|&i| parts[i].partition.sector_count > 0)
        .collect();
    order.sort_by_key(|&i| (parts[i].partition.start_lba, i));
    let mut reach: Option<(u64, usize)> = None;
    let mut found = Vec::new();
    for &i in &order {
        let p = &parts[i].partition;
        let end = p.start_lba.saturating_add(p.sector_count - 1);
        if let Some((r_end, r)) = reach {
            if p.start_lba <= r_end {
                found.push((i, r));
            }
        }
        if reach.is_none_or(|(r_end, _)| end > r_end) {
            reach = Some((end, i));
        }
    }
    for (i, r) in found {
        let (a, b) = (index_of(&parts[i]), index_of(&parts[r]));
        parts[i]
            .partition
            .anomalies
            .push(Anomaly::Gpt(GptAnomaly::EntryOverlap {
                index: a,
                other: b,
            }));
        parts[r]
            .partition
            .anomalies
            .push(Anomaly::Gpt(GptAnomaly::EntryOverlap {
                index: b,
                other: a,
            }));
    }
}

fn mark_duplicate_guids(parts: &mut [GptPartition]) {
    let mut seen: HashMap<[u8; 16], u32> = HashMap::new();
    for p in parts.iter_mut() {
        let index = index_of(p);
        if let Some(&other) = seen.get(&p.unique_guid) {
            p.partition
                .anomalies
                .push(Anomaly::Gpt(GptAnomaly::DuplicateUniqueGuid {
                    index,
                    other,
                }));
        } else {
            seen.insert(p.unique_guid, index);
        }
    }
}

fn index_of(p: &GptPartition) -> u32 {
    match p.partition.origin {
        Origin::Gpt { index } => index,
        _ => u32::MAX,
    }
}

fn partition_confidence(anomalies: &[Anomaly]) -> Confidence {
    if anomalies.is_empty() {
        return Confidence::Valid;
    }
    let texts: Vec<String> = anomalies.iter().map(describe).collect();
    if anomalies
        .iter()
        .any(|a| matches!(a, Anomaly::Gpt(GptAnomaly::EntryInverted { .. })))
    {
        Confidence::Invalid(texts.join("; "))
    } else {
        Confidence::Suspicious(texts)
    }
}

/// Decodifica un nombre UTF-16LE hasta el primer NUL. El segundo valor es `false` si había
/// sustitutos mal emparejados (sustituidos por U+FFFD).
pub fn decode_name(raw: &[u8]) -> (String, bool) {
    let units = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0);
    let mut ok = true;
    let name = char::decode_utf16(units)
        .map(|r| {
            r.unwrap_or_else(|_| {
                ok = false;
                char::REPLACEMENT_CHARACTER
            })
        })
        .collect();
    (name, ok)
}

/// Texto canónico de un GUID guardado en el orden de bytes del disco (los tres primeros campos en
/// little-endian), p. ej. `c12a7328-f81f-11d2-ba4b-00a0c93ec93b`.
pub fn format_guid(g: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

fn le32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice};

    const BASIC_DATA: [u8; 16] = [
        0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99,
        0xc7,
    ];
    const LINUX: [u8; 16] = [
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ];

    #[derive(Clone)]
    struct Entry {
        ty: [u8; 16],
        uid: [u8; 16],
        first: u64,
        last: u64,
        attrs: u64,
        name: Vec<u16>,
    }

    fn entry(ty: [u8; 16], uid: u8, first: u64, last: u64, name: &str) -> Entry {
        Entry {
            ty,
            uid: [uid; 16],
            first,
            last,
            attrs: 0,
            name: name.encode_utf16().collect(),
        }
    }

    struct Layout {
        ss: usize,
        n: u64,
        count: u32,
        esize: u32,
    }

    impl Layout {
        fn es(&self) -> u64 {
            (u64::from(self.count) * u64::from(self.esize)).div_ceil(self.ss as u64)
        }
        fn first(&self) -> u64 {
            2 + self.es()
        }
        fn last(&self) -> u64 {
            self.n - 2 - self.es()
        }
        fn backup_entries(&self) -> u64 {
            self.n - 1 - self.es()
        }
    }

    /// Disco GPT sintético según UEFI: cabecera primaria en 1, entradas en 2, copia al final.
    fn build(l: &Layout, parts: &[Entry]) -> Vec<u8> {
        let mut d = vec![0u8; l.ss * l.n as usize];
        let mut arr = vec![0u8; (l.count * l.esize) as usize];
        for (i, p) in parts.iter().enumerate() {
            let e = &mut arr[i * l.esize as usize..];
            e[0..16].copy_from_slice(&p.ty);
            e[16..32].copy_from_slice(&p.uid);
            e[32..40].copy_from_slice(&p.first.to_le_bytes());
            e[40..48].copy_from_slice(&p.last.to_le_bytes());
            e[48..56].copy_from_slice(&p.attrs.to_le_bytes());
            for (k, u) in p.name.iter().take(36).enumerate() {
                e[56 + 2 * k..58 + 2 * k].copy_from_slice(&u.to_le_bytes());
            }
        }
        let crc = crc32fast::hash(&arr);
        for at in [2, l.backup_entries()] {
            let o = at as usize * l.ss;
            d[o..o + arr.len()].copy_from_slice(&arr);
        }
        let back = l.n - 1;
        write_header(&mut d, l, 1, back, 2, crc);
        write_header(&mut d, l, back, 1, l.backup_entries(), crc);
        d
    }

    fn write_header(d: &mut [u8], l: &Layout, my: u64, alt: u64, entries: u64, ecrc: u32) {
        let o = my as usize * l.ss;
        let h = &mut d[o..o + l.ss];
        h.fill(0);
        h[0..8].copy_from_slice(SIGNATURE);
        h[8..12].copy_from_slice(&REVISION_1_0.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&l.first().to_le_bytes());
        h[48..56].copy_from_slice(&l.last().to_le_bytes());
        h[56..72].copy_from_slice(&[0x5a; 16]);
        h[72..80].copy_from_slice(&entries.to_le_bytes());
        h[80..84].copy_from_slice(&l.count.to_le_bytes());
        h[84..88].copy_from_slice(&l.esize.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        fix_header_crc(d, l.ss, my);
    }

    /// Recalcula la CRC-32 de la cabecera de `lba` (tamaño de cabecera acotado al sector).
    fn fix_header_crc(d: &mut [u8], ss: usize, lba: u64) {
        let o = lba as usize * ss;
        let h = &mut d[o..o + ss];
        let size = (le32(h, 12) as usize).clamp(92, ss);
        h[16..20].fill(0);
        let crc = crc32fast::hash(&h[..size]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
    }

    /// Modifica la cabecera de `lba` y recalcula su CRC-32.
    fn patch(d: &mut [u8], ss: usize, lba: u64, at: usize, bytes: &[u8]) {
        let o = lba as usize * ss + at;
        d[o..o + bytes.len()].copy_from_slice(bytes);
        fix_header_crc(d, ss, lba);
    }

    fn two() -> Vec<Entry> {
        vec![
            entry(BASIC_DATA, 1, 2048, 3071, "MR-DATA"),
            entry(LINUX, 2, 3072, 3583, "MR-LINUX"),
        ]
    }

    fn l512() -> Layout {
        Layout {
            ss: 512,
            n: 4096,
            count: 128,
            esize: 128,
        }
    }

    fn l4k() -> Layout {
        Layout {
            ss: 4096,
            n: 512,
            count: 128,
            esize: 128,
        }
    }

    fn dev(d: Vec<u8>, ss: u32) -> MemDevice {
        MemDevice::new(d, ss).unwrap()
    }

    fn gpt_codes(r: &GptRead) -> Vec<GptAnomaly> {
        r.gpt_anomalies().cloned().collect()
    }

    fn has(r: &GptRead, f: impl Fn(&GptAnomaly) -> bool) -> bool {
        r.gpt_anomalies().any(f)
    }

    #[test]
    fn healthy_512_uses_primary() {
        let l = l512();
        let mut parts = two();
        parts[0].attrs = ATTR_LEGACY_BIOS_BOOTABLE | (1 << 60);
        let r = read_gpt(&dev(build(&l, &parts), 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.confidence, Confidence::Valid);
        assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
        assert_eq!(
            (r.sector_size, r.shift, r.trailing_sectors_missing),
            (512, 0, 0)
        );
        assert_eq!(r.primary.entries, EntriesState::Valid);
        assert_eq!(r.backup.header, HeaderState::Valid);
        assert_eq!(r.backup.image_lba, Some(4095));
        assert_eq!(r.partitions.len(), 2);
        let p = &r.partitions[0];
        assert_eq!(p.partition.origin, Origin::Gpt { index: 0 });
        assert_eq!(
            (p.partition.start_lba, p.partition.sector_count),
            (2048, 1024)
        );
        assert_eq!(p.partition.kind, PartitionKind::Gpt(BASIC_DATA));
        assert_eq!(p.partition.name.as_deref(), Some("MR-DATA"));
        assert!(p.partition.active);
        assert_eq!(p.attributes, ATTR_LEGACY_BIOS_BOOTABLE | (1 << 60));
        assert_eq!(p.unique_guid, [1; 16]);
        assert_eq!(p.partition.confidence, Confidence::Valid);
        assert!(!r.partitions[1].partition.active);
        assert_eq!(r.header().unwrap().first_usable_lba, 34);
    }

    #[test]
    fn healthy_4kn() {
        let parts = vec![entry(BASIC_DATA, 1, 256, 383, "MR-4KN")];
        let r = read_gpt(&dev(build(&l4k(), &parts), 4096)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.sector_size, 4096);
        assert_eq!(r.header().unwrap().first_usable_lba, 6);
        assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
        assert_eq!(r.partitions[0].partition.sector_count, 128);
    }

    #[test]
    fn other_sector_size_is_probed_and_reported() {
        // GPT de 4096 B en un dispositivo declarado de 512 B.
        let parts = vec![entry(BASIC_DATA, 1, 256, 383, "MR-4KN")];
        let r = read_gpt(&dev(build(&l4k(), &parts), 512)).unwrap();
        assert_eq!((r.sector_size, r.device_sector_size), (4096, 512));
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert!(has(&r, |a| *a
            == GptAnomaly::SectorSizeMismatch {
                device: 512,
                used: 4096
            }));
        assert!(matches!(r.confidence, Confidence::Suspicious(_)));
        assert_eq!(r.partitions[0].partition.start_lba, 256);
        // Y al revés: GPT de 512 B en un dispositivo de 4096 B.
        let r = read_gpt(&dev(build(&l512(), &two()), 4096)).unwrap();
        assert_eq!((r.sector_size, r.device_sector_size), (512, 4096));
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.partitions.len(), 2);
    }

    #[test]
    fn primary_header_crc_falls_back_to_backup() {
        let l = l512();
        let mut d = build(&l, &two());
        d[512 + 40] ^= 1;
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Crc);
        assert_eq!(r.primary.entries, EntriesState::Unchecked);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::HeaderCrc {
                copy: GptCopy::Primary,
                lba: 1,
                ..
            }
        )));
        assert!(has(&r, |a| *a
            == GptAnomaly::BackupInUse {
                primary_header: HeaderState::Crc,
                primary_entries: EntriesState::Unchecked
            }));
        assert!(matches!(r.confidence, Confidence::ValidWithFixes(_)));
        assert_eq!(r.partitions.len(), 2);
    }

    #[test]
    fn header_size_out_of_range_is_crc_state() {
        for size in [0u32, 91, 513, u32::MAX] {
            let mut d = build(&l512(), &two());
            patch(&mut d, 512, 1, 12, &size.to_le_bytes());
            let r = read_gpt(&dev(d, 512)).unwrap();
            assert_eq!(r.primary.header, HeaderState::Crc, "{size}");
            assert!(has(&r, |a| *a
                == GptAnomaly::HeaderSize {
                    copy: GptCopy::Primary,
                    lba: 1,
                    size
                }));
            assert_eq!(r.selected, Some(GptCopy::Backup));
        }
        // Un tamaño mayor de 92 dentro del sector es válido si la CRC lo cubre.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 12, &512u32.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Valid);
    }

    #[test]
    fn primary_entries_crc_falls_back_to_backup() {
        let mut d = build(&l512(), &two());
        d[2 * 512 + 60] ^= 0xff;
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Valid);
        assert_eq!(r.primary.entries, EntriesState::Crc);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::EntriesCrc {
                copy: GptCopy::Primary,
                lba: 2,
                ..
            }
        )));
        // Las cabeceras siguen de acuerdo (misma CRC declarada): no hay desacuerdo.
        assert!(!has(&r, |a| matches!(a, GptAnomaly::CopiesDisagree { .. })));
        assert_eq!(r.partitions[0].partition.name.as_deref(), Some("MR-DATA"));
    }

    #[test]
    fn both_copies_bad_selects_none() {
        let mut d = build(&l512(), &two());
        d[512 + 40] ^= 1;
        d[4095 * 512..].fill(0);
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, None);
        assert!(r.partitions.is_empty());
        assert_eq!(r.backup.header, HeaderState::Missing);
        assert!(!r.confidence.is_usable());
        assert!(has(&r, |a| *a
            == GptAnomaly::HeaderMissing {
                copy: GptCopy::Backup,
                lba: 4095
            }));
    }

    #[test]
    fn backup_damage_keeps_primary_and_reports() {
        let l = l512();
        let mut d = build(&l, &two());
        d[4095 * 512 + 50] ^= 1;
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.backup.header, HeaderState::Crc);
        assert!(matches!(r.confidence, Confidence::ValidWithFixes(_)));
        let mut d = build(&l, &two());
        d[l.backup_entries() as usize * 512] ^= 1;
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.backup.entries, EntriesState::Crc);
        assert_eq!(r.selected, Some(GptCopy::Primary));
    }

    #[test]
    fn revision_rules() {
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 8, &0x0002_0000u32.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Invalid);
        assert!(r.primary.fields.is_some());
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::RevisionUnsupported { .. }
        )));
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 8, &0x0001_0001u32.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert!(has(&r, |a| matches!(a, GptAnomaly::RevisionMinor { .. })));
        assert!(matches!(r.confidence, Confidence::Suspicious(_)));
    }

    #[test]
    fn my_and_alternate_lba_coherence() {
        // Primaria con MyLBA = 5.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 24, &5u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Invalid);
        assert!(has(&r, |a| *a
            == GptAnomaly::MyLbaMismatch {
                copy: GptCopy::Primary,
                lba: 1,
                my_lba: 5,
                expected: 1
            }));
        assert_eq!(r.selected, Some(GptCopy::Backup));
        // Primaria cuyo AlternateLBA cae dentro del rango utilizable.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 32, &100u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::AlternateLbaMismatch {
                copy: GptCopy::Primary,
                alternate_lba: 100,
                ..
            }
        )));
        assert_eq!(r.selected, Some(GptCopy::Backup));
        // Respaldo que no apunta a la LBA 1.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 4095, 32, &7u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.backup.header, HeaderState::Invalid);
        assert_eq!(r.selected, Some(GptCopy::Primary));
        // AlternateLBA de la primaria apunta a un sector que no es el respaldo.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 32, &4090u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.backup.header, HeaderState::Missing);
        assert_eq!(r.backup.image_lba, Some(4090));
        // Respaldo cuyo MyLBA no coincide con la posición indicada por la primaria.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 4095, 24, &4094u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::MyLbaMismatch {
                copy: GptCopy::Backup,
                ..
            }
        )));
        assert_eq!(r.backup.header, HeaderState::Invalid);
    }

    #[test]
    fn usable_range_and_entries_location() {
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 40, &5000u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::UsableRangeInvalid { .. }
        )));
        assert_eq!(r.primary.header, HeaderState::Invalid);
        // Tabla de entradas dentro del rango utilizable.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 72, &20u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert!(has(&r, |a| *a
            == GptAnomaly::EntriesLocationInvalid {
                copy: GptCopy::Primary,
                lba: 1,
                entries_lba: 20
            }));
        // Tabla de entradas encima de la cabecera.
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 1, 72, &1u64.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.primary.header, HeaderState::Invalid);
        // Tabla de entradas de la copia con LBA enorme (sin desbordar).
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 4095, 72, &u64::MAX.to_le_bytes());
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.backup.header, HeaderState::Invalid);
    }

    #[test]
    fn entry_limits_bound_memory() {
        for (count, size) in [
            (u32::MAX, 128u32),
            (MAX_ENTRY_COUNT + 1, 128),
            (128, 100),
            (128, 192),
            (128, MAX_ENTRY_SIZE * 2),
            (2048, 4096),
            (16, 0),
        ] {
            let mut d = build(&l512(), &two());
            patch(&mut d, 512, 1, 80, &count.to_le_bytes());
            patch(&mut d, 512, 1, 84, &size.to_le_bytes());
            let r = read_gpt(&dev(d, 512)).unwrap();
            assert!(
                has(&r, |a| *a
                    == GptAnomaly::EntryArrayLimits {
                        copy: GptCopy::Primary,
                        lba: 1,
                        entry_count: count,
                        entry_size: size
                    }),
                "{count} x {size}: {:?}",
                gpt_codes(&r)
            );
            assert_eq!(r.primary.header, HeaderState::Invalid);
            assert_eq!(r.primary.entries, EntriesState::Unchecked);
            assert_eq!(r.selected, Some(GptCopy::Backup));
        }
    }

    #[test]
    fn larger_entries_and_zero_entries_are_accepted() {
        let l = Layout {
            ss: 512,
            n: 4096,
            count: 64,
            esize: 256,
        };
        let r = read_gpt(&dev(build(&l, &two()), 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.partitions.len(), 2);
        let l = Layout {
            ss: 512,
            n: 64,
            count: 0,
            esize: 128,
        };
        let r = read_gpt(&dev(build(&l, &[]), 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert!(r.partitions.is_empty());
    }

    #[test]
    fn entry_level_checks() {
        let mut parts = two();
        parts.push(entry(LINUX, 3, 3000, 3100, "SOLAPADA"));
        parts.push(entry(LINUX, 1, 3600, 3700, "GUID REPETIDO"));
        parts.push(entry(LINUX, 5, 3800, 3700, "INVERTIDA"));
        parts.push(entry(LINUX, 6, 10, 20, "FUERA"));
        let mut bad = entry(LINUX, 7, 3900, 3950, "");
        bad.name = vec![0x41, 0xd800, 0x42];
        parts.push(bad);
        let r = read_gpt(&dev(build(&l512(), &parts), 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.partitions.len(), 7);
        let an = |i: usize| -> Vec<GptAnomaly> {
            r.partitions[i]
                .partition
                .anomalies
                .iter()
                .filter_map(|a| match a {
                    Anomaly::Gpt(g) => Some(g.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(an(2).contains(&GptAnomaly::EntryOverlap { index: 2, other: 0 }));
        assert!(an(0).contains(&GptAnomaly::EntryOverlap { index: 0, other: 2 }));
        assert!(an(1)
            .iter()
            .any(|a| matches!(a, GptAnomaly::EntryOverlap { index: 1, .. })));
        assert!(an(3).contains(&GptAnomaly::DuplicateUniqueGuid { index: 3, other: 0 }));
        assert!(an(4).contains(&GptAnomaly::EntryInverted {
            index: 4,
            first: 3800,
            last: 3700
        }));
        assert_eq!(r.partitions[4].partition.sector_count, 0);
        assert!(!r.partitions[4].partition.confidence.is_usable());
        assert!(an(5).contains(&GptAnomaly::EntryOutsideUsable { index: 5 }));
        assert!(an(6).contains(&GptAnomaly::NameInvalid { index: 6 }));
        assert_eq!(
            r.partitions[6].partition.name.as_deref(),
            Some("A\u{fffd}B")
        );
        assert!(matches!(
            r.partitions[5].partition.confidence,
            Confidence::Suspicious(_)
        ));
        // Las anomalías de partición no cambian la confianza de la tabla.
        assert_eq!(r.confidence, Confidence::Valid);
    }

    #[test]
    fn copies_disagree_is_reported() {
        let mut d = build(&l512(), &two());
        patch(&mut d, 512, 4095, 56, &[0x11; 16]);
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(
            gpt_codes(&r),
            vec![GptAnomaly::CopiesDisagree {
                field: GptField::DiskGuid
            }]
        );
        assert!(matches!(r.confidence, Confidence::Suspicious(_)));
    }

    #[test]
    fn shifted_image_reports_offset_and_keeps_disk_lbas() {
        let full = build(&l512(), &two());
        // Faltan 63 sectores: primaria perdida, se usa el respaldo.
        let r = read_gpt(&dev(full[63 * 512..].to_vec(), 512)).unwrap();
        assert_eq!(r.shift, 63);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert_eq!(r.primary.header, HeaderState::OutsideImage);
        assert_eq!(r.backup.image_lba, Some(4032));
        assert_eq!(r.backup.disk_lba, 4095);
        assert!(has(&r, |a| *a == GptAnomaly::ImageShifted { offset: 63 }));
        assert!(has(&r, |a| *a
            == GptAnomaly::HeaderOutsideImage {
                copy: GptCopy::Primary,
                disk_lba: 1
            }));
        assert!(has(&r, |a| matches!(
            a,
            GptAnomaly::UsableRangeOutsideImage { lba: 4032, .. }
        )));
        assert_eq!(r.partitions[0].partition.start_lba, 2048);
        assert_eq!(r.image_lba(2048), Some(1985));
        assert!(matches!(r.confidence, Confidence::Suspicious(_)));
        // Falta un sector: la primaria está en la LBA 0 y se usa.
        let r = read_gpt(&dev(full[512..].to_vec(), 512)).unwrap();
        assert_eq!((r.shift, r.selected), (1, Some(GptCopy::Primary)));
        assert_eq!(r.primary.image_lba, Some(0));
        assert!(!has(&r, |a| matches!(a, GptAnomaly::HeaderMissing { .. })));
        // Sobran 8 sectores al inicio.
        let mut padded = vec![0u8; 8 * 512];
        padded.extend_from_slice(&full);
        let r = read_gpt(&dev(padded, 512)).unwrap();
        assert_eq!((r.shift, r.selected), (-8, Some(GptCopy::Primary)));
        assert_eq!(r.primary.image_lba, Some(9));
        assert_eq!(r.image_lba(2048), Some(2056));
    }

    #[test]
    fn truncated_image_reports_missing_tail() {
        let full = build(&l512(), &two());
        let r = read_gpt(&dev(full[..3896 * 512].to_vec(), 512)).unwrap();
        assert_eq!(r.selected, Some(GptCopy::Primary));
        assert_eq!(r.trailing_sectors_missing, 200);
        assert_eq!(r.backup.header, HeaderState::OutsideImage);
        assert!(has(&r, |a| *a == GptAnomaly::ImageTruncated { missing: 200 }));
        assert!(has(&r, |a| *a
            == GptAnomaly::HeaderOutsideImage {
                copy: GptCopy::Backup,
                disk_lba: 4095
            }));
        // Partición que pasa del final de la imagen.
        let mut parts = two();
        parts.push(entry(LINUX, 9, 3600, 4000, "COLA"));
        let full = build(&l512(), &parts);
        let r = read_gpt(&dev(full[..3896 * 512].to_vec(), 512)).unwrap();
        assert!(r.partitions[2]
            .partition
            .anomalies
            .contains(&Anomaly::Gpt(GptAnomaly::EntryOutsideImage { index: 2 })));
    }

    #[test]
    fn unreadable_sectors_are_never_parsed() {
        let d = build(&l512(), &two());
        let f = FaultyDevice::new(dev(d.clone(), 512)).with_bad([1]);
        let r = read_gpt(&f).unwrap();
        assert_eq!(r.primary.header, HeaderState::Unreadable);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(r.anomalies.contains(&Anomaly::UnreadableSector { lba: 1 }));
        let f = FaultyDevice::new(dev(d.clone(), 512)).with_bad([5]);
        let r = read_gpt(&f).unwrap();
        assert_eq!(r.primary.entries, EntriesState::Unreadable);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(r.anomalies.contains(&Anomaly::UnreadableSector { lba: 5 }));
        let f = FaultyDevice::new(dev(d, 512)).with_bad([1, 4095]);
        let r = read_gpt(&f).unwrap();
        assert_eq!(r.selected, None);
        assert_eq!(r.backup.header, HeaderState::Unreadable);
        // Sin firma visible pero con ilegibles: no se reinterpreta con otro tamaño de sector.
        assert_eq!(r.sector_size, 512);
        // 4096 interpretado sobre 512: basta un sector físico ilegible en la cabecera.
        let d4 = build(&l4k(), &[entry(BASIC_DATA, 1, 256, 383, "X")]);
        let f = FaultyDevice::new(dev(d4, 512)).with_bad([9]);
        let r = read_gpt(&f).unwrap();
        assert_eq!(r.sector_size, 4096);
        assert_eq!(r.selected, Some(GptCopy::Backup));
        assert!(r.anomalies.contains(&Anomaly::UnreadableSector { lba: 9 }));
    }

    #[test]
    fn tiny_and_empty_devices() {
        for n in [1usize, 2, 3, 8] {
            for ss in [512usize, 4096] {
                let r = read_gpt(&dev(vec![0u8; n * ss], ss as u32)).unwrap();
                assert_eq!(r.selected, None);
                assert!(!r.signature_found());
            }
        }
        // Una cabecera sola en LBA 1 de un disco de 2 sectores.
        let mut d = vec![0u8; 2 * 512];
        d[512..520].copy_from_slice(SIGNATURE);
        let r = read_gpt(&dev(d, 512)).unwrap();
        assert_eq!(r.selected, None);
    }

    #[test]
    fn guid_and_name_helpers() {
        assert_eq!(
            format_guid(&BASIC_DATA),
            "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"
        );
        let text = "B\u{e1}sico \u{1f600}";
        let raw: Vec<u8> = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain([0, 0, 0x41, 0])
            .collect();
        assert_eq!(decode_name(&raw), (text.to_string(), true));
        assert_eq!(decode_name(&[0x00, 0xdc]), ("\u{fffd}".to_string(), false));
        assert_eq!(decode_name(&[0x41]), (String::new(), true));
    }

    /// Generador determinista (xorshift64*), sin dependencias.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn seeded_fuzz_never_panics_or_hangs() {
        let start = std::time::Instant::now();
        let l = Layout {
            ss: 512,
            n: 256,
            count: 32,
            esize: 128,
        };
        let parts = vec![
            entry(BASIC_DATA, 1, 40, 99, "A"),
            entry(LINUX, 2, 100, 199, "B"),
        ];
        let base = build(&l, &parts);
        let mut rng = Rng(0x4d52_4750_5431_3301);
        let extremes: [u64; 8] = [0, 1, 2, 255, 256, u64::MAX, u64::MAX / 2, 1 << 40];
        let mut selected = 0;
        for iter in 0..3000 {
            let mut d = base.clone();
            // Sector a alterar: cabeceras, tablas o uno cualquiera.
            let lba = match rng.below(4) {
                0 => 1,
                1 => 255,
                2 => 2 + rng.below(l.es()),
                _ => rng.below(256),
            };
            for _ in 0..1 + rng.below(8) {
                let at = lba as usize * 512 + rng.below(512) as usize;
                d[at] = rng.next() as u8;
            }
            // Campo de cabecera con un valor extremo.
            if rng.below(3) == 0 {
                let h = if rng.below(2) == 0 { 1 } else { 255 };
                let field = [24usize, 32, 40, 48, 72, 80, 84][rng.below(7) as usize];
                let v = extremes[rng.below(8) as usize];
                let o = h * 512 + field;
                if field >= 80 {
                    d[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes());
                } else {
                    d[o..o + 8].copy_from_slice(&v.to_le_bytes());
                }
            }
            // La mitad de las veces se recalculan las CRC para llegar a las reglas profundas.
            if rng.below(2) == 0 {
                for h in [1u64, 255] {
                    let o = h as usize * 512;
                    let count = u64::from(le32(&d[o..], 80));
                    let size = u64::from(le32(&d[o..], 84));
                    let elba = le64(&d[o..], 72);
                    let bytes = count * size;
                    if elba < 256 && bytes <= 64 * 1024 && elba * 512 + bytes <= d.len() as u64 {
                        let s = elba as usize * 512;
                        let crc = crc32fast::hash(&d[s..s + bytes as usize]);
                        d[o + 88..o + 92].copy_from_slice(&crc.to_le_bytes());
                    }
                    fix_header_crc(&mut d, 512, h);
                }
            }
            // Recortes o desplazamientos ocasionales.
            let d = match rng.below(6) {
                0 => d[512 * (1 + rng.below(64)) as usize..].to_vec(),
                1 => d[..512 * (2 + rng.below(250)) as usize].to_vec(),
                _ => d,
            };
            let ss: u32 = if iter % 7 == 0 { 4096 } else { 512 };
            let len = d.len() / ss as usize * ss as usize;
            if len == 0 {
                continue;
            }
            let r = read_gpt(&dev(d[..len].to_vec(), ss)).unwrap();
            if let Some(h) = r.header() {
                selected += 1;
                assert!(h.array_bytes() <= MAX_ENTRY_ARRAY_BYTES);
                assert!(r.partitions.len() <= h.entry_count as usize);
            } else {
                assert!(r.partitions.is_empty());
            }
            assert_eq!(r.selected.is_some(), r.confidence.is_usable());
        }
        // El fuzzing también recorre el camino feliz (no todo acaba rechazado).
        assert!(selected > 300, "solo {selected} lecturas con copia elegida");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "el fuzzing ha tardado {:?}",
            start.elapsed()
        );
    }
}
