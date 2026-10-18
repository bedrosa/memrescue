//! Fichero NTFS: atributos de su registro base y de sus registros de extensión (`$ATTRIBUTE_LIST`)
//! y disposición de cada flujo (`$DATA` con o sin nombre) a partir de sus extensiones.
//!
//! **`$ATTRIBUTE_LIST`.** Cuando los atributos no caben en un registro, el registro base lleva una
//! lista con cada atributo, su VCN inicial y el registro que lo contiene. Se sigue con límites:
//! como mucho [`MAX_EXTENSION_RECORDS`] registros de extensión y una lista de
//! [`MAX_ATTRIBUTE_LIST_BYTES`]; cada registro de extensión se lee una sola vez y solo se usa si es
//! un registro válido cuya referencia al registro base es **exactamente** el base (número y
//! secuencia), cuya secuencia coincide con la de la entrada de la lista y que no tiene a su vez
//! una lista (eso formaría un ciclo: nunca se sigue). Única holgura: en un registro que ya no está
//! en uso (borrado), el sistema de archivos incrementa la secuencia al liberarlo, así que se
//! admite exactamente una unidad más que la referencia. Cada incumplimiento es una anomalía tipada
//! ([`FileAnomaly`]) y los atributos de ese registro no se usan; el resto del fichero sí.
//!
//! **Flujos.** Un atributo no residente puede estar partido en extensiones (una por registro), cada
//! una con su tramo de VCN y sus runs. [`NtfsFile::stream_layout`] las ordena y comprueba: la
//! primera empieza en el VCN 0 y da los tamaños (inicializado ≤ datos ≤ asignado, asignado múltiplo
//! del clúster); no se solapan; ninguna supera el tamaño asignado; los runs de cada una cubren
//! exactamente su tramo y quedan dentro del volumen; la unidad de compresión es la de NTFS (16
//! clústeres, clúster ≤ 4 KiB). Los incumplimientos que impiden saber qué hay en el flujo son
//! errores ([`StreamError`]); los que solo dejan tramos sin mapa (extensión ilegible o con runs no
//! válidos, hueco entre extensiones, runs más cortos que los datos) son avisos ([`StreamIssue`]) y
//! esos tramos se leen como `unmapped` (no fiables, nunca como datos).

use std::collections::BTreeSet;
use std::fmt;

use crate::attr::{attr_flags, types, AttrBody, AttrError};
use crate::record::{FileRecord, FileReference, RecordHeader};
use crate::runs::{decode_extent, Run, RunError};
use crate::values::{
    AttrListEntry, AttrListError, FileName, ObjectId, StandardInformation, ValueError,
    VolumeInformation,
};
use crate::RecordState;

/// Máximo de registros de extensión que se siguen desde una `$ATTRIBUTE_LIST`.
pub const MAX_EXTENSION_RECORDS: usize = 4096;
/// Tamaño máximo de una `$ATTRIBUTE_LIST` no residente que se lee.
pub const MAX_ATTRIBUTE_LIST_BYTES: u64 = 1 << 20;
/// Unidad de compresión de NTFS: 2^4 = 16 clústeres.
pub const NTFS_COMPRESSION_UNIT_LOG2: u16 = 4;
/// Clúster máximo con el que NTFS comprime.
pub const MAX_COMPRESSED_CLUSTER: u32 = 4096;

/// Campos de un atributo no residente (copiados del registro).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonResidentInfo {
    pub start_vcn: u64,
    pub last_vcn: u64,
    pub compression_unit: u16,
    pub allocated_size: u64,
    pub data_size: u64,
    pub initialized_size: u64,
    pub compressed_size: Option<u64>,
    pub mapping_pairs: Vec<u8>,
}

/// Contenido de un atributo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    Resident { value: Vec<u8>, indexed: bool },
    NonResident(NonResidentInfo),
}

/// Atributo de un fichero, con el registro (base o extensión) que lo contiene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttribute {
    pub type_code: u32,
    pub name: String,
    pub flags: u16,
    pub id: u16,
    pub record: u64,
    pub value: AttrValue,
}

impl FileAttribute {
    pub(crate) fn from_attr(record: u64, a: &crate::attr::Attribute<'_>) -> Self {
        let value = match &a.body {
            AttrBody::Resident { value, indexed } => AttrValue::Resident {
                value: value.to_vec(),
                indexed: *indexed,
            },
            AttrBody::NonResident(nr) => AttrValue::NonResident(NonResidentInfo {
                start_vcn: nr.start_vcn,
                last_vcn: nr.last_vcn,
                compression_unit: nr.compression_unit,
                allocated_size: nr.allocated_size,
                data_size: nr.data_size,
                initialized_size: nr.initialized_size,
                compressed_size: nr.compressed_size,
                mapping_pairs: nr.mapping_pairs.to_vec(),
            }),
        };
        FileAttribute {
            type_code: a.type_code,
            name: a.name(),
            flags: a.flags,
            id: a.id,
            record,
            value,
        }
    }

    pub fn resident_value(&self) -> Option<&[u8]> {
        match &self.value {
            AttrValue::Resident { value, .. } => Some(value),
            AttrValue::NonResident(_) => None,
        }
    }

    pub fn non_resident(&self) -> Option<&NonResidentInfo> {
        match &self.value {
            AttrValue::NonResident(n) => Some(n),
            AttrValue::Resident { .. } => None,
        }
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & attr_flags::COMPRESSED != 0
    }

    pub fn is_sparse(&self) -> bool {
        self.flags & attr_flags::SPARSE != 0
    }

    pub fn is_encrypted(&self) -> bool {
        self.flags & attr_flags::ENCRYPTED != 0
    }
}

/// Anomalía al reunir los atributos de un fichero (nunca detiene la lectura del resto).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAnomaly {
    /// El recorrido de atributos de un registro se detuvo en un atributo mal formado.
    AttributeWalk { record: u64, error: AttrError },
    /// Más de una `$ATTRIBUTE_LIST` en el registro base: se usa la primera.
    MultipleAttributeLists,
    /// Entrada de la lista mal formada: se usan las anteriores.
    AttributeList(AttrListError),
    /// La lista no residente no se pudo leer entera (sectores no fiables, runs no válidos o más
    /// de [`MAX_ATTRIBUTE_LIST_BYTES`]): se usan las entradas de la parte legible.
    AttributeListUnreadable { detail: String },
    /// Más de [`MAX_EXTENSION_RECORDS`] registros de extensión: no se siguen los demás.
    TooManyExtensions,
    /// La lista apunta a un registro fuera de la MFT.
    ExtensionOutOfRange { record: u64 },
    /// El registro de extensión no es un registro válido (ilegible, dañado, vacío...).
    ExtensionUnusable {
        record: u64,
        state: RecordState,
        detail: Option<String>,
    },
    /// El registro de extensión dice pertenecer a otro registro base (o a otra secuencia del base).
    ExtensionBaseMismatch { record: u64, base: FileReference },
    /// La secuencia del registro no coincide con la de la entrada de la lista (registro reutilizado).
    ExtensionSequenceMismatch {
        record: u64,
        expected: u16,
        found: u16,
    },
    /// El registro de extensión tiene su propia `$ATTRIBUTE_LIST` (no se sigue: evita ciclos).
    ExtensionHasAttributeList { record: u64 },
    /// El registro base está en uso y una extensión no (o al revés).
    ExtensionInUseMismatch { record: u64 },
    /// Una entrada de la lista no tiene su atributo en el registro indicado.
    ListedAttributeMissing {
        record: u64,
        type_code: u32,
        id: u16,
    },
    /// El valor de un atributo residente tipado no cumple su disposición.
    Value {
        type_code: u32,
        id: u16,
        error: ValueError,
    },
}

impl FileAnomaly {
    pub fn code(&self) -> &'static str {
        match self {
            FileAnomaly::AttributeWalk { error, .. } => error.code(),
            FileAnomaly::MultipleAttributeLists => "attr_list_multiple",
            FileAnomaly::AttributeList(e) => e.code(),
            FileAnomaly::AttributeListUnreadable { .. } => "attr_list_unreadable",
            FileAnomaly::TooManyExtensions => "extension_too_many",
            FileAnomaly::ExtensionOutOfRange { .. } => "extension_out_of_range",
            FileAnomaly::ExtensionUnusable { .. } => "extension_unusable",
            FileAnomaly::ExtensionBaseMismatch { .. } => "extension_base_mismatch",
            FileAnomaly::ExtensionSequenceMismatch { .. } => "extension_sequence_mismatch",
            FileAnomaly::ExtensionHasAttributeList { .. } => "extension_has_attr_list",
            FileAnomaly::ExtensionInUseMismatch { .. } => "extension_in_use_mismatch",
            FileAnomaly::ListedAttributeMissing { .. } => "listed_attribute_missing",
            FileAnomaly::Value { error, .. } => error.code(),
        }
    }
}

impl fmt::Display for FileAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tn = |t: u32| crate::attr::type_name(t).unwrap_or("atributo desconocido");
        match self {
            FileAnomaly::AttributeWalk { record, error } => {
                write!(f, "registro {record}: {error}")
            }
            FileAnomaly::MultipleAttributeLists => {
                write!(f, "varias $ATTRIBUTE_LIST en el registro base: se usa la primera")
            }
            FileAnomaly::AttributeList(e) => e.fmt(f),
            FileAnomaly::AttributeListUnreadable { detail } => {
                write!(f, "$ATTRIBUTE_LIST no legible entera: {detail}")
            }
            FileAnomaly::TooManyExtensions => write!(
                f,
                "más de {MAX_EXTENSION_RECORDS} registros de extensión: no se siguen los demás"
            ),
            FileAnomaly::ExtensionOutOfRange { record } => {
                write!(f, "la lista apunta al registro {record}, fuera de la MFT")
            }
            FileAnomaly::ExtensionUnusable {
                record,
                state,
                detail,
            } => {
                write!(f, "registro de extensión {record} no utilizable ({})", state.label())?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            FileAnomaly::ExtensionBaseMismatch { record, base } => write!(
                f,
                "el registro de extensión {record} pertenece al registro base {} (secuencia {})",
                base.record, base.sequence
            ),
            FileAnomaly::ExtensionSequenceMismatch {
                record,
                expected,
                found,
            } => write!(
                f,
                "el registro {record} tiene la secuencia {found} y la lista espera {expected} (reutilizado)"
            ),
            FileAnomaly::ExtensionHasAttributeList { record } => write!(
                f,
                "el registro de extensión {record} tiene su propia $ATTRIBUTE_LIST: no se sigue"
            ),
            FileAnomaly::ExtensionInUseMismatch { record } => write!(
                f,
                "el registro de extensión {record} no tiene el mismo estado de uso que el base"
            ),
            FileAnomaly::ListedAttributeMissing {
                record,
                type_code,
                id,
            } => write!(
                f,
                "la lista cita {} (identificador {id}) en el registro {record} y no está",
                tn(*type_code)
            ),
            FileAnomaly::Value {
                type_code,
                id,
                error,
            } => write!(f, "{} (identificador {id}): {error}", tn(*type_code)),
        }
    }
}

/// Fichero: cabecera del registro base, atributos reunidos y anomalías.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfsFile {
    pub number: u64,
    pub header: RecordHeader,
    /// Registros de extensión cuyos atributos se han usado.
    pub extensions: Vec<u64>,
    /// `$ATTRIBUTE_LIST` (si la hay).
    pub attribute_list: Vec<AttrListEntry>,
    /// Atributos del registro base y de las extensiones usadas, en orden de lectura.
    pub attributes: Vec<FileAttribute>,
    pub anomalies: Vec<FileAnomaly>,
}

impl NtfsFile {
    pub fn reference(&self) -> FileReference {
        FileReference {
            record: self.number,
            sequence: self.header.sequence,
        }
    }

    pub fn in_use(&self) -> bool {
        self.header.in_use()
    }

    pub fn is_directory(&self) -> bool {
        self.header.is_directory()
    }

    fn typed<'a, T: 'a>(
        &'a self,
        t: u32,
        parse: fn(&[u8]) -> Result<T, ValueError>,
    ) -> impl Iterator<Item = (u16, Result<T, ValueError>)> + 'a {
        self.attributes
            .iter()
            .filter(move |a| a.type_code == t)
            .filter_map(move |a| a.resident_value().map(|v| (a.id, parse(v))))
    }

    /// `$STANDARD_INFORMATION` (el primero válido).
    pub fn standard_information(&self) -> Option<StandardInformation> {
        self.typed(types::STANDARD_INFORMATION, StandardInformation::parse)
            .find_map(|(_, r)| r.ok())
    }

    /// `$FILE_NAME` válidos (uno por espacio de nombres y enlace).
    pub fn file_names(&self) -> Vec<FileName> {
        self.typed(types::FILE_NAME, FileName::parse)
            .filter_map(|(_, r)| r.ok())
            .collect()
    }

    pub fn object_id(&self) -> Option<ObjectId> {
        self.typed(types::OBJECT_ID, ObjectId::parse)
            .find_map(|(_, r)| r.ok())
    }

    pub fn volume_information(&self) -> Option<VolumeInformation> {
        self.typed(types::VOLUME_INFORMATION, VolumeInformation::parse)
            .find_map(|(_, r)| r.ok())
    }

    pub fn volume_name(&self) -> Option<String> {
        self.attributes
            .iter()
            .find(|a| a.type_code == types::VOLUME_NAME)
            .and_then(|a| a.resident_value())
            .map(crate::attr::utf16_lossy)
    }

    /// Valores tipados que no cumplen su disposición (para informarlos).
    pub fn value_anomalies(&self) -> Vec<FileAnomaly> {
        let mut out = Vec::new();
        let mut push = |t: u32, id: u16, e: Option<ValueError>| {
            if let Some(error) = e {
                out.push(FileAnomaly::Value {
                    type_code: t,
                    id,
                    error,
                });
            }
        };
        for (id, r) in self.typed(types::STANDARD_INFORMATION, StandardInformation::parse) {
            push(types::STANDARD_INFORMATION, id, r.err());
        }
        for (id, r) in self.typed(types::FILE_NAME, FileName::parse) {
            push(types::FILE_NAME, id, r.err());
        }
        for (id, r) in self.typed(types::OBJECT_ID, ObjectId::parse) {
            push(types::OBJECT_ID, id, r.err());
        }
        out
    }

    /// Nombres de los flujos `$DATA` (`""` = el principal), sin repetir, el principal primero.
    pub fn data_streams(&self) -> Vec<String> {
        let names: BTreeSet<&str> = self
            .attributes
            .iter()
            .filter(|a| a.type_code == types::DATA)
            .map(|a| a.name.as_str())
            .collect();
        names.into_iter().map(str::to_string).collect()
    }

    /// Disposición del flujo `name` del tipo `type_code` (`$DATA` = 0x80).
    pub fn stream_layout(
        &self,
        type_code: u32,
        name: &str,
        geometry: Geometry,
    ) -> Result<StreamLayout, StreamError> {
        let parts: Vec<&FileAttribute> = self
            .attributes
            .iter()
            .filter(|a| a.type_code == type_code && a.name == name)
            .collect();
        build_layout(type_code, name, &parts, geometry)
    }
}

/// Geometría del volumen que necesitan los flujos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cluster_size: u32,
    pub total_clusters: u64,
}

/// Flujo que no se puede leer (no se sabe qué contiene).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    /// No hay ningún atributo con ese tipo y nombre.
    NotFound { name: String },
    /// Cifrado (EFS): el contenido en disco no es el del fichero.
    Encrypted,
    /// Partes residentes y no residentes a la vez, o varias residentes.
    MixedForms,
    /// Ninguna extensión empieza en el VCN 0 (no se conocen los tamaños).
    MissingFirstExtent,
    /// Dos extensiones empiezan en el mismo VCN.
    DuplicateExtent { start_vcn: u64 },
    /// Una extensión empieza antes de que termine la anterior.
    ExtentOverlap { start_vcn: u64, previous_end: u64 },
    /// Tamaños incoherentes (inicializado ≤ datos ≤ asignado).
    Sizes {
        allocated: u64,
        data: u64,
        initialized: u64,
    },
    /// Tamaño asignado no múltiplo del clúster.
    AllocatedNotClusterMultiple { allocated: u64, cluster: u32 },
    /// Comprimido con una unidad de compresión distinta de la de NTFS.
    CompressionUnit { value: u16 },
    /// Comprimido con clústeres de más de 4 KiB (NTFS no comprime con ellos).
    CompressionClusterSize { cluster: u32 },
    /// Las extensiones cubren más clústeres que el tamaño asignado.
    ExceedsAllocated {
        clusters: u64,
        allocated_clusters: u64,
    },
}

impl StreamError {
    pub fn code(&self) -> &'static str {
        match self {
            StreamError::NotFound { .. } => "stream_not_found",
            StreamError::Encrypted => "stream_encrypted",
            StreamError::MixedForms => "stream_mixed_forms",
            StreamError::MissingFirstExtent => "stream_missing_first_extent",
            StreamError::DuplicateExtent { .. } => "stream_duplicate_extent",
            StreamError::ExtentOverlap { .. } => "stream_extent_overlap",
            StreamError::Sizes { .. } => "stream_sizes",
            StreamError::AllocatedNotClusterMultiple { .. } => {
                "stream_allocated_not_cluster_multiple"
            }
            StreamError::CompressionUnit { .. } => "stream_compression_unit",
            StreamError::CompressionClusterSize { .. } => "stream_compression_cluster_size",
            StreamError::ExceedsAllocated { .. } => "stream_exceeds_allocated",
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::NotFound { name } if name.is_empty() => {
                write!(f, "el fichero no tiene flujo de datos principal")
            }
            StreamError::NotFound { name } => write!(f, "no existe el flujo «{name}»"),
            StreamError::Encrypted => write!(
                f,
                "flujo cifrado (EFS): el contenido en disco no es el del fichero"
            ),
            StreamError::MixedForms => {
                write!(f, "el flujo tiene partes residentes y no residentes")
            }
            StreamError::MissingFirstExtent => write!(
                f,
                "ninguna extensión del flujo empieza en el VCN 0: tamaños desconocidos"
            ),
            StreamError::DuplicateExtent { start_vcn } => {
                write!(f, "dos extensiones empiezan en el VCN {start_vcn}")
            }
            StreamError::ExtentOverlap {
                start_vcn,
                previous_end,
            } => write!(
                f,
                "una extensión empieza en el VCN {start_vcn} y la anterior termina en el {previous_end}"
            ),
            StreamError::Sizes {
                allocated,
                data,
                initialized,
            } => write!(
                f,
                "tamaños incoherentes: asignado {allocated}, datos {data}, inicializado {initialized}"
            ),
            StreamError::AllocatedNotClusterMultiple { allocated, cluster } => write!(
                f,
                "tamaño asignado {allocated} no múltiplo del clúster ({cluster} B)"
            ),
            StreamError::CompressionUnit { value } => write!(
                f,
                "unidad de compresión 2^{value} clústeres (NTFS usa 2^{NTFS_COMPRESSION_UNIT_LOG2})"
            ),
            StreamError::CompressionClusterSize { cluster } => write!(
                f,
                "flujo comprimido con clústeres de {cluster} B (NTFS solo comprime con ≤ {MAX_COMPRESSED_CLUSTER} B)"
            ),
            StreamError::ExceedsAllocated {
                clusters,
                allocated_clusters,
            } => write!(
                f,
                "las extensiones cubren {clusters} clústeres y el tamaño asignado {allocated_clusters}"
            ),
        }
    }
}

/// Aviso de un flujo legible: tramos sin mapa (se leen como `unmapped`, no fiables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamIssue {
    /// Runs no válidos en una extensión: su tramo queda sin mapa.
    ExtentRuns {
        record: u64,
        start_vcn: u64,
        error: RunError,
    },
    /// Hueco entre extensiones (extensión no encontrada o no utilizable).
    ExtentGap { from_vcn: u64, to_vcn: u64 },
    /// Las extensiones no cubren los datos inicializados.
    RunsShort {
        mapped_clusters: u64,
        needed_clusters: u64,
    },
    /// Unidad de compresión sin el indicador de comprimido ni de disperso (los dispersos la
    /// llevan): se ignora.
    CompressionUnitIgnored { value: u16 },
}

impl StreamIssue {
    pub fn code(&self) -> &'static str {
        match self {
            StreamIssue::ExtentRuns { error, .. } => error.code(),
            StreamIssue::ExtentGap { .. } => "stream_extent_gap",
            StreamIssue::RunsShort { .. } => "stream_runs_short",
            StreamIssue::CompressionUnitIgnored { .. } => "stream_compression_unit_ignored",
        }
    }
}

impl fmt::Display for StreamIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamIssue::ExtentRuns {
                record,
                start_vcn,
                error,
            } => write!(
                f,
                "extensión desde el VCN {start_vcn} (registro {record}): {error}; su tramo queda sin mapa"
            ),
            StreamIssue::ExtentGap { from_vcn, to_vcn } => write!(
                f,
                "VCN {from_vcn} a {} sin extensión que los describa",
                to_vcn - 1
            ),
            StreamIssue::RunsShort {
                mapped_clusters,
                needed_clusters,
            } => write!(
                f,
                "las extensiones describen {mapped_clusters} clústeres y los datos inicializados ocupan {needed_clusters}"
            ),
            StreamIssue::CompressionUnitIgnored { value } => write!(
                f,
                "unidad de compresión {value} en un flujo no comprimido: se ignora"
            ),
        }
    }
}

/// Qué hay en un tramo de VCN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Clústeres en disco desde este LCN.
    Disk(u64),
    /// Disperso: ceros por definición del formato.
    Sparse,
    /// Sin mapa (extensión que falta o no válida): no fiable.
    Unmapped,
}

/// Tramo de VCN con su contenido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vcn: u64,
    pub length: u64,
    pub kind: SegmentKind,
}

impl Segment {
    pub fn end_vcn(&self) -> u64 {
        self.vcn + self.length
    }
}

/// Extensión de un flujo no residente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    pub record: u64,
    pub start_vcn: u64,
    pub last_vcn: u64,
    /// Runs no válidos (la extensión no se usa).
    pub error: Option<RunError>,
}

/// Forma del flujo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamForm {
    Resident(Vec<u8>),
    NonResident,
}

/// Disposición de un flujo ya comprobada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLayout {
    pub type_code: u32,
    pub name: String,
    pub flags: u16,
    pub form: StreamForm,
    pub data_size: u64,
    pub allocated_size: u64,
    pub initialized_size: u64,
    pub compressed_size: Option<u64>,
    /// Clústeres por unidad de compresión (0 = sin compresión).
    pub compression_unit_clusters: u64,
    pub cluster_size: u32,
    pub extents: Vec<Extent>,
    /// Runs decodificados de las extensiones válidas, en orden de VCN (tal como están).
    pub runs: Vec<Run>,
    /// Mapa completo `[0, clústeres de los datos)`, con los tramos sin mapa.
    pub segments: Vec<Segment>,
    pub issues: Vec<StreamIssue>,
}

impl StreamLayout {
    pub fn is_resident(&self) -> bool {
        matches!(self.form, StreamForm::Resident(_))
    }

    pub fn is_compressed(&self) -> bool {
        self.compression_unit_clusters > 0
    }

    pub fn is_sparse_flag(&self) -> bool {
        self.flags & attr_flags::SPARSE != 0
    }
}

fn build_layout(
    type_code: u32,
    name: &str,
    parts: &[&FileAttribute],
    g: Geometry,
) -> Result<StreamLayout, StreamError> {
    let Some(first) = parts.first() else {
        return Err(StreamError::NotFound {
            name: name.to_string(),
        });
    };
    if parts.iter().any(|a| a.is_encrypted()) {
        return Err(StreamError::Encrypted);
    }
    let residents: Vec<_> = parts.iter().filter_map(|a| a.resident_value()).collect();
    if !residents.is_empty() {
        if parts.len() != 1 {
            return Err(StreamError::MixedForms);
        }
        let v = residents[0].to_vec();
        let n = v.len() as u64;
        return Ok(StreamLayout {
            type_code,
            name: name.to_string(),
            flags: first.flags,
            form: StreamForm::Resident(v),
            data_size: n,
            allocated_size: n,
            initialized_size: n,
            compressed_size: None,
            compression_unit_clusters: 0,
            cluster_size: g.cluster_size,
            extents: Vec::new(),
            runs: Vec::new(),
            segments: Vec::new(),
            issues: Vec::new(),
        });
    }
    let mut nrs: Vec<(u64, &NonResidentInfo, u16)> = parts
        .iter()
        .filter_map(|a| a.non_resident().map(|n| (a.record, n, a.flags)))
        .collect();
    nrs.sort_by_key(|(_, n, _)| n.start_vcn);
    for w in nrs.windows(2) {
        if w[0].1.start_vcn == w[1].1.start_vcn {
            return Err(StreamError::DuplicateExtent {
                start_vcn: w[0].1.start_vcn,
            });
        }
    }
    let (_, head, flags) = nrs[0];
    if head.start_vcn != 0 {
        return Err(StreamError::MissingFirstExtent);
    }
    let cluster = u64::from(g.cluster_size);
    let (alloc, data, init) = (head.allocated_size, head.data_size, head.initialized_size);
    if init > data || data > alloc {
        return Err(StreamError::Sizes {
            allocated: alloc,
            data,
            initialized: init,
        });
    }
    if !alloc.is_multiple_of(cluster) {
        return Err(StreamError::AllocatedNotClusterMultiple {
            allocated: alloc,
            cluster: g.cluster_size,
        });
    }
    let mut issues = Vec::new();
    let compressed = flags & attr_flags::COMPRESSED != 0;
    let cu = head.compression_unit;
    let cu_clusters = if compressed {
        if cu != NTFS_COMPRESSION_UNIT_LOG2 {
            return Err(StreamError::CompressionUnit { value: cu });
        }
        if g.cluster_size > MAX_COMPRESSED_CLUSTER {
            return Err(StreamError::CompressionClusterSize {
                cluster: g.cluster_size,
            });
        }
        1u64 << cu
    } else {
        if cu != 0 && flags & attr_flags::SPARSE == 0 {
            issues.push(StreamIssue::CompressionUnitIgnored { value: cu });
        }
        0
    };
    let allocated_clusters = alloc / cluster;
    let mut extents = Vec::new();
    let mut runs = Vec::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut cursor = 0u64;
    for (record, n, _) in &nrs {
        let end = n.last_vcn.wrapping_add(1);
        if end < n.start_vcn {
            // Ya lo rechaza el iterador de atributos; defensivo.
            return Err(StreamError::ExtentOverlap {
                start_vcn: n.start_vcn,
                previous_end: cursor,
            });
        }
        if n.start_vcn < cursor {
            return Err(StreamError::ExtentOverlap {
                start_vcn: n.start_vcn,
                previous_end: cursor,
            });
        }
        if end > allocated_clusters {
            return Err(StreamError::ExceedsAllocated {
                clusters: end,
                allocated_clusters,
            });
        }
        if n.start_vcn > cursor {
            issues.push(StreamIssue::ExtentGap {
                from_vcn: cursor,
                to_vcn: n.start_vcn,
            });
            segments.push(Segment {
                vcn: cursor,
                length: n.start_vcn - cursor,
                kind: SegmentKind::Unmapped,
            });
        }
        let decoded = decode_extent(&n.mapping_pairs, n.start_vcn, n.last_vcn, g.total_clusters);
        let error = decoded.as_ref().err().copied();
        match decoded {
            Ok(r) => {
                segments.extend(r.iter().map(|x| Segment {
                    vcn: x.vcn,
                    length: x.length,
                    kind: x.lcn.map_or(SegmentKind::Sparse, SegmentKind::Disk),
                }));
                runs.extend(r);
            }
            Err(e) => {
                issues.push(StreamIssue::ExtentRuns {
                    record: *record,
                    start_vcn: n.start_vcn,
                    error: e,
                });
                if end > n.start_vcn {
                    segments.push(Segment {
                        vcn: n.start_vcn,
                        length: end - n.start_vcn,
                        kind: SegmentKind::Unmapped,
                    });
                }
            }
        }
        extents.push(Extent {
            record: *record,
            start_vcn: n.start_vcn,
            last_vcn: n.last_vcn,
            error,
        });
        cursor = end;
    }
    let needed_init = init.div_ceil(cluster);
    let needed_data = data.div_ceil(cluster);
    if cursor < needed_init {
        issues.push(StreamIssue::RunsShort {
            mapped_clusters: cursor,
            needed_clusters: needed_init,
        });
    }
    if cursor < needed_data {
        segments.push(Segment {
            vcn: cursor,
            length: needed_data - cursor,
            kind: SegmentKind::Unmapped,
        });
    }
    Ok(StreamLayout {
        type_code,
        name: name.to_string(),
        flags,
        form: StreamForm::NonResident,
        data_size: data,
        allocated_size: alloc,
        initialized_size: init,
        compressed_size: head.compressed_size,
        compression_unit_clusters: cu_clusters,
        cluster_size: g.cluster_size,
        extents,
        runs,
        segments,
        issues,
    })
}

/// Atributos de un registro (copiados) y la anomalía que detuvo el recorrido, si la hubo.
pub(crate) fn record_attributes(rec: &FileRecord) -> (Vec<FileAttribute>, Option<FileAnomaly>) {
    let mut out = Vec::new();
    for a in rec.attributes() {
        match a {
            Ok(a) => out.push(FileAttribute::from_attr(rec.number(), &a)),
            Err(error) => {
                return (
                    out,
                    Some(FileAnomaly::AttributeWalk {
                        record: rec.number(),
                        error,
                    }),
                )
            }
        }
    }
    (out, None)
}

/// Lo que necesita [`assemble`] del volumen: leer registros y el contenido de una lista no
/// residente.
pub(crate) trait RecordSource {
    fn record_count(&self) -> u64;
    /// Registro `n` interpretado, o su estado y detalle.
    fn record(
        &self,
        n: u64,
    ) -> Result<Result<FileRecord, (RecordState, Option<String>)>, crate::NtfsError>;
    /// Contenido de una `$ATTRIBUTE_LIST` no residente: bytes legibles (prefijo fiable) y, si no
    /// se pudo leer entera, el motivo.
    fn attribute_list(
        &self,
        layout: &StreamLayout,
    ) -> Result<(Vec<u8>, Option<String>), crate::NtfsError>;
    fn geometry(&self) -> Geometry;
}

/// Reúne los atributos del registro base `base` siguiendo su `$ATTRIBUTE_LIST`.
pub(crate) fn assemble(
    src: &dyn RecordSource,
    base: FileRecord,
) -> Result<NtfsFile, crate::NtfsError> {
    let number = base.number();
    let header = base.header().clone();
    let (mut attributes, walk) = record_attributes(&base);
    let mut anomalies: Vec<FileAnomaly> = walk.into_iter().collect();
    let lists: Vec<&FileAttribute> = attributes
        .iter()
        .filter(|a| a.type_code == types::ATTRIBUTE_LIST)
        .collect();
    let mut entries = Vec::new();
    let mut extensions = Vec::new();
    if let Some(list) = lists.first() {
        if lists.len() > 1 {
            anomalies.push(FileAnomaly::MultipleAttributeLists);
        }
        let bytes = match &list.value {
            AttrValue::Resident { value, .. } => Some(value.clone()),
            AttrValue::NonResident(_) => {
                let parts = [*list];
                match build_layout(types::ATTRIBUTE_LIST, &list.name, &parts, src.geometry()) {
                    Ok(l) if l.data_size > MAX_ATTRIBUTE_LIST_BYTES => {
                        anomalies.push(FileAnomaly::AttributeListUnreadable {
                            detail: format!(
                                "{} bytes (máximo {MAX_ATTRIBUTE_LIST_BYTES})",
                                l.data_size
                            ),
                        });
                        None
                    }
                    Ok(l) => {
                        let (b, problem) = src.attribute_list(&l)?;
                        if let Some(detail) = problem {
                            anomalies.push(FileAnomaly::AttributeListUnreadable { detail });
                        }
                        Some(b)
                    }
                    Err(e) => {
                        anomalies.push(FileAnomaly::AttributeListUnreadable {
                            detail: e.to_string(),
                        });
                        None
                    }
                }
            }
        };
        if let Some(bytes) = bytes {
            let (e, err) = crate::values::parse_attribute_list(&bytes);
            if let Some(err) = err {
                anomalies.push(FileAnomaly::AttributeList(err));
            }
            entries = e;
        }
    }
    // Registros de extensión, cada uno una sola vez y en orden de aparición.
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut usable: BTreeSet<u64> = BTreeSet::from([number]);
    // Secuencia actual frente a la referenciada (un registro liberado lleva una unidad más).
    let seq_ok = |found: u16, referenced: u16, in_use: bool| {
        found == referenced || (!in_use && found == referenced.wrapping_add(1))
    };
    for e in &entries {
        let r = e.reference.record;
        if r == number {
            if !seq_ok(header.sequence, e.reference.sequence, header.in_use()) && seen.insert(r) {
                anomalies.push(FileAnomaly::ExtensionSequenceMismatch {
                    record: r,
                    expected: e.reference.sequence,
                    found: header.sequence,
                });
            }
            continue;
        }
        if !seen.insert(r) {
            continue;
        }
        if extensions.len() >= MAX_EXTENSION_RECORDS {
            anomalies.push(FileAnomaly::TooManyExtensions);
            break;
        }
        if r >= src.record_count() {
            anomalies.push(FileAnomaly::ExtensionOutOfRange { record: r });
            continue;
        }
        let ext = match src.record(r)? {
            Ok(x) => x,
            Err((state, detail)) => {
                anomalies.push(FileAnomaly::ExtensionUnusable {
                    record: r,
                    state,
                    detail,
                });
                continue;
            }
        };
        let base_ref = ext.header().base_record;
        if base_ref.record != number || !seq_ok(header.sequence, base_ref.sequence, header.in_use())
        {
            anomalies.push(FileAnomaly::ExtensionBaseMismatch {
                record: r,
                base: ext.header().base_record,
            });
            continue;
        }
        if !seq_ok(ext.header().sequence, e.reference.sequence, ext.in_use()) {
            anomalies.push(FileAnomaly::ExtensionSequenceMismatch {
                record: r,
                expected: e.reference.sequence,
                found: ext.header().sequence,
            });
            continue;
        }
        if ext.in_use() != header.in_use() {
            anomalies.push(FileAnomaly::ExtensionInUseMismatch { record: r });
        }
        let (attrs, walk) = record_attributes(&ext);
        anomalies.extend(walk);
        if attrs.iter().any(|a| a.type_code == types::ATTRIBUTE_LIST) {
            anomalies.push(FileAnomaly::ExtensionHasAttributeList { record: r });
        }
        attributes.extend(
            attrs
                .into_iter()
                .filter(|a| a.type_code != types::ATTRIBUTE_LIST),
        );
        extensions.push(r);
        usable.insert(r);
    }
    for e in &entries {
        let r = e.reference.record;
        if usable.contains(&r)
            && !attributes
                .iter()
                .any(|a| a.record == r && a.type_code == e.type_code && a.id == e.id)
        {
            anomalies.push(FileAnomaly::ListedAttributeMissing {
                record: r,
                type_code: e.type_code,
                id: e.id,
            });
        }
    }
    let mut file = NtfsFile {
        number,
        header,
        extensions,
        attribute_list: entries,
        attributes,
        anomalies,
    };
    let values = file.value_anomalies();
    file.anomalies.extend(values);
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::encode_runs;

    fn nr(
        record: u64,
        flags: u16,
        start: u64,
        last: u64,
        cu: u16,
        sizes: (u64, u64, u64),
    ) -> FileAttribute {
        FileAttribute {
            type_code: types::DATA,
            name: String::new(),
            flags,
            id: 0,
            record,
            value: AttrValue::NonResident(NonResidentInfo {
                start_vcn: start,
                last_vcn: last,
                compression_unit: cu,
                allocated_size: sizes.0,
                data_size: sizes.1,
                initialized_size: sizes.2,
                compressed_size: None,
                mapping_pairs: encode_runs(&[(Some(10), last.wrapping_add(1) - start)]),
            }),
        }
    }

    const G: Geometry = Geometry {
        cluster_size: 4096,
        total_clusters: 1000,
    };

    fn code(parts: &[FileAttribute], g: Geometry) -> &'static str {
        let refs: Vec<&FileAttribute> = parts.iter().collect();
        build_layout(types::DATA, "", &refs, g).unwrap_err().code()
    }

    #[test]
    fn layout_rules_without_records() {
        let ok = nr(5, 0, 0, 3, 0, (4 * 4096, 16000, 16000));
        let refs = [&ok];
        let l = build_layout(types::DATA, "", &refs, G).unwrap();
        assert_eq!((l.data_size, l.runs.len(), l.segments.len()), (16000, 1, 1));
        assert_eq!(code(&[], G), "stream_not_found");
        assert_eq!(
            code(&[nr(5, 0, 0, 3, 0, (4 * 4096, 16000, 17000))], G),
            "stream_sizes"
        );
        assert_eq!(
            code(&[nr(5, 0, 0, 3, 0, (4 * 4096, 17000 * 4, 16000))], G),
            "stream_sizes"
        );
        assert_eq!(
            code(&[nr(5, 0, 4, 7, 0, (0, 0, 0))], G),
            "stream_missing_first_extent"
        );
        let res = FileAttribute {
            value: AttrValue::Resident {
                value: b"x".to_vec(),
                indexed: false,
            },
            ..ok.clone()
        };
        assert_eq!(code(&[res.clone(), ok.clone()], G), "stream_mixed_forms");
        assert_eq!(code(&[res.clone(), res], G), "stream_mixed_forms");
        let big = Geometry {
            cluster_size: 8192,
            total_clusters: 1000,
        };
        assert_eq!(
            code(
                &[nr(5, attr_flags::COMPRESSED, 0, 15, 4, (16 * 8192, 10, 10))],
                big
            ),
            "stream_compression_cluster_size"
        );
        // Unidad de compresión sin indicador: aviso; en un disperso, nada.
        let refs_cu = nr(5, 0, 0, 3, 4, (4 * 4096, 16000, 16000));
        let l = build_layout(types::DATA, "", &[&refs_cu], G).unwrap();
        assert_eq!(
            l.issues,
            vec![StreamIssue::CompressionUnitIgnored { value: 4 }]
        );
        let sp = nr(5, attr_flags::SPARSE, 0, 3, 4, (4 * 4096, 16000, 16000));
        assert!(build_layout(types::DATA, "", &[&sp], G)
            .unwrap()
            .issues
            .is_empty());
        // Runs fuera del volumen: aviso y tramo sin mapa.
        let small = Geometry {
            cluster_size: 4096,
            total_clusters: 12,
        };
        let l = build_layout(types::DATA, "", &[&ok], small).unwrap();
        assert_eq!(l.issues[0].code(), "runs_outside_volume");
        assert_eq!(l.segments[0].kind, SegmentKind::Unmapped);
        // Códigos estables de errores y avisos.
        let mut codes = vec![
            StreamError::Encrypted.code(),
            StreamError::DuplicateExtent { start_vcn: 0 }.code(),
        ];
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 2);
    }
}
