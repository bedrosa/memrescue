//! Pruebas de la frontera C llamando a las funciones exportadas como lo haría un cliente.

use super::*;
use std::io::Write;
use std::thread;
use std::time::Duration;

#[test]
fn struct_layouts_match_header() {
    assert_eq!(std::mem::size_of::<MrImageInfo>(), 32);
    assert_eq!(std::mem::size_of::<MrTaskStatus>(), 24);
    assert_eq!(mr_abi_version(), 1);
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
