//! Arena de nodos del árbol virtual.
//!
//! **Disposición.** Un nodo ocupa [`NODE_BYTES`] bytes fijos (comprobado en las pruebas: el
//! objetivo de INV-01 es ≤ 128 B) más su nombre en un búfer UTF-16 compartido. Los hijos se
//! guardan al final en una lista compacta (un `u32` por nodo). Sin punteros: los identificadores
//! son posiciones de la arena, estables mientras viva el árbol.
//!
//! **Invariantes.** La raíz es el nodo 0; el padre de un nodo se crea antes que él, así que su
//! identificador es menor y el árbol no puede tener ciclos; la profundidad está acotada por
//! [`Limits::max_depth`]; solo los contenedores (directorios y carpetas sintéticas) tienen hijos.
//!
//! **Orden de los hijos** (determinista, independiente del orden de construcción): primero los
//! nodos reales y después las carpetas sintéticas; dentro de cada grupo, por nombre sin distinguir
//! mayúsculas, a igualdad por las unidades originales, la categoría, el registro y, por último, el
//! identificador. Dos hermanos con el mismo nombre sin distinguir mayúsculas se marcan con
//! [`flags::DUPLICATE_NAME`].
//!
//! **Límites** ([`Limits`]): nodos, unidades de nombre en total y profundidad. Superarlos es un
//! error tipado ([`VfsError::LimitExceeded`]), nunca un árbol truncado en silencio.

use std::cmp::Ordering;

use crate::name::{self, escape_component};
use crate::{
    flags, Category, DataState, FsRef, NameReliability, NodeId, NodeKind, Result, Times, VfsError,
};

/// Bytes fijos por nodo (sin el nombre ni su entrada en la lista de hijos).
pub const NODE_BYTES: usize = std::mem::size_of::<Node>();
const _: () = assert!(NODE_BYTES <= 128, "un nodo no debe superar 128 B");

/// Máximo de notas guardadas (las siguientes solo se cuentan).
pub const MAX_NOTES: usize = 10_000;

/// Límites de la arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Nodos máximos (incluidas la raíz y las carpetas sintéticas). Por defecto 16 777 216
    /// (unos 2 GiB con nombres medios de 20 unidades).
    pub max_nodes: u32,
    /// Unidades UTF-16 de nombre en total. Por defecto 512 Mi (1 GiB).
    pub max_name_units: u64,
    /// Profundidad máxima (la raíz es 0). Por defecto 4096.
    pub max_depth: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_nodes: 1 << 24,
            max_name_units: 1 << 29,
            max_depth: 4096,
        }
    }
}

/// Nodo compacto (interno).
#[derive(Debug, Clone, Copy)]
struct Node {
    parent: u32,
    name_start: u32,
    children_start: u32,
    children_len: u32,
    flags: u32,
    attributes: u32,
    name_len: u16,
    depth: u16,
    sequence: u16,
    parent_sequence: u16,
    kind: u8,
    category: u8,
    name_rel: u8,
    data: u8,
    /// Espacio de nombres (0 POSIX, 1 Win32, 2 DOS, 3 Win32 y DOS) o 255 si no aplica.
    namespace: u8,
    /// 1 si `record`/`sequence` son una referencia al origen.
    has_ref: u8,
    size: u64,
    allocated: u64,
    record: u64,
    parent_record: u64,
    times: [u64; 4],
}

/// Datos de un nodo nuevo.
#[derive(Debug, Clone)]
pub struct NewNode<'a> {
    /// Nombre en unidades UTF-16 originales.
    pub name: &'a [u16],
    pub kind: NodeKind,
    pub category: Category,
    pub name_reliability: NameReliability,
    /// `None` en directorios y carpetas sintéticas.
    pub data: Option<DataState>,
    pub flags: u32,
    pub namespace: Option<u8>,
    pub size: u64,
    pub allocated_size: u64,
    pub times: Times,
    pub attributes: u32,
    pub origin: Option<FsRef>,
}

impl<'a> NewNode<'a> {
    /// Carpeta sintética con ese nombre y categoría.
    pub fn synthetic(name: &'a [u16], category: Category) -> Self {
        NewNode {
            name,
            kind: NodeKind::Synthetic,
            category,
            name_reliability: NameReliability::Synthetic,
            data: None,
            flags: 0,
            namespace: None,
            size: 0,
            allocated_size: 0,
            times: Times::default(),
            attributes: 0,
            origin: None,
        }
    }
}

/// Vista de un nodo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub id: NodeId,
    /// `None` en la raíz.
    pub parent: Option<NodeId>,
    pub depth: u16,
    pub kind: NodeKind,
    pub category: Category,
    pub name_reliability: NameReliability,
    /// `None` en directorios y carpetas sintéticas.
    pub data: Option<DataState>,
    pub flags: u32,
    pub namespace: Option<u8>,
    pub size: u64,
    pub allocated_size: u64,
    pub times: Times,
    pub attributes: u32,
    /// Referencia al objeto del sistema de archivos (`None` en carpetas sintéticas).
    pub origin: Option<FsRef>,
    pub child_count: u32,
}

impl NodeInfo {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub fn is_container(&self) -> bool {
        self.kind.is_container()
    }
}

/// Recuentos del árbol.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub nodes: u64,
    pub files: u64,
    pub directories: u64,
    pub synthetic: u64,
    /// Entradas con [`flags::DELETED`].
    pub deleted: u64,
    /// Nodos por categoría (índice = valor de [`Category`]).
    pub by_category: [u64; 7],
    /// Ficheros por estado de los datos (índice = valor de [`DataState`]; 0 = sin estado).
    pub by_data: [u64; 8],
    pub name_units: u64,
}

impl Stats {
    pub fn category(&self, c: Category) -> u64 {
        self.by_category[c as usize]
    }

    pub fn data(&self, d: DataState) -> u64 {
        self.by_data[d as usize]
    }
}

/// Nota de la construcción: anomalía del origen o límite alcanzado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub code: String,
    pub text: String,
    pub node: Option<NodeId>,
}

/// Constructor del árbol: añade nodos (el padre debe existir) y termina con [`VfsBuilder::finish`].
#[derive(Debug)]
pub struct VfsBuilder {
    limits: Limits,
    nodes: Vec<Node>,
    names: Vec<u16>,
    notes: Vec<Note>,
    notes_dropped: u64,
    partial: bool,
}

fn pack(n: &NewNode<'_>, parent: u32, depth: u16, name_start: u32) -> Node {
    let r = n.origin.unwrap_or_default();
    Node {
        parent,
        name_start,
        children_start: 0,
        children_len: 0,
        flags: n.flags,
        attributes: n.attributes,
        name_len: n.name.len() as u16,
        depth,
        sequence: r.sequence,
        parent_sequence: r.parent_sequence,
        kind: n.kind as u8,
        category: n.category as u8,
        name_rel: n.name_reliability as u8,
        data: if n.kind == NodeKind::File {
            n.data.map_or(0, |d| d as u8)
        } else {
            0
        },
        namespace: n.namespace.unwrap_or(255),
        has_ref: u8::from(n.origin.is_some()),
        size: n.size,
        allocated: n.allocated_size,
        record: r.record,
        parent_record: r.parent_record,
        times: [
            n.times.created,
            n.times.modified,
            n.times.changed,
            n.times.accessed,
        ],
    }
}

impl VfsBuilder {
    /// Constructor con la raíz ya creada (directorio sin nombre).
    pub fn new(limits: Limits, root: NewNode<'_>) -> Result<Self> {
        let mut b = VfsBuilder {
            limits,
            nodes: Vec::new(),
            names: Vec::new(),
            notes: Vec::new(),
            notes_dropped: 0,
            partial: false,
        };
        if limits.max_nodes == 0 {
            return Err(VfsError::LimitExceeded {
                what: "nodos",
                limit: 0,
            });
        }
        b.check_name(root.name.len())?;
        let start = b.names.len() as u32;
        b.names.extend_from_slice(root.name);
        let mut n = pack(&root, u32::MAX, 0, start);
        n.kind = if root.kind == NodeKind::File {
            NodeKind::Directory as u8
        } else {
            root.kind as u8
        };
        b.nodes.push(n);
        Ok(b)
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn check_name(&self, len: usize) -> Result<()> {
        if len > usize::from(u16::MAX) {
            return Err(VfsError::LimitExceeded {
                what: "unidades de un nombre",
                limit: u64::from(u16::MAX),
            });
        }
        let total = self.names.len() as u64 + len as u64;
        if total > self.limits.max_name_units || total > u64::from(u32::MAX) {
            return Err(VfsError::LimitExceeded {
                what: "unidades de nombre",
                limit: self.limits.max_name_units.min(u64::from(u32::MAX)),
            });
        }
        Ok(())
    }

    /// Añade un nodo bajo `parent` (que debe existir y ser un contenedor).
    pub fn push(&mut self, parent: NodeId, n: NewNode<'_>) -> Result<NodeId> {
        let p = self
            .nodes
            .get(parent.index())
            .ok_or(VfsError::NoSuchNode(parent.0))?;
        if p.kind == NodeKind::File as u8 {
            return Err(VfsError::NotAContainer(parent.0));
        }
        if self.nodes.len() >= self.limits.max_nodes as usize {
            return Err(VfsError::LimitExceeded {
                what: "nodos",
                limit: u64::from(self.limits.max_nodes),
            });
        }
        let depth = p.depth.saturating_add(1);
        if depth > self.limits.max_depth {
            return Err(VfsError::LimitExceeded {
                what: "profundidad",
                limit: u64::from(self.limits.max_depth),
            });
        }
        self.check_name(n.name.len())?;
        let start = self.names.len() as u32;
        self.names.extend_from_slice(n.name);
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(pack(&n, parent.0, depth, start));
        Ok(id)
    }

    /// Categoría de un nodo ya añadido.
    pub fn category(&self, id: NodeId) -> Option<Category> {
        self.nodes
            .get(id.index())
            .map(|n| Category::from_u8(n.category))
    }

    /// Tipo de un nodo ya añadido.
    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.nodes
            .get(id.index())
            .map(|n| NodeKind::from_u8(n.kind))
    }

    /// Referencia al origen de un nodo ya añadido.
    pub fn origin(&self, id: NodeId) -> Option<FsRef> {
        self.nodes.get(id.index()).and_then(origin_of)
    }

    /// Añade indicadores a un nodo.
    pub fn add_flags(&mut self, id: NodeId, f: u32) {
        if let Some(n) = self.nodes.get_mut(id.index()) {
            n.flags |= f;
        }
    }

    /// Cambia el estado de los datos de un fichero.
    pub fn set_data(&mut self, id: NodeId, d: Option<DataState>) {
        if let Some(n) = self.nodes.get_mut(id.index()) {
            if n.kind == NodeKind::File as u8 {
                n.data = d.map_or(0, |d| d as u8);
            }
        }
    }

    /// Añade una nota (como mucho [`MAX_NOTES`]; las demás solo se cuentan).
    pub fn note(&mut self, code: &str, text: String, node: Option<NodeId>) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(Note {
                code: code.to_string(),
                text,
                node,
            });
        } else {
            self.notes_dropped += 1;
        }
    }

    /// Marca el árbol como parcial (límite de un lector, cancelación...).
    pub fn set_partial(&mut self) {
        self.partial = true;
    }

    fn name_of(&self, n: &Node) -> &[u16] {
        &self.names[n.name_start as usize..n.name_start as usize + n.name_len as usize]
    }

    fn cmp_siblings(&self, a: u32, b: u32) -> Ordering {
        let (x, y) = (&self.nodes[a as usize], &self.nodes[b as usize]);
        let syn = |n: &Node| n.kind == NodeKind::Synthetic as u8;
        syn(x)
            .cmp(&syn(y))
            .then_with(|| name::cmp_names(self.name_of(x), self.name_of(y)))
            .then_with(|| x.category.cmp(&y.category))
            .then_with(|| x.record.cmp(&y.record))
            .then_with(|| a.cmp(&b))
    }

    /// Termina el árbol: lista de hijos ordenada, duplicados marcados y recuentos.
    pub fn finish(mut self) -> Vfs {
        let n = self.nodes.len();
        let mut counts = vec![0u32; n];
        for node in &self.nodes[1..] {
            counts[node.parent as usize] += 1;
        }
        let mut start = 0u32;
        for (i, c) in counts.iter().enumerate() {
            self.nodes[i].children_start = start;
            self.nodes[i].children_len = *c;
            start += c;
        }
        let mut children = vec![NodeId(0); n.saturating_sub(1)];
        let mut fill = vec![0u32; n];
        for (i, node) in self.nodes.iter().enumerate().skip(1) {
            let p = node.parent as usize;
            let at = self.nodes[p].children_start + fill[p];
            children[at as usize] = NodeId(i as u32);
            fill[p] += 1;
        }
        drop(fill);
        drop(counts);
        let mut dups = Vec::new();
        for i in 0..n {
            let (s, l) = (
                self.nodes[i].children_start as usize,
                self.nodes[i].children_len as usize,
            );
            if l < 2 {
                continue;
            }
            let slice = &mut children[s..s + l];
            slice.sort_by(|a, b| self.cmp_siblings(a.0, b.0));
            for w in slice.windows(2) {
                let (a, b) = (&self.nodes[w[0].index()], &self.nodes[w[1].index()]);
                if a.kind != NodeKind::Synthetic as u8
                    && b.kind != NodeKind::Synthetic as u8
                    && name::eq_ignore_case(self.name_of(a), self.name_of(b))
                {
                    dups.push(w[0]);
                    dups.push(w[1]);
                }
            }
        }
        for d in dups {
            self.nodes[d.index()].flags |= flags::DUPLICATE_NAME;
        }
        let mut stats = Stats {
            nodes: n as u64,
            name_units: self.names.len() as u64,
            ..Stats::default()
        };
        for node in &self.nodes {
            match NodeKind::from_u8(node.kind) {
                NodeKind::File => {
                    stats.files += 1;
                    stats.by_data[usize::from(node.data)] += 1;
                }
                NodeKind::Directory => stats.directories += 1,
                NodeKind::Synthetic => stats.synthetic += 1,
            }
            if node.flags & flags::DELETED != 0 {
                stats.deleted += 1;
            }
            stats.by_category[usize::from(node.category)] += 1;
        }
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        if self.notes_dropped > 0 {
            let dropped = self.notes_dropped;
            self.notes.push(Note {
                code: "notes_truncated".into(),
                text: format!("{dropped} notas más no se guardaron (límite {MAX_NOTES})"),
                node: None,
            });
        }
        Vfs {
            nodes: self.nodes,
            names: self.names,
            children,
            stats,
            notes: self.notes,
            partial: self.partial,
        }
    }
}

fn origin_of(n: &Node) -> Option<FsRef> {
    (n.has_ref != 0).then_some(FsRef {
        record: n.record,
        sequence: n.sequence,
        parent_record: n.parent_record,
        parent_sequence: n.parent_sequence,
    })
}

/// Árbol virtual terminado (inmutable, `Send + Sync`).
#[derive(Debug, Clone)]
pub struct Vfs {
    nodes: Vec<Node>,
    names: Vec<u16>,
    children: Vec<NodeId>,
    stats: Stats,
    notes: Vec<Note>,
    partial: bool,
}

impl Vfs {
    pub fn root(&self) -> NodeId {
        NodeId::ROOT
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, id: NodeId) -> bool {
        id.index() < self.nodes.len()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// `true` si la construcción no fue completa (límite de un lector o cancelación).
    pub fn is_partial(&self) -> bool {
        self.partial
    }

    /// Memoria ocupada por la arena (capacidad reservada de nodos, nombres e hijos).
    pub fn memory_bytes(&self) -> u64 {
        (self.nodes.capacity() * NODE_BYTES
            + self.names.capacity() * 2
            + self.children.capacity() * std::mem::size_of::<NodeId>()) as u64
    }

    fn raw(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    pub fn node(&self, id: NodeId) -> Option<NodeInfo> {
        let n = self.raw(id)?;
        Some(NodeInfo {
            id,
            parent: (id.0 != 0).then_some(NodeId(n.parent)),
            depth: n.depth,
            kind: NodeKind::from_u8(n.kind),
            category: Category::from_u8(n.category),
            name_reliability: NameReliability::from_u8(n.name_rel),
            data: DataState::from_u8(n.data),
            flags: n.flags,
            namespace: (n.namespace != 255).then_some(n.namespace),
            size: n.size,
            allocated_size: n.allocated,
            times: Times {
                created: n.times[0],
                modified: n.times[1],
                changed: n.times[2],
                accessed: n.times[3],
            },
            attributes: n.attributes,
            origin: origin_of(n),
            child_count: n.children_len,
        })
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let n = self.raw(id)?;
        (id.0 != 0).then_some(NodeId(n.parent))
    }

    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.raw(id).map(|n| NodeKind::from_u8(n.kind))
    }

    pub fn category(&self, id: NodeId) -> Option<Category> {
        self.raw(id).map(|n| Category::from_u8(n.category))
    }

    pub fn data(&self, id: NodeId) -> Option<DataState> {
        self.raw(id).and_then(|n| DataState::from_u8(n.data))
    }

    pub fn flags(&self, id: NodeId) -> u32 {
        self.raw(id).map_or(0, |n| n.flags)
    }

    /// Hijos en el orden determinista del módulo (vacío en ficheros o si no existe).
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.raw(id).map_or(&[], |n| {
            &self.children
                [n.children_start as usize..n.children_start as usize + n.children_len as usize]
        })
    }

    /// Hijo `index` (enumeración por índice para la ABI).
    pub fn child(&self, id: NodeId, index: u32) -> Option<NodeId> {
        self.children(id).get(index as usize).copied()
    }

    /// Nombre en unidades UTF-16 originales (vacío en la raíz).
    pub fn name_units(&self, id: NodeId) -> &[u16] {
        self.raw(id).map_or(&[], |n| {
            &self.names[n.name_start as usize..n.name_start as usize + n.name_len as usize]
        })
    }

    /// Forma visible del nombre (U+FFFD en las unidades sueltas).
    pub fn name(&self, id: NodeId) -> String {
        name::display(self.name_units(id))
    }

    /// Nombre escapado para rutas (ver [`crate::name`]). Las carpetas sintéticas no se escapan.
    pub fn escaped_name(&self, id: NodeId) -> String {
        if self.kind(id) == Some(NodeKind::Synthetic) {
            return self.name(id);
        }
        escape_component(self.name_units(id)).into_owned()
    }

    /// Antecesores de `id` desde la raíz (sin incluirla) hasta `id`.
    pub fn ancestry(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = Vec::new();
        let mut cur = id;
        while cur.0 != 0 && self.contains(cur) {
            v.push(cur);
            cur = NodeId(self.nodes[cur.index()].parent);
        }
        v.reverse();
        v
    }

    /// Ruta visible desde la raíz (`/` en la raíz), con los componentes escapados.
    pub fn path(&self, id: NodeId) -> String {
        self.relative_path(NodeId::ROOT, id)
            .map_or_else(String::new, |p| format!("/{p}"))
    }

    /// Ruta de `id` relativa a `base` (sin `/` inicial; vacía si son el mismo nodo). `None` si
    /// `id` no cuelga de `base`.
    pub fn relative_path(&self, base: NodeId, id: NodeId) -> Option<String> {
        if !self.contains(id) || !self.contains(base) {
            return None;
        }
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != base {
            if cur.0 == 0 {
                return None;
            }
            parts.push(self.escaped_name(cur));
            cur = NodeId(self.nodes[cur.index()].parent);
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// Hijo de `dir` con el componente `c`: exacto por la forma visible o escapada y, si no,
    /// sin distinguir mayúsculas. Entre varios, el primero en el orden de los hijos.
    pub fn lookup(&self, dir: NodeId, c: &str) -> Option<NodeId> {
        let kids = self.children(dir);
        if let Some(k) = kids
            .iter()
            .find(|k| self.name(**k) == c || self.escaped_name(**k) == c)
        {
            return Some(*k);
        }
        let want = name::unescape_component(c).unwrap_or_else(|| name::units(c));
        kids.iter()
            .find(|k| name::eq_ignore_case(self.name_units(**k), &want))
            .copied()
    }

    /// Nodo de una ruta (`/a/b`, también con `\` como separador si no hay `/`). Los componentes
    /// vacíos se ignoran: `/` y la cadena vacía son la raíz.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let sep = if path.contains('/') { '/' } else { '\\' };
        let mut cur = NodeId::ROOT;
        for c in path.split(sep).filter(|c| !c.is_empty()) {
            cur = self.lookup(cur, c)?;
        }
        Some(cur)
    }

    /// Nodos con la referencia `record` (en orden de identificador; varios con enlaces duros).
    pub fn find_record(&self, record: u64) -> Vec<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.has_ref != 0 && n.record == record)
            .map(|(i, _)| NodeId(i as u32))
            .collect()
    }

    /// Recorrido en profundidad desde `start` (incluido) en el orden de los hijos.
    pub fn preorder(&self, start: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if !self.contains(start) {
            return out;
        }
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.children(id).iter().rev().copied());
        }
        out
    }
}
