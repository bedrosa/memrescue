//! Dispositivo con fallos simulados, para probar de forma determinista el lector tolerante, el
//! clonado y la recuperación sin necesitar discos dañados reales.
//!
//! Además de sectores ilegibles (permanentes o intermitentes) puede simular **regiones dispersas**
//! (`Allocation::Sparse` en [`BlockDevice::allocation`]) y lleva la cuenta de cuántas lecturas ha
//! recibido cada sector, para demostrar que el lector no lee lo que no debe (saltos, dispersos).

use crate::{Allocation, AllocationRun, BlockDevice, IoError, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::sync::Mutex;

/// Envuelve un [`BlockDevice`] y hace fallar las lecturas que tocan determinados sectores.
pub struct FaultyDevice<D: BlockDevice> {
    inner: D,
    /// Sectores que fallan siempre.
    bad: BTreeSet<u64>,
    /// Sectores que fallan las primeras `n` veces que se leen y luego se leen bien.
    flaky: Mutex<HashMap<u64, u32>>,
    /// Regiones dispersas simuladas: inicio → fin (exclusivo), sin solapes.
    sparse: BTreeMap<u64, u64>,
    reads: Mutex<u64>,
    /// Número de lecturas que han tocado cada sector (incluidas las fallidas).
    sector_reads: Mutex<HashMap<u64, u64>>,
}

impl<D: BlockDevice> FaultyDevice<D> {
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            bad: BTreeSet::new(),
            flaky: Mutex::new(HashMap::new()),
            sparse: BTreeMap::new(),
            reads: Mutex::new(0),
            sector_reads: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_bad(mut self, lbas: impl IntoIterator<Item = u64>) -> Self {
        self.bad.extend(lbas);
        self
    }

    pub fn with_flaky(self, lba: u64, failures: u32) -> Self {
        self.flaky
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lba, failures);
        self
    }

    /// Declara `count` sectores desde `lba` como no asignados (dispersos) en
    /// [`BlockDevice::allocation`]. Las lecturas directas siguen devolviendo los datos del
    /// dispositivo interno (así se detecta si alguien los lee en lugar de entregar ceros).
    ///
    /// # Panics
    /// Si la región se solapa con otra ya declarada o desborda `u64` (error del caso de prueba).
    pub fn with_sparse(mut self, lba: u64, count: u64) -> Self {
        if count == 0 {
            return self;
        }
        let end = lba.checked_add(count).expect("región dispersa desbordada");
        let overlaps = self
            .sparse
            .range(..end)
            .next_back()
            .is_some_and(|(_, &e)| e > lba);
        assert!(!overlaps, "regiones dispersas solapadas en LBA {lba}");
        self.sparse.insert(lba, end);
        self
    }

    /// Número de operaciones de lectura recibidas (para comprobar reintentos).
    pub fn read_calls(&self) -> u64 {
        *self.reads.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Número de lecturas (correctas o fallidas) que han tocado el sector `lba`.
    pub fn reads_of(&self, lba: u64) -> u64 {
        self.sector_reads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&lba)
            .copied()
            .unwrap_or(0)
    }

    /// Sectores del rango que han recibido al menos una lectura, en orden.
    pub fn sectors_touched(&self, range: Range<u64>) -> Vec<u64> {
        let m = self.sector_reads.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<u64> = m.keys().copied().filter(|l| range.contains(l)).collect();
        v.sort_unstable();
        v
    }

    /// Fin de la región dispersa que contiene `lba`, si la hay.
    fn sparse_end_at(&self, lba: u64) -> Option<u64> {
        self.sparse
            .range(..=lba)
            .next_back()
            .and_then(|(_, &e)| (e > lba).then_some(e))
    }
}

/// Añade un tramo fusionándolo con el anterior si es contiguo y del mismo tipo.
fn push_run(out: &mut Vec<AllocationRun>, lba: u64, count: u64, kind: Allocation) {
    if let Some(last) = out.last_mut() {
        if last.2 == kind && last.0 + last.1 == lba {
            last.1 += count;
            return;
        }
    }
    out.push((lba, count, kind));
}

impl<D: BlockDevice> BlockDevice for FaultyDevice<D> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let base = self.inner.allocation(lba, count)?;
        if self.sparse.is_empty() {
            return Ok(base);
        }
        let mut out = Vec::new();
        for (a, c, kind) in base {
            let end = a + c;
            let mut p = a;
            while p < end {
                if let Some(e) = self.sparse_end_at(p) {
                    let e = e.min(end);
                    push_run(&mut out, p, e - p, Allocation::Sparse);
                    p = e;
                } else {
                    let e = self
                        .sparse
                        .range(p + 1..)
                        .next()
                        .map_or(end, |(&s, _)| s.min(end));
                    push_run(&mut out, p, e - p, kind);
                    p = e;
                }
            }
        }
        Ok(out)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        *self.reads.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let ss = u64::from(self.sector_size());
        let first = offset / ss;
        let last = offset.saturating_add(buf.len().max(1) as u64 - 1) / ss;
        {
            let mut m = self.sector_reads.lock().unwrap_or_else(|p| p.into_inner());
            for lba in first..=last {
                *m.entry(lba).or_insert(0) += 1;
            }
        }
        let fail = |offset| IoError::Read {
            offset,
            len: buf.len(),
            source: io::Error::other("sector ilegible (simulado)"),
        };
        if self.bad.range(first..=last).next().is_some() {
            return Err(fail(offset));
        }
        let mut flaky = self.flaky.lock().unwrap_or_else(|p| p.into_inner());
        let mut failed = false;
        for lba in first..=last {
            if let Some(n) = flaky.get_mut(&lba) {
                if *n > 0 {
                    *n -= 1;
                    failed = true;
                }
            }
        }
        drop(flaky);
        if failed {
            return Err(fail(offset));
        }
        self.inner.read_at(offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemDevice;

    fn dev() -> FaultyDevice<MemDevice> {
        FaultyDevice::new(MemDevice::new(vec![1u8; 512 * 100], 512).unwrap())
    }

    #[test]
    fn allocation_overlays_sparse_regions() {
        let d = dev()
            .with_sparse(10, 5)
            .with_sparse(15, 3)
            .with_sparse(40, 10);
        assert_eq!(
            d.allocation(0, 100).unwrap(),
            vec![
                (0, 10, Allocation::Data),
                (10, 8, Allocation::Sparse),
                (18, 22, Allocation::Data),
                (40, 10, Allocation::Sparse),
                (50, 50, Allocation::Data),
            ]
        );
        assert_eq!(
            d.allocation(12, 30).unwrap(),
            vec![
                (12, 6, Allocation::Sparse),
                (18, 22, Allocation::Data),
                (40, 2, Allocation::Sparse),
            ]
        );
        assert_eq!(
            d.allocation(45, 1).unwrap(),
            vec![(45, 1, Allocation::Sparse)]
        );
        assert!(d.allocation(90, 20).is_err());
        assert_eq!(
            dev().allocation(0, 100).unwrap(),
            vec![(0, 100, Allocation::Data)]
        );
    }

    #[test]
    #[should_panic(expected = "solapadas")]
    fn overlapping_sparse_regions_are_rejected() {
        let _ = dev().with_sparse(10, 5).with_sparse(12, 5);
    }

    #[test]
    fn counts_reads_per_sector() {
        let d = dev().with_bad([7]);
        let mut b = vec![0u8; 512 * 4];
        d.read_sectors(2, 4, &mut b).unwrap();
        assert!(d.read_sectors(5, 3, &mut b[..1536]).is_err());
        assert_eq!(
            (d.reads_of(1), d.reads_of(2), d.reads_of(5), d.reads_of(7)),
            (0, 1, 2, 1)
        );
        assert_eq!(d.sectors_touched(0..100), vec![2, 3, 4, 5, 6, 7]);
        assert_eq!(d.read_calls(), 2);
    }
}
