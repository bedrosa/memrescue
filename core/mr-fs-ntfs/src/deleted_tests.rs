//! Pruebas de borrados (#23) sobre volúmenes sintéticos (`crate::testvol`): descubrimiento,
//! padres y huérfanos, extensiones, `$Bitmap`, índice de propietarios, clasificación, recuperación
//! con estado por tramo, cancelación y fuzzing.

use std::sync::Arc;

use mr_io::{FaultyDevice, MemDevice};

use super::*;
use crate::attr::{attr_flags, types};
use crate::bitmap::{BitmapIssue, ClusterBitmap};
use crate::lznt1::compress_for_tests;
use crate::record::flags::{DIRECTORY, IN_USE};
use crate::recover::{recover_stream, RecoveryReport};
use crate::stream::RangeState;
use crate::testvol::{
    encode_runs, non_resident, non_resident_ex, record, record_with_base, resident, with_id,
    xorshift, Synth,
};
use crate::values::tests::{file_name_value, list_entry};

const C: u64 = 4096;

fn si() -> Vec<u8> {
    resident(types::STANDARD_INFORMATION, "", &[0x10; 72])
}

fn fname(parent: u64, seq: u16, name: &str) -> Vec<u8> {
    resident(
        types::FILE_NAME,
        "",
        &file_name_value(parent | (u64::from(seq) << 48), name, 1),
    )
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut r = xorshift(seed);
    (0..n).map(|_| r() as u8).collect()
}

fn nr(lcn: u64, clusters: u64, size: u64) -> Vec<u8> {
    non_resident(
        types::DATA,
        "",
        0,
        clusters - 1,
        &encode_runs(&[(Some(lcn), clusters)]),
        clusters * C,
        size,
        size,
    )
}

/// `$Bitmap` (registro 6) residente con los clústeres `allocated` a 1 y `bytes` bytes.
fn bitmap_record(allocated: &[(u64, u64)], bytes: usize) -> Vec<u8> {
    let mut b = vec![0u8; bytes];
    for &(l, n) in allocated {
        for c in l..l + n {
            if let Some(x) = b.get_mut((c / 8) as usize) {
                *x |= 1 << (c % 8);
            }
        }
    }
    record(6, 6, IN_USE, &[si(), resident(types::DATA, "", &b)], 1024)
}

/// Clústeres asignados del volumen base: arranque, MFT, copia, el fichero vivo y 170..172.
const BASE_ALLOCATED: [(u64, u64); 6] = [(0, 1), (16, 4), (128, 4), (64, 1), (160, 4), (170, 2)];

struct Vol {
    s: Synth,
    libre: Vec<u8>,
    pisado: Vec<u8>,
}

/// Volumen con raíz, un directorio vivo, un fichero vivo y borrados de cada tipo (ver cada
/// registro).
fn volume() -> Vol {
    let mut s = Synth::new(8);
    s.put_record(
        5,
        &record(5, 5, IN_USE | DIRECTORY, &[si(), fname(5, 5, ".")], 1024),
    );
    s.put_record(6, &bitmap_record(&BASE_ALLOCATED, 32));
    // Registro libre (no en uso, sin $FILE_NAME).
    s.put_record(11, &record(11, 11, 0, &[si()], 1024));
    // 12: directorio vivo /vivo.
    s.put_record(
        12,
        &record(
            12,
            1,
            IN_USE | DIRECTORY,
            &[si(), fname(5, 5, "vivo")],
            1024,
        ),
    );
    // 13: fichero vivo /vivo/dueño.bin en 160..164.
    s.put_clusters(160, &noise(13, 4 * C as usize));
    s.put_record(
        13,
        &record(
            13,
            1,
            IN_USE,
            &[si(), fname(12, 1, "dueño.bin"), nr(160, 4, 4 * C)],
            1024,
        ),
    );
    // 14: borrado íntegro /vivo/libre.bin en 150..152.
    let libre = noise(14, 8000);
    s.put_clusters(150, &libre);
    s.put_record(
        14,
        &record(
            14,
            2,
            0,
            &[si(), fname(12, 1, "libre.bin"), nr(150, 2, 8000)],
            1024,
        ),
    );
    // 15: borrado /vivo/pisado.bin en 158..162: 158 y 159 libres, 160 y 161 del vivo 13.
    let mut pisado = noise(15, 2 * C as usize);
    s.put_clusters(158, &pisado);
    pisado.extend_from_slice(&s.image[(160 * C) as usize..(160 * C + 5808) as usize]);
    s.put_record(
        15,
        &record(
            15,
            2,
            0,
            &[si(), fname(12, 1, "pisado.bin"), nr(158, 4, 14000)],
            1024,
        ),
    );
    // 16: directorio borrado /carpeta; 17: residente borrado dentro.
    s.put_record(
        16,
        &record(16, 2, DIRECTORY, &[si(), fname(5, 5, "carpeta")], 1024),
    );
    s.put_record(
        17,
        &record(
            17,
            2,
            0,
            &[
                si(),
                fname(16, 1, "dentro.txt"),
                resident(types::DATA, "", b"residente borrado"),
                resident(types::DATA, "alt", b"flujo alternativo"),
            ],
            1024,
        ),
    );
    // 18: borrado en clústeres asignados sin dueño (170..172).
    s.put_record(
        18,
        &record(
            18,
            2,
            0,
            &[si(), fname(5, 5, "asignado.bin"), nr(170, 2, 8192)],
            1024,
        ),
    );
    // 19: huérfano: su padre es el fichero vivo 13. 20: huérfano: directorio reutilizado.
    s.put_record(
        19,
        &record(
            19,
            2,
            0,
            &[
                si(),
                fname(13, 1, "huérfano.txt"),
                resident(types::DATA, "", b"h"),
            ],
            1024,
        ),
    );
    s.put_record(
        20,
        &record(
            20,
            2,
            0,
            &[
                si(),
                fname(12, 7, "reutilizado.txt"),
                resident(types::DATA, "", b"r"),
            ],
            1024,
        ),
    );
    // 21 y 22: borrados que comparten el clúster 181 (libre).
    s.put_record(
        21,
        &record(
            21,
            2,
            0,
            &[si(), fname(5, 5, "compartido_a.bin"), nr(180, 2, 8192)],
            1024,
        ),
    );
    s.put_record(
        22,
        &record(
            22,
            2,
            0,
            &[si(), fname(5, 5, "compartido_b.bin"), nr(181, 2, 8192)],
            1024,
        ),
    );
    // 23: base borrado con $ATTRIBUTE_LIST; su $DATA está en la extensión 24 (borrada, secuencias
    // incrementadas al liberar).
    s.put_clusters(190, &noise(23, 6000));
    let list: Vec<u8> = [
        list_entry(types::STANDARD_INFORMATION, "", 0, 23, 2, 0),
        list_entry(types::FILE_NAME, "", 0, 23, 2, 1),
        list_entry(types::DATA, "", 0, 24, 1, 0),
    ]
    .concat();
    s.put_record(
        23,
        &record(
            23,
            3,
            0,
            &[
                with_id(si(), 0),
                with_id(fname(5, 5, "extendido.bin"), 1),
                with_id(resident(types::ATTRIBUTE_LIST, "", &list), 2),
            ],
            1024,
        ),
    );
    s.put_record(
        24,
        &record_with_base(
            24,
            2,
            0,
            23 | (2 << 48),
            &[with_id(nr(190, 2, 6000), 0)],
            1024,
        ),
    );
    // 25: base borrado sin $ATTRIBUTE_LIST; su $DATA está en la extensión 26, unida por su
    // referencia al base.
    s.put_clusters(194, &noise(25, 3000));
    s.put_record(
        25,
        &record(25, 2, 0, &[si(), fname(5, 5, "suelto.bin")], 1024),
    );
    s.put_record(
        26,
        &record_with_base(26, 2, 0, 25 | (1 << 48), &[nr(194, 1, 3000)], 1024),
    );
    // 27: registro dañado (otra firma).
    let mut bad = record(27, 2, 0, &[si(), fname(5, 5, "dañado.bin")], 1024);
    bad[0..4].copy_from_slice(b"XXXX");
    s.put_record(27, &bad);
    Vol { s, libre, pisado }
}

fn open(img: &[u8]) -> NtfsVolume {
    NtfsVolume::open(Arc::new(MemDevice::new(img.to_vec(), 512).unwrap())).unwrap()
}

fn open_bad(img: &[u8], bad: Vec<u64>) -> NtfsVolume {
    NtfsVolume::open(Arc::new(
        FaultyDevice::new(MemDevice::new(img.to_vec(), 512).unwrap()).with_bad(bad),
    ))
    .unwrap()
}

fn scan(v: &NtfsVolume) -> DeletedScan {
    scan_deleted(v, &ScanOptions::default(), &NoObserver)
        .unwrap()
        .unwrap()
}

fn recover(v: &NtfsVolume, n: u64, name: &str) -> (Vec<u8>, RecoveryReport) {
    let mut out = Vec::new();
    let r = recover_stream(v, n, name, &mut out, &NoObserver)
        .unwrap()
        .unwrap();
    (out, r)
}

#[test]
fn discovery_parents_paths_and_record_states() {
    let vol = volume();
    let v = open(&vol.s.image);
    let s = scan(&v);
    let recs: Vec<u64> = s.entries.iter().map(|e| e.record).collect();
    assert_eq!(recs, vec![14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 25]);
    // Libres: el 11 (sin $FILE_NAME); extensiones no en uso: 24 y 26; dañado: 27.
    assert_eq!(s.free_records, 1);
    assert_eq!(s.loose_extensions, 2);
    assert_eq!(s.problems.len(), 1);
    assert_eq!(
        (s.problems[0].record, s.problems[0].state),
        (27, RecordState::Damaged)
    );
    assert!(s.bitmap_issue.is_none());
    let e = s.entry(14).unwrap();
    assert_eq!((e.sequence, e.name.as_str()), (2, "libre.bin"));
    assert_eq!(
        e.parent,
        FileReference {
            record: 12,
            sequence: 1
        }
    );
    assert_eq!(e.parent_state, ParentState::Live);
    assert_eq!(e.path.as_deref(), Some("/vivo/libre.bin"));
    assert_eq!((e.size, e.allocated_size), (8000, 2 * C));
    assert!(!e.is_directory);
    let d = s.entry(16).unwrap();
    assert!(d.is_directory);
    assert_eq!(d.recoverability(), None);
    assert_eq!(d.path.as_deref(), Some("/carpeta"));
    let e = s.entry(17).unwrap();
    assert_eq!(e.parent_state, ParentState::Deleted);
    assert_eq!(e.path.as_deref(), Some("/carpeta/dentro.txt"));
    assert_eq!(
        s.entry(19).unwrap().parent_state,
        ParentState::Orphan(OrphanReason::NotDirectory)
    );
    let e = s.entry(20).unwrap();
    assert_eq!(
        e.parent_state,
        ParentState::Orphan(OrphanReason::SequenceMismatch { found: 1 })
    );
    assert_eq!(e.path, None);
    assert_eq!(e.display_path(), "<huérfano 12>/reutilizado.txt");
    // Los vivos nunca son borrados.
    for n in [5, 12, 13] {
        assert!(s.entry(n).is_none());
    }
    // El registro dañado nunca se recupera.
    let mut sink = std::io::sink();
    assert!(matches!(
        recover_stream(&v, 27, "", &mut sink, &NoObserver),
        Err(crate::stream::CopyError::Ntfs(NtfsError::RecordUnusable {
            state: RecordState::Damaged,
            ..
        }))
    ));
}

#[test]
fn classification_full_partial_none_and_owners() {
    let vol = volume();
    let v = open(&vol.s.image);
    let s = scan(&v);
    let main = |n: u64| s.entry(n).unwrap().main_stream().unwrap().clone();
    let a = main(14);
    assert_eq!(a.recoverability, Recoverability::Full);
    assert_eq!((a.clusters, a.free), (2, 2));
    assert_eq!(a.reasons, vec![Reason::AllFree { clusters: 2 }]);
    let a = main(15);
    assert_eq!(a.recoverability, Recoverability::Partial);
    assert_eq!((a.clusters, a.free, a.owned), (4, 2, 2));
    assert_eq!(a.owners, vec![13]);
    assert_eq!(
        a.ranges,
        vec![
            ClusterRange {
                vcn: 0,
                lcn: 158,
                length: 2,
                state: ClusterState::Free
            },
            ClusterRange {
                vcn: 2,
                lcn: 160,
                length: 2,
                state: ClusterState::Owned { owner: 13 }
            },
        ]
    );
    assert_eq!(s.owners[&13].path.as_deref(), Some("/vivo/dueño.bin"));
    assert_eq!(
        s.summary(&a),
        "2 de 4 clústeres posiblemente sobrescritos, asignados ahora a /vivo/dueño.bin"
    );
    // Residente: íntegro, también su flujo alternativo.
    let e = s.entry(17).unwrap();
    assert_eq!(e.streams.len(), 2);
    assert!(e
        .streams
        .iter()
        .all(|x| x.recoverability == Recoverability::Full && x.reasons == vec![Reason::Resident]));
    // Asignado en $Bitmap sin dueño conocido: ninguno.
    let a = main(18);
    assert_eq!(a.recoverability, Recoverability::None);
    assert_eq!(a.allocated, 2);
    assert_eq!(a.reasons, vec![Reason::Allocated { clusters: 2 }]);
    // Clúster libre compartido por dos borrados: ninguno de los dos es íntegro.
    let a = main(21);
    assert_eq!(a.recoverability, Recoverability::Partial);
    assert_eq!((a.free, a.shared, a.shared_with.clone()), (1, 1, vec![22]));
    let a = main(22);
    assert_eq!((a.free, a.shared, a.shared_with.clone()), (1, 1, vec![21]));
    // Extensiones: por la lista (con secuencias incrementadas) y por la referencia al base.
    let e = s.entry(23).unwrap();
    assert_eq!(e.extensions, vec![24]);
    assert_eq!(e.size, 6000);
    assert_eq!(e.recoverability(), Some(Recoverability::Full));
    let e = s.entry(25).unwrap();
    assert_eq!(e.backref_extensions, vec![26]);
    assert_eq!(e.size, 3000);
    assert_eq!(e.recoverability(), Some(Recoverability::Full));
}

#[test]
fn recovery_reports_possibly_overwritten_and_unreadable_ranges() {
    let vol = volume();
    let v = open(&vol.s.image);
    let (out, r) = recover(&v, 14, "");
    assert_eq!(out, vol.libre);
    assert!(r.is_clean() && r.suspect.is_empty() && !r.in_use);
    assert_eq!(r.assessment.unwrap().recoverability, Recoverability::Full);
    // Parcial: los bytes son los del disco; el tramo del vivo se informa con su dueño.
    let (out, r) = recover(&v, 15, "");
    assert_eq!(out, vol.pisado);
    assert_eq!(r.copy.count(RangeState::Data), 8192);
    assert_eq!(r.copy.count(RangeState::PossiblyOverwritten), 5808);
    assert_eq!(r.copy.filled_bytes(), 0);
    assert_eq!(r.copy.untrusted.len(), 1);
    assert_eq!(
        (r.copy.untrusted[0].offset, r.copy.untrusted[0].length),
        (8192, 5808)
    );
    assert_eq!(r.suspect.len(), 1);
    let x = r.suspect[0];
    assert_eq!(
        (x.offset, x.length, x.lcn, x.clusters, x.state),
        (8192, 5808, 160, 2, ClusterState::Owned { owner: 13 })
    );
    assert_eq!(r.owners[&13].name.as_deref(), Some("dueño.bin"));
    assert!(r.summary.unwrap().contains("/vivo/dueño.bin"));
    // Flujo alternativo residente y extensión unida por referencia.
    let (out, r) = recover(&v, 17, "alt");
    assert_eq!(out, b"flujo alternativo");
    assert!(r.is_clean());
    let (out, r) = recover(&v, 25, "");
    assert_eq!(out.len(), 3000);
    assert!(r.is_clean());
    // Un registro vivo se copia tal cual, sin evaluación.
    let (out, r) = recover(&v, 13, "");
    assert_eq!(out.len(), 4 * C as usize);
    assert!(r.in_use && r.assessment.is_none() && r.is_clean());
    // Un registro libre no es un borrado.
    let mut sink = std::io::sink();
    assert!(recover_stream(&v, 11, "", &mut sink, &NoObserver).is_err());
    // Sector ilegible en un clúster libre: patrón y tramo `unreadable` con su LBA.
    let lba = 158 * C / 512 + 1;
    let fv = open_bad(&vol.s.image, vec![lba]);
    let (out, r) = recover(&fv, 15, "");
    assert_eq!(r.copy.filled_bytes(), 512);
    assert_eq!(&out[512..516], b"BAD!");
    let un: Vec<_> = r
        .copy
        .untrusted
        .iter()
        .map(|x| (x.offset, x.length, x.state, x.lbas))
        .collect();
    assert_eq!(
        un,
        vec![
            (512, 512, RangeState::Unreadable, Some((lba, 1))),
            (8192, 5808, RangeState::PossiblyOverwritten, None)
        ]
    );
}

#[test]
fn bitmap_unusable_short_or_not_in_use_never_gives_full() {
    // $Bitmap ilegible: el registro 6 cae en sectores malos.
    let vol = volume();
    let fv = open_bad(&vol.s.image, vol.s.record_lbas(6));
    let s = scan(&fv);
    assert!(matches!(s.bitmap_issue, Some(BitmapIssue::Unusable { .. })));
    let a = s.entry(14).unwrap().main_stream().unwrap();
    assert_eq!(a.recoverability, Recoverability::None);
    assert_eq!(a.reasons, vec![Reason::Unknown { clusters: 2 }]);
    // El dueño vivo se sigue nombrando (sale de sus runs, no del mapa).
    let a = s.entry(15).unwrap().main_stream().unwrap();
    assert_eq!((a.owned, a.unknown), (2, 2));
    // Los residentes no dependen del mapa.
    assert_eq!(
        s.entry(17).unwrap().recoverability(),
        Some(Recoverability::Full)
    );

    // Mapa corto (16 bytes = 128 clústeres): lo demás es desconocido.
    let mut vol = volume();
    vol.s.put_record(6, &bitmap_record(&BASE_ALLOCATED, 16));
    let v = open(&vol.s.image);
    let mut bm = ClusterBitmap::open(&v);
    assert_eq!(
        bm.issue(),
        Some(&BitmapIssue::Short {
            covered: 128,
            total_clusters: 255
        })
    );
    assert_eq!(
        bm.runs(126, 4).unwrap(),
        vec![
            (126, 2, ClusterAlloc::Free),
            (128, 2, ClusterAlloc::Unknown)
        ]
    );
    let s = scan(&v);
    assert_eq!(
        s.entry(14).unwrap().recoverability(),
        Some(Recoverability::None)
    );

    // Registro 6 no en uso.
    let mut vol = volume();
    vol.s.put_record(
        6,
        &record(6, 7, 0, &[si(), resident(types::DATA, "", &[0; 32])], 1024),
    );
    let v = open(&vol.s.image);
    assert_eq!(
        ClusterBitmap::open(&v).issue(),
        Some(&BitmapIssue::NotInUse)
    );
}

#[test]
fn bitmap_pages_are_read_once_and_bounded() {
    let vol = volume();
    let v = open(&vol.s.image);
    let mut bm = ClusterBitmap::open(&v);
    assert!(bm.issue().is_none());
    assert_eq!(
        bm.runs(0, 255).unwrap()[..3],
        [
            (0, 1, ClusterAlloc::Allocated),
            (1, 15, ClusterAlloc::Free),
            (16, 4, ClusterAlloc::Allocated)
        ]
    );
    for _ in 0..10 {
        bm.runs(100, 100).unwrap();
    }
    assert_eq!(bm.page_reads(), 1);
    // Fuera del volumen: desconocido.
    assert_eq!(
        bm.runs(250, 10).unwrap(),
        vec![
            (250, 5, ClusterAlloc::Free),
            (255, 5, ClusterAlloc::Unknown)
        ]
    );
}

#[test]
fn claim_index_keeps_only_intersections_and_is_bounded() {
    let mut ix = ClaimIndex::new(&[(100, 10), (105, 10), (300, 5), (0, 0)], 4);
    // Consulta unida: [100, 115) y [300, 305).
    ix.add(90, 20, 7); // [100, 110)
    ix.add(200, 50, 8); // nada
    ix.add(110, 200, 9); // [110, 115) y [300, 305)
    ix.add(0, 0, 10);
    ix.finish();
    assert_eq!(ix.len(), 3);
    assert!(!ix.truncated());
    assert_eq!(
        ix.overlapping(104, 8, None),
        vec![
            Claim {
                start: 104,
                end: 110,
                owner: 7
            },
            Claim {
                start: 110,
                end: 112,
                owner: 9
            }
        ]
    );
    assert_eq!(ix.overlapping(104, 8, Some(7)).len(), 1);
    assert!(ix.overlapping(200, 50, None).is_empty());
    assert_eq!(ix.overlapping(302, 100, None)[0].start, 302);
    // Límite: la quinta intersección no cabe.
    ix.add(100, 1, 11);
    ix.add(101, 1, 12);
    assert!(ix.truncated());
    assert_eq!(ix.len(), 4);
    // Sin consulta, nada se guarda.
    let mut empty = ClaimIndex::new(&[], 10);
    empty.add(0, u64::MAX, 1);
    assert!(empty.is_empty());
}

/// Clasificación de cada clúster contra un modelo directo (fuzzing determinista).
#[test]
fn fuzz_cluster_classification_matches_a_model() {
    let vol = volume();
    let v = open(&vol.s.image);
    let mut bm = ClusterBitmap::open(&v);
    let allocated = |c: u64| BASE_ALLOCATED.iter().any(|&(l, n)| l <= c && c < l + n);
    let mut r = xorshift(99);
    for _ in 0..500 {
        let lcn = 140 + r() % 120;
        let n = 1 + r() % 30;
        let mut live_claims = Vec::new();
        let mut dead_claims = Vec::new();
        let mut live = ClaimIndex::new(&[(lcn, n)], 1000);
        let mut dead = ClaimIndex::new(&[(lcn, n)], 1000);
        for _ in 0..r() % 5 {
            let (a, b, o) = (130 + r() % 140, 1 + r() % 10, 1000 + r() % 3);
            live.add(a, b, o);
            live_claims.push((a, a + b, o));
        }
        for _ in 0..r() % 5 {
            let (a, b, o) = (130 + r() % 140, 1 + r() % 10, 2000 + r() % 3);
            dead.add(a, b, o);
            dead_claims.push((a, a + b, o));
        }
        dead.add(lcn, n, 1);
        live.finish();
        dead.finish();
        let got = classify_clusters(1, lcn, n, &mut bm, &live, &dead).unwrap();
        let mut at = lcn;
        for (l, len, st) in got {
            assert_eq!(l, at);
            for c in l..l + len {
                let owner = live_claims
                    .iter()
                    .filter(|x| x.0 <= c && c < x.1)
                    .map(|x| x.2)
                    .min();
                let other = dead_claims
                    .iter()
                    .filter(|x| x.0 <= c && c < x.1)
                    .map(|x| x.2)
                    .min();
                let want = match (owner, other) {
                    (Some(o), _) => ClusterState::Owned { owner: o },
                    (None, _) if c >= 255 => ClusterState::Unknown,
                    (None, _) if allocated(c) => ClusterState::Allocated,
                    (None, Some(d)) => ClusterState::SharedWithDeleted { record: d },
                    (None, None) => ClusterState::Free,
                };
                assert_eq!(st, want, "clúster {c}");
            }
            at += len;
        }
        assert_eq!(at, lcn + n);
    }
}

#[test]
fn compressed_unit_with_a_suspect_cluster_is_entirely_suspect() {
    let mut vol = volume();
    let cu = 16 * C as usize;
    let mut plain: Vec<u8> = b"texto que se repite para comprimir bien "
        .iter()
        .copied()
        .cycle()
        .take(cu)
        .collect();
    plain.extend(noise(30, cu));
    let c0 = compress_for_tests(&plain[..cu]);
    let k0 = (c0.len() as u64).div_ceil(C);
    assert!(k0 < 16);
    vol.s.put_clusters(200, &c0);
    vol.s.put_clusters(220, &plain[cu..]);
    let runs = encode_runs(&[(Some(200), k0), (None, 16 - k0), (Some(220), 16)]);
    let size = plain.len() as u64;
    vol.s.put_record(
        21,
        &record(
            21,
            2,
            0,
            &[
                si(),
                fname(5, 5, "comprimido.txt"),
                non_resident_ex(
                    types::DATA,
                    "",
                    attr_flags::COMPRESSED,
                    0,
                    31,
                    &runs,
                    4,
                    (32 * C, size, size),
                    Some((k0 + 16) * C),
                ),
            ],
            1024,
        ),
    );
    let mut alloc = BASE_ALLOCATED.to_vec();
    alloc.push((200, 1));
    alloc.push((225, 1));
    vol.s.put_record(6, &bitmap_record(&alloc, 32));
    let v = open(&vol.s.image);
    let s = scan(&v);
    let a = s.entry(21).unwrap().main_stream().unwrap().clone();
    assert!(a.compressed);
    assert_eq!((a.clusters, a.sparse, a.allocated), (k0 + 16, 16 - k0, 2));
    assert_eq!(a.recoverability, Recoverability::Partial);
    let (out, r) = recover(&v, 21, "");
    assert_eq!(out, plain, "los bytes se recuperan tal como están");
    let ranges: Vec<_> = r
        .copy
        .untrusted
        .iter()
        .map(|x| (x.offset, x.length, x.state))
        .collect();
    assert_eq!(
        ranges,
        vec![
            (0, cu as u64, RangeState::PossiblyOverwritten),
            (cu as u64 + 5 * C, C, RangeState::PossiblyOverwritten),
        ]
    );
}

/// Observador que cancela tras `n` avisos de progreso.
struct CancelAfter(std::cell::Cell<u32>);

impl ScanObserver for CancelAfter {
    fn cancelled(&self) -> bool {
        self.0.get() == 0
    }
    fn progress(&self, _: u64, _: u64) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

#[test]
fn cancellation_progress_limits_and_focus() {
    let vol = volume();
    let v = open(&vol.s.image);
    assert!(scan_deleted(
        &v,
        &ScanOptions::default(),
        &CancelAfter(std::cell::Cell::new(0))
    )
    .unwrap()
    .is_none());
    struct Count(std::cell::Cell<(u64, u64)>);
    impl ScanObserver for Count {
        fn progress(&self, d: u64, t: u64) {
            assert!(d <= t);
            self.0.set((d, t));
        }
    }
    let c = Count(std::cell::Cell::new((0, 0)));
    scan_deleted(&v, &ScanOptions::default(), &c)
        .unwrap()
        .unwrap();
    let (d, t) = c.0.get();
    assert_eq!(d, t);
    assert!(t >= 2 * v.record_count());
    // Límite de entradas y foco.
    let o = ScanOptions {
        max_entries: 3,
        ..ScanOptions::default()
    };
    let s = scan_deleted(&v, &o, &NoObserver).unwrap().unwrap();
    assert_eq!(s.entries.len(), 3);
    assert!(s.entries_truncated);
    let o = ScanOptions {
        focus: Some(21),
        ..ScanOptions::default()
    };
    let s = scan_deleted(&v, &o, &NoObserver).unwrap().unwrap();
    assert_eq!(s.entries.len(), 1);
    // El otro borrado que comparte el clúster se sigue teniendo en cuenta.
    assert_eq!(s.entries[0].main_stream().unwrap().shared_with, vec![22]);
}

/// Fuzzing: registros mutados (incluido `$Bitmap`); sin pánicos, solo registros base no en uso,
/// recuentos coherentes y nunca `full` con clústeres sospechosos o sin mapa.
#[test]
fn fuzz_mutated_volumes_keep_the_invariants() {
    let base = volume();
    let mut r = xorshift(2323);
    for _ in 0..300 {
        let mut img = base.s.image.clone();
        let n = [6u64, 13, 14, 15, 17, 21, 23, 24, 25, 26][(r() % 10) as usize];
        let at = base.s.record_offset(n) as usize;
        for _ in 0..1 + r() % 8 {
            let i = at + (r() % 1024) as usize;
            img[i] = r() as u8;
        }
        let v = open(&img);
        let s = scan(&v);
        for e in &s.entries {
            assert!(matches!(
                v.read_record(e.record).unwrap(),
                RecordOutcome::Parsed(ref x) if !x.in_use() && x.header().is_base()
            ));
            for a in &e.streams {
                if a.recoverability == Recoverability::Full {
                    assert_eq!(a.possibly_overwritten() + a.unmapped, 0);
                }
                assert_eq!(
                    a.clusters,
                    a.free + a.owned + a.allocated + a.unknown + a.shared
                );
                let mut sink = std::io::sink();
                let _ = recover_stream(&v, e.record, &a.name, &mut sink, &NoObserver);
            }
        }
    }
}
