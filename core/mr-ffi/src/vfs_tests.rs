//! Pruebas de la frontera C del árbol virtual (#25) sobre el volumen sintético de
//! `mr_fs_ntfs::testvol::tree_volume`.

use super::*;
use mr_fs_ntfs::testvol::tree_volume;

#[test]
fn vfs_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrVfsBuildOptions>(), 32);
    assert_eq!(std::mem::size_of::<MrVfsInfo>(), 176);
    assert_eq!(std::mem::size_of::<MrVfsNode>(), 128);
    assert_eq!(std::mem::size_of::<MrVfsFilter>(), 56);
    assert_eq!(std::mem::offset_of!(MrVfsInfo, live), 48);
    assert_eq!(std::mem::offset_of!(MrVfsInfo, integro), 96);
    assert_eq!(std::mem::offset_of!(MrVfsInfo, memory_bytes), 152);
    assert_eq!(std::mem::offset_of!(MrVfsNode, size), 48);
    assert_eq!(std::mem::offset_of!(MrVfsNode, record), 96);
    assert_eq!(std::mem::offset_of!(MrVfsNode, sequence), 112);
    assert_eq!(std::mem::offset_of!(MrVfsFilter, size_min), 16);
    assert_eq!(std::mem::offset_of!(MrVfsFilter, time_field), 48);
    // Los indicadores de nodo de la cabecera son los de `mr_vfs::flags`.
    assert_eq!(mr_api::mr_vfs::flags::ALL, 0x3FFF);
}

fn last_error() -> String {
    let mut buf = vec![0u8; 4096];
    let mut n = 0usize;
    unsafe { mr_last_error(buf.as_mut_ptr(), buf.len(), &mut n) };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn string(f: impl Fn(*mut u8, usize, *mut usize) -> i32) -> Result<String, i32> {
    let mut n = 0usize;
    let rc = f(std::ptr::null_mut(), 0, &mut n);
    if rc != MR_ERR_BUFFER_TOO_SMALL {
        return Err(rc);
    }
    let mut buf = vec![0u8; n + 1];
    let rc = f(buf.as_mut_ptr(), buf.len(), &mut n);
    if rc != MR_OK {
        return Err(rc);
    }
    Ok(String::from_utf8(buf[..n].to_vec()).unwrap())
}

fn wait(task: u64) -> MrTaskStatus {
    let mut st = MrTaskStatus::default();
    for _ in 0..4000 {
        assert_eq!(unsafe { mr_task_status_get(task, &mut st) }, MR_OK);
        if st.state != 1 {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("la tarea no termina");
}

/// Volumen NTFS sintético abierto y su árbol virtual construido.
fn setup() -> (tempfile::TempDir, u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("vfs.img");
    std::fs::write(&p, tree_volume(true).image).unwrap();
    let ps = p.to_str().unwrap();
    let mut src = 0u64;
    assert_eq!(
        unsafe { mr_source_open(ps.as_ptr(), ps.len(), 0, 0, &mut src) },
        MR_OK
    );
    let mut vol = 0u64;
    assert_eq!(
        unsafe { mr_ntfs_open(src, &mut vol) },
        MR_OK,
        "{}",
        last_error()
    );
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_vfs_build_start(vol, std::ptr::null(), &mut task) },
        MR_OK,
        "{}",
        last_error()
    );
    let st = wait(task);
    assert_eq!((st.state, st.done, st.total), (2, 3000, 3000));
    let mut vfs = 0u64;
    assert_eq!(unsafe { mr_vfs_build_result(task, &mut vfs) }, MR_OK);
    assert_eq!(mr_task_free(task), MR_OK);
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
    (dir, vfs, task)
}

fn node(vfs: u64, id: u32) -> MrVfsNode {
    let mut n = MrVfsNode {
        struct_size: std::mem::size_of::<MrVfsNode>() as u32,
        ..MrVfsNode::default()
    };
    assert_eq!(
        unsafe { mr_vfs_node(vfs, id, &mut n) },
        MR_OK,
        "{}",
        last_error()
    );
    n
}

fn find(vfs: u64, path: &str) -> Result<u32, i32> {
    let mut id = 0u32;
    match unsafe { mr_vfs_find(vfs, path.as_ptr(), path.len(), &mut id) } {
        MR_OK => Ok(id),
        rc => Err(rc),
    }
}

fn query(vfs: u64, start: u32, f: &MrVfsFilter, masks: &str) -> Result<Vec<u32>, i32> {
    let mut q = 0u64;
    let rc = unsafe { mr_vfs_query(vfs, start, f, masks.as_ptr(), masks.len(), &mut q) };
    if rc != MR_OK {
        return Err(rc);
    }
    let mut n = 0u64;
    assert_eq!(unsafe { mr_vfs_query_count(q, &mut n) }, MR_OK);
    let mut out = Vec::new();
    for i in 0..n {
        let mut id = 0u32;
        assert_eq!(unsafe { mr_vfs_query_get(q, i, &mut id) }, MR_OK);
        out.push(id);
    }
    let mut id = 0u32;
    assert_eq!(
        unsafe { mr_vfs_query_get(q, n, &mut id) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(mr_vfs_query_free(q), MR_OK);
    assert_eq!(mr_vfs_query_free(q), MR_ERR_INVALID_HANDLE);
    Ok(out)
}

fn filter(flags: u32) -> MrVfsFilter {
    MrVfsFilter {
        struct_size: std::mem::size_of::<MrVfsFilter>() as u32,
        flags,
        size_max: u64::MAX,
        time_max: u64::MAX,
        ..MrVfsFilter::default()
    }
}

#[test]
fn build_navigate_and_query() {
    let (_d, vfs, task) = setup();
    // La tarea ya liberada no es válida; el árbol sobrevive al volumen y al origen.
    let mut st = MrTaskStatus::default();
    assert_eq!(
        unsafe { mr_task_status_get(task, &mut st) },
        MR_ERR_INVALID_HANDLE
    );
    let mut info = MrVfsInfo {
        struct_size: std::mem::size_of::<MrVfsInfo>() as u32,
        ..MrVfsInfo::default()
    };
    assert_eq!(unsafe { mr_vfs_info(vfs, &mut info) }, MR_OK);
    assert!(info.node_count > 15 && info.files > 8 && info.deleted >= 2);
    assert_eq!(info.flags, 0);
    assert!(info.integro >= 5 && info.orphan >= 3 && info.memory_bytes > 0);
    // Raíz y enumeración de hijos por índice.
    let root = node(vfs, 0);
    assert_eq!(
        (root.parent, root.kind, root.has_origin),
        (MR_VFS_NO_PARENT, MR_VFS_KIND_DIRECTORY, 1)
    );
    let mut names = Vec::new();
    for i in 0..root.child_count {
        let mut c = 0u32;
        assert_eq!(unsafe { mr_vfs_child(vfs, 0, i, &mut c) }, MR_OK);
        names.push(string(|b, cap, n| unsafe { mr_vfs_node_name(vfs, c, b, cap, n) }).unwrap());
    }
    assert!(names.contains(&"[Huérfanos]".to_string()), "{names:?}");
    let mut c = 0u32;
    assert_eq!(
        unsafe { mr_vfs_child(vfs, 0, root.child_count, &mut c) },
        MR_ERR_OUT_OF_RANGE
    );
    // Fichero vivo.
    let id = find(vfs, "/Docs/ñandú.bin").unwrap();
    let n = node(vfs, id);
    assert_eq!(
        (n.kind, n.category, n.data_state, n.size, n.record),
        (
            MR_VFS_KIND_FILE,
            MR_VFS_CAT_LIVE,
            MR_VFS_DATA_INTEGRO,
            3000,
            20
        )
    );
    assert_eq!(n.name_reliability, MR_VFS_NAME_STRUCTURE);
    assert_eq!(
        string(|b, cap, l| unsafe { mr_vfs_node_path(vfs, id, b, cap, l) }).unwrap(),
        "/Docs/ñandú.bin"
    );
    // Nombre sin pérdida.
    let lossy = find(vfs, "/Docs/b%uD800c").unwrap();
    let mut len = 0usize;
    assert_eq!(
        unsafe { mr_vfs_node_name_utf16(vfs, lossy, std::ptr::null_mut(), 0, &mut len) },
        MR_ERR_BUFFER_TOO_SMALL
    );
    let mut units = vec![0u16; len];
    assert_eq!(
        unsafe { mr_vfs_node_name_utf16(vfs, lossy, units.as_mut_ptr(), units.len(), &mut len) },
        MR_OK
    );
    assert_eq!(units, [0x62, 0xD800, 0x63]);
    assert!(node(vfs, lossy).flags & mr_api::mr_vfs::flags::NAME_LOSSY != 0);
    // Borrado.
    let viejo = node(vfs, find(vfs, "/Docs/viejo.txt").unwrap());
    assert_eq!(
        (viejo.category, viejo.name_reliability),
        (MR_VFS_CAT_DELETED, MR_VFS_NAME_RECORD)
    );
    assert_eq!(find(vfs, "/no/existe"), Err(MR_ERR_NOT_FOUND));
    // Consultas.
    let all = query(vfs, 0, &filter(MR_VFS_QUERY_RECURSIVE), "").unwrap();
    assert_eq!(all.len() as u64, info.node_count - 1);
    let top = query(vfs, 0, &filter(0), "").unwrap();
    assert_eq!(top.len() as u32, root.child_count);
    let del = query(
        vfs,
        0,
        &filter(MR_VFS_QUERY_RECURSIVE | MR_VFS_QUERY_DELETED_ONLY | MR_VFS_QUERY_FILES_ONLY),
        "",
    )
    .unwrap();
    assert!(del.iter().any(|i| node(vfs, *i).record == 24));
    for i in &del {
        let n = node(vfs, *i);
        assert!(n.kind != MR_VFS_KIND_FILE || n.flags & mr_api::mr_vfs::flags::DELETED != 0);
    }
    let txt = query(vfs, 0, &filter(MR_VFS_QUERY_RECURSIVE), "!o*;*.txt").unwrap();
    let paths: Vec<String> = txt
        .iter()
        .map(|i| string(|b, c, l| unsafe { mr_vfs_node_path(vfs, *i, b, c, l) }).unwrap())
        .collect();
    assert!(paths.iter().any(|p| p == "/a.txt"));
    assert!(!paths.iter().any(|p| p.ends_with("/otro.txt")));
    let mut f = filter(MR_VFS_QUERY_RECURSIVE);
    f.data_states = 1 << MR_VFS_DATA_NO_DISPONIBLE;
    let bad = query(vfs, 0, &f, "").unwrap();
    assert!(bad
        .iter()
        .any(|i| node(vfs, *i).flags & mr_api::mr_vfs::flags::RECORD_DAMAGED != 0));
    // Errores.
    assert_eq!(
        query(vfs, 0, &filter(0x100), ""),
        Err(MR_ERR_INVALID_ARGUMENT)
    );
    assert_eq!(query(vfs, 0, &filter(0), "!"), Err(MR_ERR_INVALID_ARGUMENT));
    assert_eq!(query(vfs, 99_999, &filter(0), ""), Err(MR_ERR_OUT_OF_RANGE));
    let mut small = filter(0);
    small.struct_size = 8;
    assert_eq!(query(vfs, 0, &small, ""), Err(MR_ERR_INVALID_ARGUMENT));
    let mut n = MrVfsNode::default();
    assert_eq!(
        unsafe { mr_vfs_node(vfs, 0, &mut n) },
        MR_ERR_INVALID_ARGUMENT
    );
    n.struct_size = std::mem::size_of::<MrVfsNode>() as u32;
    assert_eq!(
        unsafe { mr_vfs_node(vfs, 99_999, &mut n) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(mr_vfs_free(vfs), MR_OK);
    assert_eq!(mr_vfs_free(vfs), MR_ERR_INVALID_HANDLE);
    assert_eq!(
        unsafe { mr_vfs_node(vfs, 0, &mut n) },
        MR_ERR_INVALID_HANDLE
    );
}

#[test]
fn build_options_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("vfs2.img");
    std::fs::write(&p, tree_volume(true).image).unwrap();
    let ps = p.to_str().unwrap();
    let mut src = 0u64;
    assert_eq!(
        unsafe { mr_source_open(ps.as_ptr(), ps.len(), 0, 0, &mut src) },
        MR_OK
    );
    let mut vol = 0u64;
    assert_eq!(unsafe { mr_ntfs_open(src, &mut vol) }, MR_OK);
    let mut opts = MrVfsBuildOptions {
        struct_size: std::mem::size_of::<MrVfsBuildOptions>() as u32,
        flags: MR_VFS_BUILD_NO_DELETED | MR_VFS_BUILD_NO_ORPHANS,
        ..MrVfsBuildOptions::default()
    };
    let mut task = 0u64;
    assert_eq!(unsafe { mr_vfs_build_start(vol, &opts, &mut task) }, MR_OK);
    wait(task);
    let mut vfs = 0u64;
    assert_eq!(unsafe { mr_vfs_build_result(task, &mut vfs) }, MR_OK);
    let mut info = MrVfsInfo {
        struct_size: std::mem::size_of::<MrVfsInfo>() as u32,
        ..MrVfsInfo::default()
    };
    assert_eq!(unsafe { mr_vfs_info(vfs, &mut info) }, MR_OK);
    assert_eq!((info.deleted, info.orphan), (0, 0));
    assert_eq!(mr_vfs_free(vfs), MR_OK);
    assert_eq!(mr_task_free(task), MR_OK);
    // Límite de nodos: la tarea falla con el error tipado.
    opts.flags = 0;
    opts.max_nodes = 3;
    assert_eq!(unsafe { mr_vfs_build_start(vol, &opts, &mut task) }, MR_OK);
    assert_eq!(wait(task).state, 3);
    assert_eq!(
        unsafe { mr_vfs_build_result(task, &mut vfs) },
        MR_ERR_TASK_FAILED
    );
    assert!(last_error().contains("límite"), "{}", last_error());
    assert_eq!(mr_task_free(task), MR_OK);
    // Opciones no válidas.
    opts.flags = 0x80;
    assert_eq!(
        unsafe { mr_vfs_build_start(vol, &opts, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    opts.flags = 0;
    opts.struct_size = 16;
    assert_eq!(
        unsafe { mr_vfs_build_start(vol, &opts, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_vfs_build_start(src, std::ptr::null(), &mut task) },
        MR_ERR_INVALID_HANDLE
    );
    // Cancelación: la tarea termina cancelada (o completada si fue más rápida).
    assert_eq!(
        unsafe { mr_vfs_build_start(vol, std::ptr::null(), &mut task) },
        MR_OK
    );
    assert_eq!(mr_task_cancel(task), MR_OK);
    let st = wait(task);
    assert!(st.state == 4 || st.state == 2, "{}", st.state);
    if st.state == 4 {
        assert_eq!(
            unsafe { mr_vfs_build_result(task, &mut vfs) },
            MR_ERR_CANCELLED
        );
    }
    assert_eq!(mr_task_free(task), MR_OK);
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}
