//! Pruebas de ficheros y flujos sobre volúmenes sintéticos (`crate::testvol`): residentes, no
//! residentes, dispersos, sin inicializar, comprimidos con LZNT1, `$ATTRIBUTE_LIST`, sectores
//! ilegibles y fuzzing.

use std::sync::Arc;

use mr_io::{FaultyDevice, MemDevice};

use crate::attr::{attr_flags, types};
use crate::file::{FileAnomaly, SegmentKind, StreamError, StreamIssue};
use crate::lznt1::compress_for_tests;
use crate::record::flags::IN_USE;
use crate::stream::{RangeState, StreamChunk, MAX_STREAM_READ};
use crate::testvol::{
    encode_runs, non_resident, non_resident_ex, protect, record, record_with_base, resident,
    with_id, xorshift, Synth,
};
use crate::values::tests::{file_name_value, list_entry};
use crate::{NtfsError, NtfsVolume, RecordState};

const C: u64 = 4096;

fn si() -> Vec<u8> {
    resident(types::STANDARD_INFORMATION, "", &[0x10; 72])
}

fn fname(name: &str) -> Vec<u8> {
    resident(
        types::FILE_NAME,
        "",
        &file_name_value(5 | (5 << 48), name, 3),
    )
}

fn synth() -> Synth {
    Synth::new(8)
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

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut r = xorshift(seed);
    (0..n).map(|_| r() as u8).collect()
}

fn text(seed: u64, n: usize) -> Vec<u8> {
    let words: [&[u8]; 6] = [
        b"cluster ",
        b"registro ",
        b"flujo ",
        b"datos ",
        b"\r\n",
        b"0123 ",
    ];
    let mut r = xorshift(seed);
    let mut v = Vec::new();
    while v.len() < n {
        v.extend_from_slice(words[(r() % 6) as usize]);
    }
    v.truncate(n);
    v
}

fn states(c: &StreamChunk) -> Vec<(u64, u64, RangeState)> {
    c.ranges
        .iter()
        .map(|r| (r.offset, r.length, r.state))
        .collect()
}

fn read_all(v: &NtfsVolume, n: u64, name: &str) -> StreamChunk {
    let s = v.open_stream(n, name).unwrap();
    s.read(0, s.size() as usize).unwrap()
}

/// Registro 12: `$DATA` residente, flujo alternativo residente y otro no residente.
/// Registro 13: no residente con un hueco disperso y tamaño inicializado menor que el de datos.
fn plain_volume() -> (Synth, Vec<u8>, Vec<u8>) {
    let mut s = synth();
    let big = noise(1, 3000);
    s.put_clusters(140, &big);
    s.put_record(
        12,
        &record(
            12,
            1,
            IN_USE,
            &[
                si(),
                fname("residente.txt"),
                resident(types::DATA, "", b"contenido residente"),
                resident(types::DATA, "ads", b"alt"),
                non_resident(
                    types::DATA,
                    "grande",
                    0,
                    0,
                    &encode_runs(&[(Some(140), 1)]),
                    4096,
                    3000,
                    3000,
                ),
            ],
            1024,
        ),
    );
    // 7 clústeres: 2 en el 150, 3 dispersos, 2 en el 145; datos 26 624 B, inicializados 21 504.
    let a = noise(2, 2 * C as usize);
    let b = noise(3, 2 * C as usize);
    s.put_clusters(150, &a);
    s.put_clusters(145, &b);
    s.put_record(
        13,
        &record(
            13,
            1,
            IN_USE,
            &[
                si(),
                fname("disperso.bin"),
                non_resident_ex(
                    types::DATA,
                    "",
                    attr_flags::SPARSE,
                    0,
                    6,
                    &encode_runs(&[(Some(150), 2), (None, 3), (Some(145), 2)]),
                    4,
                    (7 * C, 26624, 21504),
                    Some(4 * C),
                ),
            ],
            1024,
        ),
    );
    let mut want = a.clone();
    want.extend(vec![0u8; 3 * C as usize]);
    want.extend_from_slice(&b[..1024]);
    want.resize(26624, 0);
    (s, big, want)
}

#[test]
fn resident_non_resident_sparse_and_uninitialized() {
    let (s, big, want) = plain_volume();
    let v = open(&s.image);
    let f = v.file(12).unwrap();
    assert!(f.anomalies.is_empty(), "{:?}", f.anomalies);
    assert_eq!(f.data_streams(), vec!["", "ads", "grande"]);
    assert_eq!(f.file_names()[0].name, "residente.txt");
    assert!(f.standard_information().unwrap().v3.is_some());
    let c = read_all(&v, 12, "");
    assert_eq!(c.data, b"contenido residente");
    assert_eq!(states(&c), vec![(0, 19, RangeState::Data)]);
    assert!(v.open_stream(12, "").unwrap().layout().is_resident());
    assert_eq!(read_all(&v, 12, "ads").data, b"alt");
    assert_eq!(read_all(&v, 12, "grande").data, big);

    let st = v.open_stream(13, "").unwrap();
    let l = st.layout();
    assert!(l.is_sparse_flag() && !l.is_compressed());
    assert!(l.issues.is_empty(), "{:?}", l.issues);
    assert_eq!(
        l.segments.iter().map(|x| x.kind).collect::<Vec<_>>(),
        vec![
            SegmentKind::Disk(150),
            SegmentKind::Sparse,
            SegmentKind::Disk(145)
        ]
    );
    let c = st.read(0, 1 << 20).unwrap();
    assert_eq!(c.data, want);
    assert_eq!(
        states(&c),
        vec![
            (0, 8192, RangeState::Data),
            (8192, 12288, RangeState::Sparse),
            (20480, 1024, RangeState::Data),
            (21504, 5120, RangeState::Uninitialized),
        ]
    );
    assert!(c.all_trustworthy());
    // Lecturas parciales en posiciones arbitrarias.
    let mut rng = xorshift(77);
    for _ in 0..200 {
        let off = rng() % 27000;
        let len = (rng() % 12000) as usize;
        let c = st.read(off, len).unwrap();
        let end = (off + len as u64).min(26624);
        let exp = if off < end {
            &want[off as usize..end as usize]
        } else {
            &[][..]
        };
        assert_eq!(c.data, exp, "{off} {len}");
        assert_eq!(
            c.ranges.iter().map(|r| r.length).sum::<u64>(),
            exp.len() as u64
        );
    }
    assert!(matches!(
        st.read(0, MAX_STREAM_READ + 1),
        Err(NtfsError::ReadTooLarge { .. })
    ));
    // Copia completa y su informe.
    let mut out = Vec::new();
    let r = st.copy_to(&mut out).unwrap();
    assert_eq!(out, want);
    assert_eq!(r.count(RangeState::Sparse), 12288);
    assert_eq!(r.count(RangeState::Uninitialized), 5120);
    assert_eq!(r.untrusted_bytes(), 0);
}

#[test]
fn unreadable_sectors_and_truncated_source_are_never_data() {
    let (s, _, want) = plain_volume();
    // Sectores 3 y 4 del clúster 151 (bytes 4096 + 1536 .. 4096 + 2560 del flujo).
    let lba = 151 * 8;
    let v = open_bad(&s.image, vec![lba + 3, lba + 4]);
    let c = read_all(&v, 13, "");
    assert_eq!(
        c.untrusted()
            .map(|r| (r.offset, r.length, r.lbas))
            .collect::<Vec<_>>(),
        vec![(5632, 1024, Some((lba + 3, 2)))]
    );
    for (i, (g, w)) in c.data.iter().zip(&want).enumerate() {
        if (5632..6656).contains(&i) {
            assert_eq!(*g, b"BAD!"[i % 4]);
        } else {
            assert_eq!(g, w);
        }
    }
    // Un sector ilegible en la zona sin inicializar no afecta: nunca se lee.
    let v = open_bad(&s.image, vec![146 * 8 + 5]);
    assert!(read_all(&v, 13, "").all_trustworthy());
    // Patrón propio.
    let v = open_bad(&s.image, vec![lba + 3]);
    let st = v.open_stream(13, "").unwrap().with_fill(*b"XY12");
    let c = st.read(5632, 4).unwrap();
    assert_eq!(c.data, b"XY12");

    // Origen truncado: el run del clúster 250 cae fuera.
    let mut s2 = synth();
    s2.put_record(
        14,
        &record(
            14,
            1,
            IN_USE,
            &[
                si(),
                non_resident(
                    types::DATA,
                    "",
                    0,
                    3,
                    &encode_runs(&[(Some(244), 4)]),
                    4 * C,
                    4 * C,
                    4 * C,
                ),
            ],
            1024,
        ),
    );
    s2.put_clusters(244, &[0xAB; 4096]);
    let cut = 246 * C as usize; // los clústeres 246 y 247 no están en el origen
    let v = NtfsVolume::open(Arc::new(
        MemDevice::new(s2.image[..cut].to_vec(), 512).unwrap(),
    ))
    .unwrap();
    let c = read_all(&v, 14, "");
    assert_eq!(
        states(&c),
        vec![
            (0, 8192, RangeState::Data),
            (8192, 8192, RangeState::OutsideSource)
        ]
    );
    assert_eq!(&c.data[..4096], &[0xAB; 4096][..]);
}

/// Fichero comprimido de 4 unidades de 16 clústeres: comprimida, sin comprimir, toda dispersa y
/// una última parcial comprimida.
fn compressed_volume() -> (Synth, Vec<u8>, [u64; 2]) {
    let mut s = synth();
    let cu = 16 * C as usize;
    let mut plain = text(10, cu);
    plain.extend(noise(11, cu));
    plain.extend(vec![0u8; cu]);
    plain.extend(text(12, 10000));
    let c0 = compress_for_tests(&plain[..cu]);
    let c3 = compress_for_tests(&plain[3 * cu..]);
    let k0 = (c0.len() as u64).div_ceil(C);
    let k3 = (c3.len() as u64).div_ceil(C);
    assert!(k0 < 16 && k3 < 16, "{k0} {k3}");
    s.put_clusters(100, &c0);
    s.put_clusters(180, &plain[cu..2 * cu]);
    s.put_clusters(200, &c3);
    let runs = encode_runs(&[
        (Some(100), k0),
        (None, 16 - k0),
        (Some(180), 16),
        (None, 16),
        (Some(200), k3),
        (None, 16 - k3),
    ]);
    let size = plain.len() as u64;
    s.put_record(
        15,
        &record(
            15,
            1,
            IN_USE,
            &[
                si(),
                fname("comprimido.txt"),
                non_resident_ex(
                    types::DATA,
                    "",
                    attr_flags::COMPRESSED,
                    0,
                    63,
                    &runs,
                    4,
                    (64 * C, size, size),
                    Some((k0 + 16 + k3) * C),
                ),
            ],
            1024,
        ),
    );
    (s, plain, [k0, k3])
}

#[test]
fn compressed_units_decompress_sparse_and_raw() {
    let (s, plain, _) = compressed_volume();
    let v = open(&s.image);
    let st = v.open_stream(15, "").unwrap();
    assert!(st.layout().is_compressed());
    assert_eq!(st.layout().compression_unit_clusters, 16);
    let c = st.read(0, 1 << 20).unwrap();
    assert_eq!(c.data.len(), plain.len());
    assert!(c.data == plain);
    assert_eq!(
        states(&c),
        vec![
            (0, 131072, RangeState::Data),
            (131072, 65536, RangeState::Sparse),
            (196608, 10000, RangeState::Data),
        ]
    );
    // Lecturas que cruzan unidades.
    let mut rng = xorshift(5);
    for _ in 0..100 {
        let off = rng() % plain.len() as u64;
        let len = (rng() % 150_000) as usize;
        let c = st.read(off, len).unwrap();
        let end = (off as usize + len).min(plain.len());
        assert!(c.data == plain[off as usize..end], "{off} {len}");
    }
    let mut out = Vec::new();
    st.copy_to(&mut out).unwrap();
    assert!(out == plain);
}

#[test]
fn unreadable_or_corrupt_compressed_units_are_reported() {
    let (s, plain, [k0, _]) = compressed_volume();
    assert!(k0 >= 2);
    // Un sector ilegible en el segundo clúster comprimido de la unidad 0.
    let bad = 101 * 8 + 2;
    let v = open_bad(&s.image, vec![bad]);
    let c = read_all(&v, 15, "");
    let un: Vec<_> = c.untrusted().copied().collect();
    assert_eq!(un.len(), 1);
    let w = un[0].offset;
    assert!(w > 0 && w % 4096 == 0 && w < 65536, "{w}");
    assert_eq!(un[0].length, 65536 - w);
    assert_eq!(un[0].state, RangeState::Unreadable);
    assert_eq!(un[0].lbas, Some((bad, 1)));
    assert!(c.data[..w as usize] == plain[..w as usize]);
    assert!(c.data[65536..] == plain[65536..]);

    // Unidad comprimida con un bloque válido y otro que referencia antes de su inicio.
    let mut s2 = synth();
    let first = text(20, 4096);
    let mut cu = compress_for_tests(&first);
    cu.extend_from_slice(&[0x02, 0xB0, 0x01, 0x00, 0x00]);
    s2.put_clusters(120, &cu);
    s2.put_record(
        14,
        &record(
            14,
            1,
            IN_USE,
            &[
                si(),
                non_resident_ex(
                    types::DATA,
                    "",
                    attr_flags::COMPRESSED,
                    0,
                    15,
                    &encode_runs(&[(Some(120), 1), (None, 15)]),
                    4,
                    (16 * C, 50000, 50000),
                    Some(C),
                ),
            ],
            1024,
        ),
    );
    let v = open(&s2.image);
    let c = read_all(&v, 14, "");
    assert_eq!(
        states(&c),
        vec![
            (0, 4096, RangeState::Data),
            (4096, 45904, RangeState::Corrupt)
        ]
    );
    assert_eq!(&c.data[..4096], &first[..]);
    assert_eq!(&c.data[4096..4100], b"BAD!");
}

/// Fichero 21 con `$ATTRIBUTE_LIST`: `$DATA` en dos extensiones (registros 22 y 23).
struct ListCase {
    base_seq: u16,
    base_flags: u16,
    ext23_base: u64,
    ext23_seq: u16,
    ext23_flags: u16,
    entry23_seq: u16,
    ext23_extra: Vec<Vec<u8>>,
    ext23_start: u64,
    ext23_len: u64,
    extra_entries: Vec<Vec<u8>>,
    non_resident_list: bool,
}

impl Default for ListCase {
    fn default() -> Self {
        ListCase {
            base_seq: 1,
            base_flags: IN_USE,
            ext23_base: 21 | (1 << 48),
            ext23_seq: 1,
            ext23_flags: IN_USE,
            entry23_seq: 1,
            ext23_extra: Vec::new(),
            ext23_start: 10,
            ext23_len: 10,
            extra_entries: Vec::new(),
            non_resident_list: false,
        }
    }
}

const LIST_SIZE: u64 = 20 * 4096 - 100;

fn list_volume(case: &ListCase) -> (Synth, Vec<u8>) {
    let mut s = synth();
    let a = noise(30, 10 * C as usize);
    let b = noise(31, 10 * C as usize);
    s.put_clusters(140, &a);
    s.put_clusters(160, &b);
    let mut list = list_entry(0x10, "", 0, 21, case.base_seq, 0);
    list.extend(list_entry(0x30, "", 0, 21, case.base_seq, 1));
    list.extend(list_entry(0x80, "", 0, 22, 1, 0));
    list.extend(list_entry(
        0x80,
        "",
        case.ext23_start,
        23,
        case.entry23_seq,
        0,
    ));
    for e in &case.extra_entries {
        list.extend_from_slice(e);
    }
    let list_attr = if case.non_resident_list {
        s.put_clusters(230, &list);
        non_resident(
            types::ATTRIBUTE_LIST,
            "",
            0,
            0,
            &encode_runs(&[(Some(230), 1)]),
            C,
            list.len() as u64,
            list.len() as u64,
        )
    } else {
        resident(types::ATTRIBUTE_LIST, "", &list)
    };
    s.put_record(
        21,
        &record(
            21,
            case.base_seq,
            case.base_flags,
            &[si(), list_attr, with_id(fname("lista.bin"), 1)],
            1024,
        ),
    );
    s.put_record(
        22,
        &record_with_base(
            22,
            1,
            case.base_flags,
            21 | (u64::from(case.base_seq) << 48),
            &[non_resident(
                types::DATA,
                "",
                0,
                9,
                &encode_runs(&[(Some(140), 10)]),
                20 * C,
                LIST_SIZE,
                LIST_SIZE,
            )],
            1024,
        ),
    );
    let mut attrs = vec![non_resident(
        types::DATA,
        "",
        case.ext23_start,
        case.ext23_start + case.ext23_len - 1,
        &encode_runs(&[(Some(160), case.ext23_len)]),
        0,
        0,
        0,
    )];
    attrs.extend(case.ext23_extra.iter().cloned());
    s.put_record(
        23,
        &record_with_base(
            23,
            case.ext23_seq,
            case.ext23_flags,
            case.ext23_base,
            &attrs,
            1024,
        ),
    );
    let mut want = a;
    want.extend(b);
    want.truncate(LIST_SIZE as usize);
    (s, want)
}

fn file_codes(v: &NtfsVolume, n: u64) -> Vec<&'static str> {
    v.file(n)
        .unwrap()
        .anomalies
        .iter()
        .map(|a| a.code())
        .collect()
}

#[test]
fn attribute_list_follows_extension_records() {
    for non_resident_list in [false, true] {
        let (s, want) = list_volume(&ListCase {
            non_resident_list,
            ..ListCase::default()
        });
        let v = open(&s.image);
        let f = v.file(21).unwrap();
        assert!(f.anomalies.is_empty(), "{:?}", f.anomalies);
        assert_eq!(f.extensions, vec![22, 23]);
        assert_eq!(f.attribute_list.len(), 4);
        let st = v.stream_of(&f, "").unwrap();
        assert_eq!(st.layout().extents.len(), 2);
        assert_eq!(st.layout().extents[1].record, 23);
        let c = st.read(0, 1 << 20).unwrap();
        assert_eq!(c.data, want);
        assert!(c.all_trustworthy());
        // Las extensiones no se abren como ficheros; un registro vacío no es un fichero.
        assert!(matches!(
            v.file(22),
            Err(NtfsError::NotBaseRecord { number: 22, .. })
        ));
        assert!(matches!(
            v.file(12),
            Err(NtfsError::RecordUnusable {
                state: RecordState::Empty,
                ..
            })
        ));
    }
    // Fichero borrado: el base y las extensiones han incrementado su secuencia al liberarse.
    let (s, want) = list_volume(&ListCase {
        base_seq: 2,
        base_flags: 0,
        ext23_base: 21 | (1 << 48),
        ext23_seq: 2,
        ext23_flags: 0,
        ..ListCase::default()
    });
    let v = open(&s.image);
    let f = v.file(21).unwrap();
    assert!(!f.in_use());
    // El base dice secuencia 2 en sus propias entradas y la extensión 22 lo cita con la 2.
    assert!(f.anomalies.is_empty(), "{:?}", f.anomalies);
    assert_eq!(read_all(&v, 21, "").data, want);
}

#[test]
fn attribute_list_rejects_foreign_or_reused_extensions() {
    let unmapped_tail = |v: &NtfsVolume| {
        let st = v.open_stream(21, "").unwrap();
        let issues: Vec<_> = st.layout().issues.iter().map(StreamIssue::code).collect();
        assert_eq!(issues, vec!["stream_runs_short"]);
        let c = st.read(0, 1 << 20).unwrap();
        let un: Vec<_> = c
            .untrusted()
            .map(|r| (r.offset, r.length, r.state))
            .collect();
        assert_eq!(un, vec![(40960, LIST_SIZE - 40960, RangeState::Unmapped)]);
        assert_eq!(&c.data[40960..40964], b"BAD!");
    };
    // Extensión que pertenece a otro registro base.
    let (s, _) = list_volume(&ListCase {
        ext23_base: 20 | (1 << 48),
        ..ListCase::default()
    });
    let v = open(&s.image);
    assert_eq!(file_codes(&v, 21), vec!["extension_base_mismatch"]);
    unmapped_tail(&v);
    // Extensión reutilizada (secuencia distinta de la de la lista).
    let (s, _) = list_volume(&ListCase {
        entry23_seq: 4,
        ..ListCase::default()
    });
    let v = open(&s.image);
    let f = v.file(21).unwrap();
    assert_eq!(
        f.anomalies,
        vec![FileAnomaly::ExtensionSequenceMismatch {
            record: 23,
            expected: 4,
            found: 1
        }]
    );
    unmapped_tail(&v);
    // Extensión ilegible.
    let (s, _) = list_volume(&ListCase::default());
    let v = open_bad(&s.image, s.record_lbas(23)[..1].to_vec());
    let f = v.file(21).unwrap();
    assert!(matches!(
        f.anomalies[..],
        [FileAnomaly::ExtensionUnusable {
            record: 23,
            state: RecordState::Unreadable,
            ..
        }]
    ));
    unmapped_tail(&v);
    // Extensión con su propia lista (no se sigue) y entrada cuyo atributo no está.
    let (s, want) = list_volume(&ListCase {
        ext23_extra: vec![resident(
            types::ATTRIBUTE_LIST,
            "",
            &list_entry(0x80, "", 0, 21, 1, 0),
        )],
        extra_entries: vec![list_entry(0x80, "x", 0, 22, 1, 9)],
        ..ListCase::default()
    });
    let v = open(&s.image);
    assert_eq!(
        file_codes(&v, 21),
        vec!["extension_has_attr_list", "listed_attribute_missing"]
    );
    assert_eq!(read_all(&v, 21, "").data, want);
    // Referencia fuera de la MFT y estado de uso distinto.
    let (s, _) = list_volume(&ListCase {
        extra_entries: vec![list_entry(0x80, "y", 0, 9999, 1, 0)],
        ext23_flags: 0,
        ..ListCase::default()
    });
    let v = open(&s.image);
    assert_eq!(
        file_codes(&v, 21),
        vec!["extension_in_use_mismatch", "extension_out_of_range"]
    );
    // Extensiones solapadas o repetidas, y sin primera extensión.
    let (s, _) = list_volume(&ListCase {
        ext23_start: 5,
        ..ListCase::default()
    });
    let e = open(&s.image).open_stream(21, "").unwrap_err();
    assert_eq!(e.code(), "stream_extent_overlap");
    let (s, _) = list_volume(&ListCase {
        ext23_start: 0,
        ..ListCase::default()
    });
    let e = open(&s.image).open_stream(21, "").unwrap_err();
    assert_eq!(e.code(), "stream_duplicate_extent");
    // La segunda extensión se pasa del tamaño asignado.
    let (s, _) = list_volume(&ListCase {
        ext23_start: 12,
        ..ListCase::default()
    });
    let e = open(&s.image).open_stream(21, "").unwrap_err();
    assert_eq!(e.code(), "stream_exceeds_allocated");
    // Hueco entre extensiones (la segunda empieza más tarde): tramo sin mapa.
    let (s, _) = list_volume(&ListCase {
        ext23_start: 12,
        ext23_len: 8,
        ..ListCase::default()
    });
    let v = open(&s.image);
    let st = v.open_stream(21, "").unwrap();
    assert_eq!(
        st.layout().issues,
        vec![StreamIssue::ExtentGap {
            from_vcn: 10,
            to_vcn: 12
        }]
    );
    let c = st.read(0, 1 << 20).unwrap();
    assert_eq!(
        c.untrusted()
            .map(|r| (r.offset, r.length))
            .collect::<Vec<_>>(),
        vec![(40960, 8192)]
    );
}

#[test]
fn stream_errors_are_typed() {
    let mut s = synth();
    let data =
        |flags: u16, cu: u16, sizes: (u64, u64, u64), runs: &[(Option<u64>, u64)], last: u64| {
            non_resident_ex(
                types::DATA,
                "",
                flags,
                0,
                last,
                &encode_runs(runs),
                cu,
                sizes,
                (flags & (attr_flags::COMPRESSED | attr_flags::SPARSE) != 0).then_some(C),
            )
        };
    let cases: Vec<(u64, Vec<u8>, &str)> = vec![
        (
            12,
            data(attr_flags::ENCRYPTED, 0, (C, 10, 10), &[(Some(140), 1)], 0),
            "stream_encrypted",
        ),
        (
            13,
            data(0, 0, (C + 1, 10, 10), &[(Some(140), 1)], 0),
            "stream_allocated_not_cluster_multiple",
        ),
        (
            14,
            data(
                attr_flags::COMPRESSED,
                3,
                (16 * C, 10, 10),
                &[(Some(140), 1), (None, 15)],
                15,
            ),
            "stream_compression_unit",
        ),
        (
            15,
            data(0, 0, (C, 10, 10), &[(Some(140), 2)], 1),
            "stream_exceeds_allocated",
        ),
    ];
    for (n, a, _) in &cases {
        s.put_record(*n, &record(*n as u32, 1, IN_USE, &[si(), a.clone()], 1024));
    }
    let v = open(&s.image);
    for (n, _, code) in &cases {
        let e = v.open_stream(*n, "").unwrap_err();
        assert_eq!(e.code(), *code, "{n}");
        assert!(matches!(e, NtfsError::Stream { number, .. } if number == *n));
    }
    assert!(matches!(
        v.open_stream(12, "otro"),
        Err(NtfsError::Stream {
            error: StreamError::NotFound { .. },
            ..
        })
    ));
    // Runs que no cubren la extensión: aviso y tramo sin mapa (no un error).
    let mut s = synth();
    s.put_record(
        12,
        &record(
            12,
            1,
            IN_USE,
            &[
                si(),
                data(0, 0, (4 * C, 4 * C, 4 * C), &[(Some(140), 2)], 3),
            ],
            1024,
        ),
    );
    let v = open(&s.image);
    let st = v.open_stream(12, "").unwrap();
    let issues: Vec<_> = st.layout().issues.iter().map(StreamIssue::code).collect();
    assert_eq!(issues, vec!["runs_coverage"]);
    let c = st.read(0, 1 << 20).unwrap();
    assert_eq!(states(&c), vec![(0, 4 * C, RangeState::Unmapped)]);
    // Atributo con tamaños incoherentes: el iterador lo rechaza y queda como anomalía.
    let mut s = synth();
    s.put_record(
        12,
        &record(
            12,
            1,
            IN_USE,
            &[si(), data(0, 0, (C, 10, 20), &[(Some(140), 1)], 0)],
            1024,
        ),
    );
    let v = open(&s.image);
    assert_eq!(file_codes(&v, 12), vec!["attr_non_resident_sizes"]);
    assert_eq!(
        v.open_stream(12, "").unwrap_err().code(),
        "stream_not_found"
    );
}

#[test]
fn mft_extensions_complete_the_layout() {
    let mut s = synth();
    let [(la, na), (lb, nb)] = s.mft_runs;
    let total = na + nb;
    // Registro 0 con solo la primera extensión de $DATA y una lista que cita la segunda en el 12.
    let mut list = list_entry(0x10, "", 0, 0, 1, 0);
    list.extend(list_entry(0x80, "", 0, 0, 1, 0));
    list.extend(list_entry(0x80, "", na, 12, 1, 0));
    let r0 = record(
        0,
        1,
        IN_USE,
        &[
            si(),
            resident(types::ATTRIBUTE_LIST, "", &list),
            non_resident(
                types::DATA,
                "",
                0,
                na - 1,
                &encode_runs(&[(Some(la), na)]),
                total * C,
                32 * 1024,
                28 * 1024,
            ),
        ],
        1024,
    );
    s.put_record(0, &r0);
    let ext = record_with_base(
        12,
        1,
        IN_USE,
        1 << 48,
        &[non_resident(
            types::DATA,
            "",
            na,
            total - 1,
            &encode_runs(&[(Some(lb), nb)]),
            0,
            0,
            0,
        )],
        1024,
    );
    s.put_record(12, &ext);
    let v = open(&s.image);
    let codes: Vec<_> = v.info().anomalies.iter().map(|a| a.code()).collect();
    assert_eq!(codes, vec!["mft_extended"]);
    assert_eq!(v.layout().runs.len(), 2);
    // Los registros de la segunda extensión vuelven a leerse.
    assert_eq!(v.read_record(17).unwrap().state(), RecordState::InUse);
    assert_eq!(read_all(&v, 17, "").data, b"datos");
    // Sin la extensión, la MFT queda incompleta.
    let mut s2 = synth();
    s2.put_record(0, &r0);
    let v = open(&s2.image);
    let codes: Vec<_> = v.info().anomalies.iter().map(|a| a.code()).collect();
    assert_eq!(codes, vec!["mft_incomplete"]);
    assert_eq!(v.read_record(17).unwrap().state(), RecordState::Unmapped);
}

/// Fuzzing determinista de ficheros y flujos: registros con atributos, lista y runs mutados; abrir
/// el fichero y leer todos sus flujos nunca entra en pánico, la salida mide lo pedido y cada tramo
/// no fiable lleva el patrón.
#[test]
fn file_and_stream_fuzz_never_panics() {
    let (base_list, _) = list_volume(&ListCase::default());
    let (base_comp, _, _) = compressed_volume();
    let (base_plain, _, _) = plain_volume();
    let mut rng = xorshift(0x5EED_2121);
    let mut read_bytes = 0u64;
    for i in 0..600u32 {
        let (mut s, targets): (Synth, &[u64]) = match i % 3 {
            0 => (clone_synth(&base_list), &[21, 22, 23]),
            1 => (clone_synth(&base_comp), &[15]),
            _ => (clone_synth(&base_plain), &[12, 13]),
        };
        for _ in 0..1 + rng() % 4 {
            let n = targets[(rng() as usize) % targets.len()];
            let mut raw = Vec::new();
            for &(d, l) in &s.pieces[n as usize] {
                raw.extend_from_slice(&s.image[d as usize..(d + l) as usize]);
            }
            crate::record::apply_fixups(&mut raw).unwrap_or(0);
            for _ in 0..1 + rng() % 3 {
                let at = 0x38 + (rng() % 0x300) as usize;
                raw[at] = rng() as u8;
            }
            protect(&mut raw);
            s.put_record(n, &raw);
        }
        let v = open(&s.image);
        for &n in targets {
            let Ok(f) = v.file(n) else { continue };
            for name in f.data_streams() {
                let Ok(st) = v.stream_of(&f, &name) else {
                    continue;
                };
                let want = st.size().min(1 << 20);
                let c = st.read(0, want as usize).unwrap();
                assert_eq!(c.data.len() as u64, want);
                assert_eq!(c.ranges.iter().map(|r| r.length).sum::<u64>(), want);
                for r in c.untrusted() {
                    let a = r.offset as usize;
                    assert_eq!(c.data[a], b"BAD!"[a % 4]);
                }
                read_bytes += want;
            }
        }
    }
    assert!(read_bytes > 0);
}

fn clone_synth(s: &Synth) -> Synth {
    Synth {
        image: s.image.clone(),
        cluster: s.cluster,
        total_sectors: s.total_sectors,
        mft_lcn: s.mft_lcn,
        mirr_lcn: s.mirr_lcn,
        pieces: s.pieces.clone(),
        mirror_offset: s.mirror_offset,
        mft_runs: s.mft_runs,
    }
}
