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
