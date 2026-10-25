//! `vmdk-extents` y `--extent NOMBRE=RUTA` (incidencia 11): las extensiones de un descriptor VMDK
//! separado solo se abren cuando se indican una a una.

use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn extents_are_listed_and_opened_only_when_named() {
    let dir = tempfile::tempdir().unwrap();
    let a: Vec<u8> = (0..8 * 512).map(|i| (i % 251) as u8).collect();
    let b = vec![0x5au8; 8 * 512];
    std::fs::write(dir.path().join("d-f001.vmdk"), &a).unwrap();
    std::fs::write(dir.path().join("d-f002.vmdk"), &b).unwrap();
    let desc = dir.path().join("d.vmdk");
    std::fs::write(
        &desc,
        "# Disk DescriptorFile\nCID=1\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentFlat\"\nRW 8 FLAT \"d-f001.vmdk\" 0\nRW 8 FLAT \"d-f002.vmdk\" 0\nRW 8 ZERO\n",
    )
    .unwrap();
    let d = desc.to_str().unwrap();
    let e1 = format!("d-f001.vmdk={}", dir.path().join("d-f001.vmdk").display());
    let e2 = format!("d-f002.vmdk={}", dir.path().join("d-f002.vmdk").display());

    let o = cli(&["vmdk-extents", d]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("\"d-f002.vmdk\"  requiere fichero"),
        "{}",
        out(&o)
    );
    assert!(err(&o).contains("No se ha abierto ninguna"));
    let o = cli(&["vmdk-extents", d, "--json"]);
    assert!(o.status.success());
    let j = out(&o);
    assert!(
        j.contains("\"virtual_size\":12288") && j.contains("\"openable\":true"),
        "{j}"
    );
    assert!(
        j.contains("\"type\":\"ZERO\"") && j.contains("\"file_name\":null"),
        "{j}"
    );

    // Sin --extent: no se sigue la referencia.
    let o = cli(&["image-info", d]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("d-f001.vmdk"), "{}", err(&o));
    // Falta una extensión.
    let o = cli(&["image-info", d, "--extent", &e1]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("d-f002.vmdk"), "{}", err(&o));
    // Abierto con las dos.
    let o = cli(&["image-info", d, "--extent", &e1, "--extent", &e2, "--json"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("12288"), "{}", out(&o));
    let o = cli(&["hash", d, "--extent", &e1, "--extent", &e2]);
    assert!(o.status.success(), "{}", err(&o));
    // Mismo hash que el RAW equivalente.
    let mut all = a.clone();
    all.extend_from_slice(&b);
    all.extend_from_slice(&[0u8; 8 * 512]);
    let raw = dir.path().join("equivalente.raw");
    std::fs::write(&raw, &all).unwrap();
    let r = cli(&["hash", raw.to_str().unwrap()]);
    assert!(r.status.success());
    let digest = |s: String| {
        s.lines()
            .find(|l| l.to_lowercase().contains("sha256"))
            .map(|l| l.split_whitespace().last().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    let expected = digest(out(&r));
    assert_eq!(expected.len(), 64, "{}", out(&r));
    assert_eq!(digest(out(&o)), expected);

    // Errores de uso: valor mal formado, --extents-from-dir rechazado, combinaciones.
    for bad in [
        vec!["image-info", d, "--extent", "sin-igual"],
        vec!["image-info", d, "--extent", "=x"],
        vec!["image-info", d, "--extents-from-dir"],
    ] {
        let o = cli(&bad);
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
    }
    let o = cli(&["image-info", d, "--extents-from-dir"]);
    assert!(err(&o).contains("uno a uno"), "{}", err(&o));
    let o = cli(&[
        "image-info",
        d,
        "--extent",
        &e1,
        "--extent",
        &e2,
        "--force-raw",
    ]);
    assert_eq!(o.status.code(), Some(1));
    // Nombre que no está en el descriptor.
    let wrong = format!("otro.vmdk={}", dir.path().join("d-f001.vmdk").display());
    let o = cli(&["image-info", d, "--extent", &wrong, "--extent", &e2]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(std::fs::read(dir.path().join("d-f001.vmdk")).unwrap(), a);
}
