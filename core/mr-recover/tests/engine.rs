//! Motor de recuperación con árboles sintéticos y contenido en memoria, a través de la vía real
//! de escritura (`mr_api::ApiSink` → `create_output`): nombres hostiles, conflictos, rutas
//! largas, destino prohibido, cancelación, repetición, determinismo, estados de tramo, división y
//! flujos alternativos (INV-01 §9).

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use common::*;
use mr_api::mr_recover::mem::{synthetic_byte, MemContent, MemStream};
use mr_api::mr_recover::source::RangeState;
use mr_api::mr_recover::{
    Capabilities, ConflictAnswer, ConflictDecision, ConflictPolicy, ConflictQuestion, RecoverError,
    RecoverObserver, RunStatus, Selection, SourceError, SplitPolicy, DEFAULT_FILL,
};
use mr_api::mr_vfs::{flags, Category, DataState, NodeId, NodeKind};

fn content() -> MemContent {
    MemContent::new(DEFAULT_FILL)
}

fn bytes_of(record: u64, size: u64) -> Vec<u8> {
    (0..size).map(|i| synthetic_byte(record, i)).collect()
}

fn summary_journal(s: &mr_api::mr_recover::RecoverSummary) -> Vec<Json> {
    journal(Path::new(&s.journal))
}

#[test]
fn hostile_names_stay_inside_and_are_escaped() {
    let mut t = Tree::new();
    let names: Vec<Vec<u16>> = vec![
        units(".."),
        units("."),
        units("a\\..\\..\\x"),
        units("a/b"),
        units("CON"),
        units("nul.txt"),
        units("fin."),
        units("fin "),
        units("a:b"),
        vec![0x31, 0x01],
        vec![0x62, 0xD800, 0x63],
        units("C:"),
        units("\\\\?\\C:"),
    ];
    let mut recs = Vec::new();
    for n in &names {
        let (_, r) = t.node(
            NodeId::ROOT,
            n,
            NodeKind::File,
            Category::Live,
            Some(DataState::Integro),
            10,
            0,
        );
        recs.push(r);
    }
    let vfs = t.finish();
    let outer = tempfile::tempdir().unwrap();
    let dest = outer.path().join("destino");
    let s = run(&vfs, &content(), all(&vfs), &opts(&dest));
    assert_eq!(s.status, RunStatus::Complete);
    assert_eq!(s.totals.files_written, names.len() as u64);
    assert_eq!(s.exit_code(), 0);
    // Nada fuera del destino: el directorio exterior solo tiene `destino`.
    let outside: Vec<_> = std::fs::read_dir(outer.path()).unwrap().collect();
    assert_eq!(outside.len(), 1);
    let files = tree_files(&dest);
    for want in [
        "%2E%2E",
        "%2E",
        "a%5C..%5C..%5Cx",
        "a%2Fb",
        "CON_",
        "nul_.txt",
        "fin%2E",
        "fin%20",
        "a%3Ab",
        "1%01",
        "b%uD800c",
        "C%3A",
        "%5C%5C%3F%5CC%3A",
    ] {
        assert!(files.contains_key(want), "falta {want}: {:?}", files.keys());
    }
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    let lossy = ev
        .values()
        .find(|e| e.get("destino").s() == "b%uD800c")
        .unwrap();
    assert_eq!(lossy.get("nombre_utf16").s(), "0062D8000063");
    assert!(lossy.get("nombre_cambiado").b());
    let con = ev
        .values()
        .find(|e| e.get("destino").s() == "CON_")
        .unwrap();
    assert_eq!(con.get("nombre").s(), "CON");
    assert_no_temps(&dest);
}

#[test]
fn conflicts_within_the_recovery_rename_case_and_deleted_versions() {
    let mut t = Tree::new();
    let (_, ra) = t.file(NodeId::ROOT, "A.txt", 5);
    let (_, rb) = t.file(NodeId::ROOT, "a.txt", 6);
    let (_, rl) = t.file(NodeId::ROOT, "doc.txt", 7);
    let (_, rd) = t.node(
        NodeId::ROOT,
        &units("doc.txt"),
        NodeKind::File,
        Category::Deleted,
        Some(DataState::IntegroNoVerificable),
        8,
        flags::DELETED,
    );
    // El escape iguala «x:y» con un «x%3Ay» real.
    let (_, rx) = t.file(NodeId::ROOT, "x:y", 3);
    let (_, ry) = t.file(NodeId::ROOT, "x%3Ay", 4);
    let vfs = t.finish();
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &content(), all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.files_written, 6);
    assert_eq!(s.totals.renamed, 3);
    let f = tree_files(dir.path());
    assert_eq!(f["A.txt"], bytes_of(ra, 5));
    assert_eq!(f["a (2).txt"], bytes_of(rb, 6));
    assert_eq!(f["doc.txt"], bytes_of(rl, 7));
    assert_eq!(f["doc (borrado).txt"], bytes_of(rd, 8));
    // En el orden estable del árbol «x%3Ay» va antes que «x:y» («%» < «:»): conserva el nombre.
    assert_eq!(f["x%3Ay"], bytes_of(ry, 4));
    assert_eq!(f["x%3Ay (2)"], bytes_of(rx, 3));
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    let c = ev["/a.txt"].get("conflicto");
    assert_eq!(c.get("motivo").s(), "misma_recuperacion");
    assert_eq!(c.get("decision").s(), "renombrado");
    assert_eq!(c.get("nombre_pedido").s(), "a.txt");
    // Un borrado nunca es íntegro: código 3.
    assert_eq!(s.exit_code(), 3);
}

fn simple_tree() -> (mr_api::mr_vfs::Vfs, Vec<(String, u64, u64)>) {
    let mut t = Tree::new();
    let mut v = Vec::new();
    let d = t.dir(NodeId::ROOT, "docs");
    for (p, name, size) in [
        (NodeId::ROOT, "uno.bin", 100u64),
        (d, "dos.txt", 2000),
        (d, "tres.dat", 0),
    ] {
        let (_, r) = t.file(p, name, size);
        let rel = if p == NodeId::ROOT {
            name.to_string()
        } else {
            format!("docs/{name}")
        };
        v.push((rel, r, size));
    }
    (t.finish(), v)
}

#[test]
fn existing_files_follow_each_policy() {
    let (vfs, files) = simple_tree();
    let c = content();
    // Primera pasada.
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &c, all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.files_written, 3);
    assert_eq!(s.exit_code(), 0);
    let first = tree_files(dir.path());
    for (rel, r, size) in &files {
        assert_eq!(first[rel.as_str()], bytes_of(*r, *size), "{rel}");
    }
    // Un fichero ajeno con el mismo nombre y otro contenido.
    std::fs::write(dir.path().join("uno.bin"), b"ajeno").unwrap();

    // Renombrar: el fichero y el directorio existentes no se tocan; se escriben «uno (2).bin» y
    // «docs (2)» con su contenido.
    let s = run(&vfs, &c, all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.files_written, 3);
    assert_eq!(s.totals.renamed, 2);
    let f = tree_files(dir.path());
    assert_eq!(f["uno.bin"], b"ajeno");
    assert_eq!(f["uno (2).bin"], bytes_of(files[0].1, 100));
    assert_eq!(f["docs (2)/dos.txt"], bytes_of(files[1].1, 2000));
    assert_eq!(f["docs/dos.txt"], first["docs/dos.txt"]);

    // Omitir si es idéntico: nada nuevo (los idénticos se reconocen bajo cualquier sufijo).
    let mut o = opts(dir.path());
    o.conflict = ConflictPolicy::SkipIfIdentical;
    let before = tree_files(dir.path());
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.files_written, 0, "{:?}", s.totals);
    assert_eq!(s.totals.skipped_identical, 3);
    assert_eq!(s.exit_code(), 0);
    assert_eq!(tree_files(dir.path()), before);
    let j = summary_journal(&s);
    assert!(j
        .iter()
        .filter(|l| l.get("tipo").s() == "fichero")
        .all(|l| l.get("accion").s() == "omitido_identico"));

    // Omitir: no se escribe nada y queda anotado.
    let mut o = opts(dir.path());
    o.conflict = ConflictPolicy::Skip;
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.files_written, 0);
    // El directorio existente no se usa: su contenido se omite con él.
    assert_eq!(s.totals.skipped_conflict, 3);
    assert_eq!(s.exit_code(), 0);
    assert_eq!(tree_files(dir.path()), before);

    // Sustituir sin confirmación: rechazado antes de escribir nada.
    let mut o = opts(dir.path());
    o.conflict = ConflictPolicy::Overwrite;
    let src = dummy_source();
    let sink = mr_api::ApiSink::new(&src);
    let e = mr_api::mr_recover::recover(
        &vfs,
        &c,
        &sink,
        &Default::default(),
        &all(&vfs),
        &o,
        &mr_api::mr_recover::NoObserver,
    )
    .unwrap_err();
    assert!(matches!(e, RecoverError::InvalidOptions(_)), "{e}");
    // Con confirmación: el ajeno se sustituye de forma atómica.
    o.overwrite_confirmed = true;
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.files_written, 3);
    let f = tree_files(dir.path());
    assert_eq!(f["uno.bin"], bytes_of(files[0].1, 100));
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    assert_eq!(ev["/uno.bin"].get("accion").s(), "escrito_sustituyendo");
    assert_eq!(
        ev["/uno.bin"].get("conflicto").get("decision").s(),
        "sustituido"
    );
    assert_eq!(
        ev["/docs/dos.txt"].get("accion").s(),
        "escrito_sustituyendo"
    );
    assert_no_temps(dir.path());
}

struct Asker {
    asked: AtomicU64,
    answer: ConflictDecision,
    all: bool,
    cancel_after: Option<u64>,
}

impl RecoverObserver for Asker {
    fn conflict(&self, q: &ConflictQuestion) -> ConflictAnswer {
        assert!(q.existing.exists());
        self.asked.fetch_add(1, Ordering::SeqCst);
        ConflictAnswer {
            decision: self.answer,
            apply_to_all: self.all,
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel_after
            .is_some_and(|n| self.asked.load(Ordering::SeqCst) >= n)
    }
}

#[test]
fn ask_policy_uses_the_callback_and_apply_to_all() {
    let (vfs, _) = simple_tree();
    let c = content();
    let dir = tempfile::tempdir().unwrap();
    run(&vfs, &c, all(&vfs), &opts(dir.path()));
    let mut o = opts(dir.path());
    o.conflict = ConflictPolicy::Ask;
    // Sin «aplicar a todos»: una pregunta por conflicto (directorio y tres ficheros).
    let a = Asker {
        asked: AtomicU64::new(0),
        answer: ConflictDecision::SkipIfIdentical,
        all: false,
        cancel_after: None,
    };
    let s = run_obs(&vfs, &c, all(&vfs), &o, &a);
    assert_eq!(a.asked.load(Ordering::SeqCst), 4);
    assert_eq!(s.totals.skipped_identical, 3);
    // Con «aplicar a todos»: una pregunta por tipo (fichero y directorio).
    let a = Asker {
        asked: AtomicU64::new(0),
        answer: ConflictDecision::Rename,
        all: true,
        cancel_after: None,
    };
    let s = run_obs(&vfs, &c, all(&vfs), &o, &a);
    assert_eq!(a.asked.load(Ordering::SeqCst), 2);
    assert_eq!(s.totals.files_written, 3);
    assert!(tree_files(dir.path()).contains_key("docs (2)/dos.txt"));
    // Cancelar desde la pregunta.
    let a = Asker {
        asked: AtomicU64::new(0),
        answer: ConflictDecision::Cancel,
        all: false,
        cancel_after: None,
    };
    let s = run_obs(&vfs, &c, all(&vfs), &o, &a);
    assert_eq!(s.status, RunStatus::Cancelled);
    assert_eq!(s.totals.files_written, 0);
}

#[test]
fn long_paths_beyond_260_and_1024_are_written_and_marked() {
    let mut t = Tree::new();
    let mut parent = NodeId::ROOT;
    let seg = "carpeta_con_un_nombre_bastante_largo_para_superar_los_limites_".repeat(3);
    let mut leaves = Vec::new();
    for i in 0..8 {
        parent = t.dir(parent, &format!("{seg}{i}"));
        let (_, r) = t.file(parent, &format!("fichero_{i}.bin"), 50);
        leaves.push((i, r));
    }
    let vfs = t.finish();
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &content(), all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.files_written, 8);
    assert!(s.totals.long_paths >= 6, "{:?}", s.totals);
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    let mut over_1024 = false;
    for e in ev.values() {
        let rel = e.get("destino").s();
        let full = format!("{}\\{}", s.destination, rel.replace('/', "\\"));
        assert_eq!(
            e.get("ruta_larga").b(),
            full.encode_utf16().count() > 259,
            "{full}"
        );
        over_1024 |= full.len() > 1024;
        // Se lee con la ruta extendida.
        let p = std::path::PathBuf::from(format!(r"\\?\{full}"));
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 50);
    }
    assert!(over_1024);
}

#[test]
fn forbidden_destinations_are_refused_before_writing() {
    let (vfs, _) = simple_tree();
    let c = content();
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("origen.img");
    std::fs::write(&img, vec![0u8; 8192]).unwrap();
    let src = mr_api::open_source(&img, 0).unwrap();
    let sink = mr_api::ApiSink::new(&src);
    let try_dest = |d: &Path| {
        mr_api::mr_recover::recover(
            &vfs,
            &c,
            &sink,
            &Default::default(),
            &all(&vfs),
            &opts(d),
            &mr_api::mr_recover::NoObserver,
        )
    };
    for d in [
        img.clone(),
        img.join("dentro"),
        Path::new(r"\\.\PhysicalDrive0").to_path_buf(),
        Path::new(r"\\?\C:").to_path_buf(),
        Path::new(r"\\?\GLOBALROOT\Device\HarddiskVolume1").to_path_buf(),
        Path::new(r"C:\x\CON\y").to_path_buf(),
    ] {
        let e = try_dest(&d).unwrap_err();
        assert!(matches!(e, RecoverError::Destination(_)), "{d:?}: {e}");
    }
    // Ni temporal ni diario: el directorio solo tiene la imagen, intacta.
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(std::fs::read(&img).unwrap(), vec![0u8; 8192]);
    // Un segmento de una imagen dividida tampoco.
    let seg1 = dir.path().join("disco.001");
    let seg2 = dir.path().join("disco.002");
    std::fs::write(&seg1, vec![0u8; 4096]).unwrap();
    std::fs::write(&seg2, vec![0u8; 4096]).unwrap();
    let split = mr_api::open_split_source(&seg1, mr_api::OpenOptions::default()).unwrap();
    let sink = mr_api::ApiSink::new(&split);
    let e = mr_api::mr_recover::recover(
        &vfs,
        &c,
        &sink,
        &Default::default(),
        &all(&vfs),
        &opts(&seg2),
        &mr_api::mr_recover::NoObserver,
    )
    .unwrap_err();
    assert!(matches!(e, RecoverError::Destination(_)), "{e}");
    // El diario tampoco puede ser el origen.
    let mut o = opts(&dir.path().join("bien"));
    o.journal = Some(seg2.clone());
    let e = mr_api::mr_recover::recover(
        &vfs,
        &c,
        &sink,
        &Default::default(),
        &all(&vfs),
        &o,
        &mr_api::mr_recover::NoObserver,
    )
    .unwrap_err();
    assert!(matches!(e, RecoverError::Journal(_)), "{e}");
    assert_eq!(std::fs::read(&seg2).unwrap(), vec![0u8; 4096]);
}

struct CancelAt {
    files: AtomicU64,
    at: u64,
}

impl RecoverObserver for CancelAt {
    fn cancelled(&self) -> bool {
        self.files.load(Ordering::SeqCst) >= self.at
    }
    fn progress(&self, p: &mr_api::mr_recover::Progress) {
        self.files.store(p.files_done, Ordering::SeqCst);
    }
}

#[test]
fn cancellation_leaves_only_committed_files_and_a_consistent_journal() {
    let mut t = Tree::new();
    let d = t.dir(NodeId::ROOT, "d");
    for i in 0..20 {
        // Ficheros de 3 MiB: la cancelación llega a mitad de uno (entre trozos de 1 MiB).
        t.file(d, &format!("f{i:02}.bin"), 3 << 20);
    }
    let vfs = t.finish();
    let dir = tempfile::tempdir().unwrap();
    let obs = CancelAt {
        files: AtomicU64::new(0),
        at: 5,
    };
    let s = run_obs(&vfs, &content(), all(&vfs), &opts(dir.path()), &obs);
    assert_eq!(s.status, RunStatus::Cancelled);
    assert_eq!(s.exit_code(), 1);
    let f = tree_files(dir.path());
    let written: Vec<_> = f.keys().filter(|k| k.ends_with(".bin")).collect();
    assert_eq!(written.len() as u64, s.totals.files_written);
    for (k, v) in &f {
        if k.ends_with(".bin") {
            assert_eq!(v.len(), 3 << 20, "fichero parcial con nombre final: {k}");
        }
    }
    assert_no_temps(dir.path());
    let j = summary_journal(&s);
    assert_eq!(j[0].get("tipo").s(), "cabecera");
    let last = j.last().unwrap();
    assert_eq!(last.get("tipo").s(), "fin");
    assert_eq!(last.get("estado").s(), "cancelada");
    assert_eq!(last.get("lineas").n(), j.len() as u64);
    let escritos = j
        .iter()
        .filter(|l| l.get("tipo").s() == "fichero" && l.get("accion").s() == "escrito")
        .count();
    assert_eq!(escritos as u64, s.totals.files_written);
}

#[test]
fn rerun_with_skip_identical_and_determinism() {
    let (vfs, _) = simple_tree();
    let c = content();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let sa = run(&vfs, &c, all(&vfs), &opts(a.path()));
    let sb = run(&vfs, &c, all(&vfs), &opts(b.path()));
    assert_eq!(tree_files(a.path()), tree_files(b.path()));
    let (ja, jb) = (summary_journal(&sa), summary_journal(&sb));
    assert_eq!(ja.len(), jb.len());
    // Las líneas de ficheros y directorios son idénticas salvo nada; cabecera y fin difieren solo
    // en fechas, identificador y raíz.
    for (x, y) in ja.iter().zip(&jb).skip(1).take(ja.len() - 2) {
        assert_eq!(x, y);
    }
    for k in ["opciones", "seleccion", "filtro", "plan", "origen"] {
        assert_eq!(ja[0].get(k), jb[0].get(k), "{k}");
    }
    assert_eq!(
        ja.last().unwrap().get("totales"),
        jb.last().unwrap().get("totales")
    );
    // Repetir con «omitir si es idéntico» no escribe nada nuevo.
    let mut o = opts(a.path());
    o.conflict = ConflictPolicy::SkipIfIdentical;
    let before = tree_files(a.path());
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.files_written, 0);
    assert_eq!(s.totals.dirs_created, 0);
    assert_eq!(s.totals.skipped_identical, 3);
    assert_eq!(tree_files(a.path()), before);
}

#[test]
fn untrusted_ranges_fill_marks_and_hash_scope() {
    let mut t = Tree::new();
    let (_, r1) = t.file(NodeId::ROOT, "ilegible.bin", 8192);
    let (_, r2) = t.node(
        NodeId::ROOT,
        &units("reasignado.bin"),
        NodeKind::File,
        Category::Deleted,
        Some(DataState::Parcial),
        4096,
        flags::DELETED,
    );
    let (_, r3) = t.node(
        NodeId::ROOT,
        &units("sobrescrito.bin"),
        NodeKind::File,
        Category::Deleted,
        Some(DataState::Sobrescrito),
        4096,
        flags::DELETED,
    );
    let (_, r4) = t.node(
        NodeId::ROOT,
        &units("cifrado.bin"),
        NodeKind::File,
        Category::Live,
        Some(DataState::Cifrado),
        10,
        0,
    );
    let (_, r5) = t.node(
        NodeId::ROOT,
        &units("roto.bin"),
        NodeKind::File,
        Category::Live,
        Some(DataState::NoDisponible),
        10,
        flags::RECORD_DAMAGED,
    );
    let vfs = t.finish();
    let mut c = content();
    let data1: Vec<u8> = (0..8192u32).map(|i| (i % 7) as u8).collect();
    c.put(
        r1,
        "",
        MemStream {
            data: data1.clone(),
            ranges: vec![(1024, 1024, RangeState::Unreadable, Some(5002))],
        },
    );
    let data2 = vec![0x55u8; 4096];
    c.put(
        r2,
        "",
        MemStream {
            data: data2.clone(),
            ranges: vec![(0, 2048, RangeState::PossiblyOverwritten, None)],
        },
    );
    c.put(
        r3,
        "",
        MemStream {
            data: vec![0x77u8; 4096],
            ranges: vec![(0, 4096, RangeState::PossiblyOverwritten, None)],
        },
    );
    c.fail(
        r4,
        SourceError::Unsupported {
            code: "encrypted",
            message: "cifrado".into(),
        },
    );
    let _ = r5;
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &c, all(&vfs), &opts(dir.path()));
    assert_eq!(s.exit_code(), 3);
    assert_eq!(s.totals.files_written, 2);
    // Sobrescrito, cifrado y no disponible se omiten por defecto.
    assert_eq!(s.totals.skipped_state, 3);
    let f = tree_files(dir.path());
    // Relleno exactamente en los bytes ilegibles.
    let mut want = data1.clone();
    for i in 1024..2048usize {
        want[i] = DEFAULT_FILL[i % 4];
    }
    assert_eq!(f["ilegible.bin"], want);
    // Los bytes posiblemente sobrescritos se escriben como relleno por defecto.
    let mut want2 = data2.clone();
    for i in 0..2048usize {
        want2[i] = DEFAULT_FILL[i % 4];
    }
    assert_eq!(f["reasignado.bin"], want2);
    let mark = String::from_utf8(f["ilegible.bin.mrbad"].clone()).unwrap();
    assert!(mark.contains("bytes 1024..2048"), "{mark}");
    assert!(mark.contains("LBA del volumen 5002"), "{mark}");
    assert!(f.contains_key("reasignado.bin.mrbad"));
    assert!(!f.contains_key("sobrescrito.bin"));
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    let e = &ev["/ilegible.bin"];
    assert_eq!(e.get("estado_inicial").s(), "integro");
    assert_eq!(e.get("estado").s(), "parcial");
    assert_eq!(e.get("marca_incompleto").s(), "ilegible.bin.mrbad");
    let st = &e.get("flujos").arr()[0];
    assert_eq!(st.get("hashes").get("ambito").s(), "escrito_con_relleno");
    assert_eq!(st.get("hashes").get("sha256").s(), sha256(&want));
    assert_eq!(st.get("bytes_no_fiables").n(), 1024);
    assert_eq!(st.get("bytes_relleno").n(), 1024);
    let r = &st.get("tramos_no_fiables").arr()[0];
    assert_eq!(r.get("desplazamiento").n(), 1024);
    assert_eq!(r.get("longitud").n(), 1024);
    assert_eq!(r.get("estado").s(), "unreadable");
    assert_eq!(r.get("lba").n(), 5002);
    assert_eq!(r.get("sectores").n(), 2);
    assert!(r.get("relleno").b());
    assert_eq!(ev["/sobrescrito.bin"].get("accion").s(), "omitido_estado");
    assert_eq!(
        ev["/sobrescrito.bin"].get("flujos").arr().len(),
        0,
        "omitido: sin hashes"
    );
    assert_eq!(ev["/cifrado.bin"].get("accion").s(), "omitido_estado");
    assert_eq!(ev["/roto.bin"].get("accion").s(), "omitido_estado");

    // Con las opciones: sobrescrito incluido (relleno), bytes reasignados del disco y cifrado
    // como no soportado.
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(dir.path());
    o.write_states |= DataState::Sobrescrito.bit() | DataState::Cifrado.bit();
    o.include_reallocated_bytes = true;
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.files_written, 3);
    assert_eq!(s.totals.unsupported, 1);
    let f = tree_files(dir.path());
    assert_eq!(f["reasignado.bin"], data2);
    assert_eq!(f["sobrescrito.bin"], vec![0x77u8; 4096]);
    let j = summary_journal(&s);
    let ev = files_by_path(&j);
    let st = &ev["/sobrescrito.bin"].get("flujos").arr()[0];
    assert_eq!(ev["/sobrescrito.bin"].get("estado").s(), "sobrescrito");
    assert_eq!(st.get("hashes").get("ambito").s(), "escrito_con_relleno");
    assert_eq!(st.get("bytes_relleno").n(), 0);
    assert!(!st.get("tramos_no_fiables").arr()[0].get("relleno").b());
    assert_eq!(ev["/cifrado.bin"].get("accion").s(), "no_soportado");
    assert_eq!(
        ev["/cifrado.bin"].get("error").get("codigo").s(),
        "encrypted"
    );
}

#[test]
fn deleted_intact_files_keep_content_scope_but_never_intact() {
    let mut t = Tree::new();
    let (_, r) = t.node(
        NodeId::ROOT,
        &units("borrado.txt"),
        NodeKind::File,
        Category::Deleted,
        Some(DataState::IntegroNoVerificable),
        300,
        flags::DELETED,
    );
    let vfs = t.finish();
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &content(), all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.written(DataState::IntegroNoVerificable), 1);
    assert_eq!(s.exit_code(), 3);
    let j = summary_journal(&s);
    let e = &files_by_path(&j)["/borrado.txt"];
    assert_eq!(e.get("estado").s(), "integro_no_verificable");
    let h = e.get("flujos").arr()[0].get("hashes");
    assert_eq!(h.get("ambito").s(), "contenido");
    assert_eq!(h.get("sha256").s(), sha256(&bytes_of(r, 300)));
    assert!(e.get("marca_incompleto").is_null());
}

#[test]
fn split_parts_and_fat32_simulation() {
    let mut t = Tree::new();
    let (_, r) = t.file(NodeId::ROOT, "grande.bin", 10_000);
    t.file(NodeId::ROOT, "pequeno.bin", 100);
    let vfs = t.finish();
    let c = content();
    let whole = bytes_of(r, 10_000);
    for (split, caps, part) in [
        (SplitPolicy::Size(4096), None, 4096usize),
        (
            SplitPolicy::Auto,
            Some(Capabilities {
                named_streams: false,
                case_sensitive: false,
                max_file_size: Some(5000),
                source: "opciones",
            }),
            5000,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.split = split;
        o.capabilities = caps;
        let s = run(&vfs, &c, all(&vfs), &o);
        assert_eq!(s.totals.files_written, 2);
        let f = tree_files(dir.path());
        assert!(!f.contains_key("grande.bin"));
        let mut joined = Vec::new();
        let n = 10_000usize.div_ceil(part);
        for i in 1..=n {
            let p = &f[format!("grande.bin.{i:03}").as_str()];
            assert!(p.len() <= part);
            joined.extend_from_slice(p);
        }
        assert!(!f.contains_key(format!("grande.bin.{:03}", n + 1).as_str()));
        assert_eq!(joined, whole);
        assert_eq!(f["pequeno.bin"].len(), 100);
        let j = summary_journal(&s);
        let ev = files_by_path(&j);
        let st = &ev["/grande.bin"].get("flujos").arr()[0];
        assert_eq!(st.get("forma").s(), "partes");
        assert_eq!(st.get("hashes").get("sha256").s(), sha256(&whole));
        let parts = st.get("partes").arr();
        assert_eq!(parts.len(), n);
        for (i, p) in parts.iter().enumerate() {
            let name = format!("grande.bin.{:03}", i + 1);
            assert_eq!(p.get("destino").s(), name);
            assert_eq!(p.get("hashes").get("sha256").s(), sha256(&f[name.as_str()]));
        }
        // Repetir con «omitir si es idéntico» reconoce el conjunto de partes.
        o.conflict = ConflictPolicy::SkipIfIdentical;
        let s = run(&vfs, &c, all(&vfs), &o);
        assert_eq!(s.totals.skipped_identical, 2, "{:?}", s.totals);
    }
    // Sin división y con el límite: error de destino para ese fichero.
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(dir.path());
    o.split = SplitPolicy::Never;
    o.capabilities = Some(Capabilities {
        named_streams: false,
        case_sensitive: false,
        max_file_size: Some(5000),
        source: "opciones",
    });
    let s = run(&vfs, &c, all(&vfs), &o);
    assert_eq!(s.totals.errors, 1);
    assert_eq!(s.exit_code(), 1);
}

#[test]
fn alternate_streams_as_streams_or_siblings() {
    let mut t = Tree::new();
    let (_, r) = t.file(NodeId::ROOT, "con_flujos.txt", 0);
    let vfs = t.finish();
    let mut c = content();
    c.put(
        r,
        "",
        MemStream {
            data: b"principal".to_vec(),
            ranges: vec![],
        },
    );
    c.put(
        r,
        "Zone.Identifier",
        MemStream {
            data: b"[ZoneTransfer]".to_vec(),
            ranges: vec![],
        },
    );
    c.put(
        r,
        "a:b",
        MemStream {
            data: b"raro".to_vec(),
            ranges: vec![],
        },
    );
    // Destino sin flujos (simulado): ficheros hermanos escapados.
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(dir.path());
    o.capabilities = Some(Capabilities {
        named_streams: false,
        case_sensitive: false,
        max_file_size: None,
        source: "opciones",
    });
    let s = run(&vfs, &c, all(&vfs), &o);
    let f = tree_files(dir.path());
    assert_eq!(f["con_flujos.txt"], b"principal");
    assert_eq!(f["con_flujos.txt%3AZone.Identifier"], b"[ZoneTransfer]");
    assert_eq!(f["con_flujos.txt%3Aa%3Ab"], b"raro");
    let j = summary_journal(&s);
    let e = &files_by_path(&j)["/con_flujos.txt"];
    let fl = e.get("flujos").arr();
    assert_eq!(fl.len(), 3);
    assert_eq!(fl[1].get("forma").s(), "fichero_hermano");
    assert_eq!(
        fl[1].get("hashes").get("sha256").s(),
        sha256(b"[ZoneTransfer]")
    );
    // Destino real (sonda): si admite flujos, como flujos del fichero.
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &c, all(&vfs), &opts(dir.path()));
    if s.capabilities.named_streams {
        let p = dir.path().join("con_flujos.txt");
        let ads = format!("{}:Zone.Identifier", p.display());
        assert_eq!(std::fs::read(ads).unwrap(), b"[ZoneTransfer]");
        let ads = format!("{}:a%3Ab", p.display());
        assert_eq!(std::fs::read(ads).unwrap(), b"raro");
        let f = tree_files(dir.path());
        assert_eq!(f.len(), 1, "{:?}", f.keys());
        let j = summary_journal(&s);
        let fl = files_by_path(&j)["/con_flujos.txt"]
            .get("flujos")
            .arr()
            .to_vec();
        assert_eq!(fl[1].get("forma").s(), "flujo");
        assert_eq!(fl[1].get("destino").s(), "con_flujos.txt:Zone.Identifier");
    }
}

#[test]
fn filters_selection_and_empty_directories() {
    let mut t = Tree::new();
    let d = t.dir(NodeId::ROOT, "d");
    let e = t.dir(NodeId::ROOT, "vacio");
    let _ = e;
    t.file(d, "a.txt", 10);
    t.file(d, "b.tmp", 10);
    let (sel_file, _) = t.file(NodeId::ROOT, "suelto.bin", 10);
    let vfs = t.finish();
    let c = content();
    let dir = tempfile::tempdir().unwrap();
    let mut sel = all(&vfs);
    sel.masks = "!*.tmp".into();
    let s = run(&vfs, &c, sel, &opts(dir.path()));
    let f = tree_files(dir.path());
    assert!(f.contains_key("d/a.txt") && !f.contains_key("d/b.tmp"));
    assert!(
        !f.contains_key("vacio/"),
        "los directorios vacíos no se crean por defecto"
    );
    assert_eq!(s.totals.files_written, 2);
    // Selección de un directorio y de un fichero: el directorio se crea con su nombre.
    let dir = tempfile::tempdir().unwrap();
    let d_id = vfs.find("/d").unwrap();
    let sel = Selection {
        nodes: vec![sel_file, d_id, vfs.find("/d/a.txt").unwrap()],
        ..Selection::default()
    };
    let mut o = opts(dir.path());
    o.create_empty_dirs = true;
    let s = run(&vfs, &c, sel, &o);
    assert_eq!(
        s.totals.files_written, 3,
        "el fichero dentro de una selección no se repite"
    );
    let f = tree_files(dir.path());
    assert!(f.contains_key("d/a.txt") && f.contains_key("d/b.tmp") && f.contains_key("suelto.bin"));
    // Con directorios vacíos.
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(dir.path());
    o.create_empty_dirs = true;
    run(&vfs, &c, all(&vfs), &o);
    assert!(tree_files(dir.path()).contains_key("vacio/"));
}

#[test]
fn times_and_readonly_are_applied_after_data() {
    let mut t = Tree::new();
    let d = t.dir(NodeId::ROOT, "d");
    t.attrs = 0x21;
    let (f_id, _) = t.node(
        d,
        &units("solo_lectura.txt"),
        NodeKind::File,
        Category::Live,
        Some(DataState::Integro),
        10,
        0,
    );
    let vfs = t.finish();
    let _ = f_id;
    let dir = tempfile::tempdir().unwrap();
    let s = run(&vfs, &content(), all(&vfs), &opts(dir.path()));
    assert_eq!(s.totals.files_written, 1);
    let n = vfs.node(vfs.find("/d/solo_lectura.txt").unwrap()).unwrap();
    let md = std::fs::metadata(dir.path().join("d/solo_lectura.txt")).unwrap();
    use mr_api::mr_recover::dest::system_to_filetime;
    assert_eq!(system_to_filetime(md.modified().unwrap()), n.times.modified);
    assert_eq!(system_to_filetime(md.accessed().unwrap()), n.times.accessed);
    assert_eq!(system_to_filetime(md.created().unwrap()), n.times.created);
    assert!(md.permissions().readonly());
    let dn = vfs.node(vfs.find("/d").unwrap()).unwrap();
    let mdd = std::fs::metadata(dir.path().join("d")).unwrap();
    assert_eq!(
        system_to_filetime(mdd.modified().unwrap()),
        dn.times.modified
    );
}
