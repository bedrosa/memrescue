//! Pruebas cruzadas contra el corpus sintético (tests/corpus).
//!
//! Requieren haber generado las imágenes (`scripts/test.ps1` lo hace). Se marcan `#[ignore]` para
//! que `cargo test` sin corpus no falle; el script las ejecuta explícitamente con `--ignored`.
//! Comprueban que el motor (Rust) coincide con implementaciones independientes (Python):
//! SHA-256 (`sha2` vs `hashlib`) y CRC-32 (`crc32fast` vs `zlib`). Las tablas de particiones
//! se comparan con los manifiestos en el módulo `partitions`.

#[path = "corpus/partitions.rs"]
mod partitions;

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

/// (LBA de la imagen, marcador) de cada `[[partitions]]` del manifiesto. Si la imagen está
/// desplazada, el marcador está en `marker_lba` (coordenadas de la imagen) en vez de en `start_lba`.
fn partition_markers(manifest: &Path) -> Vec<(u64, String)> {
    let text = std::fs::read_to_string(manifest).unwrap();
    let mut out = Vec::new();
    let mut in_partition = false;
    let mut lba = None;
    for l in text.lines() {
        if l.starts_with('[') {
            in_partition = l == "[[partitions]]";
            lba = None;
            continue;
        }
        if !in_partition {
            continue;
        }
        if let Some(v) = l
            .strip_prefix("start_lba = ")
            .or_else(|| l.strip_prefix("marker_lba = "))
        {
            lba = Some(v.parse().unwrap());
        }
        if let Some(v) = l.strip_prefix("marker = ") {
            out.push((lba.take().unwrap(), v.trim_matches('"').to_string()));
        }
    }
    out
}

/// LBA de la imagen donde el manifiesto declara una cabecera GPT con firma (válida o con CRC
/// incorrecta) y su estado (`valid` o `crc`).
fn gpt_headers(m: &HashMap<String, String>) -> Vec<(usize, String)> {
    ["primary", "backup"]
        .iter()
        .filter_map(|tag| {
            let lba = m.get(&format!("gpt_{tag}_header_image_lba"))?;
            Some((
                lba.parse().unwrap(),
                m[&format!("gpt_{tag}_header")].clone(),
            ))
        })
        .collect()
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn sector_reads_land_on_the_exact_lba_for_every_sector_size() {
    let mut checked = 0;
    for mf in manifests() {
        let m = top_level(&mf);
        let img = image_path(&m);
        let p = img.to_str().unwrap();
        let ss = &m["sector_size"];
        for (lba, marker) in partition_markers(&mf) {
            let out = cli(&["sectors", p, "--lba", &lba.to_string(), "--sector-size", ss]);
            // El volcado parte el texto en líneas de 16 bytes: se comprueban los 16 primeros.
            assert!(
                out.contains(&marker[..16]),
                "{}: LBA {lba} sin marcador\n{out}",
                m["name"]
            );
            checked += 1;
        }
        for (lba, _) in gpt_headers(&m) {
            let out = cli(&["sectors", p, "--lba", &lba.to_string(), "--sector-size", ss]);
            let off: u64 = ss.parse::<u64>().unwrap() * lba as u64;
            assert!(
                out.contains(&format!("{off:012X}  45 46 49 20 50 41 52 54")),
                "{}: la LBA {lba} debe empezar por 'EFI PART'\n{out}",
                m["name"]
            );
        }
        let v = cli(&["verify", p, "--sector-size", ss, "--json"]);
        assert!(
            v.contains(&format!(
                "\"sectors\":{},\"ok\":{},\"bad\":0",
                m["total_sectors"], m["total_sectors"]
            )),
            "{v}"
        );
    }
    assert_eq!(
        checked, 44,
        "marcadores de las particiones declaradas en los manifiestos"
    );
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn engine_crc32_validates_gpt_headers_written_by_python() {
    let (mut valid, mut damaged, mut tables) = (0, 0, 0);
    for mf in manifests() {
        let m = top_level(&mf);
        let headers = gpt_headers(&m);
        if headers.is_empty() {
            continue;
        }
        let ss: usize = m["sector_size"].parse().unwrap();
        let data = std::fs::read(image_path(&m)).unwrap();
        for (lba, state) in headers {
            let mut hdr = data[lba * ss..lba * ss + 92].to_vec();
            let stored = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
            hdr[16..20].fill(0);
            let ok = mr_common::hash::crc32(&hdr) == stored;
            match state.as_str() {
                "valid" => valid += 1,
                "crc" => damaged += 1,
                other => panic!("{}: estado inesperado {other}", m["name"]),
            }
            assert_eq!(ok, state == "valid", "{} LBA {lba} ({state})", m["name"]);
        }
        // Tabla de entradas de la copia elegida, en coordenadas de la imagen.
        let entries_key = match m["gpt_selected"].as_str() {
            "primary" => "entries_lba",
            "backup" => "backup_entries_lba",
            _ => continue,
        };
        let shift: usize = m["leading_sectors_missing"].parse().unwrap();
        let at = (m[entries_key].parse::<usize>().unwrap() - shift) * ss;
        let n: usize = m["entry_count"].parse().unwrap();
        let es: usize = m["entry_size"].parse().unwrap();
        assert_eq!(
            format!("{:08x}", mr_common::hash::crc32(&data[at..at + n * es])),
            m["entries_crc32"],
            "{}",
            m["name"]
        );
        tables += 1;
    }
    assert_eq!(
        (valid, damaged, tables),
        (16, 4, 11),
        "cabeceras GPT válidas, cabeceras con CRC incorrecta y tablas de entradas comprobadas"
    );
}
