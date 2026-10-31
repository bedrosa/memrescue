//! API de alto nivel del motor. La consumen la CLI (directamente) y la GUI (a través de `mr-ffi`).
//!
//! Operaciones disponibles (todas de solo lectura):
//! - [`probe_format`]: identifica el formato de un fichero.
//! - [`open_source`] / [`open_with`] / [`image_info`]: abre una imagen RAW o un contenedor
//!   soportado (VHD, VHDX, VMDK monolithicSparse) y devuelve sus datos.
//! - [`discover_split`] / [`open_split_source`]: imágenes divididas (apertura explícita).
//! - [`vmdk_required_extents`] / [`open_vmdk_with_extents`]: VMDK con descriptor separado; las
//!   extensiones solo se abren si el usuario aprueba cada fichero (#11).
//! - [`external_hints`] / [`open_with_parents`]: discos diferenciales
//!   VHD/VHDX; el contenedor solo da indicaciones sobre su padre y la cadena se abre con los
//!   ficheros que el usuario eligió y confirmó (ADR-018, addendum).
//! - [`open_range`]: un rango de sectores de otro origen como origen propio.
//! - [`partitions`]: tabla de particiones del origen (MBR/EBR, MBR protector o híbrido) con la
//!   confianza y las anomalías de cada partición; [`open_partition`]: una partición como origen.
//! - [`read_sectors`]: lectura estricta de un rango de sectores; [`read_sectors_mapped`]: lectura
//!   tolerante con el estado de cada sector (visor hexadecimal); [`read_sectors_mapped_with`]: la
//!   misma con salto y patrones de relleno configurables.
//! - [`start_partition_search_task`]: búsqueda exprés de particiones perdidas (cancelable, con
//!   progreso respecto al presupuesto de lecturas); [`open_found_volume`]: un volumen encontrado
//!   como origen.
//! - [`ntfs_open`]: detecta y abre el volumen NTFS de un origen (imagen completa, partición o
//!   volumen encontrado): arranque usado, MFT, `$MFTMirr` y lectura de registros (#20).
//! - [`ntfs_file`], [`ntfs_open_stream`], [`ntfs_read_stream`] y [`ntfs_copy_stream`]: atributos
//!   de un fichero por número de registro MFT y contenido de sus flujos `$DATA` (principal o
//!   alternativos) con el estado de cada tramo (#21); [`check_output_path`] valida el destino de
//!   una extracción y [`create_output`] lo escribe de forma atómica (fichero temporal en el mismo
//!   directorio y renombrado final; nunca sobrescribe sin petición explícita).
//! - [`partition_backup`], [`save_partition_backup`], [`load_partition_backup`] y
//!   [`compare_partition_backup`]: copia del particionado en formato propio y comparación con un
//!   origen, sin escribir nunca en él (#18).
//! - [`ntfs_deleted`] / [`start_ntfs_deleted_task`]: ficheros borrados de la MFT y entradas
//!   residuales de los índices con su recuperabilidad (cancelable, con progreso);
//!   [`ntfs_recover_record`], [`ntfs_recover_stream`], [`ntfs_recover_deleted`] y
//!   [`start_ntfs_recover_task`]: recuperación de un borrado con el estado de cada tramo (#23).
//! - [`ntfs_list_dir`], [`ntfs_resolve`], [`ntfs_path_of`] y [`start_ntfs_tree_task`] (#22):
//!   directorios contrastados con la MFT, rutas y árbol completo cancelable con progreso;
//!   [`ntfs_stream_source`] presenta un fichero como origen (visor) y [`ntfs_copy_file`] /
//!   [`start_ntfs_copy_task`] lo copian con la política de [`create_output`].
//! - [`vfs_build_ntfs`] / [`start_vfs_build_task`] y [`vfs_query`] (#25): árbol virtual de un
//!   volumen NTFS (vivos, borrados, huérfanos y metadatos con el estado de sus datos) como tarea
//!   cancelable con progreso, y consultas con filtro (`mr_vfs`).
//! - [`start_hash_task`]: hashes del fichero completo en segundo plano.
//! - [`start_verify_task`]: lectura completa tolerante a errores con mapa de validez por sector.

mod ntfs;
pub mod task;
mod vfs;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_image::OpenedImage;
use mr_io::resilient::{read_range, OnError, ReadPolicy, ReadReport};
use mr_io::{BlockDevice, ImageFormat, RangeView, SectorState};
use mr_store::{JobState, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use task::{Task, TaskContext};
use thiserror::Error;

pub use mr_common::ENGINE_VERSION;
pub use mr_fs_ntfs;
pub use mr_image::vmdk::{ExtentAccess as VmdkExtentAccess, ExtentType as VmdkExtentType};
pub use mr_image::vmdk_extents::{
    extent_access_name, extent_type_name, ExtentPlan as VmdkExtentPlan, Issue as VmdkExtentIssue,
    IssueKind as VmdkIssueKind, RequiredExtent as VmdkRequiredExtent,
};
use mr_image::ApprovedFiles;
/// Máximo de ficheros aprobados en [`open_vmdk_with_extents`].
pub const MAX_VMDK_APPROVED_FILES: usize = mr_image::external::MAX_APPROVED_FILES;
pub use mr_image::ExternalHint;
pub use mr_image::OpenOptions;
pub use mr_part;
pub use mr_part::search::{SearchOptions, SearchResult};
pub use mr_part::{Partition, PartitionTable};
pub use mr_vfs;
pub use ntfs::{
    ntfs_copy_file, ntfs_list_dir, ntfs_path_of, ntfs_resolve, ntfs_stream_source, ntfs_tree,
    start_ntfs_copy_task, start_ntfs_tree_task, NtfsCopyResult, STREAM_SOURCE_SECTOR,
};
pub use vfs::{start_vfs_build_task, vfs_build_ntfs, vfs_query};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Io(#[from] mr_io::IoError),
    #[error(transparent)]
    Store(#[from] mr_store::StoreError),
    #[error(transparent)]
    Ntfs(#[from] mr_fs_ntfs::NtfsError),
    /// Árbol virtual (#25): límite de la arena, filtro no válido o nodo inexistente.
    #[error(transparent)]
    Vfs(#[from] mr_vfs::VfsError),
    #[error("operación cancelada")]
    Cancelled,
    #[error("argumento no válido: {0}")]
    InvalidArgument(String),
    /// El fichero de salida ya existe y no se pidió sobrescribirlo.
    #[error("la salida {} ya existe: no se sobrescribe sin una petición explícita", .0.display())]
    OutputExists(PathBuf),
    /// No se puede comprobar que la salida no sea el origen (un fichero del origen, la propia
    /// salida o su directorio no se pueden resolver): la escritura se rechaza (#76).
    #[error(
        "no se puede comprobar que la salida {} no sea el origen ({reason}): no se escribe",
        .output.display()
    )]
    OutputNotVerifiable { output: PathBuf, reason: String },
    #[error("no se pudo iniciar la tarea: {0}")]
    Spawn(std::io::Error),
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Origen abierto (imagen RAW, contenedor, imagen dividida o rango de otro origen), siempre de
/// solo lectura. Se comparte entre lecturas y tareas.
pub type Source = Arc<OpenSource>;

/// Dispositivo abierto junto con sus metadatos.
pub struct OpenSource {
    dev: Arc<dyn BlockDevice>,
    info: ImageInfo,
}

impl OpenSource {
    fn new(dev: Arc<dyn BlockDevice>, meta: Meta) -> Self {
        let info = ImageInfo {
            path: meta.path,
            size_bytes: dev.size(),
            sector_size: dev.sector_size(),
            sector_count: dev.sector_count(),
            trailing_bytes: dev.trailing_bytes(),
            format: meta.format,
            detected: meta.detected,
            description: meta.description,
            segments: meta.segments,
            range: meta.range,
            external_files: meta.external_files,
        };
        Self { dev, info }
    }

    pub fn info(&self) -> &ImageInfo {
        &self.info
    }

    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.dev
    }
}

impl BlockDevice for OpenSource {
    fn size(&self) -> u64 {
        self.dev.size()
    }
    fn sector_size(&self) -> u32 {
        self.dev.sector_size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> mr_io::Result<()> {
        self.dev.read_at(offset, buf)
    }
    fn allocation(&self, lba: u64, count: u64) -> mr_io::Result<Vec<mr_io::AllocationRun>> {
        self.dev.allocation(lba, count)
    }
}

struct Meta {
    path: PathBuf,
    format: ImageFormat,
    detected: ImageFormat,
    description: String,
    segments: Vec<PathBuf>,
    range: Option<(u64, u64)>,
    external_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// Ruta canónica del fichero (o del primer segmento).
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sector_size: u32,
    pub sector_count: u64,
    /// Bytes sobrantes al final que no completan un sector.
    pub trailing_bytes: u64,
    /// Formato con el que se interpreta (RAW si se forzó).
    pub format: ImageFormat,
    /// Formato detectado en el fichero.
    pub detected: ImageFormat,
    /// Descripción legible (variante del contenedor, segmentos, rango…).
    pub description: String,
    /// Segmentos de una imagen dividida (vacío en los demás casos).
    pub segments: Vec<PathBuf>,
    /// Si el origen es un rango de otro: `(LBA inicial, nº de sectores)` relativos al origen padre
    /// (en sectores del padre).
    pub range: Option<(u64, u64)>,
    /// Ficheros externos que usa el origen (rutas canónicas): la cadena de padres de un disco
    /// diferencial, del inmediato al base. Vacío en los demás casos.
    pub external_files: Vec<PathBuf>,
}

impl From<OpenedImage> for OpenSource {
    fn from(o: OpenedImage) -> Self {
        OpenSource::new(
            o.device,
            Meta {
                path: o.path,
                format: o.format,
                detected: o.detected,
                description: o.description,
                segments: o.segments,
                range: None,
                external_files: o.external_files,
            },
        )
    }
}

/// Identifica el formato del fichero sin abrirlo como imagen.
pub fn probe_format(path: impl AsRef<Path>) -> Result<ImageFormat> {
    Ok(mr_io::probe_format(path)?)
}

/// Abre una imagen (RAW o contenedor soportado) en solo lectura. `sector_size` 0 = nativo.
pub fn open_source(path: impl AsRef<Path>, sector_size: u32) -> Result<Source> {
    open_with(
        path,
        OpenOptions {
            sector_size,
            ..OpenOptions::default()
        },
    )
}

/// Abre una imagen con opciones explícitas (p. ej. forzar RAW o el modo tolerante de los
/// contenedores, cuya descripción indica entonces cuántos bloques se descartaron).
pub fn open_with(path: impl AsRef<Path>, opts: OpenOptions) -> Result<Source> {
    Ok(Arc::new(mr_image::open_image(path.as_ref(), opts)?.into()))
}

/// Abre un disco diferencial VHD/VHDX con la cadena de padres que el usuario aprobó
/// explícitamente: `parents` en orden (padre inmediato primero, hasta el disco base). Cada ruta
/// pasa la política de rutas y se abre en solo lectura (`mr_image::ApprovedFiles`, papeles
/// `parent:1`, `parent:2`…); el motor valida cada padre (formato, tamaño, sector, vínculo de
/// identidad, ciclos, profundidad ≤ 16) y nunca abre otro fichero. Con `ignore_link_mismatch`
/// (petición explícita del usuario; por defecto no) se acepta un padre cuyo identificador no
/// coincide y la descripción del origen lo advierte. Una lista mal formada (el mismo fichero dos
/// veces) es `InvalidArgument`; si la imagen no es diferencial y la lista no está vacía, falla.
pub fn open_with_parents<P: AsRef<Path>>(
    path: impl AsRef<Path>,
    opts: OpenOptions,
    parents: &[P],
    ignore_link_mismatch: bool,
) -> Result<Source> {
    let invalid = |e: mr_io::IoError| match e {
        mr_io::IoError::Io(io) if io.kind() == std::io::ErrorKind::InvalidInput => {
            ApiError::InvalidArgument(io.to_string())
        }
        other => ApiError::Io(other),
    };
    let approved = mr_image::ApprovedFiles::new(
        parents
            .iter()
            .enumerate()
            .map(|(i, p)| (mr_image::parent_role(i + 1), p.as_ref())),
    )
    .map_err(invalid)?;
    let opened =
        mr_image::open_image_with_parents(path.as_ref(), opts, &approved, ignore_link_mismatch)?;
    Ok(Arc::new(opened.into()))
}

/// Indicaciones del contenedor sobre los ficheros externos que necesita (padre de un disco
/// diferencial), sin abrir ningún otro fichero. Vacío si no necesita ninguno.
pub fn external_hints(path: impl AsRef<Path>) -> Result<Vec<ExternalHint>> {
    Ok(mr_image::external_hints(path.as_ref())?)
}

/// Presenta un dispositivo ya construido (en memoria, virtual o con fallos simulados) como origen,
/// con `description` como descripción legible. Se interpreta como RAW y no tiene ruta.
pub fn source_from_device(dev: Arc<dyn BlockDevice>, description: impl Into<String>) -> Source {
    Arc::new(OpenSource::new(
        dev,
        Meta {
            path: PathBuf::new(),
            format: ImageFormat::Raw,
            detected: ImageFormat::Raw,
            description: description.into(),
            segments: Vec::new(),
            range: None,
            external_files: Vec::new(),
        },
    ))
}

/// Lista los segmentos de una imagen dividida a partir del primero, sin abrirlos.
pub fn discover_split(first: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    Ok(mr_image::split::discover(first.as_ref())?)
}

/// Abre una imagen dividida (petición explícita del usuario tras ver la lista de segmentos).
pub fn open_split_source(first: impl AsRef<Path>, opts: OpenOptions) -> Result<Source> {
    Ok(Arc::new(mr_image::open_split(first.as_ref(), opts)?.into()))
}

/// Lista las extensiones que necesita un descriptor VMDK separado (nombre declarado, tipo,
/// acceso, tamaño y problemas) sin abrir ninguna (incidencia 11).
pub fn vmdk_required_extents(descriptor: impl AsRef<Path>) -> Result<VmdkExtentPlan> {
    Ok(mr_image::list_vmdk_extents(descriptor.as_ref())?)
}

/// Abre un VMDK con descriptor separado usando **solo** los ficheros aprobados por el usuario:
/// `extents` asigna a cada nombre declarado en el descriptor (coincidencia exacta) el fichero
/// elegido. El motor no busca ni abre ningún otro fichero (ADR-018). Una lista mal formada
/// (nombre repetido, dos nombres con el mismo fichero, nombre que no corresponde a ninguna
/// extensión necesaria, el propio descriptor) es `InvalidArgument`.
pub fn open_vmdk_with_extents(
    descriptor: impl AsRef<Path>,
    extents: &[(String, PathBuf)],
    opts: OpenOptions,
) -> Result<Source> {
    let invalid = |e: mr_io::IoError| match e {
        mr_io::IoError::Io(io) if io.kind() == std::io::ErrorKind::InvalidInput => {
            ApiError::InvalidArgument(io.to_string())
        }
        other => ApiError::Io(other),
    };
    let approved =
        ApprovedFiles::new(extents.iter().map(|(n, p)| (n.as_str(), p))).map_err(invalid)?;
    let opened =
        mr_image::open_vmdk_extents(descriptor.as_ref(), &approved, opts).map_err(invalid)?;
    Ok(Arc::new(opened.into()))
}

/// Presenta `count` sectores de `parent` desde `start_lba` como un origen independiente
/// (partición o rango elegido por el usuario).
pub fn open_range(parent: &Source, start_lba: u64, count: u64) -> Result<Source> {
    open_view(parent, start_lba, count, |end| {
        format!(
            "Rango LBA {start_lba}–{end} de: {}",
            parent.info().description
        )
    })
}

/// Vista acotada de `parent` con la descripción que construye `describe` a partir de la última LBA.
fn open_view(
    parent: &Source,
    start_lba: u64,
    count: u64,
    describe: impl FnOnce(u64) -> String,
) -> Result<Source> {
    let dev: Arc<dyn BlockDevice> = Arc::clone(parent) as Arc<dyn BlockDevice>;
    let view = RangeView::new(dev, start_lba, count)?;
    let p = parent.info();
    let end = start_lba + count - 1; // RangeView::new ya validó el rango (count > 0, sin desbordes)
    let description = describe(end);
    Ok(Arc::new(OpenSource::new(
        Arc::new(view),
        Meta {
            path: p.path.clone(),
            format: p.format,
            detected: p.detected,
            description,
            segments: p.segments.clone(),
            range: Some((start_lba, count)),
            external_files: p.external_files.clone(),
        },
    )))
}

/// Tabla de particiones del origen (solo lectura, tolerante a sectores dañados): esquema, particiones
/// con su confianza y anomalías. Ver `mr_part::read_partition_table`.
pub fn partitions(src: &dyn BlockDevice) -> Result<PartitionTable> {
    Ok(mr_part::read_partition_table(src)?)
}

/// Abre la partición `index` de `table` (leída de `parent`) como origen independiente (vista
/// acotada, `RangeView`). Si termina fuera del origen se abre solo la parte que existe y la
/// descripción lo indica. Se rechaza con [`ApiError::InvalidArgument`] si empieza fuera, si la
/// tabla usa otro tamaño de sector o si es una partición GPT de una imagen desplazada y no se pide
/// `apply_shift`; con él, la LBA de la imagen se calcula con el desplazamiento informado y la
/// descripción lo dice (ADR-022, adenda). Nunca se aplica un desplazamiento en silencio.
pub fn open_partition(
    parent: &Source,
    table: &PartitionTable,
    index: usize,
    apply_shift: bool,
) -> Result<Source> {
    let info = parent.info();
    let c = mr_part::open_coordinates(
        table,
        index,
        info.sector_size,
        info.sector_count,
        apply_shift,
    )
    .map_err(ApiError::InvalidArgument)?;
    let p = &table.partitions[index];
    let kind = match &p.kind {
        mr_part::PartitionKind::Mbr(t) => format!("tipo 0x{t:02X}"),
        mr_part::PartitionKind::Gpt(g) => format!("GPT {}", mr_part::gpt::format_guid(g)),
        mr_part::PartitionKind::Apm(t) => format!("APM {}", t.text()),
    };
    open_view(parent, c.image_start, c.count, |end| {
        let mut d = if c.shift_applied != 0 {
            format!(
                "Partición LBA {}–{} del disco = LBA {}–{end} de la imagen (desplazamiento aplicado: {} sectores, LBA imagen = LBA disco - {}) ({kind}) de: {}",
                p.start_lba,
                p.start_lba.saturating_add(c.count - 1),
                c.image_start,
                c.shift_applied,
                c.shift_applied,
                info.description
            )
        } else {
            format!(
                "Partición LBA {}–{end} ({kind}) de: {}",
                c.image_start, info.description
            )
        };
        if c.missing_at_end > 0 {
            d.push_str(&format!(
                "; recortada: faltan {} sectores al final del origen",
                c.missing_at_end
            ));
        }
        d
    })
}

/// Detecta y abre el volumen NTFS que empieza en el primer byte de `src` (una imagen sin tabla,
/// una partición de [`open_partition`] o un volumen de [`open_found_volume`]). Solo lectura. Si el
/// arranque principal no es válido se usa su copia y se informa; los registros dañados o ilegibles
/// se informan al leerlos. Error [`ApiError::Ntfs`] si no es NTFS o la MFT no se puede localizar.
pub fn ntfs_open(src: &Source) -> Result<mr_fs_ntfs::NtfsVolume> {
    let dev: Arc<dyn BlockDevice> = Arc::clone(src) as Arc<dyn BlockDevice>;
    Ok(mr_fs_ntfs::NtfsVolume::open(dev)?)
}

/// Fichero del registro base `record` (atributos del registro y de sus extensiones, anomalías).
/// Vale también para registros no en uso (borrados).
pub fn ntfs_file(vol: &mr_fs_ntfs::NtfsVolume, record: u64) -> Result<mr_fs_ntfs::NtfsFile> {
    Ok(vol.file(record)?)
}

/// Abre el flujo `$DATA` `stream` (`""` = el principal) del registro base `record`.
pub fn ntfs_open_stream<'v>(
    vol: &'v mr_fs_ntfs::NtfsVolume,
    record: u64,
    stream: &str,
) -> Result<mr_fs_ntfs::DataStream<'v>> {
    Ok(vol.open_stream(record, stream)?)
}

/// Lee `len` bytes (como mucho `mr_fs_ntfs::stream::MAX_STREAM_READ`) desde `offset` del flujo
/// `stream` del registro `record`, con el estado de cada tramo: los no fiables llevan el patrón de
/// relleno y nunca son datos.
pub fn ntfs_read_stream(
    vol: &mr_fs_ntfs::NtfsVolume,
    record: u64,
    stream: &str,
    offset: u64,
    len: usize,
) -> Result<mr_fs_ntfs::StreamChunk> {
    Ok(vol.open_stream(record, stream)?.read(offset, len)?)
}

/// Copia el flujo `stream` del registro `record` a `out` con memoria acotada. El informe dice
/// cuántos bytes hay de cada estado y qué tramos no son fiables (escritos con el patrón `BAD!`).
pub fn ntfs_copy_stream(
    vol: &mr_fs_ntfs::NtfsVolume,
    record: u64,
    stream: &str,
    out: &mut dyn std::io::Write,
) -> Result<mr_fs_ntfs::CopyReport> {
    let s = vol.open_stream(record, stream)?;
    s.copy_to(out).map_err(|e| match e {
        mr_fs_ntfs::stream::CopyError::Ntfs(n) => ApiError::Ntfs(n),
        mr_fs_ntfs::stream::CopyError::Write(w) => ApiError::Io(mr_io::IoError::Io(w)),
    })
}

impl mr_fs_ntfs::ScanObserver for TaskContext {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn progress(&self, done: u64, total: u64) {
        self.set_total(total);
        self.set_done(done);
    }
}

/// Ficheros y directorios borrados del volumen (registros base no en uso con `$FILE_NAME`) con la
/// recuperabilidad de cada flujo según `$Bitmap` y los runs de los ficheros vivos, y las entradas
/// residuales de los índices de directorio combinadas con ellos (#23: una entrada por fichero con
/// todos sus orígenes; las solo residuales con registro reutilizado, dañado o libre, siempre con
/// recuperabilidad `none`). Recorre la MFT dos veces y los índices de todos los directorios;
/// progreso en registros leídos, entradas evaluadas y directorios. Al cancelar,
/// [`ApiError::Cancelled`].
pub fn ntfs_deleted(
    vol: &mr_fs_ntfs::NtfsVolume,
    ctx: &TaskContext,
) -> Result<mr_fs_ntfs::DeletedScan> {
    mr_fs_ntfs::scan_deleted(vol, &mr_fs_ntfs::ScanOptions::default(), ctx)?
        .ok_or(ApiError::Cancelled)
}

/// Lanza [`ntfs_deleted`] en segundo plano.
pub fn start_ntfs_deleted_task(
    vol: Arc<mr_fs_ntfs::NtfsVolume>,
) -> Result<Arc<Task<mr_fs_ntfs::DeletedScan>>> {
    Task::spawn(move |ctx| ntfs_deleted(&vol, ctx))
}

/// Recupera el flujo principal del registro `record` en `out` (ver [`ntfs_recover_stream`]).
pub fn ntfs_recover_record(
    src: &Source,
    vol: &mr_fs_ntfs::NtfsVolume,
    record: u64,
    out: &Path,
    overwrite: bool,
) -> Result<mr_fs_ntfs::RecoveryReport> {
    ntfs_recover_stream(
        src,
        vol,
        record,
        "",
        out,
        overwrite,
        &TaskContext::detached(),
    )
}

/// Recupera el flujo `stream` del registro `record` (borrado o no) en `out` con la política de
/// [`create_output`] (nunca el origen, nunca sin `overwrite` sobre un fichero existente,
/// escritura atómica). El informe dice qué tramos de bytes son datos, cuáles pueden no ser los
/// originales (`possibly_overwritten`: se escriben tal como están en el disco) y cuáles no se
/// pudieron leer (patrón de relleno), con el dueño actual de los clústeres. Un registro dañado o
/// ilegible es un error y no crea la salida.
pub fn ntfs_recover_stream(
    src: &Source,
    vol: &mr_fs_ntfs::NtfsVolume,
    record: u64,
    stream: &str,
    out: &Path,
    overwrite: bool,
    ctx: &TaskContext,
) -> Result<mr_fs_ntfs::RecoveryReport> {
    // Se evalúa y se abre el flujo antes de crear la salida: un error no deja ficheros.
    let (s, mut report) = mr_fs_ntfs::recover::recovery_stream(vol, record, stream, ctx)?
        .ok_or(ApiError::Cancelled)?;
    let mut w = create_output(src, out, overwrite)?;
    report.copy = s.copy_to(&mut w).map_err(|e| match e {
        mr_fs_ntfs::stream::CopyError::Ntfs(n) => ApiError::Ntfs(n),
        mr_fs_ntfs::stream::CopyError::Write(w) => ApiError::Io(mr_io::IoError::Io(w)),
    })?;
    w.commit()?;
    Ok(report)
}

/// Recupera el flujo `stream` de un fichero **borrado** (ver [`ntfs_recover_stream`]): antes de
/// crear nada exige que el registro `record` sea un registro base no en uso con la secuencia
/// `sequence` (la de la entrada de [`ntfs_deleted`]). Así nunca se recupera con el nombre de un
/// borrado el contenido de un registro reutilizado por otro fichero, ni una entrada solo residual
/// (registro reutilizado, dañado o libre: no hay runs que seguir).
#[allow(clippy::too_many_arguments)]
pub fn ntfs_recover_deleted(
    src: &Source,
    vol: &mr_fs_ntfs::NtfsVolume,
    record: u64,
    sequence: u16,
    stream: &str,
    out: &Path,
    overwrite: bool,
    ctx: &TaskContext,
) -> Result<mr_fs_ntfs::RecoveryReport> {
    let f = vol.file(record)?;
    if f.in_use() {
        return Err(ApiError::InvalidArgument(format!(
            "el registro {record} está en uso (secuencia {}): no es un fichero borrado; su \
             contenido es de otro fichero",
            f.header.sequence
        )));
    }
    if f.header.sequence != sequence {
        return Err(ApiError::InvalidArgument(format!(
            "el registro {record} tiene la secuencia {} y la entrada borrada la {sequence}: el \
             registro ya no es ese fichero",
            f.header.sequence
        )));
    }
    if f.file_names().is_empty() {
        return Err(ApiError::InvalidArgument(format!(
            "el registro {record} está libre (sin $FILE_NAME): no hay datos conocidos que recuperar"
        )));
    }
    ntfs_recover_stream(src, vol, record, stream, out, overwrite, ctx)
}

/// Lanza [`ntfs_recover_deleted`] en segundo plano (progreso de la evaluación; cancelable antes
/// de escribir: entonces no queda ninguna salida). La ruta y el registro se comprueban antes de
/// lanzar la tarea.
#[allow(clippy::too_many_arguments)]
pub fn start_ntfs_recover_task(
    src: Source,
    vol: Arc<mr_fs_ntfs::NtfsVolume>,
    record: u64,
    sequence: u16,
    stream: String,
    out: std::path::PathBuf,
    overwrite: bool,
) -> Result<Arc<Task<mr_fs_ntfs::RecoveryReport>>> {
    check_output_path(&src, &out)?;
    if !overwrite && out.exists() {
        return Err(ApiError::OutputExists(out));
    }
    Task::spawn(move |ctx| {
        ntfs_recover_deleted(&src, &vol, record, sequence, &stream, &out, overwrite, ctx)
    })
}

/// Comprueba que `out` puede ser el fichero de salida de una extracción del origen `src`: no es
/// una ruta de dispositivo (la misma lista blanca léxica que los orígenes, ADR-014, también sobre
/// la forma canónica), no es un directorio, no es un flujo alternativo (`fichero:flujo`, Windows)
/// y no es el propio origen ni uno de sus segmentos ni uno de sus ficheros externos. Nunca se
/// escribe sobre el origen.
///
/// Falla cerrada (#76): si no se puede canonicalizar un fichero del origen (borrado o renombrado
/// después de abrirlo, sin permisos…), la salida existente o el directorio de una salida nueva,
/// devuelve [`ApiError::OutputNotVerifiable`] en vez de comparar rutas sin resolver.
///
/// Equivalencias detectadas: las que resuelve `std::fs::canonicalize` (mayúsculas, prefijo
/// `\\?\`, `.` y `..`, separador `/`, nombres cortos 8.3, puntos y espacios finales que Win32
/// descarta, enlaces simbólicos y uniones en cualquier componente), comparando las rutas canónicas
/// sin distinguir mayúsculas (en todas las plataformas: solo puede rechazar de más). En Unix,
/// además, la identidad del fichero (`dev`, `ino`), que detecta enlaces duros. En Windows la
/// identidad (serie del volumen e índice del fichero) no está en Rust estable sin `unsafe`: un
/// enlace duro al origen con otro nombre **no se detecta**; [`create_output`] nunca abre un fichero
/// existente para escribir (temporal y renombrado o enlace), así que los datos del origen no
/// cambian, pero ese otro nombre pasa a ser la salida. Ver ADR-026, adenda de #76.
pub fn check_output_path(src: &Source, out: &Path) -> Result<()> {
    let raw = out.to_string_lossy();
    mr_io::path_policy::check_lexical(&raw)?;
    let unverifiable = |reason: String| ApiError::OutputNotVerifiable {
        output: out.to_path_buf(),
        reason,
    };
    if out.is_dir() {
        return Err(ApiError::InvalidArgument(format!(
            "la salida {} es un directorio",
            out.display()
        )));
    }
    #[cfg(windows)]
    if out
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(n) if n.to_string_lossy().contains(':')))
    {
        return Err(ApiError::InvalidArgument(format!(
            "la salida {} es un flujo alternativo (`fichero:flujo`): no se admite",
            out.display()
        )));
    }
    // Ruta canónica de la salida: la suya si existe; si no, la de su directorio más el nombre.
    let (canon, existing) = match std::fs::symlink_metadata(out) {
        Ok(_) => {
            let c = std::fs::canonicalize(out).map_err(|e| {
                unverifiable(format!("la salida existe pero no se puede resolver: {e}"))
            })?;
            let md = std::fs::metadata(&c).map_err(|e| {
                unverifiable(format!("no se pueden leer los metadatos de la salida: {e}"))
            })?;
            if md.is_dir() {
                return Err(ApiError::InvalidArgument(format!(
                    "la salida {} es un directorio",
                    out.display()
                )));
            }
            (c, Some(md))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let name = out.file_name().ok_or_else(|| {
                ApiError::InvalidArgument(format!(
                    "salida sin nombre de fichero: {}",
                    out.display()
                ))
            })?;
            let parent = out
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let pc = std::fs::canonicalize(parent).map_err(|e| {
                unverifiable(format!(
                    "no se puede resolver el directorio de la salida {}: {e}",
                    parent.display()
                ))
            })?;
            if !pc.is_dir() {
                return Err(ApiError::InvalidArgument(format!(
                    "{} no es un directorio",
                    parent.display()
                )));
            }
            (pc.join(name), None)
        }
        Err(e) => {
            return Err(unverifiable(format!(
                "no se puede consultar la salida: {e}"
            )))
        }
    };
    // La forma canónica tampoco puede ser un dispositivo (la resolución pudo cambiar la ruta).
    mr_io::path_policy::check_canonical(&canon.to_string_lossy())?;
    let target = comparable(&canon);
    let info = src.info();
    // Un origen sin fichero (dispositivo en memoria) no tiene nada que proteger.
    let main = (!info.path.as_os_str().is_empty()).then_some(&info.path);
    // Los segmentos (imagen dividida, extensiones VMDK) y los padres de un disco diferencial
    // también son el origen.
    for p in main
        .into_iter()
        .chain(&info.segments)
        .chain(&info.external_files)
    {
        let c = std::fs::canonicalize(p).map_err(|e| {
            unverifiable(format!(
                "no se puede resolver el fichero del origen {}: {e}",
                p.display()
            ))
        })?;
        let same_identity = match &existing {
            Some(md) => same_file_identity(md, &c).map_err(|e| {
                unverifiable(format!(
                    "no se pueden leer los metadatos del fichero del origen {}: {e}",
                    p.display()
                ))
            })?,
            None => false,
        };
        if same_identity || comparable(&c) == target {
            return Err(ApiError::InvalidArgument(format!(
                "la salida {} es el propio origen: nunca se escribe sobre él",
                out.display()
            )));
        }
    }
    Ok(())
}

/// Forma comparable de una ruta canónica: sin el prefijo `\\?\` (o `\\?\UNC\` → `\\`) y en
/// minúsculas.
fn comparable(canon: &Path) -> String {
    let s = canon.to_string_lossy();
    let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    };
    s.to_lowercase()
}

/// `true` si `out_md` (metadatos de la salida existente) es el mismo fichero que `src_canon`.
/// En Unix compara dispositivo e inodo (detecta enlaces duros). En Windows no hay una API estable
/// y segura para el índice del fichero: devuelve `false` y la comparación queda en las rutas
/// canónicas.
#[cfg(unix)]
fn same_file_identity(out_md: &std::fs::Metadata, src_canon: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let s = std::fs::metadata(src_canon)?;
    Ok(s.dev() == out_md.dev() && s.ino() == out_md.ino())
}

#[cfg(not(unix))]
fn same_file_identity(_out_md: &std::fs::Metadata, _src_canon: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// Fichero de salida en curso de [`create_output`]: se escribe en un temporal del mismo
/// directorio y solo [`OutputFile::commit`] lo publica con el nombre final. Si se descarta sin
/// confirmar (error, cancelación, pánico) o la confirmación falla, el temporal se borra: nunca
/// queda un fichero parcial con el nombre final.
pub struct OutputFile {
    writer: Option<std::io::BufWriter<std::fs::File>>,
    tmp: PathBuf,
    /// El temporal aún existe y es nuestro: `Drop` lo borra.
    tmp_live: bool,
    target: PathBuf,
    overwrite: bool,
}

/// Cómo publica [`OutputFile::commit`] el temporal sin `overwrite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Enlace duro y, si el sistema de ficheros no los admite, copia exclusiva.
    Link,
    /// Solo la copia exclusiva (para probar la alternativa en cualquier volumen).
    #[cfg(test)]
    CopyOnly,
}

impl OutputFile {
    /// Ruta del fichero temporal (para diagnóstico y pruebas).
    pub fn temp_path(&self) -> &Path {
        &self.tmp
    }

    /// Termina la escritura: vacía y sincroniza el temporal y lo publica con el nombre final.
    ///
    /// - Con `overwrite`, `std::fs::rename` sustituye el destino de forma atómica.
    /// - Sin `overwrite`, el paso final falla de forma atómica si el destino existe, aunque haya
    ///   aparecido después de [`create_output`] ([`ApiError::OutputExists`]; el destino no se
    ///   toca): enlace duro del temporal al nombre final (`std::fs::hard_link`, que nunca
    ///   sustituye; el temporal está en el mismo directorio, luego en el mismo volumen) y borrado
    ///   del temporal. Si el sistema de ficheros no admite enlaces duros (FAT, exFAT), se crea el
    ///   destino en exclusiva (`create_new`) y se copia el temporal: tampoco sustituye nunca un
    ///   fichero ajeno, pero no es atómico respecto al contenido (durante la copia el destino
    ///   existe incompleto); si la copia falla, se borra ese destino, que creó esta llamada.
    ///
    /// En cualquier error el temporal se borra.
    pub fn commit(self) -> Result<PathBuf> {
        self.commit_with(Publish::Link)
    }

    fn commit_with(mut self, how: Publish) -> Result<PathBuf> {
        let io = |e: std::io::Error| ApiError::Io(mr_io::IoError::Io(e));
        // Si algo falla a partir de aquí, `Drop` borra el temporal (`tmp_live`).
        let w = self.writer.take().expect("writer presente hasta commit");
        let f = w.into_inner().map_err(|e| io(e.into_error()))?;
        f.sync_all().map_err(io)?;
        drop(f);
        if self.overwrite {
            std::fs::rename(&self.tmp, &self.target).map_err(io)?;
            self.tmp_live = false;
            return Ok(self.target.clone());
        }
        let linked = match how {
            Publish::Link => match std::fs::hard_link(&self.tmp, &self.target) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(ApiError::OutputExists(self.target.clone()));
                }
                Err(e) if hard_link_unsupported(&e) => false,
                Err(e) => return Err(io(e)),
            },
            #[cfg(test)]
            Publish::CopyOnly => false,
        };
        if !linked {
            self.copy_exclusive()?;
        }
        // El destino ya está completo; el temporal sobra (si no se puede borrar aquí, `Drop` lo
        // reintenta).
        if std::fs::remove_file(&self.tmp).is_ok() {
            self.tmp_live = false;
        }
        Ok(self.target.clone())
    }

    /// Alternativa sin enlaces duros: crea el destino en exclusiva y copia el temporal. Si la
    /// copia falla, borra el destino (solo se llega ahí si lo creó esta llamada).
    fn copy_exclusive(&self) -> Result<()> {
        let mut dst = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.target)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ApiError::OutputExists(self.target.clone()));
            }
            Err(e) => return Err(ApiError::Io(mr_io::IoError::Io(e))),
        };
        let copied = std::fs::File::open(&self.tmp)
            .and_then(|mut src| std::io::copy(&mut src, &mut dst))
            .and_then(|_| dst.sync_all());
        drop(dst);
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&self.target);
            return Err(ApiError::Io(mr_io::IoError::Io(e)));
        }
        Ok(())
    }
}

/// `true` si el error de `std::fs::hard_link` indica que el sistema de ficheros del destino no
/// admite enlaces duros (FAT y exFAT en Windows devuelven `ERROR_INVALID_FUNCTION`; otros,
/// `ERROR_NOT_SUPPORTED`; en Unix, `EPERM` o `EOPNOTSUPP`). Cualquier otro error se devuelve tal
/// cual: la copia exclusiva no se intenta a ciegas.
fn hard_link_unsupported(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED.
    #[cfg(windows)]
    const CODES: &[i32] = &[1, 50];
    // EPERM, EOPNOTSUPP.
    #[cfg(target_os = "linux")]
    const CODES: &[i32] = &[1, 95];
    // EPERM, EOPNOTSUPP (BSD, macOS).
    #[cfg(all(unix, not(target_os = "linux")))]
    const CODES: &[i32] = &[1, 45];
    #[cfg(not(any(windows, unix)))]
    const CODES: &[i32] = &[];
    e.raw_os_error().is_some_and(|c| CODES.contains(&c))
}

impl std::io::Write for OutputFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.writer.as_mut() {
            Some(w) => w.write(buf),
            None => Err(std::io::Error::other("salida ya cerrada")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.writer.as_mut() {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for OutputFile {
    fn drop(&mut self) {
        // Se cierra antes de borrar (en Windows no se borra un fichero abierto).
        drop(self.writer.take());
        if self.tmp_live {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Crea el fichero de salida de una extracción del origen `src` con la política común (la misma
/// que usará la recuperación de M4): [`check_output_path`] (nunca el origen ni un dispositivo ni un
/// directorio); si `out` ya existe y `overwrite` es `false`, [`ApiError::OutputExists`]; la
/// escritura va a un temporal nuevo (`.<nombre>.<pid>-<n>.mrtmp`) del mismo directorio que se
/// publica al confirmar ([`OutputFile::commit`], que sin `overwrite` tampoco sustituye un destino
/// creado entretanto) y se borra si no se confirma.
pub fn create_output(src: &Source, out: &Path, overwrite: bool) -> Result<OutputFile> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    check_output_path(src, out)?;
    if !overwrite && out.exists() {
        return Err(ApiError::OutputExists(out.to_path_buf()));
    }
    let name = out.file_name().ok_or_else(|| {
        ApiError::InvalidArgument(format!("salida sin nombre de fichero: {}", out.display()))
    })?;
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut last = None;
    for _ in 0..16 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{}.{}-{n}.mrtmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => {
                return Ok(OutputFile {
                    writer: Some(std::io::BufWriter::new(f)),
                    tmp,
                    tmp_live: true,
                    target: out.to_path_buf(),
                    overwrite,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(ApiError::Io(mr_io::IoError::Io(e))),
        }
    }
    Err(ApiError::Io(mr_io::IoError::Io(last.unwrap_or_else(
        || std::io::Error::other("no se pudo crear el fichero temporal"),
    ))))
}

// ---------------------------------------------------------------------------------------------
// Búsqueda exprés de particiones perdidas (#15, ADR-023)
// ---------------------------------------------------------------------------------------------

/// Resultado de la búsqueda junto con la tabla con la que se comparó.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSearch {
    pub table: PartitionTable,
    pub result: SearchResult,
}

impl mr_part::search::SearchObserver for TaskContext {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn progress(&self, p: &mr_part::search::SearchProgress) {
        self.set_done(p.reads);
    }
}

/// Lee la tabla del origen y busca volúmenes perdidos (`mr_part::search::search`). El progreso
/// se expresa en lecturas respecto a `max_reads` (la búsqueda no recorre el disco). Al cancelar
/// devuelve el resultado parcial con estado `Cancelled` y marca la tarea como cancelada.
pub fn search_partitions(
    src: &dyn BlockDevice,
    opts: &SearchOptions,
    ctx: &TaskContext,
) -> Result<PartitionSearch> {
    opts.validate().map_err(ApiError::InvalidArgument)?;
    ctx.set_total(opts.max_reads);
    let table = mr_part::read_partition_table(src)?;
    let result =
        mr_part::search::search(src, Some(&table), opts, ctx).map_err(ApiError::InvalidArgument)?;
    if result.status == mr_part::search::SearchStatus::Cancelled {
        ctx.finish_cancelled_with_result();
    }
    Ok(PartitionSearch { table, result })
}

/// Lanza la búsqueda en segundo plano. Las opciones se validan antes de lanzarla.
pub fn start_partition_search_task(
    src: Source,
    opts: SearchOptions,
) -> Result<Arc<Task<PartitionSearch>>> {
    opts.validate().map_err(ApiError::InvalidArgument)?;
    Task::spawn(move |ctx| search_partitions(src.as_ref(), &opts, ctx))
}

/// Abre el volumen encontrado `index` de `found` (buscado en `parent`) como origen independiente.
///
/// Reglas (ADR-022 y ADR-023): un volumen parcial (empieza antes de la imagen) o cuyo inicio no es
/// múltiplo del sector del origen se rechaza siempre; uno cuya posición depende del desplazamiento
/// detectado ([`mr_part::search::FoundVolume::requires_shift`]) solo se abre con `apply_shift`, y
/// la descripción lo dice; si termina fuera del origen se abre la parte que existe.
pub fn open_found_volume(
    parent: &Source,
    found: &SearchResult,
    index: usize,
    apply_shift: bool,
) -> Result<Source> {
    let info = parent.info();
    let v = found.volumes.get(index).ok_or_else(|| {
        ApiError::InvalidArgument(format!("no existe el volumen encontrado {}", index + 1))
    })?;
    if found.sector_size != info.sector_size || found.sectors != info.sector_count {
        return Err(ApiError::InvalidArgument(
            "la búsqueda se hizo sobre otro origen o con otro tamaño de sector".into(),
        ));
    }
    if v.partial() {
        return Err(ApiError::InvalidArgument(format!(
            "el volumen empieza {} bytes antes del inicio de la imagen: es parcial y no se puede abrir",
            -v.start_byte
        )));
    }
    let ss = u64::from(info.sector_size);
    let start = v.start_byte as u64;
    if !start.is_multiple_of(ss) {
        return Err(ApiError::InvalidArgument(format!(
            "el volumen empieza en el byte {start}, que no es múltiplo del sector del origen ({ss} B)"
        )));
    }
    let delta = found.shift_bytes.unwrap_or(0);
    if v.requires_shift() && !apply_shift {
        return Err(ApiError::InvalidArgument(format!(
            "la imagen está desplazada {} sectores respecto al disco y el volumen declara otro inicio: \
             ábralo pidiendo explícitamente aplicar el desplazamiento",
            delta.div_euclid(ss as i64)
        )));
    }
    let lba = start / ss;
    if lba >= info.sector_count {
        return Err(ApiError::InvalidArgument(format!(
            "el volumen empieza en la LBA {lba}, fuera del origen ({} sectores)",
            info.sector_count
        )));
    }
    let wanted = v.sector_count(info.sector_size);
    let count = wanted.min(info.sector_count - lba);
    open_view(parent, lba, count, |end| {
        let mut d = format!(
            "Volumen encontrado {} LBA {lba}–{end} ({}) de: {}",
            v.fs.name(),
            if v.listed() {
                "partición de la tabla"
            } else {
                "fuera de la tabla"
            },
            info.description
        );
        if v.requires_shift() {
            d.push_str(&format!(
                "; desplazamiento detectado aplicado: el volumen declara la LBA {} del disco ({} sectores)",
                v.declared_start,
                delta.div_euclid(ss as i64)
            ));
        }
        if u64::from(v.fs_sector_size) != ss {
            d.push_str(&format!(
                "; el sistema de archivos usa sectores de {} B",
                v.fs_sector_size
            ));
        }
        if count < wanted {
            d.push_str(&format!(
                "; recortado: faltan {} sectores al final del origen",
                wanted - count
            ));
        }
        d
    })
}

// ---------------------------------------------------------------------------------------------
// Copia del particionado (#18, PT-10): formato propio en docs/formatos/copia-particionado.md.
// Solo lectura del origen: comparar nunca escribe en ningún disco (restaurar llega con M12).
// ---------------------------------------------------------------------------------------------

pub use mr_part::backup::{
    BackupError as PartitionBackupError, Comparison as PartitionBackupComparison,
    LayoutBackup as PartitionBackup,
};

/// Toma la copia del particionado de `src`: tabla interpretada completa y bytes de sus sectores de
/// tabla. `search`, si se da, se incluye y debe proceder del mismo origen (tamaño de sector y
/// número de sectores).
pub fn partition_backup(src: &Source, search: Option<&SearchResult>) -> Result<PartitionBackup> {
    let info = src.info();
    if let Some(r) = search {
        if r.sector_size != info.sector_size || r.sectors != info.sector_count {
            return Err(ApiError::InvalidArgument(
                "la búsqueda se hizo sobre otro origen o con otro tamaño de sector".into(),
            ));
        }
    }
    Ok(mr_part::backup::capture(
        src.as_ref(),
        &info.description,
        search,
    )?)
}

/// Guarda la copia en `out` con la política común de salida ([`create_output`]): nunca sobre el
/// origen, nunca sobrescribe sin `overwrite`, escritura atómica.
pub fn save_partition_backup(
    src: &Source,
    backup: &PartitionBackup,
    out: &Path,
    overwrite: bool,
) -> Result<PathBuf> {
    use std::io::Write;
    let mut w = create_output(src, out, overwrite)?;
    w.write_all(backup.to_text().as_bytes())
        .map_err(|e| ApiError::Io(mr_io::IoError::Io(e)))?;
    w.commit()
}

/// Lee y valida una copia guardada (fichero regular de como mucho
/// [`mr_part::backup::MAX_FILE_BYTES`]). Una copia alterada, truncada o de otra versión se
/// rechaza con [`ApiError::InvalidArgument`] y el motivo.
pub fn load_partition_backup(path: impl AsRef<Path>) -> Result<PartitionBackup> {
    use std::io::Read;
    let path = path.as_ref();
    let io = |e| ApiError::Io(mr_io::IoError::Io(e));
    let f = std::fs::File::open(path).map_err(io)?;
    let meta = f.metadata().map_err(io)?;
    if !meta.is_file() {
        return Err(ApiError::InvalidArgument(format!(
            "{} no es un fichero",
            path.display()
        )));
    }
    let max = mr_part::backup::MAX_FILE_BYTES;
    let mut bytes = Vec::new();
    f.take(max + 1).read_to_end(&mut bytes).map_err(io)?;
    PartitionBackup::from_bytes(&bytes).map_err(|e| ApiError::InvalidArgument(e.to_string()))
}

/// Compara la copia con el origen sin escribir nada: bytes de las regiones de tabla y
/// particionado interpretado ([`mr_part::backup::compare`]).
pub fn compare_partition_backup(
    src: &Source,
    backup: &PartitionBackup,
) -> Result<PartitionBackupComparison> {
    Ok(mr_part::backup::compare(backup, src.as_ref())?)
}

/// Abre la imagen en solo lectura y devuelve sus datos básicos.
pub fn image_info(path: impl AsRef<Path>, sector_size: u32) -> Result<ImageInfo> {
    let src = open_source(path, sector_size)?;
    Ok(src.info().clone())
}

/// Lee `count` sectores desde `lba` con validación estricta (rango, tamaño máximo por petición).
pub fn read_sectors(src: &dyn BlockDevice, lba: u64, count: u64) -> Result<Vec<u8>> {
    let (_, len) = mr_io::sector_span(src, lba, count)?;
    let mut buf = vec![0u8; len];
    src.read_sectors(lba, count, &mut buf)?;
    Ok(buf)
}

/// Lectura acotada para el visor con el **estado de cada sector**: los ilegibles se entregan con
/// el patrón de relleno y estado `Unreadable` (nunca como datos); los dispersos, como ceros
/// `Sparse`. `retries` = reintentos por sector tras un fallo.
pub fn read_sectors_mapped(
    src: &dyn BlockDevice,
    lba: u64,
    count: u64,
    retries: u32,
) -> Result<(Vec<u8>, Vec<SectorState>)> {
    let policy = ReadPolicy {
        retries,
        on_error: OnError::FillAndContinue,
        ..ReadPolicy::default()
    };
    Ok(mr_io::resilient::read_sectors_mapped(
        src, lba, count, &policy,
    )?)
}

/// Opciones de [`read_sectors_mapped_with`]: reintentos, salto tras un ilegible y patrones de
/// relleno. Por defecto equivale a [`read_sectors_mapped`] con 1 reintento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedReadOptions {
    /// Reintentos por sector tras un fallo.
    pub retries: u32,
    /// Tras un sector ilegible, sectores que se marcan `Skipped` sin leerlos (0 = sin salto).
    pub jump_sectors: u64,
    /// Tras un salto, leer hacia atrás desde su final hasta el siguiente error.
    pub read_back: bool,
    /// Relleno de los sectores `Unreadable`.
    pub bad_fill: [u8; 4],
    /// Relleno de los sectores `Skipped`.
    pub skip_fill: [u8; 4],
}

impl Default for MappedReadOptions {
    fn default() -> Self {
        Self {
            retries: 1,
            jump_sectors: 0,
            read_back: false,
            bad_fill: mr_io::resilient::DEFAULT_BAD_FILL,
            skip_fill: mr_io::resilient::DEFAULT_SKIP_FILL,
        }
    }
}

/// Como [`read_sectors_mapped`], con salto y patrones de relleno configurables. Los patrones se
/// validan con [`mr_io::resilient::validate_fill_patterns`] (ni uniformes, como `00 00 00 00`, ni
/// iguales entre sí) y se rechazan con [`ApiError::InvalidArgument`] antes de leer nada. El estado
/// de cada sector sigue siendo la única fuente de verdad sobre si su contenido son datos.
pub fn read_sectors_mapped_with(
    src: &dyn BlockDevice,
    lba: u64,
    count: u64,
    opts: &MappedReadOptions,
) -> Result<(Vec<u8>, Vec<SectorState>)> {
    mr_io::resilient::validate_fill_patterns(opts.bad_fill, opts.skip_fill)
        .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
    let policy = ReadPolicy {
        retries: opts.retries,
        on_error: OnError::FillAndContinue,
        bad_fill: opts.bad_fill,
        skip_fill: opts.skip_fill,
        jump_sectors: opts.jump_sectors,
        read_back: opts.read_back,
        ..ReadPolicy::default()
    };
    Ok(mr_io::resilient::read_sectors_mapped(
        src, lba, count, &policy,
    )?)
}

// ---------------------------------------------------------------------------------------------
// Hashes
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashResult {
    pub bytes: u64,
    pub digests: Vec<(HashAlgorithm, String)>,
}

/// Opciones del trabajo de hash.
#[derive(Debug, Clone)]
pub struct HashOptions {
    pub algorithms: Vec<HashAlgorithm>,
    /// Tamaño de lectura por bloque.
    pub chunk_size: usize,
    /// Si se indica, el trabajo se registra en esta base de datos de trabajo.
    pub store_path: Option<PathBuf>,
}

impl Default for HashOptions {
    fn default() -> Self {
        Self {
            algorithms: vec![HashAlgorithm::Sha256],
            chunk_size: 4 * 1024 * 1024,
            store_path: None,
        }
    }
}

/// Calcula hashes del contenido completo (incluidos los bytes finales que no completan un
/// sector). Es estricto: cualquier error de lectura aborta, porque un hash con datos rellenados
/// no representaría el origen.
pub fn hash_image(
    dev: &dyn BlockDevice,
    opts: &HashOptions,
    ctx: &TaskContext,
) -> Result<HashResult> {
    if opts.algorithms.is_empty() {
        return Err(ApiError::InvalidArgument(
            "no se indicó ningún algoritmo".into(),
        ));
    }
    let chunk = opts.chunk_size.clamp(512, mr_io::MAX_READ_REQUEST);
    let total = dev.size();
    ctx.set_total(total);
    let mut hasher = MultiHasher::new(&opts.algorithms);
    let mut buf = vec![0u8; chunk];
    let mut pos = 0u64;
    while pos < total {
        if ctx.is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        let n = usize::try_from((total - pos).min(chunk as u64)).unwrap_or(chunk);
        dev.read_at(pos, &mut buf[..n])?;
        hasher.update(&buf[..n]);
        pos += n as u64;
        ctx.set_done(pos);
    }
    Ok(HashResult {
        bytes: total,
        digests: hasher.finish(),
    })
}

/// Lanza el cálculo de hashes de una imagen en segundo plano.
pub fn start_hash_task(
    path: PathBuf,
    sector_size: u32,
    opts: HashOptions,
) -> Result<Arc<Task<HashResult>>> {
    // Se abre aquí para devolver errores de ruta de forma síncrona.
    let src = open_source(&path, sector_size)?;
    start_hash_task_on(src, opts)
}

/// Hash de un origen ya abierto.
pub fn start_hash_task_on(src: Source, opts: HashOptions) -> Result<Arc<Task<HashResult>>> {
    Task::spawn(move |ctx| run_hash_job(&src, &opts, ctx))
}

fn run_hash_job(img: &OpenSource, opts: &HashOptions, ctx: &TaskContext) -> Result<HashResult> {
    let Some(db) = &opts.store_path else {
        return hash_image(img, opts, ctx);
    };
    let mut store = Store::open(db)?;
    let src = store.add_image_source(
        &img.info().path.display().to_string(),
        img.size(),
        img.sector_size(),
        None,
    )?;
    let algos: Vec<&str> = opts.algorithms.iter().map(|a| a.name()).collect();
    let params = format!(r#"{{"algorithms":["{}"]}}"#, algos.join("\",\""));
    let job = store.create_job("hash", Some(src), &params, img.size())?;
    store.set_state(job, JobState::Running, None)?;
    let res = hash_image(img, opts, ctx);
    match &res {
        Ok(r) => {
            store.set_progress(job, r.bytes, r.bytes)?;
            let pairs: Vec<String> = r
                .digests
                .iter()
                .map(|(a, h)| format!(r#""{a}":"{h}""#))
                .collect();
            store.complete(job, &format!("{{{}}}", pairs.join(",")))?;
        }
        Err(ApiError::Cancelled) => {
            store.set_progress(job, ctx.done(), ctx.total())?;
            store.set_state(job, JobState::Cancelled, None)?;
        }
        Err(e) => {
            store.set_state(job, JobState::Failed, Some(&e.to_string()))?;
        }
    }
    res
}

// ---------------------------------------------------------------------------------------------
// Verificación de legibilidad
// ---------------------------------------------------------------------------------------------

/// Opciones de la verificación (lectura completa tolerante a errores).
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub start_lba: u64,
    /// `None` = hasta el último sector completo.
    pub count: Option<u64>,
    pub policy: ReadPolicy,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            start_lba: 0,
            count: None,
            policy: ReadPolicy {
                on_error: OnError::FillAndContinue,
                ..ReadPolicy::default()
            },
        }
    }
}

/// Lee el rango completo de sectores; el progreso se expresa en sectores. El informe incluye el
/// mapa de validez (estado de cada sector: leído, recuperado, ilegible, saltado o disperso).
pub fn verify(
    dev: &dyn BlockDevice,
    opts: &VerifyOptions,
    ctx: &TaskContext,
) -> Result<ReadReport> {
    let total = dev.sector_count();
    let count = match opts.count {
        Some(c) => c,
        None => total.checked_sub(opts.start_lba).ok_or_else(|| {
            ApiError::InvalidArgument(format!("LBA inicial {} fuera del origen", opts.start_lba))
        })?,
    };
    ctx.set_total(count);
    let r = read_range(
        dev,
        opts.start_lba,
        count,
        &opts.policy,
        |_, _, _| Ok(()),
        |done, _| {
            ctx.set_done(done);
            !ctx.is_cancelled()
        },
    );
    match r {
        Err(mr_io::IoError::Cancelled) => Err(ApiError::Cancelled),
        other => Ok(other?),
    }
}

/// Lanza la verificación de un origen en segundo plano.
pub fn start_verify_task(src: Source, opts: VerifyOptions) -> Result<Arc<Task<ReadReport>>> {
    Task::spawn(move |ctx| verify(src.as_ref(), &opts, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice, SectorState};
    use std::io::Write;
    use task::TaskState;

    #[test]
    fn hash_of_memory_device_matches_known_value() {
        let dev = MemDevice::new(vec![0u8; 1024], 512).unwrap();
        let ctx = TaskContext::detached();
        let r = hash_image(&dev, &HashOptions::default(), &ctx).unwrap();
        // SHA-256 de 1024 bytes a cero.
        assert_eq!(
            r.digests[0].1,
            "5f70bf18a086007016e948b04aed3b82103a36bea41755b6cddfaf10ace3c6ef"
        );
        assert_eq!(ctx.done(), 1024);
    }

    #[test]
    fn hash_is_strict_on_read_errors() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0u8; 8192], 512).unwrap()).with_bad([9]);
        let ctx = TaskContext::detached();
        let opts = HashOptions {
            chunk_size: 1024,
            ..HashOptions::default()
        };
        assert!(matches!(
            hash_image(&dev, &opts, &ctx),
            Err(ApiError::Io(mr_io::IoError::Read { offset: 4096, .. }))
        ));
    }

    #[test]
    fn task_runs_reports_progress_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("x.img");
        std::fs::File::create(&img)
            .unwrap()
            .write_all(&vec![7u8; 3 * 1024 * 1024])
            .unwrap();
        let db = dir.path().join("w.mrdb");
        let opts = HashOptions {
            algorithms: vec![HashAlgorithm::Crc32, HashAlgorithm::Sha256],
            chunk_size: 1024 * 1024,
            store_path: Some(db.clone()),
        };
        let t = start_hash_task(img, 512, opts).unwrap();
        let r = t.wait().unwrap();
        assert_eq!(r.bytes, 3 * 1024 * 1024);
        assert_eq!(t.state(), TaskState::Completed);
        assert_eq!(t.progress(), (r.bytes, r.bytes));
        let store = Store::open(&db).unwrap();
        let jobs = store.jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Completed);
        assert!(jobs[0].result_json.as_deref().unwrap().contains("sha256"));
    }

    #[test]
    fn cancellation_stops_and_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("big.img");
        std::fs::File::create(&img)
            .unwrap()
            .write_all(&vec![1u8; 32 * 1024 * 1024])
            .unwrap();
        let db = dir.path().join("w.mrdb");
        let opts = HashOptions {
            algorithms: vec![HashAlgorithm::Sha256],
            chunk_size: 512,
            store_path: Some(db.clone()),
        };
        let t = start_hash_task(img, 512, opts).unwrap();
        t.cancel();
        let r = t.wait();
        assert!(matches!(r, Err(ApiError::Cancelled)));
        assert_eq!(t.state(), TaskState::Cancelled);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.jobs().unwrap()[0].state, JobState::Cancelled);
    }

    #[test]
    fn rejects_device_paths() {
        assert!(matches!(
            image_info(r"\\.\PhysicalDrive0", 512),
            Err(ApiError::Io(mr_io::IoError::DevicePathRejected(_)))
        ));
        assert!(matches!(
            image_info(r"\\?\C:", 512),
            Err(ApiError::Io(mr_io::IoError::DevicePathRejected(_)))
        ));
    }

    #[test]
    fn read_sectors_reads_exact_ranges_for_all_sector_sizes() {
        for ss in mr_io::COMMON_SECTOR_SIZES {
            let data: Vec<u8> = (0..16 * ss).map(|i| (i / ss) as u8).collect();
            let dev = MemDevice::new(data, ss).unwrap();
            let b = read_sectors(&dev, 15, 1).unwrap();
            assert_eq!(b.len(), ss as usize);
            assert!(b.iter().all(|&x| x == 15));
            assert!(read_sectors(&dev, 16, 1).is_err());
            assert!(read_sectors(&dev, 0, 0).is_err());
        }
    }

    #[test]
    fn verify_reports_bad_sectors_and_progress_in_sectors() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0u8; 512 * 100], 512).unwrap())
            .with_bad([10, 11, 50]);
        let ctx = TaskContext::detached();
        let r = verify(&dev, &VerifyOptions::default(), &ctx).unwrap();
        assert_eq!(
            (
                r.map.count(SectorState::Read),
                r.map.count(SectorState::Unreadable)
            ),
            (97, 3)
        );
        assert_eq!(
            r.map.untrusted_runs().collect::<Vec<_>>(),
            vec![
                (10, 2, SectorState::Unreadable),
                (50, 1, SectorState::Unreadable)
            ]
        );
        assert!(!r.map.all_trustworthy());
        assert_eq!((ctx.done(), ctx.total()), (100, 100));
    }

    #[test]
    fn verify_applies_the_error_skip_policy() {
        // 23 = ERROR_CRC, 1117 = ERROR_IO_DEVICE (simulados).
        let dev = FaultyDevice::new(MemDevice::new(vec![7u8; 512 * 64], 512).unwrap())
            .with_bad_code([10], 23)
            .with_bad_code([20], 1117)
            .with_bad([30]);
        let src = source_from_device(Arc::new(dev), "prueba");
        assert_eq!(src.info().sector_count, 64);
        let opts = VerifyOptions {
            policy: ReadPolicy {
                chunk_sectors: 8,
                retries: 3,
                on_error: OnError::FillAndContinue,
                skip_error_codes: vec![23],
                ..ReadPolicy::default()
            },
            ..VerifyOptions::default()
        };
        let r = start_verify_task(Arc::clone(&src), opts)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(r.map.count(SectorState::Unreadable), 3);
        assert_eq!(r.omitted_errors, 1);
        // 3 bloques releídos sector a sector + 3 reintentos en cada uno de los 2 no omitidos.
        assert_eq!(r.retry_reads, 3 + 6);
        assert_eq!(
            r.error_codes
                .iter()
                .map(|(&c, &n)| (c, n))
                .collect::<Vec<_>>(),
            vec![(23, 1), (1117, 1)]
        );
        let opts = VerifyOptions {
            policy: ReadPolicy {
                chunk_sectors: 8,
                retries: 3,
                on_error: OnError::FillAndContinue,
                ignore_all_errors: true,
                ..ReadPolicy::default()
            },
            ..VerifyOptions::default()
        };
        let r = verify(src.as_ref(), &opts, &TaskContext::detached()).unwrap();
        assert_eq!((r.omitted_errors, r.retry_reads), (3, 3));
        assert_eq!(r.map.count(SectorState::Read), 61);
    }

    #[test]
    fn verify_task_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("v.img");
        std::fs::write(&img, vec![0u8; 64 * 1024 * 1024]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let opts = VerifyOptions {
            policy: ReadPolicy {
                chunk_sectors: 1,
                on_error: OnError::FillAndContinue,
                ..ReadPolicy::default()
            },
            ..VerifyOptions::default()
        };
        let t = start_verify_task(src, opts).unwrap();
        t.cancel();
        assert!(matches!(t.wait(), Err(ApiError::Cancelled)));
        assert_eq!(t.state(), TaskState::Cancelled);
    }

    #[test]
    fn open_range_is_bounded_nests_and_describes_itself() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("r.img");
        let data: Vec<u8> = (0..100 * 512).map(|i| (i / 512) as u8).collect();
        std::fs::write(&img, &data).unwrap();
        let parent = open_source(&img, 0).unwrap();
        assert_eq!(parent.info().format, ImageFormat::Raw);
        let r = open_range(&parent, 10, 20).unwrap();
        assert_eq!(r.info().sector_count, 20);
        assert_eq!(r.info().range, Some((10, 20)));
        assert!(r.info().description.starts_with("Rango LBA 10–29"));
        assert!(read_sectors(r.as_ref(), 0, 1)
            .unwrap()
            .iter()
            .all(|&b| b == 10));
        assert!(read_sectors(r.as_ref(), 20, 1).is_err());
        let r2 = open_range(&r, 5, 10).unwrap();
        assert!(read_sectors(r2.as_ref(), 9, 1)
            .unwrap()
            .iter()
            .all(|&b| b == 24));
        assert!(open_range(&parent, 95, 6).is_err());
        assert!(open_range(&parent, 0, 0).is_err());
        assert!(open_range(&parent, u64::MAX, 2).is_err());
        drop(parent);
        assert!(read_sectors(r2.as_ref(), 0, 1).is_ok());
        assert_eq!(std::fs::read(&img).unwrap(), data);
    }

    #[test]
    fn partitions_are_listed_and_open_as_sources() {
        // MBR con una primaria [8, 24) y otra que termina fuera del disco [40, 80) de 64 sectores.
        let mut data: Vec<u8> = (0..64 * 512).map(|i| (i / 512) as u8).collect();
        data[..512].fill(0);
        for (i, (kind, start, count)) in [(0x07u8, 8u32, 16u32), (0x83, 40, 40)].iter().enumerate()
        {
            let e = &mut data[0x1BE + 16 * i..0x1BE + 16 * (i + 1)];
            e[4] = *kind;
            e[8..12].copy_from_slice(&start.to_le_bytes());
            e[12..16].copy_from_slice(&count.to_le_bytes());
        }
        data[510] = 0x55;
        data[511] = 0xAA;
        let src = source_from_device(Arc::new(MemDevice::new(data, 512).unwrap()), "disco");
        let t = partitions(src.as_ref()).unwrap();
        assert_eq!(t.scheme, mr_part::Scheme::Mbr);
        assert_eq!(t.partitions.len(), 2);
        let p0 = open_partition(&src, &t, 0, false).unwrap();
        assert_eq!(p0.info().sector_count, 16);
        assert_eq!(p0.info().range, Some((8, 16)));
        assert!(p0
            .info()
            .description
            .starts_with("Partición LBA 8–23 (tipo 0x07)"));
        assert!(read_sectors(p0.as_ref(), 0, 1)
            .unwrap()
            .iter()
            .all(|&b| b == 8));
        assert!(read_sectors(p0.as_ref(), 16, 1).is_err());
        let p1 = open_partition(&src, &t, 1, false).unwrap();
        assert_eq!(p1.info().sector_count, 24);
        assert!(p1
            .info()
            .description
            .contains("recortada: faltan 16 sectores"));
        let mut outside = t.clone();
        outside.partitions[1].start_lba = 64;
        assert!(matches!(
            open_partition(&src, &outside, 1, false),
            Err(ApiError::InvalidArgument(_))
        ));
        assert!(matches!(
            open_partition(&src, &t, 2, false),
            Err(ApiError::InvalidArgument(_))
        ));
        // Origen ilegible en la LBA 0: sin tabla, con la anomalía, sin error.
        let bad = FaultyDevice::new(MemDevice::new(vec![0u8; 8 * 512], 512).unwrap()).with_bad([0]);
        let t = partitions(&bad).unwrap();
        assert_eq!(t.scheme, mr_part::Scheme::None);
        assert_eq!(
            t.anomalies,
            vec![mr_part::Anomaly::UnreadableSector { lba: 0 }]
        );
    }

    /// Disco de 6160 sectores con un NTFS sintético en la LBA 2048 (copia en su último sector).
    fn lost_ntfs() -> Vec<u8> {
        let mut d = vec![0u8; 6160 * 512];
        let mut b = vec![0u8; 512];
        b[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
        b[3..11].copy_from_slice(b"NTFS    ");
        b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        b[0x0D] = 8;
        b[0x15] = 0xF8;
        b[0x1C..0x20].copy_from_slice(&2048u32.to_le_bytes());
        b[0x28..0x30].copy_from_slice(&2047u64.to_le_bytes());
        b[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        b[0x38..0x40].copy_from_slice(&128u64.to_le_bytes());
        b[0x40] = 0xF6;
        b[0x44] = 1;
        b[0x1FE] = 0x55;
        b[0x1FF] = 0xAA;
        d[2048 * 512..2049 * 512].copy_from_slice(&b);
        d[4095 * 512..4096 * 512].copy_from_slice(&b);
        d
    }

    #[test]
    fn partition_search_task_finds_and_opens_volumes() {
        let src = source_from_device(Arc::new(MemDevice::new(lost_ntfs(), 512).unwrap()), "disco");
        let task = start_partition_search_task(Arc::clone(&src), SearchOptions::default()).unwrap();
        let out = task.wait().unwrap();
        assert_eq!(task.state(), TaskState::Completed);
        let (done, total) = task.progress();
        assert_eq!((done, total), (out.result.stats.reads, 4096));
        assert_eq!(out.result.volumes.len(), 1);
        let v = open_found_volume(&src, &out.result, 0, false).unwrap();
        assert_eq!(v.info().range, Some((2048, 2048)));
        assert!(v
            .info()
            .description
            .starts_with("Volumen encontrado NTFS LBA 2048–4095 (fuera de la tabla)"));
        assert!(read_sectors(v.as_ref(), 0, 1)
            .unwrap()
            .starts_with(&[0xEB, 0x52, 0x90]));
        assert!(open_found_volume(&src, &out.result, 1, false).is_err());
        // Parcial y desalineado: nunca.
        let mut r = out.result.clone();
        r.volumes[0].start_byte = -512;
        assert!(open_found_volume(&src, &r, 0, true).is_err());
        r.volumes[0].start_byte = 2048 * 512 + 7;
        assert!(open_found_volume(&src, &r, 0, true).is_err());
        // Desplazado: solo pidiéndolo, y la descripción lo dice.
        let mut r = out.result.clone();
        r.shift_bytes = Some(16 * 512);
        r.volumes[0]
            .anomalies
            .push(mr_part::search::VolumeAnomaly::ShiftDetected {
                delta_bytes: 16 * 512,
            });
        let err = open_found_volume(&src, &r, 0, false)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("desplazada 16 sectores"), "{err}");
        let v = open_found_volume(&src, &r, 0, true).unwrap();
        assert!(v
            .info()
            .description
            .contains("desplazamiento detectado aplicado"));
        // Búsqueda hecha sobre otro origen.
        let other = source_from_device(Arc::new(MemDevice::new(vec![0; 4096], 512).unwrap()), "x");
        assert!(open_found_volume(&other, &out.result, 0, false).is_err());
        // Opciones no válidas: error síncrono.
        let bad = SearchOptions {
            max_reads: 0,
            ..SearchOptions::default()
        };
        assert!(matches!(
            start_partition_search_task(src, bad),
            Err(ApiError::InvalidArgument(_))
        ));
    }

    /// Cancelación: la tarea termina cancelada y entrega el resultado parcial sin leer más.
    #[test]
    fn partition_search_cancellation_returns_partial_result() {
        let dev = MemDevice::new(lost_ntfs(), 512).unwrap();
        let task = Task::spawn(move |c| {
            while !c.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            search_partitions(&dev, &SearchOptions::default(), c)
        })
        .unwrap();
        task.cancel();
        let out = task.wait().unwrap();
        assert_eq!(task.state(), TaskState::Cancelled);
        assert_eq!(out.result.status, mr_part::search::SearchStatus::Cancelled);
        assert_eq!(out.result.stats.reads, 0);
        assert!(out.result.volumes.is_empty());
    }

    #[test]
    fn mapped_read_never_presents_fill_as_data() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0x41u8; 512 * 8], 512).unwrap())
            .with_bad([2])
            .with_sparse(5, 2);
        let (data, states) = read_sectors_mapped(&dev, 0, 8, 1).unwrap();
        assert_eq!(
            states,
            vec![
                SectorState::Read,
                SectorState::Read,
                SectorState::Unreadable,
                SectorState::Read,
                SectorState::Read,
                SectorState::Sparse,
                SectorState::Sparse,
                SectorState::Read,
            ]
        );
        // El ilegible lleva el relleno (no los datos), los dispersos ceros y el resto los datos.
        assert!(data[1024..1536].chunks(4).all(|c| c == b"BAD!"));
        assert!(data[5 * 512..7 * 512].iter().all(|&b| b == 0));
        assert!(data[..1024].iter().all(|&b| b == 0x41));
        // El hash estricto se niega a calcularse sobre esa misma zona.
        let ctx = TaskContext::detached();
        assert!(hash_image(&dev, &HashOptions::default(), &ctx).is_err());
    }

    #[test]
    fn mapped_read_with_custom_fills_and_jump() {
        let dev = FaultyDevice::new(MemDevice::new(vec![0x41u8; 512 * 16], 512).unwrap())
            .with_bad([2])
            .with_sparse(5, 1);
        let opts = MappedReadOptions {
            retries: 0,
            jump_sectors: 4,
            bad_fill: [0xDE, 0xAD, 0xBE, 0xEF],
            skip_fill: *b"skp?",
            ..MappedReadOptions::default()
        };
        let (data, states) = read_sectors_mapped_with(&dev, 0, 8, &opts).unwrap();
        use SectorState::*;
        assert_eq!(
            states,
            vec![Read, Read, Unreadable, Skipped, Skipped, Sparse, Skipped, Read]
        );
        for (l, st) in states.iter().enumerate() {
            let sec = &data[l * 512..(l + 1) * 512];
            match st {
                Unreadable => assert!(sec.chunks(4).all(|c| c == [0xDE, 0xAD, 0xBE, 0xEF])),
                Skipped => assert!(sec.chunks(4).all(|c| c == b"skp?")),
                Sparse => assert!(sec.iter().all(|&b| b == 0)),
                _ => assert!(sec.iter().all(|&b| b == 0x41)),
            }
        }
        // Por defecto, igual que read_sectors_mapped.
        assert_eq!(
            read_sectors_mapped_with(&dev, 0, 8, &MappedReadOptions::default()).unwrap(),
            read_sectors_mapped(&dev, 0, 8, 1).unwrap()
        );
        // Patrones rechazados antes de leer: uniformes (ceros incluidos) o iguales entre sí.
        for (bad, skip) in [
            ([0u8; 4], *b"SKIP"),
            (*b"BAD!", [0xFF; 4]),
            (*b"BAD!", *b"BAD!"),
        ] {
            let o = MappedReadOptions {
                bad_fill: bad,
                skip_fill: skip,
                ..MappedReadOptions::default()
            };
            assert!(matches!(
                read_sectors_mapped_with(&dev, 0, 8, &o),
                Err(ApiError::InvalidArgument(_))
            ));
        }
        // El hash nunca incluye relleno, sea cual sea el patrón configurado: sigue siendo estricto.
        let ctx = TaskContext::detached();
        assert!(hash_image(&dev, &HashOptions::default(), &ctx).is_err());
        let clean = MemDevice::new(vec![0x41u8; 512 * 16], 512).unwrap();
        let h = hash_image(&clean, &HashOptions::default(), &ctx).unwrap();
        let (cd, _) = read_sectors_mapped_with(&clean, 0, 16, &opts).unwrap();
        let mut hasher = MultiHasher::new(&[HashAlgorithm::Sha256]);
        hasher.update(&cd);
        assert_eq!(hasher.finish(), h.digests);
    }

    #[test]
    fn ntfs_open_rejects_a_source_without_ntfs() {
        let src = source_from_device(
            Arc::new(MemDevice::new(vec![0u8; 64 * 512], 512).unwrap()),
            "ceros",
        );
        match ntfs_open(&src) {
            Err(ApiError::Ntfs(mr_fs_ntfs::NtfsError::NotNtfs { .. })) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn output_path_is_never_the_source_or_a_device() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        // El propio origen, también escrito de otra forma.
        let alt = dir.path().join(".").join("ORIGEN.IMG");
        for bad in [img.clone(), alt] {
            match check_output_path(&src, &bad) {
                Err(ApiError::InvalidArgument(m)) => assert!(m.contains("propio origen"), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        // Dispositivos y directorios.
        for dev in [r"\\.\PhysicalDrive0", r"\\.\C:", "NUL", r"\\?\GLOBALROOT\x"] {
            assert!(
                matches!(
                    check_output_path(&src, Path::new(dev)),
                    Err(ApiError::Io(_))
                ),
                "{dev}"
            );
        }
        assert!(check_output_path(&src, dir.path()).is_err());
        // Un fichero nuevo junto al origen sí (aquí no se escribe nada).
        check_output_path(&src, &dir.path().join("salida.bin")).unwrap();
        // Un rango del origen sigue siendo el mismo fichero.
        let part = open_range(&src, 1, 4).unwrap();
        assert!(check_output_path(&part, &img).is_err());
        assert!(!dir.path().join("salida.bin").exists());
    }

    /// Origen de prueba con el fichero principal, segmentos y ficheros externos indicados (como
    /// los construye `open_split_source`, un VMDK con extensiones o un disco diferencial).
    fn file_source(path: &Path, segments: &[PathBuf], external: &[PathBuf]) -> Source {
        Arc::new(OpenSource::new(
            Arc::new(MemDevice::new(vec![0u8; 4096], 512).unwrap()),
            Meta {
                path: path.to_path_buf(),
                format: ImageFormat::Raw,
                detected: ImageFormat::Raw,
                description: "prueba".into(),
                segments: segments.to_vec(),
                range: None,
                external_files: external.to_vec(),
            },
        ))
    }

    fn assert_is_source(src: &Source, out: &Path) {
        match check_output_path(src, out) {
            Err(ApiError::InvalidArgument(m)) => assert!(m.contains("propio origen"), "{m}"),
            other => panic!("{}: {other:?}", out.display()),
        }
    }

    fn assert_unverifiable(src: &Source, out: &Path) {
        match check_output_path(src, out) {
            Err(e @ ApiError::OutputNotVerifiable { .. }) => {
                assert!(e.to_string().contains("no se escribe"), "{e}")
            }
            other => panic!("{}: {other:?}", out.display()),
        }
    }

    /// Issue #76: si un fichero del origen no se puede canonicalizar, ninguna escritura se admite
    /// (antes se comparaba la ruta sin resolver y se podía escribir sobre el origen renombrado).
    #[test]
    fn output_check_fails_closed_when_a_source_file_cannot_be_resolved() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let out = dir.path().join("salida.bin");
        check_output_path(&src, &out).unwrap();
        // El origen se renombra con el fichero abierto: la salida con el nombre nuevo es el origen
        // y cualquier otra salida tampoco se puede comprobar.
        let moved = dir.path().join("movido.img");
        std::fs::rename(&img, &moved).unwrap();
        assert_unverifiable(&src, &moved);
        assert_unverifiable(&src, &out);
        assert!(create_output(&src, &moved, true).is_err());
        assert!(create_output(&src, &out, false).is_err());
        // Ni salida ni temporal.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read(&moved).unwrap(), vec![0u8; 4096]);
        // Un rango del origen hereda la ruta: también se rechaza.
        let part = open_range(&src, 1, 4).unwrap();
        assert_unverifiable(&part, &out);
        std::fs::rename(&moved, &img).unwrap();
        check_output_path(&part, &out).unwrap();

        // Segmentos y ficheros externos: basta con que falte uno.
        let files: Vec<PathBuf> = ["a.001", "a.002", "padre.vhd", "base.vhd"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        for f in &files {
            std::fs::write(f, b"x").unwrap();
        }
        let segs = files[..2].to_vec();
        let ext = files[2..].to_vec();
        let multi = file_source(&files[0], &segs, &ext);
        check_output_path(&multi, &out).unwrap();
        for f in &files[1..] {
            let hidden = dir.path().join("oculto.tmp");
            std::fs::rename(f, &hidden).unwrap();
            assert_unverifiable(&multi, &out);
            assert_unverifiable(&multi, &hidden);
            match create_output(&multi, &out, false) {
                Err(ApiError::OutputNotVerifiable { .. }) => {}
                other => panic!("{f:?}: {:?}", other.map(|_| ())),
            }
            std::fs::rename(&hidden, f).unwrap();
        }
        // Con todo en su sitio, se escribe con normalidad.
        let mut o = create_output(&multi, &out, false).unwrap();
        o.write_all(b"ok").unwrap();
        o.commit().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"ok");
        // Un origen sin fichero (en memoria) no tiene nada que proteger.
        let mem = source_from_device(Arc::new(MemDevice::new(vec![0u8; 512], 512).unwrap()), "m");
        check_output_path(&mem, &dir.path().join("otra.bin")).unwrap();
    }

    /// Issue #76: la salida no se admite tampoco a través de un directorio que no se puede
    /// resolver, ni como flujo alternativo del origen (Windows).
    #[test]
    fn output_check_requires_a_resolvable_directory() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        assert_unverifiable(&src, &dir.path().join("no-existe").join("salida.bin"));
        // Un fichero como directorio.
        assert!(check_output_path(&src, &img.join("salida.bin")).is_err());
        #[cfg(windows)]
        for ads in ["origen.img:flujo", "origen.img::$DATA", "otro.bin:flujo"] {
            match check_output_path(&src, &dir.path().join(ads)) {
                Err(ApiError::InvalidArgument(m)) => assert!(m.contains("flujo"), "{m}"),
                other => panic!("{ads}: {other:?}"),
            }
        }
    }

    /// Issue #76: el origen, cada segmento y cada fichero externo se reconocen escritos de otra
    /// forma: mayúsculas, `\\?\`, `.` y `..`, `/`, puntos y espacios finales, nombre 8.3 (si el
    /// volumen los genera) y unión de directorios.
    #[test]
    fn output_equal_to_a_source_file_by_another_spelling_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(base.join("sub")).unwrap();
        let names = [
            "origen con nombre largo.imagen",
            "segmento con nombre largo.002",
            "padre con nombre largo.vhdx",
        ];
        let files: Vec<PathBuf> = names.iter().map(|n| base.join(n)).collect();
        for f in &files {
            std::fs::write(f, b"origen").unwrap();
        }
        let src = file_source(
            &files[0],
            &[files[0].clone(), files[1].clone()],
            &[files[2].clone()],
        );
        #[cfg(windows)]
        let junction = {
            let j = base.join("union");
            let ok = std::process::Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(dir.path().join("union"))
                .arg(dir.path())
                .output()
                .is_ok_and(|o| o.status.success());
            ok.then_some(j)
        };
        for (f, name) in files.iter().zip(names) {
            let plain = dir.path().join(name);
            let mut spellings = vec![
                f.clone(),
                plain.clone(),
                dir.path().join(".").join(name),
                dir.path().join("sub").join("..").join(name),
            ];
            #[cfg(windows)]
            {
                let p = plain.to_string_lossy().into_owned();
                let verbatim = if p.starts_with(r"\\?\") {
                    p.clone()
                } else {
                    format!(r"\\?\{p}")
                };
                spellings.extend([
                    PathBuf::from(&verbatim),
                    PathBuf::from(p.to_uppercase()),
                    dir.path().join(name.to_uppercase()),
                    PathBuf::from(p.replace('\\', "/")),
                    PathBuf::from(format!("{p}.")),
                    PathBuf::from(format!("{p} ")),
                ]);
                match short_name(f) {
                    Some(s) => spellings.push(s),
                    None => eprintln!(
                        "nota: el volumen de pruebas no genera nombres 8.3; se omite ese caso"
                    ),
                }
                match &junction {
                    Some(j) => spellings.push(j.join(name)),
                    None => eprintln!("nota: no se pudo crear la unión; se omite ese caso"),
                }
            }
            for s in &spellings {
                assert_is_source(&src, s);
                assert!(create_output(&src, s, true).is_err(), "{}", s.display());
            }
        }
        for f in &files {
            assert_eq!(std::fs::read(f).unwrap(), b"origen");
        }
        // Un fichero nuevo junto a ellos sí.
        check_output_path(&src, &base.join("salida.bin")).unwrap();
        #[cfg(windows)]
        if let Some(j) = &junction {
            std::fs::remove_dir(j).unwrap();
        }
    }

    /// Nombre corto 8.3 de `p` según `cmd` (`%~sI`), o `None` si el volumen no lo genera.
    #[cfg(windows)]
    fn short_name(p: &Path) -> Option<PathBuf> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/d /c for %I in (\"{}\") do @echo %~sI",
                p.display()
            ))
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let short = PathBuf::from(&s);
        let file = short.file_name()?.to_string_lossy().into_owned();
        (out.status.success() && file.contains('~') && short.exists()).then_some(short)
    }

    /// Issue #76: enlaces duros al origen. En Unix se detectan por identidad (`dev`, `ino`). En
    /// Windows no (sin API estable y segura): se documenta, y se comprueba que aun así escribir
    /// por el enlace nunca cambia los datos del origen (temporal y renombrado).
    #[test]
    fn hard_link_to_the_source_never_changes_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0x41u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let link = dir.path().join("enlace.img");
        if let Err(e) = std::fs::hard_link(&img, &link) {
            eprintln!("nota: el volumen de pruebas no admite enlaces duros ({e}); se omite");
            return;
        }
        // Sin `overwrite`, una salida existente nunca se toca.
        assert!(create_output(&src, &link, false).is_err());
        #[cfg(unix)]
        {
            assert_is_source(&src, &link);
            assert!(create_output(&src, &link, true).is_err());
        }
        #[cfg(not(unix))]
        {
            use std::io::Write;
            // Limitación documentada: el enlace no se reconoce como el origen...
            check_output_path(&src, &link).unwrap();
            // ...pero la escritura sustituye el nombre, no los datos del origen.
            let mut o = create_output(&src, &link, true).unwrap();
            o.write_all(b"nuevo").unwrap();
            o.commit().unwrap();
            assert_eq!(std::fs::read(&link).unwrap(), b"nuevo");
        }
        assert_eq!(std::fs::read(&img).unwrap(), vec![0x41u8; 4096]);
        let mut buf = vec![0u8; 512];
        src.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, vec![0x41u8; 512]);
    }

    #[test]
    fn create_output_is_atomic_and_never_overwrites_silently() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let out = dir.path().join("salida.bin");
        let entries = || std::fs::read_dir(dir.path()).unwrap().count();
        // Nuevo: se escribe en un temporal y aparece con el nombre final solo al confirmar.
        let mut o = create_output(&src, &out, false).unwrap();
        o.write_all(b"primero").unwrap();
        assert!(o.temp_path().exists());
        assert!(!out.exists());
        let tmp = o.temp_path().to_path_buf();
        assert_eq!(o.commit().unwrap(), out);
        assert_eq!(std::fs::read(&out).unwrap(), b"primero");
        assert!(!tmp.exists());
        // Existe y no se pidió sobrescribir: error y el fichero no cambia.
        match create_output(&src, &out, false) {
            Err(ApiError::OutputExists(p)) => assert_eq!(p, out),
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert_eq!(std::fs::read(&out).unwrap(), b"primero");
        // Con petición explícita, se sustituye.
        let mut o = create_output(&src, &out, true).unwrap();
        o.write_all(b"segundo").unwrap();
        o.commit().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"segundo");
        // Fallo a mitad (sin confirmar): ni fichero final nuevo ni temporal.
        let other = dir.path().join("otra.bin");
        let mut o = create_output(&src, &other, false).unwrap();
        o.write_all(&[7u8; 100_000]).unwrap();
        let tmp = o.temp_path().to_path_buf();
        drop(o);
        assert!(!other.exists() && !tmp.exists());
        // Sustitución fallida: el anterior se conserva intacto.
        let mut o = create_output(&src, &out, true).unwrap();
        o.write_all(b"parcial").unwrap();
        drop(o);
        assert_eq!(std::fs::read(&out).unwrap(), b"segundo");
        assert_eq!(entries(), 2, "solo el origen y la salida");
        // El origen nunca, ni con sobrescritura.
        assert!(create_output(&src, &img, true).is_err());
        assert_eq!(std::fs::read(&img).unwrap(), vec![0u8; 4096]);
    }

    /// Issue #58: un destino creado entre `create_output` y `commit` nunca se sustituye sin
    /// `overwrite`, ni con el enlace duro ni con la copia exclusiva.
    #[test]
    fn create_output_commit_never_replaces_a_concurrent_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("origen.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let src = open_source(&img, 512).unwrap();
        let entries = || std::fs::read_dir(dir.path()).unwrap().count();
        for how in [Publish::Link, Publish::CopyOnly] {
            let out = dir.path().join(format!("salida-{how:?}.bin"));
            // Camino normal: el destino aparece con el contenido y el temporal desaparece.
            let mut o = create_output(&src, &out, false).unwrap();
            o.write_all(&[5u8; 70_000]).unwrap();
            let tmp = o.temp_path().to_path_buf();
            assert_eq!(o.commit_with(how).unwrap(), out);
            assert_eq!(std::fs::read(&out).unwrap(), vec![5u8; 70_000]);
            assert!(!tmp.exists());
            std::fs::remove_file(&out).unwrap();
            // Otro proceso crea el destino después de la comprobación inicial: se rechaza, el
            // destino ajeno no cambia y el temporal se borra.
            let mut o = create_output(&src, &out, false).unwrap();
            o.write_all(b"nuestro").unwrap();
            let tmp = o.temp_path().to_path_buf();
            std::fs::write(&out, b"ajeno").unwrap();
            match o.commit_with(how) {
                Err(ApiError::OutputExists(p)) => assert_eq!(p, out),
                other => panic!("{how:?}: {other:?}"),
            }
            assert_eq!(std::fs::read(&out).unwrap(), b"ajeno");
            assert!(!tmp.exists());
            // Con `overwrite`, el mismo caso sí sustituye (rename atómico).
            let mut o = create_output(&src, &out, true).unwrap();
            o.write_all(b"nuevo").unwrap();
            std::fs::write(&out, b"ajeno otra vez").unwrap();
            o.commit_with(how).unwrap();
            assert_eq!(std::fs::read(&out).unwrap(), b"nuevo");
            std::fs::remove_file(&out).unwrap();
            assert_eq!(entries(), 1, "{how:?}: solo el origen");
        }
        // Carrera real: otro hilo crea el destino en exclusiva mientras se confirma. Gana uno
        // solo y el destino tiene exactamente su contenido; el temporal nunca queda.
        for i in 0..64 {
            let how = if i % 2 == 0 {
                Publish::Link
            } else {
                Publish::CopyOnly
            };
            let out = dir.path().join(format!("carrera-{i}.bin"));
            let mut o = create_output(&src, &out, false).unwrap();
            o.write_all(&[1u8; 8192]).unwrap();
            let tmp = o.temp_path().to_path_buf();
            let rival = {
                let out = out.clone();
                std::thread::spawn(move || {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&out)
                    {
                        Ok(mut f) => {
                            f.write_all(&[2u8; 100]).unwrap();
                            true
                        }
                        Err(e) => {
                            assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
                            false
                        }
                    }
                })
            };
            let ours = match o.commit_with(how) {
                Ok(_) => true,
                Err(ApiError::OutputExists(_)) => false,
                Err(e) => panic!("{how:?}: {e:?}"),
            };
            let theirs = rival.join().unwrap();
            assert!(ours != theirs, "{how:?}: exactamente un ganador");
            let want = if ours {
                vec![1u8; 8192]
            } else {
                vec![2u8; 100]
            };
            assert_eq!(std::fs::read(&out).unwrap(), want, "{how:?}");
            assert!(!tmp.exists());
            std::fs::remove_file(&out).unwrap();
        }
        assert_eq!(entries(), 1, "solo el origen");
    }

    #[test]
    fn hard_link_unsupported_is_recognised_only_for_unsupported_errors() {
        use std::io::{Error, ErrorKind};
        assert!(hard_link_unsupported(&Error::from(ErrorKind::Unsupported)));
        assert!(!hard_link_unsupported(&Error::from(
            ErrorKind::AlreadyExists
        )));
        assert!(!hard_link_unsupported(&Error::from(ErrorKind::NotFound)));
        #[cfg(windows)]
        {
            // ERROR_INVALID_FUNCTION (FAT, exFAT) y ERROR_NOT_SUPPORTED; no ERROR_ACCESS_DENIED.
            assert!(hard_link_unsupported(&Error::from_raw_os_error(1)));
            assert!(hard_link_unsupported(&Error::from_raw_os_error(50)));
            assert!(!hard_link_unsupported(&Error::from_raw_os_error(5)));
        }
    }
}
