//! Pruebas de apertura de volúmenes sintéticos (`crate::testvol`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use mr_io::{FaultyDevice, MemDevice};

use super::*;
use crate::record::{apply_fixups, FixupError, RecordDamage};
use crate::testvol::{protect, xorshift, Synth, SYNTH_INITIALIZED, SYNTH_RECORDS};

fn open(img: Vec<u8>) -> Result<NtfsVolume, NtfsError> {
    NtfsVolume::open(Arc::new(MemDevice::new(img, 512).unwrap()))
}

fn open_faulty(img: Vec<u8>, bad: Vec<u64>) -> Result<NtfsVolume, NtfsError> {
    NtfsVolume::open(Arc::new(
        FaultyDevice::new(MemDevice::new(img, 512).unwrap()).with_bad(bad),
    ))
}

fn states(v: &NtfsVolume) -> Vec<RecordState> {
    v.records().map(|r| r.unwrap().1.state()).collect()
}

fn codes(v: &NtfsVolume) -> Vec<&'static str> {
    v.info().anomalies.iter().map(|a| a.code()).collect()
}

/// Estados esperados del volumen sintético.
fn expected_states() -> Vec<RecordState> {
    use RecordState::*;
    (0..SYNTH_RECORDS)
        .map(|n| match n {
            0..=11 | 16 | 17 | 20 => InUse,
            12..=15 | 21..=23 => Empty,
            18 | 19 | 24..=27 => NotInUse,
            _ => Uninitialized,
        })
        .collect()
}

/// Edita un registro de la imagen (deshace la protección, aplica `edit` y vuelve a proteger).
fn edit_record(s: &mut Synth, n: u64, edit: impl FnOnce(&mut Vec<u8>)) {
    let mut raw = Vec::new();
    for &(d, l) in &s.pieces[n as usize] {
        raw.extend_from_slice(&s.image[d as usize..(d + l) as usize]);
    }
    apply_fixups(&mut raw).unwrap();
    edit(&mut raw);
    protect(&mut raw);
    let mut at = 0usize;
    for &(d, l) in &s.pieces[n as usize] {
        s.image[d as usize..(d + l) as usize].copy_from_slice(&raw[at..at + l as usize]);
        at += l as usize;
    }
}

#[test]
fn opens_a_healthy_volume_for_each_cluster_size() {
    for spc in [1u8, 2, 8] {
        let s = Synth::new(spc);
        let v = open(s.image.clone()).unwrap();
        let i = v.info();
        assert_eq!(i.boot.cluster_size as u64, s.cluster, "spc {spc}");
        assert_eq!(i.boot.serial, 0x0123_4567_89AB_CDEF);
        assert_eq!(i.boot_source, BootSource::Primary);
        assert_eq!(i.primary_boot, BootProbe::Valid);
        assert_eq!(
            i.backup_boot,
            BackupBoot {
                offset: s.total_sectors * 512,
                state: BackupState::Matches
            }
        );
        assert_eq!(i.mft.runs.len(), 2);
        assert_eq!(i.mft.runs[0].lcn, Some(s.mft_lcn));
        assert_eq!(i.mft.record_count, SYNTH_RECORDS);
        assert_eq!(i.mft.initialized_size, SYNTH_INITIALIZED * 1024);
        assert_eq!(i.mft_record0_source, RecordSource::Mft);
        assert_eq!((i.mirror.lcn, i.mirror.records), (s.mirr_lcn, 4));
        assert!(i.mirror.mismatches.is_empty());
        assert_eq!(i.label.as_deref(), Some("SINTÉTICO"));
        assert_eq!(i.version, Some((3, 1)));
        assert!(i.anomalies.is_empty(), "{:?}", i.anomalies);
        assert_eq!(i.confidence(), Confidence::Valid);
        // Enumeración por lotes = lectura registro a registro.
        assert_eq!(states(&v), expected_states(), "spc {spc}");
        for n in 0..SYNTH_RECORDS {
            let one = v.read_record(n).unwrap();
            let it = v.records_in(n..n + 1).next().unwrap().unwrap();
            assert_eq!(it, (n, one), "registro {n}");
        }
        let r18 = v.read_record(18).unwrap().into_record().unwrap();
        assert_eq!((r18.header().sequence, r18.in_use()), (2, false));
        let r19 = v.read_record(19).unwrap().into_record().unwrap();
        assert!(r19.is_directory() && !r19.in_use());
        assert_eq!(
            v.read_record(20)
                .unwrap()
                .record()
                .unwrap()
                .header()
                .sequence,
            5
        );
        assert!(matches!(
            v.read_record(SYNTH_RECORDS),
            Err(NtfsError::RecordOutOfRange {
                number: 32,
                count: 32
            })
        ));
        assert_eq!(v.records_in(30..100).count(), 2);
        assert_eq!(v.records_in(40..50).count(), 0);
    }
}

#[test]
fn sector_size_of_the_source_differs_from_the_boot() {
    // Los mismos bytes vistos con sectores de 4096 B: varios registros por sector.
    let s = Synth::new(2);
    let v = NtfsVolume::open(Arc::new(MemDevice::new(s.image.clone(), 4096).unwrap())).unwrap();
    assert_eq!(codes(&v), vec!["sector_size_mismatch"]);
    assert!(matches!(v.info().confidence(), Confidence::Suspicious(_)));
    assert_eq!(states(&v), expected_states());
}

#[test]
fn damaged_primary_boot_opens_from_the_backup() {
    let s = Synth::new(2);
    let mut img = s.image.clone();
    img[..512].fill(0x5A);
    let v = open(img).unwrap();
    let i = v.info();
    let backup = s.total_sectors * 512;
    assert_eq!(i.boot_source, BootSource::Backup { offset: backup });
    assert_eq!(i.backup_boot.state, BackupState::InUse);
    assert_eq!(i.primary_boot, BootProbe::Invalid(BootError::NoOemId));
    assert_eq!(codes(&v), vec!["boot_primary_invalid"]);
    assert!(matches!(i.confidence(), Confidence::ValidWithFixes(_)));
    assert_eq!(states(&v), expected_states());
    // Arranque principal ilegible: igual, y su relleno nunca se interpreta.
    let v = open_faulty(s.image.clone(), vec![0]).unwrap();
    assert_eq!(codes(&v), vec!["boot_primary_unreadable"]);
    assert_eq!(
        v.info().primary_boot,
        BootProbe::Unreadable { lbas: vec![0] }
    );
    // Copia un sector más allá (origen con un sector de más al final).
    let mut img = s.image.clone();
    let copy = img[backup as usize..].to_vec();
    img[..512].fill(0);
    img.extend_from_slice(&copy);
    img[backup as usize..backup as usize + 512].fill(0);
    let v = open(img).unwrap();
    assert_eq!(
        v.info().boot_source,
        BootSource::Backup {
            offset: backup + 512
        }
    );
    assert_eq!(
        codes(&v),
        vec!["boot_primary_invalid", "boot_backup_shifted"]
    );
}

#[test]
fn damaged_or_missing_backup_boot_is_reported() {
    let s = Synth::new(2);
    let backup = (s.total_sectors * 512) as usize;
    let mut img = s.image.clone();
    img[backup + 0x1FE] = 0;
    let v = open(img).unwrap();
    assert_eq!(v.info().boot_source, BootSource::Primary);
    assert_eq!(
        v.info().backup_boot.state,
        BackupState::Invalid(BootError::NoSignature)
    );
    assert_eq!(codes(&v), vec!["boot_backup_invalid"]);
    assert!(matches!(
        v.info().confidence(),
        Confidence::ValidWithFixes(_)
    ));
    // Copia con otro BPB.
    let mut img = s.image.clone();
    img[backup + 0x48] ^= 1;
    let v = open(img).unwrap();
    assert_eq!(codes(&v), vec!["boot_backup_differs"]);
    // Copia ilegible.
    let v = open_faulty(s.image.clone(), vec![s.total_sectors]).unwrap();
    assert_eq!(codes(&v), vec!["boot_backup_unreadable"]);
    // Origen sin el último sector: el volumen está entero pero la copia queda fuera (aviso).
    let v = open(s.image[..backup].to_vec()).unwrap();
    assert_eq!(codes(&v), vec!["boot_backup_outside_source"]);
    assert_eq!(v.info().confidence(), Confidence::Valid);
}

#[test]
fn not_ntfs_is_a_typed_error() {
    let s = Synth::new(2);
    let backup = (s.total_sectors * 512) as usize;
    let mut img = s.image.clone();
    img[..512].fill(0);
    img[backup..backup + 512].fill(0);
    match open(img) {
        Err(NtfsError::NotNtfs { primary, backup }) => {
            assert_eq!(primary, BootProbe::Invalid(BootError::NoOemId));
            assert_eq!(backup, BootProbe::Invalid(BootError::NoOemId));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(open(vec![0; 512]), Err(NtfsError::NotNtfs { .. })));
    // Una copia válida que no está donde dice su total no se acepta.
    let mut img = s.image.clone();
    img[..512].fill(0);
    img[backup + 0x28] ^= 0x10;
    assert!(matches!(open(img), Err(NtfsError::NotNtfs { .. })));
}

#[test]
fn fixup_mismatch_baad_and_other_damage_mark_only_that_record() {
    let mut s = Synth::new(1);
    // Tramo 1 del registro 17 con un valor distinto del USN (escritura incompleta).
    let at = s.pieces[17][0].0 as usize + 1022;
    s.image[at] ^= 0xFF;
    edit_record(&mut s, 20, |r| r[..4].copy_from_slice(b"BAAD"));
    edit_record(&mut s, 5, |r| {
        r[0x1C..0x20].copy_from_slice(&4096u32.to_le_bytes())
    });
    let v = open(s.image.clone()).unwrap();
    assert!(v.info().anomalies.is_empty());
    let mut want = expected_states();
    for n in [5, 17, 20] {
        want[n] = RecordState::Damaged;
    }
    assert_eq!(states(&v), want);
    assert_eq!(
        v.read_record(17).unwrap(),
        RecordOutcome::Damaged(RecordDamage::Fixup(FixupError::Mismatch { stride: 1 }))
    );
    assert_eq!(
        v.read_record(20).unwrap(),
        RecordOutcome::Damaged(RecordDamage::Baad)
    );
    assert_eq!(v.read_record(5).unwrap().state(), RecordState::Damaged);
    assert!(v.read_record(5).unwrap().detail().unwrap().contains("4096"));
}

#[test]
fn unreadable_mft_sectors_mark_records_unreadable_never_parsed() {
    for spc in [1u8, 8] {
        let s = Synth::new(spc);
        // Un sector del registro 16 y el sector del segundo tramo del registro 15 (con clústeres
        // de 512 B, partido entre los dos runs).
        let r16 = s.record_lbas(16)[1];
        let r15 = *s.record_lbas(15).last().unwrap();
        let v = open_faulty(s.image.clone(), vec![r16, r15]).unwrap();
        assert!(v.info().anomalies.is_empty(), "{:?}", v.info().anomalies);
        let mut want = expected_states();
        want[15] = RecordState::Unreadable;
        want[16] = RecordState::Unreadable;
        assert_eq!(states(&v), want, "spc {spc}");
        assert_eq!(
            v.read_record(16).unwrap(),
            RecordOutcome::Unreadable { lbas: vec![r16] }
        );
        assert_eq!(
            v.read_record(15).unwrap(),
            RecordOutcome::Unreadable { lbas: vec![r15] }
        );
        if spc == 1 {
            assert_eq!(s.pieces[15].len(), 2, "el registro 15 cruza los runs");
        }
    }
}

#[test]
fn record_zero_falls_back_to_the_mirror() {
    let s = Synth::new(2);
    // Registro 0 ilegible: la MFT se localiza con la copia y la discrepancia se informa.
    let v = open_faulty(s.image.clone(), s.record_lbas(0)).unwrap();
    let i = v.info();
    assert_eq!(i.mft_record0_source, RecordSource::Mirror);
    assert_eq!(
        codes(&v),
        vec!["mft_record0_from_mirror", "mftmirr_mismatch"]
    );
    assert_eq!(i.mirror.mismatches[0].record, 0);
    assert_eq!(i.mirror.mismatches[0].kind(), "mft_invalid");
    assert!(matches!(i.confidence(), Confidence::ValidWithFixes(_)));
    let mut want = expected_states();
    want[0] = RecordState::Unreadable;
    assert_eq!(states(&v), want);
    // Registro 0 dañado en las dos copias: no se puede localizar la MFT.
    let mut img = s.image.clone();
    let r0 = s.record_offset(0) as usize;
    img[r0..r0 + 4].copy_from_slice(b"XXXX");
    let m0 = s.mirror_offset as usize;
    img[m0..m0 + 4].copy_from_slice(b"YYYY");
    assert!(matches!(open(img), Err(NtfsError::MftUnusable { .. })));
}

#[test]
fn mirror_disagreement_is_reported_per_record() {
    let s = Synth::new(2);
    let mut img = s.image.clone();
    let m = s.mirror_offset as usize;
    // Copia del registro 2 sobrescrita y copia del registro 3 con un byte distinto en lo usado.
    img[m + 2048..m + 3072].fill(0x77);
    img[m + 3072 + 0x12] ^= 1;
    let v = open(img).unwrap();
    let mm = &v.info().mirror.mismatches;
    assert_eq!(mm.len(), 2);
    assert_eq!((mm[0].record, mm[0].kind()), (2, "mirror_invalid"));
    assert_eq!(mm[0].mirror, RecordState::Damaged);
    assert_eq!((mm[1].record, mm[1].kind()), (3, "differs"));
    assert_eq!(codes(&v), vec!["mftmirr_mismatch", "mftmirr_mismatch"]);
    assert!(matches!(
        v.info().confidence(),
        Confidence::ValidWithFixes(_)
    ));
    // La lectura de la MFT no cambia.
    assert_eq!(states(&v), expected_states());
    // Copia ilegible.
    let v = open_faulty(s.image.clone(), vec![s.mirror_offset / 512 + 2]).unwrap();
    let mm = &v.info().mirror.mismatches;
    assert_eq!(mm.len(), 1);
    assert_eq!((mm[0].record, mm[0].mirror), (1, RecordState::Unreadable));
}

#[test]
fn mft_layout_problems_are_reported() {
    let mut s = Synth::new(2);
    // Tamaño de datos de $MFT no múltiplo del registro y LCN distinto del arranque.
    let off = |r: &Vec<u8>| -> usize {
        // Segundo atributo: $DATA de $MFT.
        let first = u16::from_le_bytes([r[0x14], r[0x15]]) as usize;
        first
            + u32::from_le_bytes([r[first + 4], r[first + 5], r[first + 6], r[first + 7]]) as usize
    };
    edit_record(&mut s, 0, |r| {
        let a = off(r);
        r[a + 0x28..a + 0x30].copy_from_slice(&(33 * 1024u64).to_le_bytes());
        r[a + 0x30..a + 0x38].copy_from_slice(&(32 * 1024 + 100u64).to_le_bytes());
    });
    let v = open(s.image.clone()).unwrap();
    assert_eq!(codes(&v), vec!["mft_size_not_multiple", "mftmirr_mismatch"]);
    assert_eq!(v.record_count(), 32);
    // $DATA de $MFT más grande que sus runs: registros sin run.
    let mut s = Synth::new(2);
    edit_record(&mut s, 0, |r| {
        let a = off(r);
        r[a + 0x28..a + 0x30].copy_from_slice(&(64 * 1024u64).to_le_bytes());
        r[a + 0x30..a + 0x38].copy_from_slice(&(40 * 1024u64).to_le_bytes());
        r[a + 0x38..a + 0x40].copy_from_slice(&(40 * 1024u64).to_le_bytes());
    });
    let v = open(s.image.clone()).unwrap();
    assert_eq!(codes(&v), vec!["mft_incomplete", "mftmirr_mismatch"]);
    assert_eq!(v.record_count(), 40);
    let st = states(&v);
    assert_eq!(
        st[28..32],
        [RecordState::Damaged; 4],
        "basura ya inicializada"
    );
    assert_eq!(st[32..], [RecordState::Unmapped; 8]);
}

#[test]
fn truncated_source_reports_records_outside() {
    // Clústeres de 1 KiB: los registros 16 a 27 están en el segundo run, a partir de 512 KiB.
    let s = Synth::new(2);
    let v = open(s.image[..524_288].to_vec()).unwrap();
    assert_eq!(
        codes(&v),
        vec!["boot_backup_outside_source", "volume_truncated"]
    );
    let mut want = expected_states();
    for st in want.iter_mut().take(28).skip(16) {
        *st = RecordState::OutsideSource;
    }
    assert_eq!(states(&v), want);
}

/// Fuzzing determinista del volumen completo: mutaciones del arranque, la MFT y la copia. Abrir
/// devuelve un volumen o un error tipado, la enumeración termina y nada entra en pánico.
#[test]
fn volume_fuzz_is_bounded() {
    let s = Synth::new(1);
    let mut rng = xorshift(0xF022_0020);
    let regions: Vec<(u64, u64)> = vec![
        (0, 512),
        (s.pieces[0][0].0, 4096),
        (s.pieces[16][0].0, 8192),
        (s.mirror_offset, 4096),
        (s.total_sectors * 512, 512),
    ];
    let start = Instant::now();
    let mut opened = 0;
    for i in 0..400u32 {
        let mut img = s.image.clone();
        let n = 1 + rng() % 16;
        for _ in 0..n {
            let (a, l) = regions[(rng() % regions.len() as u64) as usize];
            let at = (a + rng() % l) as usize;
            img[at] = rng() as u8;
        }
        // Una de cada 4 vueltas reescribe la protección del registro 0 para pasar de los fixups.
        if i.is_multiple_of(4) {
            let r0 = s.pieces[0][0].0 as usize;
            protect(&mut img[r0..r0 + 1024]);
        }
        if let Ok(v) = open(img) {
            opened += 1;
            assert!(v.record_count() <= crate::mft::MAX_RECORDS);
            let mut seen = 0u64;
            for r in v.records_in(0..256) {
                let (_, o) = r.unwrap();
                if let Some(rec) = o.record() {
                    for a in rec.attributes().take(64) {
                        let _ = a.map(|a| a.name());
                    }
                }
                seen += 1;
            }
            assert!(seen <= 256);
            let _ = v.info().confidence();
            for a in &v.info().anomalies {
                assert!(!a.to_string().is_empty());
            }
        }
    }
    assert!(opened > 50, "{opened}");
    assert!(start.elapsed() < Duration::from_secs(60));
}
