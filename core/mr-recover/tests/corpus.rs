//! Recuperación contra el corpus NTFS (ADR-024, manifiestos con The Sleuth Kit como oráculo;
//! pruebas de aceptación de INV-01 §9). Requieren las imágenes descargadas
//! (`python -I -X utf8 tools/fetch_ntfs_corpus.py`, en `tests/corpus/ntfs/generated`;
//! `MR_NTFS_CORPUS` para otra ubicación). Se marcan `#[ignore]` y se ejecutan con
//! `cargo test -p mr-recover --test corpus -- --ignored`.
//!
//! Cada prueba comprueba además que el SHA-256 de la imagen de origen no cambia y que en el
//! destino no queda ningún temporal.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use mr_api::mr_recover::names::dest_name;
use mr_api::mr_recover::{RecoverOptions, RecoverSummary, RunStatus, Selection, DEFAULT_FILL};
use mr_api::mr_vfs::filter::Filter;
use mr_api::mr_vfs::{Category, DataState, NodeId, NtfsBuildOptions, Vfs};
use mr_api::task::TaskContext;
use mr_api::RecoverRequest;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

type Kv = HashMap<String, String>;

/// Entrada `[[files]]` con sus `[[files.ads]]`.
struct Entry {
    kv: Kv,
    ads: Vec<Kv>,
}

struct Manifest {
    top: Kv,
    partition: Option<Kv>,
    files: Vec<Entry>,
}

fn load(path: &Path) -> Manifest {
    let text = std::fs::read_to_string(path).expect("manifiesto legible");
    let mut m = Manifest {
        top: Kv::new(),
        partition: None,
        files: Vec::new(),
    };
    #[derive(PartialEq)]
    enum Cur {
        Top,
        Partition,
        File,
        Ads,
        Skip,
    }
    let mut cur = Cur::Top;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            cur = match line {
                "[[files]]" => {
                    m.files.push(Entry {
                        kv: Kv::new(),
                        ads: Vec::new(),
                    });
                    Cur::File
                }
                "[[files.ads]]" => {
                    m.files.last_mut().unwrap().ads.push(Kv::new());
                    Cur::Ads
                }
                "[partition]" => {
                    m.partition = Some(Kv::new());
                    Cur::Partition
                }
                _ => Cur::Skip,
            };
            continue;
        }
        let (k, v) = line.split_once(" = ").expect("clave = valor");
        let (k, v) = (k.trim().to_string(), v.trim().trim_matches('"').to_string());
        match cur {
            Cur::Top => {
                m.top.insert(k, v);
            }
            Cur::Partition => {
                m.partition.as_mut().unwrap().insert(k, v);
            }
            Cur::File => {
                m.files.last_mut().unwrap().kv.insert(k, v);
            }
            Cur::Ads => {
                m.files
                    .last_mut()
                    .unwrap()
                    .ads
                    .last_mut()
                    .unwrap()
                    .insert(k, v);
            }
            Cur::Skip => {}
        }
    }
    m
}

fn manifests() -> Vec<Manifest> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/ntfs/manifests"))
        .expect("directorio de manifiestos NTFS")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    v.sort();
    assert_eq!(v.len(), 10, "manifiestos NTFS versionados");
    v.iter().map(|p| load(p)).collect()
}

fn manifest(name: &str) -> Manifest {
    manifests()
        .into_iter()
        .find(|m| m.top["name"] == name)
        .unwrap()
}

struct Opened {
    image: PathBuf,
    src: mr_api::Source,
    vol: Arc<mr_api::mr_fs_ntfs::NtfsVolume>,
    vfs: Arc<Vfs>,
}

fn image_path(m: &Manifest) -> PathBuf {
    let p = generated().join(&m.top["file"]);
    assert!(
        p.exists(),
        "falta {}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py",
        p.display()
    );
    p
}

fn open_with(m: &Manifest, img: mr_api::Source) -> Opened {
    let src = match &m.partition {
        None => img,
        Some(part) => {
            let t = mr_api::partitions(img.as_ref()).unwrap();
            let index: usize = part["index"].parse().unwrap();
            mr_api::open_partition(&img, &t, index, false).unwrap()
        }
    };
    let vol = Arc::new(mr_api::ntfs_open(&src).unwrap());
    let vfs = mr_api::vfs_build_ntfs(&vol, &NtfsBuildOptions::default(), &TaskContext::detached())
        .unwrap();
    Opened {
        image: PathBuf::new(),
        src,
        vol,
        vfs,
    }
}

fn open(m: &Manifest) -> Opened {
    let image = image_path(m);
    let img = mr_api::open_source(&image, 0).unwrap();
    let mut o = open_with(m, img);
    o.image = image;
    o
}

fn recover(o: &Opened, sel: Selection, opts: RecoverOptions) -> RecoverSummary {
    let req = RecoverRequest {
        selection: sel,
        options: opts,
    };
    mr_api::recover(&o.src, &o.vol, &o.vfs, &req, &TaskContext::detached()).unwrap()
}

fn select(cats: &[Category]) -> Selection {
    Selection {
        nodes: vec![NodeId::ROOT],
        filter: Filter::categories(cats),
        masks: String::new(),
    }
}

/// Ruta de destino de una ruta del manifiesto (componentes escapados como hace la recuperación).
fn dest_of(root: &Path, path: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in path.split('/').filter(|c| !c.is_empty()) {
        p.push(dest_name(&units(c)).name);
    }
    p
}

fn file_sha(p: &Path) -> String {
    sha256(&std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
}

fn image_sha(p: &Path) -> String {
    sha256(&std::fs::read(p).unwrap())
}

/// Todos los ficheros vivos de cada manifiesto: el SHA-256 del manifiesto, la fecha de
/// modificación del registro, los flujos alternativos como flujos (destino NTFS), los registros
/// dañados sin fichero escrito y el código de salida.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn every_live_file_reproduces_the_manifest() {
    let mut checked = [0usize; 4];
    for m in manifests() {
        let name = m.top["name"].clone();
        let o = open(&m);
        let before = image_sha(&o.image);
        let dir = tempfile::tempdir().unwrap();
        let s = recover(
            &o,
            select(&[Category::Live]),
            RecoverOptions::new(dir.path()),
        );
        assert_eq!(s.status, RunStatus::Complete, "{name}");
        assert_eq!(s.totals.errors, 0, "{name}");
        let j = journal(Path::new(&s.journal));
        let ev = files_by_path(&j);
        let mut damaged = 0;
        for e in &m.files {
            let f = &e.kv;
            if f["state"] != "live" {
                continue;
            }
            let path = &f["path"];
            let dst = dest_of(dir.path(), path);
            if f.get("mft_damaged").is_some_and(|d| d == "true") {
                damaged += 1;
                checked[3] += 1;
                if f["type"] == "file" {
                    assert!(!dst.exists(), "{name}: {path}: contenido inventado");
                    let x = &ev[path.as_str()];
                    assert_eq!(x.get("estado").s(), "no_disponible", "{name}: {path}");
                    assert_eq!(x.get("accion").s(), "omitido_estado", "{name}: {path}");
                }
                continue;
            }
            if f["type"] == "dir" {
                assert!(dst.is_dir(), "{name}: {path}");
                // Flujos alternativos de directorios.
                for a in &e.ads {
                    let ads = format!("{}:{}", dst.display(), a["name"]);
                    assert_eq!(file_sha(Path::new(&ads)), a["sha256"], "{name}: {ads}");
                    checked[2] += 1;
                }
                continue;
            }
            checked[0] += 1;
            assert_eq!(file_sha(&dst), f["sha256"], "{name}: {path}");
            let x = &ev[path.as_str()];
            assert_eq!(x.get("estado").s(), "integro", "{name}: {path}");
            let st = &x.get("flujos").arr()[0];
            assert_eq!(st.get("hashes").get("ambito").s(), "contenido");
            assert_eq!(st.get("hashes").get("sha256").s(), f["sha256"]);
            // Fecha de modificación del registro (resolución de 100 ns en NTFS).
            let n = o.vfs.node(o.vfs.find(path).unwrap()).unwrap();
            let md = std::fs::metadata(&dst).unwrap();
            assert_eq!(
                mr_api::mr_recover::dest::system_to_filetime(md.modified().unwrap()),
                n.times.modified,
                "{name}: {path}"
            );
            assert!(x.get("fechas_aplicadas").b());
            for a in &e.ads {
                let ads = format!("{}:{}", dst.display(), a["name"]);
                assert_eq!(file_sha(Path::new(&ads)), a["sha256"], "{name}: {ads}");
                checked[1] += 1;
            }
        }
        // Partición: la cabecera da la LBA del volumen.
        if let Some(p) = &m.partition {
            assert_eq!(
                j[0].get("origen").get("lba_volumen").n().to_string(),
                p["start_lba"],
                "{name}"
            );
        }
        let want = if damaged > 0 { 3 } else { 0 };
        assert_eq!(s.exit_code(), want, "{name}: {:?}", s.totals);
        assert_no_temps(dir.path());
        assert_eq!(image_sha(&o.image), before, "{name}: origen intacto");
    }
    assert!(
        checked[0] >= 400 && checked[1] >= 4 && checked[2] >= 1 && checked[3] >= 4,
        "{checked:?}"
    );
}

/// Borrados: los íntegros dan el SHA-256 del manifiesto con estado `integro_no_verificable`;
/// los sobrescritos se omiten por defecto; el parcial se escribe con relleno y `.mrbad`; con la
/// opción se incluyen los sobrescritos (relleno) y con los bytes reasignados se reproduce
/// `recovered_sha256`, siempre con ámbito `escrito_con_relleno`.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn deleted_and_overwritten_files() {
    let mut seen = [0usize; 4];
    for m in manifests() {
        if !m.files.iter().any(|f| f.kv["state"] != "live") {
            continue;
        }
        let name = m.top["name"].clone();
        let o = open(&m);
        let dir = tempfile::tempdir().unwrap();
        let s = recover(
            &o,
            select(&[Category::Deleted]),
            RecoverOptions::new(dir.path()),
        );
        assert_eq!(s.exit_code(), 3, "{name}");
        let j = journal(Path::new(&s.journal));
        let ev = files_by_path(&j);
        for e in &m.files {
            let f = &e.kv;
            if f["state"] == "live" || f["type"] != "file" {
                continue;
            }
            let path = &f["path"];
            let dst = dest_of(dir.path(), path);
            let x = &ev[path.as_str()];
            match f["recoverable"].as_str() {
                "full" => {
                    seen[0] += 1;
                    assert_eq!(file_sha(&dst), f["sha256"], "{name}: {path}");
                    assert_eq!(
                        x.get("estado").s(),
                        "integro_no_verificable",
                        "{name}: {path}"
                    );
                    assert_eq!(
                        x.get("flujos").arr()[0].get("hashes").get("ambito").s(),
                        "contenido"
                    );
                }
                "partial" => {
                    seen[1] += 1;
                    assert_eq!(x.get("estado").s(), "parcial", "{name}: {path}");
                    let got = file_sha(&dst);
                    assert_ne!(got, f["sha256"]);
                    assert_ne!(&got, f.get("recovered_sha256").unwrap_or(&String::new()));
                    let st = &x.get("flujos").arr()[0];
                    assert_eq!(st.get("hashes").get("ambito").s(), "escrito_con_relleno");
                    assert!(st.get("bytes_relleno").n() > 0);
                    let mark = PathBuf::from(format!("{}.mrbad", dst.display()));
                    assert!(mark.exists(), "{name}: {path}");
                    // Los tramos sospechosos llevan la LBA de su clúster.
                    let r = &st.get("tramos_no_fiables").arr()[0];
                    assert_eq!(r.get("estado").s(), "possibly_overwritten");
                    assert!(r.get("lba").n() != u64::MAX);
                }
                "none" => {
                    seen[2] += 1;
                    assert!(!dst.exists(), "{name}: {path}");
                    assert_eq!(x.get("accion").s(), "omitido_estado");
                    assert!(matches!(x.get("estado").s(), "sobrescrito" | "sin_datos"));
                }
                other => panic!("{other}"),
            }
        }
        // Con los sobrescritos y, aparte, con los bytes reasignados.
        for include_bytes in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut opts = RecoverOptions::new(dir.path());
            opts.write_states |= DataState::Sobrescrito.bit();
            opts.include_reallocated_bytes = include_bytes;
            let s = recover(&o, select(&[Category::Deleted]), opts);
            let j = journal(Path::new(&s.journal));
            let ev = files_by_path(&j);
            for e in &m.files {
                let f = &e.kv;
                if f["state"] != "overwritten" || f["type"] != "file" {
                    continue;
                }
                let path = &f["path"];
                let x = &ev[path.as_str()];
                let st = &x.get("flujos").arr()[0];
                assert_eq!(st.get("hashes").get("ambito").s(), "escrito_con_relleno");
                let got = file_sha(&dest_of(dir.path(), path));
                assert_ne!(got, f["sha256"], "{name}: {path}");
                if include_bytes {
                    seen[3] += 1;
                    assert_eq!(&got, &f["recovered_sha256"], "{name}: {path}");
                    assert_eq!(st.get("bytes_relleno").n(), 0);
                } else {
                    assert_ne!(&got, &f["recovered_sha256"], "{name}: {path}");
                }
            }
        }
    }
    assert!(
        seen[0] >= 10 && seen[1] >= 1 && seen[2] >= 2 && seen[3] >= 3,
        "{seen:?}"
    );
}

/// Sectores ilegibles simulados dentro de un fichero del corpus (`FaultyDevice` sobre la imagen):
/// relleno exactamente en esos bytes y tramos con la LBA del origen en el diario.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn unreadable_sectors_in_corpus_files() {
    for name in ["ntfs_basic", "ntfs_mbr"] {
        let m = manifest(name);
        let start: u64 = m
            .partition
            .as_ref()
            .map_or(0, |p| p["start_lba"].parse().unwrap());
        // Primer fichero vivo no residente de al menos 8 KiB.
        let e = m
            .files
            .iter()
            .find(|e| {
                e.kv["state"] == "live"
                    && e.kv["type"] == "file"
                    && e.kv.get("resident").is_some_and(|r| r == "false")
                    && e.kv["size"].parse::<u64>().unwrap() >= 8192
                    && e.kv.contains_key("runs")
            })
            .unwrap();
        let runs = &e.kv["runs"];
        let first: Vec<u64> = runs
            .trim_matches(['[', ']'])
            .split(',')
            .take(3)
            .map(|x| x.trim().trim_matches(['[', ']']).parse().unwrap())
            .collect();
        let lcn = first[1];
        // Segundo sector del fichero (clústeres de 4 KiB, sectores de 512 B).
        let bad = start + lcn * 8 + 1;
        let image = std::fs::read(image_path(&m)).unwrap();
        let mem = mr_io::MemDevice::new(image, 512).unwrap();
        let dev = mr_io::FaultyDevice::new(mem).with_bad([bad]);
        let img = mr_api::source_from_device(Arc::new(dev), "corpus con un sector ilegible");
        let o = open_with(&m, img);
        let path = e.kv["path"].clone();
        let dir = tempfile::tempdir().unwrap();
        let sel = Selection {
            nodes: vec![o.vfs.find(&path).unwrap()],
            ..Selection::default()
        };
        let s = recover(&o, sel, RecoverOptions::new(dir.path()));
        assert_eq!(s.exit_code(), 3);
        let leaf = path.rsplit('/').next().unwrap();
        let got = std::fs::read(dir.path().join(dest_name(&units(leaf)).name)).unwrap();
        for (i, b) in got.iter().enumerate().take(1024).skip(512) {
            assert_eq!(*b, DEFAULT_FILL[i % 4], "{name}: byte {i}");
        }
        assert_ne!(sha256(&got), e.kv["sha256"]);
        let j = journal(Path::new(&s.journal));
        let x = &files_by_path(&j)[path.as_str()];
        assert_eq!(x.get("estado").s(), "parcial");
        let r = &x.get("flujos").arr()[0].get("tramos_no_fiables").arr()[0];
        assert_eq!(r.get("desplazamiento").n(), 512);
        assert_eq!(r.get("longitud").n(), 512);
        assert_eq!(r.get("lba").n(), lcn * 8 + 1, "{name}");
        assert_eq!(r.get("lba_origen").n(), bad, "{name}");
    }
}

/// Repetir con `omitir_si_identico` no escribe nada; dos recuperaciones iguales dan el mismo
/// árbol y el mismo diario salvo fechas, identificador y rutas absolutas.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn rerun_and_determinism_on_the_corpus() {
    let m = manifest("ntfs_deleted");
    let o = open(&m);
    let sel = select(&[Category::Live, Category::Deleted]);
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let sa = recover(&o, sel.clone(), RecoverOptions::new(a.path()));
    let sb = recover(&o, sel.clone(), RecoverOptions::new(b.path()));
    assert_eq!(tree_files(a.path()), tree_files(b.path()));
    let (ja, jb) = (
        journal(Path::new(&sa.journal)),
        journal(Path::new(&sb.journal)),
    );
    assert_eq!(ja.len(), jb.len());
    for (x, y) in ja.iter().zip(&jb).skip(1).take(ja.len() - 2) {
        assert_eq!(x, y);
    }
    let mut opts = RecoverOptions::new(a.path());
    opts.conflict = mr_api::mr_recover::ConflictPolicy::SkipIfIdentical;
    let s = recover(&o, sel, opts);
    assert_eq!(s.totals.files_written, 0, "{:?}", s.totals);
    assert_eq!(s.totals.skipped_identical, sa.totals.files_written);
    assert_eq!(tree_files(a.path()), tree_files(b.path()));
}
