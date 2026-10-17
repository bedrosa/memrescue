//! Dispositivo con fallos simulados, para probar de forma determinista el lector tolerante, el
//! clonado y la recuperación sin necesitar discos dañados reales.

use crate::{BlockDevice, IoError, Result};
use std::collections::{BTreeSet, HashMap};
use std::io;
use std::sync::Mutex;

/// Envuelve un [`BlockDevice`] y hace fallar las lecturas que tocan determinados sectores.
pub struct FaultyDevice<D: BlockDevice> {
    inner: D,
    /// Sectores que fallan siempre.
    bad: BTreeSet<u64>,
    /// Sectores que fallan las primeras `n` veces que se leen y luego se leen bien.
    flaky: Mutex<HashMap<u64, u32>>,
    reads: Mutex<u64>,
}

impl<D: BlockDevice> FaultyDevice<D> {
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            bad: BTreeSet::new(),
            flaky: Mutex::new(HashMap::new()),
            reads: Mutex::new(0),
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

    /// Número de operaciones de lectura recibidas (para comprobar reintentos).
    pub fn read_calls(&self) -> u64 {
        *self.reads.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl<D: BlockDevice> BlockDevice for FaultyDevice<D> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        *self.reads.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let ss = u64::from(self.sector_size());
        let first = offset / ss;
        let last = (offset + buf.len().max(1) as u64 - 1) / ss;
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
