//! API de alto nivel del motor. La consumen la CLI (directamente) y la GUI (a través de `mr-ffi`).
//!
//! Operaciones disponibles (todas de solo lectura):
//! - [`probe_format`]: identifica el formato de un fichero.
//! - [`open_source`] / [`open_with`] / [`image_info`]: abre una imagen RAW o un contenedor
//!   soportado (VHD, VHDX, VMDK monolithicSparse) y devuelve sus datos.
//! - [`discover_split`] / [`open_split_source`]: imágenes divididas (apertura explícita).
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
//! - [`start_hash_task`]: hashes del fichero completo en segundo plano.
//! - [`start_verify_task`]: lectura completa tolerante a errores con mapa de validez por sector.

pub mod task;

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
pub use mr_image::OpenOptions;
pub use mr_part;
pub use mr_part::search::{SearchOptions, SearchResult};
pub use mr_part::{Partition, PartitionTable};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Io(#[from] mr_io::IoError),
    #[error(transparent)]
    Store(#[from] mr_store::StoreError),
    #[error(transparent)]
    Ntfs(#[from] mr_fs_ntfs::NtfsError),
    #[error("operación cancelada")]
    Cancelled,
    #[error("argumento no válido: {0}")]
    InvalidArgument(String),
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
}
