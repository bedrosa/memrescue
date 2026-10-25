//! Pruebas de las entradas residuales de los índices (#23) sobre volúmenes sintéticos
//! (`crate::testvol::residual_volume` y `tree_volume`): espacio libre de los nodos, nodos libres,
//! índice de un directorio borrado, entradas caducadas, restos mal formados, registros
//! reutilizados y dañados, combinación con la MFT sin repetidos, sectores ilegibles y fuzzing.

use std::sync::Arc;

use mr_io::{FaultyDevice, MemDevice};

use super::*;
use crate::deleted::{scan_deleted, DeletedScan, DeletedStatus, NoObserver, ScanOptions};
use crate::mft::RecordState;
use crate::testvol::{
    fref, malformed_slack, named_entry, residual_volume, tree_volume, xorshift, Synth,
    RESIDUAL_INDX_LCN,
};
use crate::values::Namespace;
use crate::Recoverability;

/// LBA de 512 B del nodo `INDX` libre que las pruebas hacen ilegible (VCN 2).
fn unreadable_node_lba() -> u64 {
    (RESIDUAL_INDX_LCN + 2) * 4096 / 512 + 3
}

/// Volumen de `residual_volume` con un sector ilegible en el nodo del VCN 2.
fn open_faulty(s: &Synth) -> NtfsVolume {
    let dev = FaultyDevice::new(MemDevice::new(s.image.clone(), 512).unwrap())
        .with_bad([unreadable_node_lba()]);
    NtfsVolume::open(Arc::new(dev)).unwrap()
}

fn open(s: &Synth) -> NtfsVolume {
    NtfsVolume::open(Arc::new(MemDevice::new(s.image.clone(), 512).unwrap())).unwrap()
}

fn scan(v: &NtfsVolume) -> DeletedScan {
    scan_deleted(v, &ScanOptions::default(), &NoObserver)
        .unwrap()
        .unwrap()
}

fn ctx(dir: FileReference, in_use: bool) -> DirContext {
    DirContext {
        directory: dir,
        in_use,
        record_count: 40,
    }
}

#[test]
fn strict_candidates_accept_only_complete_entries() {
    let root = fref(5, 5);
    let mut buf = vec![0u8; 8];
    buf.extend(named_entry(fref(18, 1), root, "bueno.txt", 1, None));
    buf.extend(malformed_slack(root));
    buf.extend(named_entry(fref(19, 1), root, "otro.txt", 1, Some(7)));
    let c = ctx(FileReference::from_u64(root), true);
    let (found, rejected) = parse_candidates(&buf, 8, buf.len(), &c);
    let names: Vec<&str> = found
        .iter()
        .map(|x| x.key.file_name.name.as_str())
        .collect();
    assert_eq!(names, ["bueno.txt", "otro.txt"]);
    assert_eq!(found[0].offset, 8);
    assert_eq!(found[0].reference, FileReference::from_u64(fref(18, 1)));
    assert_eq!(rejected, 2, "fuera de la MFT y registro del sistema");
    // Un tramo que corta la última entrada no la acepta.
    let (cut, _) = parse_candidates(&buf, 8, buf.len() - 8, &c);
    assert_eq!(cut.len(), 1);
    // Otro directorio: nada es candidato.
    let other = ctx(FileReference::from_u64(fref(16, 1)), true);
    assert_eq!(
        parse_candidates(&buf, 0, buf.len(), &other),
        (Vec::new(), 0)
    );
    // Directorio borrado: la clave lleva la secuencia de antes de liberarlo.
    let gone = ctx(FileReference::from_u64(fref(5, 6)), false);
    assert_eq!(parse_candidates(&buf, 0, buf.len(), &gone).0.len(), 2);
    let live_newer = ctx(FileReference::from_u64(fref(5, 6)), true);
    assert!(parse_candidates(&buf, 0, buf.len(), &live_newer)
        .0
        .is_empty());
    // Límites fuera del búfer y tramos vacíos no leen fuera.
    assert!(parse_candidates(&buf, buf.len(), usize::MAX, &c)
        .0
        .is_empty());
    assert!(parse_candidates(&[], 0, 100, &c).0.is_empty());
}

#[test]
fn directory_scan_reports_slack_free_nodes_and_skips_bad_nodes() {
    let s = residual_volume();
    let v = open_faulty(&s);
    let (found, st) = residual_entries(&v, 5).unwrap();
    assert_eq!(st.directories, 1);
    assert_eq!(st.nodes, 2, "VCN 0 y VCN 1");
    assert_eq!(
        st.nodes_unreadable, 1,
        "VCN 2: sector ilegible, nunca se interpreta"
    );
    assert_eq!(st.nodes_invalid, 1, "VCN 3: fixups");
    assert_eq!(st.free_nodes, 1);
    assert_eq!(st.rejected, 2);
    assert!(st.slack_bytes > 0 && !st.truncated);
    let got: Vec<(EntryOrigin, NodeId, &str)> = found
        .iter()
        .map(|r| (r.origin, r.node, r.key.name.as_str()))
        .collect();
    use EntryOrigin::{IndexFreeNode as F, IndexSlack as S};
    assert_eq!(
        got,
        [
            (S, NodeId::Vcn(0), "borrado.txt"),
            (S, NodeId::Vcn(0), "borrado.txt"),
            (S, NodeId::Vcn(0), "reusado.txt"),
            (S, NodeId::Vcn(0), "dañado.txt"),
            (S, NodeId::Vcn(0), "vivo.txt"),
            (F, NodeId::Vcn(1), "borrado.txt"),
            (F, NodeId::Vcn(1), "libre.txt"),
            (F, NodeId::Vcn(1), "DOSLAR~1.TXT"),
            (F, NodeId::Vcn(1), "dos largo.txt"),
        ]
    );
    assert_eq!(st.accepted, 9);
    assert!(found
        .iter()
        .all(|r| r.directory == FileReference::from_u64(fref(5, 5))));
    assert!(!found
        .iter()
        .any(|r| r.key.name == "secreto.txt" || r.key.name == "roto.txt"));
    // Sin el sector ilegible, el VCN 2 es un nodo libre más.
    let (found, st) = residual_entries(&open(&s), 5).unwrap();
    assert_eq!((st.nodes, st.nodes_unreadable, st.free_nodes), (3, 0, 2));
    assert!(found.iter().any(|r| r.key.name == "secreto.txt"));
    // El directorio borrado: su raíz de índice entera es residual.
    let (found, st) = residual_entries(&v, 16).unwrap();
    assert_eq!(st.directories, 1);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].origin, EntryOrigin::DeletedDirectoryIndex);
    assert_eq!(found[0].node, NodeId::Root);
    assert_eq!(found[0].reference, FileReference::from_u64(fref(24, 1)));
    // Un fichero no es un directorio: nada que leer.
    let (found, st) = residual_entries(&v, 17).unwrap();
    assert!(found.is_empty());
    assert_eq!(st, ResidualStats::default());
}

#[test]
fn scan_combines_residual_entries_with_the_mft_once() {
    let s = residual_volume();
    let v = open_faulty(&s);
    let d = scan(&v);
    let st = d.residual.clone().unwrap();
    assert_eq!(st.directories, 2, "raíz y el directorio borrado");
    assert_eq!(st.accepted, 10);
    assert_eq!(d.residual_live, 1, "la copia de vivo.txt");
    assert_eq!(
        d.residual_duplicates, 1,
        "borrado.txt dos veces en el mismo nodo"
    );
    let records: Vec<(u64, &str)> = d
        .entries
        .iter()
        .map(|e| (e.record, e.status.code()))
        .collect();
    assert_eq!(
        records,
        [
            (16, "deleted"),
            (18, "deleted"),
            (24, "deleted"),
            (19, "residual_record_reused"),
            (20, "residual_record_unusable"),
            (21, "residual_record_free"),
            (23, "residual_record_unusable"),
        ]
    );
    // Encontrado por la MFT y por dos índices: una sola entrada con los tres orígenes.
    let e = d.entry(18).unwrap();
    assert_eq!(
        e.origins,
        [
            EntryOrigin::MftRecord,
            EntryOrigin::IndexSlack,
            EntryOrigin::IndexFreeNode
        ]
    );
    assert_eq!(e.index_refs.len(), 2);
    assert_eq!(
        e.names.len(),
        1,
        "el nombre del índice confirma el del registro"
    );
    assert_eq!(e.recoverability(), Some(Recoverability::Full));
    assert_eq!(e.path.as_deref(), Some("/borrado.txt"));
    let e = d.entry(24).unwrap();
    assert_eq!(
        e.origins,
        [EntryOrigin::MftRecord, EntryOrigin::DeletedDirectoryIndex]
    );
    assert_eq!(e.path.as_deref(), Some("/Viejo/hijo.txt"));
    // Registro reutilizado: nombre, tamaños y fechas de la clave; nada recuperable.
    let e = d.entry(19).unwrap();
    assert_eq!(e.status, DeletedStatus::RecordReused { found_sequence: 2 });
    assert_eq!((e.name.as_str(), e.sequence), ("reusado.txt", 1));
    assert_eq!(e.origins, [EntryOrigin::IndexSlack]);
    assert_eq!(e.recoverability(), Some(Recoverability::None));
    assert!(e.streams.is_empty());
    assert_eq!(e.times.created, 11, "fechas de la clave del índice");
    assert_eq!(e.path.as_deref(), Some("/reusado.txt"));
    // Registro dañado: con dos nombres (DOS y largo) en una sola entrada, el largo preferido.
    let e = d.entry(23).unwrap();
    assert!(matches!(
        e.status,
        DeletedStatus::RecordUnusable {
            state: RecordState::Damaged,
            ..
        }
    ));
    assert_eq!(e.name, "dos largo.txt");
    assert_ne!(e.namespace, Namespace::Dos);
    assert_eq!(e.names.len(), 2);
    assert_eq!(e.recoverability(), Some(Recoverability::None));
    let e = d.entry(21).unwrap();
    assert_eq!(e.status, DeletedStatus::RecordFree { found_sequence: 4 });
    assert_eq!(e.origins, [EntryOrigin::IndexFreeNode]);
    // Nunca un fichero vivo, ni lo del nodo ilegible o con fixups rotos.
    assert!(d.entry(17).is_none());
    assert!(d.entry(26).is_none() && d.entry(27).is_none());
    // Cada (registro, secuencia) aparece una sola vez.
    let mut keys: Vec<(u64, u16)> = d.entries.iter().map(|e| (e.record, e.sequence)).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), d.entries.len());
}

#[test]
fn residual_search_is_optional_and_skipped_when_focused() {
    let v = open_faulty(&residual_volume());
    let off = scan_deleted(
        &v,
        &ScanOptions {
            residual: false,
            ..ScanOptions::default()
        },
        &NoObserver,
    )
    .unwrap()
    .unwrap();
    assert!(off.residual.is_none());
    assert!(off
        .entries
        .iter()
        .all(|e| e.origins == [EntryOrigin::MftRecord]));
    let focus = scan_deleted(
        &v,
        &ScanOptions {
            focus: Some(18),
            ..ScanOptions::default()
        },
        &NoObserver,
    )
    .unwrap()
    .unwrap();
    assert!(focus.residual.is_none());
    assert_eq!(focus.entries.len(), 1);
    // Límite de entradas residuales.
    let lim = scan_deleted(
        &v,
        &ScanOptions {
            max_residual: 3,
            ..ScanOptions::default()
        },
        &NoObserver,
    )
    .unwrap()
    .unwrap();
    assert!(lim.residual.unwrap().truncated);
}

#[test]
fn stale_entries_of_live_indexes_are_residual_but_damage_is_not() {
    let v = open(&tree_volume(true));
    let d = scan(&v);
    // /Docs/reusado.txt → 23 en uso con otra secuencia.
    let e = d.entry(23).unwrap();
    assert_eq!(e.status, DeletedStatus::RecordReused { found_sequence: 2 });
    assert_eq!(e.origins, [EntryOrigin::IndexStale]);
    assert_eq!(e.path.as_deref(), Some("/Docs/reusado.txt"));
    // /Docs/viejo.txt → 24 no en uso: la entrada de la MFT con el índice como segundo origen.
    let e = d.entry(24).unwrap();
    assert_eq!(e.status, DeletedStatus::Deleted);
    assert_eq!(e.origins, [EntryOrigin::MftRecord, EntryOrigin::IndexStale]);
    // roto.txt (registro BAAD) es un daño, no un borrado; fuera.txt (fuera de la MFT), rechazada.
    assert!(d.entry(25).is_none());
    assert!(d.entry(99).is_none());
    assert_eq!(d.residual.as_ref().unwrap().rejected, 1);
    // Ningún vivo.
    for live in [16, 17, 18, 19, 20, 21, 22, 28] {
        assert!(d.entry(live).is_none(), "{live}");
    }
}

#[test]
fn fuzz_candidate_parsing_never_reads_out_of_range() {
    let root = fref(5, 5);
    let c = ctx(FileReference::from_u64(root), true);
    let mut base = Vec::new();
    for i in 0..6 {
        base.extend(named_entry(
            fref(18 + i, 1),
            root,
            &format!("f{i}.txt"),
            1,
            None,
        ));
    }
    base.extend(malformed_slack(root));
    let mut rnd = xorshift(0x05EE_D23B);
    for i in 0..5000 {
        let mut buf = if i % 4 == 0 {
            (0..(rnd() % 2048) as usize).map(|_| rnd() as u8).collect()
        } else {
            let mut b = base.clone();
            for _ in 0..(1 + rnd() % 12) {
                let at = (rnd() as usize) % b.len();
                b[at] = rnd() as u8;
            }
            b
        };
        if i % 7 == 0 {
            let cut = (rnd() as usize) % (buf.len() + 1);
            buf.truncate(cut);
        }
        let from = (rnd() as usize) % (buf.len() + 16);
        let to = (rnd() as usize) % (buf.len() + 64);
        let (found, _) = parse_candidates(&buf, from, to, &c);
        for x in &found {
            assert!(x.offset >= from && x.offset.is_multiple_of(8));
            let len = usize::from(u16::from_le_bytes([buf[x.offset + 8], buf[x.offset + 9]]));
            assert!(x.offset + len <= to.min(buf.len()));
            assert!(c.is_parent(x.key.file_name.parent));
            assert!(c.plausible_reference(x.reference));
            assert_eq!(
                x.key.units.len(),
                x.key.file_name.name.encode_utf16().count()
            );
        }
    }
}

#[test]
fn fuzz_mutated_index_nodes_never_report_live_files_or_recoverable_residuals() {
    let s = residual_volume();
    let mut rnd = xorshift(0x23B0_F00D);
    let first = (RESIDUAL_INDX_LCN * 4096) as usize;
    for _ in 0..300 {
        let mut img = s.image.clone();
        for _ in 0..(1 + rnd() % 40) {
            let at = first + (rnd() as usize) % (4 * 4096);
            img[at] = rnd() as u8;
        }
        let v = NtfsVolume::open(Arc::new(MemDevice::new(img, 512).unwrap())).unwrap();
        let d = scan(&v);
        for e in &d.entries {
            assert!(
                !(e.record == 17 && e.sequence == 1),
                "vivo.txt nunca es un borrado"
            );
            if !e.status.has_record() {
                assert!(e.streams.is_empty());
                assert!(matches!(
                    e.recoverability(),
                    None | Some(Recoverability::None)
                ));
                assert!(e.from_index() && !e.from_mft());
            }
        }
        let mut keys: Vec<(u64, u16, bool)> = d
            .entries
            .iter()
            .map(|e| (e.record, e.sequence, e.status.has_record()))
            .collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "sin repetidos");
    }
}
