//! Opciones `--parent` e `--ignore-parent-mismatch` de la CLI y las indicaciones de `image-info`
//! para discos diferenciales VHD/VHDX (incidencia 8).

#[path = "../../core/mr-image/src/diff_builder.rs"]
mod diff_builder;

use diff_builder::{self as db, Writes};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
}

fn run(args: &[&str]) -> Output {
    cli().args(args).output().unwrap()
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

const SIZE: u64 = 16 * 4096;

fn write(dir: &Path, name: &str, b: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b).unwrap();
    p
}

#[test]
fn image_info_shows_parent_hints_and_parent_chain_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let w0 = db::writes(0, 0..40);
    let w1 = db::writes(1, [2, 50]);
    let w2 = db::writes(2, [3]);
    let base_b = db::vhd_layer(SIZE, 4096, [0xAB; 16], None, &w0);
    let child_b = db::vhd_layer(
        SIZE,
        4096,
        [0xCD; 16],
        Some(&db::VhdParent::new([0xAB; 16], "base.vhd")),
        &w1,
    );
    let grand_b = db::vhd_layer(
        SIZE,
        4096,
        [0xEF; 16],
        Some(&db::VhdParent::new([0xCD; 16], "child.vhd")),
        &w2,
    );
    let base = write(dir.path(), "base.vhd", &base_b);
    let child = write(dir.path(), "child.vhd", &child_b);
    let grand = write(dir.path(), "grand.vhd", &grand_b);
    let other = write(
        dir.path(),
        "other.vhd",
        &db::vhd_layer(SIZE, 4096, [0x11; 16], None, &Writes::new()),
    );
    let s = |p: &PathBuf| p.to_str().unwrap().to_string();

    // Sin --parent: muestra las indicaciones, explica cómo continuar y falla (código 1).
    let o = run(&["image-info", &s(&grand)]);
    assert_eq!(o.status.code(), Some(1));
    let (out, err) = text(&o);
    assert!(out.contains("Disco diferencial"), "{out}");
    assert!(out.contains("parent_name"), "{out}");
    assert!(out.contains("child.vhd"), "{out}");
    assert!(out.contains("--parent"), "{out}");
    assert!(err.contains("no se abre automáticamente"), "{err}");
    let o = run(&["image-info", &s(&grand), "--json"]);
    let (out, _) = text(&o);
    assert!(out.starts_with("{\"path\":"), "{out}");
    assert!(out.contains("\"differencing\":true"), "{out}");
    assert!(
        out.contains("{\"key\":\"locator_W2ru\",\"value\":\".\\\\child.vhd\"}"),
        "{out}"
    );

    // Cadena completa en orden: se abre y se informa de cada padre.
    let o = run(&[
        "image-info",
        &s(&grand),
        "--parent",
        &s(&child),
        "--parent",
        &s(&base),
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(0), "{:?}", text(&o));
    let (out, err) = text(&o);
    assert!(out.contains("\"parents\":["), "{out}");
    assert!(out.contains("child.vhd\",\""), "{out}");
    assert!(out.contains("VHD diferencial"), "{out}");
    assert!(err.contains("Padre 1: "), "{err}");
    assert!(err.contains("Padre 2: "), "{err}");
    // Sectores: la capa superior manda; lo demás cae en los padres.
    let o = run(&[
        "sectors",
        &s(&grand),
        "--lba",
        "2",
        "--count",
        "2",
        "--parent",
        &s(&child),
        "--parent",
        &s(&base),
    ]);
    assert_eq!(o.status.code(), Some(0), "{:?}", text(&o));

    // Orden equivocado, padre ajeno, cadena incompleta, opciones incompatibles.
    let o = run(&["image-info", &s(&grand), "--parent", &s(&base)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).1.contains("no corresponde"), "{:?}", text(&o));
    let o = run(&["image-info", &s(&grand), "--parent", &s(&child)]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).1.contains("base.vhd"), "{:?}", text(&o));
    let o = run(&["image-info", &s(&child), "--parent", &s(&other)]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(&[
        "image-info",
        &s(&child),
        "--parent",
        &s(&other),
        "--ignore-parent-mismatch",
    ]);
    assert_eq!(o.status.code(), Some(0), "{:?}", text(&o));
    let (out, err) = text(&o);
    assert!(out.contains("NO verificado"), "{out}");
    assert!(err.contains("AVISO"), "{err}");
    for bad in [
        vec!["image-info", "x.vhd", "--ignore-parent-mismatch"],
        vec!["image-info", "x.001", "--split", "--parent", "y"],
        vec!["image-info", "x.vhd", "--force-raw", "--parent", "y"],
        vec!["image-info", "x.vhd", "--parent"],
        vec!["image-info", "x.vhd", "--parent", "y", "--extent", "a=b"],
    ] {
        assert_eq!(run(&bad).status.code(), Some(2), "{bad:?}");
    }
    let o = run(&["image-info", &s(&child), "--parent", r"\\.\PhysicalDrive0"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).1.contains("ruta no permitida"), "{:?}", text(&o));
    for (p, b) in [(&base, &base_b), (&child, &child_b), (&grand, &grand_b)] {
        assert_eq!(&std::fs::read(p).unwrap(), b);
    }
}
