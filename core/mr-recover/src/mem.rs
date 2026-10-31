//! Contenido en memoria para árboles virtuales sintéticos (pruebas de nombres hostiles,
//! conflictos, rutas largas, escala y estados de tramo sin construir un volumen NTFS).
//!
//! Cada nodo se identifica por su registro ([`mr_vfs::FsRef::record`]); sus flujos son bytes con
//! tramos opcionales de otro estado. Los tramos con relleno (`unreadable`...) se entregan con el
//! patrón, como haría el lector real; los `possibly_overwritten`, con sus bytes. Un registro sin
//! contenido registrado se lee como flujo principal de `size` bytes generados de forma
//! determinista ([`synthetic_byte`]), sin guardarlos (escala).

use std::collections::HashMap;

use mr_fs_ntfs::StreamChunk;
use mr_vfs::NodeInfo;

use crate::source::{
    CancelFlag, ContentSource, RangeState, SourceError, StreamDesc, StreamRange, StreamReader,
};

/// Flujo en memoria.
#[derive(Debug, Clone, Default)]
pub struct MemStream {
    pub data: Vec<u8>,
    /// Tramos `(desplazamiento, longitud, estado, LBA)` distintos de `data` (el resto es `data`).
    pub ranges: Vec<(u64, u64, RangeState, Option<u64>)>,
}

/// Contenido por registro.
#[derive(Debug, Default)]
pub struct MemContent {
    files: HashMap<u64, Vec<(String, MemStream)>>,
    errors: HashMap<u64, SourceError>,
    fill: [u8; 4],
}

/// Byte `i` del contenido generado del registro `record`.
pub fn synthetic_byte(record: u64, i: u64) -> u8 {
    (record.wrapping_mul(31).wrapping_add(i.wrapping_mul(7)) % 251) as u8
}

impl MemContent {
    pub fn new(fill: [u8; 4]) -> Self {
        MemContent {
            fill,
            ..MemContent::default()
        }
    }

    /// Flujo `name` del registro `record`.
    pub fn put(&mut self, record: u64, name: &str, s: MemStream) {
        let v = self.files.entry(record).or_default();
        v.retain(|(n, _)| n != name);
        v.push((name.to_string(), s));
        v.sort_by(|a, b| a.0.cmp(&b.0));
    }

    /// El registro falla al abrirse.
    pub fn fail(&mut self, record: u64, e: SourceError) {
        self.errors.insert(record, e);
    }
}

struct Reader<'a> {
    data: Option<&'a MemStream>,
    record: u64,
    size: u64,
    fill: [u8; 4],
}

impl StreamReader for Reader<'_> {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: usize) -> Result<StreamChunk, SourceError> {
        let end = offset.saturating_add(len as u64).min(self.size);
        let mut data = Vec::with_capacity(end.saturating_sub(offset) as usize);
        let mut ranges: Vec<StreamRange> = Vec::new();
        let mut pos = offset;
        while pos < end {
            // Tramo especial que contiene `pos`, o el siguiente.
            let special = self.data.and_then(|d| {
                d.ranges
                    .iter()
                    .filter(|r| r.0 + r.1 > pos)
                    .min_by_key(|r| r.0)
                    .copied()
            });
            let (state, until, lba) = match special {
                Some((o, l, s, lba)) if o <= pos => {
                    (s, (o + l).min(end), lba.map(|b| b + (pos - o) / 512))
                }
                Some((o, _, _, _)) => (RangeState::Data, o.min(end), None),
                None => (RangeState::Data, end, None),
            };
            for i in pos..until {
                let b = if state.is_filled() {
                    self.fill[(i % 4) as usize]
                } else if state == RangeState::Sparse || state == RangeState::Uninitialized {
                    0
                } else {
                    match self.data {
                        Some(d) => d.data[i as usize],
                        None => synthetic_byte(self.record, i),
                    }
                };
                data.push(b);
            }
            let lbas = if state == RangeState::Unreadable {
                lba.map(|l| (l, (until - pos).div_ceil(512)))
            } else {
                None
            };
            match ranges.last_mut() {
                Some(r) if r.state == state && lbas.is_none() && r.lbas.is_none() => {
                    r.length += until - pos
                }
                _ => ranges.push(StreamRange {
                    offset: pos,
                    length: until - pos,
                    state,
                    lbas,
                }),
            }
            pos = until;
        }
        Ok(StreamChunk {
            offset,
            data,
            ranges,
        })
    }
}

impl ContentSource for MemContent {
    fn streams(&self, node: &NodeInfo) -> Result<Vec<StreamDesc>, SourceError> {
        let rec = node.origin.map_or(u64::MAX, |o| o.record);
        if let Some(e) = self.errors.get(&rec) {
            return Err(e.clone());
        }
        match self.files.get(&rec) {
            Some(v) => Ok(v
                .iter()
                .map(|(n, s)| StreamDesc {
                    name: n.clone(),
                    size: s.data.len() as u64,
                })
                .collect()),
            None if node.kind == mr_vfs::NodeKind::File => Ok(vec![StreamDesc {
                name: String::new(),
                size: node.size,
            }]),
            None => Ok(Vec::new()),
        }
    }

    fn open<'a>(
        &'a self,
        node: &NodeInfo,
        stream: &str,
        _cancel: &dyn CancelFlag,
    ) -> Result<Box<dyn StreamReader + 'a>, SourceError> {
        let rec = node.origin.map_or(u64::MAX, |o| o.record);
        if let Some(e) = self.errors.get(&rec) {
            return Err(e.clone());
        }
        let data = self
            .files
            .get(&rec)
            .and_then(|v| v.iter().find(|(n, _)| n == stream).map(|(_, s)| s));
        let size = match data {
            Some(d) => d.data.len() as u64,
            None if stream.is_empty() => node.size,
            None => {
                return Err(SourceError::Unavailable {
                    code: "stream_not_found",
                    message: format!("no existe el flujo {stream}"),
                })
            }
        };
        Ok(Box::new(Reader {
            data,
            record: rec,
            size,
            fill: self.fill,
        }))
    }

    fn fs_name(&self) -> &'static str {
        "memoria"
    }
}
