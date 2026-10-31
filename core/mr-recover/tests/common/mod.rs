//! Utilidades de las pruebas de la recuperación: árboles virtuales sintéticos, origen ficticio
//! para `mr_api::ApiSink`, lectura del diario (analizador JSON mínimo e independiente del
//! escritor) y recorrido del destino.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mr_api::mr_recover::{
    self, ContentSource, NoObserver, RecoverObserver, RecoverOptions, RecoverSummary, Selection,
    SourceInfo,
};
use mr_api::mr_vfs::arena::{Limits, NewNode, VfsBuilder};
use mr_api::mr_vfs::{Category, DataState, FsRef, NameReliability, NodeId, NodeKind, Times, Vfs};

// ---------------------------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, k: &str) -> &Json {
        static NULL: Json = Json::Null;
        match self {
            Json::Obj(v) => v.iter().find(|(x, _)| x == k).map_or(&NULL, |(_, j)| j),
            _ => &NULL,
        }
    }
    pub fn s(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => "",
        }
    }
    pub fn n(&self) -> u64 {
        match self {
            Json::Num(n) => *n as u64,
            _ => u64::MAX,
        }
    }
    pub fn b(&self) -> bool {
        matches!(self, Json::Bool(true))
    }
    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Json::Obj(v) => v.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && b" \t\r\n".contains(&self.s[self.i]) {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return Json::Obj(v);
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.value() else {
                        panic!("clave no textual")
                    };
                    self.ws();
                    assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    let x = self.value();
                    v.push((k, x));
                    self.ws();
                    match self.s[self.i] {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Json::Obj(v);
                        }
                        c => panic!("JSON no válido: {}", c as char),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return Json::Arr(v);
                }
                loop {
                    v.push(self.value());
                    self.ws();
                    match self.s[self.i] {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Json::Arr(v);
                        }
                        c => panic!("JSON no válido: {}", c as char),
                    }
                }
            }
            b'"' => {
                self.i += 1;
                let mut units: Vec<u16> = Vec::new();
                let mut out = String::new();
                loop {
                    let c = self.s[self.i];
                    match c {
                        b'"' => {
                            self.i += 1;
                            return Json::Str(out);
                        }
                        b'\\' => {
                            let e = self.s[self.i + 1];
                            self.i += 2;
                            match e {
                                b'n' => out.push('\n'),
                                b'r' => out.push('\r'),
                                b't' => out.push('\t'),
                                b'"' => out.push('"'),
                                b'\\' => out.push('\\'),
                                b'/' => out.push('/'),
                                b'u' => {
                                    let h =
                                        std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap();
                                    units.push(u16::from_str_radix(h, 16).unwrap());
                                    self.i += 4;
                                    let s = String::from_utf16(&units);
                                    if let Ok(s) = s {
                                        out.push_str(&s);
                                        units.clear();
                                    }
                                }
                                _ => panic!("escape no válido"),
                            }
                        }
                        _ => {
                            // Un carácter UTF-8 completo.
                            let len = match c {
                                0..=0x7F => 1,
                                0xC0..=0xDF => 2,
                                0xE0..=0xEF => 3,
                                _ => 4,
                            };
                            out.push_str(
                                std::str::from_utf8(&self.s[self.i..self.i + len]).unwrap(),
                            );
                            self.i += len;
                        }
                    }
                }
            }
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let st = self.i;
                while self.i < self.s.len() && b"-+.eE0123456789".contains(&self.s[self.i]) {
                    self.i += 1;
                }
                Json::Num(
                    std::str::from_utf8(&self.s[st..self.i])
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            }
        }
    }
}

pub fn parse(s: &str) -> Json {
    let mut p = P {
        s: s.as_bytes(),
        i: 0,
    };
    let v = p.value();
    p.ws();
    assert_eq!(p.i, s.len(), "basura tras el JSON: {s}");
    v
}

/// Líneas del diario.
pub fn journal(path: &Path) -> Vec<Json> {
    let t = std::fs::read_to_string(path).expect("diario legible");
    assert!(t.ends_with('\n'), "el diario termina en salto de línea");
    t.lines().map(parse).collect()
}

/// Acontecimientos de fichero del diario por ruta virtual.
pub fn files_by_path(j: &[Json]) -> BTreeMap<String, Json> {
    j.iter()
        .filter(|l| l.get("tipo").s() == "fichero")
        .map(|l| (l.get("ruta_virtual").s().to_string(), l.clone()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Árboles sintéticos
// ---------------------------------------------------------------------------------------------

pub fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub const T0: u64 = 134_116_992_000_000_000; // 2026-01-01T00:00:00Z

pub struct Tree {
    pub b: VfsBuilder,
    next: u64,
    /// Atributos DOS de los nodos que se añaden.
    pub attrs: u32,
}

impl Tree {
    pub fn new() -> Self {
        let root = NewNode {
            name: &[],
            kind: NodeKind::Directory,
            category: Category::Live,
            name_reliability: NameReliability::Structure,
            data: None,
            flags: 0,
            namespace: None,
            size: 0,
            allocated_size: 0,
            times: Times::default(),
            attributes: 0,
            origin: Some(FsRef {
                record: 5,
                sequence: 5,
                parent_record: 5,
                parent_sequence: 5,
            }),
        };
        Tree {
            b: VfsBuilder::new(Limits::default(), root).unwrap(),
            next: 100,
            attrs: 0x20,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn node(
        &mut self,
        parent: NodeId,
        name: &[u16],
        kind: NodeKind,
        cat: Category,
        data: Option<DataState>,
        size: u64,
        flags: u32,
    ) -> (NodeId, u64) {
        let rec = self.next;
        self.next += 1;
        let t = Times {
            created: T0 + rec,
            modified: T0 + 10_000_000 * rec,
            changed: T0,
            accessed: T0 + 20_000_000 * rec,
        };
        let id = self
            .b
            .push(
                parent,
                NewNode {
                    name,
                    kind,
                    category: cat,
                    name_reliability: NameReliability::Structure,
                    data,
                    flags,
                    namespace: Some(1),
                    size,
                    allocated_size: size,
                    times: t,
                    attributes: self.attrs,
                    origin: Some(FsRef {
                        record: rec,
                        sequence: 1,
                        parent_record: 5,
                        parent_sequence: 5,
                    }),
                },
            )
            .unwrap();
        (id, rec)
    }

    pub fn file(&mut self, parent: NodeId, name: &str, size: u64) -> (NodeId, u64) {
        self.node(
            parent,
            &units(name),
            NodeKind::File,
            Category::Live,
            Some(DataState::Integro),
            size,
            0,
        )
    }

    pub fn dir(&mut self, parent: NodeId, name: &str) -> NodeId {
        self.node(
            parent,
            &units(name),
            NodeKind::Directory,
            Category::Live,
            None,
            0,
            0,
        )
        .0
    }

    pub fn finish(self) -> Vfs {
        self.b.finish()
    }
}

/// Origen ficticio (en memoria, sin ruta) para `mr_api::ApiSink`.
pub fn dummy_source() -> mr_api::Source {
    let mem = mr_io::MemDevice::new(vec![0u8; 4096], 512).unwrap();
    mr_api::source_from_device(Arc::new(mem), "origen de pruebas")
}

pub fn run(
    vfs: &Vfs,
    content: &dyn ContentSource,
    sel: Selection,
    opts: &RecoverOptions,
) -> RecoverSummary {
    run_obs(vfs, content, sel, opts, &NoObserver)
}

pub fn run_obs(
    vfs: &Vfs,
    content: &dyn ContentSource,
    sel: Selection,
    opts: &RecoverOptions,
    obs: &dyn RecoverObserver,
) -> RecoverSummary {
    let src = dummy_source();
    let sink = mr_api::ApiSink::new(&src);
    mr_recover::recover(vfs, content, &sink, &SourceInfo::default(), &sel, opts, obs)
        .expect("recuperación")
}

pub fn all(vfs: &Vfs) -> Selection {
    let _ = vfs;
    Selection {
        nodes: vec![NodeId::ROOT],
        ..Selection::default()
    }
}

/// Ficheros del destino (rutas relativas con `/`), sin el diario.
pub fn tree_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        v.sort();
        for p in v {
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with(mr_recover::JOURNAL_FOLDER) {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// Ningún temporal `.mrtmp` ni sonda `.mrprobe` en el árbol.
pub fn assert_no_temps(root: &Path) {
    for (k, _) in tree_files(root) {
        assert!(!k.contains(".mrtmp"), "temporal sin borrar: {k}");
        assert!(!k.contains(".mrprobe"), "sonda sin borrar: {k}");
    }
}

pub fn sha256(data: &[u8]) -> String {
    let mut h = mr_common::MultiHasher::new(&[mr_common::HashAlgorithm::Sha256]);
    h.update(data);
    h.finish().pop().unwrap().1
}

pub fn opts(dest: &Path) -> RecoverOptions {
    RecoverOptions::new(dest)
}
