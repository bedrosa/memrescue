//! Pruebas de la frontera C llamando a las funciones exportadas como lo haría un cliente.

use super::*;
use std::io::Write;
use std::thread;
use std::time::Duration;

#[test]
fn struct_layouts_match_header() {
    assert_eq!(std::mem::size_of::<MrImageInfo>(), 32);
    assert_eq!(std::mem::size_of::<MrTaskStatus>(), 24);
    assert_eq!(std::mem::size_of::<MrVerifyReport>(), 72);
    assert_eq!(std::mem::size_of::<MrVerifyOptions>(), 48);
    assert_eq!(std::mem::size_of::<MrReadOptions>(), 32);
    assert_eq!(std::mem::offset_of!(MrReadOptions, bad_fill), 20);
    assert_eq!(std::mem::offset_of!(MrReadOptions, skip_fill), 24);
    assert_eq!(mr_abi_version(), 3);
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
        unsafe { mr_source_verify_start(src, 0, 0, 1, 0, 0, &mut task) },
        MR_OK
    );
    let st = wait(task);
    assert_eq!((st.state, st.done, st.total), (2, 300, 300));
    let mut rep = MrVerifyReport::default();
    assert_eq!(unsafe { mr_verify_result(task, &mut rep) }, MR_OK);
    assert_eq!(
        (
            rep.sectors_requested,
            rep.sectors_read,
            rep.sectors_recovered,
            rep.sectors_unreadable,
            rep.sectors_skipped,
            rep.sectors_sparse,
            rep.untrusted_range_count
        ),
        (300, 300, 0, 0, 0, 0, 0)
    );
    let (mut l, mut c, mut st) = (0, 0, 0);
    assert_eq!(
        unsafe { mr_verify_untrusted_range(task, 0, &mut l, &mut c, &mut st) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(
        unsafe { mr_source_verify_start(src, 0, 0, 1000, 0, 0, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_source_verify_start(src, 0, 0, 1, 0, 2, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    // Con salto y lectura hacia atrás activados (imagen sana: sin efecto).
    assert_eq!(
        unsafe { mr_source_verify_start(src, 10, 50, 0, 64, MR_VERIFY_READ_BACK, &mut task) },
        MR_OK
    );
    assert_eq!(wait(task).state, 2);
    assert_eq!(unsafe { mr_verify_result(task, &mut rep) }, MR_OK);
    assert_eq!((rep.sectors_requested, rep.sectors_read), (50, 50));
    mr_task_free(task);
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
    // Cabecera VHDX sin estructuras válidas: dañado, nunca se lee como RAW por error.
    assert_eq!(open(ps, 0, 0), Err(MR_ERR_CORRUPT));
    assert!(last_error().contains("VHDX"));
    let h = open(ps, 512, MR_OPEN_FORCE_RAW).expect("forzar RAW debe abrir");
    let mut info = MrImageInfo::default();
    assert_eq!(unsafe { mr_source_info(h, &mut info) }, MR_OK);
    assert_eq!(info.format, 1);
    assert!(describe(h).contains("forzado"));
    mr_source_close(h);
    // El bit 4 es MR_OPEN_TOLERANT: la cabecera dañada sigue siendo crítica en modo tolerante.
    assert_eq!(open(ps, 512, MR_OPEN_TOLERANT), Err(MR_ERR_CORRUPT));
    assert_eq!(open(ps, 512, 8), Err(MR_ERR_INVALID_ARGUMENT));

    // QCOW: detectado pero sin lector → formato no soportado.
    let q = dir.path().join("x.qcow2");
    let mut d = vec![0u8; 4096];
    d[..4].copy_from_slice(b"QFI\xfb");
    std::fs::write(&q, d).unwrap();
    assert_eq!(
        open(q.to_str().unwrap(), 0, 0),
        Err(MR_ERR_UNSUPPORTED_FORMAT)
    );

    // Descriptor VMDK separado: referencia externa que no se sigue.
    let v = dir.path().join("flat.vmdk");
    std::fs::write(
        &v,
        "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\n\
         createType=\"monolithicFlat\"\nRW 64 FLAT \"..\\..\\Windows\\win.ini\" 0\n",
    )
    .unwrap();
    assert_eq!(
        open(v.to_str().unwrap(), 0, 0),
        Err(MR_ERR_EXTERNAL_REFERENCE)
    );
    assert!(last_error().contains("win.ini"));
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

fn describe(h: u64) -> String {
    let mut buf = vec![0u8; 4096];
    let mut len = 0usize;
    assert_eq!(
        unsafe { mr_source_describe(h, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_OK
    );
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

#[test]
fn read_mapped_reports_a_state_per_sector() {
    let (_d, p) = temp_image("m.img", 16, 512);
    let h = open(&p, 0, 0).unwrap();
    let mut buf = vec![0u8; 4 * 512];
    let mut states = vec![0xEEu8; 4];
    let mut n = 0usize;
    let rc = unsafe {
        mr_source_read_mapped(
            h,
            12,
            4,
            1,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            states.len(),
            &mut n,
        )
    };
    assert_eq!(rc, MR_OK);
    assert_eq!(n, 2048);
    assert_eq!(states, vec![0, 0, 0, 0]);
    assert!(buf[..512].iter().all(|&b| b == 12));
    // Búfer de estados insuficiente.
    let rc = unsafe {
        mr_source_read_mapped(
            h,
            12,
            4,
            1,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            3,
            &mut n,
        )
    };
    assert_eq!(rc, MR_ERR_BUFFER_TOO_SMALL);
    // Fuera de rango y reintentos excesivos.
    let rc = unsafe {
        mr_source_read_mapped(
            h,
            14,
            4,
            1,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            4,
            &mut n,
        )
    };
    assert_eq!(rc, MR_ERR_OUT_OF_RANGE);
    let rc = unsafe {
        mr_source_read_mapped(
            h,
            0,
            1,
            101,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            4,
            &mut n,
        )
    };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    mr_source_close(h);
}

#[test]
fn range_sources_are_bounded_and_nest() {
    let (_d, p) = temp_image("r.img", 100, 512);
    let parent = open(&p, 512, 0).unwrap();
    let mut r = 0u64;
    assert_eq!(
        unsafe { mr_source_open_range(parent, 10, 20, &mut r) },
        MR_OK
    );
    let mut info = MrImageInfo::default();
    assert_eq!(unsafe { mr_source_info(r, &mut info) }, MR_OK);
    assert_eq!((info.sector_count, info.sector_size), (20, 512));
    assert!(describe(r).contains("Rango LBA 10–29"));
    let mut buf = vec![0u8; 512];
    let mut n = 0;
    assert_eq!(
        unsafe { mr_source_read(r, 0, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert!(buf.iter().all(|&b| b == 10));
    assert_eq!(
        unsafe { mr_source_read(r, 20, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_ERR_OUT_OF_RANGE
    );
    // Anidado: sector 5 del rango 5.. del rango 10.. = sector 20 del padre.
    let mut r2 = 0u64;
    assert_eq!(unsafe { mr_source_open_range(r, 5, 10, &mut r2) }, MR_OK);
    assert_eq!(
        unsafe { mr_source_read(r2, 5, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert!(buf.iter().all(|&b| b == 20));
    // Rangos no válidos.
    let mut bad = 0u64;
    for (s, c) in [(90, 11), (0, 0), (u64::MAX, 2)] {
        assert_eq!(
            unsafe { mr_source_open_range(parent, s, c, &mut bad) },
            MR_ERR_OUT_OF_RANGE,
            "{s} {c}"
        );
    }
    // Cerrar el padre no invalida los rangos abiertos.
    mr_source_close(parent);
    assert_eq!(
        unsafe { mr_source_read(r2, 0, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    mr_source_close(r2);
    mr_source_close(r);
}

#[test]
fn split_images_are_listed_then_opened_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..4096u32).map(|i| (i / 512) as u8).collect();
    std::fs::write(dir.path().join("d.001"), &data[..1536]).unwrap();
    std::fs::write(dir.path().join("d.002"), &data[1536..3072]).unwrap();
    std::fs::write(dir.path().join("d.003"), &data[3072..]).unwrap();
    let first = dir.path().join("d.001");
    let fp = first.to_str().unwrap();
    let mut buf = vec![0u8; 4096];
    let (mut len, mut count) = (0usize, 0u32);
    assert_eq!(
        unsafe {
            mr_split_discover(
                fp.as_ptr(),
                fp.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
                &mut count,
            )
        },
        MR_OK
    );
    assert_eq!(count, 3);
    let list = String::from_utf8_lossy(&buf[..len]).into_owned();
    assert_eq!(list.lines().count(), 3);
    assert!(list.lines().nth(2).unwrap().ends_with("d.003"));
    // Sin la opción, solo se abre el primer segmento.
    let one = open(fp, 0, 0).unwrap();
    let mut info = MrImageInfo::default();
    unsafe { mr_source_info(one, &mut info) };
    assert_eq!(info.size_bytes, 1536);
    mr_source_close(one);
    let h = open(fp, 0, MR_OPEN_SPLIT).unwrap();
    unsafe { mr_source_info(h, &mut info) };
    assert_eq!((info.size_bytes, info.sector_count), (4096, 8));
    assert!(describe(h).contains("3 segmentos"));
    let mut n = 0;
    let mut s = vec![0u8; 1024];
    assert_eq!(
        unsafe { mr_source_read(h, 2, 2, s.as_mut_ptr(), s.len(), &mut n) },
        MR_OK
    );
    assert_eq!(&s[..], &data[1024..2048]);
    mr_source_close(h);
    // Un nombre que no es primer segmento.
    let second = dir.path().join("d.002");
    let sp = second.to_str().unwrap();
    assert_eq!(open(sp, 0, MR_OPEN_SPLIT), Err(MR_ERR_UNSUPPORTED_FEATURE));
}

// ---------- Verificación extendida: política de omisión de errores (IO-09) ----------

fn verify_options(flags: u32, retries: u32, codes: &[i32]) -> MrVerifyOptions {
    MrVerifyOptions {
        struct_size: std::mem::size_of::<MrVerifyOptions>() as u32,
        flags,
        start_lba: 0,
        count: 0,
        jump_sectors: 0,
        retries,
        skip_code_count: codes.len() as u32,
        skip_codes: if codes.is_empty() {
            std::ptr::null()
        } else {
            codes.as_ptr()
        },
    }
}

/// Origen con fallos simulados registrado como cualquier otro (sin pasar por un fichero).
fn faulty_source() -> u64 {
    let dev = mr_io::FaultyDevice::new(mr_io::MemDevice::new(vec![0x5Au8; 512 * 64], 512).unwrap())
        .with_bad_code([10], 23)
        .with_bad_code([20], 1117)
        .with_bad([30]);
    insert(Entry::Source(mr_api::source_from_device(
        Arc::new(dev),
        "fallos simulados",
    )))
}

fn run_verify_ex(src: u64, o: &MrVerifyOptions) -> (u64, MrVerifyReport) {
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_source_verify_start_ex(src, o, &mut task) },
        MR_OK,
        "{}",
        last_error()
    );
    assert_eq!(wait(task).state, 2);
    let mut rep = MrVerifyReport::default();
    assert_eq!(unsafe { mr_verify_result(task, &mut rep) }, MR_OK);
    (task, rep)
}

fn error_codes(task: u64) -> (u64, Vec<(i32, u64)>) {
    let (mut omitted, mut n) = (0u64, 0u32);
    assert_eq!(
        unsafe { mr_verify_error_summary(task, &mut omitted, &mut n) },
        MR_OK
    );
    let mut v = Vec::new();
    for i in 0..n {
        let (mut c, mut k) = (0i32, 0u64);
        assert_eq!(
            unsafe { mr_verify_error_code(task, i, &mut c, &mut k) },
            MR_OK
        );
        v.push((c, k));
    }
    let (mut c, mut k) = (0i32, 0u64);
    assert_eq!(
        unsafe { mr_verify_error_code(task, n, &mut c, &mut k) },
        MR_ERR_OUT_OF_RANGE
    );
    (omitted, v)
}

#[test]
fn verify_ex_applies_skip_codes_and_ignore_all() {
    let src = faulty_source();
    // Sin política: todos los ilegibles se reintentan (un bloque de 64 + 2 reintentos × 3 sectores).
    let (task, rep) = run_verify_ex(src, &verify_options(0, 2, &[]));
    assert_eq!((rep.sectors_unreadable, rep.retry_reads), (3, 1 + 6));
    assert_eq!(error_codes(task), (0, vec![(23, 1), (1117, 1)]));
    mr_task_free(task);
    // Códigos 23 y 1117 omitidos (con duplicados en la lista): solo el 30 se reintenta.
    let codes = [1117, 23, 23];
    let (task, rep) = run_verify_ex(src, &verify_options(0, 2, &codes));
    assert_eq!(
        (
            rep.sectors_read,
            rep.sectors_unreadable,
            rep.sectors_recovered
        ),
        (61, 3, 0)
    );
    assert_eq!(rep.retry_reads, 1 + 2);
    assert_eq!(error_codes(task), (2, vec![(23, 1), (1117, 1)]));
    // Los tramos no fiables son los tres ilegibles, nunca datos.
    assert_eq!(rep.untrusted_range_count, 3);
    mr_task_free(task);
    // Ignorar todo, con salto y lectura hacia atrás.
    let mut o = verify_options(MR_VERIFY_IGNORE_ALL_ERRORS | MR_VERIFY_READ_BACK, 5, &[]);
    o.jump_sectors = 4;
    let (task, rep) = run_verify_ex(src, &o);
    assert_eq!(rep.sectors_unreadable, 3);
    assert_eq!(rep.sectors_read + rep.sectors_unreadable, 64);
    // Sin reintentos: solo las relecturas sector a sector de los 3 bloques que fallaron.
    assert_eq!(rep.retry_reads, 3);
    assert_eq!(error_codes(task).0, 3);
    mr_task_free(task);
    // Rango acotado que solo contiene el 20.
    let mut o = verify_options(0, 3, &[1117]);
    o.start_lba = 15;
    o.count = 10;
    let (task, rep) = run_verify_ex(src, &o);
    assert_eq!((rep.sectors_requested, rep.sectors_unreadable), (10, 1));
    assert_eq!(error_codes(task), (1, vec![(1117, 1)]));
    mr_task_free(task);
    mr_source_close(src);
}

#[test]
fn verify_ex_validates_its_options() {
    let (_d, p) = temp_image("x.img", 32, 512);
    let src = open(&p, 512, 0).unwrap();
    let mut task = 0u64;
    let bad = |o: &MrVerifyOptions| {
        let mut t = 0u64;
        let rc = unsafe { mr_source_verify_start_ex(src, o, &mut t) };
        assert_eq!(t, 0);
        rc
    };
    let mut o = verify_options(0, 1, &[]);
    o.struct_size = 40;
    assert_eq!(bad(&o), MR_ERR_INVALID_ARGUMENT);
    assert!(last_error().contains("mr_verify_options"));
    o.struct_size = 56;
    assert_eq!(bad(&o), MR_ERR_INVALID_ARGUMENT);
    assert_eq!(bad(&verify_options(4, 1, &[])), MR_ERR_INVALID_ARGUMENT);
    assert_eq!(bad(&verify_options(0, 101, &[])), MR_ERR_INVALID_ARGUMENT);
    assert_eq!(
        bad(&verify_options(0, 1, &[23, 0])),
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(bad(&verify_options(0, 1, &[-5])), MR_ERR_INVALID_ARGUMENT);
    let many = vec![23i32; 257];
    assert_eq!(bad(&verify_options(0, 1, &many)), MR_ERR_INVALID_ARGUMENT);
    let mut o = verify_options(0, 1, &[]);
    o.skip_code_count = 2;
    assert_eq!(bad(&o), MR_ERR_INVALID_ARGUMENT);
    // Punteros nulos y handle inválido.
    let o = verify_options(0, 1, &[23]);
    assert_eq!(
        unsafe { mr_source_verify_start_ex(src, std::ptr::null(), &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_source_verify_start_ex(src, &o, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_source_verify_start_ex(999_999, &o, &mut task) },
        MR_ERR_INVALID_HANDLE
    );
    // Rango fuera del origen: la tarea falla con la posición.
    let mut o = verify_options(0, 1, &[23]);
    o.start_lba = 30;
    o.count = 5;
    assert_eq!(
        unsafe { mr_source_verify_start_ex(src, &o, &mut task) },
        MR_OK
    );
    assert_eq!(wait(task).state, 3);
    mr_task_free(task);
    // Imagen sana con política activada: todo leído; 256 códigos es el máximo admitido.
    let many = vec![23i32; 256];
    let (t, rep) = run_verify_ex(src, &verify_options(MR_VERIFY_IGNORE_ALL_ERRORS, 1, &many));
    assert_eq!((rep.sectors_read, rep.retry_reads), (32, 0));
    assert_eq!(error_codes(t), (0, vec![]));
    // El resumen de errores rechaza punteros nulos y tareas de otro tipo.
    let mut n = 0u32;
    assert_eq!(
        unsafe { mr_verify_error_summary(t, std::ptr::null_mut(), &mut n) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_verify_error_code(src, 0, &mut 0, &mut 0) },
        MR_ERR_INVALID_HANDLE
    );
    mr_task_free(t);
    // La función anterior sigue rechazando el indicador nuevo (su contrato no cambia).
    assert_eq!(
        unsafe { mr_source_verify_start(src, 0, 0, 1, 0, MR_VERIFY_IGNORE_ALL_ERRORS, &mut task) },
        MR_ERR_INVALID_ARGUMENT
    );
    mr_source_close(src);
}

// ---------- Modo tolerante de contenedores (MR_OPEN_TOLERANT, ADR-020) ----------

/// Suma de control del VHD: complemento a uno de la suma de los bytes, sin el propio campo.
fn vhd_seal(b: &mut [u8], field: usize) {
    let sum = b
        .iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |a, (_, &x)| a.wrapping_add(u32::from(x)));
    b[field..field + 4].copy_from_slice(&(!sum).to_be_bytes());
}

/// VHD dinámico de 4 bloques de 4 KiB (0, 1 y 2 asignados) con la entrada de la BAT del bloque 1
/// fuera del fichero. Cada byte de datos del bloque `b` vale `0x10 + b`.
fn damaged_dynamic_vhd() -> Vec<u8> {
    let size = 4 * 4096u64;
    let mut footer = vec![0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&512u64.to_be_bytes());
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[60..64].copy_from_slice(&3u32.to_be_bytes());
    vhd_seal(&mut footer, 64);
    let mut header = vec![0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    header[16..24].copy_from_slice(&1536u64.to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&4u32.to_be_bytes());
    header[32..36].copy_from_slice(&4096u32.to_be_bytes());
    vhd_seal(&mut header, 36);
    let mut bat = vec![0xFFu8; 512];
    let mut body = Vec::new();
    for b in 0..3usize {
        let sector = ((2048 + body.len()) / 512) as u32;
        let entry = if b == 1 { 0x00FF_0000 } else { sector };
        bat[b * 4..b * 4 + 4].copy_from_slice(&entry.to_be_bytes());
        let mut bitmap = vec![0u8; 512];
        bitmap[0] = 0xFF;
        body.extend_from_slice(&bitmap);
        body.extend(std::iter::repeat_n(0x10 + b as u8, 4096));
    }
    let mut img = footer.clone();
    img.extend_from_slice(&header);
    img.extend_from_slice(&bat);
    img.extend_from_slice(&body);
    img.extend_from_slice(&footer);
    img
}

#[test]
fn tolerant_flag_opens_damaged_containers_with_untrusted_blocks() {
    assert_eq!(MR_OPEN_TOLERANT, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.vhd");
    let bytes = damaged_dynamic_vhd();
    std::fs::write(&path, &bytes).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let p = path.to_str().unwrap();

    // Sin el bit: estricto, dañado.
    assert_eq!(open(p, 0, 0), Err(MR_ERR_CORRUPT));
    assert!(last_error().contains("VHD"));
    // Bits desconocidos: siguen rechazándose.
    assert_eq!(open(p, 0, 8), Err(MR_ERR_INVALID_ARGUMENT));

    let h = open(p, 0, MR_OPEN_TOLERANT).unwrap();
    let d = describe(h);
    assert!(d.contains("modo tolerante: 1 bloque descartado"), "{d}");
    // Lectura con estado: los sectores 8 a 15 (bloque 1) son ilegibles con relleno.
    let mut buf = vec![0u8; 32 * 512];
    let mut states = vec![0xEEu8; 32];
    let mut n = 0usize;
    let rc = unsafe {
        mr_source_read_mapped(
            h,
            0,
            32,
            0,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            states.len(),
            &mut n,
        )
    };
    assert_eq!(rc, MR_OK);
    for (l, &st) in states.iter().enumerate() {
        let sector = &buf[l * 512..(l + 1) * 512];
        match l {
            8..16 => {
                assert_eq!(st, 2, "LBA {l}");
                assert!(sector.chunks(4).all(|c| c == b"BAD!"));
            }
            24..32 => {
                assert_eq!(st, 4, "LBA {l}");
                assert!(sector.iter().all(|&b| b == 0));
            }
            _ => {
                assert_eq!(st, 0, "LBA {l}");
                assert!(sector.iter().all(|&b| b == 0x10 + (l / 8) as u8));
            }
        }
    }
    // Lectura estricta del bloque descartado: error de E/S, nunca relleno.
    let mut one = vec![0u8; 512];
    let rc = unsafe { mr_source_read(h, 8, 1, one.as_mut_ptr(), one.len(), &mut n) };
    assert_eq!(rc, MR_ERR_IO);
    assert_eq!(mr_source_close(h), MR_OK);

    // Con RAW forzado el bit no tiene efecto.
    let h = open(p, 512, MR_OPEN_TOLERANT | MR_OPEN_FORCE_RAW).unwrap();
    assert!(!describe(h).contains("tolerante"));
    assert_eq!(mr_source_close(h), MR_OK);

    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

// ---------- Lectura con patrones de relleno configurables (IO-08) ----------

fn read_options(retries: u32, jump: u64, bad: [u8; 4], skip: [u8; 4]) -> MrReadOptions {
    MrReadOptions {
        struct_size: std::mem::size_of::<MrReadOptions>() as u32,
        flags: 0,
        jump_sectors: jump,
        retries,
        bad_fill: bad,
        skip_fill: skip,
        reserved: 0,
    }
}

fn read_ex(src: u64, lba: u64, count: u32, o: &MrReadOptions) -> Result<(Vec<u8>, Vec<u8>), i32> {
    let mut buf = vec![0u8; count as usize * 512];
    let mut states = vec![0xEEu8; count as usize];
    let mut n = 0usize;
    let rc = unsafe {
        mr_source_read_mapped_ex(
            src,
            lba,
            count,
            o,
            buf.as_mut_ptr(),
            buf.len(),
            states.as_mut_ptr(),
            states.len(),
            &mut n,
        )
    };
    if rc == MR_OK {
        assert_eq!(n, buf.len());
        Ok((buf, states))
    } else {
        Err(rc)
    }
}

#[test]
fn read_mapped_ex_uses_the_configured_fill_patterns() {
    // Sectores 10, 20 y 30 ilegibles; el resto vale 0x5A.
    let src = faulty_source();
    let bad = [0xDE, 0xAD, 0xBE, 0xEF];
    let skip = *b"skp?";
    let (buf, states) = read_ex(src, 8, 16, &read_options(0, 3, bad, skip)).unwrap();
    // 10 ilegible, 11–13 saltados, 14–23 con 20 ilegible y 21–23 saltados.
    let expected: Vec<u8> = (8..24u64)
        .map(|l| match l {
            10 | 20 => 2,
            11..=13 | 21..=23 => 3,
            _ => 0,
        })
        .collect();
    assert_eq!(states, expected);
    for (i, &st) in states.iter().enumerate() {
        let sec = &buf[i * 512..(i + 1) * 512];
        match st {
            2 => assert!(sec.chunks(4).all(|c| c == bad)),
            3 => assert!(sec.chunks(4).all(|c| c == skip)),
            _ => assert!(sec.iter().all(|&b| b == 0x5A)),
        }
    }
    // Lectura hacia atrás: los saltados legibles pasan a leídos.
    let mut o = read_options(0, 3, bad, skip);
    o.flags = MR_READ_READ_BACK;
    let (_, states) = read_ex(src, 8, 16, &o).unwrap();
    assert!(states.iter().all(|&s| s != 3), "{states:?}");
    // Los valores por defecto de la función nueva coinciden con mr_source_read_mapped.
    let (b1, s1) = read_ex(src, 8, 4, &read_options(1, 0, *b"BAD!", *b"SKIP")).unwrap();
    let mut b2 = vec![0u8; 4 * 512];
    let mut s2 = vec![0u8; 4];
    let mut n = 0usize;
    let rc = unsafe {
        mr_source_read_mapped(
            src,
            8,
            4,
            1,
            b2.as_mut_ptr(),
            b2.len(),
            s2.as_mut_ptr(),
            s2.len(),
            &mut n,
        )
    };
    assert_eq!(rc, MR_OK);
    assert_eq!((b1, s1), (b2, s2));
    // El hash del origen sigue siendo estricto: nunca se calcula sobre relleno.
    let mut task = 0u64;
    assert_eq!(
        unsafe { mr_source_hash_start(src, 8, std::ptr::null(), 0, &mut task) },
        MR_OK
    );
    assert_eq!(wait(task).state, 3);
    assert_eq!(mr_task_free(task), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn read_mapped_ex_rejects_invalid_options() {
    let src = faulty_source();
    let ok = read_options(1, 0, *b"BAD!", *b"SKIP");
    let cases: Vec<(MrReadOptions, &str)> = vec![
        (read_options(1, 0, [0; 4], *b"SKIP"), "00 00 00 00"),
        (read_options(1, 0, *b"BAD!", [0; 4]), "saltados"),
        (
            read_options(1, 0, [0xFF; 4], *b"SKIP"),
            "cuatro bytes iguales",
        ),
        (read_options(1, 0, *b"SAME", *b"SAME"), "distintos"),
        (read_options(101, 0, *b"BAD!", *b"SKIP"), "reintentos"),
        (MrReadOptions { flags: 2, ..ok }, "opciones"),
        (MrReadOptions { reserved: 1, ..ok }, "opciones"),
        (
            MrReadOptions {
                struct_size: 24,
                ..ok
            },
            "tamaño",
        ),
        (
            MrReadOptions {
                struct_size: 40,
                ..ok
            },
            "tamaño",
        ),
    ];
    for (o, msg) in cases {
        assert_eq!(read_ex(src, 0, 1, &o), Err(MR_ERR_INVALID_ARGUMENT));
        assert!(last_error().contains(msg), "{msg}: {}", last_error());
    }
    // Puntero nulo, handle inválido, fuera de rango y búfer pequeño.
    let mut buf = vec![0u8; 512];
    let mut st = vec![0u8; 1];
    let mut n = 0usize;
    let (bp, sp) = (buf.as_mut_ptr(), st.as_mut_ptr());
    let call = |h: u64, o: *const MrReadOptions, lba: u64, cap: usize, n: &mut usize| unsafe {
        mr_source_read_mapped_ex(h, lba, 1, o, bp, cap.min(512), sp, 1, n)
    };
    assert_eq!(
        call(src, std::ptr::null(), 0, 512, &mut n),
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(call(999_999, &ok, 0, 512, &mut n), MR_ERR_INVALID_HANDLE);
    assert_eq!(call(src, &ok, 64, 512, &mut n), MR_ERR_OUT_OF_RANGE);
    assert_eq!(call(src, &ok, 0, 100, &mut n), MR_ERR_BUFFER_TOO_SMALL);
    assert_eq!(n, 512);
    assert_eq!(mr_source_close(src), MR_OK);
}
