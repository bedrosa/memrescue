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
    assert!(String::from_utf8_lossy(&o.stderr).contains("todavía no está soportado"));
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
