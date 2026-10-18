//! Búsqueda exprés de particiones perdidas (#15) contra el corpus sintético (`tests/corpus`,
//! esquema del manifiesto en `docs/PRUEBAS.md` §1.1; plan de pruebas en INV-05b §9).
//!
//! Requieren las imágenes generadas (`python -I -X utf8 tests/generators/gen_partition_images.py
//! tests/corpus/generated`; `MR_CORPUS_DIR` para otra ubicación). Se marcan `#[ignore]` y se
//! ejecutan con `cargo test -p mr-part --test search_corpus -- --ignored`.
//!
//! Todas las búsquedas pasan por un envoltorio que **cuenta y registra** las lecturas del origen,
//! para comprobar el presupuesto, el determinismo y que nada se lee después de cancelar.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use mr_io::{BlockDevice, FaultyDevice, ImageFile};
use mr_part::search::{
    search, Check, FoundVolume, NoObserver, SearchObserver, SearchOptions, SearchProgress,
    SearchResult, SearchStatus, VolumeAnomaly,
};
use mr_part::{read_partition_table, Confidence};

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
        self.get(k).unwrap().parse().unwrap()
    }

    fn table(&self, name: &str) -> Vec<&HashMap<String, String>> {
        self.tables
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, t)| t)
            .collect()
    }

    fn name(&self) -> &str {
        self.get("name").unwrap()
    }

    fn list(&self, k: &str) -> Vec<u64> {
        self.get(k)
            .map(|v| {
                v.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().parse().unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `true` si el escenario tiene GPT (el desplazamiento lo informa entonces el lector GPT).
    fn has_gpt(&self) -> bool {
        self.top.contains_key("gpt_primary_header")
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

fn image(m: &Manifest, sector_size: u32) -> ImageFile {
    let p = generated().join(m.get("file").unwrap());
    assert!(
        p.exists(),
        "falta {}: genere el corpus con tests/generators/gen_partition_images.py",
        p.display()
    );
    ImageFile::open(&p, sector_size).unwrap()
}

/// Envoltorio que registra cada `read_at` (desplazamiento y longitud).
struct Counting<D: BlockDevice> {
    inner: D,
    log: Mutex<Vec<(u64, usize)>>,
}

impl<D: BlockDevice> Counting<D> {
    fn new(inner: D) -> Self {
        Self {
            inner,
            log: Mutex::new(Vec::new()),
        }
    }

    fn log(&self) -> Vec<(u64, usize)> {
        self.log.lock().unwrap().clone()
    }

    fn reset(&self) {
        self.log.lock().unwrap().clear();
    }
}

impl<D: BlockDevice> BlockDevice for Counting<D> {
    fn size(&self) -> u64 {
        self.inner.size()
    }
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> mr_io::Result<()> {
        self.log.lock().unwrap().push((offset, buf.len()));
        self.inner.read_at(offset, buf)
    }
    fn allocation(&self, lba: u64, count: u64) -> mr_io::Result<Vec<mr_io::AllocationRun>> {
        self.inner.allocation(lba, count)
    }
}

/// Origen del escenario: la imagen con los sectores de `unreadable_lbas` ilegibles.
fn source(m: &Manifest, sector_size: u32) -> Counting<FaultyDevice<ImageFile>> {
    Counting::new(FaultyDevice::new(image(m, sector_size)).with_bad(m.list("unreadable_lbas")))
}

/// Tabla y búsqueda; devuelve el resultado y las lecturas hechas por la búsqueda.
fn run(
    dev: &Counting<impl BlockDevice>,
    opts: &SearchOptions,
    observer: &dyn SearchObserver,
) -> (SearchResult, Vec<(u64, usize)>) {
    let table = read_partition_table(dev).unwrap();
    dev.reset();
    let r = search(dev, Some(&table), opts, observer).unwrap();
    (r, dev.log())
}

fn state(c: Check) -> &'static str {
    match c {
        Check::Yes => "valid",
        Check::No => "damaged",
        Check::Unverifiable => "unreadable",
        Check::Outside => "outside_image",
        Check::NotApplicable => "not_applicable",
    }
}

fn serial(v: &FoundVolume) -> String {
    format!("{:0width$x}", v.serial, width = v.fs.serial_digits())
}

/// Compara un volumen con su `[[found_volumes]]`.
fn check_volume(name: &str, ss: u32, v: &FoundVolume, e: &HashMap<String, String>) {
    let int = |k: &str| e[k].parse::<i64>().unwrap();
    let ctx = format!("{name}: {v:#?}");
    assert_eq!(v.fs.code(), e["fs"], "{ctx}");
    assert_eq!(v.start_lba(ss), int("start_lba"), "{ctx}");
    assert_eq!(v.sector_count(ss) as i64, int("sectors"), "{ctx}");
    assert_eq!(
        v.start_lba(ss) + v.sector_count(ss) as i64 - 1,
        int("end_lba"),
        "{ctx}"
    );
    assert_eq!(v.declared_start as i64, int("hidden_sectors"), "{ctx}");
    assert_eq!(v.primary_byte / i64::from(ss), int("boot_lba"), "{ctx}");
    assert_eq!(
        v.backup_byte.map(|b| b / i64::from(ss)),
        Some(int("backup_boot_lba")),
        "{ctx}"
    );
    assert_eq!(state(v.primary), e["primary_boot"], "{ctx}");
    assert_eq!(state(v.backup), e["backup_boot"], "{ctx}");
    let evidence: Vec<String> = e["evidence"]
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(v.evidence(), evidence, "{ctx}");
    assert_eq!(v.partial().to_string(), e["partial"], "{ctx}");
    assert_eq!(serial(v), e["serial"], "{ctx}");
    assert_eq!(
        i64::from(v.fs.suggested_mbr_type()),
        int("suggested_mbr_type"),
        "{ctx}"
    );
}

/// `[[anomalies]]` del manifiesto que produce la búsqueda: `image_shifted` (en imágenes sin GPT;
/// con GPT lo informa el lector GPT) y `mbr_displaced`.
fn expected_search_anomalies(m: &Manifest) -> BTreeSet<(String, i64, Option<i64>)> {
    m.table("anomalies")
        .into_iter()
        .filter(|a| match a["code"].as_str() {
            "image_shifted" => !m.has_gpt(),
            "mbr_displaced" => true,
            _ => false,
        })
        .map(|a| {
            (
                a["code"].clone(),
                a["lba"].parse().unwrap(),
                a.get("value").map(|v| v.parse().unwrap()),
            )
        })
        .collect()
}

fn search_anomalies(r: &SearchResult) -> BTreeSet<(String, i64, Option<i64>)> {
    r.anomalies
        .iter()
        .map(|a| (a.code().to_string(), a.lba(), a.value()))
        .collect()
}

/// INV-05b §9, filas de volúmenes perdidos, copias dañadas o ilegibles, imágenes desplazadas,
/// 4Kn, superfloppy, tablas intactas y negativos: los `[[found_volumes]]` de **todos** los
/// manifiestos (y nada más), con sus estados, evidencia, coordenadas y anomalías de la búsqueda.
#[test]
#[ignore = "requiere el corpus generado"]
fn found_volumes_match_every_manifest() {
    let mut volumes = 0;
    for m in manifests() {
        let ss = m.num("sector_size") as u32;
        let dev = source(&m, ss);
        let (r, log) = run(&dev, &SearchOptions::default(), &NoObserver);
        let name = m.name();
        assert_eq!(r.status, SearchStatus::Complete, "{name}");
        assert!(
            r.stats.reads <= SearchOptions::default().max_reads,
            "{name}"
        );
        if m.list("unreadable_lbas").is_empty() {
            // Sin sectores ilegibles, cada lectura física es una lectura del origen (con ellos, el
            // lector tolerante de M1 reintenta sector a sector).
            assert_eq!(log.len() as u64, r.stats.reads, "{name}");
        }
        let expected = m.table("found_volumes");
        assert_eq!(r.volumes.len(), expected.len(), "{name}: {:#?}", r.volumes);
        for (v, e) in r.volumes.iter().zip(&expected) {
            check_volume(name, ss, v, e);
            // Confianza (ADR-003): válida solo con principal y copia coherentes y sin desplazamiento.
            let clean = e["primary_boot"] == "valid"
                && e["backup_boot"] == "valid"
                && m.num("leading_sectors_missing") == 0;
            assert_eq!(v.confidence == Confidence::Valid, clean, "{name}: {v:#?}");
            assert!(
                !v.listed(),
                "{name}: ninguna tabla del corpus describe un FS"
            );
            volumes += 1;
        }
        assert_eq!(
            search_anomalies(&r),
            expected_search_anomalies(&m),
            "{name}: {:?}",
            r.anomalies
        );
        // Los señuelos (sectores con aspecto de MBR dentro de datos) nunca son tablas.
        let decoys = m.list("decoy_lbas");
        assert!(
            r.tables.iter().all(|t| !decoys.contains(&t.lba)),
            "{name}: {:?}",
            r.tables
        );
        if !name.starts_with("image_shifted_pos") {
            assert!(r.tables.is_empty(), "{name}: {:?}", r.tables);
        }
    }
    assert_eq!(volumes, 18, "volúmenes del corpus");
}

/// Escenarios concretos de INV-05b §9 con su detalle: nivel que encuentra cada volumen,
/// indicadores y anomalías.
#[test]
#[ignore = "requiere el corpus generado"]
fn scenario_details() {
    let all: HashMap<String, Manifest> = manifests()
        .into_iter()
        .map(|m| (m.name().to_string(), m))
        .collect();
    let run_one = |name: &str| {
        let m = &all[name];
        let dev = source(m, m.num("sector_size") as u32);
        run(&dev, &SearchOptions::default(), &NoObserver).0
    };
    let has = |v: &FoundVolume, code: &str| v.anomalies.iter().any(|a| a.code() == code);

    // lost_ntfs_mbr_zeroed: rejilla de 1 MiB (P2), principal y copia.
    let r = run_one("lost_ntfs_mbr_zeroed");
    assert_eq!(r.volumes[0].tier.code(), "p2");
    assert_eq!(r.volumes[0].indicators(), "Ex P+ C+ Sx U+ A+");
    // lost_fat32_chs: alineación CHS (LBA 63) y estructuras (FAT) válidas.
    let r = run_one("lost_fat32_chs");
    assert_eq!(r.volumes[0].start_byte, 63 * 512);
    assert_eq!(
        (r.volumes[0].aligned, r.volumes[0].structures),
        (Check::Yes, Check::Yes)
    );
    // lost_exfat_1mib: suma de la región de arranque y FAT.
    let r = run_one("lost_exfat_1mib");
    assert_eq!(r.volumes[0].indicators(), "Ex P+ C+ S+ U+ A+");
    // lost_chain_three: el tercero (sin alinear) por encadenamiento P1, no por rejilla.
    let r = run_one("lost_chain_three");
    assert_eq!(r.volumes[2].tier.code(), "p1", "{:#?}", r.volumes[2]);
    assert_eq!(r.volumes[2].aligned, Check::No);
    // Principales dañadas: sospechosos restaurables desde la copia.
    for name in [
        "ntfs_primary_damaged",
        "fat32_primary_damaged",
        "exfat_primary_damaged",
    ] {
        let r = run_one(name);
        let v = &r.volumes[0];
        assert!(has(v, "found_primary_damaged"), "{name}: {v:#?}");
        assert!(matches!(v.confidence, Confidence::Suspicious(_)), "{name}");
    }
    // NTFS con principal dañada: encontrado desde la copia al final (P4).
    assert_eq!(run_one("ntfs_primary_damaged").volumes[0].tier.code(), "p4");
    // Ilegibles: «?» y nunca el relleno.
    let r = run_one("primary_unreadable");
    let v = &r.volumes[0];
    assert_eq!(v.primary, Check::Unverifiable);
    assert!(v
        .anomalies
        .contains(&VolumeAnomaly::PrimaryUnreadable { lba: 2048 }));
    assert!(v.indicators().contains("P?"));
    let r = run_one("backup_unreadable");
    let v = &r.volumes[0];
    assert_eq!(v.backup, Check::Unverifiable);
    assert!(has(v, "found_backup_unreadable"));
    assert!(matches!(v.confidence, Confidence::Suspicious(_)));
    // Imágenes desplazadas: hipótesis con dos evidencias, comunicada, y apertura solo explícita.
    let r = run_one("image_shifted_pos");
    assert_eq!(r.shift_bytes, Some(8 * 512));
    assert!(r
        .volumes
        .iter()
        .all(|v| v.requires_shift() && v.entry == Check::Yes));
    assert_eq!(r.tables.len(), 1);
    assert_eq!(r.tables[0].lba, 8);
    let r = run_one("image_shifted_neg");
    assert_eq!(r.shift_bytes, Some(-2100 * 512));
    assert!(r.volumes[0].partial() && has(&r.volumes[0], "found_primary_outside_image"));
    // 4Kn: sectores de 4096 B del FS iguales a los del dispositivo.
    let r = run_one("gpt_4kn_lost");
    assert!(r.volumes.iter().all(|v| v.fs_sector_size == 4096));
    // Superfloppy: en el byte 0 (P0).
    let r = run_one("superfloppy");
    assert_eq!(
        (r.volumes[0].start_byte, r.volumes[0].tier.code()),
        (0, "p0")
    );
}

/// 4Kn abierto como 512: mismo volumen (en bytes) con la anomalía de tamaño de sector.
#[test]
#[ignore = "requiere el corpus generado"]
fn gpt_4kn_lost_read_as_512_reports_sector_size() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "gpt_4kn_lost")
        .unwrap();
    let dev = source(&m, 512);
    let (r, _) = run(&dev, &SearchOptions::default(), &NoObserver);
    assert_eq!(r.volumes.len(), 2, "{:#?}", r.volumes);
    for (v, e) in r.volumes.iter().zip(m.table("found_volumes")) {
        assert_eq!(v.start_byte, e["start_lba"].parse::<i64>().unwrap() * 4096);
        assert_eq!(v.fs_sector_size, 4096);
        assert!(v.anomalies.contains(&VolumeAnomaly::SectorSizeMismatch {
            fs: 4096,
            device: 512
        }));
        assert!(matches!(v.confidence, Confidence::Suspicious(_)));
    }
}

/// `unreadable_lbas` y relleno que imita la firma: con **todas** las estructuras de arranque de
/// los volúmenes del corpus ilegibles no aparece ningún volumen (el relleno nunca es evidencia).
#[test]
#[ignore = "requiere el corpus generado"]
fn unreadable_boot_structures_never_yield_volumes() {
    for m in manifests() {
        let found = m.table("found_volumes");
        if found.is_empty() {
            continue;
        }
        let ss = m.num("sector_size") as u32;
        let mut bad: Vec<u64> = m.list("unreadable_lbas");
        for v in &found {
            for k in ["boot_lba", "backup_boot_lba"] {
                if let Ok(l) = v[k].parse::<u64>() {
                    bad.push(l);
                }
            }
        }
        let dev = Counting::new(FaultyDevice::new(image(&m, ss)).with_bad(bad.clone()));
        let (r, _) = run(&dev, &SearchOptions::default(), &NoObserver);
        let is_bad = |byte: i64| bad.contains(&((byte / i64::from(ss)) as u64));
        for v in &r.volumes {
            assert!(
                !(is_bad(v.primary_byte) && v.primary == Check::Yes),
                "{}: {v:#?}",
                m.name()
            );
            assert!(
                !(v.backup_byte.is_some_and(is_bad) && v.backup == Check::Yes),
                "{}: {v:#?}",
                m.name()
            );
        }
        // Ningún volumen del manifiesto puede aparecer sin sus estructuras.
        for e in &found {
            let start: i64 = e["start_lba"].parse().unwrap();
            assert!(
                r.volumes.iter().all(|v| v.start_lba(ss) != start),
                "{}: {:#?}",
                m.name(),
                r.volumes
            );
        }
    }
}

/// Determinismo: dos ejecuciones leen lo mismo en el mismo orden y dan el mismo resultado.
#[test]
#[ignore = "requiere el corpus generado"]
fn search_is_deterministic() {
    for m in manifests() {
        let ss = m.num("sector_size") as u32;
        let a = run(&source(&m, ss), &SearchOptions::default(), &NoObserver);
        let b = run(&source(&m, ss), &SearchOptions::default(), &NoObserver);
        assert_eq!(a.0, b.0, "{}", m.name());
        assert_eq!(a.1, b.1, "{}", m.name());
    }
}

/// Presupuesto: con `max_reads` pequeño el resultado es parcial, determinista y nunca supera el
/// límite (ni en lecturas de la búsqueda ni en lecturas del origen).
#[test]
#[ignore = "requiere el corpus generado"]
fn small_budget_is_respected_and_reported() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "lost_chain_three")
        .unwrap();
    let full = run(&source(&m, 512), &SearchOptions::default(), &NoObserver).0;
    assert!(full.stats.reads > 4, "{}", full.stats.reads);
    for max in [1u64, 2, 3, full.stats.reads / 2, full.stats.reads - 1] {
        let opts = SearchOptions {
            max_reads: max,
            ..SearchOptions::default()
        };
        let (r, log) = run(&source(&m, 512), &opts, &NoObserver);
        assert_eq!(r.status, SearchStatus::BudgetExhausted, "{max}");
        assert!(r.stats.reads <= max, "{max}: {}", r.stats.reads);
        assert!(log.len() as u64 <= max * 2, "{max}: {}", log.len());
        assert!(r.pending.iter().sum::<u64>() > 0, "{max}");
        let (r2, log2) = run(&source(&m, 512), &opts, &NoObserver);
        assert_eq!((r, log), (r2, log2), "{max}");
    }
    // Con presupuesto de bytes.
    let opts = SearchOptions {
        max_bytes: 16 * 1024,
        ..SearchOptions::default()
    };
    let (r, _) = run(&source(&m, 512), &opts, &NoObserver);
    assert_eq!(r.status, SearchStatus::BudgetExhausted);
    assert!(r.stats.bytes <= 16 * 1024);
}

struct CancelAfter {
    after: u64,
    seen: AtomicU64,
}

impl SearchObserver for CancelAfter {
    fn cancelled(&self) -> bool {
        self.seen.load(Ordering::Relaxed) >= self.after
    }
    fn progress(&self, p: &SearchProgress) {
        self.seen.store(p.reads, Ordering::Relaxed);
    }
}

/// Cancelación tras la k-ésima lectura: estado `Cancelled`, resultado parcial y ninguna lectura
/// posterior.
#[test]
#[ignore = "requiere el corpus generado"]
fn cancellation_stops_before_the_next_read() {
    let m = manifests()
        .into_iter()
        .find(|m| m.name() == "lost_chain_three")
        .unwrap();
    let full = run(&source(&m, 512), &SearchOptions::default(), &NoObserver).0;
    for k in [0u64, 1, 3, 7, 12] {
        let obs = CancelAfter {
            after: k,
            seen: AtomicU64::new(0),
        };
        let (r, _) = run(&source(&m, 512), &SearchOptions::default(), &obs);
        assert_eq!(r.status, SearchStatus::Cancelled, "{k}");
        assert_eq!(r.stats.reads, k, "{k}");
        assert!(r.volumes.len() <= full.volumes.len());
    }
}
