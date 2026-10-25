//! `vfs-ls` (#25) contra el corpus NTFS (ADR-024; esquema en `docs/PRUEBAS.md` §1.2).
//!
//! Las pruebas de corpus requieren las imágenes descargadas
//! (`python -I -X utf8 tools/fetch_ntfs_corpus.py`; `MR_NTFS_CORPUS` para otra ubicación) y se
//! ejecutan con `cargo test -p memrescue-cli --test vfs_corpus -- --ignored`. Las comparaciones
//! completas con los manifiestos están en `core/mr-vfs/tests/vfs_corpus.rs`; aquí se comprueba la
//! salida de la CLI (texto y JSON), las opciones y los códigos de salida.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Entradas `[[files]]` de un manifiesto y si la imagen está particionada.
fn files(name: &str) -> (bool, Vec<HashMap<String, String>>) {
    let text = std::fs::read_to_string(
        repo().join(format!("tests/corpus/ntfs/manifests/{name}.manifest.toml")),
    )
    .unwrap();
    let mut out = Vec::new();
    let mut partitioned = false;
    let mut cur: Option<HashMap<String, String>> = None;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            out.extend(cur.take());
            partitioned |= l == "[partition]";
            if l == "[[files]]" {
                cur = Some(HashMap::new());
            }
            continue;
        }
        if let (Some(c), Some((k, v))) = (cur.as_mut(), l.split_once(" = ")) {
            c.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    out.extend(cur);
    (partitioned, out)
}

fn image(name: &str) -> String {
    let p = generated().join(format!("{name}.img"));
    assert!(
        p.exists(),
        "falta {}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

/// Valores de `"clave":"valor"` de un JSON plano (sin escapes en los valores buscados).
fn json_values(json: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\":\"");
    json.match_indices(&pat)
        .map(|(i, _)| {
            let rest = &json[i + pat.len()..];
            rest[..rest.find('"').unwrap()].to_string()
        })
        .collect()
}

#[test]
fn vfs_options_are_validated() {
    let (rc, _, err) = run(&["ntfs-info", "x.img", "--deleted"]);
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("solo se aplican a vfs-ls"));
    let (rc, _, err) = run(&["vfs-ls"]);
    assert_eq!(rc, 2, "{err}");
    let (rc, _, err) = run(&["vfs-ls", "x.img", "--filter"]);
    assert_eq!(rc, 2, "{err}");
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn vfs_ls_lists_every_manifest_entry_with_its_state() {
    for name in [
        "ntfs_basic",
        "ntfs_deleted",
        "ntfs_overwritten",
        "ntfs_damaged_mft",
        "ntfs_mbr",
        "ntfs_gpt",
    ] {
        let (partitioned, entries) = files(name);
        let img = image(name);
        let mut args = vec!["vfs-ls", img.as_str(), "/", "-r", "--json"];
        if partitioned {
            args.extend(["--partition", "1"]);
        }
        let (rc, out, err) = run(&args);
        assert_eq!(rc, 0, "{name}: {err}");
        let paths = json_values(&out, "path");
        let data_by_path: HashMap<String, String> = {
            // Cada nodo es un objeto con "path" y después "data"; se emparejan en orden.
            let mut m = HashMap::new();
            for obj in out.split("{\"id\":").skip(2) {
                let p = json_values(obj, "path");
                let d = json_values(obj, "data");
                let c = json_values(obj, "category");
                if let Some(p) = p.first() {
                    m.insert(
                        p.clone(),
                        format!(
                            "{}/{}",
                            c.first().cloned().unwrap_or_default(),
                            d.first().cloned().unwrap_or_default()
                        ),
                    );
                }
            }
            m
        };
        for f in &entries {
            let path = &f["path"];
            assert_eq!(
                paths.iter().filter(|p| *p == path).count(),
                1,
                "{name}: {path}"
            );
            if f["type"] == "dir" {
                continue;
            }
            let got = &data_by_path[path];
            let want = match (
                f["state"].as_str(),
                f.get("recoverable").map(String::as_str),
            ) {
                ("live", _) if f.get("mft_damaged").is_some() => "vivo/no_disponible",
                ("live", _) => "vivo/integro",
                (_, Some("full")) => "borrado/integro_no_verificable",
                (_, Some("partial")) => "borrado/parcial",
                _ => "borrado/sobrescrito",
            };
            assert_eq!(got, want, "{name}: {path}");
        }
        assert!(out.contains("\"partial\":false"), "{name}");
    }
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn vfs_ls_filters_deleted_masks_and_states() {
    let img = image("ntfs_overwritten");
    // Texto: solo borrados, con las carpetas que llevan a ellos.
    let (rc, out, err) = run(&["vfs-ls", &img, "-r", "--deleted"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.starts_with("CATEGORÍA"));
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert!(lines
        .iter()
        .all(|l| l.starts_with("borrado") || l.trim_end().ends_with('/')));
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("borrado")).count(),
        5,
        "{out}"
    );
    assert!(out.contains("sobrescrito"));
    assert!(err.contains("entradas listadas"));
    // Estados y máscaras.
    let (rc, out, _) = run(&["vfs-ls", &img, "-r", "--state", "sobrescrito", "--json"]);
    assert_eq!(rc, 0);
    let names = json_values(&out, "name");
    assert!(names.contains(&"objetivo_1.bin".to_string()));
    assert!(names.contains(&"objetivo_2.bin".to_string()));
    assert!(!names.contains(&"objetivo_3.bin".to_string()));
    let (rc, out, _) = run(&["vfs-ls", &img, "/sobrescritura", "--filter", "!*.bin"]);
    assert_eq!(rc, 0);
    assert!(
        out.contains("superviviente_1.txt") && !out.contains(".bin"),
        "{out}"
    );
    let (rc, out, _) = run(&["vfs-ls", &img, "/", "--category", "metadatos", "-r"]);
    assert_eq!(rc, 0);
    assert!(out.contains("/[Metadatos]/$MFT"));
    assert!(!out.contains("/sobrescritura"));
    // Un fichero se lista solo; una ruta que no existe es un error de operación.
    let (rc, out, _) = run(&["vfs-ls", &img, "/sobrescritura/objetivo_3.bin"]);
    assert_eq!(rc, 0);
    assert!(out.contains("parcial"));
    let (rc, _, err) = run(&["vfs-ls", &img, "/no/existe"]);
    assert_eq!(rc, 1, "{err}");
    let (rc, _, err) = run(&["vfs-ls", &img, "--category", "otra"]);
    assert_eq!(rc, 2, "{err}");
    let (rc, _, err) = run(&["vfs-ls", &img, "--filter", "!"]);
    assert_eq!(rc, 2, "{err}");
}
