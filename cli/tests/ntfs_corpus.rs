//! `ntfs-info`, `ntfs-stat` y `ntfs-cat` contra el corpus NTFS (ADR-024; esquema en `docs/PRUEBAS.md` §1.2).
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`;
//! `MR_NTFS_CORPUS` para otra ubicación). Se ejecutan con
//! `cargo test -p memrescue-cli --test ntfs_corpus -- --ignored`. Las comparaciones completas con
//! los manifiestos están en `core/mr-fs-ntfs/tests/ntfs_corpus.rs`; aquí se comprueba la salida de
//! la CLI (texto y JSON) para cada imagen, las particionadas con `--partition 1`.

use std::collections::HashMap;

use mr_common::{HashAlgorithm, MultiHasher};
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

/// Entradas `[[files]]` con sus `[[files.ads]]`.
type Entry = (HashMap<String, String>, Vec<HashMap<String, String>>);

fn file_entries(path: &Path) -> Vec<Entry> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out: Vec<Entry> = Vec::new();
    let mut in_ads = false;
    let mut active = false;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            active = l == "[[files]]" || l == "[[files.ads]]";
            in_ads = l == "[[files.ads]]";
            if l == "[[files]]" {
                out.push((HashMap::new(), Vec::new()));
            } else if in_ads {
                out.last_mut().unwrap().1.push(HashMap::new());
            }
            continue;
        }
        if !active {
            continue;
        }
        if let Some((k, v)) = l.split_once(" = ") {
            let (k, v) = (k.to_string(), v.trim_matches('"').to_string());
            let e = out.last_mut().unwrap();
            if in_ads {
                e.1.last_mut().unwrap().insert(k, v);
            } else {
                e.0.insert(k, v);
            }
        }
    }
    out
}

fn sha256(d: &[u8]) -> String {
    let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
    h.update(d);
    h.finish().remove(0).1
}

/// Ejecuta la CLI y devuelve (código, salida estándar en bytes, salida de errores).
fn run_bytes(args: &[&str]) -> (i32, Vec<u8>, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap(),
        o.stdout,
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn image_path(name: &str) -> String {
    let p = generated().join(name);
    assert!(
        p.exists(),
        "falta {}: tools/fetch_ntfs_corpus.py",
        p.display()
    );
    p.to_str().unwrap().to_string()
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn ntfs_cat_and_stat_match_the_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for (name, part) in [
        ("ntfs_sparse_ads", false),
        ("ntfs_gpt", true),
        ("ntfs_deleted", false),
    ] {
        let img = image_path(&format!("{name}.img"));
        let mf = repo().join(format!("tests/corpus/ntfs/manifests/{name}.manifest.toml"));
        for (f, ads) in file_entries(&mf) {
            if f["type"] != "file" {
                continue;
            }
            let rec = f["mft_record"].clone();
            let mut base = vec!["ntfs-cat", img.as_str(), "--record", rec.as_str()];
            if part {
                base.extend(["--partition", "1"]);
            }
            // Flujo principal a la salida estándar.
            let (code, out, err) = run_bytes(&base);
            assert_eq!(code, 0, "{name} {}: {err}", f["path"]);
            assert_eq!(out.len().to_string(), f["size"], "{name} {}", f["path"]);
            assert_eq!(sha256(&out), f["sha256"], "{name} {}", f["path"]);
            if f["state"] == "deleted" {
                assert!(err.contains("no está en uso"), "{err}");
            }
            checked += 1;
            // Flujos alternativos a un fichero.
            for a in &ads {
                let o = dir.path().join(format!("{rec}_{}.bin", a["name"]));
                let o = o.to_str().unwrap().to_string();
                let mut args = base.clone();
                args.extend(["--stream", a["name"].as_str(), "-o", o.as_str()]);
                let (code, _, err) = run_bytes(&args);
                assert_eq!(code, 0, "{name} {}:{}: {err}", f["path"], a["name"]);
                let got = std::fs::read(&o).unwrap();
                assert_eq!(sha256(&got), a["sha256"], "{}:{}", f["path"], a["name"]);
                checked += 1;
            }
            // ntfs-stat --json: tamaño, runs y dispersos del manifiesto.
            let mut args = vec![
                "ntfs-stat",
                img.as_str(),
                "--record",
                rec.as_str(),
                "--json",
            ];
            if part {
                args.extend(["--partition", "1"]);
            }
            let (code, out, err) = run_bytes(&args);
            assert_eq!(code, 0, "{err}");
            let out = String::from_utf8(out).unwrap();
            let main_stream = format!(
                "{{\"name\":\"\",\"resident\":{},\"size\":{},",
                f["resident"], f["size"]
            );
            assert!(out.contains(&main_stream), "{main_stream} en {out}");
            if let Some(r) = f.get("runs") {
                let compact = r.replace(' ', "");
                assert!(
                    out.contains(&format!("\"runs\":{compact}")),
                    "{compact} en {out}"
                );
            }
            if let Some(sp) = f.get("sparse") {
                let compact = sp.replace(' ', "");
                assert!(out.contains(&format!("\"sparse\":{compact}")), "{compact}");
            }
            assert!(out.contains("\"anomalies\":[]"), "{out}");
        }
    }
    // ntfs_sparse_ads: 8 ficheros y 4 flujos alternativos de ficheros (el del directorio lo cubre
    // la prueba del motor); 52 de ntfs_gpt y 52 de ntfs_deleted.
    assert_eq!(checked, 116, "flujos comparados");
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn ntfs_cat_reports_unreadable_ranges_and_refuses_unsafe_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let img = image_path("ntfs_basic.img");
    // /datos/historico.bin (registro 121): 98 clústeres desde el LCN 5263 (manifiesto).
    let (code, want, _) = run_bytes(&["ntfs-cat", &img, "--record", "121"]);
    assert_eq!(code, 0);
    assert_eq!(want.len(), 400_000);
    // Copia de la imagen cortada en el clúster 5300: el resto del fichero queda fuera del origen.
    let cut = dir.path().join("cortada.img");
    let bytes = std::fs::read(&img).unwrap();
    std::fs::write(&cut, &bytes[..5300 * 4096]).unwrap();
    let cut = cut.to_str().unwrap().to_string();
    let out = dir.path().join("historico.bin");
    let out_s = out.to_str().unwrap().to_string();
    let (code, _, err) = run_bytes(&["ntfs-cat", &cut, "--record", "121", "-o", &out_s]);
    assert_eq!(code, 3, "{err}");
    let first_bad = (5300 - 5263) * 4096;
    assert!(err.contains("BAD!"), "{err}");
    assert!(err.contains(&format!("bytes {first_bad}..400000")), "{err}");
    assert!(err.contains("FUERA DEL ORIGEN"), "{err}");
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), want.len());
    assert_eq!(&got[..first_bad], &want[..first_bad]);
    for (i, b) in got[first_bad..].iter().enumerate() {
        assert_eq!(*b, b"BAD!"[(first_bad + i) % 4]);
    }

    // Salida existente: sin --overwrite, código 2 y el fichero no cambia; con él, se sustituye y
    // no quedan temporales.
    let (code, _, err) = run_bytes(&["ntfs-cat", &img, "--record", "121", "-o", &out_s]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("ya existe") && err.contains("--overwrite"),
        "{err}"
    );
    assert_eq!(std::fs::read(&out).unwrap(), got);
    let (code, _, err) = run_bytes(&[
        "ntfs-cat",
        &img,
        "--record",
        "121",
        "-o",
        &out_s,
        "--overwrite",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(std::fs::read(&out).unwrap(), want);
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".mrtmp")), "{names:?}");
    // Nunca sobre el origen ni en dispositivos (tampoco con --overwrite).
    let (code, _, err) = run_bytes(&[
        "ntfs-cat",
        &img,
        "--record",
        "121",
        "-o",
        &img,
        "--overwrite",
    ]);
    assert_eq!(code, 1, "{err}");
    let before = std::fs::metadata(&img).unwrap().len();
    let (code, _, err) = run_bytes(&["ntfs-cat", &img, "--record", "121", "-o", &img]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("propio origen"), "{err}");
    assert_eq!(std::fs::metadata(&img).unwrap().len(), before);
    for dev in [r"\\.\PhysicalDrive0", "NUL"] {
        let (code, _, err) = run_bytes(&["ntfs-cat", &img, "--record", "121", "-o", dev]);
        assert_eq!(code, 1, "{dev}: {err}");
    }
    // Errores de uso y de registro.
    let (code, _, _) = run_bytes(&["ntfs-cat", &img]);
    assert_eq!(code, 2);
    let (code, _, _) = run_bytes(&["ntfs-stat", &img, "--record", "121", "--stream", "x"]);
    assert_eq!(code, 2);
    let (code, _, err) = run_bytes(&["ntfs-cat", &img, "--record", "121", "--stream", "nada"]);
    assert_eq!(code, 1);
    assert!(err.contains("no existe el flujo"), "{err}");
    let (code, _, err) = run_bytes(&["ntfs-cat", &img, "--record", "99999"]);
    assert_eq!(code, 1);
    assert!(err.contains("no existe"), "{err}");
    let dmg = image_path("ntfs_damaged_mft.img");
    let (code, _, err) = run_bytes(&["ntfs-stat", &dmg, "--record", "97"]);
    assert_eq!(code, 1);
    assert!(err.contains("DAÑADO"), "{err}");
    // Texto de ntfs-stat de un directorio y de un fichero con flujos alternativos.
    let sp = image_path("ntfs_sparse_ads.img");
    let (code, out, _) = run_bytes(&["ntfs-stat", &sp, "--record", "72"]);
    assert_eq!(code, 0);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("«con_flujos.txt»"), "{out}");
    assert!(out.contains("«Zone.Identifier»: 26 B, residente"), "{out}");
    assert!(out.contains("«resumen»: 20000 B, no residente"), "{out}");
}
