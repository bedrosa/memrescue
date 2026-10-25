//! Pruebas de la arena, el orden, los filtros, las rutas y la construcción desde volúmenes NTFS
//! sintéticos (`mr_fs_ntfs::testvol`).

use std::sync::Arc;

use mr_fs_ntfs::record::flags::{DIRECTORY, IN_USE};
use mr_fs_ntfs::testvol::{file_name, file_name_units, fref, record, res, tree_volume, Synth};
use mr_fs_ntfs::NtfsVolume;
use mr_io::MemDevice;

use crate::arena::NODE_BYTES;
use crate::filter::{parse_categories, query, Filter, NameRules, TimeField};
use crate::name::units;
use crate::ntfs::{
    build_ntfs, BuildObserver, NoBuildObserver, NtfsBuildOptions, LOOSE_DELETED_FOLDER,
    METADATA_FOLDER, ORPHANS_FOLDER,
};
use crate::{
    flags, Category, DataState, FsRef, Limits, NameReliability, NewNode, NodeId, NodeKind, Times,
    Vfs, VfsBuilder, VfsError,
};

fn dir_node(name: &[u16]) -> NewNode<'_> {
    NewNode {
        name,
        kind: NodeKind::Directory,
        category: Category::Live,
        name_reliability: NameReliability::Structure,
        data: None,
        flags: 0,
        namespace: Some(1),
        size: 0,
        allocated_size: 0,
        times: Times::default(),
        attributes: 0,
        origin: None,
    }
}

fn file_node(name: &[u16], size: u64, data: DataState, record: u64) -> NewNode<'_> {
    NewNode {
        kind: NodeKind::File,
        data: Some(data),
        size,
        times: Times {
            modified: size * 10,
            ..Times::default()
        },
        origin: Some(FsRef {
            record,
            sequence: 1,
            parent_record: 5,
            parent_sequence: 5,
        }),
        ..dir_node(name)
    }
}

fn builder(limits: Limits) -> VfsBuilder {
    VfsBuilder::new(limits, dir_node(&[])).unwrap()
}

/// Árbol pequeño: /docs/{b.txt, A.txt, a.TXT (borrado), sub/c.bin}, /z.tmp, /[Metadatos]/$MFT.
fn sample() -> Vfs {
    let mut b = builder(Limits::default());
    let docs = b.push(NodeId::ROOT, dir_node(&units("docs"))).unwrap();
    b.push(docs, file_node(&units("b.txt"), 10, DataState::Integro, 30))
        .unwrap();
    b.push(docs, file_node(&units("A.txt"), 20, DataState::Integro, 31))
        .unwrap();
    let a_txt = units("a.TXT");
    let mut del = file_node(&a_txt, 30, DataState::IntegroNoVerificable, 32);
    del.category = Category::Deleted;
    del.flags = flags::DELETED;
    b.push(docs, del).unwrap();
    let sub = b.push(docs, dir_node(&units("sub"))).unwrap();
    b.push(sub, file_node(&units("c.bin"), 40, DataState::Parcial, 33))
        .unwrap();
    b.push(
        NodeId::ROOT,
        file_node(&units("z.tmp"), 50, DataState::Integro, 34),
    )
    .unwrap();
    let meta = b
        .push(
            NodeId::ROOT,
            NewNode::synthetic(&units("[Metadatos]"), Category::Metadata),
        )
        .unwrap();
    let mft_name = units("$MFT");
    let mut mft = file_node(&mft_name, 60, DataState::Integro, 0);
    mft.category = Category::Metadata;
    b.push(meta, mft).unwrap();
    b.finish()
}

fn names(v: &Vfs, ids: &[NodeId]) -> Vec<String> {
    ids.iter().map(|i| v.path(*i)).collect()
}

#[test]
fn node_layout_is_compact() {
    // Objetivo de INV-01: 128 B por nodo como mucho (comprobado también al compilar).
    assert_eq!(NODE_BYTES, 104);
}

#[test]
fn children_are_ordered_deterministically() {
    let v = sample();
    let kids: Vec<String> = v
        .children(NodeId::ROOT)
        .iter()
        .map(|k| v.name(*k))
        .collect();
    assert_eq!(kids, ["docs", "z.tmp", "[Metadatos]"]);
    let docs = v.find("/docs").unwrap();
    let kids: Vec<String> = v.children(docs).iter().map(|k| v.name(*k)).collect();
    // Sin distinguir mayúsculas; a igualdad, las unidades originales («A» < «a»).
    assert_eq!(kids, ["A.txt", "a.TXT", "b.txt", "sub"]);
    let a = v.find("/docs/A.txt").unwrap();
    let a2 = v.find("/docs/a.TXT").unwrap();
    assert!(v.flags(a) & flags::DUPLICATE_NAME != 0);
    assert!(v.flags(a2) & flags::DUPLICATE_NAME != 0);
    assert_eq!(
        v.flags(v.find("/docs/b.txt").unwrap()) & flags::DUPLICATE_NAME,
        0
    );
    // El orden no depende del orden de construcción.
    let mut b = builder(Limits::default());
    for n in ["sub2", "b", "B", "a"] {
        b.push(NodeId::ROOT, dir_node(&units(n))).unwrap();
    }
    let v2 = b.finish();
    let kids: Vec<String> = v2
        .children(NodeId::ROOT)
        .iter()
        .map(|k| v2.name(*k))
        .collect();
    assert_eq!(kids, ["a", "B", "b", "sub2"]);
}

#[test]
fn node_info_paths_and_lookup() {
    let v = sample();
    let c = v.find("/docs/sub/c.bin").unwrap();
    let i = v.node(c).unwrap();
    assert_eq!(i.kind, NodeKind::File);
    assert_eq!(i.data, Some(DataState::Parcial));
    assert_eq!(i.size, 40);
    assert_eq!(i.depth, 3);
    assert_eq!(i.origin.unwrap().record, 33);
    assert_eq!(v.path(c), "/docs/sub/c.bin");
    assert_eq!(v.path(NodeId::ROOT), "/");
    assert_eq!(v.find("/"), Some(NodeId::ROOT));
    assert_eq!(v.find(""), Some(NodeId::ROOT));
    assert_eq!(v.find("\\docs\\SUB\\C.BIN"), Some(c));
    assert_eq!(v.find("/docs/nada"), None);
    assert_eq!(v.find_record(33), vec![c]);
    assert_eq!(v.ancestry(c).len(), 3);
    assert_eq!(
        v.relative_path(v.find("/docs").unwrap(), c).unwrap(),
        "sub/c.bin"
    );
    assert_eq!(v.relative_path(c, NodeId::ROOT), None);
    let d = v.node(v.find("/docs").unwrap()).unwrap();
    assert_eq!((d.data, d.child_count), (None, 4));
    let s = v.stats();
    assert_eq!(
        (s.nodes, s.files, s.directories, s.synthetic),
        (10, 6, 3, 1)
    );
    assert_eq!(s.deleted, 1);
    assert_eq!(s.category(Category::Metadata), 2);
    assert_eq!(s.data(DataState::Integro), 4);
    assert!(v.node(NodeId(99)).is_none());
    assert!(v.children(NodeId(99)).is_empty());
}

#[test]
fn limits_are_typed_errors() {
    let mut b = builder(Limits {
        max_nodes: 3,
        ..Limits::default()
    });
    let d = b.push(NodeId::ROOT, dir_node(&units("d"))).unwrap();
    b.push(d, dir_node(&units("e"))).unwrap();
    assert!(matches!(
        b.push(d, dir_node(&units("f"))),
        Err(VfsError::LimitExceeded {
            what: "nodos",
            limit: 3
        })
    ));
    let mut b = builder(Limits {
        max_depth: 2,
        ..Limits::default()
    });
    let d = b.push(NodeId::ROOT, dir_node(&units("d"))).unwrap();
    let e = b.push(d, dir_node(&units("e"))).unwrap();
    assert!(matches!(
        b.push(e, dir_node(&units("f"))),
        Err(VfsError::LimitExceeded {
            what: "profundidad",
            ..
        })
    ));
    let mut b = builder(Limits {
        max_name_units: 5,
        ..Limits::default()
    });
    b.push(NodeId::ROOT, dir_node(&units("abc"))).unwrap();
    assert!(matches!(
        b.push(NodeId::ROOT, dir_node(&units("def"))),
        Err(VfsError::LimitExceeded { .. })
    ));
    // Un fichero no tiene hijos; un padre que no existe es un error.
    let mut b = builder(Limits::default());
    let f = b
        .push(
            NodeId::ROOT,
            file_node(&units("f"), 1, DataState::Integro, 1),
        )
        .unwrap();
    assert!(matches!(
        b.push(f, dir_node(&units("x"))),
        Err(VfsError::NotAContainer(_))
    ));
    assert!(matches!(
        b.push(NodeId(50), dir_node(&units("x"))),
        Err(VfsError::NoSuchNode(50))
    ));
    assert!(VfsBuilder::new(
        Limits {
            max_nodes: 0,
            ..Limits::default()
        },
        dir_node(&[])
    )
    .is_err());
}

#[test]
fn lossless_names_round_trip() {
    let lone = [0x62u16, 0xD800, 0x63];
    let trail = [0xDC00u16];
    let mut b = builder(Limits::default());
    b.push(NodeId::ROOT, file_node(&lone, 1, DataState::Integro, 40))
        .unwrap();
    b.push(NodeId::ROOT, file_node(&trail, 1, DataState::Integro, 41))
        .unwrap();
    b.push(
        NodeId::ROOT,
        file_node(&units("a/b"), 1, DataState::Integro, 42),
    )
    .unwrap();
    b.push(
        NodeId::ROOT,
        file_node(&units(".."), 1, DataState::Integro, 43),
    )
    .unwrap();
    let v = b.finish();
    let id = v.find_record(40)[0];
    assert_eq!(v.name_units(id), lone);
    assert_eq!(v.name(id), "b\u{FFFD}c");
    assert_eq!(v.path(id), "/b%uD800c");
    assert_eq!(v.find("/b%uD800c"), Some(id));
    let t = v.find_record(41)[0];
    assert_eq!(v.name_units(t), trail);
    assert_eq!(v.find("/%uDC00"), Some(t));
    let s = v.find_record(42)[0];
    assert_eq!(v.path(s), "/a%2Fb");
    assert_eq!(v.find("/a%2Fb"), Some(s));
    let dd = v.find_record(43)[0];
    assert_eq!(v.path(dd), "/%2E%2E");
    assert_eq!(v.find("/%2E%2E"), Some(dd));
}

#[test]
fn filters_and_queries() {
    let v = sample();
    let all = query(&v, NodeId::ROOT, true, &Filter::default()).unwrap();
    assert_eq!(all.len(), 9);
    assert_eq!(
        names(&v, &all),
        [
            "/docs",
            "/docs/A.txt",
            "/docs/a.TXT",
            "/docs/b.txt",
            "/docs/sub",
            "/docs/sub/c.bin",
            "/z.tmp",
            "/[Metadatos]",
            "/[Metadatos]/$MFT",
        ]
    );
    // Solo hijos directos.
    let top = query(&v, NodeId::ROOT, false, &Filter::default()).unwrap();
    assert_eq!(top.len(), 3);
    // Máscaras: inclusión y exclusión; los directorios aparecen si llevan a algo visible.
    let f = Filter {
        names: NameRules::parse("*.txt;!b*").unwrap(),
        ..Filter::default()
    };
    // Gana la primera regla que coincide: «b.txt» entra por «*.txt».
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(
        names(&v, &r),
        ["/docs", "/docs/A.txt", "/docs/a.TXT", "/docs/b.txt"]
    );
    let f = Filter {
        names: NameRules::parse("!b*;*.txt").unwrap(),
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(names(&v, &r), ["/docs", "/docs/A.txt", "/docs/a.TXT"]);
    let f = Filter {
        names: NameRules::parse("!*.tmp").unwrap(),
        files_only: true,
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert!(!names(&v, &r).contains(&"/z.tmp".to_string()));
    assert_eq!(r.len(), 8);
    // Borrados: el directorio que lleva a ellos y el borrado.
    let f = Filter {
        deleted_only: true,
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(names(&v, &r), ["/docs", "/docs/a.TXT"]);
    // Categoría y estado.
    let f = Filter {
        categories: parse_categories("metadatos").unwrap(),
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(names(&v, &r), ["/[Metadatos]", "/[Metadatos]/$MFT"]);
    let f = Filter {
        data_states: DataState::Parcial.bit(),
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(names(&v, &r), ["/docs", "/docs/sub", "/docs/sub/c.bin"]);
    // Tamaño y fecha.
    let f = Filter {
        size_min: Some(20),
        size_max: Some(40),
        ..Filter::default()
    };
    let r = query(&v, v.find("/docs").unwrap(), true, &f).unwrap();
    assert_eq!(
        names(&v, &r),
        ["/docs/A.txt", "/docs/a.TXT", "/docs/sub", "/docs/sub/c.bin"]
    );
    let f = Filter {
        time_field: TimeField::Modified,
        time_min: Some(450),
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(
        names(&v, &r),
        ["/z.tmp", "/[Metadatos]", "/[Metadatos]/$MFT"]
    );
    // Reglas de ruta relativas al nodo de partida.
    let f = Filter {
        names: NameRules::parse("sub/*").unwrap(),
        ..Filter::default()
    };
    let r = query(&v, v.find("/docs").unwrap(), true, &f).unwrap();
    assert_eq!(names(&v, &r), ["/docs/sub", "/docs/sub/c.bin"]);
    // Filtros incoherentes y nodos inexistentes.
    let bad = Filter {
        size_min: Some(5),
        size_max: Some(1),
        ..Filter::default()
    };
    assert!(query(&v, NodeId::ROOT, true, &bad).is_err());
    assert!(query(&v, NodeId(1000), true, &Filter::default()).is_err());
}

/// Árbol sintético de un millón de nodos con memoria acotada (≤ 128 B por nodo más nombres e
/// hijos) y consultas lineales.
#[test]
fn a_million_nodes_fit_in_the_memory_bound() {
    const N: u32 = 1_000_000;
    let mut b = builder(Limits::default());
    let mut dirs = vec![NodeId::ROOT];
    let mut name = String::new();
    for i in 0..1000u32 {
        name.clear();
        name.push_str(&format!("dir{i:04}"));
        let d = b
            .push(dirs[(i as usize) % dirs.len()], dir_node(&units(&name)))
            .unwrap();
        dirs.push(d);
    }
    let mut k = 0u32;
    while b.len() < N as usize {
        name.clear();
        name.push_str(&format!("f{:07}.dat", N - k));
        let d = dirs[1 + (k as usize % 1000)];
        b.push(
            d,
            file_node(
                &units(&name),
                u64::from(k),
                DataState::Integro,
                u64::from(k),
            ),
        )
        .unwrap();
        k += 1;
    }
    let v = b.finish();
    assert_eq!(v.len(), N as usize);
    let per_node = 128 + 4 + 2 * 12;
    assert!(
        v.memory_bytes() <= u64::from(N) * per_node,
        "{} bytes",
        v.memory_bytes()
    );
    // Orden determinista dentro de un directorio grande.
    let d = v.find("/dir0000").unwrap();
    let kids = v.children(d);
    assert!(kids.len() > 900);
    assert!(kids.windows(2).all(|w| crate::name::cmp_names(
        v.name_units(w[0]),
        v.name_units(w[1])
    )
    .is_le()));
    let f = Filter {
        names: NameRules::parse("F09999??.DAT").unwrap(),
        files_only: true,
        ..Filter::default()
    };
    let r = query(&v, NodeId::ROOT, true, &f).unwrap();
    assert_eq!(
        r.iter()
            .filter(|i| v.kind(**i) == Some(NodeKind::File))
            .count(),
        100
    );
}

// ---------- Construcción desde NTFS ----------

fn open(s: &Synth) -> NtfsVolume {
    NtfsVolume::open(Arc::new(MemDevice::new(s.image.clone(), 512).unwrap())).unwrap()
}

fn si() -> Vec<u8> {
    res(0x10, "", &[0x10; 72], 0)
}

fn fname(parent: u64, name: &str, dir: bool) -> Vec<u8> {
    let attrs = if dir { 0x1000_0000 } else { 0x20 };
    res(0x30, "", &file_name(parent, name, 1, attrs, 0), 1)
}

/// `tree_volume` con borrados añadidos: un directorio borrado con un fichero, un borrado con un
/// nombre con una unidad suelta y un borrado cuyo padre se reutilizó.
fn volume_with_deleted() -> Synth {
    let mut s = tree_volume(true);
    let docs = fref(16, 1);
    s.put_record(
        29,
        &record(
            29,
            2,
            DIRECTORY,
            &[si(), fname(docs, "viejo_dir", true)],
            1024,
        ),
    );
    s.put_record(
        30,
        &record(
            30,
            2,
            0,
            &[
                si(),
                fname(fref(29, 1), "dentro.txt", false),
                res(0x80, "", b"dentro", 2),
            ],
            1024,
        ),
    );
    s.put_record(
        31,
        &record(
            31,
            2,
            0,
            &[
                si(),
                res(
                    0x30,
                    "",
                    &file_name_units(docs, &[0x78, 0xDC00], 1, 0x20, 4),
                    1,
                ),
                res(0x80, "", b"xdc0", 2),
            ],
            1024,
        ),
    );
    s.put_record(
        32,
        &record(
            32,
            2,
            0,
            &[
                si(),
                fname(fref(23, 1), "suelto.txt", false),
                res(0x80, "", b"suelto", 2),
            ],
            1024,
        ),
    );
    // Directorio en uso sin padre válido, con un hijo borrado dentro.
    s.put_record(
        33,
        &record(
            33,
            1,
            IN_USE | DIRECTORY,
            &[si(), fname(fref(24, 1), "huerfano_dir", true)],
            1024,
        ),
    );
    s
}

fn build(s: &Synth, opts: &NtfsBuildOptions) -> Vfs {
    build_ntfs(&open(s), opts, &NoBuildObserver)
        .unwrap()
        .unwrap()
}

#[test]
fn ntfs_tree_volume_merges_live_deleted_and_orphans() {
    let v = build(&volume_with_deleted(), &NtfsBuildOptions::default());
    let get = |p: &str| {
        let id = v.find(p).unwrap_or_else(|| {
            let all: Vec<String> = v
                .preorder(NodeId::ROOT)
                .iter()
                .map(|i| v.path(*i))
                .collect();
            panic!("falta {p}: {all:#?}")
        });
        v.node(id).unwrap()
    };
    // Vivos íntegros (también el enlace duro, en sus dos rutas).
    for p in [
        "/a.txt",
        "/Docs/Informe.txt",
        "/Docs/ñandú.bin",
        "/Docs/Sub/deep.txt",
    ] {
        let n = get(p);
        assert_eq!(n.category, Category::Live, "{p}");
        assert_eq!(n.data, Some(DataState::Integro), "{p}");
        assert_eq!(n.name_reliability, NameReliability::Structure, "{p}");
        assert!(n.has(flags::FROM_INDEX));
    }
    assert_eq!(get("/Docs/ñandú.bin").size, 3000);
    assert!(get("/a.txt").has(flags::HARD_LINK));
    // Nombre con un sustituto suelto: sin pérdida.
    let lossy = v.find("/Docs/b%uD800c").unwrap();
    assert_eq!(v.name_units(lossy), [0x62, 0xD800, 0x63]);
    assert!(v.flags(lossy) & flags::NAME_LOSSY != 0);
    // Registros dañados o referencias no válidas citados por un índice vivo.
    for p in ["/Docs/roto.txt", "/Docs/fuera.txt"] {
        let n = get(p);
        assert_eq!(
            (n.category, n.data),
            (Category::Live, Some(DataState::NoDisponible))
        );
        assert!(n.has(flags::RECORD_DAMAGED), "{p}");
    }
    // Borrado con índice residual: una sola entrada, del recorrido de la MFT.
    let viejo = get("/Docs/viejo.txt");
    assert_eq!(viejo.category, Category::Deleted);
    assert_eq!(viejo.name_reliability, NameReliability::Record);
    assert!(viejo.has(flags::DELETED | flags::FROM_MFT));
    assert!(viejo.has(flags::FROM_INDEX));
    assert_eq!(viejo.data, Some(DataState::SinDatos));
    assert_eq!(v.find_record(24).len(), 1);
    // Entrada residual hacia un registro reutilizado: borrada, nombre residual, sin datos.
    let reusado = get("/Docs/reusado.txt");
    assert_eq!(reusado.category, Category::Deleted);
    assert_eq!(reusado.name_reliability, NameReliability::Residual);
    assert_eq!(reusado.data, Some(DataState::SinDatos));
    // Directorio borrado con su hijo.
    let d = get("/Docs/viejo_dir");
    assert_eq!(
        (d.kind, d.category),
        (NodeKind::Directory, Category::Deleted)
    );
    let dentro = get("/Docs/viejo_dir/dentro.txt");
    assert_eq!(dentro.data, Some(DataState::IntegroNoVerificable));
    assert_eq!(dentro.category, Category::Deleted);
    // Borrado con unidad suelta: sin pérdida.
    let x = v.find_record(31)[0];
    assert_eq!(v.name_units(x), [0x78, 0xDC00]);
    assert_eq!(v.path(x), "/Docs/x%uDC00");
    // Borrado cuyo padre se reutilizó.
    let suelto = get(&format!("/{LOOSE_DELETED_FOLDER}/[Registro 23]/suelto.txt"));
    assert_eq!(suelto.category, Category::Orphan);
    assert!(suelto.has(flags::PARENT_REUSED | flags::DELETED));
    assert_eq!(suelto.data, Some(DataState::IntegroNoVerificable));
    // Huérfanos en uso.
    let otro = get(&format!("/{ORPHANS_FOLDER}/[Registro 16]/otro.txt"));
    assert_eq!(
        (otro.category, otro.data),
        (Category::Orphan, Some(DataState::Integro))
    );
    assert_eq!(otro.name_reliability, NameReliability::Structure);
    get(&format!("/{ORPHANS_FOLDER}/[Registro 21]/colgado.txt"));
    get(&format!("/{ORPHANS_FOLDER}/[Registro 24]/perdido.txt"));
    get(&format!("/{ORPHANS_FOLDER}/[Registro 24]/huerfano_dir"));
    // Carpetas sintéticas.
    for f in [ORPHANS_FOLDER, LOOSE_DELETED_FOLDER] {
        let n = get(&format!("/{f}"));
        assert_eq!(
            (n.kind, n.name_reliability, n.origin),
            (NodeKind::Synthetic, NameReliability::Synthetic, None)
        );
    }
    // Cada registro de usuario en uso aparece; los borrados una vez.
    for r in [29, 30, 31, 32] {
        assert_eq!(v.find_record(r).len(), 1, "registro {r}");
    }
    // Metadatos: si la raíz lista ficheros del sistema, cuelgan de [Metadatos].
    for k in v.children(NodeId::ROOT) {
        if let Some(o) = v.node(*k).unwrap().origin {
            assert!(o.record >= 16, "{}", v.path(*k));
        }
    }
    if let Some(m) = v.find(&format!("/{METADATA_FOLDER}")) {
        for k in v.children(m) {
            assert_eq!(v.category(*k), Some(Category::Metadata));
        }
    }
    assert!(!v.is_partial());
    // Mismo árbol en una segunda construcción.
    let v2 = build(&volume_with_deleted(), &NtfsBuildOptions::default());
    let a: Vec<String> = v
        .preorder(NodeId::ROOT)
        .iter()
        .map(|i| v.path(*i))
        .collect();
    let b: Vec<String> = v2
        .preorder(NodeId::ROOT)
        .iter()
        .map(|i| v2.path(*i))
        .collect();
    assert_eq!(a, b);
}

#[test]
fn ntfs_options_skip_deleted_and_orphans() {
    let v = build(
        &volume_with_deleted(),
        &NtfsBuildOptions {
            deleted: false,
            orphans: false,
            ..NtfsBuildOptions::default()
        },
    );
    assert_eq!(v.stats().deleted, 0);
    assert_eq!(v.stats().category(Category::Orphan), 0);
    assert!(v.find("/Docs/viejo.txt").is_none());
    assert!(v.find("/Docs/a.txt").is_none());
    assert!(v.find("/Docs/Informe.txt").is_some());
}

struct CancelAfter(std::cell::Cell<u32>);

impl BuildObserver for CancelAfter {
    fn cancelled(&self) -> bool {
        let n = self.0.get();
        self.0.set(n.saturating_sub(1));
        n == 0
    }
}

struct Progress(std::cell::RefCell<Vec<(u64, u64)>>);

impl BuildObserver for Progress {
    fn progress(&self, done: u64, total: u64) {
        self.0.borrow_mut().push((done, total));
    }
}

#[test]
fn ntfs_build_cancels_and_reports_progress() {
    let s = volume_with_deleted();
    let vol = open(&s);
    for after in [0, 1, 3, 10, 40] {
        let r = build_ntfs(
            &vol,
            &NtfsBuildOptions::default(),
            &CancelAfter(std::cell::Cell::new(after)),
        )
        .unwrap();
        assert!(r.is_none(), "cancelada tras {after} consultas");
    }
    let p = Progress(std::cell::RefCell::new(Vec::new()));
    build_ntfs(&vol, &NtfsBuildOptions::default(), &p)
        .unwrap()
        .unwrap();
    let v = p.0.borrow();
    assert!(v.iter().all(|(d, t)| *t == 3000 && d <= t));
    assert!(v.windows(2).all(|w| w[0].0 <= w[1].0), "{v:?}");
    assert_eq!(v.last(), Some(&(3000, 3000)));
}

#[test]
fn ntfs_build_respects_the_node_limit() {
    let r = build_ntfs(
        &open(&volume_with_deleted()),
        &NtfsBuildOptions {
            limits: Limits {
                max_nodes: 6,
                ..Limits::default()
            },
            ..NtfsBuildOptions::default()
        },
        &NoBuildObserver,
    );
    assert!(matches!(r, Err(VfsError::LimitExceeded { .. })), "{r:?}");
}
