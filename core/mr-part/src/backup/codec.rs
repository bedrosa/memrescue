//! Correspondencia sin pérdidas entre los tipos de `mr-part` y el JSON de la copia.
//!
//! Cada estructura se escribe como objeto con **todos** sus campos (también los `null`) y cada
//! enumeración con campos como objeto con su variante en `type`. Al leer se exigen exactamente esos
//! campos: falta uno, sobra uno o tiene otro tipo o rango → [`BackupError::Invalid`] con la ruta.
//! Las macros generan la escritura y la lectura de la misma lista de campos, y el compilador obliga
//! a cubrir todas las variantes y todos los campos de las estructuras: un campo nuevo no puede
//! quedarse fuera de la copia en silencio.

use super::json::Json;
use super::BackupError;
use crate::apm::{ApmAnomaly, ApmPartition, ApmRead, ApmType};
use crate::gpt::{
    format_guid, CopyReport, EntriesState, GptAnomaly, GptCopy, GptField, GptHeader, GptPartition,
    GptRead, HeaderState,
};
use crate::mbr::{MbrAnomaly, NoTableReason};
use crate::search::{
    Check, FoundEntry, FoundTable, FoundTableKind, FoundVolume, FsKind, ReadStats, SearchAnomaly,
    SearchOptions, SearchResult, SearchStatus, Tier, VolumeAnomaly,
};
use crate::{Anomaly, Confidence, Origin, Partition, PartitionKind, PartitionTable, Scheme};

pub(crate) type Res<T> = Result<T, BackupError>;

/// Escritura y lectura de un tipo. `at` es la ruta del valor en el documento (para los errores).
pub(crate) trait Codec: Sized {
    fn enc(&self) -> Json;
    fn dec(v: &Json, at: &str) -> Res<Self>;
}

pub(crate) fn invalid(at: &str, message: impl Into<String>) -> BackupError {
    BackupError::Invalid {
        path: at.to_string(),
        message: message.into(),
    }
}

/// Lector de un objeto que exige cada campo una vez y rechaza los que sobran.
pub(crate) struct Obj<'a> {
    members: &'a [(String, Json)],
    used: Vec<bool>,
    at: String,
}

impl<'a> Obj<'a> {
    pub(crate) fn new(v: &'a Json, at: &str) -> Res<Self> {
        match v {
            Json::Obj(members) => Ok(Self {
                members,
                used: vec![false; members.len()],
                at: at.to_string(),
            }),
            _ => Err(invalid(at, "se esperaba un objeto")),
        }
    }

    pub(crate) fn raw(&mut self, key: &str) -> Res<(&'a Json, String)> {
        let i = self
            .members
            .iter()
            .position(|(k, _)| k == key)
            .ok_or_else(|| invalid(&self.at, format!("falta el campo «{key}»")))?;
        self.used[i] = true;
        Ok((&self.members[i].1, format!("{}.{key}", self.at)))
    }

    pub(crate) fn take<T: Codec>(&mut self, key: &str) -> Res<T> {
        let (v, at) = self.raw(key)?;
        T::dec(v, &at)
    }

    pub(crate) fn finish(self) -> Res<()> {
        match self.used.iter().position(|u| !u) {
            Some(i) => Err(invalid(
                &self.at,
                format!("campo desconocido «{}»", self.members[i].0),
            )),
            None => Ok(()),
        }
    }

    /// `true` si el objeto tiene el campo `key` (campos opcionales añadidos a la versión 1).
    pub(crate) fn has(&self, key: &str) -> bool {
        self.members.iter().any(|(k, _)| k == key)
    }

    pub(crate) fn path(&self) -> &str {
        &self.at
    }
}

pub(crate) fn str_of<'a>(v: &'a Json, at: &str) -> Res<&'a str> {
    match v {
        Json::Str(s) => Ok(s),
        _ => Err(invalid(at, "se esperaba una cadena")),
    }
}

// ---------------------------------------------------------------------------------------------
// Tipos básicos
// ---------------------------------------------------------------------------------------------

macro_rules! integer {
    ($($t:ty),*) => {$(
        impl Codec for $t {
            fn enc(&self) -> Json {
                Json::Int(*self as i128)
            }
            fn dec(v: &Json, at: &str) -> Res<Self> {
                match v {
                    Json::Int(n) => <$t>::try_from(*n).map_err(|_| {
                        invalid(at, format!("{n} no cabe en {}", stringify!($t)))
                    }),
                    _ => Err(invalid(at, "se esperaba un entero")),
                }
            }
        }
    )*};
}

integer!(u8, u32, u64, i64, usize);

impl Codec for bool {
    fn enc(&self) -> Json {
        Json::Bool(*self)
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        match v {
            Json::Bool(b) => Ok(*b),
            _ => Err(invalid(at, "se esperaba true o false")),
        }
    }
}

impl Codec for String {
    fn enc(&self) -> Json {
        Json::Str(self.clone())
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        str_of(v, at).map(str::to_string)
    }
}

/// Nombre del sistema de archivos de [`MbrAnomaly::Superfloppy`]: solo los que produce
/// [`crate::mbr::volume_boot_kind`].
impl Codec for &'static str {
    fn enc(&self) -> Json {
        Json::Str(self.to_string())
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        const KNOWN: [&str; 4] = ["NTFS", "exFAT", "FAT32", "FAT"];
        let s = str_of(v, at)?;
        KNOWN
            .into_iter()
            .find(|k| *k == s)
            .ok_or_else(|| invalid(at, format!("sistema de archivos desconocido: {s}")))
    }
}

impl<T: Codec> Codec for Option<T> {
    fn enc(&self) -> Json {
        self.as_ref().map_or(Json::Null, T::enc)
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        match v {
            Json::Null => Ok(None),
            v => T::dec(v, at).map(Some),
        }
    }
}

impl<T: Codec> Codec for Vec<T> {
    fn enc(&self) -> Json {
        Json::Arr(self.iter().map(T::enc).collect())
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        match v {
            Json::Arr(items) => items
                .iter()
                .enumerate()
                .map(|(i, x)| T::dec(x, &format!("{at}[{i}]")))
                .collect(),
            _ => Err(invalid(at, "se esperaba una lista")),
        }
    }
}

impl Codec for [u64; 5] {
    fn enc(&self) -> Json {
        Json::Arr(self.iter().map(Codec::enc).collect())
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let v: Vec<u64> = Codec::dec(v, at)?;
        <[u64; 5]>::try_from(v).map_err(|_| invalid(at, "se esperaban 5 valores"))
    }
}

/// GUID en su forma textual habitual (`format_guid`): biyectiva con los 16 bytes del disco.
impl Codec for [u8; 16] {
    fn enc(&self) -> Json {
        Json::Str(format_guid(self))
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let s = str_of(v, at)?;
        parse_guid(s).ok_or_else(|| invalid(at, format!("GUID no válido: {s}")))
    }
}

/// Inversa de [`format_guid`]: 8-4-4-4-12 dígitos hexadecimales en minúsculas; los tres primeros
/// grupos se guardan en little endian.
pub(crate) fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let groups: Vec<&str> = s.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    if groups.len() != 5 || groups.iter().zip(lens).any(|(g, l)| g.len() != l) {
        return None;
    }
    let hex = groups.concat();
    let bytes = hex_decode(&hex)?;
    let mut g = [0u8; 16];
    g.copy_from_slice(&bytes);
    g[0..4].reverse();
    g[4..6].reverse();
    g[6..8].reverse();
    Some(g)
}

/// Hexadecimal en minúsculas (la forma canónica del formato).
pub(crate) fn hex_encode(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// Decodifica hexadecimal en minúsculas de longitud par; `None` si no lo es.
pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    b.chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Macros: enumeraciones sin campos (cadenas), enumeraciones con campos (objeto con `type`) y
// estructuras (objeto con todos los campos).
// ---------------------------------------------------------------------------------------------

macro_rules! names {
    ($ty:ident { $($var:ident = $name:literal),* $(,)? }) => {
        impl Codec for $ty {
            fn enc(&self) -> Json {
                Json::Str(match self { $( $ty::$var => $name, )* }.to_string())
            }
            fn dec(v: &Json, at: &str) -> Res<Self> {
                match str_of(v, at)? {
                    $( $name => Ok($ty::$var), )*
                    other => Err(invalid(at, format!("valor desconocido: {other}"))),
                }
            }
        }
    };
}

/// Variantes de una enumeración con campos con nombre. Escribe `type` y los campos en el orden
/// de la lista.
pub(crate) trait Tagged: Sized {
    fn enc_fields(&self, out: &mut Vec<(String, Json)>);
    fn dec_fields(o: &mut Obj<'_>) -> Res<Self>;
}

macro_rules! tagged {
    ($ty:ident { $($var:ident { $($f:ident),* }),* $(,)? }) => {
        impl Tagged for $ty {
            fn enc_fields(&self, out: &mut Vec<(String, Json)>) {
                match self {
                    $( $ty::$var { $($f),* } => {
                        out.push(("type".to_string(), Json::Str(stringify!($var).to_string())));
                        $( out.push((stringify!($f).to_string(), Codec::enc($f))); )*
                    } )*
                }
            }
            fn dec_fields(o: &mut Obj<'_>) -> Res<Self> {
                let t: String = o.take("type")?;
                Ok(match t.as_str() {
                    $( stringify!($var) => $ty::$var { $($f: o.take(stringify!($f))?),* }, )*
                    _ => return Err(invalid(o.path(), format!("variante desconocida: {t}"))),
                })
            }
        }
        impl Codec for $ty {
            fn enc(&self) -> Json {
                let mut out = Vec::new();
                self.enc_fields(&mut out);
                Json::Obj(out)
            }
            fn dec(v: &Json, at: &str) -> Res<Self> {
                let mut o = Obj::new(v, at)?;
                let r = Self::dec_fields(&mut o)?;
                o.finish()?;
                Ok(r)
            }
        }
    };
}

macro_rules! record {
    ($ty:ident { $($f:ident),* $(,)? }) => {
        impl Codec for $ty {
            fn enc(&self) -> Json {
                Json::Obj(vec![ $( (stringify!($f).to_string(), Codec::enc(&self.$f)) ),* ])
            }
            fn dec(v: &Json, at: &str) -> Res<Self> {
                let mut o = Obj::new(v, at)?;
                let r = Self { $( $f: o.take(stringify!($f))? ),* };
                o.finish()?;
                Ok(r)
            }
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Tabla de particiones
// ---------------------------------------------------------------------------------------------

names!(Scheme {
    None = "none",
    Mbr = "mbr",
    Gpt = "gpt",
    Hybrid = "hybrid",
    Apm = "apm",
});

names!(NoTableReason {
    Blank = "blank",
    NoEntryInsideDisk = "no_entry_inside_disk",
    InvalidBootIndicator = "invalid_boot_indicator",
    MultipleExtended = "multiple_extended",
    GptHeader = "gpt_header",
    OutsideDevice = "outside_device",
});

names!(GptCopy {
    Primary = "primary",
    Backup = "backup",
});

names!(HeaderState {
    Valid = "valid",
    Crc = "crc",
    Invalid = "invalid",
    Missing = "missing",
    OutsideImage = "outside_image",
    Unreadable = "unreadable",
});

names!(EntriesState {
    Valid = "valid",
    Crc = "crc",
    OutsideImage = "outside_image",
    Unreadable = "unreadable",
    Unchecked = "unchecked",
});

names!(GptField {
    DiskGuid = "disk_guid",
    UsableRange = "usable_range",
    EntryCount = "entry_count",
    EntrySize = "entry_size",
    EntriesCrc32 = "entries_crc32",
    Location = "location",
});

tagged!(Origin {
    Primary { slot },
    Logical { ebr_lba, slot },
    Gpt { index },
    Apm { index },
});

tagged!(MbrAnomaly {
    NoTable { lba, reason },
    EbrInvalid { lba, reason },
    Empty { lba },
    SignatureMissing { lba },
    Superfloppy { lba, fs },
    InvalidBootIndicator { lba, slot, value, used },
    MultipleActive { lba, slots },
    MultipleExtended { lba, slots },
    ZeroLength { lba, slot, kind },
    OutsideDisk { lba, origin, start },
    PartiallyOutsideDisk { lba, origin, end, disk_sectors },
    Overlap { lba, first, second },
    UnknownType { lba, origin, kind },
    LogicalOutsideExtended { lba, origin },
    ExtraDataEntries { lba, count },
    EbrLoop { lba, target },
    LinkOutsideDisk { lba, target },
    LinkOutsideExtended { lba, target },
    ChainLimit { lba, limit },
    ProtectiveStart { lba, start },
    ProtectiveSizeMismatch { lba, declared, expected },
    Hybrid { lba },
    HybridMismatch { lba, slot },
});

tagged!(GptAnomaly {
    HeaderMissing { copy, lba },
    HeaderOutsideImage { copy, disk_lba },
    HeaderCrc { copy, lba, stored, computed },
    HeaderSize { copy, lba, size },
    RevisionUnsupported { copy, lba, revision },
    RevisionMinor { copy, lba, revision },
    MyLbaMismatch { copy, lba, my_lba, expected },
    AlternateLbaMismatch { copy, lba, alternate_lba },
    UsableRangeInvalid { copy, lba, first, last },
    EntriesLocationInvalid { copy, lba, entries_lba },
    EntryArrayLimits { copy, lba, entry_count, entry_size },
    EntriesCrc { copy, lba, stored, computed },
    EntriesOutsideImage { copy, disk_lba },
    BackupInUse { primary_header, primary_entries },
    CopiesDisagree { field },
    ImageShifted { offset },
    ImageTruncated { missing },
    UsableRangeOutsideImage { lba, first, last },
    SectorSizeMismatch { device, used },
    EntryInverted { index, first, last },
    EntryOutsideUsable { index },
    EntryOutsideImage { index },
    EntryOverlap { index, other },
    DuplicateUniqueGuid { index, other },
    NameInvalid { index },
});

tagged!(ApmAnomaly {
    DdmMissing {},
    BlockSizeInvalid { declared },
    BlockSizeMismatch { declared, used },
    DiskSizeMismatch { declared, available },
    MapMissing { lba },
    MapCountInvalid { lba, count },
    MapCountMismatch { lba, index, count },
    MapOutsideImage { index },
    EntrySignature { lba, index },
    EntryZeroLength { lba, index },
    EntryOutsideDisk { lba, index, start },
    EntryPartiallyOutsideDisk { lba, index, end },
    EntryOverlap { lba, index, other },
    EntryDataOutside { lba, index },
    SectorSizeMismatch { block_size, device },
});

/// Tipo APM: los 32 bytes de `pmParType` en hexadecimal (sin pérdidas, también tras el NUL).
impl Codec for ApmType {
    fn enc(&self) -> Json {
        Json::Str(hex_encode(&self.0))
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let s = str_of(v, at)?;
        hex_decode(s)
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(ApmType)
            .ok_or_else(|| invalid(at, "se esperaban 32 bytes en hexadecimal"))
    }
}

/// `{"family": "device" | "mbr" | "gpt" | "apm", "code": <código estable>, "type": <variante>, ...}`.
/// El código es informativo y se comprueba al leer.
impl Codec for Anomaly {
    fn enc(&self) -> Json {
        let family = match self {
            Anomaly::UnreadableSector { .. } => "device",
            Anomaly::Mbr(_) => "mbr",
            Anomaly::Gpt(_) => "gpt",
            Anomaly::Apm(_) => "apm",
        };
        let mut out = vec![
            ("family".to_string(), Json::Str(family.to_string())),
            ("code".to_string(), Json::Str(self.code().to_string())),
        ];
        match self {
            Anomaly::UnreadableSector { lba } => {
                out.push((
                    "type".to_string(),
                    Json::Str("UnreadableSector".to_string()),
                ));
                out.push(("lba".to_string(), lba.enc()));
            }
            Anomaly::Mbr(m) => m.enc_fields(&mut out),
            Anomaly::Gpt(g) => g.enc_fields(&mut out),
            Anomaly::Apm(a) => a.enc_fields(&mut out),
        }
        Json::Obj(out)
    }

    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let family: String = o.take("family")?;
        let code: String = o.take("code")?;
        let a = match family.as_str() {
            "device" => {
                let t: String = o.take("type")?;
                if t != "UnreadableSector" {
                    return Err(invalid(at, format!("variante desconocida: {t}")));
                }
                Anomaly::UnreadableSector {
                    lba: o.take("lba")?,
                }
            }
            "mbr" => Anomaly::Mbr(MbrAnomaly::dec_fields(&mut o)?),
            "gpt" => Anomaly::Gpt(GptAnomaly::dec_fields(&mut o)?),
            "apm" => Anomaly::Apm(ApmAnomaly::dec_fields(&mut o)?),
            other => return Err(invalid(at, format!("familia desconocida: {other}"))),
        };
        o.finish()?;
        if a.code() != code {
            return Err(invalid(
                at,
                format!(
                    "el código {code} no corresponde a la anomalía ({})",
                    a.code()
                ),
            ));
        }
        Ok(a)
    }
}

/// `{"table": "mbr", "type": 7}`, `{"table": "gpt", "type": "<GUID>"}` o
/// `{"table": "apm", "type": "<32 bytes en hexadecimal>"}`.
impl Codec for PartitionKind {
    fn enc(&self) -> Json {
        let (table, t) = match self {
            PartitionKind::Mbr(t) => ("mbr", t.enc()),
            PartitionKind::Gpt(g) => ("gpt", g.enc()),
            PartitionKind::Apm(t) => ("apm", t.enc()),
        };
        Json::Obj(vec![
            ("table".to_string(), Json::Str(table.to_string())),
            ("type".to_string(), t),
        ])
    }

    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let table: String = o.take("table")?;
        let k = match table.as_str() {
            "mbr" => PartitionKind::Mbr(o.take("type")?),
            "gpt" => PartitionKind::Gpt(o.take("type")?),
            "apm" => PartitionKind::Apm(o.take("type")?),
            other => return Err(invalid(at, format!("tabla desconocida: {other}"))),
        };
        o.finish()?;
        Ok(k)
    }
}

/// `{"level": "valid"}`, `{"level": "valid_with_fixes" | "suspicious", "reasons": [...]}` o
/// `{"level": "invalid", "reason": "..."}`.
impl Codec for Confidence {
    fn enc(&self) -> Json {
        let level = |l: &str| ("level".to_string(), Json::Str(l.to_string()));
        Json::Obj(match self {
            Confidence::Valid => vec![level("valid")],
            Confidence::ValidWithFixes(r) => {
                vec![level("valid_with_fixes"), ("reasons".to_string(), r.enc())]
            }
            Confidence::Suspicious(r) => {
                vec![level("suspicious"), ("reasons".to_string(), r.enc())]
            }
            Confidence::Invalid(r) => vec![level("invalid"), ("reason".to_string(), r.enc())],
        })
    }

    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let level: String = o.take("level")?;
        let c = match level.as_str() {
            "valid" => Confidence::Valid,
            "valid_with_fixes" => Confidence::ValidWithFixes(o.take("reasons")?),
            "suspicious" => Confidence::Suspicious(o.take("reasons")?),
            "invalid" => Confidence::Invalid(o.take("reason")?),
            other => return Err(invalid(at, format!("confianza desconocida: {other}"))),
        };
        o.finish()?;
        Ok(c)
    }
}

record!(Partition {
    origin,
    start_lba,
    sector_count,
    kind,
    name,
    active,
    confidence,
    anomalies,
});

record!(GptHeader {
    revision,
    header_size,
    header_crc32,
    my_lba,
    alternate_lba,
    first_usable_lba,
    last_usable_lba,
    disk_guid,
    entries_lba,
    entry_count,
    entry_size,
    entries_crc32,
});

record!(CopyReport {
    header,
    entries,
    disk_lba,
    image_lba,
    fields,
});

record!(GptPartition {
    partition,
    unique_guid,
    attributes,
});

record!(GptRead {
    sector_size,
    device_sector_size,
    primary,
    backup,
    selected,
    shift,
    trailing_sectors_missing,
    partitions,
    anomalies,
    confidence,
});

record!(ApmPartition {
    partition,
    status,
    data_start,
    data_count,
});

record!(ApmRead {
    ddm,
    ddm_block_size,
    ddm_block_count,
    block_size,
    disk_blocks,
    map_entries,
    map_found,
    partitions,
    anomalies,
    confidence,
});

/// Todos los campos y, solo si hay lectura APM, `apm` (#17). Sin APM el objeto es el mismo que
/// antes de #17, así que la versión 1 del formato no cambia: las copias anteriores se leen igual y
/// una copia sin APM la lee también un programa anterior. `apm` ausente o `null` es `None`; debe
/// estar presente si y solo si el esquema es `apm`.
impl Codec for PartitionTable {
    fn enc(&self) -> Json {
        let mut out = vec![
            ("scheme".to_string(), self.scheme.enc()),
            ("sector_size".to_string(), self.sector_size.enc()),
            ("partitions".to_string(), self.partitions.enc()),
            ("anomalies".to_string(), self.anomalies.enc()),
            ("gpt".to_string(), self.gpt.enc()),
        ];
        if let Some(a) = &self.apm {
            out.push(("apm".to_string(), a.enc()));
        }
        Json::Obj(out)
    }
    fn dec(v: &Json, at: &str) -> Res<Self> {
        let mut o = Obj::new(v, at)?;
        let mut r = Self {
            scheme: o.take("scheme")?,
            sector_size: o.take("sector_size")?,
            partitions: o.take("partitions")?,
            anomalies: o.take("anomalies")?,
            gpt: o.take("gpt")?,
            apm: None,
        };
        if o.has("apm") {
            r.apm = o.take("apm")?;
        }
        o.finish()?;
        if (r.scheme == Scheme::Apm) != r.apm.is_some() {
            return Err(invalid(
                at,
                "la lectura APM debe estar presente si y solo si el esquema es apm",
            ));
        }
        Ok(r)
    }
}

// ---------------------------------------------------------------------------------------------
// Búsqueda exprés
// ---------------------------------------------------------------------------------------------

names!(Tier {
    Declared = "p0",
    Derived = "p1",
    StartGrid = "p2",
    Gaps = "p3",
    EndGrid = "p4",
});

names!(FsKind {
    Ntfs = "ntfs",
    Fat12 = "fat12",
    Fat16 = "fat16",
    Fat32 = "fat32",
    ExFat = "exfat",
});

names!(Check {
    Yes = "yes",
    No = "no",
    Unverifiable = "unverifiable",
    Outside = "outside_image",
    NotApplicable = "not_applicable",
});

names!(SearchStatus {
    Complete = "complete",
    BudgetExhausted = "budget_exhausted",
    Cancelled = "cancelled",
});

names!(FoundTableKind {
    Mbr = "mbr",
    Gpt = "gpt",
});

tagged!(VolumeAnomaly {
    PrimaryDamaged { byte },
    PrimaryUnreadable { lba },
    PrimaryOutsideImage { byte },
    BackupMissing { byte, compensated },
    BackupUnreadable { lba, compensated },
    BackupOutsideImage { byte, compensated },
    BackupMismatch { byte },
    BeyondImageEnd { missing_bytes },
    SectorSizeMismatch { fs, device },
    Unaligned { byte },
    ShiftDetected { delta_bytes },
    SelfLocationMismatch { declared_byte, actual_byte },
    Overlap { other_start },
    Unconfirmed {},
});

tagged!(SearchAnomaly {
    ImageShifted { delta_bytes, sector_size, evidence },
    MbrDisplaced { lba },
});

record!(FoundVolume {
    fs,
    start_byte,
    length_bytes,
    fs_sector_size,
    primary,
    backup,
    structures,
    entry,
    self_location,
    aligned,
    primary_byte,
    backup_byte,
    declared_start,
    serial,
    tier,
    origin,
    table_partition,
    found_table,
    confidence,
    anomalies,
    score,
});

record!(FoundEntry {
    start_byte,
    length_bytes,
    mbr_type,
    extended,
});

record!(FoundTable {
    kind,
    lba,
    declared_lba,
    entries,
});

record!(ReadStats {
    reads,
    bytes,
    cache_hits,
    unverifiable_sectors,
    reads_by_tier,
});

record!(SearchOptions {
    max_reads,
    max_bytes,
    max_volumes,
    probe_window,
    start_grid_bytes,
    end_grid_bytes,
    cylinders,
    probes_per_gap,
    families,
});

record!(SearchResult {
    status,
    sector_size,
    sectors,
    volumes,
    tables,
    anomalies,
    shift_bytes,
    stats,
    probed,
    pending,
    volume_limit_reached,
    options,
});
