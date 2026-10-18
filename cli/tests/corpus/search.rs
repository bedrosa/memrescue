//! Búsqueda exprés de particiones perdidas (#15) por la CLI frente al corpus: `find-partitions
//! --json` de cada manifiesto comparado con sus `[[found_volumes]]` y apertura de volúmenes con
//! `--found` (con y sin `--apply-shift`). Las imágenes con `unreadable_lbas` se excluyen de la
//! comparación de estados: un fichero RAW no puede tener sectores ilegibles (lo comprueba
//! `core/mr-part/tests/search_corpus.rs` con `FaultyDevice`).

use super::{image_path, manifests, top_level};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap()
}

/// `[[found_volumes]]` del manifiesto como mapas de claves.
fn found_volumes(manifest: &Path) -> Vec<HashMap<String, String>> {
    let text = std::fs::read_to_string(manifest).unwrap();
    let mut out: Vec<HashMap<String, String>> = Vec::new();
    let mut inside = false;
    for l in text.lines() {
        if l.starts_with('[') {
            inside = l == "[[found_volumes]]";
            if inside {
                out.push(HashMap::new());
            }
            continue;
        }
        if let (true, Some((k, v))) = (inside, l.split_once(" = ")) {
            out.last_mut()
                .unwrap()
                .insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    out
}

fn state(s: &str) -> &str {
    match s {
        "valid" => "yes",
        "damaged" => "no",
        "unreadable" => "unverifiable",
        other => other,
    }
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn find_partitions_json_matches_every_manifest() {
    let mut compared = 0;
    for mf in manifests() {
        let m = top_level(&mf);
        if m.contains_key("unreadable_lbas") {
            continue;
        }
        let img = image_path(&m);
        let o = run(&[
            "find-partitions",
            img.to_str().unwrap(),
            "--sector-size",
            &m["sector_size"],
            "--json",
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let json = String::from_utf8(o.stdout).unwrap();
        assert!(json.starts_with("{\"status\":\"complete\""), "{json}");
        let expected = found_volumes(&mf);
        assert_eq!(
            json.matches("\"number\":").count(),
            expected.len(),
            "{}: {json}",
            m["name"]
        );
        for (i, e) in expected.iter().enumerate() {
            let want = [
                format!("\"number\":{},\"fs\":\"{}\"", i + 1, e["fs"]),
                format!(
                    "\"start_lba\":{},\"sectors\":{},\"end_lba\":{}",
                    e["start_lba"], e["sectors"], e["end_lba"]
                ),
                format!(
                    "\"primary\":\"{}\",\"backup\":\"{}\"",
                    state(&e["primary_boot"]),
                    state(&e["backup_boot"])
                ),
                format!("\"evidence\":{},\"partial\":{}", e["evidence"].replace(", ", ","), e["partial"]),
                format!(
                    "\"boot_lba\":{},\"backup_boot_lba\":{},\"hidden_sectors\":{},\"serial\":\"{}\",\"suggested_mbr_type\":{}",
                    e["boot_lba"], e["backup_boot_lba"], e["hidden_sectors"], e["serial"], e["suggested_mbr_type"]
                ),
            ];
            for w in want {
                assert!(json.contains(&w), "{}: falta {w}\n{json}", m["name"]);
            }
            compared += 1;
        }
    }
    assert_eq!(compared, 16, "volúmenes comparados por la CLI");
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn found_option_opens_volumes_with_the_shift_rule() {
    let by_name: HashMap<String, HashMap<String, String>> = manifests()
        .iter()
        .map(|mf| {
            let m = top_level(mf);
            (m["name"].clone(), m)
        })
        .collect();
    let img = |n: &str| image_path(&by_name[n]).to_str().unwrap().to_string();
    // Volumen perdido: su primer sector es el arranque NTFS («NTFS» en el desplazamiento 3).
    let p = img("lost_ntfs_mbr_zeroed");
    let o = run(&["sectors", &p, "--found", "1", "--lba", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("EB 52 90 4E 54 46 53 20"));
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("Volumen encontrado 1: Volumen encontrado NTFS LBA 2048–4095"));
    let o = run(&["verify", &p, "--found", "1", "--json"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"sectors\":2048,\"ok\":2048"));
    // Desplazada: solo con --apply-shift.
    let p = img("image_shifted_pos");
    let o = run(&["image-info", &p, "--found", "2", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("desplazada 8 sectores"));
    let o = run(&["image-info", &p, "--found", "2", "--apply-shift", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("desplazamiento detectado aplicado"));
    // Parcial: nunca.
    let p = img("image_shifted_neg");
    let o = run(&["image-info", &p, "--found", "1", "--apply-shift"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("parcial"));
    // 4Kn leído como 512: se abre en bytes exactos e indica el sector del FS.
    let p = img("gpt_4kn_lost");
    let o = run(&[
        "image-info",
        &p,
        "--sector-size",
        "512",
        "--found",
        "1",
        "--json",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let info = String::from_utf8_lossy(&o.stdout);
    assert!(
        info.contains("\"sector_count\":2048") && info.contains("sectores de 4096 B"),
        "{info}"
    );
}
