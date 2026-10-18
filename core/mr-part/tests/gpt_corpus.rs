//! GPT contra el corpus sintético (`tests/corpus`, issue #14; esquema del manifiesto en
//! `docs/PRUEBAS.md` §1.1).
//!
//! Requieren las imágenes generadas (`python -I -X utf8 tests/generators/gen_partition_images.py
//! tests/corpus/generated`; `MR_CORPUS_DIR` para otra ubicación). Se marcan `#[ignore]` para que
//! `cargo test` sin corpus no falle; se ejecutan con
//! `cargo test -p mr-part --test gpt_corpus -- --ignored`.
//!
//! Correspondencia de códigos del manifiesto con [`GptAnomaly`] en [`manifest_code`]. Los códigos
//! de MBR/EBR y de la búsqueda de volúmenes ([`NOT_GPT_CODES`]) los comprueban #12 y #15 y aquí
//! se ignoran.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use mr_io::{BlockDevice, FaultyDevice, ImageFile, MemDevice};
use mr_part::gpt::{
    format_guid, read_gpt, EntriesState, GptAnomaly, GptCopy, GptRead, HeaderState,
};
use mr_part::{Anomaly, Confidence, PartitionKind};

/// Códigos del manifiesto que no produce el analizador GPT.
const NOT_GPT_CODES: [&str; 14] = [
    "mbr_missing",
    "mbr_empty",
    "mbr_signature_missing",
    "mbr_multiple_active",
    "mbr_overlap",
    "mbr_unknown_type",
    "mbr_outside_disk",
    "ebr_loop",
    "ebr_link_outside_disk",
    "ebr_link_outside_extended",
    "hybrid_mbr",
    "protective_mbr_size_mismatch",
    "mbr_displaced",
    "superfloppy",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generated() -> PathBuf {
    std::env::var_os("MR_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/generated"))
}

/// Manifiesto TOML simple: claves de nivel superior y tablas `[[nombre]]` en orden.
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

    fn get(&self, k: &str) -> Option<&str> {
        self.top.get(k).map(String::as_str)
    }

    fn num(&self, k: &str) -> i64 {
        self.get(k)
            .unwrap_or_else(|| panic!("falta {k}"))
            .parse()
            .unwrap()
    }

    fn table(&self, name: &str) -> impl Iterator<Item = &HashMap<String, String>> + '_ {
        let name = name.to_string();
        self.tables
            .iter()
            .filter(move |(n, _)| *n == name)
            .map(|(_, t)| t)
    }

    fn name(&self) -> &str {
        self.get("name").unwrap()
    }

    fn unreadable(&self) -> Vec<u64> {
        self.get("unreadable_lbas")
            .map(|v| {
                v.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().parse().unwrap())
                    .collect()
            })
            .unwrap_or_default()
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
    let p = generated().join(m.get("file").unwrap());
    assert!(
        p.exists(),
        "falta {}: genere el corpus con tests/generators/gen_partition_images.py",
        p.display()
    );
    ImageFile::open(&p, sector_size).unwrap()
}

/// `(código, lba de la imagen, valor)` del manifiesto para una anomalía GPT; `None` si la
/// anomalía no tiene código en el manifiesto (informativas o de reglas que el corpus no ejercita).
fn manifest_code(a: &GptAnomaly, shift: i64) -> Option<(String, i64, Option<i64>)> {
    let tag = |c: &GptCopy| match c {
        GptCopy::Primary => "primary",
        GptCopy::Backup => "backup",
    };
    let image = |disk: u64| disk as i64 - shift;
    Some(match a {
        GptAnomaly::HeaderMissing { copy, lba } => (
            format!("gpt_{}_header_missing", tag(copy)),
            *lba as i64,
            None,
        ),
        GptAnomaly::HeaderOutsideImage { copy, disk_lba } => (
            format!("gpt_{}_header_outside_image", tag(copy)),
            image(*disk_lba),
            None,
        ),
        GptAnomaly::HeaderCrc { copy, lba, .. } | GptAnomaly::HeaderSize { copy, lba, .. } => {
            (format!("gpt_{}_header_crc", tag(copy)), *lba as i64, None)
        }
        GptAnomaly::EntriesCrc { copy, lba, .. } => {
            (format!("gpt_{}_entries_crc", tag(copy)), *lba as i64, None)
        }
        GptAnomaly::EntriesOutsideImage { copy, disk_lba } => (
            format!("gpt_{}_entries_outside_image", tag(copy)),
            image(*disk_lba),
            None,
        ),
        GptAnomaly::UsableRangeOutsideImage { lba, .. } => {
            ("gpt_usable_range_outside_image".into(), *lba as i64, None)
        }
        GptAnomaly::ImageShifted { offset } => ("image_shifted".into(), -offset, Some(*offset)),
        _ => return None,
    })
}

fn header_state(s: HeaderState) -> &'static str {
    match s {
        HeaderState::Valid => "valid",
        HeaderState::Crc => "crc",
        HeaderState::Missing => "missing",
        HeaderState::OutsideImage => "outside_image",
        HeaderState::Invalid => "invalid",
        HeaderState::Unreadable => "unreadable",
    }
}

fn entries_state(s: EntriesState) -> &'static str {
    match s {
        EntriesState::Valid => "valid",
        EntriesState::Crc => "crc",
        EntriesState::OutsideImage => "outside_image",
        EntriesState::Unreadable => "unreadable",
        EntriesState::Unchecked => "unchecked",
    }
}

fn read_marker(dev: &dyn BlockDevice, lba: u64, sector_size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; sector_size as usize];
    dev.read_sectors(lba, 1, &mut buf).unwrap();
    buf
}

/// Compara una lectura con todo lo que el manifiesto declara de la GPT.
fn check_against_manifest(m: &Manifest, dev: &dyn BlockDevice, r: &GptRead) {
    let name = m.name();
    let ss = m.num("sector_size") as u32;
    assert_eq!(r.sector_size, ss, "{name}: tamaño de sector");
    for (tag, c) in [("primary", &r.primary), ("backup", &r.backup)] {
        assert_eq!(
            Some(header_state(c.header)),
            m.get(&format!("gpt_{tag}_header")),
            "{name}: cabecera {tag}"
        );
        assert_eq!(
            Some(entries_state(c.entries)),
            m.get(&format!("gpt_{tag}_entries")),
            "{name}: entradas {tag}"
        );
        let expected_lba = m
            .get(&format!("gpt_{tag}_header_image_lba"))
            .map(|v| v.parse::<u64>().unwrap());
        if c.has_signature() {
            assert_eq!(
                c.image_lba, expected_lba,
                "{name}: LBA de la cabecera {tag}"
            );
        } else {
            assert_eq!(expected_lba, None, "{name}: {tag} sin firma");
        }
    }
    let selected = match r.selected {
        Some(GptCopy::Primary) => "primary",
        Some(GptCopy::Backup) => "backup",
        None => "none",
    };
    assert_eq!(
        Some(selected),
        m.get("gpt_selected"),
        "{name}: copia elegida"
    );
    assert_eq!(r.confidence.is_usable(), r.selected.is_some(), "{name}");
    if r.selected.is_some() {
        assert_eq!(
            r.shift,
            m.num("leading_sectors_missing"),
            "{name}: desplazamiento"
        );
        assert_eq!(
            r.trailing_sectors_missing as i64,
            m.num("trailing_sectors_missing"),
            "{name}: sectores finales ausentes"
        );
        let h = r.header().unwrap();
        assert_eq!(h.first_usable_lba as i64, m.num("first_usable_lba"));
        assert_eq!(h.last_usable_lba as i64, m.num("last_usable_lba"));
        assert_eq!(h.entry_count as i64, m.num("entry_count"));
        assert_eq!(h.entry_size as i64, m.num("entry_size"));
        assert_eq!(
            format!("{:08x}", h.entries_crc32),
            m.get("entries_crc32").unwrap()
        );
        assert_eq!(format_guid(&h.disk_guid), m.get("disk_guid").unwrap());
    }

    // Particiones: las de la copia elegida, con las LBA del disco.
    let expected: Vec<_> = m.table("partitions").collect();
    assert_eq!(r.partitions.len(), expected.len(), "{name}: particiones");
    for (p, e) in r.partitions.iter().zip(&expected) {
        let q = &p.partition;
        assert_eq!(q.start_lba.to_string(), e["start_lba"], "{name}");
        assert_eq!(q.sector_count.to_string(), e["sectors"], "{name}");
        let PartitionKind::Gpt(ty) = q.kind else {
            panic!("{name}: tipo no GPT")
        };
        assert_eq!(format_guid(&ty), e["gpt_type"], "{name}");
        assert_eq!(format_guid(&p.unique_guid), e["unique_guid"], "{name}");
        assert_eq!(q.name.as_deref(), Some(e["gpt_name"].as_str()), "{name}");
        assert_eq!(q.confidence, Confidence::Valid, "{name}: {:?}", q.anomalies);
        // La LBA de disco más el desplazamiento informado lleva al marcador de la imagen.
        let at = r.image_lba(q.start_lba).unwrap();
        if let Some(ml) = e.get("marker_lba") {
            assert_eq!(at.to_string(), *ml, "{name}: LBA del marcador");
        }
        let sec = read_marker(dev, at, dev.sector_size());
        assert!(
            sec.starts_with(e["marker"].as_bytes()),
            "{name}: marcador en LBA {at} de la imagen"
        );
    }

    // Anomalías: el subconjunto GPT del manifiesto, exacto.
    let mut want = BTreeSet::new();
    for a in m.table("anomalies") {
        let code = a["code"].as_str();
        if NOT_GPT_CODES.contains(&code) {
            continue;
        }
        assert!(
            code.starts_with("gpt_") || code == "image_shifted",
            "{name}: código desconocido {code}"
        );
        want.insert((
            code.to_string(),
            a["lba"].parse::<i64>().unwrap(),
            a.get("value").map(|v| v.parse::<i64>().unwrap()),
        ));
    }
    let mut got = BTreeSet::new();
    let mut extra = Vec::new();
    for a in &r.anomalies {
        match a {
            Anomaly::Gpt(g) => match manifest_code(g, r.shift) {
                Some(c) => {
                    got.insert(c);
                }
                None => extra.push(g.clone()),
            },
            other => panic!("{name}: anomalía inesperada {other:?}"),
        }
    }
    assert_eq!(got, want, "{name}: anomalías GPT");
    // Las únicas anomalías sin código en el manifiesto son las informativas esperadas.
    for g in &extra {
        let expected = match g {
            GptAnomaly::BackupInUse { .. } => r.selected == Some(GptCopy::Backup),
            GptAnomaly::ImageTruncated { missing } => *missing == r.trailing_sectors_missing,
            _ => false,
        };
        assert!(expected, "{name}: anomalía no esperada {g:?}");
    }
    assert_eq!(
        extra
            .iter()
            .any(|g| matches!(g, GptAnomaly::BackupInUse { .. })),
        r.selected == Some(GptCopy::Backup),
        "{name}: BackupInUse"
    );
}

#[test]
#[ignore = "requiere el corpus generado (tests/generators/gen_partition_images.py)"]
fn gpt_matches_every_manifest_with_gpt_state() {
    let mut checked = Vec::new();
    for m in manifests() {
        if m.get("gpt_selected").is_none() {
            continue;
        }
        let dev = open(&m, m.num("sector_size") as u32);
        let r = read_gpt(&dev).unwrap();
        check_against_manifest(&m, &dev, &r);
        // Lo mismo desde memoria (otra implementación de BlockDevice).
        let data = std::fs::read(generated().join(m.get("file").unwrap())).unwrap();
        let mem = MemDevice::new(data, m.num("sector_size") as u32).unwrap();
        assert_eq!(read_gpt(&mem).unwrap(), r, "{}", m.name());
        checked.push(m.name().to_string());
    }
    for n in [
        "gpt_basic",
        "gpt_4kn",
        "gpt_primary_header_bad",
        "gpt_entries_crc_bad",
        "gpt_backup_header_bad",
        "gpt_backup_missing",
        "gpt_both_bad",
        "gpt_4kn_primary_bad",
        "gpt_shifted_start",
        "gpt_shifted_start_1",
        "gpt_truncated_end",
        "hybrid_mbr_gpt",
        "protective_mbr_only",
    ] {
        assert!(checked.iter().any(|c| c == n), "no se comprobó {n}");
    }
}

#[test]
#[ignore = "requiere el corpus generado (tests/generators/gen_partition_images.py)"]
fn images_without_gpt_state_yield_no_gpt() {
    for m in manifests() {
        if m.get("gpt_selected").is_some() {
            continue;
        }
        let dev = open(&m, m.num("sector_size") as u32);
        let bad = m.unreadable();
        // Los escenarios con `unreadable_lbas` se leen con esos sectores ilegibles.
        let faulty = FaultyDevice::new(dev).with_bad(bad.iter().copied());
        let r = read_gpt(&faulty).unwrap();
        assert_eq!(r.selected, None, "{}", m.name());
        assert!(!r.signature_found(), "{}", m.name());
        assert!(r.partitions.is_empty());
        assert!(!r.confidence.is_usable());
        for lba in bad {
            // El relleno que imita una firma nunca se interpreta: o no se leyó, o se informó.
            let touched = faulty.reads_of(lba) > 0;
            let reported = r.anomalies.contains(&Anomaly::UnreadableSector { lba });
            assert_eq!(touched, reported, "{}: LBA {lba}", m.name());
        }
    }
}

#[test]
#[ignore = "requiere el corpus generado (tests/generators/gen_partition_images.py)"]
fn corpus_with_the_other_sector_size() {
    let all = manifests();
    let find = |n: &str| all.iter().find(|m| m.name() == n).unwrap();
    // GPT de 4096 B abierta como dispositivo de 512 B.
    let m = find("gpt_4kn");
    let dev = open(m, 512);
    let r = read_gpt(&dev).unwrap();
    assert_eq!((r.sector_size, r.device_sector_size), (4096, 512));
    assert_eq!(r.selected, Some(GptCopy::Primary));
    assert!(r.gpt_anomalies().any(|a| *a
        == GptAnomaly::SectorSizeMismatch {
            device: 512,
            used: 4096
        }));
    assert_eq!(r.partitions.len(), 1);
    assert_eq!(r.partitions[0].partition.start_lba, 256);
    // Y con la primaria dañada: la copia se encuentra igual.
    let r = read_gpt(&open(find("gpt_4kn_primary_bad"), 512)).unwrap();
    assert_eq!((r.sector_size, r.selected), (4096, Some(GptCopy::Backup)));
    // GPT de 512 B abierta como dispositivo de 4096 B.
    let r = read_gpt(&open(find("gpt_basic"), 4096)).unwrap();
    assert_eq!((r.sector_size, r.device_sector_size), (512, 4096));
    assert_eq!(r.selected, Some(GptCopy::Primary));
    assert_eq!(r.partitions.len(), 3);
}

#[test]
#[ignore = "requiere el corpus generado (tests/generators/gen_partition_images.py)"]
fn unreadable_gpt_sectors_on_corpus() {
    let all = manifests();
    let find = |n: &str| all.iter().find(|m| m.name() == n).unwrap();
    let basic = find("gpt_basic");
    let read = |m: &Manifest, bad: &[u64]| {
        let f = FaultyDevice::new(open(m, m.num("sector_size") as u32)).with_bad(bad.to_vec());
        read_gpt(&f).unwrap()
    };
    // Cabecera primaria ilegible: respaldo.
    let r = read(basic, &[1]);
    assert_eq!(r.primary.header, HeaderState::Unreadable);
    assert_eq!(r.selected, Some(GptCopy::Backup));
    assert!(r.anomalies.contains(&Anomaly::UnreadableSector { lba: 1 }));
    assert_eq!(r.partitions.len(), 3);
    // Un sector de las entradas primarias ilegible: respaldo.
    let r = read(basic, &[17]);
    assert_eq!(r.primary.entries, EntriesState::Unreadable);
    assert_eq!(r.selected, Some(GptCopy::Backup));
    // Respaldo ilegible: primaria, con el respaldo sin interpretar.
    let r = read(basic, &[32767]);
    assert_eq!(r.backup.header, HeaderState::Unreadable);
    assert_eq!(r.selected, Some(GptCopy::Primary));
    assert!(matches!(r.confidence, Confidence::ValidWithFixes(_)));
    // Las dos cabeceras ilegibles: ninguna copia y sin reinterpretar con otro tamaño.
    let r = read(basic, &[1, 32767]);
    assert_eq!(r.selected, None);
    assert_eq!(r.sector_size, 512);
    // 4Kn con la cabecera primaria ilegible.
    let r = read(find("gpt_4kn"), &[1]);
    assert_eq!(r.selected, Some(GptCopy::Backup));
    // Imagen desplazada cuyo respaldo (la única copia) es ilegible.
    let r = read(find("gpt_shifted_start"), &[4032]);
    assert_eq!(r.selected, None);
    assert_eq!(r.shift, 0);
    // En cada imagen con GPT, el sector de la cabecera elegida ilegible: nunca se usa esa copia.
    for m in all.iter().filter(|m| m.get("gpt_selected").is_some()) {
        let sel = m.get("gpt_selected").unwrap();
        if sel == "none" {
            continue;
        }
        let lba: u64 = m
            .get(&format!("gpt_{sel}_header_image_lba"))
            .unwrap()
            .parse()
            .unwrap();
        let r = read(m, &[lba]);
        let copy = if sel == "primary" {
            GptCopy::Primary
        } else {
            GptCopy::Backup
        };
        assert_ne!(r.selected, Some(copy), "{}", m.name());
        assert!(r.anomalies.contains(&Anomaly::UnreadableSector { lba }));
    }
}
