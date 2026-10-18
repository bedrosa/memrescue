//! Pruebas unitarias por regla con discos sintéticos construidos en memoria.

use super::*;
use crate::{read_partition_table, Anomaly, Origin, Scheme};
use mr_io::{FaultyDevice, MemDevice};

const SS: usize = 512;

/// Disco en memoria de `sectors` sectores de 512 B.
struct Disk(Vec<u8>);

impl Disk {
    fn new(sectors: usize) -> Self {
        Self(vec![0u8; sectors * SS])
    }

    /// Escribe una tabla (entradas `(boot, tipo, inicio, sectores)`) en `lba`, con o sin firma.
    fn table(mut self, lba: usize, entries: &[(u8, u8, u32, u32)], signature: bool) -> Self {
        let s = &mut self.0[lba * SS..(lba + 1) * SS];
        for (i, &(boot, kind, start, count)) in entries.iter().enumerate() {
            let at = TABLE_OFFSET + i * ENTRY_SIZE;
            s[at] = boot;
            s[at + 1..at + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
            s[at + 4] = kind;
            s[at + 5..at + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
            s[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
            s[at + 12..at + 16].copy_from_slice(&count.to_le_bytes());
        }
        if signature {
            s[SIGNATURE_OFFSET] = 0x55;
            s[SIGNATURE_OFFSET + 1] = 0xAA;
        }
        self
    }

    fn mbr(self, entries: &[(u8, u8, u32, u32)]) -> Self {
        self.table(0, entries, true)
    }

    fn put(mut self, lba: usize, offset: usize, bytes: &[u8]) -> Self {
        let at = lba * SS + offset;
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
        self
    }

    fn dev(self) -> MemDevice {
        MemDevice::new(self.0, SS as u32).unwrap()
    }

    fn scan(self) -> MbrScan {
        read_mbr(&self.dev()).unwrap()
    }
}

/// Códigos de todas las anomalías (tabla y particiones), ordenados.
fn codes(s: &MbrScan) -> Vec<&'static str> {
    let mut v: Vec<&str> = s
        .anomalies
        .iter()
        .chain(s.partitions.iter().flat_map(|p| p.anomalies.iter()))
        .map(Anomaly::code)
        .collect();
    v.sort_unstable();
    v
}

fn starts(s: &MbrScan) -> Vec<(u64, u64)> {
    s.partitions
        .iter()
        .map(|p| (p.start_lba, p.sector_count))
        .collect()
}

#[test]
fn healthy_primaries_are_valid() {
    let s = Disk::new(4096)
        .mbr(&[(0x80, 0x07, 2048, 1024), (0, 0x83, 3072, 1024)])
        .scan();
    assert_eq!(s.scheme, Scheme::Mbr);
    assert!(codes(&s).is_empty(), "{:?}", s.anomalies);
    assert_eq!(starts(&s), vec![(2048, 1024), (3072, 1024)]);
    assert!(s.partitions[0].active && !s.partitions[1].active);
    assert_eq!(s.partitions[0].kind, PartitionKind::Mbr(0x07));
    assert_eq!(s.partitions[1].origin, Origin::Primary { slot: 1 });
    assert!(s
        .partitions
        .iter()
        .all(|p| p.confidence == Confidence::Valid));
}

#[test]
fn blank_and_empty_sectors_have_no_table() {
    let s = Disk::new(64).scan();
    assert_eq!(s.scheme, Scheme::None);
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::NoTable {
            lba: 0,
            reason: NoTableReason::Blank
        })]
    );
    assert_eq!(codes(&s), vec!["mbr_missing"]);
    let s = Disk::new(64).mbr(&[]).scan();
    assert_eq!((s.scheme, codes(&s)), (Scheme::None, vec!["mbr_empty"]));
}

#[test]
fn missing_signature_is_read_but_suspicious() {
    let s = Disk::new(4096)
        .table(0, &[(0x80, 0x07, 2048, 1024)], false)
        .scan();
    assert_eq!(s.scheme, Scheme::Mbr);
    assert_eq!(codes(&s), vec!["mbr_signature_missing"]);
    assert!(matches!(
        s.partitions[0].confidence,
        Confidence::Suspicious(_)
    ));
}

#[test]
fn implausible_unsigned_sector_is_not_a_table() {
    // Indicador no válido en una entrada usada.
    let s = Disk::new(4096)
        .table(0, &[(0x12, 0x07, 2048, 1024)], false)
        .scan();
    assert_eq!(s.scheme, Scheme::None);
    assert!(matches!(
        s.anomalies[..],
        [Anomaly::Mbr(MbrAnomaly::NoTable {
            reason: NoTableReason::InvalidBootIndicator,
            ..
        })]
    ));
    // Ninguna entrada dentro del disco.
    let s = Disk::new(4096)
        .table(0, &[(0, 0x07, 900_000, 10)], false)
        .scan();
    assert!(matches!(
        s.anomalies[..],
        [Anomaly::Mbr(MbrAnomaly::NoTable {
            reason: NoTableReason::NoEntryInsideDisk,
            ..
        })]
    ));
    // Dos extendidas.
    let s = Disk::new(4096)
        .table(0, &[(0, 0x05, 100, 10), (0, 0x0F, 200, 10)], false)
        .scan();
    assert!(matches!(
        s.anomalies[..],
        [Anomaly::Mbr(MbrAnomaly::NoTable {
            reason: NoTableReason::MultipleExtended,
            ..
        })]
    ));
}

#[test]
fn invalid_boot_indicator_with_signature_is_invalid() {
    let s = Disk::new(4096)
        .mbr(&[(0x81, 0x07, 2048, 1024), (0, 0x83, 3072, 1024)])
        .scan();
    assert_eq!(s.scheme, Scheme::Mbr);
    assert_eq!(codes(&s), vec!["mbr_invalid_boot_indicator"]);
    assert!(matches!(s.partitions[0].confidence, Confidence::Invalid(_)));
    assert_eq!(s.partitions[1].confidence, Confidence::Valid);
    // En una entrada libre solo se anota.
    let s = Disk::new(4096)
        .mbr(&[(0, 0x07, 2048, 1024), (0x80, 0, 0, 0)])
        .scan();
    assert_eq!(codes(&s), vec!["mbr_invalid_boot_indicator"]);
    assert_eq!(s.anomalies[0].severity(), Severity::Notice);
    assert_eq!(s.partitions[0].confidence, Confidence::Valid);
}

#[test]
fn several_active_entries() {
    let s = Disk::new(4096)
        .mbr(&[(0x80, 0x07, 2048, 1024), (0x80, 0x83, 3072, 1024)])
        .scan();
    assert_eq!(codes(&s), vec!["mbr_multiple_active"]);
    assert_eq!(
        s.anomalies[0],
        Anomaly::Mbr(MbrAnomaly::MultipleActive {
            lba: 0,
            slots: vec![0, 1]
        })
    );
    assert!(s
        .partitions
        .iter()
        .all(|p| matches!(p.confidence, Confidence::Suspicious(_))));
}

#[test]
fn outside_and_partially_outside_disk() {
    let s = Disk::new(4096)
        .mbr(&[
            (0, 0x07, 2048, 1024),
            (0, 0x83, 3072, 2048),
            (0, 0x83, 5000, 10),
        ])
        .scan();
    assert_eq!(
        codes(&s),
        vec!["mbr_outside_disk", "mbr_partially_outside_disk"]
    );
    assert_eq!(s.partitions.len(), 3);
    assert!(matches!(
        s.partitions[1].confidence,
        Confidence::Suspicious(_)
    ));
    assert!(matches!(s.partitions[2].confidence, Confidence::Invalid(_)));
    assert_eq!(
        s.partitions[1].anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::PartiallyOutsideDisk {
            lba: 0,
            origin: Origin::Primary { slot: 1 },
            end: 5120,
            disk_sectors: 4096
        })]
    );
}

#[test]
fn overlapping_entries_are_reported_once_per_partition() {
    let s = Disk::new(4096)
        .mbr(&[(0, 0x07, 2048, 1024), (0, 0x0C, 2560, 1024)])
        .scan();
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::Overlap {
            lba: 0,
            first: Origin::Primary { slot: 0 },
            second: Origin::Primary { slot: 1 }
        })]
    );
    // Cuatro entradas idénticas: 3 anomalías, no 6.
    let e = (0, 0x07, 100, 100);
    let s = Disk::new(4096).mbr(&[e, e, e, e]).scan();
    assert_eq!(codes(&s), vec!["mbr_overlap"; 3]);
    // Contiguas: sin solape.
    let s = Disk::new(4096)
        .mbr(&[(0, 0x07, 100, 100), (0, 0x07, 200, 100)])
        .scan();
    assert!(codes(&s).is_empty());
}

#[test]
fn unknown_type_and_zero_length() {
    let s = Disk::new(4096)
        .mbr(&[
            (0, 0x07, 2048, 1024),
            (0, 0x7F, 3072, 512),
            (0, 0x83, 3600, 0),
        ])
        .scan();
    assert_eq!(codes(&s), vec!["mbr_unknown_type", "mbr_zero_length"]);
    assert_eq!(s.partitions.len(), 2);
    assert_eq!(
        s.partitions[1].anomalies[0],
        Anomaly::Mbr(MbrAnomaly::UnknownType {
            lba: 0,
            origin: Origin::Primary { slot: 1 },
            kind: 0x7F
        })
    );
    assert_eq!(s.partitions[1].anomalies[0].value(), Some(0x7F));
    assert!(type_name(0x7F).is_none());
    for t in [0x05, 0x07, 0x0B, 0x0C, 0x0F, 0x83, 0x85, 0xEE, 0xAF, 0x42] {
        assert!(type_name(t).is_some(), "0x{t:02X}");
    }
}

/// MBR con P1 y extendida `ext` en [2560, 4096) y una cadena de EBR. Cada EBR: `(lba, dato
/// relativo, sectores, enlace relativo a la extendida)`.
fn chain(ext_type: u8, ebrs: &[(usize, u32, u32, Option<u32>)]) -> Disk {
    let mut d = Disk::new(4096).mbr(&[(0, 0x83, 2048, 512), (0, ext_type, 2560, 1536)]);
    for &(lba, rel, count, link) in ebrs {
        let mut entries = vec![(0u8, 0x83u8, rel, count)];
        if let Some(l) = link {
            entries.push((0, 0x05, l, 512));
        }
        d = d.table(lba, &entries, true);
    }
    d
}

#[test]
fn ebr_chain_uses_both_addressing_modes_and_all_extended_types() {
    for ext in [0x05, 0x0F, 0x85] {
        let s = chain(ext, &[(2560, 64, 448, Some(512)), (3072, 64, 448, None)]).scan();
        assert_eq!(s.scheme, Scheme::Mbr);
        assert!(codes(&s).is_empty(), "0x{ext:02X}: {:?}", s.anomalies);
        // Datos relativos a cada EBR; enlace relativo al inicio de la extendida.
        assert_eq!(starts(&s), vec![(2048, 512), (2624, 448), (3136, 448)]);
        assert_eq!(s.ebr_lbas, vec![2560, 3072]);
        assert_eq!(s.extended, Some((2560, 1536, ext)));
        assert_eq!(
            s.partitions[2].origin,
            Origin::Logical {
                ebr_lba: 3072,
                slot: 0
            }
        );
    }
}

#[test]
fn ebr_loop_is_detected() {
    let s = chain(
        0x0F,
        &[(2560, 64, 448, Some(512)), (3072, 64, 448, Some(0))],
    )
    .scan();
    assert_eq!(starts(&s).len(), 3);
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::EbrLoop {
            lba: 3072,
            target: 2560
        })]
    );
    // Un EBR que se enlaza a sí mismo.
    let s = chain(0x0F, &[(2560, 64, 448, Some(0))]).scan();
    assert_eq!(codes(&s), vec!["ebr_loop"]);
}

#[test]
fn ebr_links_outside_disk_or_extended() {
    let s = chain(0x0F, &[(2560, 64, 448, Some(2536))]).scan();
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::LinkOutsideDisk {
            lba: 2560,
            target: 5096
        })]
    );
    // Dentro del disco pero fuera de la extendida (enlace "hacia atrás" con u32 enorme imposible:
    // aquí, una extendida más corta).
    let d = Disk::new(8192).mbr(&[(0, 0x0F, 2560, 1024)]).table(
        2560,
        &[(0, 0x83, 64, 448), (0, 0x05, 2000, 100)],
        true,
    );
    let s = d.scan();
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::LinkOutsideExtended {
            lba: 2560,
            target: 4560
        })]
    );
    assert_eq!(s.anomalies[0].target(), Some(4560));
}

#[test]
fn logical_outside_extended_and_multiple_links() {
    let s = chain(0x0F, &[(2560, 1400, 448, None)]).scan();
    assert_eq!(
        codes(&s),
        vec!["ebr_logical_outside_extended", "mbr_partially_outside_disk"]
    );
    let d = Disk::new(4096).mbr(&[(0, 0x0F, 2560, 1536)]).table(
        2560,
        &[(0, 0x83, 64, 100), (0, 0x05, 512, 10), (0, 0x0F, 600, 10)],
        true,
    );
    let s = d.scan();
    assert_eq!(codes(&s), vec!["mbr_multiple_extended"]);
    assert!(s.partitions.is_empty(), "un EBR con dos enlaces no se usa");
}

#[test]
fn ebr_without_signature_or_blank() {
    // EBR a cero: la cadena termina con una anomalía.
    let s = Disk::new(4096).mbr(&[(0, 0x0F, 2560, 1536)]).scan();
    assert_eq!(codes(&s), vec!["ebr_invalid"]);
    // EBR sin firma pero plausible: se usa y se anota.
    let s = Disk::new(4096)
        .mbr(&[(0, 0x0F, 2560, 1536)])
        .table(2560, &[(0, 0x83, 64, 100)], false)
        .scan();
    assert_eq!(codes(&s), vec!["mbr_signature_missing"]);
    assert!(matches!(
        s.partitions[0].confidence,
        Confidence::Suspicious(_)
    ));
    // EBR sin firma y con indicador no válido: no es una tabla.
    let s = Disk::new(4096)
        .mbr(&[(0, 0x0F, 2560, 1536)])
        .table(2560, &[(0x33, 0x83, 64, 100)], false)
        .scan();
    assert_eq!(codes(&s), vec!["ebr_invalid"]);
    assert!(s.partitions.is_empty());
}

#[test]
fn chain_length_is_limited() {
    // MAX_EBR_CHAIN + 10 EBR encadenados, cada uno con una lógica de 1 sector.
    let n = MAX_EBR_CHAIN + 10;
    let total = 16 + 2 * n + 4;
    let ext_start = 16u32;
    let mut d = Disk::new(total).mbr(&[(0, 0x0F, ext_start, (2 * n + 2) as u32)]);
    for i in 0..n {
        let lba = ext_start as usize + 2 * i;
        d = d.table(
            lba,
            &[(0, 0x83, 1, 1), (0, 0x05, (2 * (i + 1)) as u32, 2)],
            true,
        );
    }
    let s = d.scan();
    assert_eq!(s.ebr_lbas.len(), MAX_EBR_CHAIN);
    assert_eq!(s.partitions.len(), MAX_EBR_CHAIN);
    assert_eq!(codes(&s), vec!["ebr_chain_limit"]);
}

#[test]
fn protective_and_hybrid_mbr() {
    let s = Disk::new(2048).mbr(&[(0, 0xEE, 1, 2047)]).scan();
    assert_eq!((s.scheme, codes(&s)), (Scheme::Gpt, vec![]));
    assert!(s.partitions.is_empty());
    // 0xFFFFFFFF se admite (disco mayor de lo representable).
    let s = Disk::new(2048).mbr(&[(0, 0xEE, 1, u32::MAX)]).scan();
    assert!(codes(&s).is_empty());
    let s = Disk::new(2048).mbr(&[(0, 0xEE, 2, 4095)]).scan();
    assert_eq!(s.scheme, Scheme::Gpt);
    assert_eq!(
        s.anomalies,
        vec![
            Anomaly::Mbr(MbrAnomaly::ProtectiveStart { lba: 0, start: 2 }),
            Anomaly::Mbr(MbrAnomaly::ProtectiveSizeMismatch {
                lba: 0,
                declared: 4095,
                expected: 2047
            })
        ]
    );
    let s = Disk::new(4096)
        .mbr(&[
            (0, 0xEE, 1, 2047),
            (0x80, 0x07, 2048, 1024),
            (0, 0x83, 3072, 512),
        ])
        .scan();
    assert_eq!(s.scheme, Scheme::Hybrid);
    assert_eq!(codes(&s), vec!["hybrid_mbr"]);
    assert_eq!(starts(&s), vec![(2048, 1024), (3072, 512)]);
    assert_eq!(s.partitions[0].origin, Origin::Primary { slot: 1 });
    assert!(s
        .partitions
        .iter()
        .all(|p| p.confidence == Confidence::Valid));
}

#[test]
fn volume_boot_sector_and_gpt_header_at_lba0() {
    let s = Disk::new(64)
        .put(0, 0, b"\xEB\x52\x90NTFS    ")
        .put(0, 0x1FE, &[0x55, 0xAA])
        .scan();
    assert_eq!(
        s.anomalies,
        vec![Anomaly::Mbr(MbrAnomaly::Superfloppy { lba: 0, fs: "NTFS" })]
    );
    assert_eq!(codes(&s), vec!["superfloppy"]);
    let s = Disk::new(64)
        .put(0, 0, b"\xEB\x58\x90MSWIN4.1")
        .put(0, 0x52, b"FAT32   ")
        .scan();
    assert!(matches!(
        s.anomalies[..],
        [Anomaly::Mbr(MbrAnomaly::Superfloppy { fs: "FAT32", .. })]
    ));
    let s = Disk::new(64).put(0, 0, b"EFI PART").scan();
    assert!(matches!(
        s.anomalies[..],
        [Anomaly::Mbr(MbrAnomaly::NoTable {
            reason: NoTableReason::GptHeader,
            ..
        })]
    ));
}

#[test]
fn unreadable_sectors_are_never_parsed() {
    // El sector ilegible contiene un relleno; aunque el contenido real fuera una tabla, no se usa.
    let d = chain(0x0F, &[(2560, 64, 448, Some(512)), (3072, 64, 448, None)]);
    let dev = FaultyDevice::new(MemDevice::new(d.0.clone(), 512).unwrap()).with_bad([0]);
    let s = read_mbr(&dev).unwrap();
    assert_eq!(s.scheme, Scheme::None);
    assert_eq!(s.anomalies, vec![Anomaly::UnreadableSector { lba: 0 }]);
    assert!(s.partitions.is_empty());
    // EBR ilegible: la cadena se detiene ahí.
    let dev = FaultyDevice::new(MemDevice::new(d.0, 512).unwrap()).with_bad([3072]);
    let s = read_mbr(&dev).unwrap();
    assert_eq!(s.scheme, Scheme::Mbr);
    assert_eq!(starts(&s), vec![(2048, 512), (2624, 448)]);
    assert_eq!(s.anomalies, vec![Anomaly::UnreadableSector { lba: 3072 }]);
    let t = read_partition_table(&dev).unwrap();
    assert!(matches!(t.confidence(), Confidence::Suspicious(_)));
}

#[test]
fn displaced_table_is_relative_to_its_position() {
    let d = Disk::new(4096).table(8, &[(0, 0x07, 2048, 1024)], true);
    let dev = d.dev();
    let s = read_mbr(&dev).unwrap();
    assert_eq!(codes(&s), vec!["mbr_missing"]);
    let s = read_mbr_at(&dev, 8).unwrap();
    assert_eq!(s.scheme, Scheme::Mbr);
    assert_eq!(starts(&s), vec![(2056, 1024)]);
    // Fuera del origen: sin tabla, sin error.
    let s = read_mbr_at(&dev, 4096).unwrap();
    assert_eq!(codes(&s), vec!["mbr_missing"]);
}

#[test]
fn extended_overlapping_a_primary() {
    let s = Disk::new(4096)
        .mbr(&[(0, 0x07, 2048, 1024), (0, 0x0F, 2560, 1536)])
        .table(2560, &[(0, 0x83, 1000, 100)], true)
        .scan();
    assert_eq!(codes(&s), vec!["mbr_overlap"]);
    assert_eq!(
        s.anomalies[0],
        Anomaly::Mbr(MbrAnomaly::Overlap {
            lba: 0,
            first: Origin::Primary { slot: 0 },
            second: Origin::Primary { slot: 1 }
        })
    );
}

#[test]
fn sector_sizes_other_than_512() {
    let mut data = vec![0u8; 64 * 4096];
    let e = &mut data[TABLE_OFFSET..TABLE_OFFSET + 16];
    e[4] = 0x07;
    e[8..12].copy_from_slice(&8u32.to_le_bytes());
    e[12..16].copy_from_slice(&16u32.to_le_bytes());
    data[SIGNATURE_OFFSET] = 0x55;
    data[SIGNATURE_OFFSET + 1] = 0xAA;
    let dev = MemDevice::new(data, 4096).unwrap();
    let t = read_partition_table(&dev).unwrap();
    assert_eq!((t.scheme, t.sector_size), (Scheme::Mbr, 4096));
    assert_eq!(t.partitions[0].start_lba, 8);
    assert_eq!(t.confidence(), Confidence::Valid);
}

#[test]
fn anomaly_texts_are_spanish_and_codes_stable() {
    let a = Anomaly::Mbr(MbrAnomaly::EbrLoop {
        lba: 3072,
        target: 2560,
    });
    assert_eq!(a.code(), "ebr_loop");
    assert!(a.to_string().contains("bucle"), "{a}");
    assert_eq!(a.lba(), Some(3072));
    assert_eq!(
        Anomaly::UnreadableSector { lba: 5 }.code(),
        "unreadable_sector"
    );
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
fn fuzz_random_sectors_never_panic_or_hang() {
    let mut rng = Lcg(0x4D52_5041_5254);
    let start = std::time::Instant::now();
    for round in 0..3000 {
        let sectors = 8 + (rng.next() % 120) as usize;
        let mut data = vec![0u8; sectors * SS];
        // Tablas aleatorias en sectores aleatorios, a menudo con firma y tipos extendidos para
        // forzar cadenas; enlaces pequeños para que caigan dentro del disco.
        for _ in 0..(1 + rng.next() % 12) {
            let lba = (rng.next() as usize) % sectors;
            let s = &mut data[lba * SS..(lba + 1) * SS];
            for i in 0..4 {
                let at = TABLE_OFFSET + i * ENTRY_SIZE;
                let r = rng.next();
                s[at] = if r.is_multiple_of(4) { r as u8 } else { 0 };
                s[at + 4] = match r % 5 {
                    0 => 0x05,
                    1 => 0x0F,
                    2 => 0x85,
                    3 => 0,
                    _ => (r >> 8) as u8,
                };
                let small = (rng.next() % (sectors as u64 * 2)) as u32;
                let start = if r.is_multiple_of(7) {
                    rng.next() as u32
                } else {
                    small
                };
                s[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
                s[at + 12..at + 16].copy_from_slice(&((rng.next() as u32) % 64).to_le_bytes());
            }
            if !rng.next().is_multiple_of(3) {
                s[SIGNATURE_OFFSET] = 0x55;
                s[SIGNATURE_OFFSET + 1] = 0xAA;
            }
        }
        let dev = MemDevice::new(data, SS as u32).unwrap();
        let s = read_mbr(&dev).unwrap();
        assert!(s.ebr_lbas.len() <= MAX_EBR_CHAIN, "ronda {round}");
        assert!(s.partitions.len() <= 4 + 4 * MAX_EBR_CHAIN, "ronda {round}");
        // Salida acotada: como mucho unas pocas anomalías por tabla leída y partición.
        let bound = 16 * (1 + s.ebr_lbas.len()) + 4 * s.partitions.len();
        assert!(
            s.anomalies.len() <= bound,
            "ronda {round}: {:?}",
            s.anomalies
        );
        for p in &s.partitions {
            assert!(p.anomalies.len() <= 4);
            let _ = p.confidence.to_string();
        }
    }
    assert!(start.elapsed().as_secs() < 60, "demasiado lento");
}
