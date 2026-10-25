//! Árbol virtual (#25, ampliación compatible de la ABI 3): construcción como tarea desde un
//! volumen NTFS, el árbol como handle propio, nodos por identificador, hijos por índice, nombres
//! (forma visible UTF-8 y unidades UTF-16 originales), rutas, búsqueda por ruta y consultas con
//! filtro cuyo resultado es otro handle.
//!
//! Mismos patrones que el resto de la ABI: handles del registro, estructuras versionadas por
//! `struct_size` (entrada y salida) y consultas por índice.

use std::sync::Arc;

use mr_api::mr_vfs::filter::{Filter, NameRules, TimeField};
use mr_api::mr_vfs::{Category, DataState, Limits, NodeId, NtfsBuildOptions, Vfs};
use mr_api::task::Task;

use super::{
    api_fail, bad_task, check_struct_size, fail, guard, index_error, insert, read_utf8, settle,
    with_entry, write_out, write_str, Entry, Outcome, MR_ERR_BUFFER_TOO_SMALL,
    MR_ERR_INVALID_ARGUMENT, MR_ERR_INVALID_HANDLE, MR_ERR_NOT_FINISHED, MR_ERR_NOT_FOUND, MR_OK,
};

/// Tarea de construcción con su resultado.
pub(crate) struct BuildEntry {
    pub(crate) task: Arc<Task<Arc<Vfs>>>,
    outcome: Outcome<Arc<Vfs>>,
}

pub const MR_VFS_BUILD_NO_DELETED: u32 = 1;
pub const MR_VFS_BUILD_NO_ORPHANS: u32 = 2;
pub const MR_VFS_BUILD_NO_METADATA: u32 = 4;

pub const MR_VFS_INFO_PARTIAL: u32 = 1;

pub const MR_VFS_KIND_FILE: u32 = 1;
pub const MR_VFS_KIND_DIRECTORY: u32 = 2;
pub const MR_VFS_KIND_SYNTHETIC: u32 = 3;

pub const MR_VFS_CAT_LIVE: u32 = 1;
pub const MR_VFS_CAT_DELETED: u32 = 2;
pub const MR_VFS_CAT_ORPHAN: u32 = 3;
pub const MR_VFS_CAT_METADATA: u32 = 4;
pub const MR_VFS_CAT_FOUND: u32 = 5;
pub const MR_VFS_CAT_RAW: u32 = 6;

pub const MR_VFS_NAME_STRUCTURE: u32 = 1;
pub const MR_VFS_NAME_RESIDUAL: u32 = 2;
pub const MR_VFS_NAME_RECORD: u32 = 3;
pub const MR_VFS_NAME_SYNTHETIC: u32 = 4;

pub const MR_VFS_DATA_NONE: u32 = 0;
pub const MR_VFS_DATA_INTEGRO: u32 = 1;
pub const MR_VFS_DATA_INTEGRO_NO_VERIFICABLE: u32 = 2;
pub const MR_VFS_DATA_PARCIAL: u32 = 3;
pub const MR_VFS_DATA_SOBRESCRITO: u32 = 4;
pub const MR_VFS_DATA_SIN_DATOS: u32 = 5;
pub const MR_VFS_DATA_NO_DISPONIBLE: u32 = 6;
pub const MR_VFS_DATA_CIFRADO: u32 = 7;

/// Padre de la raíz en `MrVfsNode::parent`.
pub const MR_VFS_NO_PARENT: u32 = u32::MAX;
/// `MrVfsNode::name_space` sin espacio de nombres.
pub const MR_VFS_NO_NAMESPACE: u32 = 255;

pub const MR_VFS_QUERY_RECURSIVE: u32 = 1;
pub const MR_VFS_QUERY_DELETED_ONLY: u32 = 2;
pub const MR_VFS_QUERY_FILES_ONLY: u32 = 4;

pub const MR_VFS_TIME_MODIFIED: u32 = 0;
pub const MR_VFS_TIME_CREATED: u32 = 1;
pub const MR_VFS_TIME_CHANGED: u32 = 2;
pub const MR_VFS_TIME_ACCESSED: u32 = 3;

/// Opciones de construcción (32 bytes; los límites a 0 toman el valor por defecto).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVfsBuildOptions {
    pub struct_size: u32,
    /// `MR_VFS_BUILD_NO_DELETED | NO_ORPHANS | NO_METADATA`.
    pub flags: u32,
    pub max_nodes: u32,
    pub max_depth: u32,
    pub max_name_units: u64,
    pub reserved: u64,
}

/// Resumen del árbol (176 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVfsInfo {
    pub struct_size: u32,
    /// `MR_VFS_INFO_PARTIAL`.
    pub flags: u32,
    pub node_count: u64,
    pub files: u64,
    pub directories: u64,
    pub synthetic: u64,
    /// Entradas borradas (`MR_VFS_NODE_DELETED`), de cualquier categoría.
    pub deleted: u64,
    /// Nodos por categoría.
    pub live: u64,
    pub deleted_category: u64,
    pub orphan: u64,
    pub metadata: u64,
    pub found: u64,
    pub raw: u64,
    /// Ficheros por estado de los datos.
    pub integro: u64,
    pub integro_no_verificable: u64,
    pub parcial: u64,
    pub sobrescrito: u64,
    pub sin_datos: u64,
    pub no_disponible: u64,
    pub cifrado: u64,
    pub memory_bytes: u64,
    pub name_units: u64,
    pub note_count: u32,
    pub reserved: u32,
}

/// Nodo (128 bytes).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVfsNode {
    pub struct_size: u32,
    pub id: u32,
    /// `MR_VFS_NO_PARENT` en la raíz.
    pub parent: u32,
    pub child_count: u32,
    /// `MR_VFS_KIND_*`.
    pub kind: u32,
    /// `MR_VFS_CAT_*`.
    pub category: u32,
    /// `MR_VFS_NAME_*`.
    pub name_reliability: u32,
    /// `MR_VFS_DATA_*` (`MR_VFS_DATA_NONE` en directorios y carpetas sintéticas).
    pub data_state: u32,
    /// `MR_VFS_NODE_*`.
    pub flags: u32,
    /// 0 POSIX, 1 Win32, 2 DOS, 3 Win32 y DOS, `MR_VFS_NO_NAMESPACE`.
    pub name_space: u32,
    pub depth: u32,
    pub attributes: u32,
    pub size: u64,
    pub allocated_size: u64,
    /// FILETIME UTC (0 = desconocida).
    pub created: u64,
    pub modified: u64,
    pub changed: u64,
    pub accessed: u64,
    /// Referencia al origen (con `has_origin`): registro MFT y secuencia, y los del padre citado.
    pub record: u64,
    pub parent_record: u64,
    pub sequence: u32,
    pub parent_sequence: u32,
    pub has_origin: u32,
    pub reserved: u32,
}

/// Filtro de una consulta (56 bytes). Las máscaras de nombre van aparte (cadena UTF-8).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVfsFilter {
    pub struct_size: u32,
    /// `MR_VFS_QUERY_RECURSIVE | DELETED_ONLY | FILES_ONLY`.
    pub flags: u32,
    /// Máscara de `1 << MR_VFS_CAT_*` (0 = todas).
    pub categories: u32,
    /// Máscara de `1 << MR_VFS_DATA_*` (0 = todos).
    pub data_states: u32,
    /// Rango de tamaño incluido (0 y `UINT64_MAX` = sin límite).
    pub size_min: u64,
    pub size_max: u64,
    /// Rango de FILETIME incluido (0 y `UINT64_MAX` = sin límite).
    pub time_min: u64,
    pub time_max: u64,
    /// `MR_VFS_TIME_*`.
    pub time_field: u32,
    pub reserved: u32,
}

fn with_vfs<R>(h: u64, f: impl FnOnce(&Arc<Vfs>) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::Vfs(v) => Some(f(v)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de árbol virtual no válido: {h}"),
        )),
    }
}

fn vfs(h: u64) -> Result<Arc<Vfs>, i32> {
    with_vfs(h, Arc::clone)
}

fn node_error(id: u32) -> i32 {
    index_error("nodo", u64::from(id))
}

fn node_of(v: &Vfs, id: u32) -> Option<MrVfsNode> {
    let n = v.node(NodeId(id))?;
    let o = n.origin.unwrap_or_default();
    Some(MrVfsNode {
        struct_size: std::mem::size_of::<MrVfsNode>() as u32,
        id,
        parent: n.parent.map_or(MR_VFS_NO_PARENT, |p| p.0),
        child_count: n.child_count,
        kind: n.kind as u32,
        category: n.category as u32,
        name_reliability: n.name_reliability as u32,
        data_state: n.data.map_or(MR_VFS_DATA_NONE, |d| d as u32),
        flags: n.flags,
        name_space: n.namespace.map_or(MR_VFS_NO_NAMESPACE, u32::from),
        depth: u32::from(n.depth),
        attributes: n.attributes,
        size: n.size,
        allocated_size: n.allocated_size,
        created: n.times.created,
        modified: n.times.modified,
        changed: n.times.changed,
        accessed: n.times.accessed,
        record: o.record,
        parent_record: o.parent_record,
        sequence: u32::from(o.sequence),
        parent_sequence: u32::from(o.parent_sequence),
        has_origin: u32::from(n.origin.is_some()),
        reserved: 0,
    })
}

/// Lanza la construcción del árbol virtual del volumen NTFS (tarea: progreso en milésimas por
/// fase, 3000 en total; cancelable). `options` puede ser nulo (valores por defecto).
///
/// # Safety
/// `options` nulo o con `struct_size` escrito; `out_task` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_build_start(
    volume_h: u64,
    options: *const MrVfsBuildOptions,
    out_task: *mut u64,
) -> i32 {
    guard(|| {
        if out_task.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let o = if options.is_null() {
            MrVfsBuildOptions::default()
        } else {
            // SAFETY: no nulo; se lee primero solo `struct_size`.
            let size = unsafe { options.cast::<u32>().read_unaligned() };
            let expected = std::mem::size_of::<MrVfsBuildOptions>();
            if size as usize != expected {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    format!(
                        "tamaño de mr_vfs_build_options no válido: {size} (se esperaba {expected})"
                    ),
                );
            }
            // SAFETY: el tamaño declarado coincide con el de la estructura.
            unsafe { options.read_unaligned() }
        };
        let known = MR_VFS_BUILD_NO_DELETED | MR_VFS_BUILD_NO_ORPHANS | MR_VFS_BUILD_NO_METADATA;
        if o.flags & !known != 0 || o.max_depth > u32::from(u16::MAX) {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones del árbol virtual no válidas",
            );
        }
        let d = Limits::default();
        let opts = NtfsBuildOptions {
            limits: Limits {
                max_nodes: if o.max_nodes == 0 {
                    d.max_nodes
                } else {
                    o.max_nodes
                },
                max_name_units: if o.max_name_units == 0 {
                    d.max_name_units
                } else {
                    o.max_name_units
                },
                max_depth: if o.max_depth == 0 {
                    d.max_depth
                } else {
                    o.max_depth as u16
                },
            },
            deleted: o.flags & MR_VFS_BUILD_NO_DELETED == 0,
            orphans: o.flags & MR_VFS_BUILD_NO_ORPHANS == 0,
            metadata: o.flags & MR_VFS_BUILD_NO_METADATA == 0,
        };
        let vol = match super::ntfs::volume_of(volume_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        match mr_api::start_vfs_build_task(vol, opts) {
            // SAFETY: `out_task` no es nulo.
            Ok(task) => unsafe {
                write_out(
                    out_task,
                    insert(Entry::VfsBuild(Box::new(BuildEntry {
                        task,
                        outcome: None,
                    }))),
                )
            },
            Err(e) => api_fail(&e),
        }
    })
}

/// Árbol de una construcción terminada como handle propio (`mr_vfs_free`), que sobrevive a la
/// tarea. Cada llamada devuelve un handle nuevo del mismo árbol.
///
/// # Safety
/// `out_vfs` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_build_result(task: u64, out_vfs: *mut u64) -> i32 {
    guard(|| {
        if out_vfs.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        let r = with_entry(task, |e| match e {
            Entry::VfsBuild(e) => settle(&e.task, &mut e.outcome).map(|()| match &e.outcome {
                Some(Ok(v)) => Some(Arc::clone(v)),
                _ => None,
            }),
            _ => Err((
                MR_ERR_INVALID_HANDLE,
                format!("la tarea {task} no es una construcción de árbol virtual"),
            )),
        });
        match r {
            None => bad_task(task),
            Some(Err((c, m))) => fail(c, m),
            Some(Ok(None)) => fail(MR_ERR_NOT_FINISHED, "resultado no disponible"),
            // SAFETY: `out_vfs` no es nulo.
            Some(Ok(Some(v))) => unsafe { write_out(out_vfs, insert(Entry::Vfs(v))) },
        }
    })
}

/// # Safety
/// `out` debe apuntar a un `MrVfsInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_info(vfs_h: u64, out: *mut MrVfsInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_vfs_info") } {
            return c;
        }
        match with_vfs(vfs_h, |v| {
            let s = v.stats();
            MrVfsInfo {
                struct_size: std::mem::size_of::<MrVfsInfo>() as u32,
                flags: if v.is_partial() {
                    MR_VFS_INFO_PARTIAL
                } else {
                    0
                },
                node_count: s.nodes,
                files: s.files,
                directories: s.directories,
                synthetic: s.synthetic,
                deleted: s.deleted,
                live: s.category(Category::Live),
                deleted_category: s.category(Category::Deleted),
                orphan: s.category(Category::Orphan),
                metadata: s.category(Category::Metadata),
                found: s.category(Category::Found),
                raw: s.category(Category::Raw),
                integro: s.data(DataState::Integro),
                integro_no_verificable: s.data(DataState::IntegroNoVerificable),
                parcial: s.data(DataState::Parcial),
                sobrescrito: s.data(DataState::Sobrescrito),
                sin_datos: s.data(DataState::SinDatos),
                no_disponible: s.data(DataState::NoDisponible),
                cifrado: s.data(DataState::Cifrado),
                memory_bytes: v.memory_bytes(),
                name_units: s.name_units,
                note_count: u32::try_from(v.notes().len()).unwrap_or(u32::MAX),
                reserved: 0,
            }
        }) {
            // SAFETY: `out` comprobado.
            Ok(i) => unsafe { write_out(out, i) },
            Err(c) => c,
        }
    })
}

/// Nodo `node` (0 = la raíz).
///
/// # Safety
/// `out` debe apuntar a un `MrVfsNode` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_node(vfs_h: u64, node: u32, out: *mut MrVfsNode) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_vfs_node") } {
            return c;
        }
        match with_vfs(vfs_h, |v| node_of(v, node)) {
            // SAFETY: `out` comprobado.
            Ok(Some(n)) => unsafe { write_out(out, n) },
            Ok(None) => node_error(node),
            Err(c) => c,
        }
    })
}

/// Hijo `index` del nodo `node` en el orden determinista del árbol.
///
/// # Safety
/// `out_child` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_child(
    vfs_h: u64,
    node: u32,
    index: u32,
    out_child: *mut u32,
) -> i32 {
    guard(|| {
        if out_child.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_vfs(vfs_h, |v| {
            if !v.contains(NodeId(node)) {
                return Err(node_error(node));
            }
            v.child(NodeId(node), index)
                .ok_or_else(|| index_error("hijo", u64::from(index)))
        }) {
            // SAFETY: `out_child` no es nulo.
            Ok(Ok(c)) => unsafe { write_out(out_child, c.0) },
            Ok(Err(c)) | Err(c) => c,
        }
    })
}

/// Nombre visible (UTF-8; las unidades UTF-16 sueltas aparecen como U+FFFD).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_node_name(
    vfs_h: u64,
    node: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_vfs(vfs_h, |v| {
            v.contains(NodeId(node)).then(|| v.name(NodeId(node)))
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(n)) => unsafe { write_str(&n, buf, cap, out_len) },
            Ok(None) => node_error(node),
            Err(c) => c,
        }
    })
}

/// Nombre original en unidades UTF-16 (sin pérdida ni terminador). `*out_len` recibe el número
/// de unidades; si `cap` (en unidades) no basta, `MR_ERR_BUFFER_TOO_SMALL`.
///
/// # Safety
/// `buf` con `cap` unidades escribibles (o nulo con 0); `out_len` válido o nulo.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_node_name_utf16(
    vfs_h: u64,
    node: u32,
    buf: *mut u16,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        let units = match with_vfs(vfs_h, |v| {
            v.contains(NodeId(node))
                .then(|| v.name_units(NodeId(node)).to_vec())
        }) {
            Ok(Some(u)) => u,
            Ok(None) => return node_error(node),
            Err(c) => return c,
        };
        if !out_len.is_null() {
            // SAFETY: puntero válido por contrato.
            unsafe { out_len.write(units.len()) };
        }
        if units.is_empty() {
            return MR_OK;
        }
        if buf.is_null() || cap < units.len() {
            return fail(MR_ERR_BUFFER_TOO_SMALL, "búfer insuficiente");
        }
        // SAFETY: `buf` tiene al menos `units.len()` unidades escribibles.
        unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len()) };
        MR_OK
    })
}

/// Ruta visible del nodo (`/` en la raíz; componentes escapados, ver `docs/formatos/vfs.md`).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_node_path(
    vfs_h: u64,
    node: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_vfs(vfs_h, |v| {
            v.contains(NodeId(node)).then(|| v.path(NodeId(node)))
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(p)) => unsafe { write_str(&p, buf, cap, out_len) },
            Ok(None) => node_error(node),
            Err(c) => c,
        }
    })
}

/// Nodo de una ruta visible (`/a/b`, también con `\`). No existe → `MR_ERR_NOT_FOUND`.
///
/// # Safety
/// `path` debe apuntar a `path_len` bytes; `out_node` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_find(
    vfs_h: u64,
    path: *const u8,
    path_len: usize,
    out_node: *mut u32,
) -> i32 {
    guard(|| {
        if out_node.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        match with_vfs(vfs_h, |v| v.find(p)) {
            // SAFETY: `out_node` no es nulo.
            Ok(Some(n)) => unsafe { write_out(out_node, n.0) },
            Ok(None) => fail(
                MR_ERR_NOT_FOUND,
                format!("no existe {p} en el árbol virtual"),
            ),
            Err(c) => c,
        }
    })
}

/// Nota `index` de la construcción: `"<código>\n<nodo o vacío>\n<descripción>"`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_note_text(
    vfs_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_vfs(vfs_h, |v| {
            v.notes().get(index as usize).map(|n| {
                format!(
                    "{}\n{}\n{}",
                    n.code,
                    n.node.map(|i| i.0.to_string()).unwrap_or_default(),
                    n.text
                )
            })
        }) {
            // SAFETY: contrato del llamador.
            Ok(Some(t)) => unsafe { write_str(&t, buf, cap, out_len) },
            Ok(None) => index_error("nota", u64::from(index)),
            Err(c) => c,
        }
    })
}

fn filter_of(f: &MrVfsFilter, masks: &str) -> Result<Filter, i32> {
    let known = MR_VFS_QUERY_RECURSIVE | MR_VFS_QUERY_DELETED_ONLY | MR_VFS_QUERY_FILES_ONLY;
    if f.flags & !known != 0 {
        return Err(fail(
            MR_ERR_INVALID_ARGUMENT,
            "opciones de consulta no válidas",
        ));
    }
    let time_field = match f.time_field {
        MR_VFS_TIME_MODIFIED => TimeField::Modified,
        MR_VFS_TIME_CREATED => TimeField::Created,
        MR_VFS_TIME_CHANGED => TimeField::Changed,
        MR_VFS_TIME_ACCESSED => TimeField::Accessed,
        _ => return Err(fail(MR_ERR_INVALID_ARGUMENT, "campo de fecha no válido")),
    };
    let names = match NameRules::parse(masks) {
        Ok(n) => n,
        Err(e) => return Err(api_fail(&mr_api::ApiError::Vfs(e))),
    };
    Ok(Filter {
        categories: f.categories,
        data_states: f.data_states,
        deleted_only: f.flags & MR_VFS_QUERY_DELETED_ONLY != 0,
        files_only: f.flags & MR_VFS_QUERY_FILES_ONLY != 0,
        names,
        size_min: (f.size_min != 0).then_some(f.size_min),
        size_max: (f.size_max != u64::MAX).then_some(f.size_max),
        time_field,
        time_min: (f.time_min != 0).then_some(f.time_min),
        time_max: (f.time_max != u64::MAX).then_some(f.time_max),
    })
}

/// Consulta: nodos visibles bajo `start` con el filtro (y las reglas de nombre `masks`, UTF-8,
/// `regla;regla;!exclusión`), en orden determinista. Devuelve un handle de resultado
/// (`mr_vfs_query_count`, `mr_vfs_query_get`, `mr_vfs_query_free`).
///
/// # Safety
/// `filter` con `struct_size` escrito; `masks` con `masks_len` bytes (o nulo con 0);
/// `out_query` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_query(
    vfs_h: u64,
    start: u32,
    filter: *const MrVfsFilter,
    masks: *const u8,
    masks_len: usize,
    out_query: *mut u64,
) -> i32 {
    guard(|| {
        if out_query.is_null() || filter.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero nulo");
        }
        // SAFETY: no nulo; se lee primero solo `struct_size`.
        let size = unsafe { filter.cast::<u32>().read_unaligned() };
        let expected = std::mem::size_of::<MrVfsFilter>();
        if size as usize != expected {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("tamaño de mr_vfs_filter no válido: {size} (se esperaba {expected})"),
            );
        }
        // SAFETY: el tamaño declarado coincide con el de la estructura.
        let f = unsafe { filter.read_unaligned() };
        // SAFETY: contrato del llamador.
        let m = match unsafe { read_utf8(masks, masks_len) } {
            Ok(m) => m,
            Err(c) => return c,
        };
        let flt = match filter_of(&f, m) {
            Ok(x) => x,
            Err(c) => return c,
        };
        let v = match vfs(vfs_h) {
            Ok(v) => v,
            Err(c) => return c,
        };
        if !v.contains(NodeId(start)) {
            return node_error(start);
        }
        let recursive = f.flags & MR_VFS_QUERY_RECURSIVE != 0;
        match mr_api::vfs_query(&v, NodeId(start), recursive, &flt) {
            // SAFETY: `out_query` no es nulo.
            Ok(r) => unsafe { write_out(out_query, insert(Entry::VfsQuery(r))) },
            Err(e) => api_fail(&e),
        }
    })
}

fn with_query<R>(h: u64, f: impl FnOnce(&[NodeId]) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::VfsQuery(q) => Some(f(q)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de consulta no válido: {h}"),
        )),
    }
}

/// Número de nodos del resultado.
///
/// # Safety
/// `out_count` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_query_count(query: u64, out_count: *mut u64) -> i32 {
    guard(|| {
        if out_count.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_query(query, |q| q.len() as u64) {
            // SAFETY: no nulo.
            Ok(n) => unsafe { write_out(out_count, n) },
            Err(c) => c,
        }
    })
}

/// Nodo `index` del resultado.
///
/// # Safety
/// `out_node` válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vfs_query_get(query: u64, index: u64, out_node: *mut u32) -> i32 {
    guard(|| {
        if out_node.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        match with_query(query, |q| q.get(index as usize).copied()) {
            // SAFETY: no nulo.
            Ok(Some(n)) => unsafe { write_out(out_node, n.0) },
            Ok(None) => index_error("resultado", index),
            Err(c) => c,
        }
    })
}

#[no_mangle]
pub extern "C" fn mr_vfs_query_free(query: u64) -> i32 {
    super::ntfs::free_kind(query, "consulta", |e| matches!(e, Entry::VfsQuery(_)))
}

/// Libera el árbol. Los otros handles del mismo árbol siguen siendo válidos; los resultados de
/// consultas siguen existiendo, pero sus identificadores solo se interpretan con un handle del
/// árbol.
#[no_mangle]
pub extern "C" fn mr_vfs_free(vfs_h: u64) -> i32 {
    super::ntfs::free_kind(vfs_h, "árbol virtual", |e| matches!(e, Entry::Vfs(_)))
}
