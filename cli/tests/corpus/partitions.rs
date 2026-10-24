//! Tablas de particiones del corpus (#12): el analizador MBR/EBR del motor frente a la verdad de
//! cada manifiesto (esquema v2, `docs/PRUEBAS.md` §1.1), con sectores ilegibles simulados, imágenes
//! desplazadas y mutaciones deterministas de las tablas.
//!
//! Correspondencia de códigos del manifiesto con las anomalías tipadas de `mr_part::mbr`:
//! `mbr_missing` = `NoTable`, `mbr_empty` = `Empty`, `mbr_signature_missing` = `SignatureMissing`,
//! `mbr_multiple_active` = `MultipleActive`, `mbr_overlap` = `Overlap`, `mbr_unknown_type` =
//! `UnknownType`, `mbr_outside_disk` = `OutsideDisk` o `PartiallyOutsideDisk`, `ebr_loop` =
//! `EbrLoop`, `ebr_link_outside_disk` = `LinkOutsideDisk`, `ebr_link_outside_extended` =
//! `LinkOutsideExtended`, `hybrid_mbr` = `Hybrid`, `protective_mbr_size_mismatch` =
//! `ProtectiveSizeMismatch`, `superfloppy` = `Superfloppy`. Los códigos `gpt_*` son del lector GPT
//! (#13), los `apm_*`, del Apple Partition Map (#17), e `image_shifted` / `mbr_displaced`, de la
//! búsqueda de particiones perdidas (#15): aquí se ignoran (los desplazamientos se comprueban aparte
//! con las coordenadas del manifiesto).

use mr_api::mr_part::mbr::{self, MbrAnomaly, NoTableReason};
use mr_api::mr_part::{Anomaly, Origin, PartitionKind, Scheme};
use mr_io::{BlockDevice, FaultyDevice, MemDevice};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Lectura mínima del manifiesto (TOML plano generado por gen_partition_images.py)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
}

impl Value {
    fn parse(v: &str) -> Value {
        let v = v.trim();
        if let Some(inner) = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            return Value::List(
                inner
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(Value::parse)
                    .collect(),
            );
        }
        if let Some(s) = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            return Value::Str(s.replace("\\\"", "\"").replace("\\\\", "\\"));
        }
        match v {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            n => Value::Int(n.parse().unwrap_or_else(|_| panic!("valor TOML: {n}"))),
        }
    }
}

type Table = HashMap<String, Value>;

#[derive(Debug, Default)]
struct Manifest {
    top: Table,
    tables: HashMap<String, Vec<Table>>,
}

impl Manifest {
    fn load(path: &Path) -> Manifest {
        let text = std::fs::read_to_string(path).unwrap();
        let mut m = Manifest::default();
        let mut current: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
                m.tables
                    .entry(name.to_string())
                    .or_default()
                    .push(Table::new());
                current = Some(name.to_string());
                continue;
            }
            let (k, v) = line.split_once(" = ").expect("clave = valor");
            let target = match &current {
                Some(t) => m.tables.get_mut(t).unwrap().last_mut().unwrap(),
                None => &mut m.top,
            };
            target.insert(k.trim().to_string(), Value::parse(v));
        }
        m
    }

    fn str(&self, k: &str) -> &str {
        match &self.top[k] {
            Value::Str(s) => s,
            v => panic!("{k}: {v:?}"),
        }
    }

    fn int(&self, k: &str) -> i64 {
        int(&self.top, k).unwrap_or_else(|| panic!("{k}"))
    }

    fn list(&self, k: &str) -> Vec<i64> {
        match self.top.get(k) {
            Some(Value::List(v)) => v
                .iter()
                .map(|x| match x {
                    Value::Int(i) => *i,
                    o => panic!("{o:?}"),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn table(&self, name: &str) -> &[Table] {
        self.tables.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    fn name(&self) -> &str {
        self.str("name")
    }
}

fn int(t: &Table, k: &str) -> Option<i64> {
    match t.get(k) {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    }
}

fn boolean(t: &Table, k: &str) -> bool {
    matches!(t.get(k), Some(Value::Bool(true)))
}

fn labels(t: &Table) -> Vec<String> {
    match t.get("partitions") {
        Some(Value::List(v)) => v
            .iter()
            .map(|x| match x {
                Value::Str(s) => s.clone(),
                o => panic!("{o:?}"),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("corpus")
}

fn manifests() -> Vec<Manifest> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus().join("manifests"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.toml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    paths.iter().map(|p| Manifest::load(p)).collect()
}

fn image(m: &Manifest) -> Vec<u8> {
    let p = corpus().join("generated").join(m.str("file"));
    assert!(
        p.exists(),
        "falta {}: ejecute scripts/test.ps1 o el generador",
        p.display()
    );
    std::fs::read(p).unwrap()
}

fn device(m: &Manifest) -> MemDevice {
    MemDevice::new(image(m), m.int("sector_size") as u32).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Verdad esperada del manifiesto vista desde el MBR
// ---------------------------------------------------------------------------------------------

/// Códigos del manifiesto que corresponden al analizador MBR/EBR.
const MBR_CODES: &[&str] = &[
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
    "superfloppy",
];

/// Códigos que pertenecen a otros analizadores (se ignoran aquí).
fn foreign_code(code: &str) -> bool {
    code.starts_with("gpt_")
        || code.starts_with("apm_")
        || code == "image_shifted"
        || code == "mbr_displaced"
}

/// Códigos con `value` significativo en el manifiesto.
const VALUE_CODES: &[&str] = &["mbr_unknown_type", "protective_mbr_size_mismatch"];

fn expected_scheme(m: &Manifest) -> Scheme {
    let missing = m
        .table("anomalies")
        .iter()
        .any(|a| matches!(a.get("code"), Some(Value::Str(c)) if c == "mbr_missing"));
    match m.str("scheme") {
        "mbr" => Scheme::Mbr,
        "hybrid" => Scheme::Hybrid,
        "gpt" if missing => Scheme::None,
        "gpt" => Scheme::Gpt,
        // El bloque 0 de un APM es el Driver Descriptor Map: el MBR no ve ninguna tabla.
        "none" | "apm" => Scheme::None,
        s => panic!("esquema {s}"),
    }
}

/// Particiones que debe listar el MBR: `(inicio, sectores, tipo, activa, lógica)`.
fn expected_partitions(m: &Manifest) -> Vec<(u64, u64, u8, bool, bool)> {
    let row = |t: &Table, logical: bool| {
        (
            int(t, "start_lba").unwrap() as u64,
            int(t, "sectors").unwrap() as u64,
            int(t, "mbr_type").unwrap() as u8,
            boolean(t, "active"),
            logical,
        )
    };
    match m.str("scheme") {
        "mbr" => m
            .table("partitions")
            .iter()
            .map(|t| row(t, boolean(t, "logical")))
            .collect(),
        "hybrid" => m
            .table("hybrid_mbr_entries")
            .iter()
            .filter(|t| int(t, "mbr_type") != Some(i64::from(mbr::TYPE_GPT_PROTECTIVE)))
            .map(|t| row(t, false))
            .collect(),
        _ => Vec::new(),
    }
}

/// Anomalía normalizada para comparar: `(código, lba, destino, valor)`.
type Norm = (String, i64, Option<i64>, Option<i64>);

fn expected_anomalies(m: &Manifest) -> Vec<Norm> {
    let mut v: Vec<Norm> = m
        .table("anomalies")
        .iter()
        .filter_map(|a| {
            let Some(Value::Str(code)) = a.get("code") else {
                panic!("{}: anomalía sin código", m.name());
            };
            if foreign_code(code) {
                return None;
            }
            assert!(
                MBR_CODES.contains(&code.as_str()),
                "{}: código del manifiesto sin correspondencia: {code}",
                m.name()
            );
            let value = VALUE_CODES
                .contains(&code.as_str())
                .then(|| int(a, "value"))
                .flatten();
            Some((
                code.clone(),
                int(a, "lba").unwrap(),
                int(a, "target_lba"),
                value,
            ))
        })
        .collect();
    v.sort();
    v
}

/// Código del manifiesto para una anomalía del motor (`None`: el manifiesto no la contempla).
fn manifest_code(a: &Anomaly) -> Option<&'static str> {
    match a {
        Anomaly::Mbr(MbrAnomaly::PartiallyOutsideDisk { .. }) => Some("mbr_outside_disk"),
        Anomaly::Mbr(m) if MBR_CODES.contains(&m.code()) => Some(m.code()),
        _ => None,
    }
}

fn engine_anomalies(scan: &mbr::MbrScan) -> Vec<Norm> {
    let mut v: Vec<Norm> = scan
        .anomalies
        .iter()
        .chain(scan.partitions.iter().flat_map(|p| p.anomalies.iter()))
        .map(|a| {
            let code = manifest_code(a).unwrap_or_else(|| a.code());
            let value = VALUE_CODES
                .contains(&code)
                .then(|| a.value().map(|x| x as i64))
                .flatten();
            (
                code.to_string(),
                a.lba().unwrap() as i64,
                a.target().map(|x| x as i64),
                value,
            )
        })
        .collect();
    v.sort();
    v
}

fn engine_partitions(scan: &mbr::MbrScan) -> Vec<(u64, u64, u8, bool, bool)> {
    scan.partitions
        .iter()
        .map(|p| {
            let PartitionKind::Mbr(t) = p.kind else {
                panic!("tipo GPT en el MBR")
            };
            (
                p.start_lba,
                p.sector_count,
                t,
                p.active,
                matches!(p.origin, Origin::Logical { .. }),
            )
        })
        .collect()
}

/// Comprueba que las anomalías con `partitions` del manifiesto señalan las mismas particiones.
fn check_anomaly_targets(m: &Manifest, scan: &mbr::MbrScan) {
    let by_label: HashMap<String, u64> = m
        .table("partitions")
        .iter()
        .map(|t| match t.get("label") {
            Some(Value::Str(l)) => (l.clone(), int(t, "start_lba").unwrap() as u64),
            _ => panic!(),
        })
        .collect();
    for a in m.table("anomalies") {
        let wanted = labels(a);
        if wanted.is_empty() {
            continue;
        }
        let Some(Value::Str(code)) = a.get("code") else {
            panic!()
        };
        let mut want: Vec<u64> = wanted.iter().map(|l| by_label[l]).collect();
        want.sort_unstable();
        let found = scan
            .anomalies
            .iter()
            .chain(scan.partitions.iter().flat_map(|p| p.anomalies.iter()))
            .find(|x| manifest_code(x) == Some(code.as_str()))
            .unwrap_or_else(|| panic!("{}: falta {code}", m.name()));
        let Anomaly::Mbr(found) = found else { panic!() };
        let mut got: Vec<u64> = found
            .origins()
            .iter()
            .map(|o| {
                scan.partitions
                    .iter()
                    .find(|p| p.origin == *o)
                    .unwrap_or_else(|| panic!("{}: {o:?} no listado", m.name()))
                    .start_lba
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "{}: particiones de {code}", m.name());
    }
}

// ---------------------------------------------------------------------------------------------
// Pruebas
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn mbr_view_matches_every_manifest() {
    let (mut checked, mut anomalies, mut partitions) = (0, 0, 0);
    for m in manifests() {
        let dev = device(&m);
        let scan = mbr::read_mbr(&dev).unwrap();
        let name = m.name();
        assert_eq!(scan.scheme, expected_scheme(&m), "{name}: esquema");
        assert_eq!(
            engine_partitions(&scan),
            expected_partitions(&m),
            "{name}: particiones"
        );
        assert_eq!(
            engine_anomalies(&scan),
            expected_anomalies(&m),
            "{name}: anomalías MBR exactas"
        );
        check_anomaly_targets(&m, &scan);
        if m.str("scheme") == "mbr" {
            let ebrs: Vec<u64> = m.list("ebr_lbas").iter().map(|&x| x as u64).collect();
            assert_eq!(scan.ebr_lbas, ebrs, "{name}: EBR recorridos");
            if let Some(start) = m.top.get("extended_start_lba") {
                let Value::Int(start) = start else { panic!() };
                let (s, n, _) = scan.extended.unwrap();
                assert_eq!(
                    (s as i64, n as i64),
                    (*start, m.int("extended_sectors")),
                    "{name}: extendida"
                );
            }
        }
        // Las particiones sin anomalías propias ni relacionadas son válidas.
        for p in &scan.partitions {
            if p.anomalies.is_empty()
                && !scan.anomalies.iter().any(|a| match a {
                    Anomaly::Mbr(x) => x.origins().contains(&p.origin),
                    _ => false,
                })
            {
                assert!(p.confidence.is_usable(), "{name}: {p:?}");
            }
        }
        // La tabla completa empieza por las mismas particiones MBR (la GPT se añade después).
        let t = mr_api::partitions(&dev).unwrap();
        assert_eq!(
            t.partitions[..scan.partitions.len()],
            scan.partitions[..],
            "{name}"
        );
        checked += 1;
        anomalies += expected_anomalies(&m).len();
        partitions += scan.partitions.len();
    }
    assert_eq!(
        (checked, anomalies, partitions),
        (41, 29, 25),
        "manifiestos, anomalías MBR y particiones MBR comprobados"
    );
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn decoys_inside_partitions_are_not_tables() {
    for m in manifests() {
        let decoys = m.list("decoy_lbas");
        if decoys.is_empty() {
            continue;
        }
        let scan = mbr::read_mbr(&device(&m)).unwrap();
        for d in decoys {
            // El señuelo tiene aspecto de tabla, pero no se interpreta: sus entradas (100, 700)
            // no aparecen en la lista.
            assert!(
                scan.partitions
                    .iter()
                    .all(|p| p.start_lba != d as u64 + 100),
                "{}: el señuelo de la LBA {d} se interpretó",
                m.name()
            );
        }
    }
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn unreadable_sectors_are_never_interpreted() {
    let (mut scenario, mut lba0, mut ebr) = (0, 0, 0);
    for m in manifests() {
        let ss = m.int("sector_size") as u32;
        let plain = mbr::read_mbr(&device(&m)).unwrap();
        // Escenarios del manifiesto: los sectores marcados contienen un relleno que imita una
        // firma; ilegibles, el resultado del MBR no cambia (no son sectores de tabla).
        let bad = m.list("unreadable_lbas");
        if !bad.is_empty() {
            let dev = FaultyDevice::new(MemDevice::new(image(&m), ss).unwrap())
                .with_bad(bad.iter().map(|&x| x as u64));
            let scan = mbr::read_mbr(&dev).unwrap();
            assert_eq!(scan, plain, "{}", m.name());
            scenario += 1;
        }
        // LBA 0 ilegible: nunca se interpreta, sea cual sea su contenido real.
        let dev = FaultyDevice::new(MemDevice::new(image(&m), ss).unwrap()).with_bad([0]);
        let scan = mbr::read_mbr(&dev).unwrap();
        assert_eq!(scan.scheme, Scheme::None, "{}", m.name());
        assert!(scan.partitions.is_empty());
        assert_eq!(scan.anomalies, vec![Anomaly::UnreadableSector { lba: 0 }]);
        lba0 += 1;
        // Último EBR ilegible: la cadena se detiene ahí y sus lógicas no aparecen.
        if let Some(&last) = m.list("ebr_lbas").last() {
            let last = last as u64;
            let dev = FaultyDevice::new(MemDevice::new(image(&m), ss).unwrap()).with_bad([last]);
            let scan = mbr::read_mbr(&dev).unwrap();
            assert!(scan
                .anomalies
                .contains(&Anomaly::UnreadableSector { lba: last }));
            let lost = plain
                .partitions
                .iter()
                .filter(|p| matches!(p.origin, Origin::Logical { ebr_lba, .. } if ebr_lba == last))
                .count();
            assert_eq!(
                scan.partitions.len(),
                plain.partitions.len() - lost,
                "{}",
                m.name()
            );
            assert!(scan
                .partitions
                .iter()
                .all(|p| !matches!(p.origin, Origin::Logical { ebr_lba, .. } if ebr_lba == last)));
            ebr += 1;
        }
    }
    assert_eq!((scenario, lba0, ebr), (2, 41, 4));
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn displaced_images_use_manifest_coordinates() {
    let mut checked = 0;
    for m in manifests() {
        let shift = m.int("leading_sectors_missing");
        if shift == 0 {
            continue;
        }
        let dev = Arc::new(device(&m));
        let scan = mbr::read_mbr(dev.as_ref()).unwrap();
        assert_eq!(scan.scheme, Scheme::None, "{}", m.name());
        // GPT a la que falta exactamente el MBR: la LBA 0 de la imagen es la cabecera primaria.
        if int(&m.top, "gpt_primary_header_image_lba") == Some(0) {
            assert_eq!(
                scan.anomalies,
                vec![Anomaly::Mbr(MbrAnomaly::NoTable {
                    lba: 0,
                    reason: NoTableReason::GptHeader
                })],
                "{}",
                m.name()
            );
        }
        // MBR desplazado (sectores añadidos al inicio): leído en su posición, sus entradas
        // coinciden con los volúmenes del manifiesto en coordenadas de la imagen.
        for a in m.table("anomalies") {
            if a.get("code") != Some(&Value::Str("mbr_displaced".into())) {
                continue;
            }
            let at = int(a, "lba").unwrap() as u64;
            assert_eq!(at as i64, -shift, "{}", m.name());
            let moved = mbr::read_mbr_at(dev.as_ref(), at).unwrap();
            assert_eq!(moved.scheme, Scheme::Mbr);
            let got: Vec<(u64, u64)> = moved
                .partitions
                .iter()
                .map(|p| (p.start_lba, p.sector_count))
                .collect();
            let want: Vec<(u64, u64)> = m
                .table("found_volumes")
                .iter()
                .map(|v| {
                    (
                        int(v, "start_lba").unwrap() as u64,
                        int(v, "sectors").unwrap() as u64,
                    )
                })
                .collect();
            assert_eq!(got, want, "{}", m.name());
            // Cada partición abierta como origen empieza por el arranque de su volumen.
            let src = mr_api::source_from_device(dev.clone(), "desplazada");
            let table = as_table(&moved, 512);
            for (i, v) in m.table("found_volumes").iter().enumerate() {
                let part = mr_api::open_partition(&src, &table, i, false).unwrap();
                let first = mr_api::read_sectors(part.as_ref(), 0, 1).unwrap();
                let fs = match v.get("fs") {
                    Some(Value::Str(f)) => f.as_str(),
                    _ => panic!(),
                };
                let oem: &[u8] = if fs == "ntfs" {
                    b"NTFS    "
                } else {
                    b"EXFAT   "
                };
                assert_eq!(&first[3..11], oem, "{}: {fs}", m.name());
            }
            checked += 1;
        }
        checked += 1;
    }
    assert_eq!(
        checked, 5,
        "imágenes desplazadas y MBR desplazados comprobados"
    );
}

/// Dispositivo que lee de una imagen compartida con algunos sectores sustituidos (mutaciones sin
/// copiar la imagen entera en cada iteración).
struct Overlay {
    base: Arc<Vec<u8>>,
    ss: u32,
    patches: HashMap<u64, Vec<u8>>,
}

impl BlockDevice for Overlay {
    fn size(&self) -> u64 {
        self.base.len() as u64
    }
    fn sector_size(&self) -> u32 {
        self.ss
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> mr_io::Result<()> {
        let end = offset as usize + buf.len();
        buf.copy_from_slice(&self.base[offset as usize..end]);
        let ss = u64::from(self.ss);
        for (&lba, data) in &self.patches {
            let (s, e) = (lba * ss, (lba + 1) * ss);
            if s < end as u64 && offset < e {
                let from = s.max(offset);
                let to = e.min(end as u64);
                buf[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&data[(from - s) as usize..(to - s) as usize]);
            }
        }
        Ok(())
    }
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn mutated_corpus_tables_never_panic_hang_or_explode() {
    let mut rng = Lcg(0x4D52_4D42_5231);
    let mut runs = 0;
    let started = Instant::now();
    for m in manifests() {
        if !matches!(m.str("scheme"), "mbr" | "hybrid" | "gpt") {
            continue;
        }
        let ss = m.int("sector_size") as u32;
        let base = Arc::new(image(&m));
        let mut tables: Vec<u64> = vec![0];
        tables.extend(m.list("ebr_lbas").iter().map(|&x| x as u64));
        let total = base.len() as u64 / u64::from(ss);
        for _ in 0..300 {
            let mut patches = HashMap::new();
            for _ in 0..(1 + rng.next() % 3) {
                let lba = tables[(rng.next() as usize) % tables.len()];
                let at = (lba * u64::from(ss)) as usize;
                let mut sector = patches
                    .remove(&lba)
                    .unwrap_or_else(|| base[at..at + ss as usize].to_vec());
                for _ in 0..(1 + rng.next() % 8) {
                    let off = 0x1BE + (rng.next() as usize) % 66;
                    sector[off] = rng.next() as u8;
                }
                // A veces, un enlace o un inicio que apunta a otra tabla del disco (bucles).
                if rng.next().is_multiple_of(3) {
                    let slot = (rng.next() % 4) as usize;
                    let e = 0x1BE + 16 * slot;
                    sector[e + 4] = [0x05, 0x0F, 0x85][(rng.next() % 3) as usize];
                    let to = tables[(rng.next() as usize) % tables.len()];
                    let rel = (to.saturating_sub(lba) as u32).wrapping_add((rng.next() % 3) as u32);
                    sector[e + 8..e + 12].copy_from_slice(&rel.to_le_bytes());
                }
                patches.insert(lba, sector);
            }
            let src = mr_api::source_from_device(
                Arc::new(Overlay {
                    base: Arc::clone(&base),
                    ss,
                    patches,
                }),
                "mutada",
            );
            let t0 = Instant::now();
            let scan = mbr::read_mbr(src.as_ref()).unwrap();
            assert!(t0.elapsed() < Duration::from_secs(5), "{}: lento", m.name());
            assert!(scan.ebr_lbas.len() <= mbr::MAX_EBR_CHAIN);
            assert!(scan.partitions.len() <= 4 + 4 * mbr::MAX_EBR_CHAIN);
            assert!(
                scan.anomalies.len() <= 16 * (1 + scan.ebr_lbas.len()) + 4 * scan.partitions.len()
            );
            let table = as_table(&scan, ss);
            for (i, p) in scan.partitions.iter().enumerate() {
                assert!(p.anomalies.len() <= 4);
                // Toda partición que empieza dentro se abre como origen, acotada al disco.
                match mr_api::open_partition(&src, &table, i, false) {
                    Ok(part) => {
                        assert!(p.start_lba < total);
                        assert!(part.info().sector_count <= total - p.start_lba);
                    }
                    Err(_) => assert!(p.start_lba >= total || p.sector_count == 0),
                }
            }
            runs += 1;
        }
    }
    assert!(runs >= 300 * 20, "{runs} ejecuciones");
    assert!(started.elapsed() < Duration::from_secs(300));
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn cli_partitions_json_follows_the_engine() {
    for m in manifests() {
        let json = cli_partitions_json(&m);
        let scheme = m.str("scheme");
        assert!(json.contains(&format!("\"scheme\":\"{scheme}\"")), "{json}");
        for (start, sectors, kind, _, _) in expected_partitions(&m) {
            assert!(
                json.contains(&format!(
                    "\"start_lba\":{start},\"sectors\":{sectors},\"end_lba\":{},\"type\":{kind},",
                    start + sectors - 1
                )),
                "{}: {json}",
                m.name()
            );
        }
        for g in expected_gpt_partitions(&m) {
            assert!(
                json.contains(&format!(
                    "\"start_lba\":{},\"sectors\":{},\"end_lba\":{},\"type\":\"{}\",",
                    g.start,
                    g.sectors,
                    g.start + g.sectors - 1,
                    g.type_guid
                )) && json.contains(&format!("\"name\":\"{}\"", g.name)),
                "{}: {json}",
                m.name()
            );
        }
        for (i, r) in expected_apm_partitions(&m).iter().enumerate() {
            assert!(
                json.contains(&format!(
                    "{{\"number\":{},\"origin\":\"apm\",\"slot\":{},\"ebr_lba\":null,\"start_lba\":{},\"sectors\":{},\"end_lba\":{},\"type\":\"{}\",",
                    i + 1,
                    r.2,
                    r.0,
                    r.1,
                    r.0 + r.1 - 1,
                    r.3
                )) && json.contains(&format!("\"name\":\"{}\",\"status\":{},", r.4, r.5)),
                "{}: {json}",
                m.name()
            );
        }
        for (code, lba, _, _) in expected_table_anomalies(&m) {
            let alt = match code.as_str() {
                "mbr_outside_disk" => "mbr_partially_outside_disk",
                "gpt_primary_header_crc" => "gpt_primary_header_size",
                "gpt_backup_header_crc" => "gpt_backup_header_size",
                c => c,
            };
            assert!(
                json.contains(&format!("\"code\":\"{code}\""))
                    || json.contains(&format!("\"code\":\"{alt}\",\"severity\"")),
                "{}: falta {code} (LBA {lba})\n{json}",
                m.name()
            );
        }
    }
}

fn cli_partitions_json(m: &Manifest) -> String {
    let p = corpus().join("generated").join(m.str("file"));
    let ss = m.int("sector_size").to_string();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
        .args([
            "partitions",
            p.to_str().unwrap(),
            "--sector-size",
            &ss,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn cli_json_for_gpt_basic_and_damaged_primary() {
    let all = manifests();
    let find = |n: &str| all.iter().find(|m| m.name() == n).unwrap();
    let json = cli_partitions_json(find("gpt_basic"));
    assert!(
        json.starts_with(
            "{\"scheme\":\"gpt\",\"sector_size\":512,\"confidence\":\"valid\",\"gpt\":{\"selected\":\"primary\",\"shift\":0,\"trailing_sectors_missing\":0,\"sector_size\":512,\"disk_guid\":\"3735644a-4f8f-56fa-b75e-d808ec421dbb\"}"
        ),
        "{json}"
    );
    assert!(json.contains("{\"number\":3,\"origin\":\"gpt\",\"slot\":2,\"ebr_lba\":null,\"start_lba\":22528,\"sectors\":2048,\"end_lba\":24575,\"type\":\"c12a7328-f81f-11d2-ba4b-00a0c93ec93b\",\"type_name\":\"Sistema EFI\",\"active\":false,\"confidence\":\"valid\",\"name\":\"MR-ESP\",\"unique_guid\":\"78fb8b9c-9b5e-5bf0-a278-d37bf0ab1c42\",\"attributes\":0,\"anomalies\":[]}"), "{json}");
    assert!(json.ends_with("\"anomalies\":[]}\n"), "{json}");
    let json = cli_partitions_json(find("gpt_primary_header_bad"));
    assert!(
        json.starts_with("{\"scheme\":\"gpt\",\"sector_size\":512,\"confidence\":\"valid_with_fixes\",\"gpt\":{\"selected\":\"backup\",\"shift\":0,"),
        "{json}"
    );
    assert!(
        json.contains(
            "\"code\":\"gpt_primary_header_crc\",\"severity\":\"compensated\",\"lba\":1,"
        ),
        "{json}"
    );
    assert!(json.contains("\"code\":\"gpt_backup_in_use\""), "{json}");
    assert!(json.contains("\"name\":\"MR-LINUX\""), "{json}");
    // Imagen desplazada: el desplazamiento con signo y la apertura solo con --apply-shift.
    let shifted = find("gpt_shifted_start");
    let json = cli_partitions_json(shifted);
    assert!(
        json.contains("\"selected\":\"backup\",\"shift\":63,"),
        "{json}"
    );
    assert!(
        json.contains("\"code\":\"image_shifted\",\"severity\":\"suspicious\",\"lba\":null,\"target_lba\":null,\"value\":63,"),
        "{json}"
    );
    let img = corpus().join("generated").join(shifted.str("file"));
    let run = |extra: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
            .args([
                "sectors",
                img.to_str().unwrap(),
                "--partition",
                "1",
                "--lba",
                "0",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("desplazada 63 sectores"));
    let o = run(&["--apply-shift"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("desplazamiento aplicado: 63 sectores"));
    assert!(String::from_utf8_lossy(&o.stdout).contains("MEMRESCUE-TEST|g"));
}

// ---------------------------------------------------------------------------------------------
// Tabla completa (MBR + GPT) frente al manifiesto
// ---------------------------------------------------------------------------------------------

/// Partición GPT esperada (LBA del disco).
#[derive(Debug, PartialEq)]
struct GptRow {
    start: u64,
    sectors: u64,
    type_guid: String,
    name: String,
    unique: String,
}

fn expected_gpt_partitions(m: &Manifest) -> Vec<GptRow> {
    if !matches!(m.str("scheme"), "gpt" | "hybrid") {
        return Vec::new();
    }
    let s = |t: &Table, k: &str| match t.get(k) {
        Some(Value::Str(v)) => v.clone(),
        o => panic!("{k}: {o:?}"),
    };
    m.table("partitions")
        .iter()
        .map(|t| GptRow {
            start: int(t, "start_lba").unwrap() as u64,
            sectors: int(t, "sectors").unwrap() as u64,
            type_guid: s(t, "gpt_type"),
            name: s(t, "gpt_name"),
            unique: s(t, "unique_guid"),
        })
        .collect()
}

/// Códigos del manifiesto que produce la tabla integrada (MBR y GPT), normalizados. Se excluyen
/// `mbr_displaced` (búsqueda #15) e `image_shifted` en las imágenes sin GPT (lo deduce la búsqueda
/// de volúmenes, no la tabla).
fn expected_table_anomalies(m: &Manifest) -> Vec<Norm> {
    let gpt = matches!(m.str("scheme"), "gpt" | "hybrid");
    let mut v: Vec<Norm> = m
        .table("anomalies")
        .iter()
        .filter_map(|a| {
            let Some(Value::Str(code)) = a.get("code") else {
                panic!()
            };
            if code == "mbr_displaced" || (code == "image_shifted" && !gpt) {
                return None;
            }
            let value = (VALUE_CODES.contains(&code.as_str()) || code == "image_shifted")
                .then(|| int(a, "value"))
                .flatten();
            Some((
                code.clone(),
                int(a, "lba").unwrap(),
                int(a, "target_lba"),
                value,
            ))
        })
        .collect();
    v.sort();
    v
}

/// Normaliza una anomalía del motor al código y coordenadas del manifiesto (`None`: sin código en
/// el manifiesto, como `gpt_backup_in_use` o `gpt_image_truncated`; ver `docs/formatos/gpt.md`).
fn table_norm(a: &Anomaly, shift: i64) -> Option<Norm> {
    use mr_api::mr_part::gpt::GptAnomaly as G;
    let (code, lba, target, value): (String, i64, Option<i64>, Option<i64>) = match a {
        Anomaly::Gpt(g) => match g {
            G::HeaderSize { .. } | G::HeaderCrc { .. } => (
                a.code().replace("_size", "_crc"),
                a.lba()? as i64,
                None,
                None,
            ),
            G::HeaderMissing { .. } | G::EntriesCrc { .. } | G::UsableRangeOutsideImage { .. } => {
                (a.code().to_string(), a.lba()? as i64, None, None)
            }
            G::HeaderOutsideImage { disk_lba, .. } | G::EntriesOutsideImage { disk_lba, .. } => {
                (a.code().to_string(), *disk_lba as i64 - shift, None, None)
            }
            G::ImageShifted { offset } => ("image_shifted".into(), -offset, None, Some(*offset)),
            _ => return None,
        },
        Anomaly::Mbr(_) => {
            let code = manifest_code(a)?;
            let value = VALUE_CODES
                .contains(&code)
                .then(|| a.value().map(|x| x as i64))
                .flatten();
            (
                code.to_string(),
                a.lba()? as i64,
                a.target().map(|x| x as i64),
                value,
            )
        }
        Anomaly::UnreadableSector { lba } => ("unreadable_sector".into(), *lba as i64, None, None),
        Anomaly::Apm(_) => (a.code().to_string(), a.lba()? as i64, None, None),
    };
    Some((code, lba, target, value))
}

#[test]
#[ignore = "requiere el corpus generado; lo ejecuta scripts/test.ps1"]
fn partition_table_matches_every_manifest() {
    let (mut checked, mut gpt_parts, mut opened, mut anomalies, mut apm_parts) = (0, 0, 0, 0, 0);
    for m in manifests() {
        let name = m.name().to_string();
        let dev = Arc::new(device(&m));
        let t = mr_api::partitions(dev.as_ref()).unwrap();
        let scheme = match m.str("scheme") {
            "mbr" => Scheme::Mbr,
            "gpt" => Scheme::Gpt,
            "hybrid" => Scheme::Hybrid,
            "apm" => Scheme::Apm,
            _ => Scheme::None,
        };
        assert_eq!(t.scheme, scheme, "{name}: esquema");
        // Particiones MBR (o entradas del híbrido) y después GPT, en ese orden.
        let mbr_rows: Vec<(u64, u64, u8, bool, bool)> = t
            .partitions
            .iter()
            .filter_map(|p| match p.kind {
                PartitionKind::Mbr(k) => Some((
                    p.start_lba,
                    p.sector_count,
                    k,
                    p.active,
                    matches!(p.origin, Origin::Logical { .. }),
                )),
                PartitionKind::Gpt(_) | PartitionKind::Apm(_) => None,
            })
            .collect();
        assert_eq!(mbr_rows, expected_partitions(&m), "{name}: particiones MBR");
        apm_parts += check_apm(&m, &t, dev.clone());
        let gpt_rows: Vec<GptRow> = t
            .partitions
            .iter()
            .filter(|p| matches!(p.origin, Origin::Gpt { .. }))
            .map(|p| {
                let PartitionKind::Gpt(g) = p.kind else {
                    panic!()
                };
                GptRow {
                    start: p.start_lba,
                    sectors: p.sector_count,
                    type_guid: mr_api::mr_part::gpt::format_guid(&g),
                    name: p.name.clone().unwrap(),
                    unique: mr_api::mr_part::gpt::format_guid(&t.gpt_entry(p).unwrap().unique_guid),
                }
            })
            .collect();
        let want_gpt = expected_gpt_partitions(&m);
        assert_eq!(gpt_rows, want_gpt, "{name}: particiones GPT");
        let mbr_count = mbr_rows.len();
        assert!(
            t.partitions[..mbr_count]
                .iter()
                .all(|p| !matches!(p.origin, Origin::Gpt { .. })),
            "{name}: las entradas MBR van primero"
        );
        // Copia elegida y desplazamiento.
        if let Some(Value::Str(sel)) = m.top.get("gpt_selected") {
            let g = t.gpt.as_ref().unwrap_or_else(|| panic!("{name}: sin GPT"));
            let got = match g.selected {
                Some(mr_api::mr_part::gpt::GptCopy::Primary) => "primary",
                Some(mr_api::mr_part::gpt::GptCopy::Backup) => "backup",
                None => "none",
            };
            assert_eq!(got, sel, "{name}: copia elegida");
            assert_eq!(
                t.shift(),
                m.int("leading_sectors_missing"),
                "{name}: desplazamiento"
            );
        } else {
            assert!(t.gpt.is_none(), "{name}: GPT inesperada");
        }
        // Conjunto exacto de anomalías con código en el manifiesto (MBR y GPT unidas).
        let mut got: Vec<Norm> = t
            .all_anomalies()
            .filter_map(|a| table_norm(a, t.shift()))
            .collect();
        got.sort();
        let want = expected_table_anomalies(&m);
        assert_eq!(got, want, "{name}: anomalías de la tabla");
        anomalies += want.len();
        // Toda partición GPT se abre (con el desplazamiento solo si se pide) y empieza por su marcador.
        let src = mr_api::source_from_device(dev.clone(), "corpus");
        let markers: Vec<String> = m
            .table("partitions")
            .iter()
            .map(|p| match p.get("marker") {
                Some(Value::Str(s)) => s.clone(),
                _ => panic!(),
            })
            .collect();
        for (i, p) in t.partitions.iter().enumerate() {
            let Origin::Gpt { .. } = p.origin else {
                continue;
            };
            let k = i - mbr_count;
            let shifted = t.shift() != 0;
            if shifted {
                assert!(
                    matches!(
                        mr_api::open_partition(&src, &t, i, false),
                        Err(mr_api::ApiError::InvalidArgument(_))
                    ),
                    "{name}: no se aplica el desplazamiento en silencio"
                );
            }
            let part = mr_api::open_partition(&src, &t, i, shifted).unwrap();
            if shifted {
                assert!(part.info().description.contains("desplazamiento aplicado"));
            }
            let first = mr_api::read_sectors(part.as_ref(), 0, 1).unwrap();
            assert_eq!(
                &first[..markers[k].len()],
                markers[k].as_bytes(),
                "{name}: marcador de la partición GPT {k}"
            );
            opened += 1;
        }
        gpt_parts += want_gpt.len();
        checked += 1;
    }
    assert_eq!(
        (checked, gpt_parts, opened, anomalies, apm_parts),
        (41, 21, 21, 47, 11),
        "manifiestos, particiones GPT, particiones GPT abiertas, anomalías comparadas y particiones APM"
    );
}

/// Partición APM esperada: `(inicio, bloques, entrada, tipo, nombre, estado, marcador)`.
type ApmRow = (u64, u64, u32, String, String, u32, String);

fn expected_apm_partitions(m: &Manifest) -> Vec<ApmRow> {
    if m.str("scheme") != "apm" {
        return Vec::new();
    }
    let s = |t: &Table, k: &str| match t.get(k) {
        Some(Value::Str(v)) => v.clone(),
        o => panic!("{k}: {o:?}"),
    };
    m.table("partitions")
        .iter()
        .map(|t| {
            (
                int(t, "start_lba").unwrap() as u64,
                int(t, "sectors").unwrap() as u64,
                int(t, "apm_index").unwrap() as u32,
                s(t, "apm_type"),
                s(t, "apm_name"),
                int(t, "apm_status").unwrap() as u32,
                s(t, "marker"),
            )
        })
        .collect()
}

/// Particiones APM de la tabla frente al manifiesto (entrada, tipo, nombre y estado); las que
/// empiezan dentro se abren y empiezan por su marcador. Devuelve cuántas se comprobaron.
fn check_apm(m: &Manifest, t: &mr_api::PartitionTable, dev: Arc<MemDevice>) -> usize {
    let name = m.name();
    let want = expected_apm_partitions(m);
    if want.is_empty() {
        assert!(t.apm.is_none(), "{name}: APM inesperado");
        return 0;
    }
    let a = t.apm.as_ref().unwrap_or_else(|| panic!("{name}: sin APM"));
    assert_eq!(
        (
            u64::from(a.block_size),
            a.disk_blocks,
            u64::from(a.map_entries)
        ),
        (
            m.int("apm_block_size") as u64,
            m.int("apm_disk_blocks") as u64,
            m.int("apm_map_entries") as u64
        ),
        "{name}: mapa APM"
    );
    assert_eq!(
        t.sector_size, a.block_size,
        "{name}: LBA en bloques del mapa"
    );
    let got: Vec<ApmRow> = t
        .partitions
        .iter()
        .map(|p| {
            let (Origin::Apm { index }, PartitionKind::Apm(kind)) = (p.origin, p.kind) else {
                panic!("{name}: partición no APM {p:?}")
            };
            (
                p.start_lba,
                p.sector_count,
                index,
                kind.text(),
                p.name.clone().unwrap(),
                t.apm_entry(p).unwrap().status,
                String::new(),
            )
        })
        .collect();
    let strip = |v: &[ApmRow]| -> Vec<ApmRow> {
        v.iter()
            .map(|r| (r.0, r.1, r.2, r.3.clone(), r.4.clone(), r.5, String::new()))
            .collect()
    };
    assert_eq!(strip(&got), strip(&want), "{name}: particiones APM");
    let src = mr_api::source_from_device(dev.clone(), "corpus");
    let total = dev.sector_count();
    for (i, row) in want.iter().enumerate() {
        let opened = mr_api::open_partition(&src, t, i, false);
        if row.0 >= total {
            assert!(opened.is_err(), "{name}: partición que empieza fuera");
            continue;
        }
        let part = opened.unwrap();
        assert!(part.info().description.contains("APM "), "{name}");
        let first = mr_api::read_sectors(part.as_ref(), 0, 1).unwrap();
        assert_eq!(
            &first[..row.6.len()],
            row.6.as_bytes(),
            "{name}: marcador APM {i}"
        );
    }
    want.len()
}

/// Tabla construida a partir de un recorrido MBR (para abrir sus particiones en las pruebas).
fn as_table(scan: &mbr::MbrScan, sector_size: u32) -> mr_api::PartitionTable {
    mr_api::PartitionTable {
        scheme: scan.scheme,
        sector_size,
        partitions: scan.partitions.clone(),
        anomalies: scan.anomalies.clone(),
        gpt: None,
        apm: None,
    }
}
