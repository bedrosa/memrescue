//! Contenido de los nodos del árbol virtual: la recuperación lee los flujos a través del rasgo
//! [`ContentSource`], de modo que el motor no conoce NTFS. [`NtfsContent`] lo implementa sobre
//! `mr-fs-ntfs` (vivos con su flujo tal cual; borrados con la evaluación de ADR-027, los
//! clústeres sospechosos como `possibly_overwritten`) y [`crate::mem::MemContent`] en memoria
//! (pruebas y árboles sintéticos).
//!
//! Los bytes se entregan en [`StreamChunk`] con el estado de cada tramo ([`RangeState`]): los no
//! fiables con relleno ya llevan el patrón; los `possibly_overwritten` llevan los bytes del disco y
//! el motor decide si los escribe ([`crate::RecoverOptions::include_reallocated_bytes`]).

use std::sync::{Mutex, OnceLock};

use mr_fs_ntfs::deleted::{scan_deleted, DeletedScan, ScanObserver, ScanOptions};
use mr_fs_ntfs::file::{SegmentKind, StreamError};
use mr_fs_ntfs::{DataStream, NtfsError, NtfsVolume, StreamChunk};
use mr_vfs::{flags, NodeInfo};

pub use mr_fs_ntfs::stream::{RangeState, StreamRange, COPY_CHUNK};

/// Flujo de un nodo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDesc {
    /// `""` = el principal.
    pub name: String,
    pub size: u64,
}

/// Error del origen al leer un nodo (siempre de la clase «origen», nunca del destino).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// No se puede recuperar en este hito (flujo cifrado EFS, FS-05).
    Unsupported { code: &'static str, message: String },
    /// No hay datos fiables que leer: registro dañado, reutilizado o flujo con error tipado.
    Unavailable { code: &'static str, message: String },
    /// Error de E/S del origen que impide seguir con este nodo.
    Io(String),
    /// Cancelado durante una preparación larga (recorrido de borrados).
    Cancelled,
}

impl SourceError {
    pub fn code(&self) -> &'static str {
        match self {
            SourceError::Unsupported { code, .. } | SourceError::Unavailable { code, .. } => code,
            SourceError::Io(_) => "source_io",
            SourceError::Cancelled => "cancelled",
        }
    }

    pub fn message(&self) -> String {
        match self {
            SourceError::Unsupported { message, .. } | SourceError::Unavailable { message, .. } => {
                message.clone()
            }
            SourceError::Io(m) => m.clone(),
            SourceError::Cancelled => "operación cancelada".into(),
        }
    }
}

/// Cancelación que consultan las preparaciones largas del origen.
pub trait CancelFlag {
    fn cancelled(&self) -> bool;
}

/// Lector de un flujo.
pub trait StreamReader {
    fn size(&self) -> u64;
    /// Hasta `len` bytes (≤ [`COPY_CHUNK`] en el motor) desde `offset` con el estado de cada
    /// tramo (contiguos, cubren exactamente los datos).
    fn read(&self, offset: u64, len: usize) -> Result<StreamChunk, SourceError>;
    /// LBA del origen del byte `offset` del flujo y bytes contiguos en disco desde ahí (`None` si
    /// no está en disco: residente, disperso, sin mapa o comprimido).
    fn locate(&self, _offset: u64) -> Option<(u64, u64)> {
        None
    }
    /// Tamaño de sector de las LBA de [`StreamReader::locate`] y de los tramos ilegibles.
    fn sector_size(&self) -> u32 {
        512
    }
}

/// Origen de los contenidos de un árbol virtual.
pub trait ContentSource: Sync {
    /// Flujos `$DATA` del nodo, el principal (`""`) primero si existe. Un directorio puede tener
    /// flujos alternativos y no tener principal.
    fn streams(&self, node: &NodeInfo) -> Result<Vec<StreamDesc>, SourceError>;
    /// Abre el flujo `stream` del nodo para leerlo.
    fn open<'a>(
        &'a self,
        node: &NodeInfo,
        stream: &str,
        cancel: &dyn CancelFlag,
    ) -> Result<Box<dyn StreamReader + 'a>, SourceError>;
    /// Descripción del sistema de archivos para el diario (`"ntfs"`...).
    fn fs_name(&self) -> &'static str;
    /// Tamaño de clúster (0 si no aplica).
    fn cluster_size(&self) -> u32 {
        0
    }
}

/// Contenido de un volumen NTFS (ver el módulo).
pub struct NtfsContent<'v> {
    vol: &'v NtfsVolume,
    fill: [u8; 4],
    /// Recorrido de borrados de todo el volumen (una vez, al abrir el primer borrado).
    scan: OnceLock<DeletedScan>,
    scan_lock: Mutex<()>,
}

impl<'v> NtfsContent<'v> {
    pub fn new(vol: &'v NtfsVolume, fill: [u8; 4]) -> Self {
        NtfsContent {
            vol,
            fill,
            scan: OnceLock::new(),
            scan_lock: Mutex::new(()),
        }
    }

    fn scan(&self, cancel: &dyn CancelFlag) -> Result<&DeletedScan, SourceError> {
        if let Some(s) = self.scan.get() {
            return Ok(s);
        }
        let _g = self.scan_lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = self.scan.get() {
            return Ok(s);
        }
        struct Obs<'c>(&'c dyn CancelFlag);
        impl ScanObserver for Obs<'_> {
            fn cancelled(&self) -> bool {
                self.0.cancelled()
            }
        }
        let opts = ScanOptions {
            residual: false,
            ..ScanOptions::default()
        };
        match scan_deleted(self.vol, &opts, &Obs(cancel)) {
            Ok(Some(s)) => Ok(self.scan.get_or_init(|| s)),
            Ok(None) => Err(SourceError::Cancelled),
            Err(e) => Err(ntfs_error(e)),
        }
    }

    /// Fichero del nodo comprobando que el registro sigue siendo ese fichero: vivo en uso con la
    /// misma secuencia; borrado no en uso con la misma secuencia y con nombre.
    fn file_of(&self, node: &NodeInfo) -> Result<mr_fs_ntfs::NtfsFile, SourceError> {
        let Some(o) = node.origin else {
            return Err(SourceError::Unavailable {
                code: "no_origin",
                message: "el nodo no tiene referencia al origen".into(),
            });
        };
        if node.has(flags::RECORD_DAMAGED) {
            return Err(SourceError::Unavailable {
                code: "record_damaged",
                message: format!("el registro {} está dañado o es ilegible", o.record),
            });
        }
        let f = self.vol.file(o.record).map_err(ntfs_error)?;
        let deleted = node.has(flags::DELETED);
        if f.header.sequence != o.sequence {
            return Err(SourceError::Unavailable {
                code: "record_reused",
                message: format!(
                    "el registro {} tiene la secuencia {} y la entrada la {}: ya no es ese fichero",
                    o.record, f.header.sequence, o.sequence
                ),
            });
        }
        if deleted == f.in_use() {
            return Err(SourceError::Unavailable {
                code: "record_state_changed",
                message: format!(
                    "el registro {} está {} y la entrada dice lo contrario",
                    o.record,
                    if f.in_use() { "en uso" } else { "libre" }
                ),
            });
        }
        if deleted && f.file_names().is_empty() {
            return Err(SourceError::Unavailable {
                code: "record_free",
                message: format!("el registro {} está libre (sin $FILE_NAME)", o.record),
            });
        }
        Ok(f)
    }
}

fn ntfs_error(e: NtfsError) -> SourceError {
    match e {
        NtfsError::Io(io) => SourceError::Io(io.to_string()),
        NtfsError::Stream {
            error: StreamError::Encrypted,
            ..
        } => SourceError::Unsupported {
            code: "encrypted",
            message: "flujo cifrado (EFS): la exportación en bruto no está disponible en este hito"
                .into(),
        },
        NtfsError::Stream { error, .. } => SourceError::Unavailable {
            code: error.code(),
            message: error.to_string(),
        },
        other => SourceError::Unavailable {
            code: other.code(),
            message: other.to_string(),
        },
    }
}

impl ContentSource for NtfsContent<'_> {
    fn streams(&self, node: &NodeInfo) -> Result<Vec<StreamDesc>, SourceError> {
        let f = self.file_of(node)?;
        let g = self.vol.geometry();
        let mut out = Vec::new();
        for name in f.data_streams() {
            match f.stream_layout(mr_fs_ntfs::attr::types::DATA, &name, g) {
                Ok(l) => out.push(StreamDesc {
                    name,
                    size: l.data_size,
                }),
                // El error se informa al abrirlo (cifrado → no soportado; otros → no disponible).
                Err(_) => out.push(StreamDesc { name, size: 0 }),
            }
        }
        Ok(out)
    }

    fn open<'a>(
        &'a self,
        node: &NodeInfo,
        stream: &str,
        cancel: &dyn CancelFlag,
    ) -> Result<Box<dyn StreamReader + 'a>, SourceError> {
        let f = self.file_of(node)?;
        let s = if f.in_use() {
            self.vol.stream_of(&f, stream).map_err(ntfs_error)?
        } else {
            let record = f.number;
            drop(f);
            let scan = self.scan(cancel)?;
            match mr_fs_ntfs::recover::recovery_stream_from_scan(self.vol, scan, record, stream)
                .map_err(ntfs_error)?
            {
                Some((s, _)) => s,
                None => {
                    struct Obs<'c>(&'c dyn CancelFlag);
                    impl ScanObserver for Obs<'_> {
                        fn cancelled(&self) -> bool {
                            self.0.cancelled()
                        }
                    }
                    match mr_fs_ntfs::recover::recovery_stream(
                        self.vol,
                        record,
                        stream,
                        &Obs(cancel),
                    )
                    .map_err(ntfs_error)?
                    {
                        Some((s, _)) => s,
                        None => return Err(SourceError::Cancelled),
                    }
                }
            }
        };
        Ok(Box::new(NtfsReader {
            s: s.with_fill(self.fill),
            sector: self.vol.device().sector_size(),
        }))
    }

    fn fs_name(&self) -> &'static str {
        "ntfs"
    }

    fn cluster_size(&self) -> u32 {
        self.vol.geometry().cluster_size
    }
}

struct NtfsReader<'v> {
    s: DataStream<'v>,
    sector: u32,
}

impl StreamReader for NtfsReader<'_> {
    fn size(&self) -> u64 {
        self.s.size()
    }

    fn read(&self, offset: u64, len: usize) -> Result<StreamChunk, SourceError> {
        self.s.read(offset, len).map_err(ntfs_error)
    }

    fn locate(&self, offset: u64) -> Option<(u64, u64)> {
        let l = self.s.layout();
        if l.is_resident() || l.is_compressed() || offset >= l.initialized_size {
            return None;
        }
        let cluster = u64::from(l.cluster_size);
        let vcn = offset / cluster;
        let segs = &l.segments;
        let i = segs.partition_point(|x| x.end_vcn() <= vcn);
        let seg = segs.get(i).filter(|s| s.vcn <= vcn)?;
        let SegmentKind::Disk(lcn) = seg.kind else {
            return None;
        };
        let byte = (lcn + (vcn - seg.vcn)) * cluster + offset % cluster;
        let end = seg
            .end_vcn()
            .saturating_mul(cluster)
            .min(l.initialized_size);
        let sector = u64::from(self.sector.max(1));
        Some((byte / sector, end.saturating_sub(offset)))
    }

    fn sector_size(&self) -> u32 {
        self.sector
    }
}
