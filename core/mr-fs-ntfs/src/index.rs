//! Índices de directorio `$I30`: `$INDEX_ROOT` (residente), nodos `INDX` de `$INDEX_ALLOCATION` y
//! su `$BITMAP`, recorridos como un árbol B con límites explícitos.
//!
//! Disposición pública (little-endian):
//!
//! - `$INDEX_ROOT` (0x90, nombre `$I30`): tipo de atributo indexado (u32, 0x00; 0x30 =
//!   `$FILE_NAME`), regla de colación (u32, 0x04; 1 = nombres de fichero), tamaño del nodo de índice
//!   en bytes (u32, 0x08), clústeres por nodo (u8, 0x0C) y la cabecera de nodo en 0x10.
//! - Cabecera de nodo (16 bytes): desplazamiento de la primera entrada (u32, 0x00), bytes en uso
//!   (u32, 0x04) y asignados (u32, 0x08), los tres relativos al inicio de la cabecera, e indicadores
//!   (u8, 0x0C; 0x01 = el nodo tiene subnodos).
//! - Nodo `INDX` (un registro de `tamaño del nodo` bytes en `$INDEX_ALLOCATION`): firma `INDX`,
//!   secuencia de actualización igual que los registros FILE (USA en 0x04/0x06, se comprueba con
//!   [`crate::record::apply_fixups`]), LSN (0x08), VCN del propio nodo (u64, 0x10) y la cabecera de
//!   nodo en 0x18. El VCN se expresa en clústeres si el nodo mide al menos un clúster y en unidades
//!   de 512 bytes si es menor.
//! - Entrada: referencia MFT (u64, 0x00), longitud (u16, 0x08, múltiplo de 8), longitud de la clave
//!   (u16, 0x0A), indicadores (u16, 0x0C; 0x01 = tiene subnodo, 0x02 = última entrada) y la clave
//!   (un valor `$FILE_NAME`) en 0x10; si tiene subnodo, los últimos 8 bytes de la entrada son el VCN
//!   del nodo hijo. La última entrada de cada nodo no tiene clave (solo puede llevar el subnodo de
//!   los nombres mayores que todos los del nodo).
//! - `$BITMAP` (0xB0, nombre `$I30`): un bit por nodo de `$INDEX_ALLOCATION`; un nodo con el bit a 0
//!   está libre (su contenido es residual).
//!
//! Reglas: cada nodo se comprueba por separado (firma, fixups sin tolerancia, VCN de la cabecera
//! igual al VCN por el que se llegó, cabecera y entradas dentro de los bytes en uso, última entrada
//! presente) y un nodo dañado se informa con su anomalía tipada y se salta: el resto del árbol se
//! sigue listando. Un puntero a subnodo debe estar alineado al tamaño del nodo, dentro de
//! `$INDEX_ALLOCATION` y con su bit de `$BITMAP` a 1. El recorrido tiene límites de profundidad
//! ([`MAX_INDEX_DEPTH`]), nodos ([`MAX_INDEX_NODES`]) y entradas ([`MAX_INDEX_ENTRIES`]) y nunca
//! visita dos veces un nodo (un ciclo se informa como `index_loop`). Solo se interpretan índices de
//! nombres de fichero (`$I30` con colación 1); otros índices quedan fuera de este módulo.

use std::collections::HashSet;
use std::fmt;

use crate::attr::types;
use crate::file::{NtfsFile, StreamLayout};
use crate::record::{apply_fixups, FileReference, FixupError, USA_STRIDE};
use crate::upcase::Upcase;
use crate::values::{FileName, ValueError};
use crate::{NtfsError, NtfsVolume};

/// Nombre del índice de nombres de fichero de un directorio.
pub const I30: &str = "$I30";
/// Regla de colación de los nombres de fichero.
pub const COLLATION_FILE_NAME: u32 = 1;
/// Profundidad máxima del árbol B (la raíz es 0).
pub const MAX_INDEX_DEPTH: usize = 32;
/// Nodos `INDX` máximos que se leen de un índice.
pub const MAX_INDEX_NODES: usize = 1 << 20;
/// Entradas máximas de un índice.
pub const MAX_INDEX_ENTRIES: usize = 1 << 22;
/// Tamaño máximo de `$BITMAP` que se lee (32 millones de nodos).
pub const MAX_BITMAP_BYTES: u64 = 4 << 20;
/// Cabecera de nodo.
pub const NODE_HEADER: usize = 0x10;
/// Cabecera de entrada.
pub const ENTRY_HEADER: usize = 0x10;
/// Desplazamiento de la cabecera de nodo en un `INDX`.
pub const INDX_HEADER_AT: usize = 0x18;

/// Indicadores de una entrada.
pub mod entry_flags {
    pub const SUBNODE: u16 = 0x01;
    pub const LAST: u16 = 0x02;
}

/// Nodo del índice: la raíz o un nodo `INDX` por su VCN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeId {
    Root,
    Vcn(u64),
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeId::Root => write!(f, "raíz del índice"),
            NodeId::Vcn(v) => write!(f, "nodo INDX del VCN {v}"),
        }
    }
}

/// Problema de un índice o de una de sus entradas (tipado, con código estable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexIssue {
    /// `$INDEX_ROOT` más corto que su cabecera.
    RootTooShort { len: usize },
    /// El índice no es de `$FILE_NAME`.
    RootType { type_code: u32 },
    /// Regla de colación distinta de la de nombres de fichero.
    RootCollation { rule: u32 },
    /// Tamaño de nodo que no es potencia de 2 entre 512 B y 64 KiB.
    BlockSize { size: u32 },
    /// Tamaño de nodo distinto del del arranque (informativo; manda el del índice).
    BlockSizeMismatch { root: u32, boot: u32 },
    /// Cabecera de nodo fuera de límites.
    Header {
        entries_offset: u32,
        index_length: u32,
        allocated: u32,
    },
    /// Entrada con longitud no válida o que se sale de los bytes en uso.
    EntryLength { offset: usize, length: u16 },
    /// Clave que no cabe en su entrada (o entrada sin clave que no es la última).
    EntryKeyLength { offset: usize, key_length: u16 },
    /// Los bytes en uso terminan sin la entrada final.
    MissingLast,
    /// Clave que no es un `$FILE_NAME` válido (la entrada se ignora).
    EntryKey { offset: usize, error: ValueError },
    /// Nodo sin firma `INDX` (a ceros si `empty`).
    BlockSignature { found: [u8; 4] },
    /// Secuencia de actualización del nodo no válida o tramo que no coincide.
    BlockFixup(FixupError),
    /// El VCN guardado en el nodo no es el VCN por el que se llegó.
    BlockVcn { expected: u64, found: u64 },
    /// El nodo tiene bytes no fiables (sector ilegible, sin mapa, fuera del origen...).
    BlockUnreadable { detail: String },
    /// Puntero a subnodo desalineado o fuera de `$INDEX_ALLOCATION`.
    SubnodeOutOfRange { vcn: u64 },
    /// Puntero a un nodo marcado libre en `$BITMAP`.
    NodeUnallocated { vcn: u64 },
    /// Nodo al que se vuelve a llegar (ciclo o nodo compartido).
    Loop { vcn: u64 },
    /// Subnodo más allá de la profundidad máxima.
    Depth { vcn: u64 },
    /// Límite de nodos alcanzado.
    NodeLimit,
    /// Límite de entradas alcanzado.
    EntryLimit,
    /// Hay subnodos pero no `$INDEX_ALLOCATION`.
    AllocationMissing,
    /// `$INDEX_ALLOCATION` no se puede usar.
    AllocationUnusable { detail: String },
    /// `$BITMAP` ausente, ilegible o desmesurado: no se comprueba la asignación de los nodos.
    BitmapUnusable { detail: String },
    /// Entradas de un nodo fuera del orden de colación.
    Order,
    /// La entrada dice estar en otro directorio.
    EntryParentMismatch { record: u64, parent: FileReference },
    /// Entrada repetida (misma referencia y mismo nombre).
    EntryDuplicate { record: u64 },
    /// Entrada que apunta al propio directorio (distinta de «.» en la raíz).
    EntrySelfReference,
    /// El registro no tiene un `$FILE_NAME` con ese nombre en este directorio.
    EntryNameNotInRecord { record: u64 },
}

impl IndexIssue {
    /// Código estable.
    pub fn code(&self) -> &'static str {
        match self {
            IndexIssue::RootTooShort { .. } => "index_root_too_short",
            IndexIssue::RootType { .. } => "index_root_type",
            IndexIssue::RootCollation { .. } => "index_root_collation",
            IndexIssue::BlockSize { .. } => "index_block_size",
            IndexIssue::BlockSizeMismatch { .. } => "index_block_size_mismatch",
            IndexIssue::Header { .. } => "index_header_bounds",
            IndexIssue::EntryLength { .. } => "index_entry_length",
            IndexIssue::EntryKeyLength { .. } => "index_entry_key_length",
            IndexIssue::MissingLast => "index_missing_last_entry",
            IndexIssue::EntryKey { .. } => "index_entry_key",
            IndexIssue::BlockSignature { .. } => "index_block_signature",
            IndexIssue::BlockFixup(FixupError::Mismatch { .. }) => "index_block_fixup_mismatch",
            IndexIssue::BlockFixup(_) => "index_block_usa_invalid",
            IndexIssue::BlockVcn { .. } => "index_block_vcn",
            IndexIssue::BlockUnreadable { .. } => "index_block_unreadable",
            IndexIssue::SubnodeOutOfRange { .. } => "index_subnode_out_of_range",
            IndexIssue::NodeUnallocated { .. } => "index_node_unallocated",
            IndexIssue::Loop { .. } => "index_loop",
            IndexIssue::Depth { .. } => "index_depth",
            IndexIssue::NodeLimit => "index_node_limit",
            IndexIssue::EntryLimit => "index_entry_limit",
            IndexIssue::AllocationMissing => "index_allocation_missing",
            IndexIssue::AllocationUnusable { .. } => "index_allocation_unusable",
            IndexIssue::BitmapUnusable { .. } => "index_bitmap_unusable",
            IndexIssue::Order => "index_order",
            IndexIssue::EntryParentMismatch { .. } => "entry_parent_mismatch",
            IndexIssue::EntryDuplicate { .. } => "entry_duplicate",
            IndexIssue::EntrySelfReference => "entry_self_reference",
            IndexIssue::EntryNameNotInRecord { .. } => "entry_name_not_in_record",
        }
    }

    /// `true` si el problema deja nodos sin leer (parte del directorio puede faltar).
    pub fn loses_entries(&self) -> bool {
        !matches!(
            self,
            IndexIssue::BlockSizeMismatch { .. }
                | IndexIssue::BitmapUnusable { .. }
                | IndexIssue::Order
                | IndexIssue::EntryParentMismatch { .. }
                | IndexIssue::EntryDuplicate { .. }
                | IndexIssue::EntrySelfReference
                | IndexIssue::EntryNameNotInRecord { .. }
        )
    }

    /// `true` si se debe a sectores no fiables del origen.
    pub fn is_unreadable(&self) -> bool {
        matches!(self, IndexIssue::BlockUnreadable { .. })
    }
}

impl fmt::Display for IndexIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexIssue::RootTooShort { len } => {
                write!(f, "$INDEX_ROOT de {len} bytes (mínimo 32)")
            }
            IndexIssue::RootType { type_code } => {
                write!(f, "el índice $I30 indexa el tipo 0x{type_code:X} en lugar de $FILE_NAME")
            }
            IndexIssue::RootCollation { rule } => {
                write!(f, "regla de colación {rule} (se esperaba 1, nombres de fichero)")
            }
            IndexIssue::BlockSize { size } => write!(f, "tamaño de nodo de índice {size} no válido"),
            IndexIssue::BlockSizeMismatch { root, boot } => write!(
                f,
                "tamaño de nodo {root} distinto del del arranque ({boot}); se usa el del índice"
            ),
            IndexIssue::Header {
                entries_offset,
                index_length,
                allocated,
            } => write!(
                f,
                "cabecera de nodo fuera de límites (primera entrada {entries_offset}, en uso {index_length}, asignados {allocated})"
            ),
            IndexIssue::EntryLength { offset, length } => write!(
                f,
                "entrada en el byte {offset} con longitud {length} no válida o fuera de los bytes en uso"
            ),
            IndexIssue::EntryKeyLength { offset, key_length } => write!(
                f,
                "entrada en el byte {offset} con una clave de {key_length} bytes que no cabe"
            ),
            IndexIssue::MissingLast => write!(f, "el nodo termina sin la entrada final"),
            IndexIssue::EntryKey { offset, error } => {
                write!(f, "entrada en el byte {offset}: clave no válida ({error})")
            }
            IndexIssue::BlockSignature { found } => {
                if found == &[0; 4] {
                    write!(f, "nodo a ceros (sin firma INDX)")
                } else {
                    write!(
                        f,
                        "firma {:02X} {:02X} {:02X} {:02X} en lugar de INDX",
                        found[0], found[1], found[2], found[3]
                    )
                }
            }
            IndexIssue::BlockFixup(e) => write!(f, "nodo INDX: {e}"),
            IndexIssue::BlockVcn { expected, found } => write!(
                f,
                "el nodo dice ser el VCN {found} pero se llegó por el VCN {expected}"
            ),
            IndexIssue::BlockUnreadable { detail } => {
                write!(f, "nodo con bytes no fiables: {detail}; no se interpreta")
            }
            IndexIssue::SubnodeOutOfRange { vcn } => write!(
                f,
                "puntero a subnodo VCN {vcn} desalineado o fuera de $INDEX_ALLOCATION"
            ),
            IndexIssue::NodeUnallocated { vcn } => write!(
                f,
                "puntero al nodo VCN {vcn}, libre según $BITMAP: su contenido es residual y no se lista"
            ),
            IndexIssue::Loop { vcn } => {
                write!(f, "se vuelve a llegar al nodo VCN {vcn}: ciclo, no se sigue")
            }
            IndexIssue::Depth { vcn } => write!(
                f,
                "el subnodo VCN {vcn} supera la profundidad máxima ({MAX_INDEX_DEPTH})"
            ),
            IndexIssue::NodeLimit => write!(f, "límite de {MAX_INDEX_NODES} nodos alcanzado"),
            IndexIssue::EntryLimit => {
                write!(f, "límite de {MAX_INDEX_ENTRIES} entradas alcanzado")
            }
            IndexIssue::AllocationMissing => write!(
                f,
                "hay punteros a subnodos pero el directorio no tiene $INDEX_ALLOCATION"
            ),
            IndexIssue::AllocationUnusable { detail } => {
                write!(f, "$INDEX_ALLOCATION no se puede usar: {detail}")
            }
            IndexIssue::BitmapUnusable { detail } => write!(
                f,
                "$BITMAP del índice no utilizable ({detail}): no se comprueba la asignación de los nodos"
            ),
            IndexIssue::Order => write!(f, "entradas fuera del orden de colación"),
            IndexIssue::EntryParentMismatch { record, parent } => write!(
                f,
                "la entrada del registro {record} dice estar en el directorio {} (secuencia {})",
                parent.record, parent.sequence
            ),
            IndexIssue::EntryDuplicate { record } => {
                write!(f, "entrada repetida del registro {record}")
            }
            IndexIssue::EntrySelfReference => {
                write!(f, "entrada que apunta al propio directorio")
            }
            IndexIssue::EntryNameNotInRecord { record } => write!(
                f,
                "el registro {record} no tiene un $FILE_NAME con ese nombre en este directorio"
            ),
        }
    }
}

/// Problema localizado en un nodo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexAnomaly {
    pub node: NodeId,
    pub issue: IndexIssue,
}

impl fmt::Display for IndexAnomaly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.node, self.issue)
    }
}

/// Clave de una entrada: el `$FILE_NAME` y su nombre en unidades UTF-16 sin alterar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKey {
    pub file_name: FileName,
    pub units: Vec<u16>,
}

/// Entrada de un nodo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Referencia MFT (sin significado en la última entrada).
    pub reference: FileReference,
    pub flags: u16,
    /// Clave (`None` en la última entrada o si no es un `$FILE_NAME` válido).
    pub key: Option<IndexKey>,
    pub subnode: Option<u64>,
    /// Desplazamiento de la entrada en el búfer del nodo.
    pub offset: usize,
}

impl IndexEntry {
    pub fn is_last(&self) -> bool {
        self.flags & entry_flags::LAST != 0
    }
}

/// Cabecera de nodo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHeader {
    pub entries_offset: u32,
    pub index_length: u32,
    pub allocated: u32,
    pub flags: u8,
}

/// Nodo interpretado: entradas válidas y, si el recorrido de entradas se detuvo, el motivo (las
/// entradas anteriores se conservan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub header: NodeHeader,
    pub entries: Vec<IndexEntry>,
    pub issues: Vec<IndexIssue>,
}

/// Cabecera de `$INDEX_ROOT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootInfo {
    pub attr_type: u32,
    pub collation: u32,
    pub block_size: u32,
    pub clusters_per_block: u8,
}

fn le16(s: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([s[o], s[o + 1]])
}

fn le32(s: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]])
}

fn le64(s: &[u8], o: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[o..o + 8]);
    u64::from_le_bytes(b)
}

/// Interpreta la cabecera de nodo en `at` y sus entradas. `buf` es el nodo entero; los límites
/// de la cabecera se comprueban contra `buf.len()`.
pub fn parse_node(buf: &[u8], at: usize, id: NodeId) -> Result<Node, IndexIssue> {
    if at + NODE_HEADER > buf.len() {
        return Err(IndexIssue::Header {
            entries_offset: 0,
            index_length: 0,
            allocated: 0,
        });
    }
    let header = NodeHeader {
        entries_offset: le32(buf, at),
        index_length: le32(buf, at + 4),
        allocated: le32(buf, at + 8),
        flags: buf[at + 12],
    };
    let (eo, il, al) = (
        header.entries_offset as usize,
        header.index_length as usize,
        header.allocated as usize,
    );
    let room = buf.len() - at;
    if eo < NODE_HEADER || !eo.is_multiple_of(8) || il < eo || il > room || al < il || al > room {
        return Err(IndexIssue::Header {
            entries_offset: header.entries_offset,
            index_length: header.index_length,
            allocated: header.allocated,
        });
    }
    let end = at + il;
    let mut pos = at + eo;
    let mut entries = Vec::new();
    let mut issues = Vec::new();
    loop {
        if pos + ENTRY_HEADER > end {
            issues.push(IndexIssue::MissingLast);
            break;
        }
        let length = le16(buf, pos + 8);
        let key_length = le16(buf, pos + 0x0A);
        let flags = le16(buf, pos + 0x0C);
        let len = usize::from(length);
        let sub = flags & entry_flags::SUBNODE != 0;
        let last = flags & entry_flags::LAST != 0;
        let min = ENTRY_HEADER + if sub { 8 } else { 0 };
        if len < min || !len.is_multiple_of(8) || pos + len > end {
            issues.push(IndexIssue::EntryLength {
                offset: pos,
                length,
            });
            break;
        }
        let key_room = len - min;
        let kl = usize::from(key_length);
        if kl > key_room || (!last && kl == 0) {
            issues.push(IndexIssue::EntryKeyLength {
                offset: pos,
                key_length,
            });
            break;
        }
        let key = if last {
            None
        } else {
            let v = &buf[pos + ENTRY_HEADER..pos + ENTRY_HEADER + kl];
            match FileName::parse(v) {
                Ok(file_name) => {
                    let n = usize::from(v[0x40]);
                    let units = v[0x42..0x42 + 2 * n]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect();
                    Some(IndexKey { file_name, units })
                }
                Err(error) => {
                    issues.push(IndexIssue::EntryKey { offset: pos, error });
                    None
                }
            }
        };
        let entry = IndexEntry {
            reference: FileReference::from_u64(le64(buf, pos)),
            flags,
            key,
            subnode: sub.then(|| le64(buf, pos + len - 8)),
            offset: pos,
        };
        // Una entrada con clave no válida no aporta nombre pero su subnodo sigue siendo parte
        // del árbol.
        if last || entry.key.is_some() || entry.subnode.is_some() {
            entries.push(entry);
        }
        if last {
            break;
        }
        pos += len;
    }
    Ok(Node {
        id,
        header,
        entries,
        issues,
    })
}

/// Interpreta el valor de `$INDEX_ROOT`.
pub fn parse_root(value: &[u8]) -> Result<(RootInfo, Node), IndexIssue> {
    if value.len() < 0x20 {
        return Err(IndexIssue::RootTooShort { len: value.len() });
    }
    let info = RootInfo {
        attr_type: le32(value, 0),
        collation: le32(value, 4),
        block_size: le32(value, 8),
        clusters_per_block: value[0x0C],
    };
    if info.attr_type != types::FILE_NAME {
        return Err(IndexIssue::RootType {
            type_code: info.attr_type,
        });
    }
    if info.collation != COLLATION_FILE_NAME {
        return Err(IndexIssue::RootCollation {
            rule: info.collation,
        });
    }
    let node = parse_node(value, 0x10, NodeId::Root)?;
    Ok((info, node))
}

/// `true` si `size` es un tamaño de nodo válido.
pub fn valid_block_size(size: u32) -> bool {
    size.is_power_of_two() && (512..=65536).contains(&size)
}

/// Interpreta un nodo `INDX` (aplica la secuencia de actualización sobre `buf`) que se esperaba en
/// el VCN `vcn`.
pub fn parse_block(buf: &mut [u8], vcn: u64) -> Result<Node, IndexIssue> {
    if buf.len() < USA_STRIDE || !buf.len().is_multiple_of(USA_STRIDE) {
        return Err(IndexIssue::BlockFixup(FixupError::BufferSize(buf.len())));
    }
    let sig = [buf[0], buf[1], buf[2], buf[3]];
    if &sig != b"INDX" {
        return Err(IndexIssue::BlockSignature { found: sig });
    }
    apply_fixups(buf).map_err(IndexIssue::BlockFixup)?;
    let found = le64(buf, 0x10);
    if found != vcn {
        return Err(IndexIssue::BlockVcn {
            expected: vcn,
            found,
        });
    }
    parse_node(buf, INDX_HEADER_AT, NodeId::Vcn(vcn))
}

/// Resultado de buscar un nombre bajando por el árbol B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Find {
    /// Entradas con ese nombre (sin distinguir mayúsculas) en el camino de búsqueda.
    Found(Vec<IndexEntry>),
    /// El árbol está sano en el camino y el nombre no está.
    NotFound,
    /// Algún nodo del camino está dañado o desordenado: hay que recorrer el índice entero.
    Unsure,
}

/// Lector del índice `$I30` de un directorio.
pub struct IndexReader<'v> {
    vol: &'v NtfsVolume,
    record: u64,
    in_use: bool,
    root: Node,
    block_size: u32,
    vcn_unit: u64,
    alloc: Option<StreamLayout>,
    bitmap: Option<Vec<u8>>,
    /// Problemas encontrados al abrir y al leer nodos.
    pub anomalies: Vec<IndexAnomaly>,
}

impl<'v> IndexReader<'v> {
    /// Abre el índice `$I30` del fichero (error [`NtfsError::NotADirectory`] si no tiene, y
    /// [`NtfsError::IndexRoot`] si la raíz no es interpretable).
    pub fn open(vol: &'v NtfsVolume, file: &NtfsFile) -> Result<Self, NtfsError> {
        let number = file.number;
        let roots: Vec<_> = file
            .attributes
            .iter()
            .filter(|a| a.type_code == types::INDEX_ROOT && a.name == I30)
            .collect();
        let Some(root_attr) = roots.first() else {
            return Err(NtfsError::NotADirectory { number });
        };
        let Some(value) = root_attr.resident_value() else {
            return Err(NtfsError::IndexRoot {
                number,
                issue: IndexIssue::RootTooShort { len: 0 },
            });
        };
        let (info, root) =
            parse_root(value).map_err(|issue| NtfsError::IndexRoot { number, issue })?;
        let mut anomalies = Vec::new();
        for i in &root.issues {
            anomalies.push(IndexAnomaly {
                node: NodeId::Root,
                issue: i.clone(),
            });
        }
        let boot = vol.info().boot.index_record_size;
        let block_size = if valid_block_size(info.block_size) {
            if info.block_size != boot {
                anomalies.push(IndexAnomaly {
                    node: NodeId::Root,
                    issue: IndexIssue::BlockSizeMismatch {
                        root: info.block_size,
                        boot,
                    },
                });
            }
            info.block_size
        } else {
            anomalies.push(IndexAnomaly {
                node: NodeId::Root,
                issue: IndexIssue::BlockSize {
                    size: info.block_size,
                },
            });
            boot
        };
        let cluster = vol.geometry().cluster_size;
        let vcn_unit = if block_size >= cluster {
            u64::from(cluster)
        } else {
            512
        };
        let has_alloc = file
            .attributes
            .iter()
            .any(|a| a.type_code == types::INDEX_ALLOCATION && a.name == I30);
        let alloc = if has_alloc {
            match file.stream_layout(types::INDEX_ALLOCATION, I30, vol.geometry()) {
                Ok(l) => Some(l),
                Err(e) => {
                    anomalies.push(IndexAnomaly {
                        node: NodeId::Root,
                        issue: IndexIssue::AllocationUnusable {
                            detail: e.to_string(),
                        },
                    });
                    None
                }
            }
        } else {
            None
        };
        let mut reader = IndexReader {
            vol,
            record: number,
            in_use: file.in_use(),
            root,
            block_size,
            vcn_unit,
            alloc,
            bitmap: None,
            anomalies,
        };
        if reader.alloc.is_some() {
            reader.bitmap = reader.load_bitmap(file);
        }
        Ok(reader)
    }

    fn load_bitmap(&mut self, file: &NtfsFile) -> Option<Vec<u8>> {
        let present = file
            .attributes
            .iter()
            .any(|a| a.type_code == types::BITMAP && a.name == I30);
        let fail = |s: &mut Self, detail: String| {
            s.anomalies.push(IndexAnomaly {
                node: NodeId::Root,
                issue: IndexIssue::BitmapUnusable { detail },
            });
            None
        };
        if !present {
            return fail(self, "el directorio no tiene $BITMAP de $I30".into());
        }
        let stream = match self.vol.attribute_stream(file, types::BITMAP, I30) {
            Ok(s) => s,
            Err(e) => return fail(self, e.to_string()),
        };
        if stream.size() > MAX_BITMAP_BYTES {
            return fail(self, format!("{} bytes", stream.size()));
        }
        match stream.read(0, stream.size() as usize) {
            Ok(c) if c.all_trustworthy() => Some(c.data),
            Ok(c) => {
                let d = c
                    .untrusted()
                    .next()
                    .map(|r| r.to_string())
                    .unwrap_or_default();
                fail(self, d)
            }
            Err(e) => fail(self, e.to_string()),
        }
    }

    /// Registro del directorio.
    pub fn record(&self) -> u64 {
        self.record
    }

    /// Tamaño de los nodos `INDX`.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Nodo raíz.
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Comprueba un puntero a subnodo y lee el nodo.
    pub fn node(&self, vcn: u64) -> Result<Node, IndexIssue> {
        let Some(layout) = &self.alloc else {
            return Err(IndexIssue::AllocationMissing);
        };
        let bs = u64::from(self.block_size);
        let offset = vcn
            .checked_mul(self.vcn_unit)
            .filter(|o| o.is_multiple_of(bs))
            .filter(|o| o.checked_add(bs).is_some_and(|e| e <= layout.data_size))
            .ok_or(IndexIssue::SubnodeOutOfRange { vcn })?;
        if let Some(bm) = &self.bitmap {
            let bit = offset / bs;
            let set = usize::try_from(bit / 8)
                .ok()
                .and_then(|i| bm.get(i))
                .is_some_and(|b| b & (1 << (bit % 8)) != 0);
            if !set {
                return Err(IndexIssue::NodeUnallocated { vcn });
            }
        }
        let stream = self.vol.stream_view(layout, self.record, self.in_use);
        let chunk = stream.read(offset, self.block_size as usize).map_err(|e| {
            IndexIssue::BlockUnreadable {
                detail: e.to_string(),
            }
        })?;
        if let Some(r) = chunk.untrusted().next() {
            return Err(IndexIssue::BlockUnreadable {
                detail: r.to_string(),
            });
        }
        let mut buf = chunk.data;
        if buf.len() != self.block_size as usize {
            return Err(IndexIssue::SubnodeOutOfRange { vcn });
        }
        parse_block(&mut buf, vcn)
    }

    fn note(&mut self, node: NodeId, issue: IndexIssue) {
        self.anomalies.push(IndexAnomaly { node, issue });
    }

    /// `true` si las entradas con clave del nodo respetan la colación.
    fn ordered(node: &Node, up: &Upcase) -> bool {
        let keys: Vec<&[u16]> = node
            .entries
            .iter()
            .filter_map(|e| e.key.as_ref().map(|k| k.units.as_slice()))
            .collect();
        keys.windows(2)
            .all(|w| up.compare(w[0], w[1]) != std::cmp::Ordering::Greater)
    }

    /// Recorre el índice entero (raíz y todos los nodos alcanzables) y devuelve las entradas con
    /// clave y el nodo en que están. Los nodos dañados se informan y se saltan.
    pub fn walk(&mut self, up: &Upcase) -> Vec<(NodeId, IndexEntry)> {
        let mut out = Vec::new();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut stack: Vec<(Node, usize)> = Vec::new();
        let mut nodes = 0usize;
        let root = self.root.clone();
        stack.push((root, 0));
        let mut stopped = false;
        while let Some((node, depth)) = stack.pop() {
            if !Self::ordered(&node, up) {
                self.note(node.id, IndexIssue::Order);
            }
            for e in &node.entries {
                if e.key.is_some() {
                    if out.len() >= MAX_INDEX_ENTRIES {
                        if !stopped {
                            self.note(node.id, IndexIssue::EntryLimit);
                        }
                        stopped = true;
                        break;
                    }
                    out.push((node.id, e.clone()));
                }
            }
            if stopped {
                break;
            }
            // Los hijos se apilan en orden inverso para visitarlos en orden de colación.
            let children: Vec<u64> = node.entries.iter().filter_map(|e| e.subnode).collect();
            for vcn in children.into_iter().rev() {
                if depth + 1 > MAX_INDEX_DEPTH {
                    self.note(node.id, IndexIssue::Depth { vcn });
                    continue;
                }
                if !visited.insert(vcn) {
                    self.note(node.id, IndexIssue::Loop { vcn });
                    continue;
                }
                if nodes >= MAX_INDEX_NODES {
                    self.note(node.id, IndexIssue::NodeLimit);
                    stack.clear();
                    break;
                }
                nodes += 1;
                match self.node(vcn) {
                    Ok(child) => {
                        for i in &child.issues {
                            self.note(child.id, i.clone());
                        }
                        stack.push((child, depth + 1));
                    }
                    Err(issue) => {
                        let at = match issue {
                            IndexIssue::SubnodeOutOfRange { .. }
                            | IndexIssue::NodeUnallocated { .. }
                            | IndexIssue::AllocationMissing => node.id,
                            _ => NodeId::Vcn(vcn),
                        };
                        self.note(at, issue);
                    }
                }
            }
        }
        out
    }

    /// Busca `name` bajando por el árbol B según la colación. Si un nodo del camino está dañado,
    /// desordenado o en un ciclo, devuelve [`Find::Unsure`] (el llamador recorre todo el índice).
    pub fn find(&self, name: &[u16], up: &Upcase) -> Find {
        use std::cmp::Ordering;
        let mut node = self.root.clone();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut found: Vec<IndexEntry> = Vec::new();
        for _ in 0..=MAX_INDEX_DEPTH {
            if !node.issues.is_empty() || !Self::ordered(&node, up) {
                return Find::Unsure;
            }
            let mut next = None;
            for e in &node.entries {
                match &e.key {
                    Some(k) => match up.compare(name, &k.units) {
                        Ordering::Less => {
                            next = Some(e.subnode);
                            break;
                        }
                        Ordering::Equal => {
                            found.push(e.clone());
                            // Puede haber nombres iguales sin distinguir mayúsculas a ambos lados.
                            if e.subnode.is_some() {
                                return Find::Unsure;
                            }
                        }
                        Ordering::Greater => {}
                    },
                    None if e.is_last() => {
                        next = Some(e.subnode);
                        break;
                    }
                    // Entrada sin clave válida en medio del nodo: no se sabe dónde va el nombre.
                    None => return Find::Unsure,
                }
            }
            if !found.is_empty() {
                return Find::Found(found);
            }
            match next.flatten() {
                None => return Find::NotFound,
                Some(vcn) => {
                    if !visited.insert(vcn) {
                        return Find::Unsure;
                    }
                    match self.node(vcn) {
                        Ok(n) => node = n,
                        Err(_) => return Find::Unsure,
                    }
                }
            }
        }
        Find::Unsure
    }

    /// `true` si el índice tiene `$INDEX_ALLOCATION` utilizable.
    pub fn has_allocation(&self) -> bool {
        self.alloc.is_some()
    }
}

/// `true` si el fichero tiene un índice `$I30` (es un directorio con índice de nombres).
pub fn has_i30(file: &NtfsFile) -> bool {
    file.attributes
        .iter()
        .any(|a| a.type_code == types::INDEX_ROOT && a.name == I30)
}
