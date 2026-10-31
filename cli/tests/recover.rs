//! `recover` (#26) de extremo a extremo: volumen NTFS sintético (siempre) y corpus NTFS
//! (`--ignored`, requiere `python -I -X utf8 tools/fetch_ntfs_corpus.py`). Se comprueban el árbol
//! escrito, los hashes del manifiesto, el diario, los códigos de salida (0 íntegro, 3 algún
//! fichero no íntegro, 2 uso, 1 error) y que el origen no cambia.

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

fn sha256(data: &[u8]) -> String {
    let mut h = mr_common::MultiHasher::new(&[mr_common::HashAlgorithm::Sha256]);
    h.update(data);
    h.finish().pop().unwrap().1
}

fn journal_of(stdout: &str) -> String {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Diario: "))
        .expect("ruta del diario");
    std::fs::read_to_string(line).unwrap()
}

fn synthetic(dir: &Path) -> String {
    let p = dir.join("arbol.img");
    std::fs::write(&p, mr_fs_ntfs::testvol::tree_volume(true).image).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn usage_errors_exit_with_2() {
    let dir = tempfile::tempdir().unwrap();
    let img = synthetic(dir.path());
    let to = dir.path().join("d");
    let to = to.to_str().unwrap();
    for args in [
        vec!["recover", img.as_str()],
        vec!["recover", "--to", to],
        vec!["recover", img.as_str(), "--to", to, "--conflict", "otra"],
        vec![
            "recover",
            img.as_str(),
            "--to",
            to,
            "--conflict",
            "overwrite",
        ],
        vec!["recover", img.as_str(), "--to", to, "--confirm-overwrite"],
        vec!["recover", img.as_str(), "--to", to, "--json"],
        vec!["recover", img.as_str(), "--to", to, "--category", "otra"],
        vec!["vfs-ls", img.as_str(), "--to", to],
        vec!["ntfs-info", img.as_str(), "--include-overwritten"],
    ] {
        let (code, _, err) = run(&args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
    assert!(!Path::new(to).exists(), "nada escrito en errores de uso");
}

#[test]
fn synthetic_volume_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let img = synthetic(dir.path());
    let before = sha256(&std::fs::read(&img).unwrap());
    let to = dir.path().join("recuperado");
    let to_s = to.to_str().unwrap();
    // Solo /Docs/Sub: todo vivo e íntegro → 0.
    let (code, out, err) = run(&["recover", &img, "--to", to_s, "/Docs/Sub"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert_eq!(std::fs::read(to.join("Sub/deep.txt")).unwrap(), b"profundo");
    let j = journal_of(&out);
    assert!(j.lines().next().unwrap().contains("\"tipo\":\"cabecera\""));
    assert!(j
        .lines()
        .last()
        .unwrap()
        .contains("\"estado\":\"completa\""));
    // Todo el volumen: hay un registro dañado (omitido por su estado) → 3.
    let (code, out, err) = run(&["recover", &img, "--to", to_s]);
    assert_eq!(code, 3, "{out}\n{err}");
    assert_eq!(
        std::fs::read(to.join("Docs/ñandú.bin")).unwrap(),
        mr_fs_ntfs::testvol::tree_data()
    );
    // Reanudar con skip-identical: nada nuevo.
    let journal = dir.path().join("diario.jsonl");
    let (code, out, err) = run(&[
        "recover",
        &img,
        "--to",
        to_s,
        "--conflict",
        "skip-identical",
        "--journal",
        journal.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{out}\n{err}");
    assert!(out.contains(" 0 escritos"), "{out}");
    let j = std::fs::read_to_string(&journal).unwrap();
    assert!(j.contains("\"accion\":\"omitido_identico\""));
    // El diario nunca se sustituye.
    let (code, _, err) = run(&[
        "recover",
        &img,
        "--to",
        to_s,
        "--journal",
        journal.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{err}");
    // Destino prohibido: la propia imagen, dentro de ella o un dispositivo.
    for bad in [
        img.clone(),
        format!("{img}\\dentro"),
        r"\\.\PhysicalDrive0".to_string(),
    ] {
        let (code, _, err) = run(&["recover", &img, "--to", &bad]);
        assert_eq!(code, 1, "{bad}: {err}");
    }
    // Filtro y borrados.
    let to2 = dir.path().join("filtrado");
    let (code, out, err) = run(&[
        "recover",
        &img,
        "--to",
        to2.to_str().unwrap(),
        "--filter",
        "*.txt;!*",
        "--category",
        "vivo",
    ]);
    assert_eq!(code, 3, "{out}\n{err}");
    assert!(to2.join("a.txt").exists());
    assert!(!to2.join("Docs/ñandú.bin").exists());
    assert_eq!(
        sha256(&std::fs::read(&img).unwrap()),
        before,
        "origen intacto"
    );
}

// ---------------------------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------------------------

fn manifest(name: &str) -> (Option<String>, Vec<HashMap<String, String>>) {
    let text = std::fs::read_to_string(
        repo().join(format!("tests/corpus/ntfs/manifests/{name}.manifest.toml")),
    )
    .unwrap();
    let mut out = Vec::new();
    let mut part = None;
    let mut cur: Option<HashMap<String, String>> = None;
    let mut in_part = false;
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            out.extend(cur.take());
            in_part = l == "[partition]";
            if l == "[[files]]" {
                cur = Some(HashMap::new());
            }
            continue;
        }
        if in_part {
            if let Some(i) = l.strip_prefix("index = ") {
                part = Some((i.parse::<usize>().unwrap() + 1).to_string());
            }
        }
        if let (Some(c), Some((k, v))) = (cur.as_mut(), l.split_once(" = ")) {
            c.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    out.extend(cur);
    (part, out)
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

#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn corpus_live_and_deleted_recovery() {
    for name in ["ntfs_basic", "ntfs_gpt", "ntfs_deleted", "ntfs_overwritten"] {
        let (part, files) = manifest(name);
        let img = image(name);
        let before = sha256(&std::fs::read(&img).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().to_str().unwrap().to_string();
        let mut args: Vec<&str> = vec!["recover", &img, "--to", &to, "--category", "vivo"];
        if let Some(p) = &part {
            args.extend(["--partition", p.as_str()]);
        }
        let (code, out, err) = run(&args);
        assert_eq!(code, 0, "{name}: {out}\n{err}");
        let mut n = 0;
        for f in files
            .iter()
            .filter(|f| f["state"] == "live" && f["type"] == "file")
        {
            let p = dir.path().join(f["path"].trim_start_matches('/'));
            assert_eq!(
                sha256(&std::fs::read(&p).unwrap()),
                f["sha256"],
                "{name}: {p:?}"
            );
            n += 1;
        }
        assert!(n > 40, "{name}");
        // Borrados: el código es 3 (ninguno es íntegro) y los recuperables dan su SHA-256.
        if files.iter().any(|f| f["state"] != "live") {
            let dir = tempfile::tempdir().unwrap();
            let to = dir.path().to_str().unwrap().to_string();
            let (code, out, err) = run(&["recover", &img, "--to", &to, "--deleted"]);
            assert_eq!(code, 3, "{name}: {out}\n{err}");
            for f in files
                .iter()
                .filter(|f| f["state"] != "live" && f["type"] == "file")
            {
                let p = dir.path().join(f["path"].trim_start_matches('/'));
                match f["recoverable"].as_str() {
                    "full" => assert_eq!(sha256(&std::fs::read(&p).unwrap()), f["sha256"]),
                    "none" => assert!(!p.exists(), "{name}: {p:?}"),
                    _ => assert!(PathBuf::from(format!("{}.mrbad", p.display())).exists()),
                }
            }
            let j = journal_of(&out);
            assert!(
                !j.contains("\"estado\":\"integro\","),
                "{name}: un borrado como íntegro"
            );
            if name == "ntfs_overwritten" {
                let dir = tempfile::tempdir().unwrap();
                let to = dir.path().to_str().unwrap().to_string();
                let (code, _, err) = run(&[
                    "recover",
                    &img,
                    "--to",
                    &to,
                    "--deleted",
                    "--include-overwritten",
                    "--include-reallocated-bytes",
                ]);
                assert_eq!(code, 3, "{err}");
                for f in files.iter().filter(|f| f["state"] == "overwritten") {
                    let p = dir.path().join(f["path"].trim_start_matches('/'));
                    assert_eq!(
                        sha256(&std::fs::read(&p).unwrap()),
                        f["recovered_sha256"],
                        "{p:?}"
                    );
                }
            }
        }
        assert_eq!(sha256(&std::fs::read(&img).unwrap()), before, "{name}");
    }
}
