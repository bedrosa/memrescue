//! Pruebas de índices `$I30`, directorios, árbol y rutas (#22) sobre volúmenes sintéticos
//! (`crate::testvol::tree_volume`): una regla por prueba, nodos dañados que no bloquean el resto
//! y fuzzing determinista de nodos.

use std::sync::Arc;

use mr_io::{FaultyDevice, MemDevice};

use crate::dir::{NoTreeObserver, TreeObserver};
use crate::index::{parse_block, parse_node, parse_root, IndexIssue, NodeId};
use crate::record::flags::{DIRECTORY, IN_USE};
use crate::record::{apply_fixups, FileReference};
use crate::testvol::{
    encode_runs, file_name, fref, index_entry, index_root_value, indx_block, named_entry,
    non_resident, protect, record, res, tree_root_nodes, tree_volume, with_id, xorshift, Synth,
    TREE_ROOT_INDX_LCN,
};
use crate::upcase::UpcaseSource;
use crate::values::Namespace;
use crate::{
    DirListing, EntryState, NtfsError, NtfsVolume, OrphanReason, RecordState, TreeIssue,
    TreeOptions, TreeStatus,
};

fn open(s: &Synth) -> NtfsVolume {
    NtfsVolume::open(Arc::new(MemDevice::new(s.image.clone(), 512).unwrap())).unwrap()
}

fn names(l: &DirListing) -> Vec<String> {
    l.entries.iter().map(|e| e.name.clone()).collect()
}

fn codes(l: &DirListing) -> Vec<(NodeId, &'static str)> {
    l.anomalies
        .iter()
        .map(|a| (a.node, a.issue.code()))
        .collect()
}

fn root() -> u64 {
    fref(5, 5)
}

fn si() -> Vec<u8> {
    res(0x10, "", &[0x10; 72], 0)
}

#[test]
fn root_listing_reads_both_index_nodes_and_hides_dos_duplicates() {
    let v = open(&tree_volume(false));
    let l = v.list_dir(5).unwrap();
    assert!(l.anomalies.is_empty(), "{:?}", l.anomalies);
    // «.» (la raíz apuntándose a sí misma) no se lista; MEDIO~1.TXT queda como nombre corto.
    assert_eq!(names(&l), ["a.txt", "Docs", "Medio.txt", "Ágil.txt"]);
    assert!(l.entries.iter().all(|e| e.state.is_live()));
    let medio = &l.entries[2];
    assert_eq!(medio.dos_name.as_deref(), Some("MEDIO~1.TXT"));
    assert_eq!(medio.namespace, Namespace::Win32);
    assert_eq!(medio.size, 5);
    assert_eq!(l.entries[3].namespace, Namespace::Win32AndDos);
    assert!(l.entries[1].is_directory && !l.entries[0].is_directory);
    assert_eq!(
        l.directory,
        FileReference {
            record: 5,
            sequence: 5
        }
    );
    // Fechas de $STANDARD_INFORMATION (no de la clave del índice).
    assert!(!medio.times_from_index);
}

#[test]
fn stale_entries_are_reported_never_listed_as_live() {
    let v = open(&tree_volume(false));
    let l = v.list_dir(16).unwrap();
    assert!(l.anomalies.is_empty(), "{:?}", l.anomalies);
    let got: Vec<(String, &str)> = l
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.state.code()))
        .collect();
    let want: Vec<(String, &str)> = [
        ("b\u{FFFD}c", "live"),
        ("fuera.txt", "invalid_reference"),
        ("Informe.txt", "live"),
        ("reusado.txt", "stale_reused"),
        ("roto.txt", "record_unusable"),
        ("Sub", "live"),
        ("viejo.txt", "stale_not_in_use"),
        ("ñandú.bin", "live"),
    ]
    .iter()
    .map(|(n, c)| (n.to_string(), *c))
    .collect();
    assert_eq!(got, want);
    let by = |n: &str| l.entries.iter().find(|e| e.name == n).unwrap();
    assert_eq!(
        by("reusado.txt").state,
        EntryState::Reused { found_sequence: 2 }
    );
    assert_eq!(
        by("viejo.txt").state,
        EntryState::NotInUse { found_sequence: 2 }
    );
    assert!(matches!(
        by("roto.txt").state,
        EntryState::RecordUnusable {
            state: RecordState::Damaged,
            ..
        }
    ));
    assert_eq!(l.live().count(), 4);
    // Nombres: sustituto suelto decodificado con U+FFFD y marcado; unidades originales intactas.
    let lossy = by("b\u{FFFD}c");
    assert!(lossy.name_lossy);
    assert_eq!(lossy.name_units, [0x62, 0xD800, 0x63]);
    let n = by("ñandú.bin");
    assert_eq!((n.size, n.allocated_size), (3000, 4096));
    assert_eq!(by("Informe.txt").namespace, Namespace::Posix);
    assert_eq!(by("Informe.txt").link_count, 2);
}

#[test]
fn hard_links_appear_in_each_directory_and_paths_resolve() {
    let v = open(&tree_volume(false));
    let t = v.tree(&TreeOptions::default(), &NoTreeObserver).unwrap();
    assert_eq!(t.status, TreeStatus::Complete);
    let paths17: Vec<&str> = t
        .nodes
        .iter()
        .filter(|n| n.entry.record() == 17)
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(paths17, ["/a.txt", "/Docs/Informe.txt"]);
    let live: Vec<&str> = t
        .nodes
        .iter()
        .filter(|n| n.entry.state.is_live())
        .map(|n| n.path.as_str())
        .collect();
    assert_eq!(
        live,
        [
            "/",
            "/a.txt",
            "/Docs",
            "/Medio.txt",
            "/Ágil.txt",
            "/Docs/b\u{FFFD}c",
            "/Docs/Informe.txt",
            "/Docs/Sub",
            "/Docs/ñandú.bin",
            "/Docs/Sub/deep.txt",
        ]
    );
    assert_eq!(t.directories, 3);
    assert!(t.anomalies.is_empty(), "{:?}", t.anomalies);
    let orphans: Vec<(u64, OrphanReason)> = t
        .orphans
        .iter()
        .map(|o| (o.record, o.reason.clone()))
        .collect();
    assert_eq!(
        orphans,
        [
            (23, OrphanReason::NotIndexed),
            (26, OrphanReason::ParentNotDirectory),
            (27, OrphanReason::ParentNotInUse),
        ]
    );
    // Rutas sin distinguir mayúsculas, con el separador de Windows y por el nombre 8.3.
    for (p, r) in [
        ("/docs/SUB/Deep.TXT", 21),
        ("\\Docs\\Informe.txt", 17),
        ("/A.TXT", 17),
        ("/medio~1.txt", 18),
        ("/ágil.txt", 28),
        ("/DOCS/ÑANDÚ.BIN", 20),
        ("//Docs//Sub/", 19),
        ("/", 5),
    ] {
        assert_eq!(v.resolve_path(p).unwrap().record(), r, "{p}");
    }
    assert_eq!(v.path_of(21).unwrap(), "/Docs/Sub/deep.txt");
    assert_eq!(v.path_of(17).unwrap(), "/a.txt");
    assert_eq!(v.path_of(5).unwrap(), "/");
    assert_eq!(v.path_of(22).unwrap(), "/Docs/b\u{FFFD}c");
    // Un registro no en uso también tiene ruta (la base de #23).
    assert_eq!(v.path_of(24).unwrap(), "/Docs/viejo.txt");
}

#[test]
fn path_errors_are_typed() {
    let v = open(&tree_volume(false));
    for p in [
        "/Docs/viejo.txt",
        "/Docs/reusado.txt",
        "/Docs/roto.txt",
        "/no",
    ] {
        assert!(
            matches!(v.resolve_path(p), Err(NtfsError::NotFound { .. })),
            "{p}"
        );
    }
    match v.resolve_path("/Docs/nada/x") {
        Err(NtfsError::NotFound { path, component }) => {
            assert_eq!((path.as_str(), component.as_str()), ("/Docs/nada", "nada"))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        v.resolve_path("/a.txt/x"),
        Err(NtfsError::NotADirectory { number: 17 })
    ));
    for p in ["rel", "/Docs/../a.txt", "/./a.txt"] {
        assert!(
            matches!(v.resolve_path(p), Err(NtfsError::InvalidPath { .. })),
            "{p}"
        );
    }
    let orphan = |n| match v.path_of(n) {
        Err(NtfsError::Orphan { reason, .. }) => reason,
        other => panic!("{n}: {other:?}"),
    };
    assert_eq!(orphan(26), OrphanReason::ParentNotDirectory);
    assert_eq!(orphan(27), OrphanReason::ParentNotInUse);
    assert!(matches!(
        v.path_of(25),
        Err(NtfsError::RecordUnusable { .. })
    ));
    assert!(matches!(
        v.list_dir(17),
        Err(NtfsError::NotADirectory { number: 17 })
    ));
}

/// Sub (19) con su índice y su `$FILE_NAME` (padre y nombre dados).
fn sub_record(parent: u64, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut entries = extra.to_vec();
    entries.push(named_entry(fref(21, 1), fref(19, 1), "deep.txt", 3, None));
    entries.push(index_entry(0, None, None));
    let sub_index = index_root_value(&entries, 4096, false);
    record(
        19,
        1,
        IN_USE | DIRECTORY,
        &[
            si(),
            res(0x30, "", &file_name(parent, "Sub", 3, 0x1000_0000, 0), 1),
            res(0x90, "$I30", &sub_index, 2),
        ],
        1024,
    )
}

#[test]
fn parent_loops_and_reused_parents_are_orphans() {
    // Sub (19) dice ser hijo de sí mismo.
    let mut s = tree_volume(false);
    s.put_record(19, &sub_record(fref(19, 1), &[]));
    let v = open(&s);
    match v.path_of(21) {
        Err(NtfsError::Orphan { reason, .. }) => assert_eq!(reason, OrphanReason::Loop),
        other => panic!("{other:?}"),
    }
    // deep.txt con la secuencia de Sub equivocada en su referencia al padre.
    let mut s = tree_volume(false);
    s.put_record(
        21,
        &record(
            21,
            1,
            IN_USE,
            &[
                si(),
                res(0x30, "", &file_name(fref(19, 7), "deep.txt", 3, 0x20, 0), 1),
            ],
            1024,
        ),
    );
    let v = open(&s);
    match v.path_of(21) {
        Err(NtfsError::Orphan { reason, parent, .. }) => {
            assert_eq!(reason, OrphanReason::ParentReused { found_sequence: 1 });
            assert_eq!(
                parent,
                FileReference {
                    record: 19,
                    sequence: 7
                }
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn upcase_comes_from_the_volume_when_valid() {
    let v = open(&tree_volume(true));
    assert_eq!(v.upcase().source(), &UpcaseSource::Volume);
    assert_eq!(v.resolve_path("/DOCS/ÑANDÚ.BIN").unwrap().record(), 20);
    assert_eq!(v.resolve_path("/ÁGIL.TXT").unwrap().record(), 28);
    let v = open(&tree_volume(false));
    assert!(matches!(v.upcase().source(), UpcaseSource::SimpleFold(_)));
}

/// Volumen con el nodo VCN 1 de la raíz sustituido.
fn with_vcn1(block: &[u8]) -> Synth {
    let mut s = tree_volume(false);
    s.put_clusters(TREE_ROOT_INDX_LCN + 1, block);
    s
}

/// Comprueba que la raíz sigue listando lo del nodo VCN 0 y que lo perdido queda huérfano.
fn rest_still_listed(v: &NtfsVolume, code: &str, at: NodeId) {
    let l = v.list_dir(5).unwrap();
    assert!(codes(&l).contains(&(at, code)), "{code}: {:?}", l.anomalies);
    assert_eq!(names(&l), ["Medio.txt", "Ágil.txt"], "{code}");
    assert_eq!(v.resolve_path("/Ágil.txt").unwrap().record(), 28, "{code}");
    // El camino de búsqueda pasa por el nodo dañado: se recorre todo el índice.
    assert_eq!(v.resolve_path("/Medio.txt").unwrap().record(), 18, "{code}");
    assert!(matches!(
        v.resolve_path("/Docs"),
        Err(NtfsError::NotFound { .. })
    ));
    // Los directorios de debajo se siguen listando.
    assert_eq!(v.list_dir(16).unwrap().live().count(), 4, "{code}");
    let t = v.tree(&TreeOptions::default(), &NoTreeObserver).unwrap();
    assert_eq!(t.status, TreeStatus::Complete);
    assert!(t.anomalies.iter().any(|a| a.issue.code() == code));
    let orphan = |r: u64| {
        t.orphans
            .iter()
            .find(|o| o.record == r)
            .map(|o| o.reason.clone())
    };
    assert_eq!(orphan(16), Some(OrphanReason::NotIndexed), "{code}");
    assert_eq!(orphan(19), Some(OrphanReason::ParentOrphan), "{code}");
    assert_eq!(v.path_of(21).unwrap(), "/Docs/Sub/deep.txt");
}

#[test]
fn damaged_index_nodes_are_typed_and_skipped() {
    let [_, vcn1] = tree_root_nodes();
    // Fixup: el tramo 3 no termina en el USN.
    let mut b = vcn1.clone();
    b[4 * 512 - 1] ^= 0xFF;
    rest_still_listed(
        &open(&with_vcn1(&b)),
        "index_block_fixup_mismatch",
        NodeId::Vcn(1),
    );
    // USA con otro número de elementos.
    let mut b = vcn1.clone();
    b[6] = 3;
    rest_still_listed(
        &open(&with_vcn1(&b)),
        "index_block_usa_invalid",
        NodeId::Vcn(1),
    );
    // VCN de la cabecera distinto del puntero.
    let mut plain = vcn1.clone();
    apply_fixups(&mut plain).unwrap();
    plain[0x10] = 7;
    protect(&mut plain);
    rest_still_listed(&open(&with_vcn1(&plain)), "index_block_vcn", NodeId::Vcn(1));
    // Firma (nodo a ceros).
    rest_still_listed(
        &open(&with_vcn1(&[0u8; 4096])),
        "index_block_signature",
        NodeId::Vcn(1),
    );
    // Cabecera: bytes en uso más allá del nodo.
    let mut plain = vcn1.clone();
    apply_fixups(&mut plain).unwrap();
    plain[0x1C..0x20].copy_from_slice(&5000u32.to_le_bytes());
    protect(&mut plain);
    rest_still_listed(
        &open(&with_vcn1(&plain)),
        "index_header_bounds",
        NodeId::Vcn(1),
    );
    // Sector ilegible dentro del nodo: nunca se interpreta.
    let s = tree_volume(false);
    let bad = (TREE_ROOT_INDX_LCN + 1) * 4096 / 512 + 5;
    let v = NtfsVolume::open(Arc::new(
        FaultyDevice::new(MemDevice::new(s.image.clone(), 512).unwrap()).with_bad([bad]),
    ))
    .unwrap();
    rest_still_listed(&v, "index_block_unreadable", NodeId::Vcn(1));
    assert!(v.list_dir(5).unwrap().has_unreadable());
}

/// Raíz de [`tree_volume`] con otro `$BITMAP` (y el tipo del atributo de bitmap dado).
fn root_with(bitmap: u8, bitmap_type: u32) -> Vec<u8> {
    record(
        5,
        5,
        IN_USE | DIRECTORY,
        &[
            si(),
            res(0x30, "", &file_name(root(), ".", 3, 0x1000_0000, 0), 1),
            res(
                0x90,
                "$I30",
                &index_root_value(&[index_entry(0, None, Some(0))], 4096, true),
                2,
            ),
            with_id(
                non_resident(
                    0xA0,
                    "$I30",
                    0,
                    1,
                    &encode_runs(&[(Some(TREE_ROOT_INDX_LCN), 2)]),
                    8192,
                    8192,
                    8192,
                ),
                3,
            ),
            res(bitmap_type, "$I30", &[bitmap, 0, 0, 0, 0, 0, 0, 0], 4),
        ],
        1024,
    )
}

#[test]
fn bitmap_loops_and_subnode_pointers_are_checked() {
    // Nodo VCN 1 libre según $BITMAP: su contenido es residual y no se lista.
    let mut s = tree_volume(false);
    s.put_record(5, &root_with(0b01, 0xB0));
    rest_still_listed(&open(&s), "index_node_unallocated", NodeId::Vcn(0));
    // Puntero de VCN 0 a un subnodo fuera de $INDEX_ALLOCATION.
    let vcn0 = |sub: u64| {
        indx_block(
            0,
            &[
                named_entry(fref(18, 1), root(), "Medio.txt", 1, Some(sub)),
                named_entry(fref(28, 1), root(), "Ágil.txt", 3, None),
                index_entry(0, None, None),
            ],
            4096,
            true,
        )
    };
    let mut s = tree_volume(false);
    s.put_clusters(TREE_ROOT_INDX_LCN, &vcn0(5));
    rest_still_listed(&open(&s), "index_subnode_out_of_range", NodeId::Vcn(0));
    // Ciclo: la hoja apunta de vuelta a VCN 0.
    let mut s = tree_volume(false);
    s.put_clusters(TREE_ROOT_INDX_LCN, &vcn0(1));
    s.put_clusters(
        TREE_ROOT_INDX_LCN + 1,
        &indx_block(
            1,
            &[
                named_entry(fref(17, 1), root(), "a.txt", 1, Some(0)),
                index_entry(0, None, None),
            ],
            4096,
            true,
        ),
    );
    let v = open(&s);
    let l = v.list_dir(5).unwrap();
    assert!(
        codes(&l).contains(&(NodeId::Vcn(1), "index_loop")),
        "{:?}",
        l.anomalies
    );
    assert_eq!(names(&l), ["a.txt", "Medio.txt", "Ágil.txt"]);
    // Sin $BITMAP del índice: se informa y los nodos se leen igual.
    let mut s = tree_volume(false);
    s.put_record(5, &root_with(0b11, 0xB8));
    let l = open(&s).list_dir(5).unwrap();
    assert_eq!(codes(&l), [(NodeId::Root, "index_bitmap_unusable")]);
    assert_eq!(names(&l).len(), 4);
}

#[test]
fn directory_loops_depth_limits_and_cancellation() {
    // Sub (19) lista a Docs (16): ciclo de directorios.
    let mut s = tree_volume(false);
    s.put_record(
        19,
        &sub_record(
            fref(16, 1),
            &[named_entry(fref(16, 1), fref(19, 1), "arriba", 3, None)],
        ),
    );
    let v = open(&s);
    let t = v.tree(&TreeOptions::default(), &NoTreeObserver).unwrap();
    assert_eq!(t.status, TreeStatus::Complete);
    let issues: Vec<(&str, &str)> = t
        .anomalies
        .iter()
        .map(|a| (a.path.as_str(), a.issue.code()))
        .collect();
    assert!(
        issues.contains(&("/Docs/Sub/arriba", "tree_loop")),
        "{issues:?}"
    );
    // El registro 16 no tiene ese nombre en Sub.
    assert!(
        issues.contains(&("/Docs/Sub", "entry_name_not_in_record")),
        "{issues:?}"
    );
    // Profundidad y número de nodos.
    let v = open(&tree_volume(false));
    let t = v
        .tree(
            &TreeOptions {
                max_depth: 1,
                ..TreeOptions::default()
            },
            &NoTreeObserver,
        )
        .unwrap();
    assert!(t
        .anomalies
        .iter()
        .any(|a| a.issue == TreeIssue::Depth { record: 19 }));
    assert!(!t.nodes.iter().any(|n| n.path == "/Docs/Sub/deep.txt"));
    let t = v
        .tree(
            &TreeOptions {
                max_nodes: 3,
                ..TreeOptions::default()
            },
            &NoTreeObserver,
        )
        .unwrap();
    assert_eq!((t.status, t.nodes.len()), (TreeStatus::Limit, 3));
    struct Cancel;
    impl TreeObserver for Cancel {
        fn cancelled(&self) -> bool {
            true
        }
    }
    let t = v.tree(&TreeOptions::default(), &Cancel).unwrap();
    assert_eq!((t.status, t.nodes.len()), (TreeStatus::Cancelled, 1));
}

#[test]
fn small_index_blocks_use_512_byte_vcns() {
    let mut s = tree_volume(false);
    // Nodos de 1 KiB en clústeres de 4 KiB: el VCN cuenta unidades de 512 B (el nodo del byte
    // 1024 es el VCN 2).
    let mut cl = vec![0u8; 4096];
    let leaf = indx_block(
        2,
        &[
            named_entry(fref(17, 1), root(), "a.txt", 1, None),
            index_entry(0, None, None),
        ],
        1024,
        false,
    );
    cl[1024..2048].copy_from_slice(&leaf);
    s.put_clusters(TREE_ROOT_INDX_LCN, &cl);
    let r = record(
        5,
        5,
        IN_USE | DIRECTORY,
        &[
            si(),
            res(
                0x90,
                "$I30",
                &index_root_value(&[index_entry(0, None, Some(2))], 1024, true),
                2,
            ),
            with_id(
                non_resident(
                    0xA0,
                    "$I30",
                    0,
                    0,
                    &encode_runs(&[(Some(TREE_ROOT_INDX_LCN), 1)]),
                    4096,
                    4096,
                    4096,
                ),
                3,
            ),
            res(0xB0, "$I30", &[0b10, 0, 0, 0, 0, 0, 0, 0], 4),
        ],
        1024,
    );
    s.put_record(5, &r);
    let l = open(&s).list_dir(5).unwrap();
    assert_eq!(names(&l), ["a.txt"]);
    assert_eq!(codes(&l), [(NodeId::Root, "index_block_size_mismatch")]);
}

#[test]
fn node_parsing_rules() {
    let entries = [
        named_entry(fref(17, 1), root(), "a.txt", 1, None),
        named_entry(fref(18, 1), root(), "b.txt", 1, None),
        index_entry(0, None, None),
    ];
    let value = index_root_value(&entries, 4096, false);
    let (info, node) = parse_root(&value).unwrap();
    assert_eq!(
        (info.attr_type, info.collation, info.block_size),
        (0x30, 1, 4096)
    );
    assert_eq!(node.entries.len(), 3);
    assert!(node.issues.is_empty());
    // Raíz: tamaño, tipo indexado y colación.
    assert_eq!(
        parse_root(&value[..0x1F]).unwrap_err().code(),
        "index_root_too_short"
    );
    let mut v = value.clone();
    v[0] = 0x80;
    assert_eq!(parse_root(&v).unwrap_err().code(), "index_root_type");
    let mut v = value.clone();
    v[4] = 2;
    assert_eq!(parse_root(&v).unwrap_err().code(), "index_root_collation");
    // Cabecera: primera entrada desalineada, dentro de la cabecera o más allá de lo usado;
    // bytes en uso fuera del valor; asignados menores que lo usado.
    for (at, val) in [
        (0x10, 0x14u32),
        (0x10, 0x08),
        (0x10, 0x1000),
        (0x14, 0x4000),
        (0x18, 0x10),
    ] {
        let mut v = value.clone();
        v[at..at + 4].copy_from_slice(&val.to_le_bytes());
        assert_eq!(
            parse_root(&v).unwrap_err().code(),
            "index_header_bounds",
            "{at} {val}"
        );
    }
    // Entradas: longitud no válida (la anterior se conserva), clave que no cabe, sin final.
    let first = 0x20;
    let second = first + entries[0].len();
    let mut v = value.clone();
    v[second + 8..second + 10].copy_from_slice(&0x0Cu16.to_le_bytes());
    let n = parse_root(&v).unwrap().1;
    assert_eq!(n.entries.len(), 1);
    assert_eq!(n.issues[0].code(), "index_entry_length");
    let mut v = value.clone();
    v[second + 8..second + 10].copy_from_slice(&0x1000u16.to_le_bytes());
    assert_eq!(
        parse_root(&v).unwrap().1.issues[0].code(),
        "index_entry_length"
    );
    let mut v = value.clone();
    v[first + 0x0A..first + 0x0C].copy_from_slice(&0x400u16.to_le_bytes());
    assert_eq!(
        parse_root(&v).unwrap().1.issues[0].code(),
        "index_entry_key_length"
    );
    let short = index_root_value(&entries[..2], 4096, false);
    assert_eq!(
        parse_root(&short).unwrap().1.issues,
        [IndexIssue::MissingLast]
    );
    // Clave que no es un $FILE_NAME válido: la entrada se ignora y el resto se lee.
    let mut v = value.clone();
    v[first + 0x10 + 0x41] = 9; // espacio de nombres
    let n = parse_root(&v).unwrap().1;
    assert_eq!(n.issues[0].code(), "index_entry_key");
    assert_eq!(n.entries.len(), 2);
    // Nodo INDX: firma, fixups y VCN.
    let mut b = indx_block(4, &entries, 4096, false);
    assert_eq!(parse_block(&mut b.clone(), 4).unwrap().entries.len(), 3);
    assert_eq!(
        parse_block(&mut b.clone(), 5).unwrap_err().code(),
        "index_block_vcn"
    );
    b[0] = b'F';
    assert_eq!(
        parse_block(&mut b, 4).unwrap_err().code(),
        "index_block_signature"
    );
    assert_eq!(
        parse_node(&[0u8; 8], 0, NodeId::Root).unwrap_err().code(),
        "index_header_bounds"
    );
}

/// Fuzzing determinista de nodos: raíces e `INDX` mutados (con la protección rehecha la mitad de
/// las veces) nunca provocan pánicos y lo aceptado queda dentro del nodo.
#[test]
fn index_node_fuzz_never_panics() {
    let mut rng = xorshift(0x1D30_0022);
    let [vcn0, vcn1] = tree_root_nodes();
    let root_value = index_root_value(
        &[
            named_entry(fref(17, 1), root(), "a.txt", 1, Some(1)),
            index_entry(0, None, Some(0)),
        ],
        4096,
        true,
    );
    let mut accepted = 0;
    for i in 0..20_000u32 {
        let base = if i % 3 == 0 { &vcn0 } else { &vcn1 };
        let mut b = base.clone();
        if i.is_multiple_of(2) {
            apply_fixups(&mut b).unwrap();
            for _ in 0..1 + rng() % 6 {
                let at = (rng() % 0x200) as usize;
                b[at] = rng() as u8;
            }
            protect(&mut b);
        } else {
            for _ in 0..1 + rng() % 6 {
                let at = (rng() % 4096) as usize;
                b[at] = rng() as u8;
            }
        }
        let vcn = u64::from(i % 3 != 0);
        if let Ok(n) = parse_block(&mut b, vcn) {
            accepted += 1;
            for e in &n.entries {
                assert!(e.offset < 4096);
            }
        }
        let mut v = root_value.clone();
        for _ in 0..1 + rng() % 4 {
            let at = (rng() % v.len() as u64) as usize;
            v[at] = rng() as u8;
        }
        if let Ok((_, n)) = parse_root(&v) {
            assert!(n.entries.len() <= v.len() / 0x10);
        }
    }
    assert!(accepted > 3000, "{accepted}");
}

/// Fuzzing de volúmenes con nodos y registros de directorio mutados: el árbol termina siempre,
/// sin pánicos, con los límites respetados.
#[test]
fn tree_fuzz_on_mutated_volumes() {
    let mut rng = xorshift(0x7EE0_0022);
    let base = tree_volume(false);
    let regions: Vec<(usize, usize)> = vec![
        ((TREE_ROOT_INDX_LCN * 4096) as usize, 8192),
        (base.record_offset(5) as usize, 1024),
        (base.record_offset(16) as usize, 1024),
        (base.record_offset(19) as usize, 1024),
    ];
    let mut opened = 0;
    for i in 0..300u32 {
        let mut image = base.image.clone();
        let (at, len) = regions[(i as usize) % regions.len()];
        for _ in 0..1 + rng() % 8 {
            let o = at + (rng() % len as u64) as usize;
            image[o] = rng() as u8;
        }
        let Ok(v) = NtfsVolume::open(Arc::new(MemDevice::new(image, 512).unwrap())) else {
            continue;
        };
        opened += 1;
        let t = v.tree(&TreeOptions::default(), &NoTreeObserver).unwrap();
        assert!(t.nodes.len() < 200);
        for n in t.nodes.iter().skip(1) {
            let _ = v.resolve_path(&n.path);
            let _ = v.path_of(n.entry.record());
        }
    }
    assert!(opened > 250, "{opened}");
}
