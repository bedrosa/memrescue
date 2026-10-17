//! Pruebas de la frontera C llamando a las funciones exportadas como lo haría un cliente.

use super::*;
use std::io::Write;
use std::thread;
use std::time::Duration;

#[test]
fn struct_layouts_match_header() {
    assert_eq!(std::mem::size_of::<MrImageInfo>(), 32);
    assert_eq!(std::mem::size_of::<MrTaskStatus>(), 24);
    assert_eq!(std::mem::size_of::<MrVerifyReport>(), 48);
    assert_eq!(mr_abi_version(), 2);
}

fn temp_image(name: &str, sectors: usize, ss: usize) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    let data: Vec<u8> = (0..sectors * ss).map(|i| (i / ss) as u8).collect();
    std::fs::write(&p, data).unwrap();
    let s = p.to_str().unwrap().to_string();
    (dir, s)
}

fn open(p: &str, ss: u32, flags: u32) -> Result<u64, i32> {
    let mut h = 0u64;
    let rc = unsafe { mr_source_open(p.as_ptr(), p.len(), ss, flags, &mut h) };
    if rc == MR_OK {
        Ok(h)
    } else {
        Err(rc)
    }
}

fn wait(task: u64) -> MrTaskStatus {
    let mut st = MrTaskStatus::default();
    for _ in 0..2000 {
        assert_eq!(unsafe { mr_task_status_get(task, &mut st) }, MR_OK);
        if st.state != 1 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    st
}

#[test]
fn source_open_info_read_close_for_all_sector_sizes() {
    for ss in [512u32, 1024, 2048, 4096] {
        let (_d, p) = temp_image("s.img", 8, ss as usize);
        let h = open(&p, ss, 0).unwrap();
        let mut info = MrImageInfo::default();
        assert_eq!(unsafe { mr_source_info(h, &mut info) }, MR_OK);
        assert_eq!((info.sector_count, info.sector_size), (8, ss));
        let mut buf = vec![0u8; 2 * ss as usize];
        let mut n = 0usize;
        assert_eq!(
            unsafe { mr_source_read(h, 6, 2, buf.as_mut_ptr(), buf.len(), &mut n) },
            MR_OK
        );
        assert_eq!(n, 2 * ss as usize);
        assert!(buf[..ss as usize].iter().all(|&b| b == 6));
        assert!(buf[ss as usize..].iter().all(|&b| b == 7));
        // Fuera de rango, búfer pequeño y recuento cero.
        assert_eq!(
            unsafe { mr_source_read(h, 7, 2, buf.as_mut_ptr(), buf.len(), &mut n) },
            MR_ERR_OUT_OF_RANGE
        );
        assert_eq!(
            unsafe { mr_source_read(h, 0, 0, buf.as_mut_ptr(), buf.len(), &mut n) },
            MR_ERR_OUT_OF_RANGE
        );
        assert_eq!(
            unsafe { mr_source_read(h, 0, 2, buf.as_mut_ptr(), 10, &mut n) },
            MR_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(n, 2 * ss as usize);
        assert_eq!(mr_source_close(h), MR_OK);
        assert_eq!(mr_source_close(h), MR_ERR_INVALID_HANDLE);
        assert_eq!(
            unsafe { mr_source_read(h, 0, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
            MR_ERR_INVALID_HANDLE
        );
    }
}

#[test]
fn handles_of_the_wrong_kind_are_rejected() {
    let (_d, p) = temp_image("k.img", 4, 512);
    let src = open(&p, 512, 0).unwrap();
    let mut st = MrTaskStatus::default();
    assert_eq!(
        unsafe { mr_task_status_get(src, &mut st) },
        MR_ERR_INVALID_HANDLE
    );
    assert_eq!(mr_task_cancel(src), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_task_free(src), MR_ERR_INVALID_HANDLE);
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_source_hash_start(src, 8, std::ptr::null(), 0, &mut task) },
        MR_OK
    );
    wait(task);
    assert_eq!(mr_source_close(task), MR_ERR_INVALID_HANDLE);
    let mut rep = MrVerifyReport::default();
    assert_eq!(
        unsafe { mr_verify_result(task, &mut rep) },
        MR_ERR_INVALID_HANDLE
    );
    assert_eq!(mr_task_free(task), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn closing_a_source_does_not_break_a_running_task() {
    let (_d, p) = temp_image("c.img", 4096, 512);
    let src = open(&p, 512, 0).unwrap();
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_source_hash_start(src, 8, std::ptr::null(), 0, &mut task) },
        MR_OK
    );
    assert_eq!(mr_source_close(src), MR_OK);
    assert_eq!(wait(task).state, 2);
    let mut buf = vec![0u8; 65];
    let mut n = 0;
    assert_eq!(
        unsafe { mr_hash_result(task, 8, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert_eq!(n, 64);
    assert_eq!(mr_task_free(task), MR_OK);
}

#[test]
fn verify_flow_reports_clean_image() {
    let (_d, p) = temp_image("v.img", 300, 512);
    let src = open(&p, 512, 0).unwrap();
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_source_verify_start(src, 0, 0, 1, &mut task) },
        MR_OK
    );
    let st = wait(task);
    assert_eq!((st.state, st.done, st.total), (2, 300, 300));
    let mut rep = MrVerifyReport::default();
    assert_eq!(unsafe { mr_verify_result(task, &mut rep) }, MR_OK);
    assert_eq!(
        (
            rep.sectors_requested,
            rep.sectors_ok,
            rep.sectors_bad,
            rep.bad_range_count
        ),
        (300, 300, 0, 0)
    );
    let (mut l, mut c) = (0, 0);
    assert_eq!(
        unsafe { mr_verify_bad_range(task, 0, &mut l, &mut c) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(
        unsafe { mr_source_verify_start(src, 0, 0, 1000, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    mr_source_close(src);
}

#[test]
fn probe_and_unsupported_containers() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.vhdx");
    let mut d = vec![0u8; 4096];
    d[..8].copy_from_slice(b"vhdxfile");
    std::fs::write(&p, d).unwrap();
    let ps = p.to_str().unwrap();
    let mut f = 0u32;
    assert_eq!(
        unsafe { mr_image_probe(ps.as_ptr(), ps.len(), &mut f) },
        MR_OK
    );
    assert_eq!(f, 3);
    assert_eq!(open(ps, 512, 0), Err(MR_ERR_UNSUPPORTED_FORMAT));
    assert!(last_error().contains("VHDX"));
    let h = open(ps, 512, 1).expect("forzar RAW debe abrir");
    mr_source_close(h);
    assert_eq!(open(ps, 512, 4), Err(MR_ERR_INVALID_ARGUMENT));
}

#[test]
fn oversized_strings_are_rejected_before_reading() {
    let big = vec![b'a'; MAX_STRING_BYTES + 1];
    let mut f = 0u32;
    assert_eq!(
        unsafe { mr_image_probe(big.as_ptr(), big.len(), &mut f) },
        MR_ERR_INVALID_ARGUMENT
    );
    // Longitud absurda con puntero válido: debe rechazarse sin leer (evita comportamiento indefinido).
    assert_eq!(
        unsafe { mr_image_probe(big.as_ptr(), usize::MAX, &mut f) },
        MR_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn version_and_buffer_too_small() {
    let mut len = 0usize;
    let mut tiny = [0u8; 1];
    let rc = unsafe { mr_engine_version(tiny.as_mut_ptr(), tiny.len(), &mut len) };
    assert_eq!(rc, MR_ERR_BUFFER_TOO_SMALL);
    assert_eq!(len, mr_api::ENGINE_VERSION.len());
    let mut buf = vec![0u8; len + 1];
    let rc = unsafe { mr_engine_version(buf.as_mut_ptr(), buf.len(), &mut len) };
    assert_eq!(rc, MR_OK);
    assert_eq!(&buf[..len], mr_api::ENGINE_VERSION.as_bytes());
    assert_eq!(buf[len], 0);
}

fn last_error() -> String {
    let mut buf = vec![0u8; 1024];
    let mut len = 0usize;
    unsafe { mr_last_error(buf.as_mut_ptr(), buf.len(), &mut len) };
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

#[test]
fn device_paths_are_rejected_with_specific_code() {
    let p = r"\\.\PhysicalDrive0";
    let mut info = MrImageInfo::default();
    let rc = unsafe { mr_image_info_get(p.as_ptr(), p.len(), 512, &mut info) };
    assert_eq!(rc, MR_ERR_PATH_REJECTED);
    assert!(last_error().contains("no permitida"));
}

#[test]
fn invalid_utf8_and_null_pointers() {
    let bad = [0xffu8, 0xfe];
    let mut info = MrImageInfo::default();
    assert_eq!(
        unsafe { mr_image_info_get(bad.as_ptr(), bad.len(), 512, &mut info) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_image_info_get(std::ptr::null(), 5, 512, &mut info) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_image_info_get(b"x".as_ptr(), 1, 512, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn unknown_handles_are_errors_not_crashes() {
    let mut st = MrTaskStatus::default();
    assert_eq!(
        unsafe { mr_task_status_get(0xDEAD_BEEF, &mut st) },
        MR_ERR_INVALID_HANDLE
    );
    assert_eq!(mr_task_cancel(0xDEAD_BEEF), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_task_free(0xDEAD_BEEF), MR_ERR_INVALID_HANDLE);
}

#[test]
fn full_hash_flow_through_c_abi() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("a.img");
    std::fs::File::create(&img)
        .unwrap()
        .write_all(b"abc")
        .unwrap();
    let p = img.to_str().unwrap();

    let mut info = MrImageInfo::default();
    assert_eq!(
        unsafe { mr_image_info_get(p.as_ptr(), p.len(), 512, &mut info) },
        MR_OK
    );
    assert_eq!(
        (info.size_bytes, info.sector_count, info.trailing_bytes),
        (3, 0, 3)
    );

    let mut h = 0u64;
    let rc = unsafe { mr_hash_start(p.as_ptr(), p.len(), 512, 1 | 8, std::ptr::null(), 0, &mut h) };
    assert_eq!(rc, MR_OK);
    assert_ne!(h, 0);

    let mut st = MrTaskStatus::default();
    for _ in 0..500 {
        assert_eq!(unsafe { mr_task_status_get(h, &mut st) }, MR_OK);
        if st.state != 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(st.state, 2, "la tarea debía completarse");
    assert_eq!((st.done, st.total), (3, 3));

    let mut buf = vec![0u8; 65];
    let mut len = 0usize;
    assert_eq!(
        unsafe { mr_hash_result(h, 8, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_OK
    );
    assert_eq!(
        std::str::from_utf8(&buf[..len]).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        unsafe { mr_hash_result(h, 1, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_OK
    );
    assert_eq!(std::str::from_utf8(&buf[..len]).unwrap(), "352441c2");
    // MD5 no se pidió.
    assert_eq!(
        unsafe { mr_hash_result(h, 2, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(mr_task_free(h), MR_OK);
    assert_eq!(mr_task_free(h), MR_ERR_INVALID_HANDLE);
}

#[test]
fn rejects_bad_algorithm_mask() {
    let p = "no-importa.img";
    let mut h = 0u64;
    let rc = unsafe { mr_hash_start(p.as_ptr(), p.len(), 512, 0, std::ptr::null(), 0, &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    let rc = unsafe { mr_hash_start(p.as_ptr(), p.len(), 512, 0x10, std::ptr::null(), 0, &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
}
