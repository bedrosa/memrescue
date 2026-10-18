//! Lector NTFS (#20) contra el corpus NTFS (ADR-024, esquema del manifiesto en `docs/PRUEBAS.md`
//! §1.2). La verdad de los manifiestos la obtuvo The Sleuth Kit, no MemRescue.
//!
//! Requieren las imágenes descargadas (`python -I -X utf8 tools/fetch_ntfs_corpus.py`, en
//! `tests/corpus/ntfs/generated`; `MR_NTFS_CORPUS` para otra ubicación). Se marcan `#[ignore]` y se
//! ejecutan con `cargo test -p mr-fs-ntfs --test ntfs_corpus -- --ignored`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_fs_ntfs::runs::{merge_contiguous, Run};
use mr_fs_ntfs::{
    BackupState, BootSource, Confidence, NtfsVolume, RecordOutcome, RecordSource, RecordState,
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
/// repetidas (`[[damage]]`, `[[files]]`; los `[[files.ads]]` se ignoran aquí).
struct Manifest {
    top: HashMap<String, String>,
    tables: HashMap<String, HashMap<String, String>>,
    damage: Vec<HashMap<String, String>>,
    files: Vec<HashMap<String, String>>,
}

impl Manifest {
    fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("manifiesto legible");
        let mut m = Manifest {
            top: HashMap::new(),
            tables: HashMap::new(),
            damage: Vec::new(),
            files: Vec::new(),
        };
        enum Cur {
            Top,
            Table(String),
            Damage,
            File,
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
                        Cur::File
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
