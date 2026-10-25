//! VMDK con descriptor separado y extensiones aprobadas por el usuario (incidencia 11; ampliación
//! compatible de la ABI 3). Contrato en `include/memrescue_core.h` y `docs/API_FFI.md`.
//!
//! - `mr_vmdk_extents_list` interpreta el descriptor **sin abrir ninguna extensión** y devuelve un
//!   handle que se consulta por índice (`mr_vmdk_extents_info`, `mr_vmdk_extent_get`,
//!   `mr_vmdk_extent_name`, `mr_vmdk_extent_issue`, `mr_vmdk_extents_describe`,
//!   `mr_vmdk_extents_issue`) y se libera con `mr_vmdk_extents_free`.
//! - `mr_source_open_vmdk` abre el disco con la lista explícita de ficheros aprobados
//!   (`mr_approved_file`: nombre declarado → ruta elegida por el usuario). El motor no abre
//!   ningún otro fichero.

use super::{
    api_fail, check_struct_size, fail, guard, index_error, insert, read_utf8, with_entry,
    write_out, write_str, Entry, MR_ERR_INVALID_ARGUMENT, MR_ERR_INVALID_HANDLE, MR_OK,
    MR_OPEN_TOLERANT,
};
use mr_api::{
    OpenOptions, VmdkExtentAccess, VmdkExtentPlan, VmdkExtentType, VmdkIssueKind,
    VmdkRequiredExtent,
};
use std::path::PathBuf;

/// `mr_vmdk_extents_info.flags`: el descriptor declara un disco padre (no se admite).
pub const MR_VMDK_HAS_PARENT: u32 = 1;
/// `mr_vmdk_extents_info.flags`: el disco se puede abrir una vez aprobados sus ficheros.
pub const MR_VMDK_OPENABLE: u32 = 2;
/// Clase de problema (`issue_kind`).
pub const MR_VMDK_ISSUE_NONE: u32 = 0;
pub const MR_VMDK_ISSUE_CORRUPT: u32 = 1;
pub const MR_VMDK_ISSUE_UNSUPPORTED: u32 = 2;
pub const MR_VMDK_ISSUE_PARENT: u32 = 3;
/// `mr_vmdk_extent.access`.
pub const MR_VMDK_ACCESS_RW: u32 = 1;
pub const MR_VMDK_ACCESS_RDONLY: u32 = 2;
pub const MR_VMDK_ACCESS_NOACCESS: u32 = 3;
/// `mr_vmdk_extent.kind`.
pub const MR_VMDK_EXTENT_SPARSE: u32 = 1;
pub const MR_VMDK_EXTENT_FLAT: u32 = 2;
pub const MR_VMDK_EXTENT_ZERO: u32 = 3;
pub const MR_VMDK_EXTENT_VMFS: u32 = 4;
pub const MR_VMDK_EXTENT_VMFSSPARSE: u32 = 5;
pub const MR_VMDK_EXTENT_VMFSRDM: u32 = 6;
pub const MR_VMDK_EXTENT_VMFSRAW: u32 = 7;
pub const MR_VMDK_EXTENT_SESPARSE: u32 = 8;
/// `mr_vmdk_extent.flags`: hay que aprobar un fichero para esta extensión.
pub const MR_VMDK_EXTENT_NEEDS_FILE: u32 = 1;
/// `mr_vmdk_extent.flags`: la línea declara un nombre de fichero.
pub const MR_VMDK_EXTENT_HAS_FILE_NAME: u32 = 2;
/// Máximo de ficheros aprobados en `mr_source_open_vmdk`.
pub const MAX_APPROVED_FILES: u32 = mr_api::MAX_VMDK_APPROVED_FILES as u32;

/// Resumen de un descriptor (32 bytes). `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVmdkExtentsInfo {
    pub struct_size: u32,
    pub extent_count: u32,
    /// Extensiones para las que hay que aprobar un fichero.
    pub required_count: u32,
    /// `MR_VMDK_HAS_PARENT` | `MR_VMDK_OPENABLE`.
    pub flags: u32,
    /// Tamaño virtual en bytes (suma de las extensiones).
    pub virtual_size: u64,
    /// Clase del primer problema (`MR_VMDK_ISSUE_*`).
    pub issue_kind: u32,
    pub reserved: u32,
}

/// Una extensión (48 bytes). `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MrVmdkExtent {
    pub struct_size: u32,
    pub access: u32,
    pub kind: u32,
    /// `MR_VMDK_EXTENT_NEEDS_FILE` | `MR_VMDK_EXTENT_HAS_FILE_NAME`.
    pub flags: u32,
    pub sectors: u64,
    /// Desplazamiento en sectores dentro del fichero (FLAT).
    pub offset: u64,
    /// Primer sector virtual.
    pub start_sector: u64,
    pub issue_kind: u32,
    pub reserved: u32,
}

/// Fichero aprobado por el usuario para `mr_source_open_vmdk` (40 bytes en 64 bits).
/// `struct_size` lo escribe el llamador.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MrApprovedFile {
    pub struct_size: u32,
    /// Reservado: debe ser 0.
    pub reserved: u32,
    /// Nombre declarado en el descriptor (UTF-8, coincidencia exacta).
    pub name: *const u8,
    pub name_len: usize,
    /// Fichero elegido por el usuario (UTF-8).
    pub path: *const u8,
    pub path_len: usize,
}

fn issue_code(k: Option<VmdkIssueKind>) -> u32 {
    match k {
        None => MR_VMDK_ISSUE_NONE,
        Some(VmdkIssueKind::Corrupt) => MR_VMDK_ISSUE_CORRUPT,
        Some(VmdkIssueKind::Unsupported) => MR_VMDK_ISSUE_UNSUPPORTED,
        Some(VmdkIssueKind::Parent) => MR_VMDK_ISSUE_PARENT,
    }
}

fn access_code(a: VmdkExtentAccess) -> u32 {
    match a {
        VmdkExtentAccess::ReadWrite => MR_VMDK_ACCESS_RW,
        VmdkExtentAccess::ReadOnly => MR_VMDK_ACCESS_RDONLY,
        VmdkExtentAccess::NoAccess => MR_VMDK_ACCESS_NOACCESS,
    }
}

fn kind_code(k: VmdkExtentType) -> u32 {
    match k {
        VmdkExtentType::Sparse => MR_VMDK_EXTENT_SPARSE,
        VmdkExtentType::Flat => MR_VMDK_EXTENT_FLAT,
        VmdkExtentType::Zero => MR_VMDK_EXTENT_ZERO,
        VmdkExtentType::Vmfs => MR_VMDK_EXTENT_VMFS,
        VmdkExtentType::VmfsSparse => MR_VMDK_EXTENT_VMFSSPARSE,
        VmdkExtentType::VmfsRdm => MR_VMDK_EXTENT_VMFSRDM,
        VmdkExtentType::VmfsRaw => MR_VMDK_EXTENT_VMFSRAW,
        VmdkExtentType::SeSparse => MR_VMDK_EXTENT_SESPARSE,
    }
}

/// Ejecuta `f` sobre un plan de extensiones registrado.
fn with_plan<R>(h: u64, f: impl FnOnce(&VmdkExtentPlan) -> R) -> Result<R, i32> {
    match with_entry(h, |e| match e {
        Entry::VmdkExtents(p) => Some(f(p)),
        _ => None,
    }) {
        Some(Some(r)) => Ok(r),
        _ => Err(fail(
            MR_ERR_INVALID_HANDLE,
            format!("handle de lista de extensiones VMDK no válido: {h}"),
        )),
    }
}

/// Ejecuta `f` sobre la extensión `index` de un plan.
fn with_extent<R>(h: u64, index: u32, f: impl FnOnce(&VmdkRequiredExtent) -> R) -> Result<R, i32> {
    match with_plan(h, |p| p.extents.get(index as usize).map(f)) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(index_error("extensión", u64::from(index))),
        Err(c) => Err(c),
    }
}

/// Interpreta un descriptor VMDK separado sin abrir ninguna extensión.
///
/// # Safety
/// `path` UTF-8 de `path_len` bytes; `out_list` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extents_list(
    path: *const u8,
    path_len: usize,
    out_list: *mut u64,
) -> i32 {
    guard(|| {
        if out_list.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        match mr_api::vmdk_required_extents(p) {
            // SAFETY: `out_list` no es nulo.
            Ok(plan) => unsafe { write_out(out_list, insert(Entry::VmdkExtents(Box::new(plan)))) },
            Err(e) => api_fail(&e),
        }
    })
}

/// Resumen del descriptor.
///
/// # Safety
/// `out` debe apuntar a un `MrVmdkExtentsInfo` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extents_info(list_h: u64, out: *mut MrVmdkExtentsInfo) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_vmdk_extents_info") } {
            return c;
        }
        match with_plan(list_h, |p| MrVmdkExtentsInfo {
            struct_size: std::mem::size_of::<MrVmdkExtentsInfo>() as u32,
            extent_count: p.extents.len() as u32,
            required_count: p.required_names().count() as u32,
            flags: if p.parent.is_some() {
                MR_VMDK_HAS_PARENT
            } else {
                0
            } | if p.is_openable() { MR_VMDK_OPENABLE } else { 0 },
            virtual_size: p.size,
            issue_kind: issue_code(p.first_issue().map(|i| i.kind)),
            reserved: 0,
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Descripción legible del descriptor (tipo y extensiones por tipo).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extents_describe(
    list_h: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| match with_plan(list_h, |p| p.summary()) {
        // SAFETY: contrato del llamador.
        Ok(s) => unsafe { write_str(&s, buf, cap, out_len) },
        Err(c) => c,
    })
}

/// Texto del primer problema que impide abrir el disco (cadena vacía si no hay ninguno).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extents_issue(
    list_h: u64,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_plan(list_h, |p| {
            p.first_issue().map(|i| i.text.clone()).unwrap_or_default()
        }) {
            // SAFETY: contrato del llamador.
            Ok(s) => unsafe { write_str(&s, buf, cap, out_len) },
            Err(c) => c,
        }
    })
}

/// Extensión número `index` (orden del descriptor).
///
/// # Safety
/// `out` debe apuntar a un `MrVmdkExtent` escribible con `struct_size` ya escrito.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extent_get(
    list_h: u64,
    index: u32,
    out: *mut MrVmdkExtent,
) -> i32 {
    guard(|| {
        // SAFETY: contrato del llamador.
        if let Err(c) = unsafe { check_struct_size(out, "mr_vmdk_extent") } {
            return c;
        }
        match with_extent(list_h, index, |e| MrVmdkExtent {
            struct_size: std::mem::size_of::<MrVmdkExtent>() as u32,
            access: access_code(e.access),
            kind: kind_code(e.kind),
            flags: if e.needs_file {
                MR_VMDK_EXTENT_NEEDS_FILE
            } else {
                0
            } | if e.file_name.is_some() {
                MR_VMDK_EXTENT_HAS_FILE_NAME
            } else {
                0
            },
            sectors: e.sectors,
            offset: e.offset,
            start_sector: e.start_sector,
            issue_kind: issue_code(e.issue.as_ref().map(|i| i.kind)),
            reserved: 0,
        }) {
            // SAFETY: `out` comprobado.
            Ok(v) => unsafe { write_out(out, v) },
            Err(c) => c,
        }
    })
}

/// Nombre de fichero **exactamente** como lo declara el descriptor (cadena vacía en ZERO). Es el
/// nombre que hay que pasar en `mr_approved_file.name`.
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extent_name(
    list_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(
        || match with_extent(list_h, index, |e| e.file_name.clone().unwrap_or_default()) {
            // SAFETY: contrato del llamador.
            Ok(s) => unsafe { write_str(&s, buf, cap, out_len) },
            Err(c) => c,
        },
    )
}

/// Problema de la extensión (cadena vacía si no hay ninguno).
///
/// # Safety
/// Ver contrato de cadenas de salida.
#[no_mangle]
pub unsafe extern "C" fn mr_vmdk_extent_issue(
    list_h: u64,
    index: u32,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        match with_extent(list_h, index, |e| {
            e.issue.as_ref().map(|i| i.text.clone()).unwrap_or_default()
        }) {
            // SAFETY: contrato del llamador.
            Ok(s) => unsafe { write_str(&s, buf, cap, out_len) },
            Err(c) => c,
        }
    })
}

/// Libera una lista de extensiones.
#[no_mangle]
pub extern "C" fn mr_vmdk_extents_free(list_h: u64) -> i32 {
    guard(|| {
        let mut map = super::registry().lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&list_h) {
            Some(Entry::VmdkExtents(_)) => {
                map.remove(&list_h);
                MR_OK
            }
            _ => fail(
                MR_ERR_INVALID_HANDLE,
                format!("handle de lista de extensiones VMDK no válido: {list_h}"),
            ),
        }
    })
}

/// Abre un VMDK con descriptor separado con **solo** los ficheros aprobados (`files`, `count`
/// entradas; puede ser nulo con `count` = 0). `flags`: 0 o `MR_OPEN_TOLERANT`.
///
/// # Safety
/// `path` UTF-8 de `path_len` bytes; `files` apunta a `count` estructuras legibles cuyas cadenas
/// cumplen el contrato de cadenas de entrada; `out_source` puntero válido.
#[no_mangle]
pub unsafe extern "C" fn mr_source_open_vmdk(
    path: *const u8,
    path_len: usize,
    sector_size: u32,
    flags: u32,
    files: *const MrApprovedFile,
    count: u32,
    out_source: *mut u64,
) -> i32 {
    guard(|| {
        if out_source.is_null() {
            return fail(MR_ERR_INVALID_ARGUMENT, "puntero de salida nulo");
        }
        if flags & !MR_OPEN_TOLERANT != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "opciones de apertura no válidas (solo MR_OPEN_TOLERANT)",
            );
        }
        if count > MAX_APPROVED_FILES {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                format!("demasiados ficheros aprobados (máximo {MAX_APPROVED_FILES})"),
            );
        }
        if files.is_null() && count != 0 {
            return fail(
                MR_ERR_INVALID_ARGUMENT,
                "puntero nulo con número de ficheros distinto de cero",
            );
        }
        // SAFETY: contrato del llamador.
        let p = match unsafe { read_utf8(path, path_len) } {
            Ok(p) => p,
            Err(c) => return c,
        };
        let mut list: Vec<(String, PathBuf)> = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // Se lee primero solo `struct_size`, para no leer más allá de una estructura menor.
            // SAFETY: `files` tiene `count` elementos; cada uno empieza por `struct_size`.
            let item = unsafe { files.add(i) };
            // SAFETY: ídem.
            let size = unsafe { item.cast::<u32>().read_unaligned() };
            let expected = std::mem::size_of::<MrApprovedFile>();
            if size as usize != expected {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    format!(
                        "tamaño de mr_approved_file no válido: {size} (se esperaba {expected})"
                    ),
                );
            }
            // SAFETY: el tamaño declarado coincide con el de la estructura.
            let a = unsafe { item.read_unaligned() };
            if a.reserved != 0 {
                return fail(
                    MR_ERR_INVALID_ARGUMENT,
                    "mr_approved_file.reserved debe ser 0",
                );
            }
            // SAFETY: contrato del llamador.
            let name = match unsafe { read_utf8(a.name, a.name_len) } {
                Ok(n) => n.to_string(),
                Err(c) => return c,
            };
            // SAFETY: contrato del llamador.
            let file = match unsafe { read_utf8(a.path, a.path_len) } {
                Ok(f) => PathBuf::from(f),
                Err(c) => return c,
            };
            list.push((name, file));
        }
        let opts = OpenOptions {
            sector_size,
            force_raw: false,
            tolerant: flags & MR_OPEN_TOLERANT != 0,
        };
        match mr_api::open_vmdk_with_extents(p, &list, opts) {
            // SAFETY: `out_source` no es nulo.
            Ok(src) => unsafe { write_out(out_source, insert(Entry::Source(src))) },
            Err(e) => api_fail(&e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mr_source_close, mr_source_describe, mr_source_info, mr_source_read, MrImageInfo,
        MR_ERR_BUFFER_TOO_SMALL, MR_ERR_EXTERNAL_REFERENCE, MR_ERR_OUT_OF_RANGE,
        MR_ERR_PATH_REJECTED, MR_ERR_UNSUPPORTED_FEATURE, MR_OPEN_FORCE_RAW,
    };

    fn approved(name: &str, path: &str) -> MrApprovedFile {
        MrApprovedFile {
            struct_size: std::mem::size_of::<MrApprovedFile>() as u32,
            reserved: 0,
            name: name.as_ptr(),
            name_len: name.len(),
            path: path.as_ptr(),
            path_len: path.len(),
        }
    }

    fn open(p: &str, flags: u32, files: &[MrApprovedFile]) -> Result<u64, i32> {
        let mut h = 0u64;
        let ptr = if files.is_empty() {
            std::ptr::null()
        } else {
            files.as_ptr()
        };
        let rc = unsafe {
            mr_source_open_vmdk(
                p.as_ptr(),
                p.len(),
                0,
                flags,
                ptr,
                files.len() as u32,
                &mut h,
            )
        };
        if rc == MR_OK {
            Ok(h)
        } else {
            Err(rc)
        }
    }

    fn text(f: impl Fn(*mut u8, usize, *mut usize) -> i32) -> String {
        let mut buf = vec![0u8; 1024];
        let mut n = 0usize;
        assert_eq!(f(buf.as_mut_ptr(), buf.len(), &mut n), MR_OK);
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    #[test]
    fn layouts() {
        assert_eq!(std::mem::size_of::<MrVmdkExtentsInfo>(), 32);
        assert_eq!(std::mem::size_of::<MrVmdkExtent>(), 48);
        assert_eq!(std::mem::offset_of!(MrVmdkExtent, sectors), 16);
        assert_eq!(std::mem::offset_of!(MrVmdkExtent, issue_kind), 40);
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<MrApprovedFile>(), 40);
            assert_eq!(std::mem::offset_of!(MrApprovedFile, path), 24);
        }
    }

    #[test]
    fn list_and_open_with_explicit_approval() {
        let dir = tempfile::tempdir().unwrap();
        let flat: Vec<u8> = (0..16 * 512).map(|i| (i / 512) as u8 + 1).collect();
        std::fs::write(dir.path().join("x-f001.vmdk"), &flat[..8 * 512]).unwrap();
        std::fs::write(dir.path().join("x-f002.vmdk"), &flat[8 * 512..]).unwrap();
        let desc = dir.path().join("x.vmdk");
        std::fs::write(
            &desc,
            "# Disk DescriptorFile\nCID=1\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 8 FLAT \"x-f001.vmdk\" 0\nRW 8 FLAT \"x-f002.vmdk\" 0\nRW 4 ZERO\nNOACCESS 4 FLAT \"..\\\\nada\" 0\n",
        )
        .unwrap();
        let d = desc.to_str().unwrap();

        let mut list = 0u64;
        assert_eq!(
            unsafe { mr_vmdk_extents_list(d.as_ptr(), d.len(), &mut list) },
            MR_OK
        );
        let mut info = MrVmdkExtentsInfo {
            struct_size: 32,
            ..Default::default()
        };
        assert_eq!(unsafe { mr_vmdk_extents_info(list, &mut info) }, MR_OK);
        assert_eq!(
            (info.extent_count, info.required_count, info.virtual_size),
            (4, 2, 24 * 512)
        );
        assert_eq!(info.flags, MR_VMDK_OPENABLE);
        assert_eq!(info.issue_kind, MR_VMDK_ISSUE_NONE);
        let mut bad = MrVmdkExtentsInfo {
            struct_size: 31,
            ..Default::default()
        };
        assert_eq!(
            unsafe { mr_vmdk_extents_info(list, &mut bad) },
            MR_ERR_INVALID_ARGUMENT
        );
        let mut e = MrVmdkExtent {
            struct_size: 48,
            ..Default::default()
        };
        assert_eq!(unsafe { mr_vmdk_extent_get(list, 1, &mut e) }, MR_OK);
        assert_eq!(
            (e.kind, e.access, e.sectors, e.start_sector, e.flags),
            (
                MR_VMDK_EXTENT_FLAT,
                MR_VMDK_ACCESS_RW,
                8,
                8,
                MR_VMDK_EXTENT_NEEDS_FILE | MR_VMDK_EXTENT_HAS_FILE_NAME
            )
        );
        assert_eq!(unsafe { mr_vmdk_extent_get(list, 3, &mut e) }, MR_OK);
        assert_eq!(
            (e.access, e.flags),
            (MR_VMDK_ACCESS_NOACCESS, MR_VMDK_EXTENT_HAS_FILE_NAME)
        );
        assert_eq!(
            unsafe { mr_vmdk_extent_get(list, 4, &mut e) },
            MR_ERR_OUT_OF_RANGE
        );
        assert_eq!(
            text(|b, c, n| unsafe { mr_vmdk_extent_name(list, 0, b, c, n) }),
            "x-f001.vmdk"
        );
        assert_eq!(
            text(|b, c, n| unsafe { mr_vmdk_extent_name(list, 2, b, c, n) }),
            ""
        );
        assert_eq!(
            text(|b, c, n| unsafe { mr_vmdk_extent_issue(list, 0, b, c, n) }),
            ""
        );
        assert_eq!(
            text(|b, c, n| unsafe { mr_vmdk_extents_issue(list, b, c, n) }),
            ""
        );
        assert!(
            text(|b, c, n| unsafe { mr_vmdk_extents_describe(list, b, c, n) })
                .contains("twoGbMaxExtentFlat")
        );
        let mut n = 0usize;
        assert_eq!(
            unsafe { mr_vmdk_extent_name(list, 0, std::ptr::null_mut(), 0, &mut n) },
            MR_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(n, 11);
        assert_eq!(mr_vmdk_extents_free(list), MR_OK);
        assert_eq!(mr_vmdk_extents_free(list), MR_ERR_INVALID_HANDLE);
        assert_eq!(
            unsafe { mr_vmdk_extents_info(list, &mut info) },
            MR_ERR_INVALID_HANDLE
        );

        // Sin aprobar: referencia externa; con una sola: también.
        assert_eq!(open(d, 0, &[]), Err(MR_ERR_EXTERNAL_REFERENCE));
        let p1 = dir.path().join("x-f001.vmdk");
        let p2 = dir.path().join("x-f002.vmdk");
        let (p1, p2) = (p1.to_str().unwrap(), p2.to_str().unwrap());
        assert_eq!(
            open(d, 0, &[approved("x-f001.vmdk", p1)]),
            Err(MR_ERR_EXTERNAL_REFERENCE)
        );
        // Opciones no válidas, estructura de otro tamaño, nombre repetido, dispositivo.
        let both = [approved("x-f001.vmdk", p1), approved("x-f002.vmdk", p2)];
        assert_eq!(
            open(d, MR_OPEN_FORCE_RAW, &both),
            Err(MR_ERR_INVALID_ARGUMENT)
        );
        let mut wrong = both;
        wrong[1].struct_size = 32;
        assert_eq!(open(d, 0, &wrong), Err(MR_ERR_INVALID_ARGUMENT));
        let mut wrong = both;
        wrong[1].reserved = 1;
        assert_eq!(open(d, 0, &wrong), Err(MR_ERR_INVALID_ARGUMENT));
        let dup = [approved("x-f001.vmdk", p1), approved("x-f001.vmdk", p2)];
        assert_eq!(open(d, 0, &dup), Err(MR_ERR_INVALID_ARGUMENT));
        let device = r"\\.\PhysicalDrive0";
        let dev = [approved("x-f001.vmdk", device), approved("x-f002.vmdk", p2)];
        assert_eq!(open(d, 0, &dev), Err(MR_ERR_PATH_REJECTED));
        let mut h = 0u64;
        assert_eq!(
            unsafe { mr_source_open_vmdk(d.as_ptr(), d.len(), 0, 0, std::ptr::null(), 2, &mut h) },
            MR_ERR_INVALID_ARGUMENT
        );

        let h = open(d, MR_OPEN_TOLERANT, &both).unwrap();
        let mut ii = MrImageInfo::default();
        assert_eq!(unsafe { mr_source_info(h, &mut ii) }, MR_OK);
        assert_eq!(ii.size_bytes, 24 * 512);
        assert!(text(|b, c, n| unsafe { mr_source_describe(h, b, c, n) }).contains("FLAT"));
        let mut buf = vec![0u8; 16 * 512];
        let mut got = 0usize;
        assert_eq!(
            unsafe { mr_source_read(h, 0, 16, buf.as_mut_ptr(), buf.len(), &mut got) },
            MR_OK
        );
        assert_eq!(buf, flat);
        // NOACCESS: la lectura estricta falla.
        let mut one = vec![0u8; 512];
        assert_ne!(
            unsafe { mr_source_read(h, 20, 1, one.as_mut_ptr(), one.len(), &mut got) },
            MR_OK
        );
        assert_eq!(mr_source_close(h), MR_OK);

        // Nombre con ruta en el descriptor: no se admite aunque se apruebe.
        std::fs::write(
            &desc,
            "# Disk DescriptorFile\ncreateType=\"monolithicFlat\"\nRW 8 FLAT \"C:\\\\x-f001.vmdk\" 0\n",
        )
        .unwrap();
        assert_eq!(
            open(d, 0, &[approved("C:\\x-f001.vmdk", p1)]),
            Err(MR_ERR_UNSUPPORTED_FEATURE)
        );
        let mut list = 0u64;
        assert_eq!(
            unsafe { mr_vmdk_extents_list(d.as_ptr(), d.len(), &mut list) },
            MR_OK
        );
        let mut info = MrVmdkExtentsInfo {
            struct_size: 32,
            ..Default::default()
        };
        assert_eq!(unsafe { mr_vmdk_extents_info(list, &mut info) }, MR_OK);
        assert_eq!(
            (info.flags, info.issue_kind),
            (0, MR_VMDK_ISSUE_UNSUPPORTED)
        );
        assert!(!text(|b, c, n| unsafe { mr_vmdk_extent_issue(list, 0, b, c, n) }).is_empty());
        assert_eq!(mr_vmdk_extents_free(list), MR_OK);
        assert_eq!(
            std::fs::read(dir.path().join("x-f001.vmdk")).unwrap(),
            &flat[..8 * 512]
        );
    }
}
