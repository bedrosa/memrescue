//! Disposición de la MFT y lectura de registros por número, a través del mapa de validez.
//!
//! La MFT es el contenido del atributo `$DATA` sin nombre del registro 0 (`$MFT`): sus runs dicen
//! dónde está cada tramo y su tamaño de datos cuántos registros hay (`data_size / record_size`).
//! Un registro más allá del tamaño inicializado no se lee (`Uninitialized`); uno sin run que lo
//! cubra (MFT extendida en registros que esta versión no sigue) es `Unmapped`; uno cuyos bytes
//! caen fuera del origen es `OutsideSource`; uno con algún sector no fiable es `Unreadable` (su
//! relleno nunca se interpreta); uno entero a ceros es `Empty` (nunca inicializado) y los demás se
//! interpretan con [`FileRecord::parse`]: `Parsed` o `Damaged`.

use std::collections::VecDeque;

use mr_io::BlockDevice;

use crate::read::read_span;
use crate::record::{FileRecord, RecordDamage};
use crate::runs::Run;
use crate::NtfsError;

/// Máximo de registros admitidos en la MFT (2^32: muy por encima de cualquier volumen real; limita
/// los recorridos y los cálculos).
pub const MAX_RECORDS: u64 = 1 << 32;

/// Registros que se leen de una vez al enumerar (si están contiguos en disco).
pub const BATCH_RECORDS: u64 = 64;

/// Dónde está la MFT y cuántos registros tiene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftLayout {
    pub record_size: u32,
    pub cluster_size: u32,
    /// Runs del `$DATA` de `$MFT` tal como están en el registro 0.
    pub runs: Vec<Run>,
    pub allocated_size: u64,
    pub data_size: u64,
    pub initialized_size: u64,
    /// Registros de la MFT: `data_size / record_size` (acotado por [`MAX_RECORDS`]).
    pub record_count: u64,
}

impl MftLayout {
    /// Tramos en disco `(byte del volumen, bytes)` de `[start, start + len)` de la MFT; `None` si
    /// alguna parte no tiene run con clústeres.
    pub fn map(&self, start: u64, len: u64) -> Option<Vec<(u64, u64)>> {
        let cluster = u64::from(self.cluster_size);
        let end = start.checked_add(len)?;
        let mut out: Vec<(u64, u64)> = Vec::new();
        let mut pos = start;
        while pos < end {
            let vcn = pos / cluster;
            let run = self
                .runs
                .iter()
                .find(|r| r.vcn <= vcn && vcn < r.end_vcn())?;
            let lcn = run.lcn?;
            let run_end_byte = run.end_vcn().checked_mul(cluster)?;
            let take = (end.min(run_end_byte)) - pos;
            let disk = (lcn + (vcn - run.vcn))
                .checked_mul(cluster)?
                .checked_add(pos % cluster)?;
            match out.last_mut() {
                Some((d, l)) if *d + *l == disk => *l += take,
                _ => out.push((disk, take)),
            }
            pos += take;
        }
        Some(out)
    }

    /// Registros que caben en los runs (cubiertos por clústeres asignados en disco o no).
    pub fn mapped_records(&self) -> u64 {
        let clusters = self.runs.last().map_or(0, |r| r.end_vcn());
        (clusters.saturating_mul(u64::from(self.cluster_size)) / u64::from(self.record_size))
            .min(MAX_RECORDS)
    }
}

/// Resultado de leer un registro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Registro FILE válido (en uso o no).
    Parsed(FileRecord),
    /// Todo a ceros: nunca se inicializó.
    Empty,
    /// Más allá del tamaño inicializado de `$MFT`: no se lee.
    Uninitialized,
    /// Sin run que lo cubra (o en un run disperso).
    Unmapped,
    /// Sus bytes caen fuera del origen (volumen truncado).
    OutsideSource,
    /// Algún sector no es fiable (ilegible o saltado): no se interpreta.
    Unreadable { lbas: Vec<u64> },
    /// Leído pero no válido: nunca se usa como registro.
    Damaged(RecordDamage),
}

/// Estado resumido de un registro (para listados y recuentos).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RecordState {
    InUse,
    NotInUse,
    Empty,
    Uninitialized,
    Unmapped,
    OutsideSource,
    Unreadable,
    Damaged,
}

impl RecordState {
    pub const ALL: [RecordState; 8] = [
        RecordState::InUse,
        RecordState::NotInUse,
        RecordState::Empty,
        RecordState::Uninitialized,
        RecordState::Unmapped,
        RecordState::OutsideSource,
        RecordState::Unreadable,
        RecordState::Damaged,
    ];

    /// Código estable.
    pub fn code(self) -> &'static str {
        match self {
            RecordState::InUse => "in_use",
            RecordState::NotInUse => "not_in_use",
            RecordState::Empty => "empty",
            RecordState::Uninitialized => "uninitialized",
            RecordState::Unmapped => "unmapped",
            RecordState::OutsideSource => "outside_source",
            RecordState::Unreadable => "unreadable",
            RecordState::Damaged => "damaged",
        }
    }

    /// Texto breve en español.
    pub fn label(self) -> &'static str {
        match self {
            RecordState::InUse => "en uso",
            RecordState::NotInUse => "no en uso (borrado o libre)",
            RecordState::Empty => "vacío (nunca inicializado)",
            RecordState::Uninitialized => "más allá del tamaño inicializado",
            RecordState::Unmapped => "sin run que lo cubra",
            RecordState::OutsideSource => "fuera del origen",
            RecordState::Unreadable => "ILEGIBLE",
            RecordState::Damaged => "DAÑADO",
        }
    }
}

impl RecordOutcome {
    pub fn state(&self) -> RecordState {
        match self {
            RecordOutcome::Parsed(r) if r.in_use() => RecordState::InUse,
            RecordOutcome::Parsed(_) => RecordState::NotInUse,
            RecordOutcome::Empty => RecordState::Empty,
            RecordOutcome::Uninitialized => RecordState::Uninitialized,
            RecordOutcome::Unmapped => RecordState::Unmapped,
            RecordOutcome::OutsideSource => RecordState::OutsideSource,
            RecordOutcome::Unreadable { .. } => RecordState::Unreadable,
            RecordOutcome::Damaged(_) => RecordState::Damaged,
        }
    }

    pub fn record(&self) -> Option<&FileRecord> {
        match self {
            RecordOutcome::Parsed(r) => Some(r),
            _ => None,
        }
    }

    pub fn into_record(self) -> Option<FileRecord> {
        match self {
            RecordOutcome::Parsed(r) => Some(r),
            _ => None,
        }
    }

    /// Texto del motivo para los estados que no son un registro interpretado.
    pub fn detail(&self) -> Option<String> {
        match self {
            RecordOutcome::Unreadable { lbas } => Some(format!(
                "sectores no fiables: {}",
                lbas.iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            RecordOutcome::Damaged(d) => Some(d.to_string()),
            _ => None,
        }
    }
}

/// Clasifica los bytes fiables de un registro.
pub(crate) fn classify(number: u64, raw: Vec<u8>, record_size: u32) -> RecordOutcome {
    if raw.iter().all(|&b| b == 0) {
        return RecordOutcome::Empty;
    }
    match FileRecord::parse(number, raw, record_size) {
        Ok(r) => RecordOutcome::Parsed(r),
        Err(d) => RecordOutcome::Damaged(d),
    }
}

/// Lee el registro `number` según `layout` (sin comprobar `record_count`: lo hace el llamador).
pub(crate) fn read_record_at(
    dev: &(impl BlockDevice + ?Sized),
    layout: &MftLayout,
    number: u64,
    retries: u32,
) -> Result<RecordOutcome, NtfsError> {
    let rs = u64::from(layout.record_size);
    let Some(start) = number.checked_mul(rs) else {
        return Ok(RecordOutcome::Unmapped);
    };
    if start >= layout.initialized_size {
        return Ok(RecordOutcome::Uninitialized);
    }
    let Some(pieces) = layout.map(start, rs) else {
        return Ok(RecordOutcome::Unmapped);
    };
    let size = dev.sector_count() * u64::from(dev.sector_size());
    if pieces
        .iter()
        .any(|&(d, l)| d.checked_add(l).is_none_or(|e| e > size))
    {
        return Ok(RecordOutcome::OutsideSource);
    }
    let mut raw = Vec::with_capacity(rs as usize);
    let mut lbas = Vec::new();
    for (d, l) in pieces {
        let s = read_span(dev, d, l as usize, retries)?;
        lbas.extend(s.untrusted.iter().map(|&(lba, _, _)| lba));
        raw.extend_from_slice(&s.data);
    }
    if !lbas.is_empty() {
        return Ok(RecordOutcome::Unreadable { lbas });
    }
    Ok(classify(number, raw, layout.record_size))
}

/// Lee `count` registros desde `first` que están contiguos en disco a partir del byte `disk`.
pub(crate) fn read_contiguous(
    dev: &(impl BlockDevice + ?Sized),
    layout: &MftLayout,
    first: u64,
    count: u64,
    disk: u64,
    retries: u32,
) -> Result<Vec<(u64, RecordOutcome)>, NtfsError> {
    let rs = layout.record_size as usize;
    let s = read_span(dev, disk, rs * count as usize, retries)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let n = first + i as u64;
        let (a, b) = (i * rs, (i + 1) * rs);
        let lbas = s.untrusted_in(a, b);
        let o = if lbas.is_empty() {
            classify(n, s.data[a..b].to_vec(), layout.record_size)
        } else {
            RecordOutcome::Unreadable { lbas }
        };
        out.push((n, o));
    }
    Ok(out)
}

/// Iterador de registros `[first, end)` con su resultado. Lee por lotes de registros contiguos en
/// disco ([`BATCH_RECORDS`]) y cada registro se clasifica por separado: un registro dañado o
/// ilegible no impide leer los demás. Un error de E/S no recuperable (no un sector ilegible, que se
/// informa en el registro) termina el recorrido.
pub struct Records<'v> {
    pub(crate) dev: &'v dyn BlockDevice,
    pub(crate) layout: &'v MftLayout,
    pub(crate) retries: u32,
    pub(crate) next: u64,
    pub(crate) end: u64,
    pub(crate) pending: VecDeque<(u64, RecordOutcome)>,
    pub(crate) failed: bool,
}

impl Records<'_> {
    fn fill(&mut self) -> Result<(), NtfsError> {
        let n = self.next;
        let rs = u64::from(self.layout.record_size);
        let size = self.dev.sector_count() * u64::from(self.dev.sector_size());
        // Lote: registros inicializados, en el mismo tramo contiguo en disco y dentro del origen.
        let init_limit = self.layout.initialized_size / rs;
        let mut batch = (self.end - n).min(BATCH_RECORDS);
        if n < init_limit {
            batch = batch.min(init_limit - n);
            if let Some(p) = self.layout.map(n * rs, batch * rs) {
                let (disk, len) = p[0];
                let fit = len / rs;
                if fit > 0 && disk.checked_add(fit * rs).is_some_and(|e| e <= size) {
                    let v = read_contiguous(self.dev, self.layout, n, fit, disk, self.retries)?;
                    self.next += fit;
                    self.pending.extend(v);
                    return Ok(());
                }
            }
        }
        let o = read_record_at(self.dev, self.layout, n, self.retries)?;
        self.next += 1;
        self.pending.push_back((n, o));
        Ok(())
    }
}

impl Iterator for Records<'_> {
    type Item = Result<(u64, RecordOutcome), NtfsError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        if self.pending.is_empty() && self.next < self.end {
            if let Err(e) = self.fill() {
                self.failed = true;
                return Some(Err(e));
            }
        }
        self.pending.pop_front().map(Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(runs: Vec<Run>, cluster: u32, record: u32) -> MftLayout {
        let clusters = runs.last().map_or(0, |r| r.end_vcn());
        let bytes = clusters * u64::from(cluster);
        MftLayout {
            record_size: record,
            cluster_size: cluster,
            runs,
            allocated_size: bytes,
            data_size: bytes,
            initialized_size: bytes,
            record_count: bytes / u64::from(record),
        }
    }

    #[test]
    fn map_follows_runs_and_crosses_boundaries() {
        // Clúster de 512 B y registros de 1 KiB: un registro puede partirse entre dos runs.
        let l = layout(
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(100),
                    length: 3,
                },
                Run {
                    vcn: 3,
                    lcn: Some(10),
                    length: 5,
                },
                Run {
                    vcn: 8,
                    lcn: None,
                    length: 2,
                },
            ],
            512,
            1024,
        );
        assert_eq!(l.map(0, 1024), Some(vec![(51200, 1024)]));
        assert_eq!(l.map(1024, 1024), Some(vec![(52224, 512), (5120, 512)]));
        assert_eq!(l.map(2048, 1024), Some(vec![(5632, 1024)]));
        assert_eq!(l.map(4096, 1024), None, "run disperso");
        assert_eq!(l.map(5120, 1024), None, "fuera de los runs");
        assert_eq!(l.mapped_records(), 5);
        // Clúster de 4 KiB con 4 registros por clúster.
        let l = layout(
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(4),
                    length: 1,
                },
                Run {
                    vcn: 1,
                    lcn: Some(9),
                    length: 1,
                },
            ],
            4096,
            1024,
        );
        assert_eq!(l.map(3072, 1024), Some(vec![(4 * 4096 + 3072, 1024)]));
        assert_eq!(l.map(4096, 1024), Some(vec![(9 * 4096, 1024)]));
        assert_eq!(l.map(0, 8192), Some(vec![(16384, 4096), (36864, 4096)]));
        assert_eq!(l.map(u64::MAX, 2), None);
    }

    #[test]
    fn state_codes_are_unique() {
        let mut codes: Vec<_> = RecordState::ALL.iter().map(|s| s.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), RecordState::ALL.len());
        assert!(RecordState::ALL.iter().all(|s| !s.label().is_empty()));
    }
}
