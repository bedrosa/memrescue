//! Pruebas de la frontera C del árbol NTFS (#22) sobre el volumen sintético de
//! `mr_fs_ntfs::testvol::tree_volume`.

use super::*;
use mr_fs_ntfs::testvol::{tree_data, tree_volume};

#[test]
fn ntfs_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrNtfsVolumeInfo>(), 72);
    assert_eq!(std::mem::size_of::<MrNtfsEntry>(), 104);
    assert_eq!(std::mem::size_of::<MrNtfsListingInfo>(), 32);
    assert_eq!(std::mem::size_of::<MrNtfsTreeInfo>(), 40);
    assert_eq!(std::mem::size_of::<MrNtfsOrphan>(), 40);
    assert_eq!(std::mem::size_of::<MrNtfsCopyReport>(), 56);
    assert_eq!(std::mem::offset_of!(MrNtfsVolumeInfo, mft_lcn), 64);
    assert_eq!(std::mem::offset_of!(MrNtfsEntry, size), 32);
    assert_eq!(std::mem::offset_of!(MrNtfsEntry, created), 48);
    assert_eq!(std::mem::offset_of!(MrNtfsEntry, file_attributes), 80);
    assert_eq!(std::mem::offset_of!(MrNtfsEntry, found_sequence), 100);
    assert_eq!(std::mem::offset_of!(MrNtfsTreeInfo, directories), 32);
    assert_eq!(std::mem::offset_of!(MrNtfsCopyReport, flags), 48);
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

/// Imagen del volumen sintético, su origen y el volumen abierto.
fn setup() -> (tempfile::TempDir, u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("árbol.img");
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
    (dir, src, vol)
}

fn entry() -> MrNtfsEntry {
    MrNtfsEntry {
        struct_size: std::mem::size_of::<MrNtfsEntry>() as u32,
        ..MrNtfsEntry::default()
    }
}

/// Plazo para que una tarea termine. Las tareas de estas pruebas tardan milisegundos, pero las de
/// copia sincronizan el fichero en disco (`FlushFileBuffers`) y crean ficheros nuevos que el
/// antivirus analiza: con el equipo cargado (compilaciones o pruebas en paralelo) eso no tiene
/// cota. El plazo se mide en tiempo real, no en número de sondeos, y es el mismo margen que las
/// pruebas de interfaz conceden al análisis antivirus (docs/PRUEBAS_UI.md).
const TASK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

fn wait(task: u64) -> MrTaskStatus {
    let mut st = MrTaskStatus::default();
    let start = std::time::Instant::now();
    loop {
        assert_eq!(unsafe { mr_task_status_get(task, &mut st) }, MR_OK);
        if st.state != 1 {
            return st;
        }
        assert!(
            start.elapsed() < TASK_DEADLINE,
            "la tarea {task} no terminó en {TASK_DEADLINE:?} (progreso {}/{})",
            st.done,
            st.total
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Espera una copia y comprueba que terminó bien; si no, el fallo incluye el error de la tarea
/// (antes solo decía «3 != 2» y no se podía saber qué operación de E/S había fallado).
fn wait_copy(task: u64) -> MrNtfsCopyReport {
    let st = wait(task);
    let mut r = MrNtfsCopyReport {
        struct_size: std::mem::size_of::<MrNtfsCopyReport>() as u32,
        ..MrNtfsCopyReport::default()
    };
    let rc = unsafe { mr_ntfs_copy_result(task, &mut r) };
    assert!(
        st.state == 2 && rc == MR_OK,
        "la copia {task} terminó en el estado {} (código {rc}): {}",
        st.state,
        last_error()
    );
    r
}

#[test]
fn volume_listing_and_entries_by_index() {
    let (_d, src, vol) = setup();
    let mut info = MrNtfsVolumeInfo {
        struct_size: std::mem::size_of::<MrNtfsVolumeInfo>() as u32,
        ..MrNtfsVolumeInfo::default()
    };
    assert_eq!(unsafe { mr_ntfs_volume_info(vol, &mut info) }, MR_OK);
    assert_eq!((info.cluster_size, info.record_size), (4096, 1024));
    assert_eq!(info.record_count, 40);
    assert_ne!(info.flags & MR_NTFS_UPCASE_FROM_VOLUME, 0);
    assert_eq!(info.serial, 0x0123_4567_89AB_CDEF);
    let label = string(|b, c, n| unsafe { mr_ntfs_volume_label(vol, b, c, n) }).unwrap();
    assert_eq!(label, "SINTÉTICO");
    // El volumen sobrevive al cierre de su origen.
    assert_eq!(mr_source_close(src), MR_OK);
    let mut listing = 0u64;
    assert_eq!(unsafe { mr_ntfs_list_dir(vol, 16, &mut listing) }, MR_OK);
    let mut li = MrNtfsListingInfo {
        struct_size: std::mem::size_of::<MrNtfsListingInfo>() as u32,
        ..MrNtfsListingInfo::default()
    };
    assert_eq!(unsafe { mr_ntfs_listing_info(listing, &mut li) }, MR_OK);
    assert_eq!((li.entry_count, li.live_count, li.anomaly_count), (8, 4, 0));
    assert_eq!((li.directory_record, li.directory_sequence), (16, 1));
    let mut got = Vec::new();
    for i in 0..li.entry_count {
        let mut e = entry();
        assert_eq!(unsafe { mr_ntfs_listing_entry(listing, i, &mut e) }, MR_OK);
        let name =
            string(|b, c, n| unsafe { mr_ntfs_listing_entry_name(listing, i, b, c, n) }).unwrap();
        got.push((name, e.state, e.record));
        if e.record == 20 {
            assert_eq!(e.size, 3000);
            assert_eq!(e.name_space, 3);
            assert_eq!(e.flags & MR_NTFS_ENTRY_DIRECTORY, 0);
        }
        if e.record == 25 {
            assert_eq!(
                (e.state, e.record_state),
                (MR_NTFS_ENTRY_RECORD_UNUSABLE, 8)
            );
        }
        if e.record == 22 {
            assert_ne!(e.flags & MR_NTFS_ENTRY_NAME_LOSSY, 0);
        }
    }
    assert_eq!(
        got.iter().map(|g| g.1).collect::<Vec<_>>(),
        [
            MR_NTFS_ENTRY_LIVE,
            MR_NTFS_ENTRY_INVALID_REFERENCE,
            MR_NTFS_ENTRY_LIVE,
            MR_NTFS_ENTRY_STALE_REUSED,
            MR_NTFS_ENTRY_RECORD_UNUSABLE,
            MR_NTFS_ENTRY_LIVE,
            MR_NTFS_ENTRY_STALE_NOT_IN_USE,
            MR_NTFS_ENTRY_LIVE,
        ]
    );
    assert_eq!(got[7].0, "ñandú.bin");
    // Índices fuera de límites, handles de otro tipo, tamaños erróneos.
    let mut e = entry();
    assert_eq!(
        unsafe { mr_ntfs_listing_entry(listing, 8, &mut e) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(
        unsafe { mr_ntfs_listing_entry(vol, 0, &mut e) },
        MR_ERR_INVALID_HANDLE
    );
    let mut bad = MrNtfsEntry {
        struct_size: 100,
        ..MrNtfsEntry::default()
    };
    assert_eq!(
        unsafe { mr_ntfs_listing_entry(listing, 0, &mut bad) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_ntfs_list_dir(vol, 17, &mut listing) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_ntfs_list_dir(vol, 25, &mut listing) },
        MR_ERR_CORRUPT
    );
    assert_eq!(mr_ntfs_listing_free(listing), MR_OK);
    assert_eq!(mr_ntfs_listing_free(listing), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_ntfs_listing_free(vol), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_ntfs_volume_free(vol), MR_ERR_INVALID_HANDLE);
}

#[test]
fn resolve_and_path_of() {
    let (_d, src, vol) = setup();
    let mut e = entry();
    let p = "/DOCS/sub/DEEP.txt";
    assert_eq!(
        unsafe { mr_ntfs_resolve(vol, p.as_ptr(), p.len(), &mut e) },
        MR_OK
    );
    assert_eq!(
        (e.record, e.parent_record, e.state),
        (21, 19, MR_NTFS_ENTRY_LIVE)
    );
    let p = "/Docs/viejo.txt";
    assert_eq!(
        unsafe { mr_ntfs_resolve(vol, p.as_ptr(), p.len(), &mut e) },
        MR_ERR_NOT_FOUND
    );
    assert!(last_error().contains("viejo.txt"));
    let p = "/a.txt/x";
    assert_eq!(
        unsafe { mr_ntfs_resolve(vol, p.as_ptr(), p.len(), &mut e) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_ntfs_resolve(src, p.as_ptr(), p.len(), &mut e) },
        MR_ERR_INVALID_HANDLE
    );
    let path = string(|b, c, n| unsafe { mr_ntfs_path_of(vol, 20, b, c, n) }).unwrap();
    assert_eq!(path, "/Docs/ñandú.bin");
    assert_eq!(
        string(|b, c, n| unsafe { mr_ntfs_path_of(vol, 26, b, c, n) }),
        Err(MR_ERR_CORRUPT)
    );
    assert!(last_error().contains("no es un directorio"));
}

#[test]
fn tree_task_lists_nodes_and_orphans() {
    let (_d, _src, vol) = setup();
    let mut task = 0u64;
    assert_eq!(unsafe { mr_ntfs_tree_start(vol, 0, &mut task) }, MR_OK);
    let st = wait(task);
    assert_eq!(st.state, 2);
    assert!(st.total > 0 && st.done == st.total);
    let mut info = MrNtfsTreeInfo {
        struct_size: std::mem::size_of::<MrNtfsTreeInfo>() as u32,
        ..MrNtfsTreeInfo::default()
    };
    assert_eq!(unsafe { mr_ntfs_tree_result(task, &mut info) }, MR_OK);
    assert_eq!(info.status, MR_NTFS_TREE_COMPLETE);
    assert_eq!(
        (info.orphan_count, info.directories, info.anomaly_count),
        (3, 3, 0)
    );
    let mut paths = Vec::new();
    for i in 0..info.node_count {
        let (mut parent, mut depth, mut e) = (0u64, 0u32, entry());
        assert_eq!(
            unsafe { mr_ntfs_tree_node(task, i, &mut parent, &mut depth, &mut e) },
            MR_OK
        );
        if i == 0 {
            assert_eq!((parent, depth, e.record), (MR_NTFS_ROOT_PARENT, 0, 5));
        } else {
            assert!(parent < i);
        }
        let p = string(|b, c, n| unsafe { mr_ntfs_tree_node_path(task, i, b, c, n) }).unwrap();
        if e.state == MR_NTFS_ENTRY_LIVE {
            paths.push(p);
        }
    }
    assert!(paths.contains(&"/Docs/Sub/deep.txt".to_string()));
    assert!(
        paths.contains(&"/a.txt".to_string()) && paths.contains(&"/Docs/Informe.txt".to_string())
    );
    let mut o = MrNtfsOrphan {
        struct_size: std::mem::size_of::<MrNtfsOrphan>() as u32,
        ..MrNtfsOrphan::default()
    };
    assert_eq!(unsafe { mr_ntfs_tree_orphan(task, 1, &mut o) }, MR_OK);
    assert_eq!((o.record, o.parent_record, o.reason), (26, 21, 6));
    let text = string(|b, c, n| unsafe { mr_ntfs_tree_orphan_text(task, 1, b, c, n) }).unwrap();
    assert!(
        text.starts_with("orphan_parent_not_directory\ncolgado.txt\n"),
        "{text}"
    );
    assert_eq!(
        unsafe { mr_ntfs_tree_orphan(task, 3, &mut o) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(
        unsafe { mr_ntfs_tree_start(vol, 2, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(mr_task_free(task), MR_OK);
    // Sin huérfanos.
    let mut t2 = 0u64;
    assert_eq!(
        unsafe { mr_ntfs_tree_start(vol, MR_NTFS_TREE_NO_ORPHANS, &mut t2) },
        MR_OK
    );
    wait(t2);
    assert_eq!(unsafe { mr_ntfs_tree_result(t2, &mut info) }, MR_OK);
    assert_eq!(info.orphan_count, 0);
    assert_eq!(mr_task_free(t2), MR_OK);
}

#[test]
fn stream_source_and_copy_task() {
    let (d, src, vol) = setup();
    let mut s = 0u64;
    assert_eq!(
        unsafe { mr_ntfs_open_stream(vol, 20, std::ptr::null(), 0, &mut s) },
        MR_OK
    );
    let mut info = MrImageInfo::default();
    assert_eq!(unsafe { mr_source_info(s, &mut info) }, MR_OK);
    assert_eq!((info.sector_size, info.size_bytes), (512, 3072));
    let mut buf = vec![0u8; 3072];
    let mut states = vec![0u8; 6];
    let mut n = 0usize;
    assert_eq!(
        unsafe {
            mr_source_read_mapped(
                s,
                0,
                6,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                states.as_mut_ptr(),
                states.len(),
                &mut n,
            )
        },
        MR_OK
    );
    assert_eq!(&buf[..3000], &tree_data()[..]);
    assert!(states.iter().all(|s| *s == 0));
    assert_eq!(mr_source_close(s), MR_OK);
    let ads = "ads";
    assert_eq!(
        unsafe { mr_ntfs_open_stream(vol, 20, ads.as_ptr(), ads.len(), &mut s) },
        MR_ERR_CORRUPT
    );
    // Copia.
    let out = d.path().join("copia ñ.bin");
    let o = out.to_str().unwrap();
    let mut task = 0u64;
    let copy = |path: &str, flags: u32, task: &mut u64| unsafe {
        mr_ntfs_copy_start(
            vol,
            20,
            std::ptr::null(),
            0,
            path.as_ptr(),
            path.len(),
            flags,
            task,
        )
    };
    assert_eq!(copy(o, 0, &mut task), MR_OK, "{}", last_error());
    let r = wait_copy(task);
    assert_eq!((r.bytes, r.untrusted_bytes, r.flags), (3000, 0, 0));
    assert_eq!(std::fs::read(&out).unwrap(), tree_data());
    assert_eq!(mr_task_free(task), MR_OK);
    // Nunca se sobrescribe sin la marca; nunca sobre el origen; ni con opciones desconocidas.
    assert_eq!(copy(o, 0, &mut task), MR_ERR_ALREADY_EXISTS);
    let image_path = d.path().join("árbol.img");
    let ip = image_path.to_str().unwrap();
    assert_eq!(
        copy(ip, MR_NTFS_COPY_OVERWRITE, &mut task),
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(copy(o, 4, &mut task), MR_ERR_INVALID_ARGUMENT);
    assert_eq!(
        copy(r"\\.\PhysicalDrive0", 0, &mut task),
        MR_ERR_PATH_REJECTED
    );
    assert_eq!(
        copy(o, MR_NTFS_COPY_OVERWRITE, &mut task),
        MR_OK,
        "{}",
        last_error()
    );
    wait_copy(task);
    assert_eq!(std::fs::read(&out).unwrap(), tree_data());
    assert_eq!(mr_task_free(task), MR_OK);
    // El origen sigue intacto.
    assert_eq!(std::fs::read(&image_path).unwrap(), tree_volume(true).image);
    // Sin handles abiertos al terminar (el registro es global y compartido por todas las pruebas).
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

// ---------------------------------------------------------------------------------------------
// Borrados y entradas residuales (#23) sobre `mr_fs_ntfs::testvol::residual_volume`
// ---------------------------------------------------------------------------------------------

#[test]
fn deleted_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrNtfsDeletedInfo>(), 88);
    assert_eq!(std::mem::size_of::<MrNtfsDeletedDetail>(), 48);
    assert_eq!(std::mem::size_of::<MrNtfsRecoverReport>(), 56);
    assert_eq!(std::mem::size_of::<MrNtfsSuspectRange>(), 48);
    assert_eq!(
        std::mem::offset_of!(MrNtfsDeletedInfo, residual_rejected),
        80
    );
    assert_eq!(std::mem::offset_of!(MrNtfsDeletedDetail, clusters), 24);
    assert_eq!(
        std::mem::offset_of!(MrNtfsRecoverReport, recoverability),
        40
    );
    assert_eq!(std::mem::offset_of!(MrNtfsSuspectRange, owner), 40);
}

fn residual_setup() -> (tempfile::TempDir, u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("residual.img");
    std::fs::write(&p, mr_fs_ntfs::testvol::residual_volume().image).unwrap();
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
    (dir, src, vol)
}

#[test]
fn deleted_task_lists_mft_and_residual_entries() {
    let (_d, src, vol) = residual_setup();
    let mut task = 0u64;
    assert_eq!(unsafe { mr_ntfs_deleted_start(vol, &mut task) }, MR_OK);
    let st = wait(task);
    assert_eq!(st.state, 2, "completada");
    assert!(st.done == st.total && st.total > 0);
    let mut info = MrNtfsDeletedInfo {
        struct_size: std::mem::size_of::<MrNtfsDeletedInfo>() as u32,
        ..MrNtfsDeletedInfo::default()
    };
    assert_eq!(unsafe { mr_ntfs_deleted_result(task, &mut info) }, MR_OK);
    assert_eq!(info.entry_count, 8);
    assert_eq!(info.residual_only_count, 5);
    assert_eq!((info.index_directories, info.index_nodes), (2, 3));
    assert_eq!((info.index_free_nodes, info.residual_rejected), (2, 2));
    assert_eq!(info.flags & MR_NTFS_DELETED_INDEX_UNREADABLE, 0);
    let mut got = Vec::new();
    for i in 0..info.entry_count {
        let mut e = entry();
        assert_eq!(unsafe { mr_ntfs_deleted_entry(task, i, &mut e) }, MR_OK);
        let mut d = MrNtfsDeletedDetail {
            struct_size: std::mem::size_of::<MrNtfsDeletedDetail>() as u32,
            ..MrNtfsDeletedDetail::default()
        };
        assert_eq!(unsafe { mr_ntfs_deleted_detail(task, i, &mut d) }, MR_OK);
        let text = |f: u32| {
            string(|b, c, n| unsafe { mr_ntfs_deleted_text(task, i, f, b, c, n) }).unwrap()
        };
        got.push((
            e.record,
            e.state,
            d.recoverability,
            d.origins,
            text(0),
            text(1),
        ));
        if e.state != MR_NTFS_ENTRY_DELETED {
            assert_ne!(e.flags & MR_NTFS_ENTRY_TIMES_FROM_INDEX, 0);
            assert_eq!(d.stream_count, 0);
            assert!(text(2).starts_with("entrada residual"), "{}", text(2));
        }
        if e.record == 23 {
            assert_eq!(e.record_state, 8, "dañado");
        }
        if e.record == 19 {
            assert_eq!((e.sequence, e.found_sequence), (1, 2));
        }
    }
    let o_mft = MR_NTFS_ORIGIN_MFT;
    let full = MR_NTFS_RECOVERABLE_FULL;
    let none = MR_NTFS_RECOVERABLE_NONE;
    assert_eq!(
        got.iter().map(|g| (g.0, g.1, g.2, g.3)).collect::<Vec<_>>(),
        [
            (
                16,
                MR_NTFS_ENTRY_DELETED,
                MR_NTFS_RECOVERABLE_NOT_APPLICABLE,
                o_mft
            ),
            (
                18,
                MR_NTFS_ENTRY_DELETED,
                full,
                o_mft | MR_NTFS_ORIGIN_INDEX_SLACK | MR_NTFS_ORIGIN_INDEX_FREE_NODE
            ),
            (
                24,
                MR_NTFS_ENTRY_DELETED,
                full,
                o_mft | MR_NTFS_ORIGIN_DELETED_DIRECTORY_INDEX
            ),
            (
                19,
                MR_NTFS_ENTRY_RESIDUAL_REUSED,
                none,
                MR_NTFS_ORIGIN_INDEX_SLACK
            ),
            (
                20,
                MR_NTFS_ENTRY_RESIDUAL_UNUSABLE,
                none,
                MR_NTFS_ORIGIN_INDEX_SLACK
            ),
            (
                21,
                MR_NTFS_ENTRY_RESIDUAL_FREE,
                none,
                MR_NTFS_ORIGIN_INDEX_FREE_NODE
            ),
            (
                23,
                MR_NTFS_ENTRY_RESIDUAL_UNUSABLE,
                none,
                MR_NTFS_ORIGIN_INDEX_FREE_NODE
            ),
            (
                26,
                MR_NTFS_ENTRY_RESIDUAL_FREE,
                none,
                MR_NTFS_ORIGIN_INDEX_FREE_NODE
            ),
        ]
    );
    assert_eq!(got[1].4, "borrado.txt");
    assert_eq!(got[2].5, "/Viejo/hijo.txt");
    assert_eq!(got[6].4, "dos largo.txt");
    // Índice y campo fuera de rango.
    let mut e = entry();
    assert_eq!(
        unsafe { mr_ntfs_deleted_entry(task, 8, &mut e) },
        MR_ERR_OUT_OF_RANGE
    );
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_ntfs_deleted_text(task, 0, 9, std::ptr::null_mut(), 0, &mut n) },
        MR_ERR_INVALID_ARGUMENT
    );
    // Otro tipo de handle.
    assert_eq!(
        unsafe { mr_ntfs_deleted_result(vol, &mut info) },
        MR_ERR_INVALID_HANDLE
    );
    assert_eq!(mr_task_free(task), MR_OK);
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn recover_task_writes_only_deleted_records() {
    let (d, src, vol) = residual_setup();
    let out = d.path().join("recuperado.txt");
    let os = out.to_str().unwrap();
    let start = |rec: u64, seq: u32, flags: u32, task: &mut u64| unsafe {
        mr_ntfs_recover_start(
            vol,
            rec,
            seq,
            std::ptr::null(),
            0,
            os.as_ptr(),
            os.len(),
            flags,
            task,
        )
    };
    // Entradas solo residuales y vivos: nunca se recuperan ni crean la salida.
    for (rec, seq) in [(19u64, 1u32), (21, 3), (17, 1), (18, 1)] {
        let mut t = 0u64;
        assert_eq!(start(rec, seq, 0, &mut t), MR_ERR_INVALID_ARGUMENT, "{rec}");
        assert!(!out.exists());
    }
    let mut t = 0u64;
    assert_ne!(start(20, 1, 0, &mut t), MR_OK, "registro dañado");
    let mut task = 0u64;
    assert_eq!(start(18, 2, 0, &mut task), MR_OK, "{}", last_error());
    assert_eq!(wait(task).state, 2);
    let mut r = MrNtfsRecoverReport {
        struct_size: std::mem::size_of::<MrNtfsRecoverReport>() as u32,
        ..MrNtfsRecoverReport::default()
    };
    assert_eq!(unsafe { mr_ntfs_recover_result(task, &mut r) }, MR_OK);
    assert_eq!((r.bytes, r.untrusted_bytes, r.suspect_count), (12, 0, 0));
    assert_eq!(r.recoverability, MR_NTFS_RECOVERABLE_FULL);
    assert_eq!(std::fs::read(&out).unwrap(), b"hola borrado");
    let summary =
        string(|b, c, n| unsafe { mr_ntfs_recover_text(task, u32::MAX, b, c, n) }).unwrap();
    assert!(summary.contains("residente"), "{summary}");
    let mut x = MrNtfsSuspectRange {
        struct_size: std::mem::size_of::<MrNtfsSuspectRange>() as u32,
        ..MrNtfsSuspectRange::default()
    };
    assert_eq!(
        unsafe { mr_ntfs_recover_suspect(task, 0, &mut x) },
        MR_ERR_OUT_OF_RANGE
    );
    // Salida existente: sin confirmación, MR_ERR_ALREADY_EXISTS; con ella, se sustituye.
    let mut t = 0u64;
    assert_eq!(start(24, 2, 0, &mut t), MR_ERR_ALREADY_EXISTS);
    assert_eq!(std::fs::read(&out).unwrap(), b"hola borrado");
    let mut t = 0u64;
    assert_eq!(start(24, 2, MR_NTFS_RECOVER_OVERWRITE, &mut t), MR_OK);
    assert_eq!(wait(t).state, 2);
    assert_eq!(std::fs::read(&out).unwrap(), b"hijo");
    // Opciones desconocidas.
    let mut t = 0u64;
    assert_eq!(start(24, 2, 8, &mut t), MR_ERR_INVALID_ARGUMENT);
    assert_eq!(mr_ntfs_volume_free(vol), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}
