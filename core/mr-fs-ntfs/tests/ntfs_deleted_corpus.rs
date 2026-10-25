//! Ficheros borrados (#23) contra el corpus NTFS (ADR-024; esquema del manifiesto en
//! `docs/PRUEBAS.md` §1.2). La verdad de los manifiestos la obtuvo The Sleuth Kit, no MemRescue.
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`;
//! `MR_NTFS_CORPUS` para otra ubicación). Se ejecutan con
//! `cargo test -p mr-fs-ntfs --test ntfs_deleted_corpus -- --ignored`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_fs_ntfs::deleted::{NoObserver, ParentState};
use mr_fs_ntfs::{
    recover_stream, scan_deleted, DeletedScan, DeletedStatus, EntryOrigin, NtfsError, NtfsVolume,
    RangeState, RecordState, Recoverability, ScanOptions,
};
use mr_io::{BlockDevice, ImageFile, RangeView};
use mr_part::{open_coordinates, read_partition_table, Scheme};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

/// Lo que estas pruebas leen del manifiesto: claves de nivel superior, `[partition]`, los
/// registros de `[[damage]]` y las entradas de `[[files]]` (sin sus `[[files.ads]]`).
struct Manifest {
    top: HashMap<String, String>,
    partition: Option<HashMap<String, String>>,
    damaged_records: Vec<u64>,
    files: Vec<HashMap<String, String>>,
}

fn load(path: &Path) -> Manifest {
    let text = std::fs::read_to_string(path).unwrap();
    let mut m = Manifest {
        top: HashMap::new(),
        partition: None,
        damaged_records: Vec::new(),
        files: Vec::new(),
    };
    #[derive(PartialEq)]
    enum Cur {
        Top,
        Partition,
        Damage,
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
                "[partition]" => {
                    m.partition = Some(HashMap::new());
                    Cur::Partition
                }
                "[[damage]]" => Cur::Damage,
                "[[files]]" => {
                    m.files.push(HashMap::new());
                    Cur::File
                }
                _ => Cur::Skip,
            };
            continue;
        }
        let (k, v) = line.split_once(" = ").unwrap();
        let (k, v) = (k.trim().to_string(), v.trim().trim_matches('"').to_string());
        match cur {
            Cur::Top => {
                m.top.insert(k, v);
            }
            Cur::Partition => {
                m.partition.as_mut().unwrap().insert(k, v);
            }
            Cur::Damage if k == "records" => {
                m.damaged_records.extend(
                    v.trim_matches(['[', ']'])
                        .split(',')
                        .filter(|x| !x.trim().is_empty())
                        .map(|x| x.trim().parse::<u64>().unwrap()),
                );
            }
            Cur::File => {
                m.files.last_mut().unwrap().insert(k, v);
            }
            _ => {}
        }
    }
    m
}

fn manifests() -> Vec<Manifest> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/ntfs/manifests"))
        .unwrap()
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
        "falta {}: descargue el corpus con `python -I -X utf8 tools/fetch_ntfs_corpus.py`",
        p.display()
    );
    let img: Arc<dyn BlockDevice> = Arc::new(ImageFile::open(&p, 512).unwrap());
    let dev: Arc<dyn BlockDevice> = match &m.partition {
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

/// SHA-256 de lo que escribe la recuperación del flujo principal y su informe.
fn recover_sha(v: &NtfsVolume, record: u64) -> (String, mr_fs_ntfs::RecoveryReport) {
    struct W(MultiHasher);
    impl std::io::Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = W(MultiHasher::new(&[HashAlgorithm::Sha256]));
    let r = recover_stream(v, record, "", &mut w, &NoObserver)
        .unwrap()
        .unwrap();
    let sha = w.0.finish().remove(0).1;
    (sha, r)
}

/// Rutas que nombra el `reason` del manifiesto tras "asignados ahora a ".
fn reason_owners(reason: &str) -> BTreeSet<String> {
    reason
        .split_once("asignados ahora a ")
        .map(|(_, r)| r.split(", ").map(str::to_string).collect())
        .unwrap_or_default()
}

fn scan(v: &NtfsVolume) -> DeletedScan {
    scan_deleted(v, &ScanOptions::default(), &NoObserver)
        .unwrap()
        .unwrap()
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn deleted_entries_match_every_manifest() {
    let mut checked = [0usize; 3];
    let mut recovered = 0usize;
    for m in manifests() {
        let name = m.top["name"].clone();
        let v = open(&m);
        let s = scan(&v);
        assert!(s.bitmap_issue.is_none(), "{name}: {:?}", s.bitmap_issue);
        assert!(!s.claims_truncated && !s.entries_truncated, "{name}");
        let cluster = u64::from(v.info().boot.cluster_size);
        let mut expected_records = BTreeSet::new();
        for f in &m.files {
            let what = format!("{name} {}", f["path"]);
            let record: u64 = f["mft_record"].parse().unwrap();
            let state = f["state"].as_str();
            if state == "live" {
                assert!(
                    s.entry(record).is_none(),
                    "{what}: un fichero vivo no es un borrado"
                );
                continue;
            }
            expected_records.insert(record);
            let e = s
                .entry(record)
                .unwrap_or_else(|| panic!("{what}: no aparece entre los borrados"));
            assert_eq!(e.sequence.to_string(), f["sequence"], "{what}");
            assert_eq!(e.is_directory, f["type"] == "dir", "{what}");
            let leaf = f["path"].rsplit('/').next().unwrap();
            assert_eq!(e.name, leaf, "{what}");
            assert_eq!(e.path.as_deref(), Some(f["path"].as_str()), "{what}");
            assert!(!e.parent_state.is_orphan(), "{what}: {}", e.parent_state);
            if f["type"] == "dir" {
                assert_eq!(e.recoverability(), None, "{what}");
                continue;
            }
            assert_eq!(e.size.to_string(), f["size"], "{what}");
            let want = match f["recoverable"].as_str() {
                "full" => Recoverability::Full,
                "partial" => Recoverability::Partial,
                "none" => Recoverability::None,
                x => panic!("{what}: recoverable {x}"),
            };
            let a = e.main_stream().unwrap();
            assert_eq!(a.recoverability, want, "{what}: {}", s.summary(a));
            checked[want as usize] += 1;
            let (sha, r) = recover_sha(&v, record);
            assert_eq!(r.copy.bytes.to_string(), f["size"], "{what}");
            assert_eq!(r.copy.filled_bytes(), 0, "{what}");
            recovered += 1;
            if want == Recoverability::Full {
                assert_eq!(sha, f["sha256"], "{what}");
                assert!(r.is_clean(), "{what}: {:?}", r.copy.untrusted);
                assert!(r.suspect.is_empty(), "{what}");
                continue;
            }
            assert_eq!(sha, f["recovered_sha256"], "{what}");
            assert_eq!(a.clusters.to_string(), f["clusters"], "{what}");
            assert_eq!(
                a.possibly_overwritten().to_string(),
                f["overwritten_clusters"],
                "{what}"
            );
            assert_eq!(a.owned, a.possibly_overwritten(), "{what}");
            let owners: BTreeSet<String> = a.owners.iter().map(|o| s.owners[o].display()).collect();
            assert_eq!(owners, reason_owners(&f["reason"]), "{what}");
            // Los bytes de los clústeres sospechosos se informan por tramo.
            let size: u64 = f["size"].parse().unwrap();
            let over: u64 = f["overwritten_clusters"].parse().unwrap();
            let suspect_bytes: u64 = r.suspect.iter().map(|x| x.length).sum();
            assert_eq!(suspect_bytes, (over * cluster).min(size), "{what}");
            assert_eq!(
                r.copy.count(RangeState::PossiblyOverwritten),
                suspect_bytes,
                "{what}"
            );
            assert!(!r.is_clean(), "{what}");
            let summary = r.summary.unwrap();
            for o in reason_owners(&f["reason"]) {
                assert!(summary.contains(&o), "{what}: {summary}");
            }
        }
        // Ninguna entrada borrada que el manifiesto no conozca, salvo las que se listan.
        let extra: Vec<_> = s
            .entries
            .iter()
            .filter(|e| !expected_records.contains(&e.record))
            .map(|e| (e.record, e.display_path()))
            .collect();
        assert!(
            extra.is_empty(),
            "{name}: borrados sin manifiesto {extra:?}"
        );
        // Registros dañados: nunca borrados, siempre informados y nunca recuperados.
        for &d in &m.damaged_records {
            assert!(s.entry(d).is_none(), "{name}: {d}");
            assert!(
                s.problems
                    .iter()
                    .any(|p| p.record == d && p.state == RecordState::Damaged),
                "{name}: registro {d} no informado como dañado"
            );
            let mut sink = std::io::sink();
            match recover_stream(&v, d, "", &mut sink, &NoObserver) {
                Err(mr_fs_ntfs::stream::CopyError::Ntfs(NtfsError::RecordUnusable {
                    state: RecordState::Damaged,
                    ..
                })) => {}
                other => panic!("{name}: registro {d}: {other:?}"),
            }
        }
        // Padres: los borrados de ntfs_deleted dentro de /docs/cartas tienen el padre borrado.
        if name == "ntfs_deleted" {
            let e = s.entry(99).unwrap();
            assert_eq!(e.parent_state, ParentState::Deleted);
            assert_eq!(e.parent.record, 68);
        }
    }
    // full, partial, none
    assert_eq!(checked, [12, 1, 2], "flujos principales comprobados");
    assert_eq!(recovered, 15);
}

fn find(m: &[Manifest], name: &str) -> usize {
    m.iter().position(|x| x.top["name"] == name).unwrap()
}

fn faulty(m: &Manifest, bad: Vec<u64>) -> NtfsVolume {
    let dev =
        mr_io::FaultyDevice::new(ImageFile::open(generated().join(&m.top["file"]), 512).unwrap())
            .with_bad(bad);
    NtfsVolume::open(Arc::new(dev)).unwrap()
}

/// Sector ilegible en `$Bitmap`: los clústeres cuyo bit no se puede leer nunca se dan por libres
/// (el fichero deja de ser `full`), aunque su contenido siga intacto; los residentes no cambian.
/// Sector ilegible en un clúster libre de un sobrescrito parcial: tramo `unreadable` con su LBA y
/// patrón, además de los `possibly_overwritten`.
#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn unreadable_bitmap_and_data_sectors_are_never_reported_as_recoverable() {
    let all = manifests();
    let m = &all[find(&all, "ntfs_overwritten")];
    let healthy = open(m);
    let cluster = u64::from(healthy.info().boot.cluster_size);
    let bm = healthy.open_stream(6, "").unwrap();
    let bm_lcn = bm.layout().runs[0].lcn.unwrap();
    // superviviente_2.bin: registro 131, clústeres 5406..5416.
    let byte = 5406 / 8;
    assert!(
        byte / cluster == 0,
        "el byte está en el primer clúster de $Bitmap"
    );
    let bad_lba = (bm_lcn * cluster + byte) / 512;
    let v = faulty(m, vec![bad_lba]);
    let s = scan(&v);
    let e = s.entry(131).unwrap();
    let a = e.main_stream().unwrap();
    assert_eq!(a.recoverability, Recoverability::None, "{}", s.summary(a));
    assert_eq!((a.unknown, a.clusters), (10, 10));
    let (sha, r) = recover_sha(&v, 131);
    let f = m.files.iter().find(|f| f["mft_record"] == "131").unwrap();
    assert_eq!(sha, f["sha256"], "el contenido se copia tal cual");
    assert_eq!(r.copy.count(RangeState::PossiblyOverwritten), 40000);
    assert_eq!(r.copy.filled_bytes(), 0);
    // El residente borrado sigue íntegro.
    let e = s.entry(130).unwrap();
    assert_eq!(e.recoverability(), Some(Recoverability::Full));

    // objetivo_3.bin: registro 129, 16 clústeres desde el 5389, 6 libres.
    let h = scan(&healthy);
    let a = h.entry(129).unwrap().main_stream().unwrap().clone();
    let free = a
        .ranges
        .iter()
        .find(|r| r.state == mr_fs_ntfs::deleted::ClusterState::Free)
        .unwrap();
    let lba = free.lcn * cluster / 512 + 3;
    let v = faulty(m, vec![lba]);
    let (_, r) = recover_sha(&v, 129);
    let un: Vec<_> = r
        .copy
        .untrusted
        .iter()
        .filter(|x| x.state == RangeState::Unreadable)
        .collect();
    assert_eq!(un.len(), 1);
    assert_eq!(un[0].offset, free.vcn * cluster + 3 * 512);
    assert_eq!((un[0].length, un[0].lbas), (512, Some((lba, 1))));
    assert_eq!(r.copy.filled_bytes(), 512);
    assert_eq!(
        r.copy.count(RangeState::PossiblyOverwritten),
        a.possibly_overwritten() * cluster
    );
}

/// Entradas residuales de los índices (#23) tal como las deja ntfs-3g en el corpus: al quitar una
/// entrada mueve las siguientes y escribe la entrada final encima, así que el espacio libre de los
/// nodos solo conserva restos con la cabecera pisada (referencia imposible: rechazados, nunca
/// aceptados); el índice de un directorio borrado (`/docs/cartas`, registro 68) conserva su última
/// entrada (`sello.bin`, registro 103), que se une a la entrada de la MFT. El corpus no tiene nodos
/// `INDX` libres. Cada borrado del manifiesto aparece exactamente una vez y ningún vivo aparece.
#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn residual_index_entries_match_what_the_corpus_shows() {
    for m in manifests() {
        let name = m.top["name"].clone();
        let v = open(&m);
        let s = scan(&v);
        let r = s.residual.clone().expect("búsqueda de residuales hecha");
        assert_eq!(r.nodes_unreadable + r.nodes_invalid, 0, "{name}");
        assert_eq!(r.directories_unusable, 0, "{name}");
        assert_eq!(r.free_nodes, 0, "{name}: el corpus no tiene nodos libres");
        assert!(r.slack_bytes > 0 && !r.truncated, "{name}");
        assert_eq!(s.residual_live, 0, "{name}");
        assert_eq!(s.residual_only().count(), 0, "{name}");
        let (accepted, rejected) = if name == "ntfs_deleted" {
            (1, 4)
        } else {
            (0, 0)
        };
        assert_eq!((r.accepted, r.rejected), (accepted, rejected), "{name}");
        for f in &m.files {
            let record: u64 = f["mft_record"].parse().unwrap();
            let n = s.entries.iter().filter(|e| e.record == record).count();
            let want = usize::from(f["state"] != "live");
            assert_eq!(n, want, "{name} {}: veces entre los borrados", f["path"]);
        }
        if name == "ntfs_deleted" {
            let e = s.entry(103).unwrap();
            assert_eq!(
                e.origins,
                [EntryOrigin::MftRecord, EntryOrigin::DeletedDirectoryIndex]
            );
            assert_eq!(e.index_refs.len(), 1);
            assert_eq!(e.index_refs[0].directory.record, 68);
            assert_eq!(e.index_refs[0].name, "sello.bin");
            assert_eq!(e.names.len(), 1, "el índice confirma el nombre y el padre");
            assert_eq!(e.status, DeletedStatus::Deleted);
            assert_eq!(e.recoverability(), Some(Recoverability::Full));
            let only_mft = s
                .entries
                .iter()
                .filter(|e| e.origins == [EntryOrigin::MftRecord])
                .count();
            assert_eq!(only_mft, s.entries.len() - 1, "{name}");
        } else {
            assert!(
                s.entries
                    .iter()
                    .all(|e| e.origins == [EntryOrigin::MftRecord]),
                "{name}"
            );
        }
    }
}

/// Un sector ilegible en el nodo INDX del directorio borrado /docs/cartas (clúster 4773 de
/// ntfs_deleted): el nodo no se interpreta y sello.bin queda solo con su origen en la MFT.
#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn unreadable_index_node_is_never_parsed_for_residual_entries() {
    let all = manifests();
    let m = &all[find(&all, "ntfs_deleted")];
    let v = faulty(m, vec![4773 * 8 + 1]);
    let s = scan(&v);
    let r = s.residual.clone().unwrap();
    assert_eq!(r.nodes_unreadable, 1);
    assert_eq!(r.accepted, 0);
    // De los 4 restos rechazados, 3 estaban en ese nodo; queda el de /docs (clúster 4611).
    assert_eq!(r.rejected, 1);
    let e = s.entry(103).unwrap();
    assert_eq!(e.origins, [EntryOrigin::MftRecord]);
    assert_eq!(e.recoverability(), Some(Recoverability::Full));
}
