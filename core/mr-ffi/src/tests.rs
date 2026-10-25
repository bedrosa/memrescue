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

#[test]
fn untrusted_ranges_are_indexed_in_constant_time() {
    // 10 000 tramos ilegibles alternos: recorrer todos los índices debe ser lineal en total (antes
    // cada consulta recorría el mapa desde el principio bajo el bloqueo global).
    const N: u64 = 10_000;
    let dev = mr_io::FaultyDevice::new(
        mr_io::MemDevice::new(vec![0x11u8; 512 * 2 * N as usize], 512).unwrap(),
    )
    .with_bad((0..N).map(|i| 2 * i + 1));
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(dev),
        "fallos alternos",
    )));
    let (task, rep) = run_verify_ex(src, &verify_options(0, 0, &[]));
    assert_eq!(rep.untrusted_range_count, N);
    let started = std::time::Instant::now();
    for i in 0..N {
        let (mut l, mut c, mut st) = (0u64, 0u64, 0u32);
        assert_eq!(
            unsafe { mr_verify_untrusted_range(task, i, &mut l, &mut c, &mut st) },
            MR_OK
        );
        assert_eq!((l, c, st), (2 * i + 1, 1, 2));
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    let (mut l, mut c, mut st) = (0u64, 0u64, 0u32);
    assert_eq!(
        unsafe { mr_verify_untrusted_range(task, N, &mut l, &mut c, &mut st) },
        MR_ERR_OUT_OF_RANGE
    );
    mr_task_free(task);
    mr_source_close(src);
}

// ---------- Particiones (issue #12) ----------

/// Disco de 4096 sectores: P1 activa [2048, +1024) tipo 0x07, P2 tipo 0x7F (desconocido) que termina
/// fuera del disco y extendida 0x0F en [3600, +496) con una lógica Linux. Cada sector vale su LBA
/// módulo 256 salvo las tablas.
fn partitioned_disk() -> Vec<u8> {
    let mut d: Vec<u8> = (0..4096 * 512).map(|i| (i / 512) as u8).collect();
    let mut table = |lba: usize, entries: &[(u8, u8, u32, u32)]| {
        let s = &mut d[lba * 512..(lba + 1) * 512];
        s.fill(0);
        for (i, &(boot, kind, start, count)) in entries.iter().enumerate() {
            let e = &mut s[0x1BE + 16 * i..0x1BE + 16 * (i + 1)];
            e[0] = boot;
            e[4] = kind;
            e[8..12].copy_from_slice(&start.to_le_bytes());
            e[12..16].copy_from_slice(&count.to_le_bytes());
        }
        s[510] = 0x55;
        s[511] = 0xAA;
    };
    table(
        0,
        &[
            (0x80, 0x07, 2048, 1024),
            (0, 0x7F, 3072, 2000),
            (0, 0x0F, 3600, 496),
        ],
    );
    table(3600, &[(0, 0x83, 8, 100)]);
    d
}

fn table_info(t: u64) -> Result<MrPartitionTableInfo, i32> {
    let mut i = MrPartitionTableInfo {
        struct_size: std::mem::size_of::<MrPartitionTableInfo>() as u32,
        ..Default::default()
    };
    match unsafe { mr_partitions_info(t, &mut i) } {
        MR_OK => Ok(i),
        rc => Err(rc),
    }
}

fn partition(t: u64, index: u32) -> Result<MrPartitionInfo, i32> {
    let mut p = MrPartitionInfo {
        struct_size: std::mem::size_of::<MrPartitionInfo>() as u32,
        ..Default::default()
    };
    match unsafe { mr_partition_get(t, index, &mut p) } {
        MR_OK => Ok(p),
        rc => Err(rc),
    }
}

fn anomaly(t: u64, part: u32, index: u32) -> Result<(MrPartitionAnomaly, String), i32> {
    let mut a = MrPartitionAnomaly {
        struct_size: std::mem::size_of::<MrPartitionAnomaly>() as u32,
        ..Default::default()
    };
    let rc = unsafe { mr_partition_anomaly(t, part, index, &mut a) };
    if rc != MR_OK {
        return Err(rc);
    }
    let mut buf = vec![0u8; 1024];
    let mut n = 0usize;
    let rc =
        unsafe { mr_partition_anomaly_text(t, part, index, buf.as_mut_ptr(), buf.len(), &mut n) };
    assert_eq!(rc, MR_OK);
    Ok((a, String::from_utf8(buf[..n].to_vec()).unwrap()))
}

#[test]
fn partition_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrPartitionTableInfo>(), 80);
    assert_eq!(std::mem::size_of::<MrPartitionInfo>(), 96);
    assert_eq!(std::mem::offset_of!(MrPartitionInfo, ebr_lba), 16);
    assert_eq!(std::mem::offset_of!(MrPartitionInfo, active), 40);
    assert_eq!(std::mem::offset_of!(MrPartitionInfo, gpt_type), 56);
    assert_eq!(std::mem::offset_of!(MrPartitionInfo, unique_guid), 72);
    assert_eq!(std::mem::offset_of!(MrPartitionInfo, attributes), 88);
    assert_eq!(std::mem::offset_of!(MrPartitionTableInfo, shift), 40);
    assert_eq!(std::mem::offset_of!(MrPartitionTableInfo, disk_guid), 64);
    assert_eq!(std::mem::size_of::<MrPartitionAnomaly>(), 40);
    assert_eq!(std::mem::offset_of!(MrPartitionAnomaly, lba), 16);
}

#[test]
fn partitions_are_enumerated_by_index_and_open_as_sources() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("particiones.img");
    std::fs::write(&path, partitioned_disk()).unwrap();
    let src = open(path.to_str().unwrap(), 512, 0).unwrap();
    let mut t = 0u64;
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    let info = table_info(t).unwrap();
    assert_eq!(
        (info.scheme, info.sector_size, info.partition_count),
        (MR_SCHEME_MBR, 512, 3)
    );
    assert_eq!(info.confidence, MR_CONFIDENCE_SUSPICIOUS);
    assert_eq!(
        info.anomaly_count, 2,
        "solapes de P2 con la extendida y con la lógica"
    );

    let p0 = partition(t, 0).unwrap();
    assert_eq!(
        (
            p0.origin,
            p0.slot,
            p0.mbr_type,
            p0.start_lba,
            p0.sector_count,
            p0.active
        ),
        (MR_ORIGIN_PRIMARY, 0, 0x07, 2048, 1024, 1)
    );
    assert_eq!((p0.confidence, p0.anomaly_count), (MR_CONFIDENCE_VALID, 0));
    let p1 = partition(t, 1).unwrap();
    assert_eq!(
        (p1.mbr_type, p1.confidence, p1.anomaly_count),
        (0x7F, MR_CONFIDENCE_SUSPICIOUS, 2)
    );
    let p2 = partition(t, 2).unwrap();
    assert_eq!(
        (
            p2.origin,
            p2.ebr_lba,
            p2.start_lba,
            p2.sector_count,
            p2.active
        ),
        (MR_ORIGIN_LOGICAL, 3600, 3608, 100, 0)
    );
    assert_eq!(partition(t, 3).unwrap_err(), MR_ERR_OUT_OF_RANGE);

    // Nombres de tipo (vacío si es desconocido) con el protocolo de búfer pequeño.
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_partition_type_name(t, 0, std::ptr::null_mut(), 0, &mut n) },
        MR_ERR_BUFFER_TOO_SMALL
    );
    let mut buf = vec![0u8; n + 1];
    assert_eq!(
        unsafe { mr_partition_type_name(t, 0, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert_eq!(&buf[..n], "NTFS / exFAT / HPFS".as_bytes());
    assert_eq!(
        unsafe { mr_partition_type_name(t, 1, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert_eq!(n, 0);

    // Anomalías de la partición 1 y de la tabla.
    let (a, text) = anomaly(t, 1, 0).unwrap();
    assert_eq!(
        (a.code, a.severity, a.lba),
        (107, MR_SEVERITY_SUSPICIOUS, 0)
    );
    assert_eq!((a.flags, a.value), (MR_ANOMALY_HAS_VALUE, 5072));
    assert!(text.starts_with("mbr_partially_outside_disk\n"), "{text}");
    let (a, _) = anomaly(t, 1, 1).unwrap();
    assert_eq!((a.code, a.value), (109, 0x7F));
    let (a, text) = anomaly(t, MR_PARTITION_TABLE, 0).unwrap();
    assert_eq!(a.code, 108);
    assert!(
        text.starts_with("mbr_overlap\n") && text.contains("se solapa"),
        "{text}"
    );
    assert_eq!(anomaly(t, 1, 2).unwrap_err(), MR_ERR_OUT_OF_RANGE);
    assert_eq!(anomaly(t, 9, 0).unwrap_err(), MR_ERR_OUT_OF_RANGE);

    // Abrir particiones: la recortada se abre hasta el final del origen.
    let mut ps = 0u64;
    assert_eq!(unsafe { mr_partition_open(t, 0, &mut ps) }, MR_OK);
    let mut pi = MrImageInfo::default();
    assert_eq!(unsafe { mr_source_info(ps, &mut pi) }, MR_OK);
    assert_eq!(pi.sector_count, 1024);
    assert!(describe(ps).starts_with("Partición LBA 2048–3071 (tipo 0x07)"));
    let mut sector = vec![0u8; 512];
    assert_eq!(
        unsafe { mr_source_read(ps, 1, 1, sector.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert!(sector.iter().all(|&b| b == (2049 % 256) as u8));
    let mut trimmed = 0u64;
    assert_eq!(unsafe { mr_partition_open(t, 1, &mut trimmed) }, MR_OK);
    assert_eq!(unsafe { mr_source_info(trimmed, &mut pi) }, MR_OK);
    assert_eq!(pi.sector_count, 1024);
    assert!(describe(trimmed).contains("recortada"));
    assert_eq!(
        unsafe { mr_partition_open(t, 7, &mut trimmed) },
        MR_ERR_OUT_OF_RANGE
    );

    // La tabla sobrevive al cierre del origen y las particiones abiertas, a la tabla.
    assert_eq!(mr_source_close(src), MR_OK);
    assert!(table_info(t).is_ok());
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_partitions_free(t), MR_ERR_INVALID_HANDLE);
    assert_eq!(table_info(t).unwrap_err(), MR_ERR_INVALID_HANDLE);
    assert_eq!(
        unsafe { mr_source_read(ps, 0, 1, sector.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert_eq!(mr_source_close(ps), MR_OK);
    assert_eq!(mr_source_close(trimmed), MR_OK);
}

#[test]
fn partition_calls_validate_handles_sizes_and_pointers() {
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(mr_io::MemDevice::new(vec![0u8; 512 * 8], 512).unwrap()),
        "vacío",
    )));
    let mut t = 0u64;
    assert_eq!(
        unsafe { mr_source_partitions(src, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    let info = table_info(t).unwrap();
    assert_eq!(
        (
            info.scheme,
            info.partition_count,
            info.anomaly_count,
            info.confidence
        ),
        (MR_SCHEME_NONE, 0, 1, MR_CONFIDENCE_VALID)
    );
    let (a, text) = anomaly(t, MR_PARTITION_TABLE, 0).unwrap();
    assert_eq!((a.code, a.severity), (100, MR_SEVERITY_NOTICE));
    assert!(text.starts_with("mbr_missing\n"));
    // struct_size distinto del esperado, punteros nulos y handles de otro tipo.
    let mut i = MrPartitionTableInfo {
        struct_size: 24,
        ..Default::default()
    };
    assert_eq!(
        unsafe { mr_partitions_info(t, &mut i) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert!(last_error().contains("mr_partition_table_info"));
    assert_eq!(
        unsafe { mr_partitions_info(t, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    let mut p = MrPartitionInfo::default();
    assert_eq!(
        unsafe { mr_partition_get(t, 0, &mut p) },
        MR_ERR_INVALID_ARGUMENT
    );
    let mut a = MrPartitionAnomaly {
        struct_size: 41,
        ..Default::default()
    };
    assert_eq!(
        unsafe { mr_partition_anomaly(t, MR_PARTITION_TABLE, 0, &mut a) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(table_info(src).unwrap_err(), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_partitions_free(src), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_source_close(t), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_task_free(t), MR_ERR_INVALID_HANDLE);
    assert_eq!(mr_task_cancel(t), MR_ERR_INVALID_HANDLE);
    let mut st = MrTaskStatus::default();
    assert_eq!(
        unsafe { mr_task_status_get(t, &mut st) },
        MR_ERR_INVALID_HANDLE
    );
    assert_eq!(
        unsafe { mr_source_partitions(t, &mut t) },
        MR_ERR_INVALID_HANDLE
    );
    let mut out = 0u64;
    assert_eq!(
        unsafe { mr_partition_open(t, 0, &mut out) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(
        unsafe { mr_partition_open(t, 0, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn unreadable_mbr_is_an_anomaly_not_an_error() {
    let dev = mr_io::FaultyDevice::new(mr_io::MemDevice::new(partitioned_disk(), 512).unwrap())
        .with_bad([3600]);
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(dev),
        "EBR ilegible",
    )));
    let mut t = 0u64;
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    let info = table_info(t).unwrap();
    assert_eq!(
        info.partition_count, 2,
        "la lógica del EBR ilegible no aparece"
    );
    let codes: Vec<u32> = (0..info.anomaly_count as u32)
        .map(|i| anomaly(t, MR_PARTITION_TABLE, i).unwrap().0.code)
        .collect();
    assert!(codes.contains(&1), "{codes:?}");
    let (a, _) = anomaly(
        t,
        MR_PARTITION_TABLE,
        codes.iter().position(|&c| c == 1).unwrap() as u32,
    )
    .unwrap();
    assert_eq!(a.lba, 3600);
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

// ---------- GPT integrada en la tabla (issue #12 sobre #13) ----------

/// GUID «Datos básicos de Microsoft» en el orden del disco.
const BASIC_DATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

/// Disco GPT de 256 sectores de 512 B según UEFI: MBR protector, cabeceras y tablas primaria y de
/// respaldo, una partición «DATOS» en [64, 128). Cada sector fuera de las estructuras vale su LBA
/// del disco (mod 256). Con `cut` > 0 se quitan los `cut` primeros sectores (imagen desplazada).
fn gpt_disk(cut: usize) -> Vec<u8> {
    const TOTAL: u64 = 256;
    let mut d: Vec<u8> = (0..TOTAL as usize * 512).map(|i| (i / 512) as u8).collect();
    let mut entries = vec![0u8; 128 * 128];
    entries[..16].copy_from_slice(&BASIC_DATA);
    entries[16..32].copy_from_slice(&[0x5A; 16]);
    entries[32..40].copy_from_slice(&64u64.to_le_bytes());
    entries[40..48].copy_from_slice(&127u64.to_le_bytes());
    for (i, c) in "DATOS".encode_utf16().enumerate() {
        entries[56 + 2 * i..58 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    let ecrc = mr_common::hash::crc32(&entries);
    let header = |my: u64, alt: u64, at: u64| {
        let mut h = vec![0u8; 512];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(TOTAL - 34).to_le_bytes());
        h[56..72].copy_from_slice(&[0x77; 16]);
        h[72..80].copy_from_slice(&at.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        let crc = mr_common::hash::crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    d[..512].fill(0);
    let e = &mut d[0x1BE..0x1CE];
    e[4] = 0xEE;
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&((TOTAL - 1) as u32).to_le_bytes());
    d[510] = 0x55;
    d[511] = 0xAA;
    d[512..1024].copy_from_slice(&header(1, TOTAL - 1, 2));
    d[1024..1024 + entries.len()].copy_from_slice(&entries);
    let back = (TOTAL as usize - 33) * 512;
    d[back..back + entries.len()].copy_from_slice(&entries);
    d[(TOTAL as usize - 1) * 512..].copy_from_slice(&header(TOTAL - 1, 1, TOTAL - 33));
    d.split_off(cut * 512)
}

fn gpt_table(cut: usize) -> (u64, u64) {
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(mr_io::MemDevice::new(gpt_disk(cut), 512).unwrap()),
        "gpt",
    )));
    let mut t = 0u64;
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    (src, t)
}

fn partition_name(t: u64, index: u32) -> String {
    let mut buf = vec![0u8; 256];
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_partition_name(t, index, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    String::from_utf8(buf[..n].to_vec()).unwrap()
}

#[test]
fn gpt_partitions_are_enumerated_with_copy_shift_and_names() {
    let (src, t) = gpt_table(0);
    let info = table_info(t).unwrap();
    assert_eq!(
        (
            info.scheme,
            info.partition_count,
            info.anomaly_count,
            info.confidence
        ),
        (MR_SCHEME_GPT, 1, 0, MR_CONFIDENCE_VALID)
    );
    assert_eq!(
        (
            info.gpt_selected,
            info.gpt_flags,
            info.shift,
            info.gpt_sector_size
        ),
        (MR_GPT_PRIMARY, MR_GPT_PRESENT, 0, 512)
    );
    assert_eq!(info.disk_guid, [0x77; 16]);
    let p = partition(t, 0).unwrap();
    assert_eq!(
        (p.origin, p.slot, p.start_lba, p.sector_count, p.mbr_type),
        (MR_ORIGIN_GPT, 0, 64, 64, 0)
    );
    assert_eq!((p.gpt_type, p.unique_guid), (BASIC_DATA, [0x5A; 16]));
    assert_eq!(partition_name(t, 0), "DATOS");
    let mut buf = vec![0u8; 128];
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_partition_type_name(t, 0, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert_eq!(&buf[..n], "Datos básicos de Microsoft".as_bytes());
    let mut ps = 0u64;
    assert_eq!(unsafe { mr_partition_open(t, 0, &mut ps) }, MR_OK);
    let mut sector = vec![0u8; 512];
    assert_eq!(
        unsafe { mr_source_read(ps, 0, 1, sector.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert!(sector.iter().all(|&b| b == 64));
    assert_eq!(mr_source_close(ps), MR_OK);
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn shifted_gpt_partitions_open_only_when_the_shift_is_requested() {
    let (src, t) = gpt_table(3);
    let info = table_info(t).unwrap();
    assert_eq!(
        (info.scheme, info.gpt_selected, info.shift),
        (MR_SCHEME_GPT, MR_GPT_BACKUP, 3)
    );
    let codes: Vec<(u32, u32, u64)> = (0..info.anomaly_count as u32)
        .map(|i| {
            let (a, _) = anomaly(t, MR_PARTITION_TABLE, i).unwrap();
            (a.code, a.flags, a.value)
        })
        .collect();
    assert!(codes.contains(&(100, 0, 0)), "LBA 0 sin MBR: {codes:?}");
    assert!(
        codes.contains(&(232, MR_ANOMALY_HAS_VALUE | MR_ANOMALY_VALUE_SIGNED, 3)),
        "{codes:?}"
    );
    assert!(
        codes.contains(&(230, 0, 0)),
        "copia de respaldo en uso: {codes:?}"
    );
    let p = partition(t, 0).unwrap();
    assert_eq!(
        p.start_lba, 64,
        "las particiones conservan la LBA del disco"
    );
    let mut ps = 0u64;
    assert_eq!(
        unsafe { mr_partition_open(t, 0, &mut ps) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert!(last_error().contains("desplazada"), "{}", last_error());
    assert_eq!(
        unsafe { mr_partition_open_ex(t, 0, 8, &mut ps) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_partition_open_ex(t, 0, MR_PARTITION_OPEN_APPLY_SHIFT, &mut ps) },
        MR_OK
    );
    assert!(describe(ps).contains("desplazamiento aplicado: 3 sectores"));
    let mut sector = vec![0u8; 512];
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_source_read(ps, 0, 1, sector.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert!(
        sector.iter().all(|&b| b == 64),
        "LBA 64 del disco = 61 de la imagen"
    );
    assert_eq!(mr_source_close(ps), MR_OK);
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

// ---------- Apple Partition Map (#17) ----------

/// Disco APM de 64 bloques de 512 B: DDM, mapa de 3 entradas (el propio mapa, `Apple_HFS` en
/// [16, 32) con estado de arranque y otra entrada sin firma). Cada bloque de datos vale su número.
fn apm_disk() -> Vec<u8> {
    let mut d: Vec<u8> = (0..64 * 512).map(|i| (i / 512) as u8).collect();
    d[..512].fill(0);
    d[..2].copy_from_slice(b"ER");
    d[2..4].copy_from_slice(&512u16.to_be_bytes());
    d[4..8].copy_from_slice(&64u32.to_be_bytes());
    let entries: [(u32, u32, &str, &str, u32); 2] = [
        (1, 3, "Apple_partition_map", "Apple", 0x37),
        (16, 16, "Apple_HFS", "MR-HFS", 0x8000_0033),
    ];
    for (i, (start, size, kind, name, status)) in entries.iter().enumerate() {
        let e = &mut d[(i + 1) * 512..(i + 2) * 512];
        e.fill(0);
        e[..2].copy_from_slice(b"PM");
        e[4..8].copy_from_slice(&3u32.to_be_bytes());
        e[8..12].copy_from_slice(&start.to_be_bytes());
        e[12..16].copy_from_slice(&size.to_be_bytes());
        e[16..16 + name.len()].copy_from_slice(name.as_bytes());
        e[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        e[88..92].copy_from_slice(&status.to_be_bytes());
    }
    d
}

fn partition_string(
    t: u64,
    index: u32,
    f: unsafe extern "C" fn(u64, u32, *mut u8, usize, *mut usize) -> i32,
) -> String {
    let mut buf = vec![0u8; 256];
    let mut n = 0usize;
    assert_eq!(
        unsafe { f(t, index, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    String::from_utf8(buf[..n].to_vec()).unwrap()
}

#[test]
fn apm_partitions_are_enumerated_and_open_as_sources() {
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(mr_io::MemDevice::new(apm_disk(), 512).unwrap()),
        "apm",
    )));
    let mut t = 0u64;
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    let info = table_info(t).unwrap();
    assert_eq!(
        (
            info.scheme,
            info.sector_size,
            info.partition_count,
            info.confidence,
            info.gpt_flags
        ),
        (MR_SCHEME_APM, 512, 2, MR_CONFIDENCE_SUSPICIOUS, 0)
    );
    let table: Vec<(u32, u32, u64, u64)> = (0..info.anomaly_count as u32)
        .map(|i| {
            let (a, _) = anomaly(t, MR_PARTITION_TABLE, i).unwrap();
            (a.code, a.severity, a.lba, a.value)
        })
        .collect();
    assert_eq!(
        table,
        vec![
            (100, MR_SEVERITY_NOTICE, 0, 0),
            (408, MR_SEVERITY_SUSPICIOUS, 3, 3)
        ]
    );
    let (_, text) = anomaly(t, MR_PARTITION_TABLE, 1).unwrap();
    assert!(text.starts_with("apm_entry_signature\n"), "{text}");
    let p = partition(t, 1).unwrap();
    assert_eq!(
        (
            p.origin,
            p.slot,
            p.start_lba,
            p.sector_count,
            p.mbr_type,
            p.active,
            p.attributes
        ),
        (MR_ORIGIN_APM, 2, 16, 16, 0, 1, 0x8000_0033)
    );
    assert_eq!((p.gpt_type, p.unique_guid), ([0; 16], [0; 16]));
    assert_eq!(partition_string(t, 1, mr_partition_name), "MR-HFS");
    assert_eq!(partition_string(t, 1, mr_partition_apm_type), "Apple_HFS");
    assert_eq!(partition_string(t, 1, mr_partition_type_name), "HFS / HFS+");
    assert_eq!(
        partition_string(t, 0, mr_partition_type_name),
        "Mapa de particiones de Apple"
    );
    let mut ps = 0u64;
    assert_eq!(unsafe { mr_partition_open(t, 1, &mut ps) }, MR_OK);
    let mut sector = vec![0u8; 512];
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_source_read(ps, 0, 1, sector.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert!(sector.iter().all(|&b| b == 16));
    assert!(describe(ps).contains("APM Apple_HFS"), "{}", describe(ps));
    assert_eq!(mr_source_close(ps), MR_OK);
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
    // En un MBR la función de tipo APM devuelve una cadena vacía.
    let src = insert(Entry::Source(mr_api::source_from_device(
        Arc::new(mr_io::MemDevice::new(partitioned_disk(), 512).unwrap()),
        "mbr",
    )));
    assert_eq!(unsafe { mr_source_partitions(src, &mut t) }, MR_OK);
    assert_eq!(partition_string(t, 0, mr_partition_apm_type), "");
    let mut buf = [0u8; 8];
    assert_eq!(
        unsafe { mr_partition_apm_type(t, 99, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_ERR_OUT_OF_RANGE
    );
    assert_eq!(mr_partitions_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

// ---------- Búsqueda exprés de particiones perdidas (#15) ----------

#[test]
fn search_structs_match_header() {
    assert_eq!(std::mem::size_of::<MrPartitionSearchOptions>(), 64);
    assert_eq!(std::mem::size_of::<MrPartitionSearchInfo>(), 96);
    assert_eq!(std::mem::size_of::<MrFoundVolume>(), 136);
    assert_eq!(std::mem::size_of::<MrFoundTable>(), 32);
    assert_eq!(
        std::mem::offset_of!(MrPartitionSearchOptions, max_volumes),
        40
    );
    assert_eq!(std::mem::offset_of!(MrPartitionSearchInfo, shift_bytes), 24);
    assert_eq!(std::mem::offset_of!(MrPartitionSearchInfo, sectors), 88);
    assert_eq!(std::mem::offset_of!(MrFoundVolume, fs_sector_size), 40);
    assert_eq!(std::mem::offset_of!(MrFoundVolume, primary_byte), 96);
    assert_eq!(std::mem::offset_of!(MrFoundVolume, score), 128);
}

/// Sector de arranque NTFS sintético (total = `sectors - 1`, copia en el último sector).
fn ntfs_boot(hidden: u32, sectors: u64) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    b[3..11].copy_from_slice(b"NTFS    ");
    b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
    b[0x0D] = 8;
    b[0x15] = 0xF8;
    b[0x1C..0x20].copy_from_slice(&hidden.to_le_bytes());
    b[0x28..0x30].copy_from_slice(&(sectors - 1).to_le_bytes());
    b[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
    b[0x38..0x40].copy_from_slice(&(sectors / 16).to_le_bytes());
    b[0x40] = 0xF6;
    b[0x44] = 1;
    b[0x48..0x50].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    b[0x1FE] = 0x55;
    b[0x1FF] = 0xAA;
    b
}

/// Disco sin tabla con dos NTFS. Con `added` sectores añadidos al inicio, los volúmenes declaran
/// sus LBA del disco original (2048 y 4096) y están `added` sectores más allá.
fn lost_disk(added: usize) -> Vec<u8> {
    let mut d = vec![0u8; (6144 + added) * 512];
    for (start, len) in [(2048usize, 2048usize), (4096, 2048)] {
        let b = ntfs_boot(start as u32, len as u64);
        let at = start + added;
        d[at * 512..(at + 1) * 512].copy_from_slice(&b);
        d[(at + len - 1) * 512..(at + len) * 512].copy_from_slice(&b);
    }
    d
}

fn search_source(data: &[u8]) -> (tempfile::TempDir, u64) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("lost.img");
    std::fs::write(&p, data).unwrap();
    let h = open(p.to_str().unwrap(), 512, 0).unwrap();
    (dir, h)
}

fn search_options_default() -> MrPartitionSearchOptions {
    MrPartitionSearchOptions {
        struct_size: std::mem::size_of::<MrPartitionSearchOptions>() as u32,
        ..Default::default()
    }
}

fn start_search(src: u64, o: Option<&MrPartitionSearchOptions>) -> Result<u64, i32> {
    let mut t = 0u64;
    let ptr = o.map_or(std::ptr::null(), |o| o as *const MrPartitionSearchOptions);
    match unsafe { mr_source_partition_search_start(src, ptr, &mut t) } {
        MR_OK => Ok(t),
        rc => Err(rc),
    }
}

fn search_info(t: u64) -> Result<MrPartitionSearchInfo, i32> {
    let mut i = MrPartitionSearchInfo {
        struct_size: std::mem::size_of::<MrPartitionSearchInfo>() as u32,
        ..Default::default()
    };
    match unsafe { mr_partition_search_result(t, &mut i) } {
        MR_OK => Ok(i),
        rc => Err(rc),
    }
}

fn found_volume(t: u64, index: u32) -> Result<MrFoundVolume, i32> {
    let mut v = MrFoundVolume {
        struct_size: std::mem::size_of::<MrFoundVolume>() as u32,
        ..Default::default()
    };
    match unsafe { mr_found_volume_get(t, index, &mut v) } {
        MR_OK => Ok(v),
        rc => Err(rc),
    }
}

fn found_anomaly(t: u64, vi: u32, ai: u32) -> Result<(MrPartitionAnomaly, String), i32> {
    let mut a = MrPartitionAnomaly {
        struct_size: std::mem::size_of::<MrPartitionAnomaly>() as u32,
        ..Default::default()
    };
    let rc = unsafe { mr_found_volume_anomaly(t, vi, ai, &mut a) };
    if rc != MR_OK {
        return Err(rc);
    }
    let mut buf = vec![0u8; 1024];
    let mut n = 0usize;
    let rc =
        unsafe { mr_found_volume_anomaly_text(t, vi, ai, buf.as_mut_ptr(), buf.len(), &mut n) };
    assert_eq!(rc, MR_OK);
    Ok((a, String::from_utf8(buf[..n].to_vec()).unwrap()))
}

#[test]
fn partition_search_finds_volumes_and_opens_them() {
    let (_d, src) = search_source(&lost_disk(0));
    let t = start_search(src, None).unwrap();
    let st = wait(t);
    assert_eq!(st.state, TaskState::Completed as u32);
    let i = search_info(t).unwrap();
    assert_eq!(
        (i.status, i.volume_count, i.flags),
        (MR_SEARCH_COMPLETE, 2, 0)
    );
    assert_eq!((i.max_reads, i.sector_size, i.sectors), (4096, 512, 6144));
    assert_eq!((st.done, st.total), (i.reads, 4096));
    assert!(i.reads > 0 && i.reads < 400);
    let v = found_volume(t, 0).unwrap();
    assert_eq!(
        (v.fs, v.start_lba, v.sector_count),
        (MR_FS_NTFS, 2048, 2048)
    );
    assert_eq!((v.primary, v.backup), (1, 1));
    assert_eq!(v.confidence, MR_CONFIDENCE_VALID);
    assert_eq!(v.flags, MR_FOUND_HAS_BACKUP);
    assert_eq!(v.table_index, MR_PARTITION_TABLE);
    assert_eq!((v.serial, v.suggested_mbr_type), (0x1122_3344_5566_7788, 7));
    let mut buf = vec![0u8; 512];
    let mut n = 0usize;
    assert_eq!(
        unsafe { mr_found_volume_origin(t, 0, buf.as_mut_ptr(), buf.len(), &mut n) },
        MR_OK
    );
    assert!(String::from_utf8_lossy(&buf[..n]).contains("1 MiB"));
    // Abrir el volumen: el primer sector es su arranque.
    let mut vs = 0u64;
    assert_eq!(unsafe { mr_found_volume_open(t, 0, 0, &mut vs) }, MR_OK);
    assert!(describe(vs).starts_with("Volumen encontrado NTFS LBA 2048–4095"));
    assert_eq!(
        unsafe { mr_source_read(vs, 0, 1, buf.as_mut_ptr(), 512, &mut n) },
        MR_OK
    );
    assert_eq!(&buf[3..11], b"NTFS    ");
    assert_eq!(mr_source_close(vs), MR_OK);
    // Índices, tamaños, punteros y opciones no válidos.
    assert_eq!(found_volume(t, 2).unwrap_err(), MR_ERR_OUT_OF_RANGE);
    let mut small = MrFoundVolume {
        struct_size: 8,
        ..Default::default()
    };
    assert_eq!(
        unsafe { mr_found_volume_get(t, 0, &mut small) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_found_volume_open(t, 0, 2, &mut vs) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_found_volume_open(t, 0, 0, std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        found_anomaly(t, MR_PARTITION_TABLE, 0).unwrap_err(),
        MR_ERR_OUT_OF_RANGE
    );
    let mut ft = MrFoundTable {
        struct_size: std::mem::size_of::<MrFoundTable>() as u32,
        ..Default::default()
    };
    assert_eq!(
        unsafe { mr_found_table_get(t, 0, &mut ft) },
        MR_ERR_OUT_OF_RANGE
    );
    // Una tabla de particiones o un origen no son búsquedas; una búsqueda no es otra tarea.
    assert_eq!(found_volume(src, 0).unwrap_err(), MR_ERR_INVALID_HANDLE);
    assert_eq!(search_info(src).unwrap_err(), MR_ERR_INVALID_HANDLE);
    let mut r = MrVerifyReport::default();
    assert_eq!(
        unsafe { mr_verify_result(t, &mut r) },
        MR_ERR_INVALID_HANDLE
    );
    // El origen puede cerrarse: el volumen se abre igualmente desde la búsqueda.
    assert_eq!(mr_source_close(src), MR_OK);
    assert_eq!(unsafe { mr_found_volume_open(t, 1, 0, &mut vs) }, MR_OK);
    assert_eq!(mr_source_close(vs), MR_OK);
    assert_eq!(mr_task_free(t), MR_OK);
    assert_eq!(search_info(t).unwrap_err(), MR_ERR_INVALID_HANDLE);
}

#[test]
fn partition_search_options_are_versioned_and_validated() {
    let (_d, src) = search_source(&lost_disk(0));
    let mut o = search_options_default();
    o.struct_size = 60;
    assert_eq!(start_search(src, Some(&o)), Err(MR_ERR_INVALID_ARGUMENT));
    assert!(last_error().contains("mr_partition_search_options"));
    for f in [
        |o: &mut MrPartitionSearchOptions| o.flags = 1,
        |o: &mut MrPartitionSearchOptions| o.reserved = 1,
        |o: &mut MrPartitionSearchOptions| o.probe_window = 1000,
        |o: &mut MrPartitionSearchOptions| o.families = 64,
        |o: &mut MrPartitionSearchOptions| o.max_reads = u64::MAX,
    ] {
        let mut o = search_options_default();
        f(&mut o);
        assert_eq!(start_search(src, Some(&o)), Err(MR_ERR_INVALID_ARGUMENT));
    }
    let mut t = 0u64;
    assert_eq!(
        unsafe { mr_source_partition_search_start(src, std::ptr::null(), std::ptr::null_mut()) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { mr_source_partition_search_start(999_999, std::ptr::null(), &mut t) },
        MR_ERR_INVALID_HANDLE
    );
    // Presupuesto pequeño: resultado parcial informado.
    let mut o = search_options_default();
    o.max_reads = 3;
    let t = start_search(src, Some(&o)).unwrap();
    assert_eq!(wait(t).state, TaskState::Completed as u32);
    let i = search_info(t).unwrap();
    assert_eq!((i.status, i.max_reads), (MR_SEARCH_BUDGET_EXHAUSTED, 3));
    assert!(i.reads <= 3 && i.pending > 0);
    assert_eq!(mr_task_free(t), MR_OK);
    // Solo exFAT: los NTFS no aparecen.
    let mut o = search_options_default();
    o.families = MR_SEARCH_FAMILY_EXFAT;
    let t = start_search(src, Some(&o)).unwrap();
    wait(t);
    assert_eq!(search_info(t).unwrap().volume_count, 0);
    assert_eq!(mr_task_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn shifted_search_reports_the_shift_and_opens_only_on_request() {
    let (_d, src) = search_source(&lost_disk(16));
    let t = start_search(src, None).unwrap();
    wait(t);
    let i = search_info(t).unwrap();
    assert_eq!((i.volume_count, i.anomaly_count), (2, 1));
    assert_eq!((i.flags, i.shift_bytes), (MR_SEARCH_HAS_SHIFT, 16 * 512));
    let (a, text) = found_anomaly(t, MR_PARTITION_TABLE, 0).unwrap();
    assert_eq!((a.code, a.lba as i64, a.value as i64), (320, 16, -16));
    assert_eq!(
        a.flags,
        MR_ANOMALY_LBA_SIGNED | MR_ANOMALY_HAS_VALUE | MR_ANOMALY_VALUE_SIGNED
    );
    assert!(text.starts_with("image_shifted\n"), "{text}");
    let v = found_volume(t, 0).unwrap();
    assert_eq!(v.flags & MR_FOUND_REQUIRES_SHIFT, MR_FOUND_REQUIRES_SHIFT);
    assert_eq!((v.start_lba, v.declared_start), (2064, 2048));
    assert_eq!(v.confidence, MR_CONFIDENCE_SUSPICIOUS);
    let texts: Vec<String> = (0..v.anomaly_count)
        .map(|k| found_anomaly(t, 0, k).unwrap().1)
        .collect();
    assert!(
        texts
            .iter()
            .any(|x| x.starts_with("found_shift_detected\n")),
        "{texts:?}"
    );
    let mut vs = 0u64;
    assert_eq!(
        unsafe { mr_found_volume_open(t, 0, 0, &mut vs) },
        MR_ERR_INVALID_ARGUMENT
    );
    assert!(
        last_error().contains("desplazada 16 sectores"),
        "{}",
        last_error()
    );
    assert_eq!(
        unsafe { mr_found_volume_open(t, 0, MR_PARTITION_OPEN_APPLY_SHIFT, &mut vs) },
        MR_OK
    );
    assert!(describe(vs).contains("desplazamiento detectado aplicado"));
    assert_eq!(mr_source_close(vs), MR_OK);
    assert_eq!(mr_task_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

#[test]
fn cancelled_search_keeps_its_partial_result() {
    // Imagen dispersa grande: la búsqueda tarda lo bastante para cancelarla a mitad.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.img");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(2 << 30).unwrap();
    drop(f);
    let src = open(p.to_str().unwrap(), 512, 0).unwrap();
    let t = start_search(src, None).unwrap();
    assert_eq!(mr_task_cancel(t), MR_OK);
    let st = wait(t);
    let i = search_info(t).unwrap();
    if st.state == TaskState::Cancelled as u32 {
        assert_eq!(i.status, MR_SEARCH_CANCELLED);
    } else {
        assert_eq!(st.state, TaskState::Completed as u32);
        assert_eq!(i.status, MR_SEARCH_COMPLETE);
    }
    assert_eq!(i.volume_count, 0);
    assert_eq!(mr_task_free(t), MR_OK);
    assert_eq!(mr_source_close(src), MR_OK);
}

// ---------- Ficheros externos: discos diferenciales (mr_source_open_ex, issue #8) ----------

#[path = "../../mr-image/src/diff_builder.rs"]
mod diff_builder;

fn open_ex(p: &str, flags: u32, externals: &[&str]) -> Result<u64, i32> {
    let list: Vec<MrStr> = externals
        .iter()
        .map(|s| MrStr {
            ptr: s.as_ptr(),
            len: s.len(),
        })
        .collect();
    let o = MrOpenOptions {
        struct_size: std::mem::size_of::<MrOpenOptions>() as u32,
        flags,
        sector_size: 0,
        external_count: list.len() as u32,
        externals: if list.is_empty() {
            std::ptr::null()
        } else {
            list.as_ptr()
        },
        reserved: 0,
    };
    let mut h = 0u64;
    let rc = unsafe { mr_source_open_ex(p.as_ptr(), p.len(), &o, &mut h) };
    if rc == MR_OK {
        Ok(h)
    } else {
        Err(rc)
    }
}

fn hints(p: &str) -> (i32, String, u32) {
    let mut buf = vec![0u8; 4096];
    let (mut len, mut count) = (0usize, 0u32);
    let rc = unsafe {
        mr_image_external_hints(
            p.as_ptr(),
            p.len(),
            buf.as_mut_ptr(),
            buf.len(),
            &mut len,
            &mut count,
        )
    };
    (
        rc,
        String::from_utf8_lossy(&buf[..len.min(buf.len())]).into_owned(),
        count,
    )
}

#[test]
fn open_ex_layout_and_differencing_chain() {
    use diff_builder as db;
    assert_eq!(std::mem::size_of::<MrOpenOptions>(), 32);
    assert_eq!(std::mem::size_of::<MrStr>(), 16);
    assert_eq!(std::mem::offset_of!(MrOpenOptions, externals), 16);
    assert_eq!(
        (MR_OPEN_IGNORE_LINK_MISMATCH, MR_ERR_EXTERNAL_MISMATCH),
        (8, 17)
    );

    let dir = tempfile::tempdir().unwrap();
    let size = 16 * 4096u64;
    let w0 = db::writes(0, 0..40);
    let w1 = db::writes(1, [2, 50]);
    let base_b = db::vhd_layer(size, 4096, [0xAB; 16], None, &w0);
    let child_b = db::vhd_layer(
        size,
        4096,
        [0xCD; 16],
        Some(&db::VhdParent::new([0xAB; 16], "base.vhd")),
        &w1,
    );
    let other_b = db::vhd_layer(size, 4096, [0x11; 16], None, &w0);
    let write = |n: &str, b: &[u8]| {
        let p = dir.path().join(n);
        std::fs::write(&p, b).unwrap();
        p.to_str().unwrap().to_string()
    };
    let base = write("base.vhd", &base_b);
    let child = write("child.vhd", &child_b);
    let other = write("other.vhd", &other_b);

    // Indicaciones sin abrir el padre; ninguna en un disco que no es diferencial.
    let (rc, text, n) = hints(&child);
    assert_eq!(rc, MR_OK);
    assert_eq!(n, 5);
    assert!(text.lines().any(|l| l == "parent_name\tbase.vhd"), "{text}");
    assert!(
        text.lines().any(|l| l == "locator_W2ru\t.\\base.vhd"),
        "{text}"
    );
    assert_eq!(hints(&base), (MR_OK, String::new(), 0));
    assert_eq!(hints("C:\\no\\existe.vhd").0, MR_ERR_IO);

    // Sin padres: referencia externa (también con mr_source_open).
    assert_eq!(open(&child, 0, 0), Err(MR_ERR_EXTERNAL_REFERENCE));
    assert_eq!(open_ex(&child, 0, &[]), Err(MR_ERR_EXTERNAL_REFERENCE));
    let h = open_ex(&child, 0, &[&base]).unwrap();
    assert!(describe(h).starts_with("VHD diferencial"));
    let mut buf = vec![0u8; 512];
    let mut len = 0usize;
    assert_eq!(
        unsafe { mr_source_read(h, 50, 1, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_OK
    );
    assert_eq!(buf, db::sector_data(1, 50));
    assert_eq!(
        unsafe { mr_source_read(h, 3, 1, buf.as_mut_ptr(), buf.len(), &mut len) },
        MR_OK
    );
    assert_eq!(buf, db::sector_data(0, 3));
    assert_eq!(mr_source_close(h), MR_OK);

    // Vínculo que no coincide: código propio; con el bit explícito se abre y se advierte.
    assert_eq!(open_ex(&child, 0, &[&other]), Err(MR_ERR_EXTERNAL_MISMATCH));
    assert!(last_error().contains("no corresponde"), "{}", last_error());
    let h = open_ex(&child, MR_OPEN_IGNORE_LINK_MISMATCH, &[&other]).unwrap();
    assert!(describe(h).contains("NO verificado"));
    assert_eq!(mr_source_close(h), MR_OK);
    // Ficheros que sobran, rutas de dispositivo y combinaciones no válidas.
    assert_eq!(open_ex(&base, 0, &[&child]), Err(MR_ERR_EXTERNAL_MISMATCH));
    assert_eq!(
        open_ex(&child, 0, &[r"\\.\PhysicalDrive0"]),
        Err(MR_ERR_PATH_REJECTED)
    );
    assert_eq!(open_ex(&child, 0, &[""]), Err(MR_ERR_INVALID_ARGUMENT));
    assert_eq!(
        open_ex(&child, MR_OPEN_SPLIT, &[&base]),
        Err(MR_ERR_INVALID_ARGUMENT)
    );
    assert_eq!(open_ex(&child, 16, &[&base]), Err(MR_ERR_INVALID_ARGUMENT));
    let many: Vec<&str> = (0..=MR_MAX_EXTERNAL_FILES).map(|_| base.as_str()).collect();
    assert_eq!(open_ex(&child, 0, &many), Err(MR_ERR_INVALID_ARGUMENT));

    // Estructura: tamaño desconocido, reservado, punteros nulos.
    let mut h = 0u64;
    let mut o = MrOpenOptions {
        struct_size: 24,
        flags: 0,
        sector_size: 0,
        external_count: 0,
        externals: std::ptr::null(),
        reserved: 0,
    };
    let rc = unsafe { mr_source_open_ex(child.as_ptr(), child.len(), &o, &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    o.struct_size = 32;
    o.reserved = 1;
    let rc = unsafe { mr_source_open_ex(child.as_ptr(), child.len(), &o, &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    o.reserved = 0;
    o.external_count = 1;
    let rc = unsafe { mr_source_open_ex(child.as_ptr(), child.len(), &o, &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    let rc = unsafe { mr_source_open_ex(child.as_ptr(), child.len(), std::ptr::null(), &mut h) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    let rc = unsafe { mr_source_open_ex(child.as_ptr(), child.len(), &o, std::ptr::null_mut()) };
    assert_eq!(rc, MR_ERR_INVALID_ARGUMENT);
    // Un RAW con struct correcto y sin externos se abre como con mr_source_open.
    let (_d, raw) = temp_image("r.img", 4, 512);
    let h = open_ex(&raw, 0, &[]).unwrap();
    assert_eq!(mr_source_close(h), MR_OK);

    assert_eq!(std::fs::read(&base).unwrap(), base_b);
    assert_eq!(std::fs::read(&child).unwrap(), child_b);
}
