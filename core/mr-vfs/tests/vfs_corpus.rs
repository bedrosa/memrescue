//! Árbol virtual (#25) contra el corpus NTFS (ADR-024, esquema del manifiesto en
//! `docs/PRUEBAS.md` §1.2). La verdad de los manifiestos la obtuvo The Sleuth Kit, no MemRescue.
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`, en
//! `tests/corpus/ntfs/generated`; `MR_NTFS_CORPUS` para otra ubicación). Se marcan `#[ignore]` y se
//! ejecutan con `cargo test -p mr-vfs --test vfs_corpus -- --ignored`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_fs_ntfs::NtfsVolume;
use mr_io::{BlockDevice, ImageFile, RangeView};
use mr_part::{open_coordinates, read_partition_table, Scheme};
use mr_vfs::filter::{query, Filter};
use mr_vfs::ntfs::{LOOSE_DELETED_FOLDER, METADATA_FOLDER, ORPHANS_FOLDER};
use mr_vfs::{
    build_ntfs, flags, Category, DataState, NameReliability, NoBuildObserver, NodeId, NodeKind,
    NtfsBuildOptions, Vfs,
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

/// Manifiesto: claves de nivel superior, `[partition]` y las entradas `[[files]]`.
struct Manifest {
    top: HashMap<String, String>,
    partition: Option<HashMap<String, String>>,
    files: Vec<HashMap<String, String>>,
}

fn load(path: &Path) -> Manifest {
    let text = std::fs::read_to_string(path).expect("manifiesto legible");
    let mut m = Manifest {
        top: HashMap::new(),
        partition: None,
        files: Vec::new(),
    };
    #[derive(PartialEq)]
    enum Cur {
        Top,
        Partition,
        File,
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
                    m.files.push(HashMap::new());
                    Cur::File
                }
                "[partition]" => {
                    m.partition = Some(HashMap::new());
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
                m.files.last_mut().unwrap().insert(k, v);
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

fn open(m: &Manifest) -> NtfsVolume {
    let p = generated().join(&m.top["file"]);
    assert!(
        p.exists(),
        "falta {}: descargue el corpus con python -I -X utf8 tools/fetch_ntfs_corpus.py",
        p.display()
    );
    let img: Arc<dyn BlockDevice> = Arc::new(ImageFile::open(&p, 512).unwrap());
    let dev = match &m.partition {
        None => img,
        Some(part) => {
            let t = read_partition_table(img.as_ref()).unwrap();
            let want = if part["scheme"] == "gpt" {
                Scheme::Gpt
            } else {
                Scheme::Mbr
            };
            assert_eq!(t.scheme, want);
            let index: usize = part["index"].parse().unwrap();
            let c =
                open_coordinates(&t, index, img.sector_size(), img.sector_count(), false).unwrap();
            Arc::new(RangeView::new(img, c.image_start, c.count).unwrap())
        }
    };
    NtfsVolume::open(dev).unwrap()
}

fn build(m: &Manifest) -> Vfs {
    build_ntfs(&open(m), &NtfsBuildOptions::default(), &NoBuildObserver)
        .unwrap_or_else(|e| panic!("{}: {e}", m.top["name"]))
        .expect("no cancelada")
}

/// Rutas de todos los nodos con su número de apariciones.
fn path_counts(v: &Vfs) -> HashMap<String, (usize, NodeId)> {
    let mut out: HashMap<String, (usize, NodeId)> = HashMap::new();
    for id in v.preorder(NodeId::ROOT) {
        let e = out.entry(v.path(id)).or_insert((0, id));
        e.0 += 1;
    }
    out
}

/// Cada entrada del manifiesto aparece una sola vez en su ruta con la categoría y el estado de
/// datos que corresponden; nada borrado se presenta como íntegro.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn every_manifest_entry_is_in_place_with_its_state() {
    let mut checked = [0usize; 3];
    for m in manifests() {
        let name = m.top["name"].clone();
        let v = build(&m);
        assert!(!v.is_partial(), "{name}");
        let paths = path_counts(&v);
        for f in &m.files {
            let path = &f["path"];
            let (count, id) = paths
                .get(path)
                .copied()
                .unwrap_or_else(|| panic!("{name}: falta {path}"));
            assert_eq!(count, 1, "{name}: {path} aparece {count} veces");
            let n = v.node(id).unwrap();
            let dir = f["type"] == "dir";
            assert_eq!(
                n.kind,
                if dir {
                    NodeKind::Directory
                } else {
                    NodeKind::File
                },
                "{name}: {path}"
            );
            let record: u64 = f["mft_record"].parse().unwrap();
            assert_eq!(n.origin.unwrap().record, record, "{name}: {path}");
            // Nombre UTF-16 exacto.
            let last = path.rsplit('/').next().unwrap();
            assert_eq!(v.name_units(id), last.encode_utf16().collect::<Vec<_>>());
            let damaged = f.get("mft_damaged").is_some_and(|d| d == "true");
            match f["state"].as_str() {
                "live" => {
                    checked[0] += 1;
                    assert_eq!(n.category, Category::Live, "{name}: {path}");
                    assert_eq!(n.name_reliability, NameReliability::Structure);
                    assert!(!n.has(flags::DELETED), "{name}: {path}");
                    if damaged {
                        assert!(n.has(flags::RECORD_DAMAGED), "{name}: {path}");
                        if !dir {
                            assert_eq!(n.data, Some(DataState::NoDisponible), "{name}: {path}");
                        }
                    } else if !dir {
                        assert_eq!(n.data, Some(DataState::Integro), "{name}: {path}");
                        assert_eq!(n.size.to_string(), f["size"], "{name}: {path}");
                    }
                }
                "deleted" | "overwritten" => {
                    checked[1] += 1;
                    assert_eq!(n.category, Category::Deleted, "{name}: {path}");
                    assert!(n.has(flags::DELETED | flags::FROM_MFT), "{name}: {path}");
                    assert_eq!(n.name_reliability, NameReliability::Record);
                    if !dir {
                        checked[2] += 1;
                        let want: &[DataState] = match f["recoverable"].as_str() {
                            "full" => &[DataState::IntegroNoVerificable],
                            "partial" => &[DataState::Parcial],
                            "none" => &[DataState::Sobrescrito, DataState::SinDatos],
                            other => panic!("{other}"),
                        };
                        assert!(
                            n.data.is_some_and(|d| want.contains(&d)),
                            "{name}: {path}: {:?}, esperado {want:?}",
                            n.data
                        );
                        if f["state"] == "overwritten" && f["recoverable"] == "none" {
                            assert_eq!(n.data, Some(DataState::Sobrescrito), "{name}: {path}");
                        }
                        assert_eq!(n.size.to_string(), f["size"], "{name}: {path}");
                    }
                }
                other => panic!("{name}: estado {other}"),
            }
        }
        // Invariantes del árbol completo.
        for id in v.preorder(NodeId::ROOT) {
            let n = v.node(id).unwrap();
            if n.has(flags::DELETED) {
                assert_ne!(n.data, Some(DataState::Integro), "{name}: {}", v.path(id));
            }
            if n.kind == NodeKind::File {
                assert!(n.data.is_some(), "{name}: {}", v.path(id));
                assert!(n.origin.is_some());
            }
        }
        // Metadatos: los ficheros del sistema en [Metadatos], ninguno en la raíz.
        let meta = v
            .find(&format!("/{METADATA_FOLDER}"))
            .unwrap_or_else(|| panic!("{name}: sin {METADATA_FOLDER}"));
        let mft = v
            .lookup(meta, "$MFT")
            .unwrap_or_else(|| panic!("{name}: sin $MFT"));
        assert_eq!(v.category(mft), Some(Category::Metadata));
        assert_eq!(v.data(mft), Some(DataState::Integro), "{name}");
        for k in v.children(NodeId::ROOT) {
            assert!(!v.name(*k).starts_with('$'), "{name}: {}", v.path(*k));
        }
        // Los recuentos del manifiesto coinciden con las categorías.
        let deleted = m.files.iter().filter(|f| f["state"] != "live").count() as u64;
        assert_eq!(v.stats().category(Category::Deleted), deleted, "{name}");
        assert_eq!(v.stats().deleted, deleted, "{name}");
        assert!(v.find(&format!("/{ORPHANS_FOLDER}")).is_none(), "{name}");
        assert!(
            v.find(&format!("/{LOOSE_DELETED_FOLDER}")).is_none(),
            "{name}"
        );
    }
    assert!(
        checked[0] >= 500 && checked[1] >= 16 && checked[2] >= 15,
        "{checked:?}"
    );
}

/// Filtros y consultas sobre `ntfs_deleted`: borrados, exclusión por máscara y determinismo.
#[test]
#[ignore = "requiere el corpus NTFS descargado"]
fn deleted_view_filters_and_determinism() {
    let m = manifests()
        .into_iter()
        .find(|m| m.top["name"] == "ntfs_deleted")
        .unwrap();
    let v = build(&m);
    let f = Filter {
        deleted_only: true,
        files_only: true,
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    let files: Vec<String> = r
        .iter()
        .filter(|i| v.kind(**i) == Some(NodeKind::File))
        .map(|i| v.path(*i))
        .collect();
    let want: Vec<String> = m
        .files
        .iter()
        .filter(|f| f["state"] == "deleted" && f["type"] == "file")
        .map(|f| f["path"].clone())
        .collect();
    let mut a = files.clone();
    let mut b = want.clone();
    a.sort();
    b.sort();
    assert_eq!(a, b);
    let f = Filter {
        categories: Category::Live.bit(),
        names: mr_vfs::NameRules::parse("!*.bin").unwrap(),
        files_only: true,
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert!(r
        .iter()
        .all(|i| v.kind(*i) != Some(NodeKind::File) || !v.name(*i).ends_with(".bin")));
    let live_files = m
        .files
        .iter()
        .filter(|f| f["state"] == "live" && f["type"] == "file" && !f["path"].ends_with(".bin"))
        .count();
    let got = r
        .iter()
        .filter(|i| v.kind(**i) == Some(NodeKind::File) && v.category(**i) == Some(Category::Live))
        .count();
    assert_eq!(got, live_files);
    let v2 = build(&m);
    let p1: Vec<String> = v
        .preorder(NodeId::ROOT)
        .iter()
        .map(|i| v.path(*i))
        .collect();
    let p2: Vec<String> = v2
        .preorder(NodeId::ROOT)
        .iter()
        .map(|i| v2.path(*i))
        .collect();
    assert_eq!(p1, p2);
}
