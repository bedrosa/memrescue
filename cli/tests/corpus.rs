//! Pruebas cruzadas contra el corpus sintético (tests/corpus).
//!
//! Requieren haber generado las imágenes (`scripts/test.ps1` lo hace). Se marcan `#[ignore]` para
//! que `cargo test` sin corpus no falle; el script las ejecuta explícitamente con `--ignored`.
//! Comprueban que el motor (Rust) coincide con implementaciones independientes (Python):
//! SHA-256 (`sha2` vs `hashlib`) y CRC-32 (`crc32fast` vs `zlib`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("corpus")
}

/// Lee las claves de nivel superior de un manifiesto TOML simple (antes de la primera tabla).
fn top_level(manifest: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(manifest).expect("manifiesto legible");
    text.lines()
        .take_while(|l| !l.starts_with('['))
        .filter_map(|l| l.split_once(" = "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

fn manifests() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root().join("manifests"))
        .expect("directorio de manifiestos")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no hay manifiestos de referencia");
    v
}

fn image_path(m: &HashMap<String, String>) -> PathBuf {
    let p = root().join("generated").join(&m["file"]);
    assert!(
        p.exists(),
        "falta {}: ejecute scripts/test.ps1 o el generador",
        p.display()
    );
    p
}

fn cli(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn engine_size_and_sha256_match_independent_generator() {
    for mf in manifests() {
        let m = top_level(&mf);
        let img = image_path(&m);
        let p = img.to_str().unwrap();
        let ss = &m["sector_size"];
        let info = cli(&["image-info", p, "--sector-size", ss, "--json"]);
        assert!(
            info.contains(&format!("\"size_bytes\":{}", m["size_bytes"])),
            "{info}"
        );
        assert!(
            info.contains(&format!("\"sector_count\":{}", m["total_sectors"])),
            "{info}"
        );
        assert!(info.contains("\"trailing_bytes\":0"), "{info}");
        let h = cli(&["hash", p, "--algo", "sha256", "--json"]);
        assert!(
            h.contains(&format!("\"sha256\":\"{}\"", m["sha256"])),
            "{}: SHA-256 del motor distinto del de Python: {h}",
            m["name"]
        );
    }
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn engine_crc32_validates_gpt_headers_written_by_python() {
    let mut checked = 0;
    for mf in manifests() {
        let m = top_level(&mf);
        if m["scheme"] != "gpt" {
            continue;
        }
        let ss: usize = m["sector_size"].parse().unwrap();
        let total: usize = m["total_sectors"].parse().unwrap();
        let data = std::fs::read(image_path(&m)).unwrap();
        for lba in [1, total - 1] {
            let mut hdr = data[lba * ss..lba * ss + 92].to_vec();
            let stored = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
            hdr[16..20].fill(0);
            assert_eq!(
                mr_common::hash::crc32(&hdr),
                stored,
                "{} LBA {lba}",
                m["name"]
            );
            checked += 1;
        }
        let entries_lba: usize = m["entries_lba"].parse().unwrap();
        let n: usize = m["entry_count"].parse().unwrap();
        let es: usize = m["entry_size"].parse().unwrap();
        let table = &data[entries_lba * ss..entries_lba * ss + n * es];
        assert_eq!(
            format!("{:08x}", mr_common::hash::crc32(table)),
            m["entries_crc32"]
        );
    }
    assert_eq!(
        checked, 4,
        "se esperaban 2 imágenes GPT con 2 cabeceras cada una"
    );
}
