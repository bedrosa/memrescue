//! Recuperación desde un volumen NTFS sintético (`mr_fs_ntfs::testvol::tree_volume`) por la API
//! (`mr_api::recover` y `start_recover_task`): contenido de vivos, enlaces duros, nombres con
//! sustitutos sueltos, registros dañados, huérfanos, sectores ilegibles simulados
//! (`FaultyDevice`) y tarea con progreso, preguntas y cancelación.

mod common;

use std::sync::Arc;

use common::*;
use mr_api::mr_fs_ntfs::testvol::{tree_data, tree_volume, TREE_DATA_LCN};
use mr_api::mr_recover::{
    ConflictAnswer, ConflictDecision, ConflictPolicy, RecoverOptions, RunStatus, Selection,
    DEFAULT_FILL,
};
use mr_api::mr_vfs::filter::Filter;
use mr_api::mr_vfs::{Category, NodeId, NtfsBuildOptions};
use mr_api::task::{TaskContext, TaskState};
use mr_api::RecoverRequest;

fn live_selection() -> Selection {
    Selection {
        nodes: vec![NodeId::ROOT],
        filter: Filter::categories(&[Category::Live, Category::Deleted, Category::Orphan]),
        masks: String::new(),
    }
}

fn open_bytes(image: Vec<u8>, bad: &[u64]) -> mr_api::Source {
    let mem = mr_io::MemDevice::new(image, 512).unwrap();
    let dev = mr_io::FaultyDevice::new(mem).with_bad(bad.iter().copied());
    mr_api::source_from_device(Arc::new(dev), "volumen sintético con sectores ilegibles")
}

#[test]
fn synthetic_volume_live_files_and_damaged_records() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("arbol.img");
    std::fs::write(&img, tree_volume(true).image).unwrap();
    let before = sha256(&std::fs::read(&img).unwrap());
    let src = mr_api::open_source(&img, 0).unwrap();
    let vol = mr_api::ntfs_open(&src).unwrap();
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .unwrap();
    let dest = dir.path().join("recuperado");
    let req = RecoverRequest {
        selection: live_selection(),
        options: RecoverOptions::new(&dest),
    };
    let s = mr_api::recover(&src, &vol, &vfs, &req, &TaskContext::detached()).unwrap();
    assert_eq!(s.status, RunStatus::Complete);
    let f = tree_files(&dest);
    assert_eq!(f["a.txt"], b"hola");
    // Enlace duro: copia independiente en cada ruta.
    assert_eq!(f["Docs/Informe.txt"], b"hola");
    assert_eq!(f["Docs/ñandú.bin"], tree_data());
    assert_eq!(f["Docs/Sub/deep.txt"], b"profundo");
    assert_eq!(f["Docs/b%uD800c"], b"sustituto");
    assert_eq!(f["Medio.txt"], b"medio");
    assert_eq!(f["Ágil.txt"], b"agil");
    assert!(
        !f.contains_key("Docs/roto.txt"),
        "registro dañado: sin contenido inventado"
    );
    assert!(!f.contains_key("Docs/viejo.txt"), "residual sin datos");
    let j = journal(std::path::Path::new(&s.journal));
    let ev = files_by_path(&j);
    assert_eq!(ev["/Docs/roto.txt"].get("estado").s(), "no_disponible");
    assert_eq!(ev["/Docs/roto.txt"].get("accion").s(), "omitido_estado");
    assert_eq!(ev["/Docs/viejo.txt"].get("estado").s(), "sin_datos");
    let n = &ev["/Docs/ñandú.bin"];
    assert_eq!(n.get("estado").s(), "integro");
    let h = n.get("flujos").arr()[0].get("hashes");
    assert_eq!(h.get("ambito").s(), "contenido");
    assert_eq!(h.get("sha256").s(), sha256(&tree_data()));
    assert!(n.get("nombre_utf16").is_null());
    assert_eq!(ev["/Docs/b%uD800c"].get("nombre_utf16").s(), "0062D8000063");
    // Cabecera: sistema de archivos y tamaño de clúster.
    assert_eq!(j[0].get("origen").get("sistema_archivos").s(), "ntfs");
    assert_eq!(j[0].get("origen").get("tamano_cluster").n(), 4096);
    // Algo no íntegro omitido por su estado: código 3.
    assert_eq!(s.exit_code(), 3);
    assert_eq!(
        sha256(&std::fs::read(&img).unwrap()),
        before,
        "origen intacto"
    );
    assert_no_temps(&dest);
}

#[test]
fn unreadable_sectors_are_filled_and_journaled_with_their_lba() {
    let image = tree_volume(true).image;
    // ñandú.bin: 3000 bytes en el clúster 210 (4 KiB): LBA 1680..1686. Se estropea el segundo
    // sector del fichero.
    let lba = TREE_DATA_LCN * 8 + 1;
    let src = open_bytes(image, &[lba]);
    let vol = mr_api::ntfs_open(&src).unwrap();
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sel = Selection {
        nodes: vec![vfs.find("/Docs/ñandú.bin").unwrap()],
        ..Selection::default()
    };
    let req = RecoverRequest {
        selection: sel,
        options: RecoverOptions::new(dir.path()),
    };
    let s = mr_api::recover(&src, &vol, &vfs, &req, &TaskContext::detached()).unwrap();
    assert_eq!(s.exit_code(), 3);
    assert_eq!(s.totals.written(mr_api::mr_vfs::DataState::Parcial), 1);
    let f = tree_files(dir.path());
    let mut want = tree_data();
    for (i, b) in want.iter_mut().enumerate().take(1024).skip(512) {
        *b = DEFAULT_FILL[i % 4];
    }
    assert_eq!(
        f["ñandú.bin"], want,
        "relleno exactamente en el sector ilegible"
    );
    let mark = String::from_utf8(f["ñandú.bin.mrbad"].clone()).unwrap();
    assert!(
        mark.contains(&format!("LBA del volumen {lba} (1 sectores)")),
        "{mark}"
    );
    let j = journal(std::path::Path::new(&s.journal));
    let e = &files_by_path(&j)["/Docs/ñandú.bin"];
    assert_eq!(e.get("estado_inicial").s(), "integro");
    assert_eq!(e.get("estado").s(), "parcial");
    let st = &e.get("flujos").arr()[0];
    assert_eq!(st.get("hashes").get("ambito").s(), "escrito_con_relleno");
    assert_ne!(st.get("hashes").get("sha256").s(), sha256(&tree_data()));
    let r = st.get("tramos_no_fiables").arr();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].get("desplazamiento").n(), 512);
    assert_eq!(r[0].get("longitud").n(), 512);
    assert_eq!(r[0].get("estado").s(), "unreadable");
    assert_eq!(r[0].get("lba").n(), lba);
    assert_eq!(r[0].get("lba_origen").n(), lba);
    assert_eq!(r[0].get("sectores").n(), 1);
    assert_eq!(st.get("bytes_por_estado").get("unreadable").n(), 512);
    assert_eq!(st.get("bytes_por_estado").get("data").n(), 2488);
}

#[test]
fn recover_task_progress_questions_and_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("arbol.img");
    std::fs::write(&img, tree_volume(true).image).unwrap();
    let src = mr_api::open_source(&img, 0).unwrap();
    let vol = Arc::new(mr_api::ntfs_open(&src).unwrap());
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .unwrap();
    let dest = dir.path().join("d");
    let req = RecoverRequest {
        selection: live_selection(),
        options: RecoverOptions::new(&dest),
    };
    let t = mr_api::start_recover_task(
        Arc::clone(&src),
        Arc::clone(&vol),
        Arc::clone(&vfs),
        req.clone(),
    )
    .unwrap();
    let s = t.wait().unwrap();
    assert_eq!(t.state(), TaskState::Completed);
    let p = t.progress();
    assert_eq!(p.files_done, p.files_total);
    assert_eq!(p.bytes_done, p.bytes_total);
    assert!(p.bytes_total >= 3000);
    assert_eq!(t.task().progress(), (p.bytes_done, p.bytes_total));
    assert!(s.totals.files_written >= 7);
    // Segunda vez con «preguntar»: la tarea espera la respuesta.
    let mut req2 = req.clone();
    req2.options.conflict = ConflictPolicy::Ask;
    let t = mr_api::start_recover_task(
        Arc::clone(&src),
        Arc::clone(&vol),
        Arc::clone(&vfs),
        req2.clone(),
    )
    .unwrap();
    let mut answered = 0;
    while t.state() == TaskState::Running {
        if let Some((id, q)) = t.pending_question() {
            assert!(!q.requested.is_empty());
            assert!(!t.answer(
                id + 1000,
                ConflictAnswer {
                    decision: ConflictDecision::Skip,
                    apply_to_all: false,
                }
            ));
            assert!(t.answer(
                id,
                ConflictAnswer {
                    decision: ConflictDecision::SkipIfIdentical,
                    apply_to_all: true,
                }
            ));
            answered += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let s = t.wait().unwrap();
    assert_eq!(answered, 2, "una pregunta por tipo con «aplicar a todos»");
    assert_eq!(s.totals.files_written, 0);
    assert!(s.totals.skipped_identical >= 7);
    // Cancelación mientras espera una respuesta: estado cancelada, nada nuevo.
    let t = mr_api::start_recover_task(Arc::clone(&src), vol, vfs, req2).unwrap();
    while t.pending_question().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    t.cancel();
    let s = t.wait().unwrap();
    assert_eq!(t.state(), TaskState::Cancelled);
    assert_eq!(s.status, RunStatus::Cancelled);
    assert_eq!(s.totals.files_written, 0);
    // Destino dentro del origen: rechazado antes de lanzar.
    let bad = RecoverRequest {
        selection: live_selection(),
        options: RecoverOptions::new(img.join("x")),
    };
    let vol = Arc::new(mr_api::ntfs_open(&src).unwrap());
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .unwrap();
    assert!(
        mr_api::start_recover_task(Arc::clone(&src), Arc::clone(&vol), Arc::clone(&vfs), bad)
            .is_err()
    );
    let dev = RecoverRequest {
        selection: live_selection(),
        options: RecoverOptions::new(r"\\.\PhysicalDrive0"),
    };
    assert!(matches!(
        mr_api::start_recover_task(src, vol, vfs, dev),
        Err(mr_api::ApiError::Io(mr_io::IoError::DevicePathRejected(_)))
    ));
}
