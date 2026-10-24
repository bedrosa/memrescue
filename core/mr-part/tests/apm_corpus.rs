//! Apple Partition Map contra el corpus sintético (`tests/corpus`, issue #17; esquema del manifiesto
//! en `docs/PRUEBAS.md` §1.1), siempre a través de `read_partition_table`.
//!
//! Requieren las imágenes generadas (`python -I -X utf8 tests/generators/gen_partition_images.py
//! tests/corpus/generated`; `MR_CORPUS_DIR` para otra ubicación). Se marcan `#[ignore]` para que
//! `cargo test` sin corpus no falle; se ejecutan con
//! `cargo test -p mr-part --test apm_corpus -- --ignored`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use mr_io::{BlockDevice, FaultyDevice, ImageFile};
use mr_part::apm::read_apm;
use mr_part::{read_partition_table, Anomaly, Confidence, Origin, PartitionKind, Scheme};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/generated"))
}

/// Manifiesto TOML plano: claves de nivel superior y tablas `[[nombre]]` en orden.
struct Manifest {
    top: HashMap<String, String>,
    tables: Vec<(String, HashMap<String, String>)>,
}

impl Manifest {
    fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("manifiesto legible");
        let mut m = Manifest {
            top: HashMap::new(),
            tables: Vec::new(),
        };
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
                m.tables.push((name.to_string(), HashMap::new()));
                continue;
            }
            let (k, v) = line.split_once(" = ").expect("clave = valor");
            let v = v.trim().trim_matches('"').to_string();
            match m.tables.last_mut() {
                Some((_, t)) => t.insert(k.trim().to_string(), v),
                None => m.top.insert(k.trim().to_string(), v),
            };
        }
        m
    }

    fn get(&self, k: &str) -> &str {
        self.top
            .get(k)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("falta {k}"))
    }

    fn num(&self, k: &str) -> u64 {
        self.get(k).parse().unwrap()
    }

    fn table(&self, name: &str) -> Vec<&HashMap<String, String>> {
        self.tables
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, t)| t)
            .collect()
    }

    fn name(&self) -> &str {
        self.get("name")
    }
}

fn manifests() -> Vec<Manifest> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(repo().join("tests/corpus/manifests"))
        .expect("directorio de manifiestos")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    paths.iter().map(|p| Manifest::load(p)).collect()
}

fn open(m: &Manifest, sector_size: u32) -> ImageFile {
    let p = generated().join(m.get("file"));
    assert!(
        p.exists(),
        "falta {}: genere el corpus con tests/generators/gen_partition_images.py",
        p.display()
    );
    ImageFile::open(&p, sector_size).unwrap()
}

/// `(código, lba, valor)`; el valor solo cuenta en los códigos `apm_*`.
type Norm = (String, i64, Option<u64>);

fn expected_anomalies(m: &Manifest) -> BTreeSet<Norm> {
    m.table("anomalies")
        .iter()
        .map(|a| {
            let code = a["code"].clone();
            let value = a
                .get("value")
                .filter(|_| code.starts_with("apm_"))
                .map(|v| v.parse().unwrap());
            (code, a["lba"].parse().unwrap(), value)
        })
        .collect()
}

fn engine_anomalies<'a>(all: impl Iterator<Item = &'a Anomaly>) -> BTreeSet<Norm> {
    all.map(|a| {
        let value = a.value().filter(|_| a.code().starts_with("apm_"));
        (a.code().to_string(), a.lba().unwrap() as i64, value)
    })
    .collect()
}

#[test]
#[ignore = "requiere el corpus generado"]
fn partition_table_matches_every_apm_manifest() {
    let (mut images, mut parts) = (0, 0);
    for m in manifests() {
        let dev = open(&m, m.num("sector_size") as u32);
        let t = read_partition_table(&dev).unwrap();
        let name = m.name();
        if m.get("scheme") != "apm" {
            assert!(t.apm.is_none(), "{name}: APM inesperado");
            assert!(read_apm(&dev).unwrap().is_none(), "{name}: falso APM");
            continue;
        }
        assert_eq!(t.scheme, Scheme::Apm, "{name}");
        let a = t.apm.as_ref().unwrap();
        assert_eq!(
            (
                u64::from(a.block_size),
                a.disk_blocks,
                u64::from(a.map_entries)
            ),
            (
                m.num("apm_block_size"),
                m.num("apm_disk_blocks"),
                m.num("apm_map_entries")
            ),
            "{name}"
        );
        assert!(a.ddm && a.map_found, "{name}");
        assert_eq!(t.sector_size, a.block_size);
        let want = m.table("partitions");
        assert_eq!(t.partitions.len(), want.len(), "{name}: nº de particiones");
        for (p, w) in t.partitions.iter().zip(&want) {
            let (Origin::Apm { index }, PartitionKind::Apm(kind)) = (p.origin, p.kind) else {
                panic!("{name}: {p:?}")
            };
            let e = t.apm_entry(p).unwrap();
            assert_eq!(
                (
                    p.start_lba,
                    p.sector_count,
                    u64::from(index),
                    kind.text(),
                    p.name.clone().unwrap(),
                    u64::from(e.status)
                ),
                (
                    w["start_lba"].parse().unwrap(),
                    w["sectors"].parse().unwrap(),
                    w["apm_index"].parse().unwrap(),
                    w["apm_type"].clone(),
                    w["apm_name"].clone(),
                    w["apm_status"].parse().unwrap()
                ),
                "{name}: partición {}",
                w["label"]
            );
            // Confianza por regla: empieza fuera = inválida; termina fuera = sospechosa.
            let codes: Vec<&str> = p.anomalies.iter().map(Anomaly::code).collect();
            let expected_ok = match codes.as_slice() {
                [] => p.confidence == Confidence::Valid,
                ["apm_entry_outside_disk"] => matches!(p.confidence, Confidence::Invalid(_)),
                ["apm_entry_partially_outside_disk"] => {
                    matches!(p.confidence, Confidence::Suspicious(_))
                }
                _ => false,
            };
            assert!(expected_ok, "{name}: {codes:?} {:?}", p.confidence);
            parts += 1;
        }
        assert_eq!(
            engine_anomalies(t.all_anomalies()),
            expected_anomalies(&m),
            "{name}: anomalías"
        );
        images += 1;
    }
    assert_eq!((images, parts), (3, 11));
}

#[test]
#[ignore = "requiere el corpus generado"]
fn unreadable_map_sectors_are_never_interpreted() {
    let all = manifests();
    let m = all.iter().find(|m| m.name() == "apm_basic").unwrap();
    let plain = read_partition_table(&open(m, 512)).unwrap();
    assert_eq!(plain.partitions.len(), 4);
    for lba in 0..=4u64 {
        let dev = FaultyDevice::new(open(m, 512)).with_bad([lba]);
        let t = read_partition_table(&dev).unwrap();
        assert_eq!(t.scheme, Scheme::Apm, "LBA {lba}");
        let codes: Vec<(&str, Option<u64>)> =
            t.all_anomalies().map(|a| (a.code(), a.lba())).collect();
        assert!(
            codes.contains(&("unreadable_sector", Some(lba))),
            "{codes:?}"
        );
        match lba {
            // DDM ilegible: el mapa se encuentra con el tamaño de sector.
            0 => assert_eq!(t.partitions.len(), 4),
            // Primera entrada ilegible: sin mapa, nada se lista.
            1 => {
                assert!(t.partitions.is_empty());
                assert!(codes.contains(&("apm_map_missing", Some(1))));
                assert!(matches!(t.confidence(), Confidence::Invalid(_)));
            }
            // Otra entrada ilegible: se pierde solo esa.
            _ => {
                assert_eq!(t.partitions.len(), 3);
                assert!(t
                    .partitions
                    .iter()
                    .all(|p| p.origin != Origin::Apm { index: lba as u32 }));
            }
        }
    }
}

#[test]
#[ignore = "requiere el corpus generado"]
fn apm_read_with_4096_byte_sectors_keeps_512_byte_blocks() {
    let all = manifests();
    let m = all.iter().find(|m| m.name() == "apm_basic").unwrap();
    let dev = open(m, 4096);
    let t = read_partition_table(&dev).unwrap();
    assert_eq!((t.scheme, t.sector_size), (Scheme::Apm, 512));
    assert_eq!(t.partitions.len(), 4);
    assert!(t
        .anomalies
        .iter()
        .any(|a| a.code() == "apm_sector_size_mismatch"));
    // Abrir exige el tamaño de bloque del mapa: las LBA no se convierten en silencio.
    let err = mr_part::open_coordinates(&t, 1, 4096, dev.sector_count(), false).unwrap_err();
    assert!(err.contains("512 B"), "{err}");
}
