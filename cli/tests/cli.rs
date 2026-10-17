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
