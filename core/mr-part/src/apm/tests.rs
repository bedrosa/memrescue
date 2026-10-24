//! Pruebas unitarias por regla con mapas APM sintéticos construidos en memoria según la
//! disposición pública de Apple.

use super::*;
use crate::{open_coordinates, read_partition_table, Scheme};
use mr_io::{FaultyDevice, MemDevice};

/// Entrada del mapa: `(inicio, bloques, tipo, nombre, estado)`.
type Entry = (u32, u32, &'static str, &'static str, u32);

const MAP: Entry = (1, 63, "Apple_partition_map", "Apple", 0x37);
const HFS: Entry = (64, 64, "Apple_HFS", "MR-HFS", 0x4000_0033);
const FREE: Entry = (128, 64, "Apple_Free", "Extra", 0);
const DATA: Entry = (192, 64, "Apple_HFS", "MR-DATA", 0x8000_0033);

/// Disco APM de `blocks` bloques de `bs` bytes con DDM (si `ddm`) y las entradas dadas; el primer
/// byte de cada bloque de datos vale su número de bloque (mod 256).
struct Apm {
    bs: usize,
    data: Vec<u8>,
}

impl Apm {
    fn new(bs: usize, blocks: usize, ddm: bool, entries: &[Entry]) -> Self {
        let mut data = vec![0u8; bs * blocks];
        for b in 1..blocks {
            data[b * bs] = b as u8;
        }
        let mut d = Self { bs, data };
        if ddm {
            d.put(0, 0, b"ER");
            d.put(0, 2, &(bs as u16).to_be_bytes());
            d.put(0, 4, &(blocks as u32).to_be_bytes());
        }
        for (i, e) in entries.iter().enumerate() {
            d.entry(i + 1, entries.len() as u32, e);
        }
        d
    }

    fn put(&mut self, block: usize, offset: usize, bytes: &[u8]) {
        let at = block * self.bs + offset;
        self.data[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn entry(&mut self, block: usize, count: u32, e: &Entry) {
        let (start, size, kind, name, status) = *e;
        let mut r = vec![0u8; 512];
        r[..2].copy_from_slice(b"PM");
        r[4..8].copy_from_slice(&count.to_be_bytes());
        r[8..12].copy_from_slice(&start.to_be_bytes());
        r[12..16].copy_from_slice(&size.to_be_bytes());
        r[16..16 + name.len()].copy_from_slice(name.as_bytes());
        r[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        r[84..88].copy_from_slice(&size.to_be_bytes());
        r[88..92].copy_from_slice(&status.to_be_bytes());
        self.put(block, 0, &r);
    }

    fn dev(&self, ss: u32) -> MemDevice {
        MemDevice::new(self.data.clone(), ss).unwrap()
    }
}

fn basic() -> Apm {
    Apm::new(512, 256, true, &[MAP, HFS, FREE, DATA])
}

fn read(d: &Apm, ss: u32) -> ApmRead {
    read_apm(&d.dev(ss)).unwrap().expect("APM")
}

fn codes(a: &[Anomaly]) -> Vec<&'static str> {
    a.iter().map(Anomaly::code).collect()
}

fn all_codes(r: &ApmRead) -> Vec<&'static str> {
    let mut v = codes(&r.anomalies);
    for p in &r.partitions {
        v.extend(codes(&p.partition.anomalies));
    }
    v
}

#[test]
fn healthy_map_lists_every_entry() {
    let r = read(&basic(), 512);
    assert!(r.ddm && r.map_found);
    assert_eq!((r.block_size, r.disk_blocks, r.map_entries), (512, 256, 4));
    assert_eq!((r.ddm_block_size, r.ddm_block_count), (512, 256));
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    assert_eq!(r.confidence, Confidence::Valid);
    let rows: Vec<(Origin, u64, u64, String, String, bool)> = r
        .partitions
        .iter()
        .map(|p| {
            let q = &p.partition;
            let PartitionKind::Apm(t) = q.kind else {
                panic!()
            };
            (
                q.origin,
                q.start_lba,
                q.sector_count,
                t.text(),
                q.name.clone().unwrap(),
                q.active,
            )
        })
        .collect();
    let row = |i: u32, e: Entry, active: bool| {
        (
            Origin::Apm { index: i },
            u64::from(e.0),
            u64::from(e.1),
            e.2.to_string(),
            e.3.to_string(),
            active,
        )
    };
    assert_eq!(
        rows,
        vec![
            row(1, MAP, false),
            row(2, HFS, false),
            row(3, FREE, false),
            row(4, DATA, true)
        ]
    );
    assert_eq!(r.partitions[1].status, 0x4000_0033);
    assert_eq!(
        (r.partitions[1].data_start, r.partitions[1].data_count),
        (0, 64)
    );
    assert!(r
        .partitions
        .iter()
        .all(|p| p.partition.confidence == Confidence::Valid));
}

#[test]
fn partition_table_reports_apm_and_opens_partitions() {
    let d = basic();
    let dev = d.dev(512);
    let t = read_partition_table(&dev).unwrap();
    assert_eq!((t.scheme, t.sector_size), (Scheme::Apm, 512));
    assert!(t.gpt.is_none());
    assert_eq!(t.partitions.len(), 4);
    // El bloque 0 es el DDM: el MBR no está (informativo).
    assert_eq!(codes(&t.anomalies), vec!["mbr_missing"]);
    assert_eq!(t.confidence(), Confidence::Valid);
    let hfs = &t.partitions[1];
    assert_eq!(t.apm_entry(hfs).unwrap().status, 0x4000_0033);
    assert!(t.gpt_entry(hfs).is_none());
    assert_eq!(hfs.kind.type_name(), Some("HFS / HFS+"));
    assert_eq!(t.partitions[2].kind.type_name(), Some("Espacio libre"));
    let c = open_coordinates(&t, 1, 512, 256, false).unwrap();
    assert_eq!((c.image_start, c.count, c.shift_applied), (64, 64, 0));
}

#[test]
fn larger_blocks_keep_block_units_and_refuse_silent_conversion() {
    // Bloques de 2048 B en un origen de 512 B: entradas cada 2048 B y LBA en bloques de 2048.
    let d = Apm::new(
        2048,
        64,
        true,
        &[
            (1, 3, "Apple_partition_map", "Apple", 0),
            (16, 32, "Apple_HFS", "CD", 0),
        ],
    );
    let r = read(&d, 512);
    assert_eq!((r.block_size, r.disk_blocks), (2048, 64));
    assert_eq!(codes(&r.anomalies), vec!["apm_sector_size_mismatch"]);
    assert_eq!(r.anomalies[0].severity(), Severity::Notice);
    assert_eq!(r.anomalies[0].value(), Some(2048));
    let t = read_partition_table(&d.dev(512)).unwrap();
    assert_eq!((t.scheme, t.sector_size), (Scheme::Apm, 2048));
    assert_eq!(
        (t.partitions[1].start_lba, t.partitions[1].sector_count),
        (16, 32)
    );
    assert!(open_coordinates(&t, 1, 512, 256, false)
        .unwrap_err()
        .contains("2048 B"));
    // Con el origen abierto con sectores de 2048 B no hay nada que convertir.
    let t = read_partition_table(&d.dev(2048)).unwrap();
    assert_eq!((t.scheme, t.sector_size), (Scheme::Apm, 2048));
    assert!(t.all_anomalies().all(|a| a.code() == "mbr_missing"));
    let c = open_coordinates(&t, 1, 2048, 64, false).unwrap();
    assert_eq!((c.image_start, c.count), (16, 32));
}

#[test]
fn smaller_blocks_inside_large_sectors() {
    // Bloques de 512 B en un origen de 4096 B: varias entradas dentro del mismo sector.
    let d = basic();
    let r = read(&d, 4096);
    assert_eq!(r.block_size, 512);
    assert_eq!(r.partitions.len(), 4);
    assert_eq!(codes(&r.anomalies), vec!["apm_sector_size_mismatch"]);
    // La LBA de cada anomalía es la del sector del origen que contiene la entrada.
    let mut d = basic();
    d.put(3, 0, b"XX");
    let r = read(&d, 4096);
    let a = r
        .anomalies
        .iter()
        .find(|a| a.code() == "apm_entry_signature")
        .unwrap();
    assert_eq!((a.lba(), a.value()), (Some(0), Some(3)));
}

#[test]
fn damaged_entry_is_not_listed() {
    let mut d = basic();
    d.put(3, 0, b"\0\0");
    let r = read(&d, 512);
    assert_eq!(codes(&r.anomalies), vec!["apm_entry_signature"]);
    assert_eq!(r.anomalies[0].lba(), Some(3));
    assert_eq!(r.anomalies[0].severity(), Severity::Suspicious);
    let idx: Vec<Origin> = r.partitions.iter().map(|p| p.partition.origin).collect();
    assert_eq!(
        idx,
        vec![
            Origin::Apm { index: 1 },
            Origin::Apm { index: 2 },
            Origin::Apm { index: 4 }
        ]
    );
    assert!(matches!(r.confidence, Confidence::Suspicious(_)));
}

#[test]
fn entry_bounds_overlap_and_data_area() {
    let d = Apm::new(
        512,
        256,
        true,
        &[
            MAP,
            (300, 10, "Apple_HFS", "Fuera", 0),
            (250, 10, "Apple_HFS", "Parcial", 0),
            (64, 0, "Apple_HFS", "Cero", 0),
            (64, 64, "Apple_HFS", "A", 0),
            (100, 20, "Apple_HFS", "B", 0),
        ],
    );
    let r = read(&d, 512);
    assert_eq!(codes(&r.anomalies), vec!["apm_entry_zero_length"]);
    let own = |i: u32| {
        let p = r
            .partitions
            .iter()
            .find(|p| p.partition.origin == Origin::Apm { index: i })
            .unwrap();
        (
            codes(&p.partition.anomalies),
            p.partition.confidence.clone(),
        )
    };
    let (c, conf) = own(2);
    assert_eq!(c, vec!["apm_entry_outside_disk"]);
    assert!(matches!(conf, Confidence::Invalid(_)));
    let (c, conf) = own(3);
    assert_eq!(c, vec!["apm_entry_partially_outside_disk"]);
    assert!(matches!(conf, Confidence::Suspicious(_)));
    assert_eq!(own(5).0, Vec::<&str>::new());
    assert_eq!(own(6).0, vec!["apm_entry_overlap"]);
    assert_eq!(r.partitions.len(), 5);
    assert!(matches!(r.confidence, Confidence::Invalid(_)));
    // Área de datos lógica fuera de la partición.
    let mut d = basic();
    d.put(2, 80, &60u32.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(all_codes(&r), vec!["apm_entry_data_outside"]);
}

#[test]
fn map_count_limits() {
    // Cuenta 0: se lee solo la primera entrada.
    let mut d = basic();
    d.put(1, 4, &0u32.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(r.map_entries, 0);
    assert_eq!(r.partitions.len(), 1);
    assert_eq!(codes(&r.anomalies), vec!["apm_map_count_invalid"]);
    assert_eq!(r.anomalies[0].value(), Some(0));
    // Cuenta enorme: se limita a MAX_MAP_ENTRIES y al origen (255 entradas caben en 256 bloques);
    // los bloques sin firma no se listan.
    let mut d = basic();
    d.put(1, 4, &u32::MAX.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(r.partitions.len(), 4);
    let c = codes(&r.anomalies);
    assert_eq!(c[..2], ["apm_map_count_invalid", "apm_map_outside_image"]);
    assert_eq!(r.anomalies[1].value(), Some(256));
    assert_eq!(
        c.iter().filter(|&&x| x == "apm_map_count_mismatch").count(),
        3
    );
    assert_eq!(
        c.iter().filter(|&&x| x == "apm_entry_signature").count(),
        251
    );
    // Límite explícito con un origen grande: como mucho MAX_MAP_ENTRIES entradas leídas.
    let mut d = Apm::new(512, 2048, true, &[MAP, HFS]);
    d.put(1, 4, &100_000u32.to_be_bytes());
    let r = read(&d, 512);
    let c = codes(&r.anomalies);
    assert_eq!(c[0], "apm_map_count_invalid");
    assert!(!c.contains(&"apm_map_outside_image"));
    assert_eq!(
        c.iter().filter(|&&x| x == "apm_entry_signature").count(),
        MAX_MAP_ENTRIES as usize - 2
    );
    // Más entradas de las que caben en el origen.
    let d = Apm::new(512, 8, true, &[(1, 7, "Apple_partition_map", "Apple", 0)]);
    let mut d = d;
    d.put(1, 4, &20u32.to_be_bytes());
    let r = read(&d, 512);
    let a = r
        .anomalies
        .iter()
        .find(|a| a.code() == "apm_map_outside_image")
        .unwrap();
    assert_eq!((a.lba(), a.value()), (None, Some(8)));
    assert!(r.anomalies.len() <= 8);
}

#[test]
fn ddm_problems() {
    // Sin DDM (bloque 0 a cero): el mapa se lee igual.
    let d = Apm::new(512, 256, false, &[MAP, HFS]);
    let r = read(&d, 512);
    assert!(!r.ddm);
    assert_eq!(codes(&r.anomalies), vec!["apm_ddm_missing"]);
    assert_eq!(r.partitions.len(), 2);
    // Tamaño de bloque no admitido.
    let mut d = basic();
    d.put(0, 2, &1000u16.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(codes(&r.anomalies), vec!["apm_block_size_invalid"]);
    assert_eq!(r.anomalies[0].value(), Some(1000));
    // DDM con 2048 y mapa en bloques de 512 (como en algunos CD híbridos).
    let mut d = basic();
    d.put(0, 2, &2048u16.to_be_bytes());
    d.put(0, 4, &64u32.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(r.block_size, 512);
    assert_eq!(codes(&r.anomalies), vec!["apm_block_size_mismatch"]);
    // Tamaño declarado: menor que el origen es informativo; mayor (imagen recortada), sospechoso.
    let mut d = basic();
    d.put(0, 4, &100u32.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(r.anomalies[0].severity(), Severity::Notice);
    assert_eq!(r.confidence, Confidence::Valid);
    let mut d = basic();
    d.put(0, 4, &1000u32.to_be_bytes());
    let r = read(&d, 512);
    assert_eq!(codes(&r.anomalies), vec!["apm_disk_size_mismatch"]);
    assert_eq!(r.anomalies[0].severity(), Severity::Suspicious);
    assert_eq!(r.anomalies[0].value(), Some(1000));
}

#[test]
fn ddm_without_map() {
    let d = Apm::new(512, 64, true, &[]);
    let r = read(&d, 512);
    assert!(!r.map_found && r.partitions.is_empty());
    assert_eq!(codes(&r.anomalies), vec!["apm_map_missing"]);
    assert_eq!(r.anomalies[0].lba(), Some(1));
    let t = read_partition_table(&d.dev(512)).unwrap();
    assert_eq!(t.scheme, Scheme::Apm);
    assert!(matches!(t.confidence(), Confidence::Invalid(_)));
    // «ER» con un tamaño de bloque no admitido y sin mapa no es un APM.
    let mut d = Apm::new(512, 64, false, &[]);
    d.put(0, 0, b"ER\x03\xe8");
    assert!(read_apm(&d.dev(512)).unwrap().is_none());
    let t = read_partition_table(&d.dev(512)).unwrap();
    assert_eq!(t.scheme, Scheme::None);
}

#[test]
fn not_apm_and_mbr_precedence() {
    let blank = MemDevice::new(vec![0u8; 64 * 512], 512).unwrap();
    assert!(read_apm(&blank).unwrap().is_none());
    // Un MBR clásico manda aunque haya un mapa APM detrás.
    let mut d = basic();
    let e = &mut d.data[0x1BE..0x1CE];
    e[4] = 0x07;
    e[8..12].copy_from_slice(&64u32.to_le_bytes());
    e[12..16].copy_from_slice(&64u32.to_le_bytes());
    d.data[510] = 0x55;
    d.data[511] = 0xAA;
    let t = read_partition_table(&d.dev(512)).unwrap();
    assert_eq!(t.scheme, Scheme::Mbr);
    assert!(t.apm.is_none());
    // Origen más pequeño que dos entradas.
    let tiny = MemDevice::new(b"ER".iter().copied().chain([0u8; 510]).collect(), 512).unwrap();
    assert!(read_apm(&tiny).unwrap().is_none());
}

#[test]
fn unreadable_sectors_are_never_interpreted() {
    // Entrada 2 ilegible: no se lista y se informa la LBA.
    let d = basic();
    let dev = FaultyDevice::new(d.dev(512)).with_bad([2]);
    let r = read_apm(&dev).unwrap().unwrap();
    assert_eq!(codes(&r.anomalies), vec!["unreadable_sector"]);
    assert_eq!(r.anomalies[0].lba(), Some(2));
    assert_eq!(r.partitions.len(), 3);
    // DDM ilegible: el mapa se encuentra igualmente con el tamaño de sector.
    let dev = FaultyDevice::new(d.dev(512)).with_bad([0]);
    let r = read_apm(&dev).unwrap().unwrap();
    assert_eq!(codes(&r.anomalies), vec!["unreadable_sector"]);
    assert_eq!(r.partitions.len(), 4);
    // Primera entrada ilegible tras un DDM válido: sin mapa.
    let dev = FaultyDevice::new(d.dev(512)).with_bad([1]);
    let r = read_apm(&dev).unwrap().unwrap();
    assert_eq!(
        codes(&r.anomalies),
        vec!["unreadable_sector", "apm_map_missing"]
    );
}

#[test]
fn texts_and_type_names() {
    assert_eq!(decode_text(b"Apple_HFS\0garbage"), "Apple_HFS");
    assert_eq!(decode_text(&[b'A', 0xE9, b'B']), "A?B");
    let mut t = [0u8; 32];
    t[..9].copy_from_slice(b"apple_hfs");
    assert_eq!(type_name(&ApmType(t)), Some("HFS / HFS+"));
    t[..9].copy_from_slice(b"Vendor_XY");
    assert_eq!(type_name(&ApmType(t)), None);
    assert!(valid_block_size(512) && valid_block_size(4096));
    assert!(!valid_block_size(1000) && !valid_block_size(8192) && !valid_block_size(256));
    let a = Anomaly::Apm(ApmAnomaly::EntryOutsideDisk {
        lba: 4,
        index: 4,
        start: 5000,
    });
    assert_eq!(
        (a.code(), a.lba(), a.value(), a.severity()),
        (
            "apm_entry_outside_disk",
            Some(4),
            Some(4),
            Severity::Invalid
        )
    );
    assert!(a.to_string().contains("bloque 5000"));
    assert!(!a.value_signed());
}

/// Generador congruencial determinista (sin dependencias).
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
fn seeded_fuzz_never_panics_hangs_or_explodes() {
    let started = std::time::Instant::now();
    let mut rng = Lcg(0x4D52_4150_4D31);
    let mut listed = 0;
    for round in 0..3000 {
        let mut d = basic();
        for _ in 0..1 + rng.next() % 10 {
            let block = (rng.next() % 6) as usize;
            let off = match rng.next() % 3 {
                0 => (rng.next() % 16) as usize,
                1 => [2, 4, 8, 12, 80, 84][(rng.next() % 6) as usize],
                _ => (rng.next() % 512) as usize,
            };
            let v = [0u32, 1, 255, 256, 512, 4096, u32::MAX, rng.next() as u32]
                [(rng.next() % 8) as usize];
            let bytes = v.to_be_bytes();
            let n = (4).min(512 - off);
            d.put(block, off, &bytes[..n]);
        }
        let ss = [512u32, 512, 4096, 2048][round % 4];
        let len = d.data.len() / ss as usize * ss as usize;
        let dev = MemDevice::new(d.data[..len].to_vec(), ss).unwrap();
        let t0 = std::time::Instant::now();
        if let Some(r) = read_apm(&dev).unwrap() {
            assert!(r.partitions.len() <= MAX_MAP_ENTRIES as usize);
            assert!(
                r.anomalies.len() <= 8 + 2 * MAX_MAP_ENTRIES as usize,
                "ronda {round}"
            );
            for p in &r.partitions {
                assert!(p.partition.anomalies.len() <= 3);
                let _ = p.partition.confidence.to_string();
            }
            listed += r.partitions.len();
        }
        let t = read_partition_table(&dev).unwrap();
        for i in 0..t.partitions.len() {
            let _ = open_coordinates(&t, i, ss, dev.sector_count(), false);
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    }
    assert!(
        listed > 3000,
        "el fuzzing también recorre el camino feliz ({listed})"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
}
