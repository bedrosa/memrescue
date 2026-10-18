//! Pruebas de extremo a extremo del ejecutable `memrescue-cli`.

use std::io::Write;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
}

#[test]
fn version_runs() {
    let o = cli().arg("version").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("MemRescue engine"));
}

#[test]
fn sectors_dumps_requested_lba_with_absolute_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("s.img");
    let mut data = vec![0u8; 4 * 1024];
    data[1024..1024 + 8].copy_from_slice(b"MARCADOR");
    std::fs::write(&img, &data).unwrap();
    let o = cli()
        .args([
            "sectors",
            img.to_str().unwrap(),
            "--lba",
            "1",
            "--sector-size",
            "1024",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("== LBA 1 (desplazamiento 1024 = 0x400)"),
        "{out}"
    );
    assert!(
        out.contains("000000000400  4D 41 52 43 41 44 4F 52"),
        "{out}"
    );
    assert!(out.contains("MARCADOR"), "{out}");
    // Fuera de rango y recuento no válido.
    let o = cli()
        .args([
            "sectors",
            img.to_str().unwrap(),
            "--lba",
            "4",
            "--sector-size",
            "1024",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = cli()
        .args([
            "sectors",
            img.to_str().unwrap(),
            "--lba",
            "0",
            "--count",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn verify_and_probe() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("v.img");
    std::fs::write(&img, vec![0u8; 2048 * 512]).unwrap();
    let o = cli()
        .args(["verify", img.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"sectors\":2048,\"ok\":2048,\"bad\":0"));
    assert!(String::from_utf8_lossy(&o.stdout).contains(
        "\"states\":{\"read\":2048,\"recovered\":0,\"unreadable\":0,\"skipped\":0,\"sparse\":0},\"untrusted_ranges\":[]"
    ));
    let o = cli()
        .args(["verify", img.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Sectores fiables (leídos, recuperados o dispersos): 2048 de 2048"));
    assert!(out.contains("ilegibles   0"));

    let vmdk = dir.path().join("d.vmdk");
    std::fs::write(&vmdk, b"# Disk DescriptorFile\nversion=1\n").unwrap();
    let o = cli()
        .args(["probe", vmdk.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("VMDK (descriptor) (todavía no soportado)"));
    let o = cli()
        .args(["image-info", vmdk.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no soportada"));
}

#[test]
fn split_range_and_sector_states() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..64 * 512).map(|i| (i / 512) as u8).collect();
    for (n, part) in data.chunks(20 * 512).enumerate() {
        std::fs::write(dir.path().join(format!("s.{:03}", n + 1)), part).unwrap();
    }
    let first = dir.path().join("s.001");
    let f = first.to_str().unwrap();
    let o = cli().args(["split-list", f]).output().unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 4);
    let o = cli()
        .args(["image-info", f, "--split", "--json"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("\"sector_count\":64"), "{out}");
    assert!(out.contains("4 segmentos"), "{out}");
    // Rango 30..40 de la imagen dividida: su LBA 5 es la 35 del conjunto (cruza segmentos).
    let o = cli()
        .args(["sectors", f, "--split", "--range", "30:10", "--lba", "5"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("== LBA 5 (desplazamiento 2560 = 0xA00) [leído]"),
        "{out}"
    );
    assert!(out.contains("23 23 23 23"), "{out}");
    let o = cli()
        .args(["sectors", f, "--split", "--range", "30:10", "--lba", "10"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = cli().args(["verify", f, "--read-back"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = cli()
        .args(["hash", f, "--split", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success());
    // Mismo hash que el contenido concatenado.
    let whole = dir.path().join("whole.img");
    std::fs::write(&whole, &data).unwrap();
    let o2 = cli()
        .args(["hash", whole.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(o.stdout, o2.stdout);
}

#[test]
fn volume_alias_is_refused() {
    let o = cli().args(["image-info", r"\\?\C:"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no permitida"));
}

#[test]
fn bad_usage_exits_with_2() {
    let o = cli().arg("comando-inexistente").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn physical_device_is_refused_with_1() {
    let o = cli()
        .args(["image-info", r"\\.\PhysicalDrive0"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no permitida"));
}

#[test]
fn hash_json_and_job_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("abc.img");
    std::fs::File::create(&img)
        .unwrap()
        .write_all(b"abc")
        .unwrap();
    let db = dir.path().join("t.mrdb");
    let o = cli()
        .args([
            "hash",
            img.to_str().unwrap(),
            "--algo",
            "crc32,md5",
            "--json",
            "--db",
        ])
        .arg(&db)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("\"crc32\":\"352441c2\""), "{out}");
    assert!(
        out.contains("\"md5\":\"900150983cd24fb0d6963f7d28e17f72\""),
        "{out}"
    );
    let j = cli().arg("jobs").arg(&db).output().unwrap();
    let jobs = String::from_utf8_lossy(&j.stdout);
    assert!(jobs.contains("completed"), "{jobs}");
}

#[test]
fn verify_error_policy_options() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("p.img");
    std::fs::write(&img, vec![0u8; 256 * 512]).unwrap();
    let f = img.to_str().unwrap();
    let o = cli()
        .args([
            "verify",
            f,
            "--skip-codes",
            "1117, 23,0x17",
            "--retries",
            "3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("\"sectors\":256,\"ok\":256,\"bad\":0"),
        "{out}"
    );
    assert!(
        out.contains(
            "\"error_policy\":{\"retries\":3,\"skip_codes\":[23,1117],\"ignore_all\":false},\"omitted_errors\":0,\"error_codes\":{}"
        ),
        "{out}"
    );
    let o = cli()
        .args([
            "verify",
            f,
            "--ignore-errors",
            "--jump",
            "16",
            "--read-back",
            "--range",
            "10:100",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("100 de 100"), "{out}");
    assert!(
        out.contains("Política de errores: ignorar todos (sin reintentos)"),
        "{out}"
    );
    assert!(
        out.contains("errores omitidos (ilegibles sin reintentar): 0"),
        "{out}"
    );
    let o = cli()
        .args(["verify", f, "--skip-codes", "23"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout)
        .contains("Política de errores: sin reintentos para los códigos 23"));
    // Sin política no se muestra nada nuevo.
    let o = cli().args(["verify", f]).output().unwrap();
    assert!(!String::from_utf8_lossy(&o.stdout).contains("Política de errores"));
    // Listas no válidas: uso incorrecto (código 2).
    for bad in ["abc", "0", "-5", "23,,x", "", "0x"] {
        let o = cli()
            .args(["verify", f, "--skip-codes", bad])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "--skip-codes {bad:?}");
    }
    let many: Vec<String> = (1..=257).map(|i| i.to_string()).collect();
    let o = cli()
        .args(["verify", f, "--skip-codes", &many.join(",")])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = cli().args(["verify", f, "--skip-codes"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn sectors_use_the_configured_fill_patterns() {
    // Validación de las opciones; el relleno sobre sectores ilegibles se prueba en tolerant.rs.
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("f.img");
    std::fs::write(&img, vec![0x11u8; 8 * 512]).unwrap();
    let f = img.to_str().unwrap();
    // Patrón inválido: uso incorrecto (2), sin abrir la imagen.
    for args in [
        vec!["--bad-fill", "00000000"],
        vec!["--skip-fill", "FFFFFFFF"],
        vec!["--bad-fill", "SAME", "--skip-fill", "SAME"],
        vec!["--bad-fill", "BAD"],
    ] {
        let o = cli()
            .args(["sectors", f, "--lba", "0"])
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
    // Solo en sectors.
    let o = cli()
        .args(["verify", f, "--bad-fill", "DEADBEEF"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    // Patrones válidos sobre sectores legibles: datos sin cambios.
    let o = cli()
        .args([
            "sectors",
            f,
            "--lba",
            "0",
            "--bad-fill",
            "0xDEADBEEF",
            "--skip-fill",
            "sk1p",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("11 11 11 11"));
}

/// Disco de 4096 sectores (cada byte = LBA mod 256 fuera de las tablas): primaria activa 0x07 en
/// [2048, +1024), extendida 0x0F en [3072, +1024) con una lógica Linux en 3136 y un tipo
/// desconocido 0x7F que termina fuera del disco.
fn partitioned_image(dir: &std::path::Path) -> std::path::PathBuf {
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
    table(0, &[(0x80, 0x07, 2048, 1024), (0, 0x0F, 3072, 1024)]);
    table(3072, &[(0, 0x83, 64, 512), (0, 0x05, 640, 384)]);
    table(3712, &[(0, 0x7F, 64, 900)]);
    let img = dir.join("p.img");
    std::fs::write(&img, d).unwrap();
    img
}

#[test]
fn partitions_lists_table_confidence_and_anomalies() {
    let dir = tempfile::tempdir().unwrap();
    let img = partitioned_image(dir.path());
    let f = img.to_str().unwrap();
    let o = cli().args(["partitions", f]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Esquema:         MBR"), "{out}");
    assert!(out.contains("Confianza:       SOSPECHOSA"), "{out}");
    assert!(out.contains("NTFS / exFAT / HPFS"), "{out}");
    assert!(out.contains("lógica (EBR 3072)"), "{out}");
    assert!(out.contains("0x7F desconocido"), "{out}");
    assert!(out.contains("mbr_unknown_type"), "{out}");
    assert!(out.contains("mbr_partially_outside_disk"), "{out}");
    let o = cli().args(["partitions", f, "--json"]).output().unwrap();
    assert!(o.status.success());
    let json = String::from_utf8_lossy(&o.stdout);
    assert!(
        json.starts_with("{\"scheme\":\"mbr\",\"sector_size\":512,\"confidence\":\"suspicious\""),
        "{json}"
    );
    assert!(json.contains("{\"number\":1,\"origin\":\"primary\",\"slot\":0,\"ebr_lba\":null,\"start_lba\":2048,\"sectors\":1024,\"end_lba\":3071,\"type\":7,\"type_name\":\"NTFS / exFAT / HPFS\",\"active\":true,\"confidence\":\"valid\",\"anomalies\":[]}"), "{json}");
    assert!(
        json.contains("\"origin\":\"logical\",\"slot\":0,\"ebr_lba\":3072,\"start_lba\":3136"),
        "{json}"
    );
    assert!(json.contains("\"code\":\"mbr_unknown_type\",\"severity\":\"suspicious\",\"lba\":3712,\"target_lba\":null,\"value\":127"), "{json}");
    // Sin tabla: esquema none con la anomalía informativa.
    let blank = dir.path().join("blank.img");
    std::fs::write(&blank, vec![0u8; 64 * 512]).unwrap();
    let o = cli()
        .args(["partitions", blank.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let json = String::from_utf8_lossy(&o.stdout);
    assert!(
        json.contains(
            "\"scheme\":\"none\",\"sector_size\":512,\"confidence\":\"valid\",\"gpt\":null,\"partitions\":[]"
        ),
        "{json}"
    );
    assert!(json.contains("\"code\":\"mbr_missing\""), "{json}");
    assert_eq!(
        std::fs::read(&img).unwrap()[..2],
        [0, 0],
        "el origen no cambia"
    );
}

#[test]
fn partition_option_opens_a_partition_for_sectors_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let img = partitioned_image(dir.path());
    let f = img.to_str().unwrap();
    // Partición 1: su LBA 1 es la 2049 del disco (bytes 0x01).
    let o = cli()
        .args(["sectors", f, "--partition", "1", "--lba", "1"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("01 01 01 01"));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Partición 1: Partición LBA 2048–3071"));
    // Fuera de la partición: error de operación.
    let o = cli()
        .args(["sectors", f, "--partition", "1", "--lba", "1024"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    // La lógica (partición 2) se verifica entera: 512 sectores.
    let o = cli()
        .args(["verify", f, "--partition", "2", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"sectors\":512,\"ok\":512,\"bad\":0"));
    // La recortada (partición 3) se abre hasta el final del disco.
    let o = cli()
        .args(["image-info", f, "--partition", "3", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let info = String::from_utf8_lossy(&o.stdout);
    assert!(info.contains("\"sector_count\":320"), "{info}");
    assert!(info.contains("recortada"), "{info}");
    // Número inexistente, cero y combinación con --range.
    let o = cli()
        .args(["sectors", f, "--partition", "9", "--lba", "0"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no existe la partición 9"));
    let o = cli()
        .args(["sectors", f, "--partition", "0", "--lba", "0"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = cli()
        .args([
            "sectors",
            f,
            "--partition",
            "1",
            "--range",
            "0:8",
            "--lba",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
