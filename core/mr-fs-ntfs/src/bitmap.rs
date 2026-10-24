//! Mapa de asignación de clústeres del volumen (`$Bitmap`, registro 6), #23.
//!
//! El `$DATA` sin nombre de `$Bitmap` tiene un bit por clúster del volumen (bit `lcn % 8` del byte
//! `lcn / 8`, el menos significativo primero): 1 = asignado, 0 = libre. Se lee a través de
//! [`DataStream`] (y por tanto del mapa de validez, ADR-019): un byte del mapa que cae en un tramo
//! no fiable (sector ilegible, sin mapa...) deja sus 8 clústeres en estado [`ClusterAlloc::Unknown`],
//! igual que los clústeres que el mapa no cubre (mapa más corto que el volumen) o todos si
//! `$Bitmap` no se puede abrir. **Nunca** se supone libre un clúster cuya asignación no se ha
//! leído.
//!
//! **Memoria acotada**: el mapa no se carga entero (un volumen de 16 TiB con clústeres de 4 KiB
//! tiene un mapa de 512 MiB); se leen páginas de [`PAGE_BYTES`] bajo demanda y se conservan como
//! mucho [`MAX_PAGES`] (sustitución de la más antigua).

use std::fmt;

use crate::attr::types;
use crate::stream::DataStream;
use crate::{NtfsError, NtfsVolume};

/// Registro de `$Bitmap`.
pub const BITMAP_RECORD: u64 = 6;
/// Bytes del mapa por página leída (512 Ki clústeres).
pub const PAGE_BYTES: u64 = 64 * 1024;
/// Páginas conservadas en memoria (4 MiB).
pub const MAX_PAGES: usize = 64;

/// Asignación de un clúster según `$Bitmap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClusterAlloc {
    Free,
    Allocated,
    /// No se pudo leer su bit (tramo no fiable, mapa corto o `$Bitmap` inutilizable).
    Unknown,
}

impl ClusterAlloc {
    pub fn code(self) -> &'static str {
        match self {
            ClusterAlloc::Free => "free",
            ClusterAlloc::Allocated => "allocated",
            ClusterAlloc::Unknown => "unknown",
        }
    }
}

/// Problema del propio mapa (afecta a la fiabilidad de todas las consultas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitmapIssue {
    /// `$Bitmap` no se puede abrir: toda asignación es desconocida.
    Unusable { reason: String },
    /// El registro 6 no está en uso.
    NotInUse,
    /// El mapa cubre menos clústeres que el volumen: los demás son desconocidos.
    Short { covered: u64, total_clusters: u64 },
}

impl BitmapIssue {
    pub fn code(&self) -> &'static str {
        match self {
            BitmapIssue::Unusable { .. } => "bitmap_unusable",
            BitmapIssue::NotInUse => "bitmap_not_in_use",
            BitmapIssue::Short { .. } => "bitmap_short",
        }
    }
}

impl fmt::Display for BitmapIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitmapIssue::Unusable { reason } => write!(
                f,
                "$Bitmap no se puede leer ({reason}): la asignación de todos los clústeres es desconocida"
            ),
            BitmapIssue::NotInUse => write!(
                f,
                "el registro de $Bitmap no está en uso: la asignación de todos los clústeres es desconocida"
            ),
            BitmapIssue::Short {
                covered,
                total_clusters,
            } => write!(
                f,
                "$Bitmap cubre {covered} de {total_clusters} clústeres: el resto es desconocido"
            ),
        }
    }
}

/// Página del mapa: bytes y, para cada byte, si es fiable.
struct Page {
    index: u64,
    bytes: Vec<u8>,
    trusted: Vec<bool>,
}

/// Tramos `(lcn, clústeres, asignación)` de `[lcn, lcn + count)` a partir de los bytes del mapa
/// `bytes` que empiezan en el clúster `first` (múltiplo de 8). `trusted[i]` dice si el byte `i` es
/// fiable; los clústeres fuera de `bytes` o de bytes no fiables son [`ClusterAlloc::Unknown`].
/// Tramos contiguos del mismo estado unidos. Función pura (pruebas y fuzzing).
pub fn alloc_runs(
    bytes: &[u8],
    trusted: &[bool],
    first: u64,
    lcn: u64,
    count: u64,
) -> Vec<(u64, u64, ClusterAlloc)> {
    let mut out: Vec<(u64, u64, ClusterAlloc)> = Vec::new();
    let mut push = |l: u64, n: u64, s: ClusterAlloc| {
        if n == 0 {
            return;
        }
        match out.last_mut() {
            Some((pl, pn, ps)) if *ps == s && *pl + *pn == l => *pn += n,
            _ => out.push((l, n, s)),
        }
    };
    let end = lcn.saturating_add(count);
    let mut c = lcn;
    while c < end {
        let Some(rel) = c.checked_sub(first) else {
            // Antes de estos bytes: desconocido hasta `first`.
            let n = first.min(end) - c;
            push(c, n, ClusterAlloc::Unknown);
            c += n;
            continue;
        };
        let byte = (rel / 8) as usize;
        if byte >= bytes.len() {
            push(c, end - c, ClusterAlloc::Unknown);
            break;
        }
        let bit = rel % 8;
        // Hasta el final de este byte o del tramo pedido.
        let n = (8 - bit).min(end - c);
        if !trusted.get(byte).copied().unwrap_or(false) {
            push(c, n, ClusterAlloc::Unknown);
        } else {
            let b = bytes[byte];
            if (bit == 0 && n == 8) && (b == 0 || b == 0xFF) {
                let s = if b == 0 {
                    ClusterAlloc::Free
                } else {
                    ClusterAlloc::Allocated
                };
                push(c, 8, s);
            } else {
                for k in 0..n {
                    let s = if b >> (bit + k) & 1 == 1 {
                        ClusterAlloc::Allocated
                    } else {
                        ClusterAlloc::Free
                    };
                    push(c + k, 1, s);
                }
            }
        }
        c += n;
    }
    out
}

/// Mapa de asignación del volumen con lectura por páginas (ver el módulo).
pub struct ClusterBitmap<'v> {
    stream: Option<DataStream<'v>>,
    total_clusters: u64,
    /// Clústeres que cubre el mapa (8 por byte del tamaño de datos), acotado al volumen.
    covered: u64,
    issue: Option<BitmapIssue>,
    pages: Vec<Page>,
    reads: u64,
}

impl fmt::Debug for ClusterBitmap<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterBitmap")
            .field("total_clusters", &self.total_clusters)
            .field("covered", &self.covered)
            .field("issue", &self.issue)
            .finish_non_exhaustive()
    }
}

impl<'v> ClusterBitmap<'v> {
    /// Abre `$Bitmap` del volumen. Nunca falla: si no se puede usar, todo es desconocido y
    /// [`ClusterBitmap::issue`] dice por qué.
    pub fn open(vol: &'v NtfsVolume) -> Self {
        let total = vol.geometry().total_clusters;
        let unusable = |issue: BitmapIssue| ClusterBitmap {
            stream: None,
            total_clusters: total,
            covered: 0,
            issue: Some(issue),
            pages: Vec::new(),
            reads: 0,
        };
        let file = match vol.file(BITMAP_RECORD) {
            Ok(f) => f,
            Err(e) => {
                return unusable(BitmapIssue::Unusable {
                    reason: e.to_string(),
                })
            }
        };
        if !file.in_use() {
            return unusable(BitmapIssue::NotInUse);
        }
        let stream = match vol.stream_of(&file, "") {
            Ok(s) if s.layout().type_code == types::DATA => s,
            Ok(_) => {
                return unusable(BitmapIssue::Unusable {
                    reason: "sin $DATA".into(),
                })
            }
            Err(e) => {
                return unusable(BitmapIssue::Unusable {
                    reason: e.to_string(),
                })
            }
        };
        let covered = stream.size().saturating_mul(8).min(total);
        let issue = (covered < total).then_some(BitmapIssue::Short {
            covered,
            total_clusters: total,
        });
        ClusterBitmap {
            stream: Some(stream),
            total_clusters: total,
            covered,
            issue,
            pages: Vec::new(),
            reads: 0,
        }
    }

    pub fn issue(&self) -> Option<&BitmapIssue> {
        self.issue.as_ref()
    }

    pub fn total_clusters(&self) -> u64 {
        self.total_clusters
    }

    /// Páginas leídas del disco hasta ahora (diagnóstico y pruebas de la caché).
    pub fn page_reads(&self) -> u64 {
        self.reads
    }

    fn page(&mut self, index: u64) -> Result<usize, NtfsError> {
        if let Some(i) = self.pages.iter().position(|p| p.index == index) {
            return Ok(i);
        }
        let s = self.stream.as_ref().expect("solo con flujo");
        let c = s.read(index * PAGE_BYTES, PAGE_BYTES as usize)?;
        let mut trusted = vec![false; c.data.len()];
        for r in &c.ranges {
            if r.state.is_trustworthy() {
                let a = (r.offset - c.offset) as usize;
                let b = a + r.length as usize;
                trusted[a..b].fill(true);
            }
        }
        self.reads += 1;
        if self.pages.len() >= MAX_PAGES {
            self.pages.remove(0);
        }
        self.pages.push(Page {
            index,
            bytes: c.data,
            trusted,
        });
        Ok(self.pages.len() - 1)
    }

    /// Asignación de `[lcn, lcn + count)` en tramos `(lcn, clústeres, estado)` contiguos.
    pub fn runs(
        &mut self,
        lcn: u64,
        count: u64,
    ) -> Result<Vec<(u64, u64, ClusterAlloc)>, NtfsError> {
        let end = lcn.saturating_add(count);
        let mut out: Vec<(u64, u64, ClusterAlloc)> = Vec::new();
        let push = |out: &mut Vec<(u64, u64, ClusterAlloc)>, l: u64, n: u64, s| match out.last_mut()
        {
            Some((pl, pn, ps)) if *ps == s && *pl + *pn == l => *pn += n,
            _ => out.push((l, n, s)),
        };
        let per_page = PAGE_BYTES * 8;
        let mut c = lcn;
        while c < end {
            if self.stream.is_none() || c >= self.covered {
                push(&mut out, c, end - c, ClusterAlloc::Unknown);
                break;
            }
            let index = c / per_page;
            let first = index * per_page;
            let until = end.min(first + per_page).min(self.covered);
            let i = self.page(index)?;
            let p = &self.pages[i];
            for (l, n, s) in alloc_runs(&p.bytes, &p.trusted, first, c, until - c) {
                push(&mut out, l, n, s);
            }
            c = until;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::xorshift;

    #[test]
    fn bits_are_lsb_first_and_runs_are_merged() {
        let bytes = [0b0000_0101u8, 0xFF, 0x00, 0x80];
        let t = [true; 4];
        assert_eq!(
            alloc_runs(&bytes, &t, 0, 0, 32),
            vec![
                (0, 1, ClusterAlloc::Allocated),
                (1, 1, ClusterAlloc::Free),
                (2, 1, ClusterAlloc::Allocated),
                (3, 5, ClusterAlloc::Free),
                (8, 8, ClusterAlloc::Allocated),
                (16, 15, ClusterAlloc::Free),
                (31, 1, ClusterAlloc::Allocated),
            ]
        );
        // Subtramo que empieza y termina dentro de un byte.
        assert_eq!(
            alloc_runs(&bytes, &t, 0, 2, 9),
            vec![
                (2, 1, ClusterAlloc::Allocated),
                (3, 5, ClusterAlloc::Free),
                (8, 3, ClusterAlloc::Allocated),
            ]
        );
    }

    #[test]
    fn bounds_and_untrusted_bytes_are_unknown() {
        let bytes = [0u8, 0xFF];
        // Byte 1 no fiable.
        let t = [true, false];
        assert_eq!(
            alloc_runs(&bytes, &t, 8, 0, 30),
            vec![
                (0, 8, ClusterAlloc::Unknown),
                (8, 8, ClusterAlloc::Free),
                (16, 14, ClusterAlloc::Unknown),
            ]
        );
        assert!(alloc_runs(&bytes, &t, 0, 5, 0).is_empty());
        assert_eq!(
            alloc_runs(&[], &[], 0, u64::MAX - 3, 10),
            vec![(u64::MAX - 3, 3, ClusterAlloc::Unknown)]
        );
    }

    /// Fuzzing determinista: la salida cubre exactamente lo pedido, en orden y sin tramos vacíos,
    /// y cada clúster coincide con su bit.
    #[test]
    fn fuzz_alloc_runs_cover_exactly_the_request() {
        let mut r = xorshift(23);
        for _ in 0..5000 {
            let n = (r() % 40) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| r() as u8).collect();
            let trusted: Vec<bool> = (0..n).map(|_| !r().is_multiple_of(5)).collect();
            let first = (r() % 4) * 8;
            let lcn = r() % 400;
            let count = r() % 400;
            let out = alloc_runs(&bytes, &trusted, first, lcn, count);
            let mut at = lcn;
            for &(l, len, s) in &out {
                assert_eq!(l, at);
                assert!(len > 0);
                for c in l..l + len {
                    let want = match c.checked_sub(first).map(|x| ((x / 8) as usize, x % 8)) {
                        Some((b, bit)) if b < n && trusted[b] => {
                            if bytes[b] >> bit & 1 == 1 {
                                ClusterAlloc::Allocated
                            } else {
                                ClusterAlloc::Free
                            }
                        }
                        _ => ClusterAlloc::Unknown,
                    };
                    assert_eq!(s, want, "clúster {c}");
                }
                at += len;
            }
            assert_eq!(at, lcn + count);
            assert!(out.windows(2).all(|w| w[0].2 != w[1].2));
        }
    }
}
