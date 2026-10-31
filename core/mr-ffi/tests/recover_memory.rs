//! Memoria máxima de la recuperación (#26, INV-01 §7.1 invariante 6 y §9 «Escala»): constante
//! respecto al tamaño de los ficheros (trozos de 1 MiB) y lineal y pequeña respecto al número de
//! nodos de la selección (plan de 4 B por nodo y nombres de un mismo directorio).
//!
//! La medida usa un asignador global que cuenta los bytes vivos y su máximo; está en `mr-ffi`
//! porque es el único crate donde el `unsafe` está permitido (`GlobalAlloc` es un rasgo `unsafe`),
//! como `table_memory.rs`. Las pruebas se ejecutan una tras otra para que la medida de una no
//! incluya a otra. Los árboles son sintéticos (`mr_vfs::arena` y `mr_recover::mem`) y el destino
//! es un directorio temporal.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mr_api::mr_recover::mem::MemContent;
use mr_api::mr_recover::{NoObserver, RecoverOptions, Selection, SourceInfo, DEFAULT_FILL};
use mr_api::mr_vfs::arena::{Limits, NewNode, VfsBuilder};
use mr_api::mr_vfs::{Category, DataState, FsRef, NameReliability, NodeId, NodeKind, Times, Vfs};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let now = LIVE.fetch_add(n, Ordering::SeqCst) + n;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

fn shrink(n: usize) {
    LIVE.fetch_sub(n, Ordering::SeqCst);
}

// SAFETY: solo se añade contabilidad; cada operación delega en `System` con los mismos
// argumentos, así que se cumple el contrato de `GlobalAlloc` que cumple `System`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: mismo contrato que el llamador garantiza para `alloc`.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: mismo contrato que el llamador garantiza para `alloc_zeroed`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` y `layout` vienen de una asignación de este mismo asignador.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        grow(new_size);
        // SAFETY: mismo contrato que el llamador garantiza para `realloc`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            shrink(new_size);
        } else {
            shrink(layout.size());
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Una prueba a la vez.
static SERIAL: Mutex<()> = Mutex::new(());

fn node<'a>(name: &'a [u16], kind: NodeKind, size: u64, record: u64) -> NewNode<'a> {
    NewNode {
        name,
        kind,
        category: Category::Live,
        name_reliability: NameReliability::Structure,
        data: (kind == NodeKind::File).then_some(DataState::Integro),
        flags: 0,
        namespace: Some(1),
        size,
        allocated_size: size,
        times: Times::default(),
        attributes: 0x20,
        origin: Some(FsRef {
            record,
            sequence: 1,
            parent_record: 5,
            parent_sequence: 5,
        }),
    }
}

fn tree(dirs: u32, per_dir: u32, size: u64) -> Vfs {
    let root: Vec<u16> = Vec::new();
    let mut b = VfsBuilder::new(Limits::default(), node(&root, NodeKind::Directory, 0, 5)).unwrap();
    let mut rec = 100u64;
    for d in 0..dirs {
        let dn: Vec<u16> = format!("carpeta_{d:05}").encode_utf16().collect();
        let did = b
            .push(NodeId::ROOT, node(&dn, NodeKind::Directory, 0, rec))
            .unwrap();
        rec += 1;
        for f in 0..per_dir {
            let fname: Vec<u16> = format!("fichero_{f:05}.bin").encode_utf16().collect();
            b.push(did, node(&fname, NodeKind::File, size, rec))
                .unwrap();
            rec += 1;
        }
    }
    b.finish()
}

/// Pico de memoria de una recuperación por encima de la memoria viva al empezar.
fn peak_of(vfs: &Vfs) -> (usize, mr_api::mr_recover::RecoverSummary) {
    let dir = tempfile::tempdir().unwrap();
    let content = MemContent::new(DEFAULT_FILL);
    let mem = mr_io::MemDevice::new(vec![0u8; 4096], 512).unwrap();
    let src = mr_api::source_from_device(Arc::new(mem), "pruebas de memoria");
    let sink = mr_api::ApiSink::new(&src);
    let sel = Selection {
        nodes: vec![NodeId::ROOT],
        ..Selection::default()
    };
    let mut opts = RecoverOptions::new(dir.path());
    opts.times = false;
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let s = mr_api::mr_recover::recover(
        vfs,
        &content,
        &sink,
        &SourceInfo::default(),
        &sel,
        &opts,
        &NoObserver,
    )
    .unwrap();
    let peak = PEAK.load(Ordering::SeqCst) - base;
    (peak, s)
}

/// Un fichero de 96 MiB cuesta lo mismo que uno de 8 MiB: la copia va por trozos.
#[test]
fn memory_is_independent_of_file_size() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let small = tree(1, 1, 8 << 20);
    let big = tree(1, 1, 96 << 20);
    let (ps, s1) = peak_of(&small);
    let (pb, s2) = peak_of(&big);
    assert_eq!(s1.totals.files_written, 1);
    assert_eq!(s2.totals.bytes_written, 96 << 20);
    eprintln!(
        "pico: fichero de 8 MiB {} KiB, de 96 MiB {} KiB",
        ps >> 10,
        pb >> 10
    );
    assert!(pb < 16 << 20, "pico {pb}");
    assert!(
        pb < ps + (2 << 20),
        "el pico crece con el tamaño: {ps} → {pb}"
    );
}

/// Árbol sintético de 100 000 ficheros pequeños: la memoria de la recuperación (sin la del árbol,
/// que ya existe) queda en unos pocos MiB.
#[test]
#[ignore = "escala: escribe 100 000 ficheros (minutos en Windows con antivirus)"]
fn memory_is_bounded_on_a_large_tree() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let vfs = tree(1000, 100, 64);
    assert_eq!(vfs.stats().files, 100_000);
    let t0 = std::time::Instant::now();
    let (peak, s) = peak_of(&vfs);
    eprintln!(
        "100 000 ficheros: pico {} KiB, {:?} (árbol: {} KiB)",
        peak >> 10,
        t0.elapsed(),
        vfs.memory_bytes() >> 10
    );
    assert_eq!(s.totals.files_written, 100_000);
    assert_eq!(s.totals.dirs_created, 1000);
    // Plan (4 B por nodo), visibilidad (1 B por nodo), nombres de un directorio y búferes.
    assert!(peak < 24 << 20, "pico {peak}");
}
