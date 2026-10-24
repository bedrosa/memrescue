//! Copia del particionado (#18) de extremo a extremo: `partitions --save-backup`,
//! `find-partitions --save-backup` y `partition-backup-check`, con imágenes sintéticas en un
//! directorio temporal.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// MBR con una primaria y una extendida con una lógica (4096 sectores de 512 B).
fn image(dir: &Path, name: &str, primary_type: u8) -> PathBuf {
    let mut d = vec![0u8; 4096 * 512];
    let mut table = |lba: usize, entries: &[(u8, u8, u32, u32)]| {
        let s = &mut d[lba * 512..(lba + 1) * 512];
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
        &[(0x80, primary_type, 2048, 1024), (0, 0x0F, 3072, 1024)],
    );
    table(3072, &[(0, 0x83, 64, 512)]);
    let p = dir.join(name);
    std::fs::write(&p, d).unwrap();
    p
}

#[test]
fn save_check_and_detect_changes() {
    let dir = tempfile::tempdir().unwrap();
    let img = image(dir.path(), "disco.img", 0x07);
    let original = std::fs::read(&img).unwrap();
    let f = img.to_str().unwrap();
    let out = dir.path().join("copia.json");
    let o = out.to_str().unwrap();

    let r = cli(&["partitions", f, "--save-backup", o]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(
        text(&r).contains("Copia del particionado guardada"),
        "{}",
        text(&r)
    );
    let saved = std::fs::read_to_string(&out).unwrap();
    assert!(saved.contains("\"format\": \"memrescue-partition-backup\""));
    assert!(saved.contains("\"version\": 1"));
    assert!(saved.contains("\"kind\": \"ebr\""));
    assert!(saved.contains("\"search\": null"));

    // Nunca se sobrescribe sin petición explícita; con --overwrite, sí.
    let r = cli(&["partitions", f, "--save-backup", o]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r));
    let r = cli(&["partitions", f, "--json", "--save-backup", o, "--overwrite"]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(String::from_utf8_lossy(&r.stdout).starts_with("{\"scheme\":\"mbr\""));
    // Nunca sobre el propio origen.
    let r = cli(&["partitions", f, "--save-backup", f, "--overwrite"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r));
    assert_eq!(
        std::fs::read(&img).unwrap(),
        original,
        "el origen no cambia"
    );

    // Misma imagen: coincide.
    let r = cli(&["partition-backup-check", o, f]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(
        text(&r).contains("El origen coincide con la copia"),
        "{}",
        text(&r)
    );
    let r = cli(&["partition-backup-check", o, f, "--json"]);
    assert!(r.status.success(), "{}", text(&r));
    let json = String::from_utf8_lossy(&r.stdout);
    assert!(
        json.contains("\"identical\":true,\"differences\":[]"),
        "{json}"
    );
    assert!(
        json.contains("\"scheme\":\"mbr\",\"partitions\":2,\"regions\":2"),
        "{json}"
    );

    // Copia modificada de la imagen (otro tipo en la entrada 0): diferencias y código 4.
    let other = image(dir.path(), "cambiada.img", 0x0C);
    let r = cli(&[
        "partition-backup-check",
        o,
        other.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.status.code(), Some(4), "{}", text(&r));
    let json = String::from_utf8_lossy(&r.stdout);
    assert!(json.contains("\"identical\":false"), "{json}");
    assert!(json.contains("\"code\":\"region_changed\""), "{json}");
    assert!(json.contains("\"code\":\"partition_changed\""), "{json}");
    let r = cli(&["partition-backup-check", o, other.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(4));
    assert!(text(&r).contains("[partition_changed]"), "{}", text(&r));

    // Copia alterada: se rechaza sin comparar.
    let tampered = dir.path().join("alterada.json");
    std::fs::write(
        &tampered,
        saved.replacen("\"start_lba\": 2048", "\"start_lba\": 2049", 1),
    )
    .unwrap();
    let r = cli(&["partition-backup-check", tampered.to_str().unwrap(), f]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r));
    assert!(text(&r).contains("alterada"), "{}", text(&r));
    // Copia truncada.
    std::fs::write(&tampered, &saved[..saved.len() / 2]).unwrap();
    let r = cli(&["partition-backup-check", tampered.to_str().unwrap(), f]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r));
    assert!(text(&r).contains("mal formada"), "{}", text(&r));
}

#[test]
fn find_partitions_saves_the_found_volumes_too() {
    let dir = tempfile::tempdir().unwrap();
    let img = image(dir.path(), "disco.img", 0x07);
    let f = img.to_str().unwrap();
    let out = dir.path().join("con-busqueda.json");
    let o = out.to_str().unwrap();
    let r = cli(&["find-partitions", f, "--json", "--save-backup", o]);
    assert!(r.status.success(), "{}", text(&r));
    let saved = std::fs::read_to_string(&out).unwrap();
    assert!(saved.contains("\"search\": {"), "{saved}");
    assert!(saved.contains("\"status\": \"complete\""), "{saved}");
    let r = cli(&["partition-backup-check", o, f, "--json"]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(String::from_utf8_lossy(&r.stdout).contains("\"search\":true"));
}

#[test]
fn backup_options_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let img = image(dir.path(), "disco.img", 0x07);
    let f = img.to_str().unwrap();
    let out = dir.path().join("x.json");
    let o = out.to_str().unwrap();
    for args in [
        vec!["verify", f, "--save-backup", o],
        vec!["partitions", f, "--save-backup"],
        vec!["partitions", f, "--overwrite"],
        vec!["partition-backup-check", o],
    ] {
        let r = cli(&args);
        assert_eq!(r.status.code(), Some(2), "{args:?}: {}", text(&r));
    }
    assert!(!out.exists());
    // Copia inexistente.
    let r = cli(&["partition-backup-check", o, f]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r));
}
