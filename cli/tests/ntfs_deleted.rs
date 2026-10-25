//! `ntfs-deleted` y `ntfs-recover` (#23): argumentos y, con el corpus NTFS (`--ignored`;
//! `python -I -X utf8 tools/fetch_ntfs_corpus.py`, `MR_NTFS_CORPUS` para otra ubicación), la
//! salida de la CLI contra los manifiestos. Las comparaciones completas con los manifiestos están
//! en `core/mr-fs-ntfs/tests/ntfs_deleted_corpus.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use mr_common::{HashAlgorithm, MultiHasher};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

/// Ejecuta la CLI: (código, salida estándar, salida de errores).
fn run(args: &[&str]) -> (i32, String, String) {
    let o = cli().args(args).output().unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn ntfs_deleted_and_recover_validate_arguments_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("ceros.img");
    std::fs::write(&img, vec![0u8; 64 * 1024]).unwrap();
    let f = img.to_str().unwrap();
    let out = dir.path().join("salida.bin");
    let o_s = out.to_str().unwrap();
    // Origen sin NTFS: error de operación y ningún fichero de salida.
    assert_eq!(run(&["ntfs-deleted", f]).0, 1);
    assert_eq!(run(&["ntfs-recover", f, "--record", "5", "-o", o_s]).0, 1);
    assert!(!out.exists());
    for args in [
        vec!["ntfs-deleted"],
        vec!["ntfs-deleted", f, "--record", "5"],
        vec!["ntfs-deleted", f, "-o", o_s],
        vec!["ntfs-deleted", f, "--stream", "x"],
        vec!["ntfs-recover", f, "--record", "5"],
        vec!["ntfs-recover", f, "-o", o_s],
        vec!["ntfs-recover", f, "--record", "5", "-o", o_s, "--json"],
        vec!["ntfs-recover", f, "--record", "5", "--overwrite"],
    ] {
        assert_eq!(run(&args).0, 2, "{args:?}");
    }
    assert!(!out.exists());
}

/// Entradas `[[files]]` y registros de `[[damage]]` de un manifiesto.
fn manifest(name: &str) -> (Vec<HashMap<String, String>>, Vec<u64>) {
    let text = std::fs::read_to_string(
        repo().join(format!("tests/corpus/ntfs/manifests/{name}.manifest.toml")),
    )
    .unwrap();
    let mut files: Vec<HashMap<String, String>> = Vec::new();
    let mut damaged = Vec::new();
    let mut in_file = false;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_file = l == "[[files]]";
            if in_file {
                files.push(HashMap::new());
            }
            continue;
        }
        let Some((k, v)) = l.split_once(" = ") else {
            continue;
        };
        if k == "records" {
            damaged.extend(
                v.trim_matches(['[', ']'])
                    .split(',')
                    .map(|x| x.trim().parse::<u64>().unwrap()),
            );
        } else if in_file {
            files
                .last_mut()
                .unwrap()
                .insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    (files, damaged)
}

fn image(name: &str) -> String {
    let p = generated().join(format!("{name}.img"));
    assert!(
        p.exists(),
        "falta {}: tools/fetch_ntfs_corpus.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

fn sha256(d: &[u8]) -> String {
    let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
    h.update(d);
    h.finish().remove(0).1
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn ntfs_deleted_lists_the_manifest_entries() {
    for name in [
        "ntfs_deleted",
        "ntfs_overwritten",
        "ntfs_damaged_mft",
        "ntfs_basic",
    ] {
        let (files, damaged) = manifest(name);
        let img = image(name);
        let (code, out, err) = run(&["ntfs-deleted", &img, "--json"]);
        assert_eq!(code, 0, "{name}: {err}");
        assert!(out.starts_with("{\"records\":"), "{name}");
        assert!(out.contains("\"bitmap_issue\":null"), "{name}");
        // Entrada: desde {"record":N,"sequence":S,"status" hasta su "streams" (el padre, los
        // dueños, los problemas y las referencias de índice no llevan "status").
        let entry = |rec: &str| -> Option<&str> {
            let pat = format!("{{\"record\":{rec},\"sequence\":");
            out.match_indices(&pat).find_map(|(i, _)| {
                let rest = &out[i + pat.len()..];
                let digits = rest.find(|c: char| !c.is_ascii_digit())?;
                rest[digits..].starts_with(",\"status\":").then(|| {
                    let all = &out[i..];
                    &all[..all.find("\"streams\"").unwrap_or(all.len())]
                })
            })
        };
        let mut want = 0;
        for f in &files {
            let rec = &f["mft_record"];
            if f["state"] == "live" {
                assert!(entry(rec).is_none(), "{name} {}", f["path"]);
                continue;
            }
            want += 1;
            let c = entry(rec).unwrap_or_else(|| panic!("{name} {}", f["path"]));
            assert!(
                c.starts_with(&format!(
                    "{{\"record\":{rec},\"sequence\":{},\"status\":\"deleted\",\
                     \"status_message\":\"borrado\",\"origins\":[\"mft_record\"",
                    f["sequence"]
                )),
                "{name} {c}"
            );
            assert!(
                c.contains(&format!("\"type\":\"{}\"", f["type"])),
                "{name} {c}"
            );
            // sello.bin: también en el índice del directorio borrado /docs/cartas.
            let origins = if name == "ntfs_deleted" && rec == "103" {
                "[\"mft_record\",\"deleted_directory_index\"]"
            } else {
                "[\"mft_record\"]"
            };
            assert!(c.contains(&format!("\"origins\":{origins},")), "{name} {c}");
            assert!(
                c.contains(&format!("\"path\":{}", json_str(&f["path"]))),
                "{name} {c}"
            );
            if f["type"] == "file" {
                assert!(
                    c.ends_with(&format!("\"recoverable\":\"{}\",", f["recoverable"])),
                    "{name} {c}"
                );
            }
            if let Some(reason) = f.get("reason") {
                let owners = reason.split_once("asignados ahora a ").unwrap().1;
                for o in owners.split(", ") {
                    assert!(out.contains(&json_str(o)), "{name}: {o}");
                }
            }
        }
        let n = out.matches(",\"namespace\":").count();
        assert_eq!(n, want, "{name}: entradas");
        // Entradas residuales: lo que muestra el corpus (ver ntfs_deleted_corpus.rs).
        let (accepted, rejected) = if name == "ntfs_deleted" {
            (1, 4)
        } else {
            (0, 0)
        };
        assert!(out.contains("\"nodes_unreadable\":0,\"nodes_invalid\":0,\"free_nodes\":0,"));
        assert!(
            out.contains(&format!(
                "\"accepted\":{accepted},\"rejected\":{rejected},\"duplicates\":0,\
                 \"live_copies\":0,\"residual_only\":0,\"truncated\":false}}}}"
            )),
            "{name}: {}",
            &out[out.rfind("\"index_entries\"").unwrap()..]
        );
        assert!(!out.contains("pending"), "{name}");
        if name == "ntfs_deleted" {
            assert!(out.contains(
                "\"index_entries\":[{\"origin\":\"deleted_directory_index\",\
                 \"directory\":{\"record\":68,\"sequence\":2},\"vcn\":0,\"offset\":64,\
                 \"reference\":{\"record\":103,\"sequence\":1},\"name\":\"sello.bin\"}]"
            ));
        }
        for d in damaged {
            assert!(entry(&d.to_string()).is_none());
            assert!(
                out.contains(&format!("{{\"record\":{d},\"state\":\"damaged\"")),
                "{name}: {d}"
            );
        }
        // Texto.
        let (code, out, _) = run(&["ntfs-deleted", &img]);
        assert_eq!(code, 0);
        assert!(out.contains("Borrados: "), "{name}");
        assert!(out.contains("; 0 solo en índices)"), "{name}");
        assert!(
            out.contains(&format!(
                "{accepted} aceptadas (0 repetidas, 0 copias de entradas vivas), {rejected} \
                 restos rechazados"
            )),
            "{name}: {out}"
        );
        if name == "ntfs_deleted" {
            assert!(out.contains("encontrado en: registro MFT, índice de un directorio borrado"));
        }
    }
    // Volumen en una partición GPT: sin borrados.
    let (code, out, err) = run(&[
        "ntfs-deleted",
        &image("ntfs_gpt"),
        "--partition",
        "1",
        "--json",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"entries\":[]"));
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn ntfs_recover_matches_the_manifests_and_refuses_unsafe_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for name in ["ntfs_deleted", "ntfs_overwritten"] {
        let (files, _) = manifest(name);
        let img = image(name);
        for f in files
            .iter()
            .filter(|f| f["state"] != "live" && f["type"] == "file")
        {
            let out = dir.path().join(format!("{name}-{}.bin", f["mft_record"]));
            let o_s = out.to_str().unwrap();
            let (code, _, err) = run(&[
                "ntfs-recover",
                &img,
                "--record",
                &f["mft_record"],
                "-o",
                o_s,
            ]);
            let data = std::fs::read(&out).unwrap();
            assert_eq!(data.len().to_string(), f["size"], "{name} {}", f["path"]);
            if f["recoverable"] == "full" {
                assert_eq!(code, 0, "{name} {}: {err}", f["path"]);
                assert_eq!(sha256(&data), f["sha256"], "{name} {}", f["path"]);
                assert!(!err.contains("POSIBLEMENTE"), "{err}");
            } else {
                assert_eq!(code, 3, "{name} {}: {err}", f["path"]);
                assert_eq!(sha256(&data), f["recovered_sha256"], "{name} {}", f["path"]);
                assert!(err.contains("POSIBLEMENTE SOBRESCRITOS"), "{err}");
                let owners = f["reason"].split_once("asignados ahora a ").unwrap().1;
                for o in owners.split(", ") {
                    assert!(err.contains(o), "{err}");
                }
                assert!(err.contains(&format!(
                    "{} de {} clústeres posiblemente sobrescritos",
                    f["overwritten_clusters"], f["clusters"]
                )));
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 15);
    // Salida existente: código 2 sin --overwrite y sustituida con él.
    let img = image("ntfs_deleted");
    let out = dir.path().join("ntfs_deleted-83.bin");
    let o_s = out.to_str().unwrap();
    assert_eq!(
        run(&["ntfs-recover", &img, "--record", "83", "-o", o_s]).0,
        2
    );
    std::fs::write(&out, b"anterior").unwrap();
    assert_eq!(
        run(&[
            "ntfs-recover",
            &img,
            "--record",
            "83",
            "-o",
            o_s,
            "--overwrite"
        ])
        .0,
        0
    );
    assert_eq!(std::fs::read(&out).unwrap().len(), 4095);
    // La salida no puede ser el origen.
    let (code, _, _) = run(&[
        "ntfs-recover",
        &img,
        "--record",
        "83",
        "-o",
        &img,
        "--overwrite",
    ]);
    assert_eq!(code, 1);
    // Registro dañado: error y ninguna salida.
    let bad = dir.path().join("dañado.bin");
    let (code, _, err) = run(&[
        "ntfs-recover",
        &image("ntfs_damaged_mft"),
        "--record",
        "122",
        "-o",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no es utilizable"), "{err}");
    assert!(!bad.exists());
    // Sin temporales.
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".mrtmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
