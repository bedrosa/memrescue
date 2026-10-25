//! Apertura unificada de orígenes de imagen: admite la ruta, identifica el formato y devuelve el
//! lector adecuado como `Arc<dyn BlockDevice>`.
//!
//! - RAW → [`mr_io::ImageFile`]; VHD → [`VhdImage`]; VHDX → [`VhdxImage`]; VMDK → [`VmdkImage`].
//! - Las imágenes divididas solo se abren con [`open_split`] (petición explícita: la interfaz
//!   muestra antes la lista de segmentos de [`crate::split::discover`]).
//! - Los demás contenedores detectados (EWF, QCOW, VDI, DMG) se rechazan con
//!   `IoError::UnsupportedFormat`, salvo que el usuario fuerce la lectura como RAW.
//! - Si se pide un tamaño de sector distinto del nativo del contenedor, se presenta a través de
//!   [`Reblocked`] (mismos bytes, otra división en sectores).
//! - Los contenedores se abren en modo estricto salvo que se pida [`OpenOptions::tolerant`]
//!   (ADR-020): entonces las entradas de asignación inválidas o ambiguas descartan solo su bloque o
//!   grano y la descripción lo indica con el número de bloques descartados.
//! - Los padres de un disco diferencial solo se usan si el llamador los pasa en [`ApprovedFiles`]
//!   (papeles `parent:1`, `parent:2`…) con [`open_image_with_parents`] (ADR-018, addendum);
//!   [`external_hints`] lista lo que el contenedor dice de ellos sin abrir nada más.

use crate::differencing::{self, Chain};
use crate::external::{ApprovedFiles, ExternalHint};
use crate::split::{discover, SplitImage};
use crate::vhd::VhdImage;
use crate::vhdx::VhdxImage;
use crate::vmdk::VmdkImage;
use crate::vmdk_extents::{ExtentPlan, VmdkExtentImage};
use mr_io::path_policy::{admit, open_regular_file};
use mr_io::{
    probe_file, validate_sector_size, Allocation, AllocationRun, BlockDevice, ImageFile,
    ImageFormat, IoError, Result,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tamaño de sector que se usa para RAW cuando no se indica ninguno.
pub const DEFAULT_RAW_SECTOR_SIZE: u32 = 512;

/// Opciones de apertura.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// Tamaño de sector lógico pedido; 0 = el nativo del formato (512 para RAW).
    pub sector_size: u32,
    /// Leer el fichero como volcado sector a sector aunque parezca un contenedor.
    pub force_raw: bool,
    /// Modo tolerante para contenedores VHD, VHDX y VMDK con estructuras de asignación
    /// parcialmente dañadas (ADR-020). Desactivado por defecto: sin él, una entrada inválida
    /// rechaza la imagen con `IoError::Corrupt`. No tiene efecto en RAW ni en imágenes divididas.
    pub tolerant: bool,
}

/// Origen abierto y sus metadatos.
pub struct OpenedImage {
    pub device: Arc<dyn BlockDevice>,
    /// Formato con el que se interpreta (RAW si se forzó).
    pub format: ImageFormat,
    /// Formato detectado en el fichero (puede diferir de `format` si se forzó RAW).
    pub detected: ImageFormat,
    /// Descripción legible de la variante.
    pub description: String,
    /// Ruta canónica del fichero (o del primer segmento).
    pub path: PathBuf,
    /// Segmentos de una imagen dividida (vacío en los demás casos).
    pub segments: Vec<PathBuf>,
    /// Tamaño de sector nativo del formato.
    pub native_sector_size: u32,
    /// Bloques (o granos) descartados si el contenedor se abrió en modo tolerante; `None` si se
    /// abrió en modo estricto o el origen no es un contenedor.
    pub discarded_blocks: Option<u64>,
    /// Padres de un disco diferencial usados (rutas canónicas, del inmediato al base). Vacío si no
    /// se usó ninguno.
    pub external_files: Vec<PathBuf>,
}

/// Abre un fichero de imagen identificando su formato. No usa ningún fichero externo: un disco
/// diferencial devuelve `IoError::ExternalReference`.
pub fn open_image(path: &Path, opts: OpenOptions) -> Result<OpenedImage> {
    open_image_with_parents(path, opts, &ApprovedFiles::empty(), false)
}

/// Error de una apertura que recibió padres que la imagen no usa.
fn unused_external(detected: ImageFormat, external: &ApprovedFiles) -> IoError {
    IoError::ExternalMismatch {
        format: detected.name(),
        detail: format!(
            "se indicaron {} ficheros padre y esta imagen no es un disco diferencial",
            external.len()
        ),
    }
}

/// Como [`open_image`], con la cadena de padres de un disco diferencial VHD/VHDX que el usuario
/// aprobó explícitamente: `parents` con los papeles `parent:1` (padre inmediato), `parent:2`… hasta
/// el disco base (ver `crate::differencing`). Con `ignore_link_mismatch` (petición explícita) se
/// acepta un padre cuyo identificador no coincide y la descripción lo advierte. Si la imagen no
/// es diferencial y `parents` no está vacío, la apertura falla con `IoError::ExternalMismatch`
/// (nunca se ignoran en silencio).
pub fn open_image_with_parents(
    path: &Path,
    opts: OpenOptions,
    external: &ApprovedFiles,
    ignore_link_mismatch: bool,
) -> Result<OpenedImage> {
    if opts.sector_size != 0 {
        validate_sector_size(opts.sector_size)?;
    }
    let canon = admit(path)?;
    let file = open_regular_file(&canon)?;
    let size = file.metadata()?.len();
    let detected = probe_file(&file, size)?;
    let uses_external = matches!(detected, ImageFormat::Vhd | ImageFormat::Vhdx) && !opts.force_raw;
    if !external.is_empty() && !uses_external {
        return Err(unused_external(detected, external));
    }
    if opts.force_raw || detected == ImageFormat::Raw {
        drop(file);
        let ss = non_zero(opts.sector_size, DEFAULT_RAW_SECTOR_SIZE);
        let img = ImageFile::open_as_raw(&canon, ss)?;
        let description = if detected == ImageFormat::Raw {
            "RAW (sector a sector)".to_string()
        } else {
            format!(
                "RAW forzado por el usuario (el fichero parece {})",
                detected.name()
            )
        };
        return Ok(OpenedImage {
            device: Arc::new(img),
            format: ImageFormat::Raw,
            detected,
            description,
            path: canon,
            segments: Vec::new(),
            native_sector_size: ss,
            discarded_blocks: None,
            external_files: Vec::new(),
        });
    }
    let tolerant = opts.tolerant;
    let mut external_files = Vec::new();
    let (device, description, discarded): (Arc<dyn BlockDevice>, String, u64) = match detected {
        ImageFormat::Vhd if !external.is_empty() => {
            let c: Chain<VhdImage> =
                differencing::open_chain(file, &canon, tolerant, external, ignore_link_mismatch)?;
            external_files = c.parents;
            (Arc::new(c.top), c.description, c.discarded)
        }
        ImageFormat::Vhdx if !external.is_empty() => {
            let c: Chain<VhdxImage> =
                differencing::open_chain(file, &canon, tolerant, external, ignore_link_mismatch)?;
            external_files = c.parents;
            (Arc::new(c.top), c.description, c.discarded)
        }
        ImageFormat::Vhd => {
            let d = VhdImage::open_with(file, tolerant)?;
            let (s, n) = (d.describe(), d.discarded_blocks());
            (Arc::new(d), s, n)
        }
        ImageFormat::Vhdx => {
            let d = VhdxImage::open_with(file, tolerant)?;
            let (s, n) = (d.describe(), d.discarded_blocks());
            (Arc::new(d), s, n)
        }
        // El lector VMDK también interpreta (sin seguirlos) los descriptores de texto separados.
        ImageFormat::Vmdk | ImageFormat::VmdkDescriptor => {
            let d = VmdkImage::open_with(file, tolerant)?;
            let (s, n) = (d.describe(), d.discarded_blocks());
            (Arc::new(d), s, n)
        }
        other => return Err(IoError::UnsupportedFormat(other)),
    };
    let native = device.sector_size();
    let (device, description) = reblock(device, description, opts.sector_size)?;
    Ok(OpenedImage {
        device,
        format: detected,
        detected,
        description,
        path: canon,
        segments: Vec::new(),
        native_sector_size: native,
        discarded_blocks: tolerant.then_some(discarded),
        external_files,
    })
}

/// Indicaciones que da el contenedor sobre los ficheros externos que necesita (padre de un disco
/// diferencial VHD o VHDX), **sin abrir ningún otro fichero**. Vacío si no necesita ninguno. Los
/// valores son texto saneado y acotado: solo para mostrarlos al usuario, que elige y confirma los
/// ficheros. Las entradas de asignación dañadas no impiden leerlas (se interpreta en modo
/// tolerante); las estructuras críticas dañadas sí (`Corrupt`).
pub fn external_hints(path: &Path) -> Result<Vec<ExternalHint>> {
    let canon = admit(path)?;
    let file = open_regular_file(&canon)?;
    let size = file.metadata()?.len();
    match probe_file(&file, size)? {
        ImageFormat::Vhd => differencing::hints_of::<VhdImage>(file),
        ImageFormat::Vhdx => differencing::hints_of::<VhdxImage>(file),
        _ => Ok(Vec::new()),
    }
}

/// Abre una imagen dividida (`x.001`, `x.002`…) a partir de su primer segmento. El contenido se
/// trata como RAW concatenado; si el primer segmento parece un contenedor se rechaza salvo que se
/// fuerce RAW.
pub fn open_split(first: &Path, opts: OpenOptions) -> Result<OpenedImage> {
    let ss = non_zero(opts.sector_size, DEFAULT_RAW_SECTOR_SIZE);
    validate_sector_size(ss)?;
    let paths = discover(first)?;
    let img = SplitImage::open(&paths, ss)?;
    let detected = {
        let canon = admit(&paths[0])?;
        let f = open_regular_file(&canon)?;
        let len = f.metadata()?.len();
        probe_file(&f, len)?
    };
    if detected != ImageFormat::Raw && !opts.force_raw {
        return Err(IoError::UnsupportedFormat(detected));
    }
    let description = img.describe();
    let segments = img.segments().to_vec();
    Ok(OpenedImage {
        path: segments[0].clone(),
        device: Arc::new(img),
        format: ImageFormat::Raw,
        detected,
        description,
        segments,
        native_sector_size: ss,
        discarded_blocks: None,
        external_files: Vec::new(),
    })
}

/// Lista las extensiones que necesita un descriptor VMDK separado (nombres, tipos, tamaños y
/// problemas) **sin abrir** ninguna (incidencia 11).
pub fn list_vmdk_extents(descriptor: &Path) -> Result<ExtentPlan> {
    let canon = admit(descriptor)?;
    let file = open_regular_file(&canon)?;
    crate::vmdk_extents::read_plan(&file)
}

/// Abre un VMDK descrito por un descriptor separado usando **solo** los ficheros de extensión
/// que el usuario aprobó explícitamente (`approved`: nombre declarado → fichero elegido; ver
/// [`crate::vmdk_extents`]). `force_raw` no se admite; `tolerant` se aplica a las extensiones
/// SPARSE. `segments` del resultado lista los ficheros de extensión abiertos (para que nunca se
/// usen como destino de una extracción).
pub fn open_vmdk_extents(
    descriptor: &Path,
    approved: &ApprovedFiles,
    opts: OpenOptions,
) -> Result<OpenedImage> {
    if opts.force_raw {
        return Err(IoError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "un descriptor VMDK con extensiones no se puede forzar como RAW",
        )));
    }
    if opts.sector_size != 0 {
        validate_sector_size(opts.sector_size)?;
    }
    let canon = admit(descriptor)?;
    let file = open_regular_file(&canon)?;
    let img = VmdkExtentImage::open(&file, &canon, approved, opts.tolerant)?;
    let (description, discarded) = (img.describe(), img.discarded_blocks());
    let segments = img.extent_paths().to_vec();
    let device: Arc<dyn BlockDevice> = Arc::new(img);
    let native = device.sector_size();
    let (device, description) = reblock(device, description, opts.sector_size)?;
    Ok(OpenedImage {
        device,
        format: ImageFormat::VmdkDescriptor,
        detected: ImageFormat::VmdkDescriptor,
        description,
        path: canon,
        segments,
        native_sector_size: native,
        discarded_blocks: opts.tolerant.then_some(discarded),
        // Las extensiones van en `segments` (también protegidas como origen).
        external_files: Vec::new(),
    })
}

fn non_zero(v: u32, default: u32) -> u32 {
    if v == 0 {
        default
    } else {
        v
    }
}

fn reblock(
    device: Arc<dyn BlockDevice>,
    description: String,
    requested: u32,
) -> Result<(Arc<dyn BlockDevice>, String)> {
    let native = device.sector_size();
    if requested == 0 || requested == native {
        return Ok((device, description));
    }
    let view = Reblocked::new(device, requested)?;
    let d = format!("{description}; visto con sectores de {requested} B (nativo {native} B)");
    Ok((Arc::new(view), d))
}

/// Presenta los mismos bytes de otro dispositivo con otro tamaño de sector lógico (p. ej. un
/// disco 4Kn guardado en un VHD, cuyo sector nativo es de 512 B).
pub struct Reblocked {
    inner: Arc<dyn BlockDevice>,
    sector_size: u32,
}

impl Reblocked {
    pub fn new(inner: Arc<dyn BlockDevice>, sector_size: u32) -> Result<Self> {
        validate_sector_size(sector_size)?;
        Ok(Self { inner, sector_size })
    }

    pub fn inner(&self) -> &Arc<dyn BlockDevice> {
        &self.inner
    }
}

impl BlockDevice for Reblocked {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }

    /// Un sector propio es `Sparse` solo si **todos** sus bytes son dispersos en el interior; si
    /// toca cualquier byte con datos (o bytes finales que el interior no puede describir), `Data`.
    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        let end = lba.checked_add(count);
        if count == 0 || end.is_none_or(|e| e > total) {
            return Err(IoError::InvalidRange { lba, count, total });
        }
        let end = lba + count;
        let ss = u128::from(self.sector_size);
        let iss = u128::from(self.inner.sector_size());
        // Rango de bytes pedido (cabe en u64: end ≤ total ≤ size / ss).
        let b0 = u128::from(lba) * ss;
        let b1 = u128::from(end) * ss;
        let inner_total = u128::from(self.inner.sector_count());
        let i0 = b0 / iss;
        let i1 = b1.div_ceil(iss).min(inner_total);

        // Intervalos de datos en sectores propios, ordenados y fusionados.
        let mut data: Vec<(u64, u64)> = Vec::new();
        let mut push_data = |a: u128, b: u128| {
            // Bytes [a, b) con datos → sectores propios [a/ss, ceil(b/ss)) ∩ [lba, end).
            let s0 = (a / ss).max(u128::from(lba)) as u64;
            let s1 = b.div_ceil(ss).min(u128::from(end)) as u64;
            if s0 >= s1 {
                return;
            }
            match data.last_mut() {
                Some(last) if s0 <= last.1 => last.1 = last.1.max(s1),
                _ => data.push((s0, s1)),
            }
        };
        if i0 < i1 {
            let runs = self.inner.allocation(i0 as u64, (i1 - i0) as u64)?;
            for (ilba, n, a) in runs {
                if a == Allocation::Data {
                    let a0 = u128::from(ilba) * iss;
                    let a1 = (u128::from(ilba) + u128::from(n)) * iss;
                    push_data(a0, a1);
                }
            }
        }
        // Bytes finales que el interior no describe en sectores completos: se tratan como datos.
        let described = inner_total * iss;
        if b1 > described {
            push_data(described.max(b0), b1);
        }

        let mut out: Vec<AllocationRun> = Vec::new();
        let mut pos = lba;
        for (s0, s1) in data {
            if s0 > pos {
                out.push((pos, s0 - pos, Allocation::Sparse));
            }
            out.push((s0, s1 - s0, Allocation::Data));
            pos = s1;
        }
        if pos < end {
            out.push((pos, end - pos, Allocation::Sparse));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mr_io::{FaultyDevice, MemDevice};

    fn mem(sectors: usize, ss: u32) -> MemDevice {
        MemDevice::new(
            (0..sectors * ss as usize)
                .map(|i| (i % 251) as u8)
                .collect(),
            ss,
        )
        .unwrap()
    }

    #[test]
    fn reblocked_reads_same_bytes() {
        let inner: Arc<dyn BlockDevice> = Arc::new(mem(64, 512));
        let v = Reblocked::new(Arc::clone(&inner), 4096).unwrap();
        assert_eq!(v.size(), inner.size());
        assert_eq!(v.sector_count(), 8);
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        v.read_sectors(3, 1, &mut a).unwrap();
        inner.read_at(3 * 4096, &mut b).unwrap();
        assert_eq!(a, b);
        assert!(v.read_sectors(8, 1, &mut a).is_err());
        assert!(Reblocked::new(inner, 1000).is_err());
    }

    #[test]
    fn reblocked_allocation_larger_sectors() {
        // Interior de 512 B: dispersos 8..16 (sector propio 1 entero) y 17..24 (parte del 2).
        let inner: Arc<dyn BlockDevice> = Arc::new(
            FaultyDevice::new(mem(64, 512))
                .with_sparse(8, 8)
                .with_sparse(17, 7)
                .with_sparse(40, 24),
        );
        let v = Reblocked::new(inner, 4096).unwrap();
        assert_eq!(
            v.allocation(0, 8).unwrap(),
            vec![
                (0, 1, Allocation::Data),
                (1, 1, Allocation::Sparse),
                (2, 3, Allocation::Data),
                (5, 3, Allocation::Sparse),
            ]
        );
        assert_eq!(
            v.allocation(1, 1).unwrap(),
            vec![(1, 1, Allocation::Sparse)]
        );
        assert!(v.allocation(7, 2).is_err());
        assert!(v.allocation(0, 0).is_err());
        assert!(v.allocation(u64::MAX, 1).is_err());
    }

    #[test]
    fn reblocked_allocation_smaller_sectors() {
        // Interior de 4096 B con el sector 1 disperso → sectores propios de 512 B 8..16.
        let inner: Arc<dyn BlockDevice> =
            Arc::new(FaultyDevice::new(mem(4, 4096)).with_sparse(1, 1));
        let v = Reblocked::new(inner, 512).unwrap();
        assert_eq!(
            v.allocation(4, 16).unwrap(),
            vec![
                (4, 4, Allocation::Data),
                (8, 8, Allocation::Sparse),
                (16, 4, Allocation::Data),
            ]
        );
    }

    #[test]
    fn open_image_rejects_devices_and_unknown_containers() {
        assert!(matches!(
            open_image(Path::new(r"\\.\PhysicalDrive0"), OpenOptions::default()),
            Err(IoError::DevicePathRejected(_))
        ));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.qcow2");
        let mut data = vec![0u8; 4096];
        data[..4].copy_from_slice(b"QFI\xfb");
        std::fs::write(&p, &data).unwrap();
        assert!(matches!(
            open_image(&p, OpenOptions::default()),
            Err(IoError::UnsupportedFormat(ImageFormat::Qcow))
        ));
        let forced = open_image(
            &p,
            OpenOptions {
                force_raw: true,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(forced.format, ImageFormat::Raw);
        assert_eq!(forced.detected, ImageFormat::Qcow);
        assert!(forced.description.contains("forzado"));
        assert_eq!(std::fs::read(&p).unwrap(), data);
    }

    #[test]
    fn open_image_raw_uses_requested_sector_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.img");
        std::fs::write(&p, vec![0u8; 8192]).unwrap();
        let o = open_image(&p, OpenOptions::default()).unwrap();
        assert_eq!((o.format, o.device.sector_size()), (ImageFormat::Raw, 512));
        let o = open_image(
            &p,
            OpenOptions {
                sector_size: 4096,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(o.device.sector_count(), 2);
        assert!(open_image(
            &p,
            OpenOptions {
                sector_size: 3000,
                ..OpenOptions::default()
            }
        )
        .is_err());
    }

    #[test]
    fn open_split_concatenates_and_requires_explicit_call() {
        let dir = tempfile::tempdir().unwrap();
        let a: Vec<u8> = (0..3000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(dir.path().join("s.001"), &a[..1024]).unwrap();
        std::fs::write(dir.path().join("s.002"), &a[1024..2048]).unwrap();
        std::fs::write(dir.path().join("s.003"), &a[2048..]).unwrap();
        // Abierto como fichero suelto, solo es el primer segmento.
        let one = open_image(&dir.path().join("s.001"), OpenOptions::default()).unwrap();
        assert_eq!(one.device.size(), 1024);
        let o = open_split(&dir.path().join("s.001"), OpenOptions::default()).unwrap();
        assert_eq!(o.segments.len(), 3);
        assert_eq!(o.device.size(), 3000);
        let mut b = vec![0u8; 3000];
        o.device.read_at(0, &mut b).unwrap();
        assert_eq!(b, a);
    }
}
