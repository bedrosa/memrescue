//! `ntfs-info` contra el corpus NTFS (ADR-024; esquema en `docs/PRUEBAS.md` §1.2).
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`;
//! `MR_NTFS_CORPUS` para otra ubicación). Se ejecutan con
//! `cargo test -p memrescue-cli --test ntfs_corpus -- --ignored`. Las comparaciones completas con
//! los manifiestos están en `core/mr-fs-ntfs/tests/ntfs_corpus.rs`; aquí se comprueba la salida de
//! la CLI (texto y JSON) para cada imagen, las particionadas con `--partition 1`.

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

/// Claves de nivel superior y de `[volume]` (`volume.clave`) y si tiene `[partition]`.
fn manifest(path: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out = HashMap::new();
    let mut prefix = String::new();
    for l in text.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            prefix = l.trim_matches(['[', ']']).to_string();
            out.entry(format!("has.{prefix}"))
                .or_insert_with(String::new);
            continue;
        }
        if prefix.is_empty() || prefix == "volume" || prefix == "damage" {
            if let Some((k, v)) = l.split_once(" = ") {
                let key = if prefix.is_empty() {
                    k.to_string()
                } else {
                    format!("{prefix}.{k}")
                };
                out.insert(key, v.trim_matches('"').to_string());
            }
        }
    }
    out
}

fn run(args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8(o.stdout).unwrap() + &String::from_utf8_lossy(&o.stderr),
    )
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn ntfs_info_matches_every_manifest() {
    let mut dir: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/ntfs/manifests"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dir.sort();
    assert_eq!(dir.len(), 10);
    for mf in dir {
        let m = manifest(&mf);
        let img = generated().join(&m["file"]);
        assert!(
            img.exists(),
            "falta {}: tools/fetch_ntfs_corpus.py",
            img.display()
        );
        let img = img.to_str().unwrap().to_string();
        let mut args = vec!["ntfs-info", img.as_str(), "--json"];
        if m.contains_key("has.partition") {
            args.extend(["--partition", "1"]);
        }
        let (code, out) = run(&args);
        let name = &m["name"];
        assert_eq!(code, 0, "{name}: {out}");
        let has = |s: String| assert!(out.contains(&s), "{name}: falta {s} en {out}");
        has(format!("\"serial\":\"{}\"", m["volume.serial"]));
        has(format!("\"label\":\"{}\"", m["volume.label"]));
        has("\"version\":\"3.1\"".into());
        has(format!("\"mft_lcn\":{},", m["volume.mft_lcn"]));
        has(format!("\"mftmirr_lcn\":{},", m["volume.mftmirr_lcn"]));
        has(format!("\"cluster_size\":{},", m["volume.cluster_size"]));
        has(format!("\"total_sectors\":{},", m["volume.total_sectors"]));
        let entries: u64 = m["volume.mft_entries"].parse().unwrap();
        has(format!("\"record_count\":{}}}", entries - 1));
        has("\"unreadable_records\":[]".into());
        match m.get("damage.kind").map(String::as_str) {
            None => {
                has("\"boot_source\":\"primary\"".into());
                has("\"state\":\"matches\"".into());
                has("\"mismatches\":[]".into());
                has("\"damaged_records\":[]".into());
                has("\"confidence\":\"valid\"".into());
            }
            Some("boot") => {
                has(format!(
                    "\"boot_source\":\"backup\",\"boot_offset\":{}",
                    m["volume.backup_boot_offset"]
                ));
                has("\"primary_boot\":\"invalid\"".into());
                has("\"code\":\"boot_primary_invalid\"".into());
                has("\"confidence\":\"valid_with_fixes\"".into());
            }
            Some("mft") => {
                for r in [97, 110, 122, 124] {
                    has(format!(
                        "{{\"record\":{r},\"code\":\"record_bad_signature\""
                    ));
                }
                has("\"damaged\":4,".into());
            }
            Some("mftmirr") => {
                for r in 0..4 {
                    has(format!(
                        "{{\"record\":{r},\"kind\":\"mirror_invalid\",\"mft\":\"in_use\",\"mirror\":\"damaged\"}}"
                    ));
                }
                has("\"confidence\":\"valid_with_fixes\"".into());
            }
            Some(other) => panic!("{name}: {other}"),
        }
    }
    // Texto: imagen con particiones sin --partition (indica la opción) y con ella.
    let gpt = generated().join("ntfs_gpt.img");
    let gpt = gpt.to_str().unwrap();
    let (code, out) = run(&["ntfs-info", gpt]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("--partition"), "{out}");
    let (code, out) = run(&["ntfs-info", gpt, "--partition", "1"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Etiqueta:             MR_GPT"), "{out}");
    assert!(out.contains("$MFTMirr:             LCN 4095, 4 registros comparados, coinciden"));
}
