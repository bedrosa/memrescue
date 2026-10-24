//! Pruebas de la copia del particionado con discos sintéticos en memoria: ida y vuelta, todas las
//! variantes, manipulaciones, comparación y fuzzing del lector.

use super::*;
use crate::apm::ApmAnomaly;
use crate::gpt::{GptAnomaly, GptField, HeaderState};
use crate::mbr::{MbrAnomaly, NoTableReason};
use crate::search::{
    self, Check, FoundEntry, FoundTable, FoundTableKind, FoundVolume, FsKind, NoObserver,
    SearchAnomaly, SearchOptions, SearchStatus, Tier, VolumeAnomaly,
};
use crate::{Confidence, PartitionKind};
use mr_io::{FaultyDevice, MemDevice};

const SS: usize = 512;

fn put_table(d: &mut [u8], lba: usize, entries: &[(u8, u8, u32, u32)]) {
    let s = &mut d[lba * SS..(lba + 1) * SS];
    for (i, &(boot, kind, start, count)) in entries.iter().enumerate() {
        let at = 446 + i * 16;
        s[at] = boot;
        s[at + 4] = kind;
        s[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
        s[at + 12..at + 16].copy_from_slice(&count.to_le_bytes());
    }
    s[510] = 0x55;
    s[511] = 0xAA;
}

/// MBR con una primaria NTFS y una extendida con dos lógicas (dos EBR).
fn mbr_disk() -> Vec<u8> {
    let mut d = vec![0u8; 8192 * SS];
    put_table(
        &mut d,
        0,
        &[(0x80, 0x07, 2048, 2048), (0x00, 0x0F, 4096, 4096)],
    );
    put_table(&mut d, 4096, &[(0, 0x83, 63, 1000), (0, 0x05, 2048, 2048)]);
    put_table(&mut d, 6144, &[(0, 0x0B, 63, 1000)]);
    d
}

const BASIC_DATA: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];

/// GPT de 4096 sectores de 512 B con dos particiones (128 entradas de 128 B) y MBR protector.
fn gpt_disk() -> Vec<u8> {
    let n: u64 = 4096;
    let mut d = vec![0u8; n as usize * SS];
    put_table(&mut d, 0, &[(0, 0xEE, 1, (n - 1) as u32)]);
    let mut arr = vec![0u8; 128 * 128];
    for (i, (first, last, name)) in [(2048u64, 3071u64, "MR-DATA"), (3072, 3583, "Ñandú")]
        .iter()
        .enumerate()
    {
        let e = &mut arr[i * 128..(i + 1) * 128];
        e[0..16].copy_from_slice(&BASIC_DATA);
        e[16..32].copy_from_slice(&[i as u8 + 1; 16]);
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        e[48..56].copy_from_slice(&(1u64 << 63 | 4).to_le_bytes());
        for (k, u) in name.encode_utf16().enumerate() {
            e[56 + 2 * k..58 + 2 * k].copy_from_slice(&u.to_le_bytes());
        }
    }
    let crc = mr_common::hash::crc32(&arr);
    let backup_entries = n - 33;
    for at in [2, backup_entries] {
        let o = at as usize * SS;
        d[o..o + arr.len()].copy_from_slice(&arr);
    }
    for (my, alt, entries) in [(1, n - 1, 2), (n - 1, 1, backup_entries)] {
        let o = my as usize * SS;
        let h = &mut d[o..o + SS];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(n - 34).to_le_bytes());
        h[56..72].copy_from_slice(&[0x5a; 16]);
        h[72..80].copy_from_slice(&entries.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc.to_le_bytes());
        let hc = mr_common::hash::crc32(&h[..92]);
        h[16..20].copy_from_slice(&hc.to_le_bytes());
    }
    d
}

fn dev(d: Vec<u8>) -> MemDevice {
    MemDevice::new(d, SS as u32).unwrap()
}

fn round_trip(b: &LayoutBackup) {
    let text = b.to_text();
    let back = LayoutBackup::from_text(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(&back, b);
    // Escritura determinista: releer y reescribir da el mismo texto.
    assert_eq!(back.to_text(), text);
}

fn kinds(b: &LayoutBackup) -> Vec<(RegionKind, u64, u64)> {
    b.regions
        .iter()
        .map(|r| (r.kind, r.offset, r.length))
        .collect()
}

#[test]
fn mbr_with_ebr_chain_round_trips_with_its_sectors() {
    let d = dev(mbr_disk());
    let b = capture(&d, "disco MBR sintético", None).unwrap();
    assert_eq!(b.layout.scheme, Scheme::Mbr);
    assert_eq!(b.layout.partitions.len(), 3);
    assert_eq!(
        kinds(&b),
        vec![
            (RegionKind::Mbr, 0, 512),
            (RegionKind::Ebr, 4096 * 512, 512),
            (RegionKind::Ebr, 6144 * 512, 512),
        ]
    );
    assert_eq!(
        b.regions[0].content,
        RegionContent::Data(mbr_disk()[..512].to_vec())
    );
    round_trip(&b);
    assert!(compare(&b, &d).unwrap().identical());
}

#[test]
fn gpt_round_trips_with_both_copies_and_search() {
    let d = dev(gpt_disk());
    let table = crate::read_partition_table(&d).unwrap();
    let found = search::search(&d, Some(&table), &SearchOptions::default(), &NoObserver).unwrap();
    let b = capture(&d, "GPT", Some(&found)).unwrap();
    assert_eq!(b.layout.scheme, Scheme::Gpt);
    assert_eq!(b.layout.partitions[1].name.as_deref(), Some("Ñandú"));
    assert_eq!(
        kinds(&b),
        vec![
            (RegionKind::Mbr, 0, 512),
            (RegionKind::GptPrimaryHeader, 512, 512),
            (RegionKind::GptPrimaryEntries, 1024, 16384),
            (RegionKind::GptBackupHeader, 4095 * 512, 512),
            (RegionKind::GptBackupEntries, 4063 * 512, 16384),
        ]
    );
    round_trip(&b);
    let text = b.to_text();
    assert!(text.contains("\"format\": \"memrescue-partition-backup\""));
    assert!(text.contains("\"version\": 1"));
    assert!(text.contains("ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"));
    assert!(compare(&b, &d).unwrap().identical());
}

#[test]
fn gpt_seen_with_another_sector_size_round_trips() {
    // La misma GPT de 512 B vista desde un dispositivo de 4096 B: se prueba el otro tamaño.
    let d = MemDevice::new(gpt_disk(), 4096).unwrap();
    let b = capture(&d, "", None).unwrap();
    round_trip(&b);
    assert!(compare(&b, &d).unwrap().identical());
}

#[test]
fn unreadable_table_sectors_are_recorded_without_content() {
    let d = FaultyDevice::new(dev(gpt_disk())).with_bad([1, 3]);
    let b = capture(&d, "", None).unwrap();
    let r = |k| b.regions.iter().find(|r| r.kind == k).unwrap();
    assert_eq!(
        r(RegionKind::GptPrimaryHeader).content,
        RegionContent::Unreadable(vec![1])
    );
    assert_eq!(
        b.layout.gpt.as_ref().unwrap().selected,
        Some(GptCopy::Backup)
    );
    round_trip(&b);
    assert!(compare(&b, &d).unwrap().identical());
    // Sobre el disco sano: las regiones ilegibles ahora se leen y la copia en uso cambia.
    let c = compare(&b, &dev(gpt_disk())).unwrap();
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::GptPrimaryHeader,
            backup: RegionStatus::Unreadable(_),
            current: RegionStatus::Data(_),
            ..
        }
    )));
    assert!(c.differences.contains(&Difference::GptSelected {
        backup: Some(GptCopy::Backup),
        current: Some(GptCopy::Primary)
    }));
}

#[test]
fn blank_and_tiny_devices_round_trip() {
    for d in [vec![0u8; 512], vec![0u8; 64 * 512]] {
        let d = dev(d);
        let b = capture(&d, "", None).unwrap();
        assert_eq!(b.layout.scheme, Scheme::None);
        round_trip(&b);
    }
}

/// Una anomalía de cada variante, todas las confianzas y una búsqueda con todas las variantes:
/// la escritura y la lectura cubren cada nombre.
#[test]
fn every_variant_round_trips() {
    use crate::gpt::EntriesState;
    let o = Origin::Logical {
        ebr_lba: 7,
        slot: 1,
    };
    let mbr = vec![
        MbrAnomaly::NoTable {
            lba: 0,
            reason: NoTableReason::Blank,
        },
        MbrAnomaly::EbrInvalid {
            lba: 1,
            reason: NoTableReason::OutsideDevice,
        },
        MbrAnomaly::Empty { lba: 2 },
        MbrAnomaly::SignatureMissing { lba: 3 },
        MbrAnomaly::Superfloppy {
            lba: 0,
            fs: "exFAT",
        },
        MbrAnomaly::InvalidBootIndicator {
            lba: 0,
            slot: 2,
            value: 0x12,
            used: true,
        },
        MbrAnomaly::MultipleActive {
            lba: 0,
            slots: vec![0, 1],
        },
        MbrAnomaly::MultipleExtended {
            lba: 0,
            slots: vec![2, 3],
        },
        MbrAnomaly::ZeroLength {
            lba: 0,
            slot: 1,
            kind: 7,
        },
        MbrAnomaly::OutsideDisk {
            lba: 0,
            origin: Origin::Primary { slot: 3 },
            start: u64::MAX,
        },
        MbrAnomaly::PartiallyOutsideDisk {
            lba: 0,
            origin: o,
            end: 9,
            disk_sectors: 8,
        },
        MbrAnomaly::Overlap {
            lba: 0,
            first: o,
            second: Origin::Gpt { index: u32::MAX },
        },
        MbrAnomaly::UnknownType {
            lba: 0,
            origin: o,
            kind: 0xFF,
        },
        MbrAnomaly::LogicalOutsideExtended { lba: 5, origin: o },
        MbrAnomaly::ExtraDataEntries { lba: 5, count: 2 },
        MbrAnomaly::EbrLoop { lba: 5, target: 4 },
        MbrAnomaly::LinkOutsideDisk { lba: 5, target: 99 },
        MbrAnomaly::LinkOutsideExtended { lba: 5, target: 98 },
        MbrAnomaly::ChainLimit {
            lba: 5,
            limit: 1024,
        },
        MbrAnomaly::ProtectiveStart { lba: 0, start: 2 },
        MbrAnomaly::ProtectiveSizeMismatch {
            lba: 0,
            declared: 1,
            expected: 2,
        },
        MbrAnomaly::Hybrid { lba: 0 },
        MbrAnomaly::HybridMismatch { lba: 0, slot: 1 },
    ];
    let c = GptCopy::Backup;
    let gpt = vec![
        GptAnomaly::HeaderMissing { copy: c, lba: 1 },
        GptAnomaly::HeaderOutsideImage {
            copy: c,
            disk_lba: 2,
        },
        GptAnomaly::HeaderCrc {
            copy: c,
            lba: 1,
            stored: 1,
            computed: u32::MAX,
        },
        GptAnomaly::HeaderSize {
            copy: c,
            lba: 1,
            size: 9,
        },
        GptAnomaly::RevisionUnsupported {
            copy: c,
            lba: 1,
            revision: 2,
        },
        GptAnomaly::RevisionMinor {
            copy: c,
            lba: 1,
            revision: 3,
        },
        GptAnomaly::MyLbaMismatch {
            copy: c,
            lba: 1,
            my_lba: 4,
            expected: 5,
        },
        GptAnomaly::AlternateLbaMismatch {
            copy: c,
            lba: 1,
            alternate_lba: 6,
        },
        GptAnomaly::UsableRangeInvalid {
            copy: c,
            lba: 1,
            first: 7,
            last: 6,
        },
        GptAnomaly::EntriesLocationInvalid {
            copy: c,
            lba: 1,
            entries_lba: 8,
        },
        GptAnomaly::EntryArrayLimits {
            copy: c,
            lba: 1,
            entry_count: 9,
            entry_size: 10,
        },
        GptAnomaly::EntriesCrc {
            copy: GptCopy::Primary,
            lba: 2,
            stored: 3,
            computed: 4,
        },
        GptAnomaly::EntriesOutsideImage {
            copy: c,
            disk_lba: 11,
        },
        GptAnomaly::BackupInUse {
            primary_header: HeaderState::Unreadable,
            primary_entries: EntriesState::Unchecked,
        },
        GptAnomaly::CopiesDisagree {
            field: GptField::Location,
        },
        GptAnomaly::ImageShifted { offset: i64::MIN },
        GptAnomaly::ImageTruncated { missing: 12 },
        GptAnomaly::UsableRangeOutsideImage {
            lba: 1,
            first: 2,
            last: 3,
        },
        GptAnomaly::SectorSizeMismatch {
            device: 512,
            used: 4096,
        },
        GptAnomaly::EntryInverted {
            index: 1,
            first: 5,
            last: 4,
        },
        GptAnomaly::EntryOutsideUsable { index: 1 },
        GptAnomaly::EntryOutsideImage { index: 2 },
        GptAnomaly::EntryOverlap { index: 1, other: 2 },
        GptAnomaly::DuplicateUniqueGuid { index: 1, other: 2 },
        GptAnomaly::NameInvalid { index: 3 },
    ];
    let mut anomalies: Vec<Anomaly> = mbr.into_iter().map(Anomaly::Mbr).collect();
    anomalies.extend(gpt.into_iter().map(Anomaly::Gpt));
    anomalies.push(Anomaly::UnreadableSector { lba: u64::MAX });
    let part = |origin, kind, confidence| Partition {
        origin,
        start_lba: 1,
        sector_count: 2,
        kind,
        name: Some("\"comillas\" \\ \u{1}\u{7f} \u{fffd} 😀".into()),
        active: true,
        confidence,
        anomalies: anomalies[..3].to_vec(),
    };
    let partitions = vec![
        part(
            Origin::Primary { slot: 0 },
            PartitionKind::Mbr(0),
            Confidence::Valid,
        ),
        part(
            o,
            PartitionKind::Mbr(0xFF),
            Confidence::ValidWithFixes(vec!["a".into(), String::new()]),
        ),
        part(
            Origin::Gpt { index: 0 },
            PartitionKind::Gpt([0xFF; 16]),
            Confidence::Suspicious(vec![]),
        ),
        part(
            Origin::Gpt { index: 1 },
            PartitionKind::Gpt(BASIC_DATA),
            Confidence::Invalid("x".into()),
        ),
    ];
    let d = dev(gpt_disk());
    let mut b = capture(&d, "todas las variantes", None).unwrap();
    b.layout.partitions = partitions;
    b.layout.anomalies = anomalies;
    let vols = [
        VolumeAnomaly::PrimaryDamaged { byte: -1 },
        VolumeAnomaly::PrimaryUnreadable { lba: 1 },
        VolumeAnomaly::PrimaryOutsideImage { byte: -2 },
        VolumeAnomaly::BackupMissing {
            byte: 3,
            compensated: true,
        },
        VolumeAnomaly::BackupUnreadable {
            lba: 4,
            compensated: false,
        },
        VolumeAnomaly::BackupOutsideImage {
            byte: 5,
            compensated: true,
        },
        VolumeAnomaly::BackupMismatch { byte: 6 },
        VolumeAnomaly::BeyondImageEnd { missing_bytes: 7 },
        VolumeAnomaly::SectorSizeMismatch {
            fs: 4096,
            device: 512,
        },
        VolumeAnomaly::Unaligned { byte: 9 },
        VolumeAnomaly::ShiftDetected { delta_bytes: -10 },
        VolumeAnomaly::SelfLocationMismatch {
            declared_byte: 11,
            actual_byte: 12,
        },
        VolumeAnomaly::Overlap { other_start: 13 },
        VolumeAnomaly::Unconfirmed,
    ];
    let checks = [
        Check::Yes,
        Check::No,
        Check::Unverifiable,
        Check::Outside,
        Check::NotApplicable,
    ];
    let fss = [
        FsKind::Ntfs,
        FsKind::Fat12,
        FsKind::Fat16,
        FsKind::Fat32,
        FsKind::ExFat,
    ];
    let volumes: Vec<FoundVolume> = Tier::ALL
        .iter()
        .enumerate()
        .map(|(i, &tier)| FoundVolume {
            fs: fss[i],
            start_byte: -512 + i as i64,
            length_bytes: 1 << 40,
            fs_sector_size: 4096,
            primary: checks[i],
            backup: checks[(i + 1) % 5],
            structures: checks[(i + 2) % 5],
            entry: checks[(i + 3) % 5],
            self_location: checks[(i + 4) % 5],
            aligned: checks[i],
            primary_byte: -512,
            backup_byte: (i % 2 == 0).then_some(1),
            declared_start: u64::MAX,
            serial: u64::MAX,
            tier,
            origin: format!("origen {i}"),
            table_partition: (i % 2 == 1).then_some(i),
            found_table: (i % 2 == 0).then_some(0),
            confidence: Confidence::Suspicious(vec!["s".into()]),
            anomalies: vols.to_vec(),
            score: u32::MAX,
        })
        .collect();
    let opts = SearchOptions {
        max_reads: 1,
        ..SearchOptions::default()
    };
    for status in [
        SearchStatus::Complete,
        SearchStatus::BudgetExhausted,
        SearchStatus::Cancelled,
    ] {
        b.search = Some(SearchResult {
            status,
            sector_size: 512,
            sectors: 4096,
            volumes: volumes.clone(),
            tables: vec![
                FoundTable {
                    kind: FoundTableKind::Mbr,
                    lba: 63,
                    declared_lba: 0,
                    entries: vec![FoundEntry {
                        start_byte: -1,
                        length_bytes: 2,
                        mbr_type: 5,
                        extended: true,
                    }],
                },
                FoundTable {
                    kind: FoundTableKind::Gpt,
                    lba: 64,
                    declared_lba: 1,
                    entries: vec![],
                },
            ],
            anomalies: vec![
                SearchAnomaly::ImageShifted {
                    delta_bytes: -512,
                    sector_size: 512,
                    evidence: 2,
                },
                SearchAnomaly::MbrDisplaced { lba: 1 },
            ],
            shift_bytes: Some(-512),
            stats: Default::default(),
            probed: 1,
            pending: [1, 2, 3, 4, u64::MAX],
            volume_limit_reached: true,
            options: opts,
        });
        round_trip(&b);
    }
}

// ---------------------------------------------------------------------------------------------
// Manipulaciones
// ---------------------------------------------------------------------------------------------

/// Vuelve a calcular `body_sha256` de un documento editado (simula una edición que también
/// actualiza la suma del cuerpo).
fn refresh_body_sum(text: &str) -> String {
    let doc = json::parse(text).unwrap();
    let Json::Obj(mut m) = doc else {
        panic!("se esperaba un objeto")
    };
    let body = m.iter().find(|(k, _)| k == "body").unwrap().1.clone();
    for (k, v) in m.iter_mut() {
        if k == "body_sha256" {
            *v = Json::Str(body_sha256(&body));
        }
    }
    let mut out = String::new();
    json::write_pretty(&mut out, &Json::Obj(m), 0);
    out
}

fn mbr_text() -> String {
    capture(&dev(mbr_disk()), "", None).unwrap().to_text()
}

/// Cambia el primer dígito hexadecimal que sigue a `marker`.
fn flip_after(text: &str, marker: &str) -> String {
    let at = text.find(marker).unwrap() + marker.len();
    let mut s = text.to_string();
    let c = if &text[at..at + 1] == "0" { "1" } else { "0" };
    s.replace_range(at..at + 1, c);
    s
}

#[test]
fn edited_region_bytes_are_detected() {
    let text = mbr_text();
    let edited = flip_after(&text, "\"data\": [\n          \"");
    assert_ne!(edited, text);
    // Sin tocar la suma del cuerpo: la detecta el cuerpo.
    match LayoutBackup::from_text(&edited) {
        Err(BackupError::Integrity(m)) => assert!(m.contains("body_sha256"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Aunque se recalcule la suma del cuerpo: la detecta la SHA-256 de la región.
    match LayoutBackup::from_text(&refresh_body_sum(&edited)) {
        Err(BackupError::Integrity(m)) => assert!(m.contains("regions[0]"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn edited_checksums_are_detected() {
    let text = mbr_text();
    let edited = flip_after(&text, "\"sha256\": \"");
    assert!(matches!(
        LayoutBackup::from_text(&edited),
        Err(BackupError::Integrity(_))
    ));
    assert!(matches!(
        LayoutBackup::from_text(&refresh_body_sum(&edited)),
        Err(BackupError::Integrity(m)) if m.contains("regions[0]")
    ));
    // La suma del cuerpo editada.
    let edited = flip_after(&text, "\"body_sha256\": \"");
    assert!(matches!(
        LayoutBackup::from_text(&edited),
        Err(BackupError::Integrity(_))
    ));
    // Un valor del particionado editado (inicio de una partición).
    let edited = text.replacen("\"start_lba\": 2048", "\"start_lba\": 2049", 1);
    assert_ne!(edited, text);
    assert!(matches!(
        LayoutBackup::from_text(&edited),
        Err(BackupError::Integrity(_))
    ));
}

#[test]
fn whitespace_changes_keep_the_backup_valid() {
    let text = mbr_text();
    let mut compact = String::new();
    json::write_compact(&mut compact, &json::parse(&text).unwrap());
    assert_eq!(
        LayoutBackup::from_text(&compact).unwrap(),
        LayoutBackup::from_text(&text).unwrap()
    );
    let crlf = text.replace('\n', "\r\n");
    assert!(LayoutBackup::from_text(&crlf).is_ok());
}

#[test]
fn wrong_format_or_version_is_rejected() {
    let text = mbr_text();
    for (from, to) in [
        ("\"version\": 1,", "\"version\": 2,"),
        ("\"version\": 1,", "\"version\": 0,"),
        ("\"version\": 1,", "\"version\": \"1\","),
    ] {
        let edited = text.replacen(from, to, 1);
        assert_ne!(edited, text);
        assert!(matches!(
            LayoutBackup::from_text(&edited),
            Err(BackupError::UnsupportedVersion(_))
        ));
    }
    assert_eq!(
        LayoutBackup::from_text(&text.replacen(
            "memrescue-partition-backup",
            "memrescue-partition-backup2",
            1
        )),
        Err(BackupError::NotABackup)
    );
    assert_eq!(LayoutBackup::from_text("{}"), Err(BackupError::NotABackup));
    assert!(matches!(
        LayoutBackup::from_text("[]"),
        Err(BackupError::Invalid { .. })
    ));
}

#[test]
fn truncated_files_are_rejected() {
    let text = mbr_text();
    let bytes = text.as_bytes();
    // Cortes en todo el documento: ninguno es válido (termina en "}\n").
    let mut cut = 0;
    while cut < bytes.len() - 2 {
        assert!(
            LayoutBackup::from_bytes(&bytes[..cut]).is_err(),
            "corte en {cut} aceptado"
        );
        cut += if cut < 2000 { 1 } else { 97 };
    }
    assert!(LayoutBackup::from_bytes(&bytes[..bytes.len() - 1]).is_ok());
    assert_eq!(
        LayoutBackup::from_bytes(&[0xFF, 0xFE]),
        Err(BackupError::Encoding)
    );
}

#[test]
fn unknown_missing_or_out_of_range_fields_are_rejected() {
    let text = mbr_text();
    let cases = [
        // Campo desconocido.
        text.replacen("\"created_by\":", "\"extra\": 1, \"created_by\":", 1),
        // Campo ausente.
        text.replacen("\"active\": true,", "", 1),
        // Fuera de rango para el tipo.
        text.replacen("\"slot\": 0", "\"slot\": 256", 1),
        // Tipo distinto.
        text.replacen("\"active\": true", "\"active\": 1", 1),
        // Variante desconocida.
        text.replacen("\"type\": \"Primary\"", "\"type\": \"Primaria\"", 1),
        // Región fuera del origen.
        text.replacen("\"offset\": 3145728", "\"offset\": 4194304", 1),
        // Identidad incoherente.
        text.replacen("\"sector_count\": 8192", "\"sector_count\": 8191", 1),
    ];
    for (i, edited) in cases.iter().enumerate() {
        assert_ne!(edited, &text, "caso {i}");
        let r = LayoutBackup::from_text(&refresh_body_sum(edited));
        assert!(
            matches!(r, Err(BackupError::Invalid { .. })),
            "caso {i}: {r:?}"
        );
    }
    // Código de anomalía que no corresponde.
    let d = FaultyDevice::new(dev(gpt_disk())).with_bad([1]);
    let t = capture(&d, "", None).unwrap().to_text();
    let edited = t.replacen("\"code\": \"unreadable_sector\"", "\"code\": \"otro\"", 1);
    assert_ne!(edited, t);
    assert!(matches!(
        LayoutBackup::from_text(&refresh_body_sum(&edited)),
        Err(BackupError::Invalid { .. })
    ));
}

#[test]
fn oversized_files_are_rejected_before_parsing() {
    let big = vec![b' '; MAX_FILE_BYTES as usize + 1];
    assert!(matches!(
        LayoutBackup::from_bytes(&big),
        Err(BackupError::TooLarge { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// Comparación
// ---------------------------------------------------------------------------------------------

#[test]
fn comparison_reports_changed_mbr_entry_and_ebr() {
    let b = capture(&dev(mbr_disk()), "", None).unwrap();
    // Entrada primaria 0 movida y el segundo EBR borrado.
    let mut d = mbr_disk();
    d[446 + 8..446 + 12].copy_from_slice(&2049u32.to_le_bytes());
    d[6144 * SS..6145 * SS].fill(0);
    let c = compare(&b, &dev(d)).unwrap();
    assert!(!c.identical());
    let regions: Vec<(RegionKind, u64)> = c
        .differences
        .iter()
        .filter_map(|x| match x {
            Difference::Region { kind, offset, .. } => Some((*kind, *offset)),
            _ => None,
        })
        .collect();
    assert_eq!(
        regions,
        vec![(RegionKind::Mbr, 0), (RegionKind::Ebr, 6144 * 512)]
    );
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::PartitionChanged { fields, current, .. }
            if fields.contains(&"inicio") && current.start_lba == 2049
    )));
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::PartitionRemoved { .. })));
    for x in &c.differences {
        assert!(!x.to_string().is_empty() && !x.code().is_empty());
    }
}

#[test]
fn comparison_reports_damaged_gpt_copy_and_size() {
    let b = capture(&dev(gpt_disk()), "", None).unwrap();
    let mut d = gpt_disk();
    d[2 * SS + 40] ^= 1; // entradas primarias: CRC-32 incorrecta
    let c = compare(&b, &dev(d)).unwrap();
    assert!(c.differences.contains(&Difference::GptSelected {
        backup: Some(GptCopy::Primary),
        current: Some(GptCopy::Backup)
    }));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::GptPrimaryEntries,
            backup: RegionStatus::Data(_),
            current: RegionStatus::Data(_),
            ..
        }
    )));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::AnomalyAdded(Anomaly::Gpt(GptAnomaly::EntriesCrc { .. }))
    )));
    // Origen recortado: tamaño distinto y copia de respaldo fuera del origen.
    let mut d = gpt_disk();
    d.truncate(2048 * SS);
    let c = compare(&b, &dev(d)).unwrap();
    assert!(c.differences.contains(&Difference::SourceSize {
        backup: 4096 * 512,
        current: 2048 * 512
    }));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::GptBackupHeader,
            current: RegionStatus::Absent,
            ..
        }
    )));
    // Un MBR en lugar de la GPT.
    let c = compare(&b, &dev(mbr_disk())).unwrap();
    assert!(c.differences.contains(&Difference::Scheme {
        backup: Scheme::Gpt,
        current: Scheme::Mbr
    }));
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::PartitionAdded { .. })));
}

// ---------------------------------------------------------------------------------------------
// Fuzzing del lector
// ---------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn seeded_fuzz_never_panics() {
    let seeds = [
        mbr_text().into_bytes(),
        capture(&FaultyDevice::new(dev(gpt_disk())).with_bad([1]), "", None)
            .unwrap()
            .to_text()
            .into_bytes(),
    ];
    let tokens: [&[u8]; 12] = [
        b"{",
        b"}",
        b"[",
        b"]",
        b"\"",
        b",",
        b":",
        b"null",
        b"-1",
        b"99999999999999999999",
        b"\\u",
        b"\"\\ud800\"",
    ];
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut accepted = 0;
    for i in 0..1500 {
        let mut d = seeds[i % 2].clone();
        for _ in 0..1 + rng.below(4) {
            if d.is_empty() {
                break;
            }
            let at = rng.below(d.len());
            match rng.below(6) {
                0 => d[at] = rng.next() as u8,
                1 => d[at] ^= 1 << rng.below(8),
                2 => {
                    d.remove(at);
                }
                3 => d.truncate(at),
                4 => {
                    let t = tokens[rng.below(tokens.len())];
                    d.splice(at..at, t.iter().copied());
                }
                _ => {
                    let len = rng.below(64).min(d.len() - at);
                    let chunk = d[at..at + len].to_vec();
                    let to = rng.below(d.len());
                    d.splice(to..to, chunk);
                }
            }
        }
        if LayoutBackup::from_bytes(&d).is_ok() {
            accepted += 1;
        }
    }
    // Solo pasan mutaciones que no cambian el contenido (espacios, final del fichero).
    assert!(accepted < 150, "{accepted}");
    for _ in 0..500 {
        let len = rng.below(512);
        let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = LayoutBackup::from_bytes(&d);
    }
}

// ---------------------------------------------------------------------------------------------
// Apple Partition Map (#17)
// ---------------------------------------------------------------------------------------------

/// Disco APM de 256 bloques de 512 B: DDM y mapa de 3 entradas (mapa, `Apple_HFS` en [64, 128) y
/// `Apple_Free` en [128, 256)).
fn apm_disk() -> Vec<u8> {
    let mut d = vec![0u8; 256 * SS];
    d[..2].copy_from_slice(b"ER");
    d[2..4].copy_from_slice(&512u16.to_be_bytes());
    d[4..8].copy_from_slice(&256u32.to_be_bytes());
    let entries: [(u32, u32, &str, &str, u32); 3] = [
        (1, 63, "Apple_partition_map", "Apple", 0x37),
        (64, 64, "Apple_HFS", "MR-HFS", 0x4000_0033),
        (128, 128, "Apple_Free", "Extra", 0),
    ];
    for (i, (start, size, kind, name, status)) in entries.iter().enumerate() {
        let e = &mut d[(i + 1) * SS..(i + 2) * SS];
        e[..2].copy_from_slice(b"PM");
        e[4..8].copy_from_slice(&3u32.to_be_bytes());
        e[8..12].copy_from_slice(&start.to_be_bytes());
        e[12..16].copy_from_slice(&size.to_be_bytes());
        e[16..16 + name.len()].copy_from_slice(name.as_bytes());
        e[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        e[84..88].copy_from_slice(&size.to_be_bytes());
        e[88..92].copy_from_slice(&status.to_be_bytes());
    }
    d
}

#[test]
fn apm_round_trips_with_its_map() {
    let d = dev(apm_disk());
    let b = capture(&d, "disco APM sintético", None).unwrap();
    assert_eq!(b.layout.scheme, Scheme::Apm);
    assert_eq!(b.layout.partitions.len(), 3);
    assert!(b.layout.apm.is_some());
    assert_eq!(
        kinds(&b),
        vec![
            (RegionKind::Mbr, 0, 512),
            (RegionKind::ApmMap, 512, 3 * 512)
        ]
    );
    round_trip(&b);
    let text = b.to_text();
    assert!(text.contains("\"scheme\": \"apm\""), "{text}");
    assert!(text.contains("\"kind\": \"apm_map\""));
    assert!(compare(&b, &d).unwrap().identical());
    // Una copia sin APM no lleva el campo `apm` (mismo documento que antes de #17).
    assert!(!mbr_text().contains("\"apm\""));
}

#[test]
fn every_apm_variant_round_trips() {
    let mut b = capture(&dev(apm_disk()), "", None).unwrap();
    let all = vec![
        ApmAnomaly::DdmMissing,
        ApmAnomaly::BlockSizeInvalid { declared: 1000 },
        ApmAnomaly::BlockSizeMismatch {
            declared: 2048,
            used: 512,
        },
        ApmAnomaly::DiskSizeMismatch {
            declared: u64::MAX,
            available: 1,
        },
        ApmAnomaly::MapMissing { lba: 1 },
        ApmAnomaly::MapCountInvalid {
            lba: 1,
            count: u32::MAX,
        },
        ApmAnomaly::MapCountMismatch {
            lba: 2,
            index: 2,
            count: 0,
        },
        ApmAnomaly::MapOutsideImage { index: 9 },
        ApmAnomaly::EntrySignature { lba: 3, index: 3 },
        ApmAnomaly::EntryZeroLength { lba: 4, index: 4 },
        ApmAnomaly::EntryOutsideDisk {
            lba: 5,
            index: 5,
            start: u64::MAX,
        },
        ApmAnomaly::EntryPartiallyOutsideDisk {
            lba: 6,
            index: 6,
            end: 7,
        },
        ApmAnomaly::EntryOverlap {
            lba: 7,
            index: 7,
            other: 2,
        },
        ApmAnomaly::EntryDataOutside { lba: 8, index: 8 },
        ApmAnomaly::SectorSizeMismatch {
            block_size: 2048,
            device: 512,
        },
    ];
    let all: Vec<Anomaly> = all.into_iter().map(Anomaly::Apm).collect();
    b.layout.anomalies = all.clone();
    b.layout.partitions[1].anomalies = all.clone();
    // Tipo con bytes tras el NUL: se conserva sin pérdidas.
    let mut raw = [0xFFu8; 32];
    raw[..4].copy_from_slice(b"X\0Y\x01");
    b.layout.partitions[1].kind = PartitionKind::Apm(crate::apm::ApmType(raw));
    let a = b.layout.apm.as_mut().unwrap();
    a.anomalies = all;
    a.partitions[0].status = u32::MAX;
    round_trip(&b);
}

#[test]
fn apm_field_must_match_the_scheme() {
    let b = capture(&dev(apm_disk()), "", None).unwrap();
    let text = b.to_text();
    // Esquema apm sin lectura APM.
    let mut no_apm = b.clone();
    no_apm.layout.apm = None;
    let edited = refresh_body_sum(&no_apm.to_text());
    match LayoutBackup::from_text(&edited) {
        Err(BackupError::Invalid { path, .. }) => assert!(path.ends_with("layout"), "{path}"),
        other => panic!("{other:?}"),
    }
    // Lectura APM con otro esquema.
    let edited = refresh_body_sum(&text.replacen("\"scheme\": \"apm\"", "\"scheme\": \"none\"", 1));
    assert!(matches!(
        LayoutBackup::from_text(&edited),
        Err(BackupError::Invalid { .. })
    ));
    // `apm: null` explícito en una copia sin APM se acepta (equivale a ausente).
    let mbr = mbr_text();
    let edited =
        refresh_body_sum(&mbr.replacen("\"gpt\": null", "\"gpt\": null,\n    \"apm\": null", 1));
    assert_ne!(edited, mbr);
    assert_eq!(
        LayoutBackup::from_text(&edited).unwrap(),
        LayoutBackup::from_text(&mbr).unwrap()
    );
}

#[test]
fn comparison_reports_changed_apm_entry_and_map() {
    let b = capture(&dev(apm_disk()), "", None).unwrap();
    let mut d = apm_disk();
    // Entrada 2: inicio movido y estado cambiado.
    d[2 * SS + 8..2 * SS + 12].copy_from_slice(&65u32.to_be_bytes());
    d[2 * SS + 88..2 * SS + 92].copy_from_slice(&0u32.to_be_bytes());
    let c = compare(&b, &dev(d)).unwrap();
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::Region {
            kind: RegionKind::ApmMap,
            offset: 512,
            ..
        }
    )));
    assert!(c.differences.iter().any(|x| matches!(
        x,
        Difference::PartitionChanged { fields, .. }
            if fields.contains(&"inicio") && fields.contains(&"estado APM")
    )));
    // Mapa borrado (queda el DDM): las particiones desaparecen y aparece `apm_map_missing`.
    let mut d = apm_disk();
    d[SS..4 * SS].fill(0);
    let c = compare(&b, &dev(d)).unwrap();
    let removed = c
        .differences
        .iter()
        .filter(|x| matches!(x, Difference::PartitionRemoved { .. }))
        .count();
    assert_eq!(removed, 3);
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::AnomalyAdded(a) if a.code() == "apm_map_missing")));
    // Sin DDM ni mapa ya no hay APM: cambia el esquema.
    let mut d = apm_disk();
    d[..4 * SS].fill(0);
    let c = compare(&b, &dev(d)).unwrap();
    assert!(c
        .differences
        .iter()
        .any(|x| matches!(x, Difference::Scheme { .. })));
    for x in &c.differences {
        assert!(!x.to_string().is_empty() && !x.code().is_empty());
    }
}
