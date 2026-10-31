//! Pruebas de la frontera C de la recuperación (#26) sobre el volumen sintético de
//! `mr_fs_ntfs::testvol::tree_volume`: estructuras, opciones, tarea con progreso, informe,
//! textos, preguntas de conflicto respondidas por sondeo, cancelación y rechazos.

use super::*;
use mr_fs_ntfs::testvol::{tree_data, tree_volume};

#[test]
fn recover_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrRecoverOptions>(), 48);
    assert_eq!(std::mem::size_of::<MrRecoverProgress>(), 56);
    assert_eq!(std::mem::size_of::<MrRecoverQuestion>(), 40);
    assert_eq!(std::mem::size_of::<MrRecoverReport>(), 200);
    assert_eq!(std::mem::offset_of!(MrRecoverOptions, split_size), 24);
    assert_eq!(std::mem::offset_of!(MrRecoverOptions, fill), 32);
    assert_eq!(std::mem::offset_of!(MrRecoverOptions, rename_limit), 36);
    assert_eq!(std::mem::offset_of!(MrRecoverQuestion, existing_size), 24);
    assert_eq!(std::mem::offset_of!(MrRecoverReport, written_integro), 72);
    assert_eq!(std::mem::offset_of!(MrRecoverReport, capabilities), 184);
    assert_eq!(std::mem::offset_of!(MrRecoverReport, max_file_size), 192);
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

fn status(task: u64) -> MrTaskStatus {
    let mut st = MrTaskStatus::default();
    assert_eq!(unsafe { mr_task_status_get(task, &mut st) }, MR_OK);
    st
}

fn wait(task: u64) -> MrTaskStatus {
    for _ in 0..4000 {
        let st = status(task);
        if st.state != 1 {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("la tarea no termina");
}

struct Setup {
    dir: tempfile::TempDir,
    image: std::path::PathBuf,
    src: u64,
    vol: u64,
    vfs: u64,
}

impl Drop for Setup {
    fn drop(&mut self) {
        mr_vfs_free(self.vfs);
        mr_ntfs_volume_free(self.vol);
        mr_source_close(self.src);
    }
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("vfs.img");
    std::fs::write(&image, tree_volume(true).image).unwrap();
    let ps = image.to_str().unwrap();
    let mut src = 0u64;
    assert_eq!(
        unsafe { mr_source_open(ps.as_ptr(), ps.len(), 0, 0, &mut src) },
        MR_OK
    );
    let mut vol = 0u64;
    assert_eq!(unsafe { mr_ntfs_open(src, &mut vol) }, MR_OK);
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_vfs_build_start(vol, std::ptr::null(), &mut task) },
        MR_OK
    );
    wait(task);
    let mut vfs = 0u64;
    assert_eq!(unsafe { mr_vfs_build_result(task, &mut vfs) }, MR_OK);
    assert_eq!(mr_task_free(task), MR_OK);
    Setup {
        dir,
        image,
        src,
        vol,
        vfs,
    }
}

fn opts() -> MrRecoverOptions {
    MrRecoverOptions {
        struct_size: std::mem::size_of::<MrRecoverOptions>() as u32,
        ..MrRecoverOptions::default()
    }
}

fn start_with(
    s: &Setup,
    volume: u64,
    nodes: &[u32],
    o: Option<&MrRecoverOptions>,
    dest: &str,
    masks: &str,
    journal: &str,
) -> Result<u64, i32> {
    let mut task = 0u64;
    let rc = unsafe {
        mr_recover_start(
            volume,
            s.vfs,
            nodes.as_ptr(),
            nodes.len(),
            o.map_or(std::ptr::null(), |o| o as *const _),
            dest.as_ptr(),
            dest.len(),
            masks.as_ptr(),
            masks.len(),
            journal.as_ptr(),
            journal.len(),
            &mut task,
        )
    };
    if rc == MR_OK {
        Ok(task)
    } else {
        Err(rc)
    }
}

fn start(s: &Setup, nodes: &[u32], o: Option<&MrRecoverOptions>, dest: &str) -> Result<u64, i32> {
    start_with(s, s.vol, nodes, o, dest, "", "")
}

fn report(task: u64) -> Result<MrRecoverReport, i32> {
    let mut r = MrRecoverReport {
        struct_size: std::mem::size_of::<MrRecoverReport>() as u32,
        ..MrRecoverReport::default()
    };
    match unsafe { mr_recover_result(task, &mut r) } {
        MR_OK => Ok(r),
        rc => Err(rc),
    }
}

fn progress(task: u64) -> MrRecoverProgress {
    let mut p = MrRecoverProgress {
        struct_size: std::mem::size_of::<MrRecoverProgress>() as u32,
        ..MrRecoverProgress::default()
    };
    assert_eq!(unsafe { mr_recover_progress(task, &mut p) }, MR_OK);
    p
}

#[test]
fn recover_task_report_texts_questions_and_cancel() {
    let s = setup();
    let dest = s.dir.path().join("rec");
    let d = dest.to_str().unwrap();
    let t = start(&s, &[0], None, d).unwrap();
    let st = wait(t);
    assert_eq!(st.state, 2, "{}", last_error());
    assert_eq!(st.done, st.total);
    let p = progress(t);
    assert_eq!(p.files_done, p.files_total);
    assert_eq!(p.flags, 0);
    let r = report(t).unwrap();
    assert_eq!(r.status, MR_RECOVER_STATUS_COMPLETE);
    assert!(r.files_written >= 7, "{r:?}");
    assert_eq!(r.written_integro, r.files_written);
    assert!(r.skipped_state >= 1, "registro dañado omitido");
    assert_eq!(r.exit_code, 3);
    assert_eq!(r.errors, 0);
    let journal =
        string(|b, c, n| unsafe { mr_recover_result_text(t, MR_RECOVER_TEXT_JOURNAL, b, c, n) })
            .unwrap();
    assert!(std::fs::read_to_string(&journal)
        .unwrap()
        .contains("\"tipo\":\"fin\""));
    let root = string(|b, c, n| unsafe {
        mr_recover_result_text(t, MR_RECOVER_TEXT_DESTINATION, b, c, n)
    })
    .unwrap();
    assert!(root.ends_with("rec"), "{root}");
    assert_eq!(
        std::fs::read(dest.join("Docs").join("ñandú.bin")).unwrap(),
        tree_data()
    );
    // Sin pregunta pendiente.
    let mut q = MrRecoverQuestion {
        struct_size: std::mem::size_of::<MrRecoverQuestion>() as u32,
        ..MrRecoverQuestion::default()
    };
    assert_eq!(unsafe { mr_recover_question(t, &mut q) }, MR_ERR_NOT_FOUND);
    assert_eq!(
        mr_recover_answer(t, 1, MR_RECOVER_CONFLICT_SKIP, 0),
        MR_ERR_NOT_FOUND
    );
    assert_eq!(mr_task_free(t), MR_OK);
    assert_eq!(report(t).unwrap_err(), MR_ERR_INVALID_HANDLE);

    // Segunda vez con «preguntar»: se responden las preguntas por sondeo.
    let mut o = opts();
    o.conflict = MR_RECOVER_CONFLICT_ASK;
    let t = start(&s, &[0], Some(&o), d).unwrap();
    let mut answered = 0;
    while status(t).state == 1 {
        let mut q = MrRecoverQuestion {
            struct_size: std::mem::size_of::<MrRecoverQuestion>() as u32,
            ..MrRecoverQuestion::default()
        };
        if unsafe { mr_recover_question(t, &mut q) } == MR_OK {
            let req = string(|b, c, n| unsafe {
                mr_recover_question_text(t, q.id, MR_RECOVER_QUESTION_TEXT_REQUESTED, b, c, n)
            })
            .unwrap();
            let existing = string(|b, c, n| unsafe {
                mr_recover_question_text(t, q.id, MR_RECOVER_QUESTION_TEXT_EXISTING, b, c, n)
            })
            .unwrap();
            assert!(!req.is_empty() && std::path::Path::new(&existing).exists());
            assert_eq!(mr_recover_answer(t, q.id, 99, 0), MR_ERR_INVALID_ARGUMENT);
            assert_eq!(
                mr_recover_answer(t, q.id, MR_RECOVER_CONFLICT_SKIP_IDENTICAL, 1),
                MR_OK
            );
            answered += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(answered, 2);
    let r = report(t).unwrap();
    assert_eq!(r.files_written, 0);
    assert!(r.skipped_identical >= 7);
    mr_task_free(t);

    // Cancelación mientras espera la respuesta.
    let t = start(&s, &[0], Some(&o), d).unwrap();
    while progress(t).flags & MR_RECOVER_PROGRESS_QUESTION == 0 {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(mr_task_cancel(t), MR_OK);
    assert_eq!(wait(t).state, 4);
    let r = report(t).unwrap();
    assert_eq!(r.status, MR_RECOVER_STATUS_CANCELLED);
    assert_eq!(r.files_written, 0);
    mr_task_free(t);
}

#[test]
fn recover_start_rejections_journal_and_overwrite() {
    let s = setup();
    let dest = s.dir.path().join("rec2");
    let d = dest.to_str().unwrap();
    // Destino = la imagen o un dispositivo.
    let img = s.image.to_str().unwrap();
    assert_eq!(start(&s, &[0], None, img), Err(MR_ERR_INVALID_ARGUMENT));
    assert_eq!(
        start(&s, &[0], None, r"\\.\PhysicalDrive0"),
        Err(MR_ERR_PATH_REJECTED)
    );
    // Opciones: tamaño, indicadores, política, sustitución sin confirmar, relleno.
    let mut o = opts();
    o.struct_size = 8;
    assert_eq!(start(&s, &[0], Some(&o), d), Err(MR_ERR_INVALID_ARGUMENT));
    let mut o = opts();
    o.flags = 1 << 30;
    assert_eq!(start(&s, &[0], Some(&o), d), Err(MR_ERR_INVALID_ARGUMENT));
    let mut o = opts();
    o.conflict = 77;
    assert_eq!(start(&s, &[0], Some(&o), d), Err(MR_ERR_INVALID_ARGUMENT));
    let mut o = opts();
    o.conflict = MR_RECOVER_CONFLICT_OVERWRITE;
    assert_eq!(start(&s, &[0], Some(&o), d), Err(MR_ERR_INVALID_ARGUMENT));
    let mut o = opts();
    o.fill = [7; 4];
    assert_eq!(start(&s, &[0], Some(&o), d), Err(MR_ERR_INVALID_ARGUMENT));
    // Nodos: ninguno o inexistente; handle de otro tipo; reglas no válidas.
    assert_eq!(start(&s, &[], None, d), Err(MR_ERR_INVALID_ARGUMENT));
    assert_eq!(start(&s, &[999_999], None, d), Err(MR_ERR_OUT_OF_RANGE));
    assert_eq!(
        start_with(&s, s.src, &[0], None, d, "", ""),
        Err(MR_ERR_INVALID_HANDLE)
    );
    assert_eq!(
        start_with(&s, s.vol, &[0], None, d, "!", ""),
        Err(MR_ERR_INVALID_ARGUMENT)
    );
    assert!(!dest.exists(), "nada escrito");
    // Diario explícito.
    let j = s.dir.path().join("diario.jsonl");
    let mut o = opts();
    o.flags = MR_RECOVER_NO_TIMES | MR_RECOVER_NO_MARK_INCOMPLETE;
    let t = start_with(&s, s.vol, &[0], Some(&o), d, "", j.to_str().unwrap()).unwrap();
    wait(t);
    assert!(report(t).unwrap().files_written >= 7, "{}", last_error());
    mr_task_free(t);
    assert!(j.exists());
    // El diario nunca se sustituye: la tarea falla.
    let t = start_with(&s, s.vol, &[0], None, d, "", j.to_str().unwrap()).unwrap();
    assert_eq!(wait(t).state, 3);
    assert_eq!(report(t).unwrap_err(), MR_ERR_TASK_FAILED);
    mr_task_free(t);
    // Sustitución confirmada: sobre lo ya escrito, sin renombrar.
    let mut o = opts();
    o.conflict = MR_RECOVER_CONFLICT_OVERWRITE;
    o.flags = MR_RECOVER_CONFIRM_OVERWRITE;
    let t = start(&s, &[0], Some(&o), d).unwrap();
    wait(t);
    let r = report(t).unwrap();
    assert!(r.files_written >= 7);
    assert_eq!(r.renamed, 0);
    mr_task_free(t);
    // El origen no cambia.
    assert_eq!(std::fs::read(&s.image).unwrap(), tree_volume(true).image);
}
