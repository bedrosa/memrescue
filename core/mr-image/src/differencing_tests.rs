//! Pruebas de las cadenas de discos diferenciales VHD y VHDX (incidencia 8) con imágenes
//! sintéticas construidas según las especificaciones (`diff_builder`).

use crate::crc32c::crc32c;
use crate::diff_builder::{self as db, Writes, SECTOR};
use crate::differencing::parent_role;
use crate::differencing::MAX_CHAIN_DEPTH;
use crate::external::ApprovedFiles;
use crate::open::{external_hints, open_image, open_image_with_parents, OpenOptions, OpenedImage};
use mr_io::{Allocation, BlockDevice, IoError};
use std::path::{Path, PathBuf};

const MIB: u64 = 1024 * 1024;

struct Dir {
    dir: tempfile::TempDir,
}

impl Dir {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Contenido de todos los ficheros (para comprobar que nada cambia).
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let m = std::fs::metadata(&p).unwrap();
                (p.clone(), std::fs::read(&p).unwrap(), m.modified().unwrap())
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// Padres aprobados con los papeles `parent:1`, `parent:2`… en el orden dado.
fn ext(paths: &[&PathBuf]) -> ApprovedFiles {
    ApprovedFiles::new(
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| (parent_role(i + 1), *p)),
    )
    .unwrap()
}

fn open(p: &Path, e: &ApprovedFiles) -> mr_io::Result<OpenedImage> {
    open_image_with_parents(p, OpenOptions::default(), e, false)
}

/// Como [`open`], aceptando explícitamente un vínculo que no coincide.
fn open_allow(p: &Path, e: &ApprovedFiles) -> mr_io::Result<OpenedImage> {
    open_image_with_parents(p, OpenOptions::default(), e, true)
}

fn read_all(d: &dyn BlockDevice) -> Vec<u8> {
    let mut v = vec![0u8; d.size() as usize];
    d.read_at(0, &mut v).unwrap();
    v
}

/// Lecturas en trozos irregulares que cruzan sectores, bloques y capas.
fn check_reads(d: &dyn BlockDevice, expect: &[u8]) {
    assert_eq!(d.size(), expect.len() as u64);
    assert!(read_all(d) == expect, "lectura completa distinta");
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..200 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let start = (x % expect.len() as u64) as usize;
        let len = ((x >> 32) % 9000) as usize % (expect.len() - start) + 1;
        let mut b = vec![0u8; len];
        d.read_at(start as u64, &mut b).unwrap();
        assert!(b == expect[start..start + len], "{start}+{len}");
    }
}

/// `allocation` expandida a un indicador por sector (`true` = `Data`), comprobando que los
/// tramos cubren el rango exactamente y en orden.
fn data_map(d: &dyn BlockDevice) -> Vec<bool> {
    let n = d.sector_count();
    let mut v = Vec::new();
    for (lba, c, k) in d.allocation(0, n).unwrap() {
        assert_eq!(lba, v.len() as u64);
        v.extend(std::iter::repeat_n(k == Allocation::Data, c as usize));
    }
    assert_eq!(v.len() as u64, n);
    v
}

// ---------------------------------------------------------------------------------------------
// VHD
// ---------------------------------------------------------------------------------------------

const VHD_SIZE: u64 = 40 * 4096; // 320 sectores, bloques de 4 KiB (8 sectores)
const VHD_BS: u32 = 4096;
const ID_BASE: [u8; 16] = [0xB0; 16];
const ID_CHILD: [u8; 16] = [0xC1; 16];
const ID_GRAND: [u8; 16] = [0xC2; 16];

struct VhdChain {
    base: Vec<u8>,
    child: Vec<u8>,
    grand: Vec<u8>,
    w: [Writes; 3],
}

fn vhd_chain() -> VhdChain {
    let w0 = db::writes(0, (0..80).chain(200..210));
    let w1 = db::writes(1, [3, 5, 6, 7, 100, 205].into_iter().chain(16..24));
    let w2 = db::writes(2, [5, 101, 300]);
    let base = db::vhd_layer(VHD_SIZE, VHD_BS, ID_BASE, None, &w0);
    let child = db::vhd_layer(
        VHD_SIZE,
        VHD_BS,
        ID_CHILD,
        Some(&db::VhdParent::new(ID_BASE, "base.vhd")),
        &w1,
    );
    let grand = db::vhd_layer(
        VHD_SIZE,
        VHD_BS,
        ID_GRAND,
        Some(&db::VhdParent::new(ID_CHILD, "child.vhd")),
        &w2,
    );
    VhdChain {
        base,
        child,
        grand,
        w: [w0, w1, w2],
    }
}

fn flat(size: u64, layers: &[&Writes]) -> Vec<u8> {
    let mut v = vec![0u8; size as usize];
    for w in layers {
        db::apply(&mut v, w);
    }
    v
}

#[test]
fn vhd_chains_of_one_and_two_levels_fall_through_per_sector() {
    let c = vhd_chain();
    let d = Dir::new();
    let base = d.write("base.vhd", &c.base);
    let child = d.write("child.vhd", &c.child);
    let grand = d.write("grand.vhd", &c.grand);
    let before = d.snapshot();

    // Un nivel.
    let o = open(&child, &ext(&[&base])).unwrap();
    let expect1 = flat(VHD_SIZE, &[&c.w[0], &c.w[1]]);
    check_reads(o.device.as_ref(), &expect1);
    assert_eq!(o.external_files.len(), 1);
    assert!(o.external_files[0].ends_with("base.vhd"));
    assert!(
        o.description
            .starts_with("VHD diferencial, bloques de 4 KiB; padre 1: "),
        "{}",
        o.description
    );
    assert!(o.description.contains("(VHD dinámico, bloques de 4 KiB)"));
    // La marca de tiempo del sintético no es la fecha del fichero: solo se informa.
    assert!(
        o.description.contains("marca de tiempo"),
        "{}",
        o.description
    );
    assert_eq!(o.discarded_blocks, None);
    let map = data_map(o.device.as_ref());
    for (s, &is_data) in map.iter().enumerate() {
        let s = s as u64;
        let written = c.w[0].contains_key(&s) || c.w[1].contains_key(&s);
        assert_eq!(is_data, written, "sector {s}");
    }

    // Dos niveles.
    let o = open(&grand, &ext(&[&child, &base])).unwrap();
    let expect2 = flat(VHD_SIZE, &[&c.w[0], &c.w[1], &c.w[2]]);
    check_reads(o.device.as_ref(), &expect2);
    assert_eq!(o.external_files.len(), 2);
    assert!(o.description.contains("; padre 2: "));
    let map = data_map(o.device.as_ref());
    for (s, &is_data) in map.iter().enumerate() {
        let s = s as u64;
        assert_eq!(
            is_data,
            c.w.iter().any(|w| w.contains_key(&s)),
            "sector {s}"
        );
    }
    // Visto con sectores de 4096 B: los mismos bytes.
    let o = open_image_with_parents(
        &grand,
        OpenOptions {
            sector_size: 4096,
            ..OpenOptions::default()
        },
        &ext(&[&child, &base]),
        false,
    )
    .unwrap();
    assert_eq!(o.device.sector_size(), 4096);
    assert!(read_all(o.device.as_ref()) == expect2);
    drop(o);
    assert_eq!(d.snapshot(), before, "los ficheros no deben cambiar");
}

#[test]
fn vhd_without_parents_reports_external_reference_and_hints() {
    let c = vhd_chain();
    let d = Dir::new();
    let base = d.write("base.vhd", &c.base);
    let child = d.write("child.vhd", &c.child);
    let grand = d.write("grand.vhd", &c.grand);
    match open_image(&child, OpenOptions::default()) {
        Err(IoError::ExternalReference { format, reference }) => {
            assert_eq!((format, reference.as_str()), ("VHD", "base.vhd"))
        }
        r => panic!("{:?}", r.err()),
    }
    // Cadena incompleta: el padre indicado también es diferencial.
    match open(&grand, &ext(&[&child])) {
        Err(IoError::ExternalReference { reference, .. }) => assert_eq!(reference, "base.vhd"),
        r => panic!("{:?}", r.err()),
    }
    let h = external_hints(&child).unwrap();
    let get = |k: &str| h.iter().find(|x| x.key == k).map(|x| x.value.clone());
    assert_eq!(get("parent_name").as_deref(), Some("base.vhd"));
    assert_eq!(
        get("parent_unique_id").as_deref(),
        Some("b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0")
    );
    assert_eq!(
        get("parent_timestamp").as_deref(),
        Some("2009-09-04 22:51:36 UTC")
    );
    assert_eq!(get("locator_W2ru").as_deref(), Some(".\\base.vhd"));
    assert_eq!(get("locator_W2ku").as_deref(), Some("C:\\vm\\base.vhd"));
    assert!(external_hints(&base).unwrap().is_empty());
}

#[test]
fn vhd_link_mismatch_is_refused_unless_explicitly_accepted() {
    let c = vhd_chain();
    let d = Dir::new();
    let child = d.write("child.vhd", &c.child);
    let other = d.write(
        "other.vhd",
        &db::vhd_layer(VHD_SIZE, VHD_BS, [0x77; 16], None, &c.w[0]),
    );
    match open(&child, &ext(&[&other])) {
        Err(IoError::ExternalMismatch { format, detail }) => {
            assert_eq!(format, "VHD");
            assert!(detail.contains("77777777-7777"), "{detail}");
            assert!(detail.contains("b0b0b0b0-b0b0"), "{detail}");
        }
        r => panic!("{:?}", r.err()),
    }
    let o = open_allow(&child, &ext(&[&other])).unwrap();
    assert!(
        o.description
            .contains("AVISO: vínculo con el padre 1 NO verificado"),
        "{}",
        o.description
    );
    check_reads(o.device.as_ref(), &flat(VHD_SIZE, &[&c.w[0], &c.w[1]]));
}

#[test]
fn wrong_parents_loops_depth_and_sizes_are_rejected() {
    let c = vhd_chain();
    let d = Dir::new();
    let base = d.write("base.vhd", &c.base);
    let child = d.write("child.vhd", &c.child);
    let grand = d.write("grand.vhd", &c.grand);
    let is_mismatch = |r: mr_io::Result<OpenedImage>, what: &str| match r {
        Err(IoError::ExternalMismatch { .. }) => {}
        r => panic!("{what}: {:?}", r.err()),
    };
    // Sobran ficheros: el padre 1 ya es un disco base.
    is_mismatch(open(&child, &ext(&[&base, &grand])), "sobra uno");
    // Una imagen que no es diferencial no usa ficheros externos.
    is_mismatch(open(&base, &ext(&[&child])), "base con padre");
    // Ciclo por ruta: el diferencial como su propio padre.
    is_mismatch(open(&child, &ext(&[&child])), "ciclo por ruta");
    // Ciclo por identidad: una copia del hijo se presenta como su abuelo.
    let copy = d.write("copy.vhd", &c.grand);
    let child_of_grand = d.write(
        "loop.vhd",
        &db::vhd_layer(
            VHD_SIZE,
            VHD_BS,
            [0x99; 16],
            Some(&db::VhdParent::new(ID_GRAND, "grand.vhd")),
            &c.w[2],
        ),
    );
    is_mismatch(
        open(&child_of_grand, &ext(&[&grand, &child, &copy])),
        "ciclo por identidad",
    );
    // Tamaño virtual distinto.
    let small = d.write(
        "small.vhd",
        &db::vhd_layer(VHD_SIZE - 4096, VHD_BS, ID_BASE, None, &Writes::new()),
    );
    is_mismatch(open(&child, &ext(&[&small])), "tamaño");
    // Formato distinto (un VHDX como padre de un VHD).
    let vhdx = d.write(
        "base.vhdx",
        &db::vhdx_layer(
            4 * MIB,
            MIB as u32,
            [1; 16],
            None,
            &Writes::new(),
            &[],
            crc32c,
        ),
    );
    is_mismatch(open(&child, &ext(&[&vhdx])), "formato");
    // Padre que no existe o no es un fichero: lo rechaza la política al aprobar la lista.
    assert!(ApprovedFiles::new([(parent_role(1), d.dir.path().join("no.vhd"))]).is_err());
    assert!(matches!(
        ApprovedFiles::new([(parent_role(1), PathBuf::from(r"\\.\PhysicalDrive0"))]),
        Err(IoError::DevicePathRejected(_))
    ));
    // Papeles que no son parent:1 … parent:N: se rechazan sin abrir la cadena.
    for roles in [
        vec!["parent:2"],
        vec!["padre"],
        vec!["parent:1", "parent:3"],
        vec!["parent:01"],
    ] {
        let files = [&base, &grand, &copy];
        let a = ApprovedFiles::new(roles.iter().zip(files).map(|(r, p)| (*r, p))).unwrap();
        is_mismatch(open(&child, &a), &format!("papeles {roles:?}"));
    }
    // El mismo fichero con dos papeles: lo rechaza la lista aprobada.
    assert!(ApprovedFiles::new([(parent_role(1), &base), (parent_role(2), &base)]).is_err());
}

#[test]
fn chains_longer_than_the_limit_are_rejected() {
    let d = Dir::new();
    let w = db::writes(0, [1]);
    let mut paths = Vec::new();
    let id = |i: usize| [i as u8 + 1; 16];
    let base = d.write("l0.vhd", &db::vhd_layer(VHD_SIZE, VHD_BS, id(0), None, &w));
    paths.push(base);
    for i in 1..=MAX_CHAIN_DEPTH + 1 {
        let wi = db::writes(i as u8, [i as u64]);
        let img = db::vhd_layer(
            VHD_SIZE,
            VHD_BS,
            id(i),
            Some(&db::VhdParent::new(id(i - 1), &format!("l{}.vhd", i - 1))),
            &wi,
        );
        paths.push(d.write(&format!("l{i}.vhd"), &img));
    }
    // Exactamente MAX_CHAIN_DEPTH padres: se abre.
    let top = &paths[MAX_CHAIN_DEPTH];
    let parents: Vec<&PathBuf> = paths[..MAX_CHAIN_DEPTH].iter().rev().collect();
    let o = open(top, &ext(&parents)).unwrap();
    let mut b = vec![0u8; 512];
    o.device.read_at(7 * 512, &mut b).unwrap();
    assert_eq!(b, db::sector_data(7, 7));
    // Uno más: se rechaza.
    let top = &paths[MAX_CHAIN_DEPTH + 1];
    let parents: Vec<&PathBuf> = paths[..=MAX_CHAIN_DEPTH].iter().rev().collect();
    match open(top, &ext(&parents)) {
        Err(IoError::ExternalMismatch { detail, .. }) => {
            assert!(detail.contains("más de 16 padres"), "{detail}")
        }
        r => panic!("{:?}", r.err()),
    }
}

#[test]
fn vhd_tolerant_mode_never_substitutes_parent_data_for_discarded_blocks() {
    let c = vhd_chain();
    let d = Dir::new();
    let base = d.write("base.vhd", &c.base);
    let mut bad = c.child.clone();
    // Bloque 2 del hijo (sectores 16..24, escritos enteros) apuntando fuera del fichero.
    bad[1536 + 8..1536 + 12].copy_from_slice(&0x00FF_0000u32.to_be_bytes());
    let child = d.write("child.vhd", &bad);
    assert!(matches!(
        open(&child, &ext(&[&base])),
        Err(IoError::Corrupt { format: "VHD", .. })
    ));
    let o = open_image_with_parents(
        &child,
        OpenOptions {
            tolerant: true,
            ..OpenOptions::default()
        },
        &ext(&[&base]),
        false,
    )
    .unwrap();
    assert_eq!(o.discarded_blocks, Some(1));
    let mut b = vec![0u8; 512];
    assert!(matches!(
        o.device.read_at(17 * 512, &mut b),
        Err(IoError::Read { offset, .. }) if offset == 17 * 512
    ));
    assert_eq!(
        o.device.allocation(16, 8).unwrap(),
        vec![(16, 8, Allocation::Data)]
    );
    o.device.read_at(3 * 512, &mut b).unwrap();
    assert_eq!(b, db::sector_data(1, 3));
}

// ---------------------------------------------------------------------------------------------
// VHDX
// ---------------------------------------------------------------------------------------------

const VHDX_SIZE: u64 = 3 * MIB + 512 * 1024; // 4 bloques de 1 MiB (2048 sectores), el último medio
const DWG_BASE: [u8; 16] = [0xDA; 16];
const DWG_CHILD: [u8; 16] = [0xDC; 16];
const DWG_GRAND: [u8; 16] = [0xDD; 16];

struct VhdxChain {
    base: Vec<u8>,
    child: Vec<u8>,
    grand: Vec<u8>,
    expect: Vec<u8>,
    data: Vec<bool>,
}

fn vhdx(parent: Option<&db::VhdxParent>, dwg: [u8; 16], w: &Writes, st: &[(u64, u64)]) -> Vec<u8> {
    db::vhdx_layer(VHDX_SIZE, MIB as u32, dwg, parent, w, st, crc32c)
}

/// Base: bloques 0, 1 y 3 con datos (FULLY_PRESENT). Hijo: parcial en 0 y 1, completo en 2 y
/// ZERO en 3. Nieto: parcial en 0 y en 3.
fn vhdx_chain() -> VhdxChain {
    let w0 = db::writes(0, (0..10).chain(2048..4096).chain([6200]));
    let w1 = db::writes(1, [5, 2048 + 7].into_iter().chain(4096..6144));
    let w2 = db::writes(2, [6, 7000]);
    let base = vhdx(None, DWG_BASE, &w0, &[]);
    let child = vhdx(
        Some(&db::VhdxParent::new(DWG_BASE, "base.vhdx")),
        DWG_CHILD,
        &w1,
        &[(3, db::PAYLOAD_ZERO)],
    );
    let grand = vhdx(
        Some(&db::VhdxParent::new(DWG_CHILD, "child.vhdx")),
        DWG_GRAND,
        &w2,
        &[],
    );
    let mut expect = flat(VHDX_SIZE, &[&w0, &w1]);
    expect[(3 * MIB) as usize..].fill(0); // bloque 3 ZERO en el hijo
    db::apply(&mut expect, &w2);
    let spb = 2048u64;
    let data = (0..VHDX_SIZE / SECTOR)
        .map(|s| {
            if w2.contains_key(&s) || w1.contains_key(&s) {
                true
            } else if s / spb == 3 {
                false
            } else {
                w0.keys().any(|k| k / spb == s / spb)
            }
        })
        .collect();
    VhdxChain {
        base,
        child,
        grand,
        expect,
        data,
    }
}

#[test]
fn vhdx_chains_use_sector_bitmaps_and_block_states() {
    let c = vhdx_chain();
    let d = Dir::new();
    let base = d.write("base.vhdx", &c.base);
    let child = d.write("child.vhdx", &c.child);
    let grand = d.write("grand.vhdx", &c.grand);
    let before = d.snapshot();
    let o = open(&grand, &ext(&[&child, &base])).unwrap();
    assert!(
        o.description
            .starts_with("VHDX diferencial, bloques de 1 MiB"),
        "{}",
        o.description
    );
    assert!(o.description.contains("(VHDX dinámico, bloques de 1 MiB"));
    check_reads(o.device.as_ref(), &c.expect);
    assert_eq!(data_map(o.device.as_ref()), c.data);
    // Un nivel.
    let o = open(&child, &ext(&[&base])).unwrap();
    let mut b = vec![0u8; 4 * 512];
    o.device.read_at(4 * 512, &mut b).unwrap();
    assert_eq!(&b[..512], &db::sector_data(0, 4)[..]);
    assert_eq!(&b[512..1024], &db::sector_data(1, 5)[..]);
    assert_eq!(&b[1024..1536], &db::sector_data(0, 6)[..]);
    drop(o);
    assert_eq!(d.snapshot(), before);
    // Sin padres: referencia externa con la ruta relativa del localizador.
    match open_image(&child, OpenOptions::default()) {
        Err(IoError::ExternalReference { reference, .. }) => assert_eq!(reference, ".\\base.vhdx"),
        r => panic!("{:?}", r.err()),
    }
    let h = external_hints(&grand).unwrap();
    let keys: Vec<&str> = h.iter().map(|x| x.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "locator_type",
            "parent_linkage",
            "relative_path",
            "volume_path",
            "absolute_win32_path"
        ]
    );
    assert_eq!(h[0].value, "B04AEFB7-D19E-4A81-B789-25B8E9445913");
    assert_eq!(h[1].value, db::guid_text(&DWG_CHILD));
}

#[test]
fn vhdx_undefined_reads_the_parent_and_unmapped_reads_zeros() {
    let w0 = db::writes(0, (0..2048).chain(2048..4096));
    let base = vhdx(None, DWG_BASE, &w0, &[]);
    let child = vhdx(
        Some(&db::VhdxParent::new(DWG_BASE, "b")),
        DWG_CHILD,
        &Writes::new(),
        &[(0, db::PAYLOAD_UNDEFINED), (1, db::PAYLOAD_UNMAPPED)],
    );
    let d = Dir::new();
    let (b, c) = (d.write("b.vhdx", &base), d.write("c.vhdx", &child));
    let o = open(&c, &ext(&[&b])).unwrap();
    let mut expect = flat(VHDX_SIZE, &[&w0]);
    expect[MIB as usize..2 * MIB as usize].fill(0);
    check_reads(o.device.as_ref(), &expect);
    assert_eq!(
        o.device.allocation(0, 7168).unwrap(),
        vec![
            (0, 2048, Allocation::Data),
            (2048, 5120, Allocation::Sparse)
        ]
    );
}

#[test]
fn vhdx_link_checks_parent_linkage_and_linkage2() {
    let c = vhdx_chain();
    let d = Dir::new();
    let child = d.write("child.vhdx", &c.child);
    let w = db::writes(0, [1]);
    let other = d.write("other.vhdx", &vhdx(None, [0x42; 16], &w, &[]));
    match open(&child, &ext(&[&other])) {
        Err(IoError::ExternalMismatch { detail, .. }) => {
            assert!(detail.contains("DataWriteGuid"), "{detail}")
        }
        r => panic!("{:?}", r.err()),
    }
    let o = open_allow(&child, &ext(&[&other])).unwrap();
    assert!(o.description.contains("NO verificado"));
    // parent_linkage2 coincide: verificado.
    let mut p = db::VhdxParent::new(DWG_BASE, "x");
    p.entries
        .push(("parent_linkage2".into(), db::guid_text(&[0x42; 16])));
    let child2 = d.write("c2.vhdx", &vhdx(Some(&p), DWG_CHILD, &w, &[]));
    let o = open(&child2, &ext(&[&other])).unwrap();
    assert!(
        o.description.contains("parent_linkage2"),
        "{}",
        o.description
    );
    // Sin parent_linkage: no se puede verificar.
    let p = db::VhdxParent {
        entries: vec![("relative_path".into(), "x.vhdx".into())],
    };
    let child3 = d.write("c3.vhdx", &vhdx(Some(&p), DWG_CHILD, &w, &[]));
    assert!(matches!(
        open(&child3, &ext(&[&other])),
        Err(IoError::ExternalMismatch { .. })
    ));
}

#[test]
fn vhdx_partial_block_without_bitmap_is_corrupt_or_discarded() {
    let c = vhdx_chain();
    let d = Dir::new();
    let base = d.write("base.vhdx", &c.base);
    let mut bad = c.child.clone();
    // Entrada del mapa de bits del grupo 0 (índice 4096 de la BAT) a NOT_PRESENT.
    let o = (db::VHDX_BAT_OFF + 4096 * 8) as usize;
    bad[o..o + 8].fill(0);
    let child = d.write("child.vhdx", &bad);
    assert!(matches!(
        open(&child, &ext(&[&base])),
        Err(IoError::Corrupt { format: "VHDX", .. })
    ));
    let t = open_image_with_parents(
        &child,
        OpenOptions {
            tolerant: true,
            ..OpenOptions::default()
        },
        &ext(&[&base]),
        false,
    )
    .unwrap();
    // Bloques 0 y 1 (parciales) descartados: error, nunca los datos del padre.
    assert_eq!(t.discarded_blocks, Some(2));
    assert!(t.device.read_at(0, &mut [0u8; 512]).is_err());
    assert!(t.device.read_at(MIB, &mut [0u8; 512]).is_err());
    let mut b = vec![0u8; 512];
    t.device.read_at(2 * MIB, &mut b).unwrap();
    assert_eq!(b, db::sector_data(1, 4096));
    // Estado de mapa de bits no válido: igual.
    bad[o..o + 8].copy_from_slice(&3u64.to_le_bytes());
    let child = d.write("child2.vhdx", &bad);
    assert!(matches!(
        open(&child, &ext(&[&base])),
        Err(IoError::Corrupt { .. })
    ));
}

#[test]
fn random_corruption_of_parent_locators_never_panics() {
    let c = vhd_chain();
    let x = vhdx_chain();
    let d = Dir::new();
    let vbase = d.write("base.vhd", &c.base);
    let xbase = d.write("base.vhdx", &x.base);
    let loc = db::vhdx_meta_item(&x.child, 5) as usize;
    let mut r = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r
    };
    for i in 0..200 {
        // VHD: localizadores (576..768 de la cabecera dinámica), nombre, id y datos.
        let mut b = c.child.clone();
        for _ in 0..1 + next() % 6 {
            let pos = match next() % 3 {
                0 => 512 + 576 + (next() % 192) as usize,
                1 => 512 + 40 + (next() % 536) as usize,
                _ => 1536 + 512 + (next() % 2048) as usize,
            };
            b[pos] = next() as u8;
        }
        db::vhd_seal(&mut b[512..1536], 36);
        let p = d.write(&format!("v{i}.vhd"), &b);
        let _ = external_hints(&p);
        if let Ok(o) = open(&p, &ext(&[&vbase])) {
            let _ = o.device.read_at(0, &mut vec![0u8; VHD_SIZE as usize]);
        }
        // VHDX: elemento del localizador.
        let mut b = x.child.clone();
        for _ in 0..1 + next() % 6 {
            let pos = loc + (next() % 400) as usize;
            b[pos] = next() as u8;
        }
        let p = d.write(&format!("x{i}.vhdx"), &b);
        let _ = external_hints(&p);
        if let Ok(o) = open(&p, &ext(&[&xbase])) {
            let _ = o.device.allocation(0, o.device.sector_count());
            let _ = o.device.read_at(0, &mut vec![0u8; 1 << 16]);
        }
        let _ = std::fs::remove_file(d.dir.path().join(format!("v{i}.vhd")));
        let _ = std::fs::remove_file(d.dir.path().join(format!("x{i}.vhdx")));
    }
}
