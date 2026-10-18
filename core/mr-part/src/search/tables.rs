//! Tablas encontradas en posiciones candidatas: MBR/EBR en modo estricto (INV-05 §2.3, regla de
//! búsqueda de tablas perdidas) y cabeceras GPT con CRC-32 correcta (UEFI §5.3.2).
//!
//! Un sector solo cuenta como tabla si supera todas las reglas estrictas: firma `55 AA`, al menos una
//! entrada usada, todas las entradas usadas de tipo conocido, con indicador de arranque válido,
//! tamaño mayor que 0 y dentro del disco, como mucho una extendida y ninguna 0xEE (un MBR protector
//! lo trata el lector GPT). Así un sector de datos con aspecto de MBR difícilmente pasa, y los que
//! caen dentro de volúmenes o particiones válidas ni se prueban (ver `super`).

use crate::gpt::{GptHeader, MIN_ENTRY_SIZE, MIN_HEADER_SIZE};
use crate::mbr::{is_extended, parse_raw, type_name, TYPE_GPT_PROTECTIVE};

/// Entrada de una tabla encontrada, en coordenadas de la imagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoundEntry {
    /// Primer byte en la imagen (puede ser negativo si la tabla declara posiciones anteriores).
    pub start_byte: i64,
    pub length_bytes: u64,
    /// Tipo MBR (0 en GPT).
    pub mbr_type: u8,
    /// `true` si la entrada es una extendida o un enlace de EBR (no describe datos).
    pub extended: bool,
}

/// Clase de tabla encontrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundTableKind {
    /// MBR o EBR que supera el modo estricto.
    Mbr,
    /// Cabecera GPT con firma y CRC-32 correctas.
    Gpt,
}

/// Tabla de particiones encontrada por la búsqueda en una posición que no es la de la tabla
/// principal del origen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundTable {
    pub kind: FoundTableKind,
    /// LBA de la imagen donde está el sector.
    pub lba: u64,
    /// LBA que la cabecera GPT declara para sí misma (`MyLBA`); 0 en MBR.
    pub declared_lba: u64,
    /// Entradas de datos (y extendidas) que declara, en coordenadas de la imagen.
    pub entries: Vec<FoundEntry>,
}

/// Interpreta `sector` (situado en el byte `pos` de la imagen) como MBR o EBR en modo estricto.
/// `ext_base` es el inicio de la extendida si el sector es un EBR (los enlaces son relativos a
/// ella); `ss` el tamaño de sector y `total` los sectores del dispositivo.
pub(crate) fn strict_mbr(
    sector: &[u8],
    pos: u64,
    ext_base: Option<u64>,
    ss: u64,
    total: u64,
) -> Option<Vec<FoundEntry>> {
    let raw = parse_raw(sector)?;
    if !raw.signature {
        return None;
    }
    let used: Vec<_> = raw.entries.iter().filter(|e| e.is_used()).collect();
    if used.is_empty() || used.iter().filter(|e| is_extended(e.kind)).count() > 1 {
        return None;
    }
    let lba = pos / ss;
    let mut out = Vec::new();
    for e in used {
        if !e.boot_valid()
            || type_name(e.kind).is_none()
            || e.kind == TYPE_GPT_PROTECTIVE
            || e.sectors == 0
            || e.start == 0
        {
            return None;
        }
        let base = match (is_extended(e.kind), ext_base) {
            (true, Some(b)) => b,
            _ => lba,
        };
        let start = base.checked_add(u64::from(e.start))?;
        if start.checked_add(u64::from(e.sectors))? > total {
            return None;
        }
        out.push(FoundEntry {
            start_byte: i64::try_from(start.checked_mul(ss)?).ok()?,
            length_bytes: u64::from(e.sectors).checked_mul(ss)?,
            mbr_type: e.kind,
            extended: is_extended(e.kind),
        });
    }
    // Entradas de datos que se solapan: no es una tabla coherente.
    let mut data: Vec<_> = out.iter().filter(|e| !e.extended).collect();
    data.sort_by_key(|e| e.start_byte);
    if data
        .windows(2)
        .any(|w| w[0].start_byte + w[0].length_bytes as i64 > w[1].start_byte)
    {
        return None;
    }
    Some(out)
}

/// Cabecera GPT con firma, tamaño y CRC-32 correctos, o `None`.
pub(crate) fn gpt_header(sector: &[u8]) -> Option<GptHeader> {
    let h = GptHeader::parse(sector)?;
    let size = h.header_size as usize;
    if h.header_size < MIN_HEADER_SIZE || size > sector.len() {
        return None;
    }
    let mut copy = sector[..size].to_vec();
    copy[16..20].fill(0);
    if crc32fast::hash(&copy) != h.header_crc32
        || h.entry_size < MIN_ENTRY_SIZE
        || !h.entry_size.is_multiple_of(8)
    {
        return None;
    }
    Some(h)
}

/// Entradas GPT usadas del array `data` (ya comprobado con su CRC-32) en LBA de la cabecera:
/// `(primera LBA, última LBA)`.
pub(crate) fn gpt_entries(h: &GptHeader, data: &[u8]) -> Vec<(u64, u64)> {
    let size = h.entry_size as usize;
    data.chunks_exact(size)
        .take(h.entry_count as usize)
        .filter(|e| e[..16].iter().any(|&b| b != 0))
        .filter_map(|e| {
            let first = u64::from_le_bytes(e[32..40].try_into().ok()?);
            let last = u64::from_le_bytes(e[40..48].try_into().ok()?);
            (last >= first).then_some((first, last))
        })
        .collect()
}
