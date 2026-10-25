//! Sistema de archivos NTFS (#22 y #23, ampliaciones compatibles de la ABI 3): volumen,
//! directorios, rutas, árbol en segundo plano, un fichero como origen, copia de un fichero a una
//! salida, borrados y entradas residuales de los índices, y recuperación de un borrado.
//!
//! Mismos patrones que las particiones: handles del registro (nunca punteros), estructuras de
//! salida versionadas por `struct_size`, consultas por índice y tareas con
//! `mr_task_status_get` / `mr_task_cancel` / `mr_task_free`.

use std::path::PathBuf;
use std::sync::Arc;

use mr_api::mr_fs_ntfs::deleted::ClusterState;
use mr_api::mr_fs_ntfs::dir::{DirEntry, OrphanReason, Tree, TreeOptions, TreeStatus};
use mr_api::mr_fs_ntfs::upcase::UpcaseSource;
use mr_api::mr_fs_ntfs::{
    BootSource, DeletedEntry, DeletedScan, DeletedStatus, EntryOrigin, EntryState, NtfsVolume,
    ParentState, RangeState, RecordState, Recoverability, RecoveryReport, SuspectRange, TreeIssue,
};
use mr_api::task::Task;
use mr_api::{NtfsCopyResult, Source};

use super::{
    api_fail, bad_task, check_struct_size, confidence_code, fail, guard, index_error, insert,
    read_utf8, settle, with_entry, write_out, write_str, Entry, Outcome, MR_ERR_INVALID_ARGUMENT,
    MR_ERR_INVALID_HANDLE, MR_ERR_NOT_FINISHED, MR_OK,
};

/// Volumen NTFS registrado: el origen (para las salidas y los flujos) y el volumen.
pub(crate) struct NtfsEntry {
    source: Source,
    vol: Arc<NtfsVolume>,
}

/// Tarea de árbol con su resultado.
pub(crate) struct TreeEntry {
    pub(crate) task: Arc<Task<Tree>>,
    outcome: Outcome<Tree>,
}

/// Tarea de copia con su resultado.
pub(crate) struct CopyEntry {
    pub(crate) task: Arc<Task<NtfsCopyResult>>,
    outcome: Outcome<NtfsCopyResult>,
}

pub const MR_NTFS_BOOT_FROM_BACKUP: u32 = 1;
pub const MR_NTFS_VERSION_KNOWN: u32 = 2;
pub const MR_NTFS_UPCASE_FROM_VOLUME: u32 = 4;

pub const MR_NTFS_ENTRY_LIVE: u32 = 1;
pub const MR_NTFS_ENTRY_STALE_NOT_IN_USE: u32 = 2;
pub const MR_NTFS_ENTRY_STALE_REUSED: u32 = 3;
pub const MR_NTFS_ENTRY_RECORD_UNUSABLE: u32 = 4;
pub const MR_NTFS_ENTRY_INVALID_REFERENCE: u32 = 5;

pub const MR_NTFS_ENTRY_DIRECTORY: u32 = 1;
pub const MR_NTFS_ENTRY_SYSTEM: u32 = 2;
pub const MR_NTFS_ENTRY_NAME_LOSSY: u32 = 4;
pub const MR_NTFS_ENTRY_HAS_DOS_NAME: u32 = 8;
pub const MR_NTFS_ENTRY_TIMES_FROM_INDEX: u32 = 16;

pub const MR_NTFS_TREE_NO_ORPHANS: u32 = 1;
pub const MR_NTFS_TREE_COMPLETE: u32 = 1;
pub const MR_NTFS_TREE_LIMIT: u32 = 2;
pub const MR_NTFS_TREE_CANCELLED: u32 = 3;
pub const MR_NTFS_ROOT_PARENT: u64 = u64::MAX;

pub const MR_NTFS_COPY_OVERWRITE: u32 = 1;
pub const MR_NTFS_COPY_RANGES_TRUNCATED: u32 = 1;
pub const MR_NTFS_COPY_RECORD_NOT_IN_USE: u32 = 2;

/// Datos del volumen (72 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsVolumeInfo {
    pub struct_size: u32,
    /// `MR_CONFIDENCE_*`.
    pub confidence: u32,
    pub serial: u64,
    pub record_count: u64,
    pub total_clusters: u64,
    pub cluster_size: u32,
    pub record_size: u32,
    pub index_record_size: u32,
    pub bytes_per_sector: u32,
    pub version_major: u32,
    pub version_minor: u32,
    /// `MR_NTFS_BOOT_FROM_BACKUP | MR_NTFS_VERSION_KNOWN | MR_NTFS_UPCASE_FROM_VOLUME`.
    pub flags: u32,
    pub anomaly_count: u32,
    pub mft_lcn: u64,
}

/// Entrada de directorio (104 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsEntry {
    pub struct_size: u32,
    /// `MR_NTFS_ENTRY_LIVE`... en listados y árbol; `MR_NTFS_ENTRY_DELETED` (16) a
    /// `MR_NTFS_ENTRY_RESIDUAL_FREE` (19) en los borrados (#23; hasta 31 reservados).
    pub state: u32,
    pub record: u64,
    pub parent_record: u64,
    pub sequence: u32,
    pub parent_sequence: u32,
    pub size: u64,
    pub allocated_size: u64,
    /// FILETIME (100 ns desde 1601, UTC).
    pub created: u64,
    pub modified: u64,
    pub mft_modified: u64,
    pub accessed: u64,
    /// Atributos DOS.
    pub file_attributes: u32,
    /// `MR_NTFS_ENTRY_DIRECTORY | SYSTEM | NAME_LOSSY | HAS_DOS_NAME | TIMES_FROM_INDEX`.
    pub flags: u32,
    /// 0 POSIX, 1 Win32, 2 DOS, 3 Win32 y DOS.
    pub name_space: u32,
    pub link_count: u32,
    /// Con `MR_NTFS_ENTRY_RECORD_UNUSABLE`: estado del registro (1 en uso … 8 dañado).
    pub record_state: u32,
    /// Secuencia actual del registro (0 si no se pudo leer).
    pub found_sequence: u32,
}

/// Resumen de un listado (32 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsListingInfo {
    pub struct_size: u32,
    pub entry_count: u32,
    pub directory_record: u64,
    pub directory_sequence: u32,
    pub anomaly_count: u32,
    pub live_count: u32,
    /// 1 si algún nodo o registro tenía sectores no fiables.
    pub unreadable: u32,
}

/// Resumen del árbol (40 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsTreeInfo {
    pub struct_size: u32,
    /// `MR_NTFS_TREE_COMPLETE` / `_LIMIT` / `_CANCELLED`.
    pub status: u32,
    pub node_count: u64,
    pub orphan_count: u64,
    pub anomaly_count: u32,
    pub reserved: u32,
    pub directories: u64,
}

/// Registro en uso no alcanzado desde la raíz (40 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsOrphan {
    pub struct_size: u32,
    /// `MR_NTFS_ORPHAN_*`.
    pub reason: u32,
    pub record: u64,
    pub parent_record: u64,
    pub sequence: u32,
    pub parent_sequence: u32,
    /// `MR_NTFS_ENTRY_DIRECTORY`.
    pub flags: u32,
    pub reserved: u32,
}

/// Informe de una copia (56 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsCopyReport {
    pub struct_size: u32,
    pub untrusted_range_count: u32,
    pub bytes: u64,
    /// Bytes no fiables: con el patrón de relleno o posiblemente sobrescritos (#23).
    pub untrusted_bytes: u64,
    pub sparse_bytes: u64,
    pub uninitialized_bytes: u64,
    /// Parte de `untrusted_bytes` leída del disco pero en clústeres que pueden haberse
    /// reutilizado (`RangeState::PossiblyOverwritten`): nunca son datos fiables.
    pub possibly_overwritten_bytes: u64,
    /// `MR_NTFS_COPY_RANGES_TRUNCATED | MR_NTFS_COPY_RECORD_NOT_IN_USE`.
    pub flags: u32,
    pub reserved: u32,
}

fn orphan_code(r: &OrphanReason) -> u32 {
    match r {
        OrphanReason::NoName => 1,
        OrphanReason::ParentOutOfRange => 2,
        OrphanReason::ParentUnusable { .. } => 3,
        OrphanReason::ParentNotInUse => 4,
        OrphanReason::ParentReused { .. } => 5,
        OrphanReason::ParentNotDirectory => 6,
        OrphanReason::NotIndexed => 7,
        OrphanReason::ParentOrphan => 8,
        OrphanReason::Loop => 9,
        OrphanReason::TooDeep => 10,
    }
}

fn record_state_code(s: RecordState) -> u32 {
    RecordState::ALL
        .iter()
        .position(|x| *x == s)
        .map_or(0, |i| i as u32 + 1)
}

fn entry_of(e: &DirEntry) -> MrNtfsEntry {
    let (state, record_state) = match &e.state {
        EntryState::Live => (MR_NTFS_ENTRY_LIVE, 0),
        EntryState::NotInUse { .. } => (MR_NTFS_ENTRY_STALE_NOT_IN_USE, 0),
        EntryState::Reused { .. } => (MR_NTFS_ENTRY_STALE_REUSED, 0),
        EntryState::RecordUnusable { state, .. } => {
            (MR_NTFS_ENTRY_RECORD_UNUSABLE, record_state_code(*state))
        }
        EntryState::InvalidReference { .. } => (MR_NTFS_ENTRY_INVALID_REFERENCE, 0),
    };
    let mut flags = 0;
    for (on, bit) in [
        (e.is_directory, MR_NTFS_ENTRY_DIRECTORY),
        (e.system, MR_NTFS_ENTRY_SYSTEM),
        (e.name_lossy, MR_NTFS_ENTRY_NAME_LOSSY),
        (e.dos_name.is_some(), MR_NTFS_ENTRY_HAS_DOS_NAME),
        (e.times_from_index, MR_NTFS_ENTRY_TIMES_FROM_INDEX),
    ] {
        if on {
            flags |= bit;
        }
    }
    MrNtfsEntry {
        struct_size: std::mem::size_of::<MrNtfsEntry>() as u32,
        state,
        record: e.reference.record,
        parent_record: e.parent.record,
        sequence: u32::from(e.reference.sequence),
        parent_sequence: u32::from(e.parent.sequence),
        size: e.size,
        allocated_size: e.allocated_size,
        created: e.times.created,
        modified: e.times.modified,
        mft_modified: e.times.mft_modified,
        accessed: e.times.accessed,
        file_attributes: e.file_attributes,
        flags,
        name_space: match e.namespace {
            mr_api::mr_fs_ntfs::values::Namespace::Posix => 0,
            mr_api::mr_fs_ntfs::values::Namespace::Win32 => 1,
            mr_api::mr_fs_ntfs::values::Namespace::Dos => 2,
            mr_api::mr_fs_ntfs::values::Namespace::Win32AndDos => 3,
        },
        link_count: u32::from(e.link_count),
        record_state,
        found_sequence: e.record_sequence.map_or(0, u32::from),
    }
}

fn volume(h: u64) -> Result<(Source, Arc<NtfsVolume>), i32> {
    match with_entry(h, |e| match e {
        Entry::Ntfs(n) => Some((Arc::clone(&n.source), Arc::clone(&n.vol))),
        _ => None,
    }) {
        Some(Some(v)) => Ok(v),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de volumen NTFS no válido: {h}"),
        )),
    }
}

fn with_listing<R>(h: u64, f: impl FnOnce(&mr_api::mr_fs_ntfs::DirListing) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::NtfsListing(l) => Some(f(l)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de listado NTFS no válido: {h}"),
        )),
    }
}

fn with_tree<R>(task: u64, f: impl FnOnce(&Tree) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::NtfsTree(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
            Some(Ok(t)) => Some(f(t)),
            _ => None,
        }),
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es un árbol NTFS"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(Some(v))) => Ok(v),
        Some(Ok(None)) => Err(fail(MR_ERR_NOT_FINISHED, "resultado no disponible")),
    }
}

/// Detecta y abre el volumen NTFS que empieza en el origen (imagen sin tabla, partición o
/// volumen encontrado). El volumen conserva el origen aunque este se cierre.
///
/// # Safety
/// `out_volume` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_open(source_h: u64, out_volume: *mut u64) -> i32 {
    guard(|| {
        if out_volume.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let src = match super::source(source_h) {
            Ok(s) => s,
            Err(c) => return c,
        };
        match mr_api::ntfs_open(&src) {
            // SAFETY: `out_volume` no es nulo.
            Ok(vol) => unsafe {
                write_out(
                    out_volume,
                    insert(Entry::Ntfs(Box::new(NtfsEntry {
                        source: src,
                        vol: Arc::new(vol),
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// `out` debe apuntar a un `MrNtfsVolumeInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_volume_info(volume_h: u64, out: *mut MrNtfsVolumeInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_volume_info") } {
            return c;
        }
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        let i = vol.info();
        let mut flags = 0;
        if matches!(i.boot_source, BootSource::Backup { .. }) {
            flags |= MR_NTFS_BOOT_FROM_BACKUP;
        }
        if i.version.is_some() {
            flags |= MR_NTFS_VERSION_KNOWN;
        }
        if vol.upcase().source() == &UpcaseSource::Volume {
            flags |= MR_NTFS_UPCASE_FROM_VOLUME;
        }
        let (major, minor) = i.version.unwrap_or((0, 0));
        let v = MrNtfsVolumeInfo {
            struct_size: std::mem::size_of::<MrNtfsVolumeInfo>() as u32,
            confidence: confidence_code(&i.confidence()),
            serial: i.boot.serial,
            record_count: vol.record_count(),
            total_clusters: i.boot.total_clusters,
            cluster_size: i.boot.cluster_size,
            record_size: i.boot.mft_record_size,
            index_record_size: i.boot.index_record_size,
            bytes_per_sector: i.boot.bytes_per_sector,
            version_major: u32::from(major),
            version_minor: u32::from(minor),
            flags,
            anomaly_count: u32::try_from(i.anomalies.len()).unwrap_or(u32::MAX),
            mft_lcn: i.boot.mft_lcn,
        };
        // SAFETY: `out` comprobado.
        unsafe { write_out(out, v) }
    })
}

/// Etiqueta del volumen (cadena vacía si no tiene).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_volume_label(
    volume_h: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| match volume(volume_h) {
        // SAFETY: contrato del llamador.
        Ok((_, vol)) => unsafe {
            write_str(vol.info().label.as_deref().unwrap_or(""), buf, cap, out_len)
        },
        Err(c) => c,
    })
}

/// Anomalía `index` del volumen: `"<código>\n<descripción>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_volume_anomaly_text(
    volume_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match vol.info().anomalies.get(index as usize) {
            // SAFETY: contrato del llamador.
            Some(a) => unsafe { write_str(&format!("{}\n{a}", a.code()), buf, cap, out_len) },
            None => index_error("anomalía", u64::from(index)),
        }
    })
}

/// Libera el volumen (los listados, tareas y orígenes abiertos desde él siguen siendo válidos).
#[no_mangle]
pub extern "C" fn mr_ntfs_volume_free(volume_h: u64) -> i32 {
    free_kind(volume_h, "volumen NTFS", |e| matches!(e, Entry::Ntfs(_)))
}

fn free_kind(h: u64, what: &str, is: impl Fn(&Entry) -> bool) -> i32 {
    guard(|| {
        let mut map = super::registry().lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&h) {
            Some(e) if is(e) => {
                map.remove(&h);
                MR_OK
            }
            _ => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de {what} no válido: {h}"),
            ),
        }
    })
}

/// Lista el directorio del registro `record` (5 = raíz). Devuelve un handle de listado.
///
/// # Safety
/// `out_listing` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_list_dir(
    volume_h: u64,
    record: u64,
    out_listing: *mut u64,
) -> i32 {
    guard(|| {
        if out_listing.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::ntfs_list_dir(&vol, record) {
            // SAFETY: `out_listing` no es nulo.
            Ok(l) => unsafe { write_out(out_listing, insert(Entry::NtfsListing(Box::new(l)))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// `out` debe apuntar a un `MrNtfsListingInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_listing_info(listing_h: u64, out: *mut MrNtfsListingInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_listing_info") } {
            return c;
        }
        match with_listing(listing_h, |l| MrNtfsListingInfo {
            struct_size: std::mem::size_of::<MrNtfsListingInfo>() as u32,
            entry_count: l.entries.len() as u32,
            directory_record: l.directory.record,
            directory_sequence: u32::from(l.directory.sequence),
            anomaly_count: l.anomalies.len() as u32,
            live_count: l.live().count() as u32,
            unreadable: u32::from(l.has_unreadable()),
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Entrada `index` del listado (orden de colación).
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsEntry` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_listing_entry(
    listing_h: u64,
    index: u32,
    out: *mut MrNtfsEntry,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_entry") } {
            return c;
        }
        match with_listing(listing_h, |l| l.entries.get(index as usize).map(entry_of)) {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("entrada", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Nombre (UTF-8; las unidades UTF-16 sueltas aparecen como U+FFFD) de la entrada `index`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_listing_entry_name(
    listing_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_listing(listing_h, |l| {
            l.entries.get(index as usize).map(|e| e.name.clone())
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(n)) => unsafe { write_str(&n, buf, cap, out_len) },
            Ok(None) => index_error("entrada", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Anomalía `index` del listado: `"<código>\n<descripción>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_listing_anomaly_text(
    listing_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_listing(listing_h, |l| {
            l.anomalies
                .get(index as usize)
                .map(|a| format!("{}\n{a}", a.issue.code()))
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("anomalía", u64::from(index)),
            Err(c) => c,
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_ntfs_listing_free(listing_h: u64) -> i32 {
    free_kind(listing_h, "listado NTFS", |e| {
        matches!(e, Entry::NtfsListing(_))
    })
}

/// Resuelve una ruta absoluta (`/a/b`, también con `\`) sin distinguir mayúsculas. Ruta que no
/// existe → `MR_ERR_NOT_FOUND`; componente intermedio que no es un directorio o ruta mal formada →
/// `MR_ERR_INVALID_ARGUMENT`.
///
/// # Safety
/// `path` debe apuntar a `path_len` bytes; `out` a un `MrNtfsEntry` con `struct_size` escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_resolve(
    volume_h: u64,
    path: *const u8,
    path_len: usize,
    out: *mut MrNtfsEntry,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_entry") } {
            return c;
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::ntfs_resolve(&vol, p) {
            // SAFETY: `out` comprobado.
            Ok(e) => unsafe { write_out(out, entry_of(&e)) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Ruta del registro `record` subiendo por las referencias al padre. Huérfano →
/// `MR_ERR_CORRUPT` con el motivo en `mr_last_error`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_path_of(
    volume_h: u64,
    record: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::ntfs_path_of(&vol, record) {
            // SAFETY: contrato del llamador.
            Ok(p) => unsafe { write_str(&p, buf, cap, out_len) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Lanza el árbol completo del volumen (tarea con progreso; al cancelar termina cancelada con
/// el árbol parcial). `flags`: `MR_NTFS_TREE_NO_ORPHANS` omite la búsqueda de huérfanos.
///
/// # Safety
/// `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_start(volume_h: u64, flags: u32, out_task: *mut u64) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_NTFS_TREE_NO_ORPHANS != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones del árbol no válidas");
        }
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        let opts = TreeOptions {
            orphans: flags & MR_NTFS_TREE_NO_ORPHANS == 0,
            ..TreeOptions::default()
        };
        match mr_api::start_ntfs_tree_task(vol, opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::NtfsTree(Box::new(TreeEntry {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// # Safety
/// `out` debe apuntar a un `MrNtfsTreeInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_result(task: u64, out: *mut MrNtfsTreeInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_tree_info") } {
            return c;
        }
        match with_tree(task, |t| MrNtfsTreeInfo {
            struct_size: std::mem::size_of::<MrNtfsTreeInfo>() as u32,
            status: match t.status {
                TreeStatus::Complete => MR_NTFS_TREE_COMPLETE,
                TreeStatus::Limit => MR_NTFS_TREE_LIMIT,
                TreeStatus::Cancelled => MR_NTFS_TREE_CANCELLED,
            },
            node_count: t.nodes.len() as u64,
            orphan_count: t.orphans.len() as u64,
            anomaly_count: u32::try_from(t.anomalies.len()).unwrap_or(u32::MAX),
            reserved: 0,
            directories: t.directories,
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Nodo `index` del árbol (0 = la raíz; orden en anchura): índice del padre
/// (`MR_NTFS_ROOT_PARENT` en la raíz), profundidad y entrada.
///
/// # Safety
/// `out_parent` y `out_depth` válidos; `out_entry` con `struct_size` escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_node(
    task: u64,
    index: u64,
    out_parent: *mut u64,
    out_depth: *mut u32,
    out_entry: *mut MrNtfsEntry,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out_entry, "mr_ntfs_entry") } {
            return c;
        }
        if out_parent.is_null() || out_depth.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_tree(task, |t| {
            t.nodes.get(index as usize).map(|n| {
                (
                    n.parent.map_or(MR_NTFS_ROOT_PARENT, |p| p as u64),
                    n.depth,
                    entry_of(&n.entry),
                )
            })
        }) {
            // SAFETY: punteros comprobados.
            Ok(Some((p, d, e))) => unsafe {
                out_parent.write(p);
                out_depth.write(d);
                write_out(out_entry, e)
            },
            Ok(None) => index_error("nodo", index),
            Err(c) => c,
        }
    })
}

/// Ruta completa del nodo `index` (`/` en la raíz).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_node_path(
    task: u64,
    index: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_tree(task, |t| {
            t.nodes.get(index as usize).map(|n| n.path.clone())
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(p)) => unsafe { write_str(&p, buf, cap, out_len) },
            Ok(None) => index_error("nodo", index),
            Err(c) => c,
        }
    })
}

/// Anomalía `index` del árbol: `"<código>\n<ruta del directorio>\n<descripción>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_anomaly_text(
    task: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_tree(task, |t| {
            t.anomalies.get(index as usize).map(|a| {
                let detail = match &a.issue {
                    TreeIssue::Index(i) => i.to_string(),
                    other => other.to_string(),
                };
                format!("{}\n{}\n{detail}", a.issue.code(), a.path)
            })
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("anomalía", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Huérfano `index`.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsOrphan` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_orphan(task: u64, index: u32, out: *mut MrNtfsOrphan) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_orphan") } {
            return c;
        }
        match with_tree(task, |t| {
            t.orphans.get(index as usize).map(|o| MrNtfsOrphan {
                struct_size: std::mem::size_of::<MrNtfsOrphan>() as u32,
                reason: orphan_code(&o.reason),
                record: o.record,
                parent_record: o.parent.record,
                sequence: u32::from(o.sequence),
                parent_sequence: u32::from(o.parent.sequence),
                flags: if o.is_directory {
                    MR_NTFS_ENTRY_DIRECTORY
                } else {
                    0
                },
                reserved: 0,
            })
        }) {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("huérfano", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Texto del huérfano `index`: `"<código>\n<nombre>\n<descripción del motivo>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_tree_orphan_text(
    task: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_tree(task, |t| {
            t.orphans
                .get(index as usize)
                .map(|o| format!("{}\n{}\n{}", o.reason.code(), o.name, o.reason))
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("huérfano", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Abre el flujo `$DATA` `stream` (vacío = el principal) del registro `record` como un origen de
/// sectores de 512 B (para el visor): los tramos no fiables se leen como sectores ilegibles y
/// los sectores enteros tras el final del fichero como dispersos. Se cierra con
/// `mr_source_close`.
///
/// # Safety
/// `stream` debe apuntar a `stream_len` bytes (o ser nulo con 0); `out_source` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_open_stream(
    volume_h: u64,
    record: u64,
    stream: *const u8,
    stream_len: usize,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        // SAFETY: contrato del llamador.
        let name = match unsafe { read_utf8(stream, stream_len) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let (src, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::ntfs_stream_source(&src, &vol, record, name) {
            // SAFETY: `out_source` no es nulo.
            Ok(s) => unsafe { write_out(out_source, insert(Entry::Source(s))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Copia el flujo `stream` del registro `record` a `out_path` (tarea con progreso en bytes y
/// cancelable). Política común de salida: nunca el origen (ni sus segmentos) ni un dispositivo ni
/// un directorio; si existe, `MR_ERR_ALREADY_EXISTS` salvo con `MR_NTFS_COPY_OVERWRITE`;
/// escritura en un temporal del mismo directorio que se renombra al terminar (nada queda con el
/// nombre final si falla o se cancela). La ruta se valida antes de lanzar la tarea.
///
/// # Safety
/// `stream` y `out_path` deben apuntar a sus longitudes en bytes; `out_task` válido.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mr_ntfs_copy_start(
    volume_h: u64,
    record: u64,
    stream: *const u8,
    stream_len: usize,
    out_path: *const u8,
    out_path_len: usize,
    flags: u32,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_NTFS_COPY_OVERWRITE != 0 {
            return fail(MR_ERR_INVALID_ARGUMENT, "opciones de copia no válidas");
        }
        // SAFETY: contrato del llamador.
        let name = match unsafe { read_utf8(stream, stream_len) } {
            Ok(s) => s.to_string(),
            Err(c) => return c,
        };
        // SAFETY: contrato del llamador.
        let path = match unsafe { read_utf8(out_path, out_path_len) } {
            Ok(p) if !p.is_empty() => PathBuf::from(p),
            Ok(_) => return fail(MR_ERR_INVALID_ARGUMENT, "ruta de salida vacía"),
            Err(c) => return c,
        };
        let (src, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        let overwrite = flags & MR_NTFS_COPY_OVERWRITE != 0;
        // Errores inmediatos (ruta prohibida, salida existente, fichero que no existe) antes de
        // lanzar nada.
        if let Err(e) = mr_api::check_output_path(&src, &path) {
            return api_fail(&e);
        }
        if !overwrite && path.exists() {
            return api_fail(&mr_api::ApiError::OutputExists(path));
        }
        if let Err(e) = vol.open_stream(record, &name) {
            return api_fail(&mr_api::ApiError::Ntfs(e));
        }
        match mr_api::start_ntfs_copy_task(src, vol, record, name, path, overwrite) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::NtfsCopy(Box::new(CopyEntry {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Informe de una copia terminada.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsCopyReport` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_copy_result(task: u64, out: *mut MrNtfsCopyReport) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_copy_report") } {
            return c;
        }
        let r = with_entry(task, |e| match e {
            Entry::NtfsCopy(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
                Some(Ok(r)) => Some(r.clone()),
                _ => None,
            }),
            _ => Err((
                MR_ERR_INVALID_HANDLE,
                format!("la tarea {task} no es una copia NTFS"),
            )),
        });
        let r = match r {
            None => return bad_task(task),
            Some(Err((c, m))) => return fail(c, m),
            Some(Ok(None)) => return fail(MR_ERR_NOT_FINISHED, "resultado no disponible"),
            Some(Ok(Some(r))) => r,
        };
        let mut flags = 0;
        if r.report.truncated {
            flags |= MR_NTFS_COPY_RANGES_TRUNCATED;
        }
        if !r.record_in_use {
            flags |= MR_NTFS_COPY_RECORD_NOT_IN_USE;
        }
        let v = MrNtfsCopyReport {
            struct_size: std::mem::size_of::<MrNtfsCopyReport>() as u32,
            untrusted_range_count: u32::try_from(r.report.untrusted.len()).unwrap_or(u32::MAX),
            bytes: r.report.bytes,
            untrusted_bytes: r.report.untrusted_bytes(),
            sparse_bytes: r.report.count(RangeState::Sparse),
            uninitialized_bytes: r.report.count(RangeState::Uninitialized),
            possibly_overwritten_bytes: r.report.count(RangeState::PossiblyOverwritten),
            flags,
            reserved: 0,
        };
        // SAFETY: `out` comprobado.
        unsafe { write_out(out, v) }
    })
}

// ---------------------------------------------------------------------------------------------
// Borrados y entradas residuales (#23)
// ---------------------------------------------------------------------------------------------

/// Tarea de borrados con su resultado.
pub(crate) struct DeletedEntryTask {
    pub(crate) task: Arc<Task<DeletedScan>>,
    outcome: Outcome<DeletedScan>,
}

/// Tarea de recuperación con su resultado.
pub(crate) struct RecoverEntry {
    pub(crate) task: Arc<Task<RecoveryReport>>,
    outcome: Outcome<RecoveryReport>,
}

/// Borrado con el registro (MFT; puede tener también orígenes de índice).
pub const MR_NTFS_ENTRY_DELETED: u32 = 16;
/// Solo en índices; el registro se reutilizó (el contenido es de otro fichero).
pub const MR_NTFS_ENTRY_RESIDUAL_REUSED: u32 = 17;
/// Solo en índices; el registro no es interpretable (`record_state`).
pub const MR_NTFS_ENTRY_RESIDUAL_UNUSABLE: u32 = 18;
/// Solo en índices; el registro está libre sin `$FILE_NAME`.
pub const MR_NTFS_ENTRY_RESIDUAL_FREE: u32 = 19;

pub const MR_NTFS_ORIGIN_MFT: u32 = 1;
pub const MR_NTFS_ORIGIN_INDEX_STALE: u32 = 2;
pub const MR_NTFS_ORIGIN_INDEX_SLACK: u32 = 4;
pub const MR_NTFS_ORIGIN_INDEX_FREE_NODE: u32 = 8;
pub const MR_NTFS_ORIGIN_DELETED_DIRECTORY_INDEX: u32 = 16;

pub const MR_NTFS_RECOVERABLE_NOT_APPLICABLE: u32 = 0;
pub const MR_NTFS_RECOVERABLE_FULL: u32 = 1;
pub const MR_NTFS_RECOVERABLE_PARTIAL: u32 = 2;
pub const MR_NTFS_RECOVERABLE_NONE: u32 = 3;

pub const MR_NTFS_PARENT_LIVE: u32 = 1;
pub const MR_NTFS_PARENT_DELETED: u32 = 2;
pub const MR_NTFS_PARENT_ORPHAN: u32 = 3;

pub const MR_NTFS_DELETED_ENTRIES_TRUNCATED: u32 = 1;
pub const MR_NTFS_DELETED_BITMAP_UNUSABLE: u32 = 2;
pub const MR_NTFS_DELETED_CLAIMS_TRUNCATED: u32 = 4;
pub const MR_NTFS_DELETED_RESIDUAL_TRUNCATED: u32 = 8;
pub const MR_NTFS_DELETED_INDEX_UNREADABLE: u32 = 16;
pub const MR_NTFS_DELETED_MFT_UNREADABLE: u32 = 32;

pub const MR_NTFS_DELETED_TEXT_NAME: u32 = 0;
pub const MR_NTFS_DELETED_TEXT_PATH: u32 = 1;
pub const MR_NTFS_DELETED_TEXT_SUMMARY: u32 = 2;
pub const MR_NTFS_DELETED_TEXT_ORIGINS: u32 = 3;

pub const MR_NTFS_RECOVER_OVERWRITE: u32 = 1;
pub const MR_NTFS_RECOVER_RANGES_TRUNCATED: u32 = 1;

pub const MR_NTFS_CLUSTER_FREE: u32 = 1;
pub const MR_NTFS_CLUSTER_OWNED: u32 = 2;
pub const MR_NTFS_CLUSTER_ALLOCATED: u32 = 3;
pub const MR_NTFS_CLUSTER_UNKNOWN: u32 = 4;
pub const MR_NTFS_CLUSTER_SHARED_WITH_DELETED: u32 = 5;
pub const MR_NTFS_NO_OWNER: u64 = u64::MAX;

/// Resumen de la búsqueda de borrados (88 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsDeletedInfo {
    pub struct_size: u32,
    /// `MR_NTFS_DELETED_*`.
    pub flags: u32,
    pub records: u64,
    pub entry_count: u64,
    /// Entradas que salen solo de los índices (estados 17 a 19).
    pub residual_only_count: u64,
    pub problem_count: u64,
    pub index_directories: u64,
    pub index_nodes: u64,
    pub index_nodes_unreadable: u64,
    pub index_free_nodes: u64,
    pub residual_accepted: u64,
    pub residual_rejected: u64,
}

/// Datos de un borrado además de su `MrNtfsEntry` (48 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsDeletedDetail {
    pub struct_size: u32,
    /// `MR_NTFS_RECOVERABLE_*` del flujo principal.
    pub recoverability: u32,
    /// `MR_NTFS_ORIGIN_*`.
    pub origins: u32,
    /// `MR_NTFS_PARENT_*`.
    pub parent_state: u32,
    pub stream_count: u32,
    pub index_ref_count: u32,
    /// Flujo principal: clústeres con contenido, libres y posiblemente sobrescritos.
    pub clusters: u64,
    pub free_clusters: u64,
    pub possibly_overwritten_clusters: u64,
}

/// Informe de una recuperación (56 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsRecoverReport {
    pub struct_size: u32,
    /// `MR_NTFS_RECOVER_RANGES_TRUNCATED`.
    pub flags: u32,
    pub bytes: u64,
    /// Bytes no fiables: patrón de relleno más posiblemente sobrescritos.
    pub untrusted_bytes: u64,
    pub possibly_overwritten_bytes: u64,
    /// Bytes con el patrón de relleno (ilegibles, sin mapa...).
    pub filled_bytes: u64,
    /// `MR_NTFS_RECOVERABLE_*` del flujo evaluado.
    pub recoverability: u32,
    pub suspect_count: u32,
    pub sequence: u32,
    pub reserved: u32,
}

/// Tramo de bytes leído de clústeres posiblemente sobrescritos (48 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrNtfsSuspectRange {
    pub struct_size: u32,
    /// `MR_NTFS_CLUSTER_*`.
    pub state: u32,
    pub offset: u64,
    pub length: u64,
    pub lcn: u64,
    pub clusters: u64,
    /// Registro dueño (vivo) o borrado que lo reclama; `MR_NTFS_NO_OWNER` si no hay.
    pub owner: u64,
}

fn recoverability_code(r: Option<Recoverability>) -> u32 {
    match r {
        None => MR_NTFS_RECOVERABLE_NOT_APPLICABLE,
        Some(Recoverability::Full) => MR_NTFS_RECOVERABLE_FULL,
        Some(Recoverability::Partial) => MR_NTFS_RECOVERABLE_PARTIAL,
        Some(Recoverability::None) => MR_NTFS_RECOVERABLE_NONE,
    }
}

fn origin_bit(o: EntryOrigin) -> u32 {
    match o {
        EntryOrigin::MftRecord => MR_NTFS_ORIGIN_MFT,
        EntryOrigin::IndexStale => MR_NTFS_ORIGIN_INDEX_STALE,
        EntryOrigin::IndexSlack => MR_NTFS_ORIGIN_INDEX_SLACK,
        EntryOrigin::IndexFreeNode => MR_NTFS_ORIGIN_INDEX_FREE_NODE,
        EntryOrigin::DeletedDirectoryIndex => MR_NTFS_ORIGIN_DELETED_DIRECTORY_INDEX,
        _ => 0,
    }
}

fn deleted_entry_of(e: &DeletedEntry) -> MrNtfsEntry {
    let (state, record_state, found) = match &e.status {
        DeletedStatus::Deleted => (MR_NTFS_ENTRY_DELETED, 0, u32::from(e.sequence)),
        DeletedStatus::RecordReused { found_sequence } => {
            (MR_NTFS_ENTRY_RESIDUAL_REUSED, 0, u32::from(*found_sequence))
        }
        DeletedStatus::RecordUnusable { state, .. } => (
            MR_NTFS_ENTRY_RESIDUAL_UNUSABLE,
            record_state_code(*state),
            0,
        ),
        DeletedStatus::RecordFree { found_sequence } => {
            (MR_NTFS_ENTRY_RESIDUAL_FREE, 0, u32::from(*found_sequence))
        }
    };
    let attrs = e
        .names
        .iter()
        .find(|n| n.name == e.name)
        .map_or(0, |n| n.file_attributes);
    let mut flags = 0;
    for (on, bit) in [
        (e.is_directory, MR_NTFS_ENTRY_DIRECTORY),
        (e.record < 16, MR_NTFS_ENTRY_SYSTEM),
        (e.name.contains('\u{FFFD}'), MR_NTFS_ENTRY_NAME_LOSSY),
        (!e.status.has_record(), MR_NTFS_ENTRY_TIMES_FROM_INDEX),
    ] {
        if on {
            flags |= bit;
        }
    }
    MrNtfsEntry {
        struct_size: std::mem::size_of::<MrNtfsEntry>() as u32,
        state,
        record: e.record,
        parent_record: e.parent.record,
        sequence: u32::from(e.sequence),
        parent_sequence: u32::from(e.parent.sequence),
        size: e.size,
        allocated_size: e.allocated_size,
        created: e.times.created,
        modified: e.times.modified,
        mft_modified: e.times.mft_modified,
        accessed: e.times.accessed,
        file_attributes: attrs,
        flags,
        name_space: match e.namespace {
            mr_api::mr_fs_ntfs::values::Namespace::Posix => 0,
            mr_api::mr_fs_ntfs::values::Namespace::Win32 => 1,
            mr_api::mr_fs_ntfs::values::Namespace::Dos => 2,
            mr_api::mr_fs_ntfs::values::Namespace::Win32AndDos => 3,
        },
        link_count: 0,
        record_state,
        found_sequence: found,
    }
}

fn with_deleted<R>(task: u64, f: impl FnOnce(&DeletedScan) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::NtfsDeleted(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
            Some(Ok(s)) => Some(f(s)),
            _ => None,
        }),
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es una búsqueda de borrados NTFS"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(Some(v))) => Ok(v),
        Some(Ok(None)) => Err(fail(MR_ERR_NOT_FINISHED, "resultado no disponible")),
    }
}

fn with_recover<R>(task: u64, f: impl FnOnce(&RecoveryReport) -> R) -> Result<R, i32> {
    let r = with_entry(task, |e| match e {
        Entry::NtfsRecover(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
            Some(Ok(s)) => Some(f(s)),
            _ => None,
        }),
        _ => Err((
            MR_ERR_INVALID_HANDLE,
            format!("la tarea {task} no es una recuperación NTFS"),
        )),
    });
    match r {
        None => Err(bad_task(task)),
        Some(Err((c, m))) => Err(fail(c, m)),
        Some(Ok(Some(v))) => Ok(v),
        Some(Ok(None)) => Err(fail(MR_ERR_NOT_FINISHED, "resultado no disponible")),
    }
}

/// Lanza la búsqueda de ficheros borrados (MFT y entradas residuales de los índices) del volumen
/// (tarea con progreso y cancelable).
///
/// # Safety
/// `out_task` debe ser un puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_deleted_start(volume_h: u64, out_task: *mut u64) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let (_, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::start_ntfs_deleted_task(vol) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::NtfsDeleted(Box::new(DeletedEntryTask {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Resumen de una búsqueda de borrados terminada.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsDeletedInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_deleted_result(task: u64, out: *mut MrNtfsDeletedInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_deleted_info") } {
            return c;
        }
        match with_deleted(task, |s| {
            let r = s.residual.clone().unwrap_or_default();
            let mut flags = 0;
            for (on, bit) in [
                (s.entries_truncated, MR_NTFS_DELETED_ENTRIES_TRUNCATED),
                (s.bitmap_issue.is_some(), MR_NTFS_DELETED_BITMAP_UNUSABLE),
                (s.claims_truncated, MR_NTFS_DELETED_CLAIMS_TRUNCATED),
                (r.truncated, MR_NTFS_DELETED_RESIDUAL_TRUNCATED),
                (r.nodes_unreadable > 0, MR_NTFS_DELETED_INDEX_UNREADABLE),
                (
                    s.problems
                        .iter()
                        .any(|p| p.state == RecordState::Unreadable),
                    MR_NTFS_DELETED_MFT_UNREADABLE,
                ),
            ] {
                if on {
                    flags |= bit;
                }
            }
            MrNtfsDeletedInfo {
                struct_size: std::mem::size_of::<MrNtfsDeletedInfo>() as u32,
                flags,
                records: s.records,
                entry_count: s.entries.len() as u64,
                residual_only_count: s.residual_only().count() as u64,
                problem_count: s.problems.len() as u64,
                index_directories: r.directories,
                index_nodes: r.nodes,
                index_nodes_unreadable: r.nodes_unreadable,
                index_free_nodes: r.free_nodes,
                residual_accepted: r.accepted,
                residual_rejected: r.rejected,
            }
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Entrada `index` de los borrados: `state` 16 (`MR_NTFS_ENTRY_DELETED`) a 19
/// (`MR_NTFS_ENTRY_RESIDUAL_*`), registro y secuencia, padre, tamaños, fechas y espacio de nombres.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsEntry` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_deleted_entry(
    task: u64,
    index: u64,
    out: *mut MrNtfsEntry,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_entry") } {
            return c;
        }
        match with_deleted(task, |s| {
            usize::try_from(index)
                .ok()
                .and_then(|i| s.entries.get(i))
                .map(deleted_entry_of)
        }) {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("entrada", index),
            Err(c) => c,
        }
    })
}

/// Recuperabilidad, orígenes, estado del padre y clústeres del flujo principal de la entrada
/// `index`.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsDeletedDetail` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_deleted_detail(
    task: u64,
    index: u64,
    out: *mut MrNtfsDeletedDetail,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_deleted_detail") } {
            return c;
        }
        match with_deleted(task, |s| {
            usize::try_from(index)
                .ok()
                .and_then(|i| s.entries.get(i))
                .map(|e| {
                    let main = e.main_stream();
                    MrNtfsDeletedDetail {
                        struct_size: std::mem::size_of::<MrNtfsDeletedDetail>() as u32,
                        recoverability: recoverability_code(e.recoverability()),
                        origins: e.origins.iter().fold(0, |a, o| a | origin_bit(*o)),
                        parent_state: match e.parent_state {
                            ParentState::Live => MR_NTFS_PARENT_LIVE,
                            ParentState::Deleted => MR_NTFS_PARENT_DELETED,
                            ParentState::Orphan(_) => MR_NTFS_PARENT_ORPHAN,
                        },
                        stream_count: e.streams.len() as u32,
                        index_ref_count: u32::try_from(e.index_refs.len()).unwrap_or(u32::MAX),
                        clusters: main.map_or(0, |a| a.clusters),
                        free_clusters: main.map_or(0, |a| a.free),
                        possibly_overwritten_clusters: main.map_or(0, |a| a.possibly_overwritten()),
                    }
                })
        }) {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("entrada", index),
            Err(c) => c,
        }
    })
}

fn deleted_text(s: &DeletedScan, e: &DeletedEntry, field: u32) -> Option<String> {
    Some(match field {
        MR_NTFS_DELETED_TEXT_NAME => e.name.clone(),
        MR_NTFS_DELETED_TEXT_PATH => e.display_path(),
        MR_NTFS_DELETED_TEXT_SUMMARY => {
            if !e.status.has_record() {
                e.status.to_string()
            } else if let Some(a) = e.main_stream() {
                s.summary(a)
            } else {
                String::new()
            }
        }
        MR_NTFS_DELETED_TEXT_ORIGINS => e
            .origins
            .iter()
            .map(|o| o.label())
            .collect::<Vec<_>>()
            .join(", "),
        _ => return None,
    })
}

/// Texto de la entrada `index`: `MR_NTFS_DELETED_TEXT_NAME` (nombre), `_PATH` (ruta o
/// `<huérfano N>/nombre`), `_SUMMARY` (motivo de la recuperabilidad o estado de una entrada solo
/// residual) u `_ORIGINS` (fuentes, separadas por comas).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_deleted_text(
    task: u64,
    index: u64,
    field: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        if field > MR_NTFS_DELETED_TEXT_ORIGINS {
            return fail(MR_ERR_INVALID_ARGUMENT, "campo de texto no válido");
        }
        match with_deleted(task, |s| {
            usize::try_from(index)
                .ok()
                .and_then(|i| s.entries.get(i))
                .and_then(|e| deleted_text(s, e, field))
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("entrada", index),
            Err(c) => c,
        }
    })
}

/// Recupera el flujo `stream` (vacío = el principal) del borrado `record` con la secuencia
/// `sequence` (la de su entrada) en `out_path` (tarea). Antes de lanzarla se comprueban la ruta
/// (nunca el origen, un dispositivo ni un directorio; si existe, `MR_ERR_ALREADY_EXISTS` salvo con
/// `MR_NTFS_RECOVER_OVERWRITE`) y el registro: un registro en uso, con otra secuencia o libre es
/// `MR_ERR_INVALID_ARGUMENT` (una entrada solo residual no se recupera). Escritura atómica.
///
/// # Safety
/// `stream` y `out_path` deben apuntar a sus longitudes en bytes; `out_task` válido.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mr_ntfs_recover_start(
    volume_h: u64,
    record: u64,
    sequence: u32,
    stream: *const u8,
    stream_len: usize,
    out_path: *const u8,
    out_path_len: usize,
    flags: u32,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_NTFS_RECOVER_OVERWRITE != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones de recuperación no válidas",
            );
        }
        let Ok(sequence) = u16::try_from(sequence) else {
            return fail(MR_ERR_INVALID_ARGUMENT, "secuencia fuera de rango");
        };
        // SAFETY: contrato del llamador.
        let name = match unsafe { read_utf8(stream, stream_len) } {
            Ok(s) => s.to_string(),
            Err(c) => return c,
        };
        // SAFETY: contrato del llamador.
        let path = match unsafe { read_utf8(out_path, out_path_len) } {
            Ok(p) if !p.is_empty() => PathBuf::from(p),
            Ok(_) => return fail(MR_ERR_INVALID_ARGUMENT, "ruta de salida vacía"),
            Err(c) => return c,
        };
        let (src, vol) = match volume(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        // El registro se comprueba antes de lanzar nada (sin crear la salida).
        match vol.file(record) {
            Ok(f) if f.in_use() || f.header.sequence != sequence || f.file_names().is_empty() => {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    format!(
                        "el registro {record} (secuencia {}) no es el borrado pedido (secuencia \
                         {sequence}): no se recupera",
                        f.header.sequence
                    ),
                )
            }
            Ok(_) => {}
            Err(e) => return api_fail(&mr_api::ApiError::Ntfs(e)),
        }
        let overwrite = flags & MR_NTFS_RECOVER_OVERWRITE != 0;
        match mr_api::start_ntfs_recover_task(src, vol, record, sequence, name, path, overwrite) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::NtfsRecover(Box::new(RecoverEntry {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Informe de una recuperación terminada.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsRecoverReport` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_recover_result(task: u64, out: *mut MrNtfsRecoverReport) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_recover_report") } {
            return c;
        }
        match with_recover(task, |r| MrNtfsRecoverReport {
            struct_size: std::mem::size_of::<MrNtfsRecoverReport>() as u32,
            flags: if r.copy.truncated {
                MR_NTFS_RECOVER_RANGES_TRUNCATED
            } else {
                0
            },
            bytes: r.copy.bytes,
            untrusted_bytes: r.copy.untrusted_bytes(),
            possibly_overwritten_bytes: r.copy.count(RangeState::PossiblyOverwritten),
            filled_bytes: r.copy.filled_bytes(),
            recoverability: recoverability_code(r.assessment.as_ref().map(|a| a.recoverability)),
            suspect_count: u32::try_from(r.suspect.len()).unwrap_or(u32::MAX),
            sequence: u32::from(r.sequence),
            reserved: 0,
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

fn suspect_of(x: &SuspectRange) -> MrNtfsSuspectRange {
    let (state, owner) = match x.state {
        ClusterState::Free => (MR_NTFS_CLUSTER_FREE, MR_NTFS_NO_OWNER),
        ClusterState::Owned { owner } => (MR_NTFS_CLUSTER_OWNED, owner),
        ClusterState::Allocated => (MR_NTFS_CLUSTER_ALLOCATED, MR_NTFS_NO_OWNER),
        ClusterState::Unknown => (MR_NTFS_CLUSTER_UNKNOWN, MR_NTFS_NO_OWNER),
        ClusterState::SharedWithDeleted { record } => (MR_NTFS_CLUSTER_SHARED_WITH_DELETED, record),
    };
    MrNtfsSuspectRange {
        struct_size: std::mem::size_of::<MrNtfsSuspectRange>() as u32,
        state,
        offset: x.offset,
        length: x.length,
        lcn: x.lcn,
        clusters: x.clusters,
        owner,
    }
}

/// Tramo `index` de bytes posiblemente sobrescritos de una recuperación terminada.
///
/// # Safety
/// `out` debe apuntar a un `MrNtfsSuspectRange` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_recover_suspect(
    task: u64,
    index: u32,
    out: *mut MrNtfsSuspectRange,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_ntfs_suspect_range") } {
            return c;
        }
        match with_recover(task, |r| r.suspect.get(index as usize).map(suspect_of)) {
            // SAFETY: `out` comprobado.
            Ok(Some(v)) => unsafe { write_out(out, v) },
            Ok(None) => index_error("tramo", u64::from(index)),
            Err(c) => c,
        }
    })
}

/// Texto de una recuperación terminada: con `index` = `u32::MAX`, el resumen de la
/// recuperabilidad (vacío si el registro no se evaluó); si no, el dueño del tramo sospechoso
/// `index` (ruta o nombre del fichero vivo, o descripción del estado).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_ntfs_recover_text(
    task: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_recover(task, |r| {
            if index == u32::MAX {
                return Some(r.summary.clone().unwrap_or_default());
            }
            r.suspect.get(index as usize).map(|x| match x.state {
                ClusterState::Owned { owner } => r
                    .owners
                    .get(&owner)
                    .map_or_else(|| format!("registro {owner}"), |o| o.display()),
                ClusterState::Allocated => "asignados en $Bitmap sin dueño conocido".into(),
                ClusterState::Unknown => "asignación desconocida".into(),
                ClusterState::SharedWithDeleted { record } => {
                    format!("reclamados también por el borrado {record}")
                }
                ClusterState::Free => "libres".into(),
            })
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("tramo", u64::from(index)),
            Err(c) => c,
        }
    })
}
