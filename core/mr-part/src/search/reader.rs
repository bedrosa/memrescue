//! Lector de la búsqueda: presupuesto de lecturas, caché de sectores, cancelación y mapa de validez.
//!
//! - Toda lectura física pasa por [`read_trusted`]: un sector `Unreadable` o `Skipped` se guarda en
//!   la caché como **no fiable** y nunca se entrega a un reconocedor (ADR-019, INV-05b §8.9).
//! - Cada lectura física cuenta para el presupuesto (`max_reads`, `max_bytes`); los aciertos de la
//!   caché no. Antes de cada lectura física se comprueba la cancelación.
//! - Reservas por nivel (INV-05b §8.5): los niveles P2 a P4 no pueden gastar más del 40 % de
//!   `max_reads` cada uno ni la reserva del 25 % que queda para P0 y P1.
//! - La caché es FIFO y determinista (mismas lecturas en el mismo orden en cada ejecución).

use super::{SearchObserver, SearchProgress, Tier};
use crate::sector::read_trusted;
use mr_io::BlockDevice;
use std::collections::{BTreeMap, VecDeque};

/// Motivo por el que una lectura no se hizo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Presupuesto global agotado (`max_reads` o `max_bytes`).
    Budget,
    /// El nivel actual agotó su cupo o invadiría la reserva de P0 y P1.
    TierCap,
    /// El usuario canceló.
    Cancelled,
}

/// Resultado de pedir un tramo de bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bytes {
    /// Todos los sectores del tramo son fiables (leídos, recuperados o dispersos).
    Data(Vec<u8>),
    /// Algún sector del tramo no es fiable: LBA del primero. Su contenido nunca se entrega.
    Unreadable(u64),
    /// El tramo empieza antes del inicio o termina después del final de la imagen.
    Outside,
}

/// Contadores de la búsqueda (lecturas físicas, no aciertos de caché).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Lecturas físicas hechas.
    pub reads: u64,
    /// Bytes leídos físicamente.
    pub bytes: u64,
    /// Peticiones servidas íntegramente desde la caché.
    pub cache_hits: u64,
    /// Sectores no fiables distintos encontrados (ilegibles o saltados).
    pub unverifiable_sectors: u64,
    /// Lecturas por nivel (P0 a P4).
    pub reads_by_tier: [u64; 5],
}

pub(crate) struct Reader<'a> {
    dev: &'a dyn BlockDevice,
    ss: u64,
    sectors: u64,
    size: u64,
    cache: BTreeMap<u64, Option<Box<[u8]>>>,
    order: VecDeque<u64>,
    cap_sectors: usize,
    max_reads: u64,
    max_bytes: u64,
    tier_cap: u64,
    reserve: u64,
    pub(crate) tier: Tier,
    pub(crate) stats: ReadStats,
    untrusted: BTreeMap<u64, ()>,
    observer: &'a dyn SearchObserver,
    pub(crate) volumes: u64,
    pub(crate) candidate: i64,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(
        dev: &'a dyn BlockDevice,
        max_reads: u64,
        max_bytes: u64,
        cache_bytes: u64,
        observer: &'a dyn SearchObserver,
    ) -> Self {
        let ss = u64::from(dev.sector_size());
        let sectors = dev.sector_count();
        let cap_sectors = usize::try_from((cache_bytes / ss).max(256)).unwrap_or(256);
        Self {
            dev,
            ss,
            sectors,
            size: sectors.saturating_mul(ss),
            cache: BTreeMap::new(),
            order: VecDeque::new(),
            cap_sectors,
            max_reads,
            max_bytes,
            tier_cap: max_reads.saturating_mul(2).div_ceil(5),
            reserve: max_reads / 4,
            tier: Tier::Declared,
            stats: ReadStats::default(),
            untrusted: BTreeMap::new(),
            observer,
            volumes: 0,
            candidate: 0,
        }
    }

    /// Tamaño de sector lógico del dispositivo.
    pub(crate) fn sector_size(&self) -> u64 {
        self.ss
    }

    /// Bytes de sectores completos del dispositivo.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn sectors(&self) -> u64 {
        self.sectors
    }

    /// `true` si `off` y `len` caen dentro de la imagen.
    pub(crate) fn inside(&self, off: i128, len: u64) -> bool {
        off >= 0 && off + i128::from(len) <= i128::from(self.size)
    }

    /// Bytes `[off, off + len)` con el mapa de validez aplicado. Lee físicamente solo los
    /// sectores que no están en la caché, en tramos contiguos (una lectura por tramo).
    pub(crate) fn get(&mut self, off: i128, len: usize) -> Result<Bytes, Stop> {
        if len == 0 || !self.inside(off, len as u64) {
            return Ok(Bytes::Outside);
        }
        let off = off as u64; // dentro de [0, size)
        let first = off / self.ss;
        let last = (off + len as u64 - 1) / self.ss;
        let count = last - first + 1;
        // Expulsión anticipada: lo que se va a usar ahora no puede salir de la caché a mitad.
        let keep = self
            .cap_sectors
            .saturating_sub(usize::try_from(count).unwrap_or(usize::MAX));
        while self.order.len() > keep {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
        let mut run: Option<(u64, u64)> = None;
        let mut any_missing = false;
        for lba in first..=last {
            if self.cache.contains_key(&lba) {
                if let Some((s, c)) = run.take() {
                    self.physical(s, c)?;
                }
                continue;
            }
            any_missing = true;
            run = match run {
                Some((s, c)) if c < crate::sector::MAX_TRUSTED_SECTORS => Some((s, c + 1)),
                Some((s, c)) => {
                    self.physical(s, c)?;
                    Some((lba, 1))
                }
                None => Some((lba, 1)),
            };
        }
        if let Some((s, c)) = run {
            self.physical(s, c)?;
        }
        if !any_missing {
            self.stats.cache_hits += 1;
        }
        let mut out = Vec::with_capacity(count as usize * self.ss as usize);
        for lba in first..=last {
            match self.cache.get(&lba) {
                Some(Some(d)) => out.extend_from_slice(d),
                _ => return Ok(Bytes::Unreadable(lba)),
            }
        }
        let skip = (off - first * self.ss) as usize;
        Ok(Bytes::Data(out[skip..skip + len].to_vec()))
    }

    /// Una lectura física de `count` sectores desde `lba`, con todas las comprobaciones previas.
    fn physical(&mut self, lba: u64, count: u64) -> Result<(), Stop> {
        if self.observer.cancelled() {
            return Err(Stop::Cancelled);
        }
        let bytes = count * self.ss;
        if self.stats.reads >= self.max_reads
            || self.stats.bytes.saturating_add(bytes) > self.max_bytes
        {
            return Err(Stop::Budget);
        }
        let t = self.tier.index();
        if self.tier.is_grid() {
            let reserved = self
                .reserve
                .saturating_sub(self.stats.reads_by_tier[0] + self.stats.reads_by_tier[1]);
            if self.stats.reads_by_tier[t] >= self.tier_cap
                || self.stats.reads + 1 > self.max_reads.saturating_sub(reserved)
            {
                return Err(Stop::TierCap);
            }
        }
        let got = read_trusted(self.dev, lba, count, crate::mbr::READ_RETRIES);
        for i in 0..count {
            let l = lba + i;
            let data = match &got {
                Ok(t) => t.sector(l).map(|s| s.to_vec().into_boxed_slice()),
                Err(_) => None,
            };
            if data.is_none() {
                self.untrusted.insert(l, ());
            }
            self.cache.insert(l, data);
            self.order.push_back(l);
        }
        self.stats.reads += 1;
        self.stats.bytes += bytes;
        self.stats.reads_by_tier[t] += 1;
        self.stats.unverifiable_sectors = self.untrusted.len() as u64;
        self.observer.progress(&SearchProgress {
            reads: self.stats.reads,
            max_reads: self.max_reads,
            bytes: self.stats.bytes,
            tier: self.tier,
            candidate_byte: self.candidate,
            volumes: self.volumes,
        });
        Ok(())
    }
}
