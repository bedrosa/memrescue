//! Entradas residuales de los índices de directorio `$I30` (#23, ADR-027).
//!
//! Cuando se borra un nombre, el sistema de archivos quita su entrada del nodo moviendo las
//! siguientes hacia el principio: los bytes de las entradas antiguas pueden quedar en el **espacio
//! libre del nodo** (entre los bytes en uso y los asignados). Un nodo que se libera entero se marca a
//! 0 en `$BITMAP` del índice, pero sus bytes siguen en el disco (**nodo libre**). Y el índice de un
//! directorio borrado conserva sus nodos tal como estaban (**índice de un directorio borrado**).
//! Además, una entrada de la parte en uso de un índice vivo puede apuntar a un registro que ya no es
//! ese fichero (**entrada caducada**, ver [`crate::dir::EntryState`]).
//!
//! Nada de eso es fiable por sí mismo: un nodo libre o el espacio libre pueden tener restos
//! parciales (la cabecera de una entrada pisada por otra) o bytes de otra cosa. Por eso cada
//! candidata se valida de forma estricta ([`parse_candidates`]) y solo se aceptan las que cumplen
//! todo:
//!
//! - Nodo leído entero con bytes fiables (un sector ilegible nunca se interpreta), firma `INDX`,
//!   secuencia de actualización sin tolerancia, VCN de la cabecera igual al de su posición y
//!   cabecera de nodo dentro de límites. Lo que no cumple se cuenta y se salta.
//! - Entrada alineada a 8, indicadores sin bits desconocidos y sin el de última entrada, longitud
//!   exactamente `alinear8(0x10 + clave) (+ 8 con subnodo)` dentro del tramo, clave de exactamente
//!   `0x42 + 2 × caracteres` bytes que es un `$FILE_NAME` válido, sin unidades nulas ni `/`.
//! - Padre de la clave = el directorio que se lee (número y secuencia; en un directorio borrado,
//!   la secuencia de antes de liberarlo).
//! - Referencia plausible: registro de usuario (≥ 16) dentro de la MFT, distinto del propio
//!   directorio y con secuencia distinta de 0.
//!
//! Las que tienen la estructura y el padre correctos pero no la referencia se cuentan como
//! rechazadas ([`ResidualStats::rejected`]); el resto de bytes no son candidatas. La combinación
//! con los registros de la MFT está en [`crate::deleted`].

use crate::deleted::EntryOrigin;
use crate::dir::FIRST_USER_RECORD;
use crate::index::{
    entry_flags, has_i30, parse_node, IndexEntry, IndexIssue, IndexKey, IndexReader, NodeId,
    ENTRY_HEADER, INDX_HEADER_AT, MAX_INDEX_NODES,
};
use crate::mft::RecordOutcome;
use crate::record::FileReference;
use crate::values::FileName;
use crate::{NtfsError, NtfsFile, NtfsVolume};

/// Desplazamiento de la cabecera de nodo en el valor de `$INDEX_ROOT`.
const ROOT_HEADER_AT: usize = 0x10;
/// Bytes fijos de un `$FILE_NAME` antes del nombre.
const FILE_NAME_FIXED: usize = 0x42;

/// Entrada residual aceptada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidualEntry {
    /// Directorio leído (número y secuencia actual de su registro).
    pub directory: FileReference,
    pub node: NodeId,
    /// Desplazamiento de la entrada en el nodo (o en el valor de `$INDEX_ROOT`).
    pub offset: usize,
    pub origin: EntryOrigin,
    /// Referencia de la entrada (lo que afirma el índice).
    pub reference: FileReference,
    pub key: FileName,
    /// Nombre en unidades UTF-16 tal como está en disco.
    pub units: Vec<u16>,
}

/// Recuentos de la búsqueda de entradas residuales.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResidualStats {
    /// Directorios cuyo índice se leyó (vivos y borrados).
    pub directories: u64,
    /// Directorios cuyo índice no se pudo abrir (raíz no interpretable, registro dañado...).
    pub directories_unusable: u64,
    /// Nodos `INDX` leídos y válidos.
    pub nodes: u64,
    /// Nodos con algún sector no fiable (no se interpretan).
    pub nodes_unreadable: u64,
    /// Nodos sin firma, con fixups o VCN incorrectos o cabecera fuera de límites (no se usan).
    pub nodes_invalid: u64,
    /// Nodos libres según `$BITMAP` con un nodo `INDX` válido.
    pub free_nodes: u64,
    /// Bytes de espacio libre de nodos examinados.
    pub slack_bytes: u64,
    /// Entradas aceptadas (antes de quitar repetidas).
    pub accepted: u64,
    /// Candidatas con la estructura y el padre correctos pero una referencia imposible.
    pub rejected: u64,
    /// Se alcanzó el máximo de entradas residuales.
    pub truncated: bool,
}

impl ResidualStats {
    pub fn add(&mut self, o: &ResidualStats) {
        self.directories += o.directories;
        self.directories_unusable += o.directories_unusable;
        self.nodes += o.nodes;
        self.nodes_unreadable += o.nodes_unreadable;
        self.nodes_invalid += o.nodes_invalid;
        self.free_nodes += o.free_nodes;
        self.slack_bytes += o.slack_bytes;
        self.accepted += o.accepted;
        self.rejected += o.rejected;
        self.truncated |= o.truncated;
    }
}

/// Contexto de validación: el directorio que se lee y el tamaño de la MFT.
#[derive(Debug, Clone, Copy)]
pub struct DirContext {
    pub directory: FileReference,
    pub in_use: bool,
    pub record_count: u64,
}

impl DirContext {
    /// `true` si `parent` (el padre que dice la clave) es este directorio. Un directorio liberado
    /// lleva la secuencia incrementada: se admite exactamente una unidad más.
    pub fn is_parent(&self, parent: FileReference) -> bool {
        parent.record == self.directory.record
            && (parent.sequence == self.directory.sequence
                || (!self.in_use && self.directory.sequence == parent.sequence.wrapping_add(1)))
    }

    /// `true` si la referencia puede ser la de un fichero de este directorio.
    pub fn plausible_reference(&self, r: FileReference) -> bool {
        r.record >= FIRST_USER_RECORD
            && r.record < self.record_count
            && r.record != self.directory.record
            && r.sequence != 0
    }
}

/// Candidata de [`parse_candidates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub offset: usize,
    pub reference: FileReference,
    pub key: IndexKey,
}

fn le16(s: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([s[o], s[o + 1]])
}

fn le64(s: &[u8], o: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[o..o + 8]);
    u64::from_le_bytes(b)
}

/// Clave `$FILE_NAME` de exactamente `0x42 + 2 × caracteres` bytes, sin unidades nulas ni `/`.
fn strict_key(v: &[u8]) -> Option<IndexKey> {
    if v.len() < FILE_NAME_FIXED || v.len() != FILE_NAME_FIXED + 2 * usize::from(v[0x40]) {
        return None;
    }
    let file_name = FileName::parse(v).ok()?;
    let units: Vec<u16> = v[FILE_NAME_FIXED..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    if units.iter().any(|&u| u == 0 || u == u16::from(b'/')) {
        return None;
    }
    Some(IndexKey { file_name, units })
}

/// Busca entradas en `buf[from..to)` (posiciones alineadas a 8) con las reglas del módulo. Devuelve
/// las aceptadas y el número de rechazadas. Nunca lee fuera de `buf[from..to)`.
pub fn parse_candidates(
    buf: &[u8],
    from: usize,
    to: usize,
    ctx: &DirContext,
) -> (Vec<Candidate>, u64) {
    let to = to.min(buf.len());
    let mut out = Vec::new();
    let mut rejected = 0u64;
    let mut pos = from.next_multiple_of(8);
    while pos
        .checked_add(ENTRY_HEADER + FILE_NAME_FIXED)
        .is_some_and(|e| e <= to)
    {
        let step = (|| {
            let length = usize::from(le16(buf, pos + 8));
            let kl = usize::from(le16(buf, pos + 0x0A));
            let flags = le16(buf, pos + 0x0C);
            if flags & !(entry_flags::SUBNODE | entry_flags::LAST) != 0
                || flags & entry_flags::LAST != 0
            {
                return None;
            }
            let sub = flags & entry_flags::SUBNODE != 0;
            let expected = (ENTRY_HEADER + kl).next_multiple_of(8) + if sub { 8 } else { 0 };
            if kl < FILE_NAME_FIXED || length != expected || pos + length > to {
                return None;
            }
            let key = strict_key(&buf[pos + ENTRY_HEADER..pos + ENTRY_HEADER + kl])?;
            if !ctx.is_parent(key.file_name.parent) {
                return None;
            }
            Some((length, FileReference::from_u64(le64(buf, pos)), key))
        })();
        match step {
            Some((length, reference, key)) => {
                if ctx.plausible_reference(reference) {
                    out.push(Candidate {
                        offset: pos,
                        reference,
                        key,
                    });
                    pos += length;
                } else {
                    rejected += 1;
                    pos += 8;
                }
            }
            None => pos += 8,
        }
    }
    (out, rejected)
}

/// Lector de entradas residuales con límite de entradas.
struct Collector<'a> {
    vol: &'a NtfsVolume,
    ctx: DirContext,
    out: &'a mut Vec<ResidualEntry>,
    stats: ResidualStats,
    limit: usize,
}

impl Collector<'_> {
    fn push(&mut self, node: NodeId, origin: EntryOrigin, c: Candidate) {
        if self.out.len() >= self.limit {
            self.stats.truncated = true;
            return;
        }
        self.stats.accepted += 1;
        self.out.push(ResidualEntry {
            directory: self.ctx.directory,
            node,
            offset: c.offset,
            origin,
            reference: c.reference,
            key: c.key.file_name,
            units: c.key.units,
        });
    }

    fn scan(&mut self, buf: &[u8], from: usize, to: usize, node: NodeId, origin: EntryOrigin) {
        if origin == EntryOrigin::IndexSlack {
            self.stats.slack_bytes += to.saturating_sub(from) as u64;
        }
        let (found, rejected) = parse_candidates(buf, from, to, &self.ctx);
        self.stats.rejected += rejected;
        for c in found {
            self.push(node, origin, c);
        }
    }

    /// Entradas de la parte en uso de un nodo vivo (ya validadas por [`parse_node`]): solo las
    /// que cumplen las mismas reglas de padre y referencia.
    fn stale(&mut self, node: NodeId, entries: &[IndexEntry]) {
        for e in entries {
            let Some(k) = &e.key else { continue };
            if e.is_last() || !self.ctx.is_parent(k.file_name.parent) {
                continue;
            }
            // La entrada «.» de la raíz y las de ficheros del sistema no son de usuario.
            if e.reference.record == self.ctx.directory.record
                || e.reference.record < FIRST_USER_RECORD
            {
                continue;
            }
            if !self.ctx.plausible_reference(e.reference) {
                self.stats.rejected += 1;
                continue;
            }
            // Las entradas vivas son el directorio, no restos. Las que apuntan a un registro no
            // interpretable son daños de un fichero vivo (el listado las informa como
            // `record_unusable`), no borrados: tampoco se recogen.
            match self.vol.read_record(e.reference.record) {
                Ok(RecordOutcome::Parsed(r)) => {
                    if r.in_use()
                        && r.header().is_base()
                        && r.header().sequence == e.reference.sequence
                    {
                        continue;
                    }
                }
                _ => continue,
            }
            self.push(
                node,
                EntryOrigin::IndexStale,
                Candidate {
                    offset: e.offset,
                    reference: e.reference,
                    key: k.clone(),
                },
            );
        }
    }

    /// Un nodo (raíz o `INDX`) con su cabecera en `at`.
    fn node(&mut self, buf: &[u8], at: usize, id: NodeId, origin: EntryOrigin) {
        let node = match parse_node(buf, at, id) {
            Ok(n) => n,
            Err(_) => {
                self.stats.nodes_invalid += 1;
                return;
            }
        };
        let eo = at + node.header.entries_offset as usize;
        let used = at + node.header.index_length as usize;
        let alloc = at + node.header.allocated as usize;
        match origin {
            // Nodo vivo de un directorio vivo: la parte en uso (caducadas) y el espacio libre.
            EntryOrigin::IndexSlack => {
                self.stale(id, &node.entries);
                self.scan(buf, used, alloc, id, EntryOrigin::IndexSlack);
            }
            // Nodo libre o de un directorio borrado: todo el nodo es residual.
            _ => self.scan(buf, eo, alloc, id, origin),
        }
    }
}

/// Máximo de entradas residuales por defecto.
pub const MAX_RESIDUAL_ENTRIES: usize = 1_000_000;

/// Entradas residuales del índice `$I30` del directorio `file` (vivo o borrado), añadidas a `out`
/// hasta `limit` en total. Un directorio sin índice utilizable cuenta en `directories_unusable`.
pub fn collect_directory(
    vol: &NtfsVolume,
    file: &NtfsFile,
    out: &mut Vec<ResidualEntry>,
    limit: usize,
) -> ResidualStats {
    let mut stats = ResidualStats::default();
    if !has_i30(file) {
        return stats;
    }
    let reader = match IndexReader::open(vol, file) {
        Ok(r) => r,
        Err(_) => {
            stats.directories_unusable = 1;
            return stats;
        }
    };
    let ctx = DirContext {
        directory: file.reference(),
        in_use: file.in_use(),
        record_count: vol.record_count(),
    };
    let mut c = Collector {
        vol,
        ctx,
        out,
        stats: ResidualStats {
            directories: 1,
            ..ResidualStats::default()
        },
        limit,
    };
    let live_origin = if ctx.in_use {
        EntryOrigin::IndexSlack
    } else {
        EntryOrigin::DeletedDirectoryIndex
    };
    let root = reader.root_value().to_vec();
    c.node(&root, ROOT_HEADER_AT, NodeId::Root, live_origin);
    let slots = reader.slot_count().min(MAX_INDEX_NODES as u64);
    for slot in 0..slots {
        if c.stats.truncated {
            break;
        }
        // Sin `$BITMAP` utilizable no se sabe qué nodos están libres: se tratan como asignados
        // (su parte en uso se contrasta igual con la MFT y su espacio libre se examina).
        let allocated = reader.slot_allocated(slot).unwrap_or(true);
        let origin = if !ctx.in_use {
            EntryOrigin::DeletedDirectoryIndex
        } else if allocated {
            EntryOrigin::IndexSlack
        } else {
            EntryOrigin::IndexFreeNode
        };
        let vcn = reader.slot_vcn(slot);
        match reader.read_slot(slot) {
            Ok(buf) => {
                c.stats.nodes += 1;
                if origin == EntryOrigin::IndexFreeNode {
                    c.stats.free_nodes += 1;
                }
                c.node(&buf, INDX_HEADER_AT, NodeId::Vcn(vcn), origin);
            }
            Err(IndexIssue::BlockUnreadable { .. }) => c.stats.nodes_unreadable += 1,
            Err(_) => c.stats.nodes_invalid += 1,
        }
    }
    c.stats
}

/// Entradas residuales del directorio `record` (para pruebas y diagnóstico).
pub fn residual_entries(
    vol: &NtfsVolume,
    record: u64,
) -> Result<(Vec<ResidualEntry>, ResidualStats), NtfsError> {
    let file = vol.file(record)?;
    let mut out = Vec::new();
    let stats = collect_directory(vol, &file, &mut out, MAX_RESIDUAL_ENTRIES);
    Ok((out, stats))
}

#[cfg(test)]
#[path = "residual_tests.rs"]
mod tests;
