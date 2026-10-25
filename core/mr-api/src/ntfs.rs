//! Árbol de ficheros NTFS (#22): listado de directorios, rutas, árbol completo como tarea, un
//! flujo como origen (para el visor) y copia de un fichero a una salida con la política común.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_fs_ntfs::dir::{DirEntry, DirListing, Tree, TreeObserver, TreeOptions, TreeStatus};
use mr_fs_ntfs::file::StreamLayout;
use mr_fs_ntfs::stream::{CopyError, MAX_STREAM_READ};
use mr_fs_ntfs::{CopyReport, NtfsVolume};
use mr_io::{Allocation, AllocationRun, BlockDevice, IoError};

use crate::task::{Task, TaskContext};
use crate::{create_output, ApiError, Meta, OpenSource, Result, Source};

/// Lista el directorio del registro `record` (la raíz es el 5): entradas de su índice `$I30`
/// contrastadas con la MFT (vivas y residuales, cada una con su estado) y anomalías del índice.
pub fn ntfs_list_dir(vol: &NtfsVolume, record: u64) -> Result<DirListing> {
    Ok(vol.list_dir(record)?)
}

/// Resuelve una ruta absoluta (`/a/b/c`, sin distinguir mayúsculas según `$UpCase`).
pub fn ntfs_resolve(vol: &NtfsVolume, path: &str) -> Result<DirEntry> {
    Ok(vol.resolve_path(path)?)
}

/// Ruta de un registro subiendo por las referencias al padre (error si está huérfano).
pub fn ntfs_path_of(vol: &NtfsVolume, record: u64) -> Result<String> {
    Ok(vol.path_of(record)?)
}

impl TreeObserver for TaskContext {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn progress(&self, done: u64, total: u64) {
        self.set_total(total);
        self.set_done(done);
    }
}

/// Árbol completo del volumen. El progreso es la suma de entradas listadas y de registros de la
/// MFT revisados para buscar huérfanos. Al cancelar devuelve el árbol parcial
/// (`TreeStatus::Cancelled`) y la tarea termina cancelada con ese resultado.
pub fn ntfs_tree(vol: &NtfsVolume, opts: &TreeOptions, ctx: &TaskContext) -> Result<Tree> {
    let t = vol.tree(opts, ctx)?;
    if t.status == TreeStatus::Cancelled {
        ctx.finish_cancelled_with_result();
    }
    Ok(t)
}

/// Lanza [`ntfs_tree`] en segundo plano (cancelable, con progreso).
pub fn start_ntfs_tree_task(vol: Arc<NtfsVolume>, opts: TreeOptions) -> Result<Arc<Task<Tree>>> {
    Task::spawn(move |ctx| ntfs_tree(&vol, &opts, ctx))
}

/// Sector del origen que presenta un flujo.
pub const STREAM_SOURCE_SECTOR: u32 = 512;

/// Flujo de un fichero NTFS presentado como dispositivo de sectores de 512 B (para el visor).
/// El tamaño se redondea a sectores completos: los bytes tras el final del flujo son ceros y los
/// sectores enteros más allá del final se declaran dispersos. Un tramo no fiable del flujo
/// (sector ilegible, sin mapa, fuera del origen, unidad comprimida dañada o posiblemente
/// sobrescrito, #23) es un error de lectura: la lectura tolerante lo entrega con el patrón de
/// relleno y el estado ilegible, nunca como datos.
struct NtfsStreamDevice {
    vol: Arc<NtfsVolume>,
    layout: StreamLayout,
    record: u64,
    in_use: bool,
}

impl BlockDevice for NtfsStreamDevice {
    fn size(&self) -> u64 {
        self.layout
            .data_size
            .next_multiple_of(u64::from(STREAM_SOURCE_SECTOR))
    }

    fn sector_size(&self) -> u32 {
        STREAM_SOURCE_SECTOR
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> mr_io::Result<()> {
        let size = self.size();
        let len = buf.len();
        let end = offset
            .checked_add(len as u64)
            .filter(|&e| e <= size)
            .ok_or(IoError::OutOfBounds { offset, len, size })?;
        let data_end = end.min(self.layout.data_size);
        let stream = self.vol.stream_view(&self.layout, self.record, self.in_use);
        let mut pos = offset;
        while pos < data_end {
            let n = ((data_end - pos) as usize).min(MAX_STREAM_READ);
            let fail = |detail: String| IoError::Read {
                offset,
                len,
                source: std::io::Error::other(detail),
            };
            let c = stream.read(pos, n).map_err(|e| fail(e.to_string()))?;
            if let Some(r) = c.untrusted().next() {
                return Err(fail(format!("tramo no fiable del flujo: {r}")));
            }
            let at = (pos - offset) as usize;
            buf[at..at + c.data.len()].copy_from_slice(&c.data);
            pos += c.data.len() as u64;
            if c.data.is_empty() {
                break;
            }
        }
        let tail = (data_end.max(offset) - offset) as usize;
        buf[tail..].fill(0);
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> mr_io::Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        if count == 0 || lba.checked_add(count).is_none_or(|e| e > total) {
            return Err(IoError::InvalidRange { lba, count, total });
        }
        let first_sparse = self
            .layout
            .data_size
            .div_ceil(u64::from(STREAM_SOURCE_SECTOR));
        let end = lba + count;
        let mut runs = Vec::new();
        if lba < first_sparse {
            runs.push((lba, first_sparse.min(end) - lba, Allocation::Data));
        }
        if end > first_sparse {
            let a = lba.max(first_sparse);
            runs.push((a, end - a, Allocation::Sparse));
        }
        Ok(runs)
    }
}

/// Nombre preferido (no DOS) del registro, o `registro N`.
fn display_name(vol: &NtfsVolume, record: u64) -> String {
    vol.file(record)
        .ok()
        .and_then(|f| mr_fs_ntfs::dir::preferred_name(&f.file_names()).map(|n| n.name.clone()))
        .unwrap_or_else(|| format!("registro {record}"))
}

/// Abre el flujo `$DATA` `stream` (`""` = el principal) del registro `record` como un origen de
/// solo lectura con sectores de 512 B (ver el visor). `src` es el origen del volumen: el nuevo
/// origen conserva su ruta y sus segmentos, así que una salida nunca puede ser la imagen.
pub fn ntfs_stream_source(
    src: &Source,
    vol: &Arc<NtfsVolume>,
    record: u64,
    stream: &str,
) -> Result<Source> {
    let s = vol.open_stream(record, stream)?;
    let layout = s.layout().clone();
    let in_use = s.record_in_use();
    drop(s);
    let size = layout.data_size;
    let name = display_name(vol, record);
    let p = src.info();
    let mut description = format!(
        "Fichero NTFS «{name}» (registro {record}{}, {size} bytes",
        if stream.is_empty() {
            String::new()
        } else {
            format!(", flujo «{stream}»")
        }
    );
    if !size.is_multiple_of(u64::from(STREAM_SOURCE_SECTOR)) {
        description.push_str("; el último sector se completa con ceros");
    }
    if !in_use {
        description.push_str("; registro no en uso: los clústeres pueden haberse reutilizado");
    }
    description.push_str(&format!(") de: {}", p.description));
    let dev = NtfsStreamDevice {
        vol: Arc::clone(vol),
        layout,
        record,
        in_use,
    };
    Ok(Arc::new(OpenSource::new(
        Arc::new(dev),
        Meta {
            path: p.path.clone(),
            format: mr_io::ImageFormat::Raw,
            detected: p.detected,
            description,
            segments: p.segments.clone(),
            range: None,
            external_files: p.external_files.clone(),
        },
    )))
}

/// Resultado de copiar un fichero NTFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfsCopyResult {
    /// Ruta final escrita.
    pub path: PathBuf,
    /// Bytes por estado y tramos no fiables (escritos con el patrón `BAD!`).
    pub report: CopyReport,
    /// `false` si el registro no está en uso (fichero borrado).
    pub record_in_use: bool,
}

/// Escritor que informa del progreso y atiende la cancelación entre trozos.
struct Progress<'a, W: Write> {
    inner: W,
    ctx: &'a TaskContext,
    done: u64,
}

impl<W: Write> Write for Progress<'_, W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.ctx.is_cancelled() {
            // No es Interrupted: write_all reintenta esos errores.
            return Err(std::io::Error::other("copia cancelada"));
        }
        let n = self.inner.write(b)?;
        self.done += n as u64;
        self.ctx.set_done(self.done);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Copia el flujo `stream` del registro `record` del volumen `vol` (abierto sobre `src`) a `out`
/// con la política común de salida ([`create_output`]): nunca el origen ni un dispositivo ni un
/// directorio, nunca sobre un fichero existente sin `overwrite`, escritura en un temporal del
/// mismo directorio renombrado al terminar y borrado si se cancela o falla. Los tramos no fiables
/// se escriben con el patrón de relleno y se informan en el resultado.
pub fn ntfs_copy_file(
    src: &Source,
    vol: &NtfsVolume,
    record: u64,
    stream: &str,
    out: &Path,
    overwrite: bool,
    ctx: &TaskContext,
) -> Result<NtfsCopyResult> {
    // El flujo se abre antes de crear la salida: un error no deja ficheros.
    let s = vol.open_stream(record, stream)?;
    ctx.set_total(s.size());
    let file = create_output(src, out, overwrite)?;
    let mut w = Progress {
        inner: file,
        ctx,
        done: 0,
    };
    let report = match s.copy_to(&mut w) {
        Ok(r) => r,
        Err(CopyError::Ntfs(e)) => return Err(ApiError::Ntfs(e)),
        Err(CopyError::Write(e)) if ctx.is_cancelled() => {
            drop(e);
            return Err(ApiError::Cancelled);
        }
        Err(CopyError::Write(e)) => return Err(ApiError::Io(IoError::Io(e))),
    };
    if ctx.is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    let path = w.inner.commit()?;
    Ok(NtfsCopyResult {
        path,
        report,
        record_in_use: s.record_in_use(),
    })
}

/// Lanza [`ntfs_copy_file`] en segundo plano (progreso en bytes, cancelable: al cancelar no queda
/// ningún fichero con el nombre final).
pub fn start_ntfs_copy_task(
    src: Source,
    vol: Arc<NtfsVolume>,
    record: u64,
    stream: String,
    out: PathBuf,
    overwrite: bool,
) -> Result<Arc<Task<NtfsCopyResult>>> {
    Task::spawn(move |ctx| ntfs_copy_file(&src, &vol, record, &stream, &out, overwrite, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskState;
    use mr_fs_ntfs::testvol::{tree_data, tree_volume};
    use mr_fs_ntfs::EntryState;

    fn volume() -> (tempfile::TempDir, Source, Arc<NtfsVolume>) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("arbol.img");
        std::fs::write(&p, tree_volume(true).image).unwrap();
        let src = crate::open_source(&p, 0).unwrap();
        let vol = Arc::new(crate::ntfs_open(&src).unwrap());
        (dir, src, vol)
    }

    #[test]
    fn list_resolve_and_path_of() {
        let (_d, _src, vol) = volume();
        let l = ntfs_list_dir(&vol, 16).unwrap();
        assert_eq!(l.live().count(), 4);
        assert!(l
            .entries
            .iter()
            .any(|e| matches!(e.state, EntryState::NotInUse { .. })));
        let e = ntfs_resolve(&vol, "/docs/ñandú.BIN").unwrap();
        assert_eq!((e.record(), e.size), (20, 3000));
        assert_eq!(ntfs_path_of(&vol, 21).unwrap(), "/Docs/Sub/deep.txt");
        assert!(matches!(
            ntfs_resolve(&vol, "/nada"),
            Err(ApiError::Ntfs(mr_fs_ntfs::NtfsError::NotFound { .. }))
        ));
    }

    #[test]
    fn tree_task_reports_progress_and_cancels() {
        let (_d, _src, vol) = volume();
        let t = start_ntfs_tree_task(Arc::clone(&vol), TreeOptions::default()).unwrap();
        let tree = t.wait().unwrap();
        assert_eq!(t.state(), TaskState::Completed);
        assert_eq!(tree.status, TreeStatus::Complete);
        assert_eq!(tree.orphans.len(), 3);
        let (done, total) = t.progress();
        assert!(done == total && total > 0, "{done}/{total}");
        let ctx = TaskContext::detached();
        ctx.cancel_for_tests();
        let tree = ntfs_tree(&vol, &TreeOptions::default(), &ctx).unwrap();
        assert_eq!(tree.status, TreeStatus::Cancelled);
    }

    #[test]
    fn stream_source_presents_the_file_by_sectors() {
        let (_d, src, vol) = volume();
        let s = ntfs_stream_source(&src, &vol, 20, "").unwrap();
        assert_eq!(s.info().sector_size, 512);
        assert_eq!(s.info().size_bytes, 3072);
        assert_eq!(s.info().path, src.info().path);
        assert!(s.info().description.contains("«ñandú.bin»"));
        let (data, states) = crate::read_sectors_mapped(s.as_ref(), 0, 6, 0).unwrap();
        assert_eq!(&data[..3000], &tree_data()[..]);
        assert!(data[3000..].iter().all(|b| *b == 0));
        assert!(states.iter().all(|s| s.is_trustworthy()));
        // Fuera del flujo, error de rango; un flujo inexistente, error tipado.
        assert!(crate::read_sectors(s.as_ref(), 6, 1).is_err());
        assert!(ntfs_stream_source(&src, &vol, 20, "ads").is_err());
    }

    #[test]
    fn copy_follows_the_output_policy() {
        let (d, src, vol) = volume();
        let out = d.path().join("copia.bin");
        let ctx = TaskContext::detached();
        let r = ntfs_copy_file(&src, &vol, 20, "", &out, false, &ctx).unwrap();
        assert_eq!(r.path, out);
        assert_eq!(std::fs::read(&out).unwrap(), tree_data());
        assert_eq!(r.report.untrusted_bytes(), 0);
        assert_eq!(ctx.done(), 3000);
        // Nunca se sobrescribe sin petición; nunca sobre el origen; con petición, sí.
        assert!(matches!(
            ntfs_copy_file(&src, &vol, 21, "", &out, false, &ctx),
            Err(ApiError::OutputExists(_))
        ));
        assert!(matches!(
            ntfs_copy_file(&src, &vol, 21, "", &src.info().path, true, &ctx),
            Err(ApiError::InvalidArgument(_))
        ));
        ntfs_copy_file(&src, &vol, 21, "", &out, true, &ctx).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"profundo");
        // Cancelada: no queda ni el fichero final ni el temporal.
        let other = d.path().join("cancelada.bin");
        let c = TaskContext::detached();
        c.cancel_for_tests();
        assert!(matches!(
            ntfs_copy_file(&src, &vol, 20, "", &other, false, &c),
            Err(ApiError::Cancelled)
        ));
        let left: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        // La tarea.
        let t = start_ntfs_copy_task(
            Arc::clone(&src),
            Arc::clone(&vol),
            17,
            String::new(),
            d.path().join("hola.txt"),
            false,
        )
        .unwrap();
        assert_eq!(t.wait().unwrap().report.bytes, 4);
        assert_eq!(std::fs::read(d.path().join("hola.txt")).unwrap(), b"hola");
    }
}
