//! Lector NTFS (#20, #21, #22) contra el corpus NTFS (ADR-024, esquema del manifiesto en `docs/PRUEBAS.md`
//! §1.2). La verdad de los manifiestos la obtuvo The Sleuth Kit, no MemRescue.
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`, en
//! `tests/corpus/ntfs/generated`; `MR_NTFS_CORPUS` para otra ubicación). Se marcan `#[ignore]` y se
//! ejecutan con `cargo test -p mr-fs-ntfs --test ntfs_corpus -- --ignored`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_common::{HashAlgorithm, MultiHasher};
use mr_fs_ntfs::dir::NoTreeObserver;
use mr_fs_ntfs::runs::{merge_contiguous, sparse_ranges, Run};
use mr_fs_ntfs::upcase::UpcaseSource;
use mr_fs_ntfs::{
    BackupState, BootSource, Confidence, DataStream, EntryState, NodeId, NtfsError, NtfsVolume,
    OrphanReason, RangeState, RecordOutcome, RecordSource, RecordState, TreeOptions, TreeStatus,
};
use mr_io::{BlockDevice, FaultyDevice, ImageFile, RangeView};
use mr_part::{open_coordinates, read_partition_table, Scheme};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_NTFS_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/ntfs/generated"))
}

/// Manifiesto NTFS: claves de nivel superior, tablas simples (`[volume]`, `[partition]`) y tablas
/// repetidas (`[[damage]]`, `[[files]]` y sus `[[files.ads]]`).
struct Manifest {
    top: HashMap<String, String>,
    tables: HashMap<String, HashMap<String, String>>,
    damage: Vec<HashMap<String, String>>,
    files: Vec<HashMap<String, String>>,
    /// Flujos alternativos de cada entrada de `files` (mismo índice).
    ads: Vec<Vec<HashMap<String, String>>>,
}

impl Manifest {
    fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("manifiesto legible");
        let mut m = Manifest {
            top: HashMap::new(),
            tables: HashMap::new(),
            damage: Vec::new(),
            files: Vec::new(),
            ads: Vec::new(),
        };
        enum Cur {
            Top,
            Table(String),
            Damage,
            File,
            Ads,
            Skip,
        }
        let mut cur = Cur::Top;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(n) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
                cur = match n {
                    "damage" => {
                        m.damage.push(HashMap::new());
                        Cur::Damage
                    }
                    "files" => {
                        m.files.push(HashMap::new());
                        m.ads.push(Vec::new());
                        Cur::File
                    }
                    "files.ads" => {
                        m.ads.last_mut().unwrap().push(HashMap::new());
                        Cur::Ads
                    }
                    _ => Cur::Skip,
                };
                continue;
            }
            if let Some(n) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                m.tables.insert(n.to_string(), HashMap::new());
                cur = Cur::Table(n.to_string());
                continue;
            }
            let (k, v) = line.split_once(" = ").expect("clave = valor");
            let (k, v) = (k.trim().to_string(), v.trim().trim_matches('"').to_string());
            match &cur {
                Cur::Top => {
                    m.top.insert(k, v);
                }
                Cur::Table(t) => {
                    m.tables.get_mut(t).unwrap().insert(k, v);
                }
                Cur::Damage => {
                    m.damage.last_mut().unwrap().insert(k, v);
                }
                Cur::File => {
                    m.files.last_mut().unwrap().insert(k, v);
                }
                Cur::Ads => {
                    m.ads.last_mut().unwrap().last_mut().unwrap().insert(k, v);
                }
                Cur::Skip => {}
            }
        }
        m
    }

    fn name(&self) -> &str {
        &self.top["name"]
    }

    fn vol(&self, k: &str) -> &str {
        &self.tables["volume"][k]
    }

    fn vol_num(&self, k: &str) -> u64 {
        self.vol(k).parse().unwrap()
    }
}

/// `[[a, b, c], ...]` o `[a, b]` como listas de enteros.
fn int_lists(v: &str) -> Vec<Vec<u64>> {
    let inner = v
        .trim()
        .strip_prefix('[')
        .unwrap()
        .strip_suffix(']')
        .unwrap()
        .trim();
    if inner.is_empty() {
        return Vec::new();
    }
    if !inner.starts_with('[') {
        return vec![inner
            .split(',')
            .map(|x| x.trim().parse().unwrap())
            .collect()];
    }
    inner
        .split(']')
        .map(|p| p.trim_start_matches([',', ' ', '[']))
        .filter(|p| !p.is_empty())
        .map(|p| p.split(',').map(|x| x.trim().parse().unwrap()).collect())
        .collect()
}

fn manifests() -> Vec<Manifest> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/ntfs/manifests"))
        .expect("directorio de manifiestos NTFS")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    v.sort();
    assert_eq!(v.len(), 10, "manifiestos NTFS versionados");
    v.iter().map(|p| Manifest::load(p)).collect()
}

fn image(m: &Manifest) -> Arc<dyn BlockDevice> {
    let p = generated().join(&m.top["file"]);
    assert!(
        p.exists(),
        "falta {}: descargue el corpus con `python -I -X utf8 tools/fetch_ntfs_corpus.py`",
        p.display()
    );
    let f = ImageFile::open(&p, 512).unwrap();
    assert_eq!(f.size().to_string(), m.top["image_size"]);
    Arc::new(f)
}

/// Origen del volumen: la imagen completa o la partición del manifiesto leída con `mr-part`.
fn volume_source(m: &Manifest, img: Arc<dyn BlockDevice>) -> Arc<dyn BlockDevice> {
    let Some(p) = m.tables.get("partition") else {
        assert_eq!(m.vol("offset"), "0", "{}", m.name());
        return img;
    };
    let t = read_partition_table(img.as_ref()).unwrap();
    let want = match p["scheme"].as_str() {
        "mbr" => Scheme::Mbr,
        "gpt" => Scheme::Gpt,
        other => panic!("esquema {other}"),
    };
    assert_eq!(t.scheme, want, "{}", m.name());
    let index: usize = p["index"].parse().unwrap();
    let part = &t.partitions[index];
    assert_eq!(part.start_lba.to_string(), p["start_lba"], "{}", m.name());
    assert_eq!(part.sector_count.to_string(), p["sectors"], "{}", m.name());
    let c = open_coordinates(&t, index, img.sector_size(), img.sector_count(), false).unwrap();
    assert_eq!(c.image_start * 512, m.vol_num("offset"), "{}", m.name());
    Arc::new(RangeView::new(img, c.image_start, c.count).unwrap())
}

fn open(m: &Manifest) -> NtfsVolume {
    NtfsVolume::open(volume_source(m, image(m))).unwrap_or_else(|e| panic!("{}: {e}", m.name()))
}

/// Estados de todos los registros.
fn all_states(v: &NtfsVolume) -> BTreeMap<u64, RecordState> {
    v.records()
        .map(|r| {
            let (n, o) = r.unwrap();
            (n, o.state())
        })
        .collect()
}

/// Runs de `$MFT` como los da TSK: contiguos fusionados y recortados al tamaño de los datos.
fn tsk_like_runs(v: &NtfsVolume) -> Vec<Vec<u64>> {
    let i = v.info();
    let cluster = u64::from(i.boot.cluster_size);
    let mut left = i.mft.data_size.div_ceil(cluster);
    let mut out = Vec::new();
    for Run { vcn, lcn, length } in merge_contiguous(&i.mft.runs) {
        if left == 0 {
            break;
        }
        let n = length.min(left);
        out.push(vec![vcn, lcn.unwrap(), n]);
        left -= n;
    }
    out
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn volume_fields_match_every_manifest() {
    let mut partitioned = 0;
    for m in manifests() {
        let name = m.name().to_string();
        let v = open(&m);
        let i = v.info();
        let b = &i.boot;
        assert_eq!(format!("{:016x}", b.serial), m.vol("serial"), "{name}");
        assert_eq!(i.label.as_deref(), Some(m.vol("label")), "{name}");
        // TSK nombra las versiones por el sistema que las introdujo: 3.1 = «Windows XP».
        assert_eq!(m.vol("version"), "Windows XP", "{name}");
        assert_eq!(i.version, Some((3, 1)), "{name}");
        assert_eq!(b.mft_lcn, m.vol_num("mft_lcn"), "{name}");
        assert_eq!(b.mftmirr_lcn, m.vol_num("mftmirr_lcn"), "{name}");
        assert_eq!(u64::from(b.mft_record_size), m.vol_num("mft_record_size"));
        assert_eq!(
            u64::from(b.index_record_size),
            m.vol_num("index_record_size")
        );
        assert_eq!(u64::from(b.bytes_per_sector), m.vol_num("sector_size"));
        assert_eq!(u64::from(b.cluster_size), m.vol_num("cluster_size"));
        assert_eq!(b.total_sectors, m.vol_num("total_sectors"), "{name}");
        // TSK añade una entrada virtual ($OrphanFiles) tras el último registro de la MFT.
        assert_eq!(v.record_count() + 1, m.vol_num("mft_entries"), "{name}");
        assert_eq!(tsk_like_runs(&v), int_lists(m.vol("mft_runs")), "{name}");
        assert_eq!(i.mft_record0_source, RecordSource::Mft, "{name}");
        let offset = m.vol_num("offset");
        assert_eq!(
            offset + i.backup_boot.offset,
            m.vol_num("backup_boot_offset"),
            "{name}"
        );
        assert_eq!(i.mirror.lcn, b.mftmirr_lcn);
        assert_eq!(i.mirror.records, 4, "{name}");
        if m.tables.contains_key("partition") {
            partitioned += 1;
        }

        let states = all_states(&v);
        let damaged: BTreeSet<u64> = states
            .iter()
            .filter(|(_, s)| **s == RecordState::Damaged)
            .map(|(n, _)| *n)
            .collect();
        assert!(
            !states.values().any(|s| matches!(
                s,
                RecordState::Unreadable | RecordState::Unmapped | RecordState::OutsideSource
            )),
            "{name}"
        );
        let kinds: Vec<&str> = m.damage.iter().map(|d| d["kind"].as_str()).collect();
        let codes: Vec<&str> = i.anomalies.iter().map(|a| a.code()).collect();
        match kinds.as_slice() {
            [] => {
                assert_eq!(i.boot_source, BootSource::Primary, "{name}");
                assert_eq!(i.backup_boot.state, BackupState::Matches, "{name}");
                assert!(i.anomalies.is_empty(), "{name}: {:?}", i.anomalies);
                assert_eq!(i.confidence(), Confidence::Valid, "{name}");
                assert!(damaged.is_empty(), "{name}: {damaged:?}");
            }
            ["boot"] => {
                let d = &m.damage[0];
                assert_eq!(int_lists(&d["regions"]), vec![vec![0, 512]]);
                assert_eq!(
                    i.boot_source,
                    BootSource::Backup {
                        offset: m.vol_num("backup_boot_offset") - offset
                    },
                    "{name}"
                );
                assert_eq!(i.backup_boot.state, BackupState::InUse);
                assert_eq!(codes, vec!["boot_primary_invalid"], "{name}");
                assert!(matches!(i.confidence(), Confidence::ValidWithFixes(_)));
                assert!(damaged.is_empty(), "{name}: {damaged:?}");
            }
            ["mft"] => {
                let d = &m.damage[0];
                let want: BTreeSet<u64> = int_lists(&d["records"]).concat().into_iter().collect();
                let tsk: BTreeSet<u64> = int_lists(&d["tsk_unreadable_records"])
                    .concat()
                    .into_iter()
                    .collect();
                assert_eq!(
                    damaged, want,
                    "{name}: exactamente los registros del manifiesto"
                );
                assert_eq!(damaged, tsk, "{name}");
                // Cada región dañada es exactamente uno de esos registros.
                let rs = u64::from(b.mft_record_size);
                let cl = u64::from(b.cluster_size);
                let regions: BTreeSet<Vec<u64>> = int_lists(&d["regions"]).into_iter().collect();
                let from_records: BTreeSet<Vec<u64>> = want
                    .iter()
                    .map(|n| vec![b.mft_lcn * cl + n * rs, rs])
                    .collect();
                assert_eq!(regions, from_records, "{name}");
                for n in &want {
                    let o = v.read_record(*n).unwrap();
                    assert!(
                        matches!(o, RecordOutcome::Damaged(_)),
                        "{name}: registro {n}: {o:?}"
                    );
                }
                assert!(i.anomalies.is_empty(), "{name}: {:?}", i.anomalies);
            }
            ["mftmirr"] => {
                let d = &m.damage[0];
                let cl = u64::from(b.cluster_size);
                assert_eq!(
                    int_lists(&d["regions"]),
                    vec![vec![b.mftmirr_lcn * cl, 4 * u64::from(b.mft_record_size)]]
                );
                let mm: Vec<(u64, &str)> = i
                    .mirror
                    .mismatches
                    .iter()
                    .map(|x| (x.record, x.kind()))
                    .collect();
                assert_eq!(
                    mm,
                    (0..4).map(|r| (r, "mirror_invalid")).collect::<Vec<_>>(),
                    "{name}"
                );
                assert_eq!(codes, vec!["mftmirr_mismatch"; 4], "{name}");
                assert!(matches!(i.confidence(), Confidence::ValidWithFixes(_)));
                assert!(damaged.is_empty(), "{name}: {damaged:?}");
            }
            other => panic!("{name}: daños inesperados {other:?}"),
        }
    }
    assert_eq!(partitioned, 2, "ntfs_mbr y ntfs_gpt");
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn file_records_match_state_and_sequence() {
    let mut checked = 0;
    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    for m in manifests() {
        let name = m.name().to_string();
        let v = open(&m);
        let mut live = 0;
        let mut deleted = 0;
        let mut overwritten = 0;
        for f in &m.files {
            let n: u64 = f["mft_record"].parse().unwrap();
            let state = f["state"].as_str();
            match state {
                "live" => live += 1,
                "deleted" => deleted += 1,
                "overwritten" => overwritten += 1,
                other => panic!("{name}: estado {other}"),
            }
            *by_state.entry(state.to_string()).or_default() += 1;
            let o = v.read_record(n).unwrap();
            if f.get("mft_damaged").map(String::as_str) == Some("true") {
                assert!(
                    matches!(o, RecordOutcome::Damaged(_)),
                    "{name} {}: {o:?}",
                    f["path"]
                );
                continue;
            }
            let r = o
                .record()
                .unwrap_or_else(|| panic!("{name} {} (registro {n}): {o:?}", f["path"]));
            assert_eq!(
                r.header().sequence.to_string(),
                f["sequence"],
                "{name} {}",
                f["path"]
            );
            assert_eq!(r.in_use(), state == "live", "{name} {}", f["path"]);
            assert_eq!(r.is_directory(), f["type"] == "dir", "{name} {}", f["path"]);
            assert!(r.header().is_base(), "{name} {}", f["path"]);
            // Los atributos se recorren hasta la marca de fin sin errores.
            assert!(r.attributes().all(|a| a.is_ok()), "{name} {}", f["path"]);
            checked += 1;
        }
        assert_eq!(live.to_string(), m.top["entries_live"], "{name}");
        // `entries_deleted` cuenta todas las entradas no en uso (también las sobrescritas).
        assert_eq!(
            (deleted + overwritten).to_string(),
            m.top["entries_deleted"],
            "{name}"
        );
        assert_eq!(
            overwritten.to_string(),
            m.top["entries_overwritten"],
            "{name}"
        );
    }
    // 523 entradas de los 10 manifiestos; las 4 con `mft_damaged` no tienen registro legible.
    let want: BTreeMap<String, usize> = [("deleted", 13), ("live", 507), ("overwritten", 3)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(by_state, want);
    assert_eq!(checked, 523 - 4);
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn unreadable_mft_sectors_on_the_corpus() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "ntfs_basic")
        .unwrap();
    let healthy = all_states(&open(&m));
    // Un sector de dos registros de ficheros y otro de un registro libre: solo ellos cambian.
    let base = m.vol_num("mft_lcn") * m.vol_num("cluster_size") / 512;
    let per = m.vol_num("mft_record_size") / 512;
    let targets = [74u64, 100, 125];
    let bad: Vec<u64> = targets.iter().map(|n| base + n * per + per - 1).collect();
    let dev = FaultyDevice::new(ImageFile::open(generated().join(&m.top["file"]), 512).unwrap())
        .with_bad(bad.clone());
    let v = NtfsVolume::open(Arc::new(dev)).unwrap();
    let states = all_states(&v);
    for (n, s) in &states {
        if targets.contains(n) {
            assert_eq!(*s, RecordState::Unreadable, "registro {n}");
        } else {
            assert_eq!(*s, healthy[n], "registro {n}");
        }
    }
    for (n, lba) in targets.iter().zip(&bad) {
        assert_eq!(
            v.read_record(*n).unwrap(),
            RecordOutcome::Unreadable { lbas: vec![*lba] }
        );
    }
    // $MFTMirr ilegible: se informa sin cambiar la lectura de la MFT.
    let mirr = m.vol_num("mftmirr_lcn") * m.vol_num("cluster_size") / 512;
    let dev = FaultyDevice::new(ImageFile::open(generated().join(&m.top["file"]), 512).unwrap())
        .with_bad([mirr]);
    let v = NtfsVolume::open(Arc::new(dev)).unwrap();
    let mm = &v.info().mirror.mismatches;
    assert_eq!(mm.len(), 1);
    assert_eq!((mm[0].record, mm[0].mirror), (0, RecordState::Unreadable));
    assert_eq!(all_states(&v), healthy);
}

/// Escritor que solo calcula el SHA-256.
struct Sha(MultiHasher);

impl std::io::Write for Sha {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// SHA-256 del flujo y su informe; el flujo debe ser íntegramente fiable.
fn stream_sha(s: &DataStream<'_>) -> (String, mr_fs_ntfs::CopyReport) {
    let mut h = Sha(MultiHasher::new(&[HashAlgorithm::Sha256]));
    let r = s.copy_to(&mut h).unwrap();
    (h.0.finish().remove(0).1, r)
}

/// Comprueba un flujo contra su entrada del manifiesto (fichero o flujo alternativo): tamaño,
/// residencia, runs, dispersos, tamaño inicializado y SHA-256 del contenido.
fn check_stream(what: &str, s: &DataStream<'_>, e: &HashMap<String, String>, want_sha: &str) {
    let l = s.layout();
    let size: u64 = e["size"].parse().unwrap();
    assert_eq!(s.size(), size, "{what}");
    assert_eq!(l.is_resident().to_string(), e["resident"], "{what}");
    assert!(l.issues.is_empty(), "{what}: {:?}", l.issues);
    if !l.is_resident() {
        let runs: Vec<Vec<u64>> = merge_contiguous(&l.runs)
            .iter()
            .filter_map(|r| r.lcn.map(|lcn| vec![r.vcn, lcn, r.length]))
            .collect();
        assert_eq!(runs, int_lists(&e["runs"]), "{what}: runs");
        let sparse: Vec<Vec<u64>> = sparse_ranges(&l.runs)
            .iter()
            .map(|&(v, n)| vec![v, n])
            .collect();
        let want_sparse = e.get("sparse").map(|x| int_lists(x)).unwrap_or_default();
        assert_eq!(sparse, want_sparse, "{what}: dispersos");
        let init: u64 = e.get("init_size").map_or(size, |x| x.parse().unwrap());
        assert_eq!(l.initialized_size, init, "{what}: init_size");
    }
    let (sha, r) = stream_sha(s);
    assert_eq!(r.bytes, size, "{what}");
    assert_eq!(r.untrusted_bytes(), 0, "{what}: {:?}", r.untrusted);
    if !l.is_resident() {
        // Lo que queda más allá del tamaño inicializado son ceros del formato, nunca leídos.
        let init = l.initialized_size.min(size);
        assert_eq!(r.count(RangeState::Uninitialized), size - init, "{what}");
        let cluster = u64::from(l.cluster_size);
        let sparse_bytes: u64 = sparse_ranges(&l.runs)
            .iter()
            .map(|&(v, n)| {
                let a = (v * cluster).min(init);
                let b = ((v + n) * cluster).min(init);
                b - a
            })
            .sum();
        assert_eq!(r.count(RangeState::Sparse), sparse_bytes, "{what}");
    }
    assert_eq!(sha, want_sha, "{what}: SHA-256");
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn file_contents_match_every_manifest() {
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut damaged = 0;
    for m in manifests() {
        let name = m.name().to_string();
        let v = open(&m);
        for (f, ads) in m.files.iter().zip(&m.ads) {
            let n: u64 = f["mft_record"].parse().unwrap();
            let what = format!("{name} {}", f["path"]);
            if f.get("mft_damaged").map(String::as_str) == Some("true") {
                match v.open_stream(n, "") {
                    Err(NtfsError::RecordUnusable { state, .. }) => {
                        assert_eq!(state, RecordState::Damaged, "{what}")
                    }
                    other => panic!("{what}: {other:?}"),
                }
                damaged += 1;
                continue;
            }
            let file = v.file(n).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(file.anomalies.is_empty(), "{what}: {:?}", file.anomalies);
            assert!(file.standard_information().is_some(), "{what}");
            assert!(!file.file_names().is_empty(), "{what}");
            // Flujos $DATA: el principal (ficheros) y los alternativos del manifiesto.
            let mut want_streams: Vec<String> = ads.iter().map(|a| a["name"].clone()).collect();
            if f["type"] == "file" {
                want_streams.push(String::new());
            }
            want_streams.sort();
            assert_eq!(file.data_streams(), want_streams, "{what}");
            if f["type"] == "file" {
                let s = v.stream_of(&file, "").unwrap();
                assert_eq!(s.record_in_use(), f["state"] == "live", "{what}");
                let (kind, sha) = match (f["state"].as_str(), f.get("recovered_sha256")) {
                    ("overwritten", Some(r)) => ("overwritten", r),
                    ("deleted", None) => {
                        assert_eq!(f["recoverable"], "full", "{what}");
                        ("deleted", &f["sha256"])
                    }
                    ("live", None) => ("live", &f["sha256"]),
                    other => panic!("{what}: {other:?}"),
                };
                check_stream(&what, &s, f, sha);
                *by_kind.entry(kind).or_default() += 1;
            }
            for a in ads {
                let w = format!("{what}:{}", a["name"]);
                let s = v.stream_of(&file, &a["name"]).unwrap();
                check_stream(&w, &s, a, &a["sha256"]);
                *by_kind.entry("ads").or_default() += 1;
            }
            // Un flujo que no existe es un error tipado.
            assert!(matches!(
                v.stream_of(&file, "no-existe"),
                Err(NtfsError::Stream { .. })
            ));
        }
    }
    let got: Vec<(&str, usize)> = by_kind.into_iter().collect();
    println!("flujos comparados por SHA-256: {got:?}; registros dañados: {damaged}");
    assert_eq!(damaged, 4);
    // 439 flujos con el SHA-256 del manifiesto: 419 vivos, 12 borrados íntegros, 3 sobrescritos
    // (contenido que devuelve TSK) y 5 flujos alternativos.
    assert_eq!(
        got,
        vec![
            ("ads", 5),
            ("deleted", 12),
            ("live", 419),
            ("overwritten", 3)
        ]
    );
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn unreadable_clusters_inside_a_file_are_reported_per_range() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "ntfs_basic")
        .unwrap();
    let f = m
        .files
        .iter()
        .find(|f| f["path"] == "/datos/historico.bin")
        .unwrap();
    let n: u64 = f["mft_record"].parse().unwrap();
    let run = &int_lists(&f["runs"])[0];
    assert_eq!(run[2], 98);
    let cluster = m.vol_num("cluster_size");
    let lba = |vcn: u64, sector: u64| (run[1] + vcn) * cluster / 512 + sector;
    // Dos sectores contiguos en el clúster 10 y uno en el 50.
    let bad = [lba(10, 2), lba(10, 3), lba(50, 7)];
    let healthy = open(&m);
    let mut want = Vec::new();
    healthy
        .open_stream(n, "")
        .unwrap()
        .copy_to(&mut want)
        .unwrap();
    let dev = FaultyDevice::new(ImageFile::open(generated().join(&m.top["file"]), 512).unwrap())
        .with_bad(bad);
    let v = NtfsVolume::open(Arc::new(dev)).unwrap();
    let s = v.open_stream(n, "").unwrap();
    let mut got = Vec::new();
    let r = s.copy_to(&mut got).unwrap();
    assert_eq!(got.len(), want.len());
    let a = 10 * cluster + 1024;
    let b = 50 * cluster + 7 * 512;
    let want_ranges = [(a, 1024, Some((bad[0], 2))), (b, 512, Some((bad[2], 1)))];
    assert_eq!(r.untrusted.len(), want_ranges.len());
    for (x, w) in r.untrusted.iter().zip(want_ranges) {
        assert_eq!((x.offset, x.length, x.lbas), w);
    }
    assert!(r
        .untrusted
        .iter()
        .all(|x| x.state == RangeState::Unreadable));
    assert_eq!(r.untrusted_bytes(), 1536);
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        let i = i as u64;
        let inside = (a..a + 1024).contains(&i) || (b..b + 512).contains(&i);
        if inside {
            assert_eq!(*g, b"BAD!"[(i % 4) as usize], "byte {i}");
        } else {
            assert_eq!(g, w, "byte {i}");
        }
    }
}

/// Árbol (#22) de un volumen con las opciones por defecto (con búsqueda de huérfanos).
fn full_tree(v: &NtfsVolume) -> mr_fs_ntfs::Tree {
    v.tree(&TreeOptions::default(), &NoTreeObserver).unwrap()
}

fn is_damaged(f: &HashMap<String, String>) -> bool {
    f.get("mft_damaged").map(String::as_str) == Some("true")
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn tree_matches_the_live_paths_of_every_manifest() {
    let mut total = 0;
    let mut accented = 0;
    let mut deepest = 0;
    for m in manifests() {
        let name = m.name().to_string();
        let v = open(&m);
        assert_eq!(v.upcase().source(), &UpcaseSource::Volume, "{name}");
        let t = full_tree(&v);
        assert_eq!(t.status, TreeStatus::Complete, "{name}");
        assert!(t.anomalies.is_empty(), "{name}: {:?}", t.anomalies);
        assert!(t.orphans.is_empty(), "{name}: {:?}", t.orphans);
        assert_eq!(t.nodes[0].path, "/");
        let got: BTreeSet<(String, &str, u64)> = t
            .nodes
            .iter()
            .skip(1)
            .filter(|n| n.entry.state.is_live() && !n.entry.system)
            .map(|n| {
                let ty = if n.entry.is_directory { "dir" } else { "file" };
                (n.path.clone(), ty, n.entry.record())
            })
            .collect();
        let want: BTreeSet<(String, &str, u64)> = m
            .files
            .iter()
            .filter(|f| f["state"] == "live" && !is_damaged(f))
            .map(|f| {
                (
                    f["path"].clone(),
                    f["type"].as_str(),
                    f["mft_record"].parse().unwrap(),
                )
            })
            .collect();
        assert_eq!(got, want, "{name}");
        // Entradas que apuntan a registros dañados: listadas con su estado, nunca vivas.
        let damaged: BTreeSet<String> = t
            .nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.entry.state,
                    EntryState::RecordUnusable {
                        state: RecordState::Damaged,
                        ..
                    }
                )
            })
            .map(|n| n.path.clone())
            .collect();
        let want_damaged: BTreeSet<String> = m
            .files
            .iter()
            .filter(|f| is_damaged(f))
            .map(|f| f["path"].clone())
            .collect();
        assert_eq!(damaged, want_damaged, "{name}");
        // Nada fuera de lo vivo y lo dañado del manifiesto (los borrados no se listan como vivos).
        assert!(
            t.nodes
                .iter()
                .all(|n| n.entry.state.is_live() || damaged.contains(&n.path)),
            "{name}"
        );
        // Tamaños de los ficheros vivos.
        for f in m
            .files
            .iter()
            .filter(|f| f["state"] == "live" && f["type"] == "file")
        {
            if is_damaged(f) {
                continue;
            }
            let n = t.nodes.iter().find(|n| n.path == f["path"]).unwrap();
            assert_eq!(n.entry.size.to_string(), f["size"], "{name} {}", f["path"]);
        }
        total += want.len();
        accented += want.iter().filter(|(p, _, _)| !p.is_ascii()).count();
        deepest = deepest.max(
            want.iter()
                .map(|(p, _, _)| p.matches('/').count())
                .max()
                .unwrap_or(0),
        );
    }
    println!("rutas vivas comparadas: {total} (con caracteres no ASCII: {accented}; profundidad máxima {deepest})");
    assert!(accented >= 50 && deepest >= 4, "{accented} {deepest}");
}

#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn every_live_path_resolves_to_its_record_and_content() {
    let mut files = 0;
    let mut dirs = 0;
    for m in manifests() {
        let name = m.name().to_string();
        let v = open(&m);
        assert_eq!(v.resolve_path("/").unwrap().record(), 5);
        for f in &m.files {
            let what = format!("{name} {}", f["path"]);
            let record: u64 = f["mft_record"].parse().unwrap();
            if is_damaged(f) {
                // El índice la lista, pero su registro está dañado: no se resuelve como viva.
                assert!(
                    matches!(v.resolve_path(&f["path"]), Err(NtfsError::NotFound { .. })),
                    "{what}"
                );
                continue;
            }
            if f["state"] != "live" {
                assert!(v.resolve_path(&f["path"]).is_err(), "{what}");
                continue;
            }
            let e = v
                .resolve_path(&f["path"])
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(e.record(), record, "{what}");
            assert!(e.state.is_live(), "{what}");
            // Sin distinguir mayúsculas ($UpCase del volumen) y con separador de Windows.
            let upper = f["path"].to_uppercase().replace('/', "\\");
            assert_eq!(v.resolve_path(&upper).unwrap().record(), record, "{what}");
            assert_eq!(v.path_of(record).unwrap(), f["path"], "{what}");
            if f["type"] == "file" {
                let s = v.open_stream(e.record(), "").unwrap();
                let (sha, r) = stream_sha(&s);
                assert_eq!(r.untrusted_bytes(), 0, "{what}");
                assert_eq!(sha, f["sha256"], "{what}");
                files += 1;
            } else {
                assert!(e.is_directory, "{what}");
                dirs += 1;
            }
        }
        assert!(matches!(
            v.resolve_path("/no/existe.txt"),
            Err(NtfsError::NotFound { .. })
        ));
    }
    println!("rutas resueltas: {files} ficheros (SHA-256) y {dirs} directorios");
    assert!(files > 400 && dirs > 80, "{files} {dirs}");
}

/// Un nodo INDX ilegible (sector no fiable en el nodo de la raíz) no bloquea el resto: se informa,
/// los demás directorios se listan y lo que colgaba de él aparece como huérfano con su ruta.
#[test]
#[ignore = "requiere el corpus NTFS (tools/fetch_ntfs_corpus.py)"]
fn unreadable_index_node_does_not_block_the_rest() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "ntfs_basic")
        .unwrap();
    let healthy = open(&m);
    let root = healthy.file(5).unwrap();
    let alloc = root
        .stream_layout(0xA0, "$I30", healthy.geometry())
        .unwrap();
    let lcn = alloc.runs[0].lcn.unwrap();
    let cluster = m.vol_num("cluster_size");
    let bad = lcn * cluster / 512 + 3;
    let dev = FaultyDevice::new(ImageFile::open(generated().join(&m.top["file"]), 512).unwrap())
        .with_bad([bad]);
    let v = NtfsVolume::open(Arc::new(dev)).unwrap();
    let listing = v.list_dir(5).unwrap();
    assert!(listing.has_unreadable());
    assert!(listing
        .anomalies
        .iter()
        .any(|a| a.issue.code() == "index_block_unreadable" && a.node == NodeId::Vcn(0)));
    let t = full_tree(&v);
    assert_eq!(t.status, TreeStatus::Complete);
    assert!(t
        .anomalies
        .iter()
        .any(|a| a.path == "/" && a.issue.code() == "index_block_unreadable"));
    // Lo alcanzado más lo que cuelga de los huérfanos (con su ruta) es todo lo vivo.
    let mut got: BTreeSet<String> = t
        .nodes
        .iter()
        .skip(1)
        .filter(|n| n.entry.state.is_live() && !n.entry.system)
        .map(|n| n.path.clone())
        .collect();
    assert!(t
        .orphans
        .iter()
        .any(|o| o.reason == OrphanReason::NotIndexed && o.name == "docs"));
    for o in &t.orphans {
        // Los de $Extend (sistema) también quedan huérfanos: no son del manifiesto.
        let p = v.path_of(o.record).unwrap();
        if !p.starts_with("/$") {
            got.insert(p);
        }
    }
    let want: BTreeSet<String> = m.files.iter().map(|f| f["path"].clone()).collect();
    assert_eq!(got, want);
    // Los directorios de debajo se siguen listando y resolviendo por debajo de la raíz.
    let docs = healthy.resolve_path("/docs").unwrap().record();
    assert!(v.list_dir(docs).unwrap().anomalies.is_empty());
    assert!(matches!(
        v.resolve_path("/docs"),
        Err(NtfsError::NotFound { .. })
    ));
}
