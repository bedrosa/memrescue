//! Dispositivo con fallos simulados, para probar de forma determinista el lector tolerante, el
//! clonado y la recuperación sin necesitar discos dañados reales.
//!
//! Además de sectores ilegibles (permanentes o intermitentes) puede simular **regiones dispersas**
//! (`Allocation::Sparse` en [`BlockDevice::allocation`]) y lleva la cuenta de cuántas lecturas ha
//! recibido cada sector, para demostrar que el lector no lee lo que no debe (saltos, dispersos).
//!
//! Cada sector que falla puede llevar un **código de error del sistema** distinto
//! ([`io::Error::from_raw_os_error`]), para probar la omisión de errores por código de la política
//! de lectura. Si una lectura toca varios sectores que fallan, el error devuelto lleva el código
//! del primero (el de menor LBA).

use crate::{Allocation, AllocationRun, BlockDevice, IoError, Result};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::ops::Range;
use std::sync::Mutex;

/// Envuelve un [`BlockDevice`] y hace fallar las lecturas que tocan determinados sectores.
pub struct FaultyDevice<D: BlockDevice> {
    inner: D,
    /// Sectores que fallan siempre, con el código de error del sistema que devuelven (si lo hay).
    bad: BTreeMap<u64, Option<i32>>,
    /// Sectores que fallan las primeras `n` veces que se leen y luego se leen bien (con su código).
    flaky: Mutex<BTreeMap<u64, (u32, Option<i32>)>>,
    /// Regiones dispersas simuladas: inicio → fin (exclusivo), sin solapes.
    sparse: BTreeMap<u64, u64>,
    /// Regiones cuya consulta de asignación falla: inicio → fin (exclusivo).
    alloc_errors: BTreeMap<u64, u64>,
    reads: Mutex<u64>,
    /// Número de lecturas que han tocado cada sector (incluidas las fallidas).
    sector_reads: Mutex<HashMap<u64, u64>>,
}

impl<D: BlockDevice> FaultyDevice<D> {
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            bad: BTreeMap::new(),
            flaky: Mutex::new(BTreeMap::new()),
            sparse: BTreeMap::new(),
            alloc_errors: BTreeMap::new(),
            reads: Mutex::new(0),
            sector_reads: Mutex::new(HashMap::new()),
        }
    }

    /// Sectores que fallan siempre con un error sin código del sistema.
    pub fn with_bad(mut self, lbas: impl IntoIterator<Item = u64>) -> Self {
        self.bad.extend(lbas.into_iter().map(|l| (l, None)));
        self
    }

    /// Sectores que fallan siempre con el código de error del sistema `code` (p. ej. 23 o 1117).
    pub fn with_bad_code(mut self, lbas: impl IntoIterator<Item = u64>, code: i32) -> Self {
        self.bad.extend(lbas.into_iter().map(|l| (l, Some(code))));
        self
    }

    /// El sector `lba` falla las primeras `failures` lecturas (error sin código) y luego se lee bien.
    pub fn with_flaky(self, lba: u64, failures: u32) -> Self {
        self.flaky
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lba, (failures, None));
        self
    }

    /// Como [`Self::with_flaky`], pero cada fallo lleva el código de error del sistema `code`.
    pub fn with_flaky_code(self, lba: u64, failures: u32, code: i32) -> Self {
        self.flaky
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lba, (failures, Some(code)));
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

    /// Hace fallar [`BlockDevice::allocation`] (error de lectura sin código del sistema) en toda
    /// consulta que toque alguno de los `count` sectores desde `lba`, como un mapa de bits de
    /// bloque ilegible. Las lecturas de esos sectores no se ven afectadas (para eso, `with_bad`).
    pub fn with_allocation_error(mut self, lba: u64, count: u64) -> Self {
        if count > 0 {
            let end = lba.checked_add(count).expect("región desbordada");
            self.alloc_errors.insert(lba, end);
        }
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
        let end = lba + count;
        if let Some((&s, _)) = self.alloc_errors.iter().find(|(&s, &e)| s < end && e > lba) {
            let ss = u64::from(self.sector_size());
            return Err(IoError::Read {
                offset: s.max(lba) * ss,
                len: self.sector_size() as usize,
                source: io::Error::other("mapa de asignación ilegible (simulado)"),
            });
        }
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
        let fail = |code: Option<i32>| IoError::Read {
            offset,
            len: buf.len(),
            source: match code {
                Some(c) => io::Error::from_raw_os_error(c),
                None => io::Error::other("sector ilegible (simulado)"),
            },
        };
        if let Some((_, &code)) = self.bad.range(first..=last).next() {
            return Err(fail(code));
        }
        let mut flaky = self.flaky.lock().unwrap_or_else(|p| p.into_inner());
        let mut failed: Option<Option<i32>> = None;
        for (_, (n, code)) in flaky.range_mut(first..=last) {
            if *n > 0 {
                *n -= 1;
                failed.get_or_insert(*code);
            }
        }
        drop(flaky);
        if let Some(code) = failed {
            return Err(fail(code));
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
    fn allocation_errors_fail_only_queries_that_touch_them() {
        let d = dev().with_allocation_error(20, 5).with_sparse(30, 5);
        assert!(matches!(
            d.allocation(0, 100),
            Err(IoError::Read { offset: 10240, .. })
        ));
        assert!(matches!(
            d.allocation(22, 1),
            Err(IoError::Read { offset: 11264, .. })
        ));
        assert_eq!(
            d.allocation(25, 10).unwrap(),
            vec![(25, 5, Allocation::Data), (30, 5, Allocation::Sparse)]
        );
        assert_eq!(
            d.allocation(0, 20).unwrap(),
            vec![(0, 20, Allocation::Data)]
        );
        // Las lecturas no se ven afectadas.
        let mut b = vec![0u8; 512 * 5];
        d.read_sectors(20, 5, &mut b).unwrap();
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

    #[test]
    fn failing_sectors_carry_their_os_error_code() {
        let d = dev()
            .with_bad_code([7], 23)
            .with_bad_code([9], 1117)
            .with_bad([11])
            .with_flaky_code(20, 1, 21);
        let mut b = vec![0u8; 512 * 4];
        let code = |lba, n: u64| {
            let mut b = vec![0u8; 512 * n as usize];
            d.read_sectors(lba, n, &mut b).unwrap_err().os_error_code()
        };
        assert_eq!(code(7, 1), Some(23));
        assert_eq!(code(9, 1), Some(1117));
        assert_eq!(code(11, 1), None);
        // Varios sectores malos en la misma lectura: el código es el del primero.
        assert_eq!(code(6, 4), Some(23));
        assert_eq!(code(8, 4), Some(1117));
        // Intermitente con código: falla una vez con 21 y después se lee bien.
        assert_eq!(code(18, 4), Some(21));
        d.read_sectors(18, 4, &mut b).unwrap();
        assert!(matches!(
            d.read_sectors(7, 1, &mut b[..512]),
            Err(IoError::Read {
                offset: 3584,
                len: 512,
                ..
            })
        ));
    }
}
