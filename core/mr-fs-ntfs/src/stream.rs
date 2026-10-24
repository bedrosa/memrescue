//! Lectura del contenido de un flujo con su mapa de validez por tramos (ADR-019).
//!
//! [`DataStream::read`] devuelve los bytes pedidos y, para cada tramo, su estado
//! ([`RangeState`]):
//!
//! | Estado | Origen de los bytes | Fiable |
//! |---|---|---|
//! | `data` | leídos del disco (o del valor residente, o descomprimidos) | sí |
//! | `sparse` | run disperso o unidad de compresión sin clústeres: ceros del formato | sí |
//! | `uninitialized` | más allá del tamaño inicializado: ceros del formato, **nunca** se leen | sí |
//! | `unreadable` | sector ilegible o saltado del origen | no: patrón de relleno |
//! | `unmapped` | sin extensión que lo describa (registro de extensión perdido, runs no válidos) | no: patrón |
//! | `outside_source` | clústeres más allá del final del origen (volumen truncado) | no: patrón |
//! | `corrupt` | unidad de compresión que no se puede descomprimir | no: patrón |
//! | `possibly_overwritten` | leídos del disco, de clústeres que un fichero borrado ya no tiene en exclusiva (#23) | no: bytes tal como están |
//!
//! Los tramos no fiables se rellenan con el patrón ([`DEFAULT_FILL`], `BAD!`, alineado al
//! desplazamiento del flujo) y nunca se presentan como datos. Una unidad de compresión con algún
//! sector no fiable entre sus clústeres comprimidos se descomprime hasta el primer bloque afectado:
//! lo anterior es `data` y el resto de la unidad `unreadable`.
//!
//! **Clústeres sospechosos** ([`DataStream::with_suspect_clusters`], recuperación de borrados,
//! #23): los bytes leídos de esos clústeres se devuelven tal como están en el disco, pero con el
//! estado `possibly_overwritten` en lugar de `data` (pueden ser de otro fichero). Una unidad de
//! compresión con algún clúster sospechoso es entera `possibly_overwritten`.
//!
//! **Memoria acotada**: cada lectura pide como mucho [`MAX_STREAM_READ`] bytes; el disco se lee
//! por tramos de [`DISK_PIECE`] y una unidad de compresión ocupa como mucho 64 KiB.

use std::fmt;

use mr_io::BlockDevice;

use crate::file::{SegmentKind, StreamForm, StreamLayout};
use crate::lznt1::{decompress_into, Lznt1End};
use crate::read::read_span;
use crate::NtfsError;

/// Máximo de bytes por lectura de un flujo.
pub const MAX_STREAM_READ: usize = 16 << 20;
/// Tramo máximo del disco por lectura.
pub const DISK_PIECE: usize = 4 << 20;
/// Patrón de relleno de los tramos no fiables (el mismo que los sectores ilegibles de `mr-io`).
pub const DEFAULT_FILL: [u8; 4] = mr_io::resilient::DEFAULT_BAD_FILL;
/// Tamaño de los trozos de [`DataStream::copy_to`] (múltiplo de la unidad de compresión).
pub const COPY_CHUNK: usize = 1 << 20;
/// Máximo de tramos no fiables que guarda el informe de una copia (los recuentos son exactos).
pub const MAX_REPORTED_RANGES: usize = 10_000;

/// Estado de un tramo de bytes del flujo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RangeState {
    Data,
    Sparse,
    Uninitialized,
    Unreadable,
    Unmapped,
    OutsideSource,
    Corrupt,
    /// Leído del disco, pero de un clúster que puede pertenecer ya a otro fichero (#23).
    PossiblyOverwritten,
}

impl RangeState {
    pub const ALL: [RangeState; 8] = [
        RangeState::Data,
        RangeState::Sparse,
        RangeState::Uninitialized,
        RangeState::Unreadable,
        RangeState::Unmapped,
        RangeState::OutsideSource,
        RangeState::Corrupt,
        RangeState::PossiblyOverwritten,
    ];

    /// `true` si los bytes son contenido del fichero (datos o ceros definidos por el formato).
    pub fn is_trustworthy(self) -> bool {
        matches!(
            self,
            RangeState::Data | RangeState::Sparse | RangeState::Uninitialized
        )
    }

    /// `true` si los bytes son el patrón de relleno (no se pudo leer nada que interpretar). Un
    /// tramo `possibly_overwritten` no es fiable pero no lleva patrón: son los bytes del disco.
    pub fn is_filled(self) -> bool {
        !self.is_trustworthy() && self != RangeState::PossiblyOverwritten
    }

    pub fn code(self) -> &'static str {
        match self {
            RangeState::Data => "data",
            RangeState::Sparse => "sparse",
            RangeState::Uninitialized => "uninitialized",
            RangeState::Unreadable => "unreadable",
            RangeState::Unmapped => "unmapped",
            RangeState::OutsideSource => "outside_source",
            RangeState::Corrupt => "corrupt",
            RangeState::PossiblyOverwritten => "possibly_overwritten",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RangeState::Data => "datos",
            RangeState::Sparse => "disperso (ceros del formato)",
            RangeState::Uninitialized => "sin inicializar (ceros del formato)",
            RangeState::Unreadable => "ILEGIBLE",
            RangeState::Unmapped => "SIN MAPA",
            RangeState::OutsideSource => "FUERA DEL ORIGEN",
            RangeState::Corrupt => "COMPRIMIDO DAÑADO",
            RangeState::PossiblyOverwritten => "POSIBLEMENTE SOBRESCRITO",
        }
    }
}

/// Tramo `[offset, offset + length)` del flujo con su estado. `lbas` = primer LBA del origen y
/// número de sectores no fiables que lo causan (solo en `unreadable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRange {
    pub offset: u64,
    pub length: u64,
    pub state: RangeState,
    pub lbas: Option<(u64, u64)>,
}

impl fmt::Display for StreamRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bytes {}..{} ({} B): {}",
            self.offset,
            self.offset + self.length,
            self.length,
            self.state.label()
        )?;
        if let Some((l, n)) = self.lbas {
            write!(f, " (LBA {l}, {n} sectores)")?;
        }
        Ok(())
    }
}

/// Resultado de una lectura: bytes y tramos (contiguos, en orden, cubren exactamente `data`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamChunk {
    pub offset: u64,
    pub data: Vec<u8>,
    pub ranges: Vec<StreamRange>,
}

impl StreamChunk {
    pub fn all_trustworthy(&self) -> bool {
        self.ranges.iter().all(|r| r.state.is_trustworthy())
    }

    pub fn untrusted(&self) -> impl Iterator<Item = &StreamRange> {
        self.ranges.iter().filter(|r| !r.state.is_trustworthy())
    }
}

/// Construye un [`StreamChunk`] uniendo tramos contiguos del mismo estado.
struct Builder {
    offset: u64,
    data: Vec<u8>,
    ranges: Vec<StreamRange>,
    fill: [u8; 4],
}

impl Builder {
    fn new(offset: u64, cap: usize, fill: [u8; 4]) -> Self {
        Builder {
            offset,
            data: Vec::with_capacity(cap),
            ranges: Vec::new(),
            fill,
        }
    }

    fn pos(&self) -> u64 {
        self.offset + self.data.len() as u64
    }

    fn mark(&mut self, len: u64, state: RangeState, lbas: Option<(u64, u64)>) {
        if len == 0 {
            return;
        }
        let at = self.pos() - len;
        if let Some(last) = self.ranges.last_mut() {
            let lbas_join = match (last.lbas, lbas) {
                (None, None) => Some(None),
                (Some((a, n)), Some((b, m))) if a + n == b => Some(Some((a, n + m))),
                _ => None,
            };
            if last.state == state && last.offset + last.length == at {
                if let Some(l) = lbas_join {
                    last.length += len;
                    last.lbas = l;
                    return;
                }
            }
        }
        self.ranges.push(StreamRange {
            offset: at,
            length: len,
            state,
            lbas,
        });
    }

    fn bytes(&mut self, b: &[u8], state: RangeState) {
        self.data.extend_from_slice(b);
        self.mark(b.len() as u64, state, None);
    }

    fn zeros(&mut self, n: u64, state: RangeState) {
        self.data.resize(self.data.len() + n as usize, 0);
        self.mark(n, state, None);
    }

    /// `n` bytes del patrón de relleno (alineado al desplazamiento del flujo).
    fn pattern(&mut self, n: u64, state: RangeState, lbas: Option<(u64, u64)>) {
        let start = self.pos();
        for i in 0..n {
            self.data.push(self.fill[((start + i) % 4) as usize]);
        }
        self.mark(n, state, lbas);
    }

    /// Copia `[from, to)` (relativo a `src_offset`) de otro constructor.
    fn take(&mut self, src: &Builder, from: u64, to: u64) {
        for r in &src.ranges {
            let a = r.offset.max(from);
            let b = (r.offset + r.length).min(to);
            if a >= b {
                continue;
            }
            let s = (a - src.offset) as usize;
            let e = (b - src.offset) as usize;
            self.data.extend_from_slice(&src.data[s..e]);
            self.mark(b - a, r.state, r.lbas);
        }
    }

    fn finish(self) -> StreamChunk {
        StreamChunk {
            offset: self.offset,
            data: self.data,
            ranges: self.ranges,
        }
    }
}

/// Recuento por estado e informe de una copia completa.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyReport {
    pub bytes: u64,
    /// Bytes por estado (en el orden de [`RangeState::ALL`]).
    pub counts: Vec<(RangeState, u64)>,
    /// Tramos no fiables (como mucho [`MAX_REPORTED_RANGES`]).
    pub untrusted: Vec<StreamRange>,
    /// Hay más tramos no fiables que los guardados.
    pub truncated: bool,
}

impl CopyReport {
    pub fn count(&self, s: RangeState) -> u64 {
        self.counts
            .iter()
            .find(|(k, _)| *k == s)
            .map_or(0, |(_, n)| *n)
    }

    /// Añade un tramo no fiable, unido al anterior si es contiguo (en bytes y en LBA).
    fn add_untrusted(&mut self, r: StreamRange) {
        if let Some(l) = self.untrusted.last_mut() {
            if l.state == r.state && l.offset + l.length == r.offset {
                let lbas = match (l.lbas, r.lbas) {
                    (None, None) => Some(None),
                    (Some((a, n)), Some((b, m))) if a + n == b => Some(Some((a, n + m))),
                    _ => None,
                };
                if let Some(lbas) = lbas {
                    l.length += r.length;
                    l.lbas = lbas;
                    return;
                }
            }
        }
        if self.untrusted.len() >= MAX_REPORTED_RANGES {
            self.truncated = true;
        } else {
            self.untrusted.push(r);
        }
    }

    pub fn untrusted_bytes(&self) -> u64 {
        RangeState::ALL
            .iter()
            .filter(|s| !s.is_trustworthy())
            .map(|s| self.count(*s))
            .sum()
    }

    /// Bytes escritos con el patrón de relleno (no fiables y sin datos del disco).
    pub fn filled_bytes(&self) -> u64 {
        RangeState::ALL
            .iter()
            .filter(|s| s.is_filled())
            .map(|s| self.count(*s))
            .sum()
    }
}

/// Error de [`DataStream::copy_to`].
#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error(transparent)]
    Ntfs(#[from] NtfsError),
    #[error("error al escribir la salida: {0}")]
    Write(#[from] std::io::Error),
}

/// Flujo abierto para leer (ver el módulo).
pub struct DataStream<'v> {
    dev: &'v dyn BlockDevice,
    retries: u32,
    layout: StreamLayout,
    record: u64,
    record_in_use: bool,
    fill: [u8; 4],
    /// Tramos de clústeres `(lcn, n)` sospechosos, ordenados y sin solaparse.
    suspect: Vec<(u64, u64)>,
}

impl fmt::Debug for DataStream<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataStream")
            .field("record", &self.record)
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl<'v> DataStream<'v> {
    pub(crate) fn new(
        dev: &'v dyn BlockDevice,
        retries: u32,
        layout: StreamLayout,
        record: u64,
        record_in_use: bool,
    ) -> Self {
        DataStream {
            dev,
            retries,
            layout,
            record,
            record_in_use,
            fill: DEFAULT_FILL,
            suspect: Vec::new(),
        }
    }

    /// Marca los clústeres `(lcn, n)` como sospechosos: lo leído de ellos tiene el estado
    /// [`RangeState::PossiblyOverwritten`] (los bytes no cambian). Se ordenan y se unen.
    pub fn with_suspect_clusters(mut self, mut ranges: Vec<(u64, u64)>) -> Self {
        ranges.retain(|r| r.1 > 0);
        ranges.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (l, n) in ranges {
            let e = l.saturating_add(n);
            match out.last_mut() {
                Some((pl, pn)) if pl.saturating_add(*pn) >= l => *pn = (*pn).max(e - *pl),
                _ => out.push((l, e - l)),
            }
        }
        self.suspect = out;
        self
    }

    /// Clústeres sospechosos (ver [`DataStream::with_suspect_clusters`]).
    pub fn suspect_clusters(&self) -> &[(u64, u64)] {
        &self.suspect
    }

    /// Añade `data` (leído del byte `disk` del volumen) con el estado `data` o, en los clústeres
    /// sospechosos, `possibly_overwritten`.
    fn emit_disk(&self, b: &mut Builder, data: &[u8], disk: u64) {
        if self.suspect.is_empty() || data.is_empty() {
            b.bytes(data, RangeState::Data);
            return;
        }
        let cluster = u64::from(self.layout.cluster_size).max(1);
        let end = disk + data.len() as u64;
        let mut pos = disk;
        let mut i = self
            .suspect
            .partition_point(|&(l, n)| l.saturating_add(n).saturating_mul(cluster) <= pos);
        while pos < end {
            let (state, until) = match self.suspect.get(i) {
                Some(&(l, n)) => {
                    let a = l.saturating_mul(cluster);
                    let z = l.saturating_add(n).saturating_mul(cluster);
                    if pos < a {
                        (RangeState::Data, a.min(end))
                    } else {
                        i += 1;
                        (RangeState::PossiblyOverwritten, z.min(end))
                    }
                }
                None => (RangeState::Data, end),
            };
            let (s, e) = ((pos - disk) as usize, (until - disk) as usize);
            b.bytes(&data[s..e], state);
            pos = until;
        }
    }

    /// Cambia el patrón de relleno de los tramos no fiables.
    pub fn with_fill(mut self, fill: [u8; 4]) -> Self {
        self.fill = fill;
        self
    }

    pub fn layout(&self) -> &StreamLayout {
        &self.layout
    }

    /// Registro base del fichero.
    pub fn record(&self) -> u64 {
        self.record
    }

    /// `false` si el registro base no está en uso (fichero borrado: sus clústeres pueden haberse
    /// reutilizado; #23 da la fiabilidad).
    pub fn record_in_use(&self) -> bool {
        self.record_in_use
    }

    pub fn size(&self) -> u64 {
        self.layout.data_size
    }

    /// Lee hasta `len` bytes (como mucho [`MAX_STREAM_READ`]) desde `offset`; se recorta al tamaño
    /// del flujo.
    pub fn read(&self, offset: u64, len: usize) -> Result<StreamChunk, NtfsError> {
        if len > MAX_STREAM_READ {
            return Err(NtfsError::ReadTooLarge {
                len,
                max: MAX_STREAM_READ,
            });
        }
        let size = self.layout.data_size;
        let end = offset.saturating_add(len as u64).min(size);
        let mut b = Builder::new(offset, end.saturating_sub(offset) as usize, self.fill);
        if offset >= end {
            return Ok(b.finish());
        }
        match &self.layout.form {
            StreamForm::Resident(v) => {
                let a = offset as usize;
                let e = (end as usize).min(v.len());
                if a < e {
                    b.bytes(&v[a..e], RangeState::Data);
                }
            }
            StreamForm::NonResident => {
                let init = self.layout.initialized_size.min(size);
                let content_end = end.min(init);
                if offset < content_end {
                    if self.layout.is_compressed() {
                        self.read_compressed(offset, content_end, &mut b)?;
                    } else {
                        self.read_plain(offset, content_end, &mut b)?;
                    }
                }
                let z = offset.max(init);
                if z < end {
                    b.zeros(end - z, RangeState::Uninitialized);
                }
            }
        }
        Ok(b.finish())
    }

    /// Bytes del volumen `[disk, disk + n)` como contenido del flujo.
    fn read_disk(&self, disk: u64, n: u64, b: &mut Builder) -> Result<(), NtfsError> {
        let source = self.dev.sector_count() * u64::from(self.dev.sector_size());
        let inside = source.saturating_sub(disk).min(n);
        let mut done = 0u64;
        while done < inside {
            let take = (inside - done).min(DISK_PIECE as u64);
            let s = read_span(self.dev, disk + done, take as usize, self.retries)?;
            let mut at = 0usize;
            let base = disk + done;
            for &(lba, from, to) in &s.untrusted {
                if from > at {
                    self.emit_disk(b, &s.data[at..from], base + at as u64);
                }
                b.pattern((to - from) as u64, RangeState::Unreadable, Some((lba, 1)));
                at = to;
            }
            if at < s.data.len() {
                self.emit_disk(b, &s.data[at..], base + at as u64);
            }
            done += take;
        }
        if inside < n {
            b.pattern(n - inside, RangeState::OutsideSource, None);
        }
        Ok(())
    }

    /// Índice del segmento que contiene `vcn`.
    fn segment_at(&self, vcn: u64) -> Option<usize> {
        let s = &self.layout.segments;
        let i = s.partition_point(|x| x.end_vcn() <= vcn);
        (i < s.len() && s[i].vcn <= vcn).then_some(i)
    }

    fn read_plain(&self, from: u64, to: u64, b: &mut Builder) -> Result<(), NtfsError> {
        let cluster = u64::from(self.layout.cluster_size);
        let mut pos = from;
        while pos < to {
            let vcn = pos / cluster;
            let Some(i) = self.segment_at(vcn) else {
                b.pattern(to - pos, RangeState::Unmapped, None);
                break;
            };
            let seg = self.layout.segments[i];
            let seg_end = seg.end_vcn().saturating_mul(cluster);
            let n = seg_end.min(to) - pos;
            match seg.kind {
                SegmentKind::Sparse => b.zeros(n, RangeState::Sparse),
                SegmentKind::Unmapped => b.pattern(n, RangeState::Unmapped, None),
                SegmentKind::Disk(lcn) => {
                    // LCN + longitud ≤ clústeres del volumen (comprobado al decodificar).
                    let disk = (lcn + (vcn - seg.vcn)) * cluster + pos % cluster;
                    self.read_disk(disk, n, b)?;
                }
            }
            pos += n;
        }
        Ok(())
    }

    fn read_compressed(&self, from: u64, to: u64, b: &mut Builder) -> Result<(), NtfsError> {
        let cluster = u64::from(self.layout.cluster_size);
        let cu_clusters = self.layout.compression_unit_clusters;
        let cu_bytes = cu_clusters * cluster;
        let mut k = from / cu_bytes;
        while k * cu_bytes < to {
            let unit = self.unit(k)?;
            let a = from.max(k * cu_bytes);
            let e = to.min((k + 1) * cu_bytes);
            b.take(&unit, a, e);
            k += 1;
        }
        Ok(())
    }

    /// Contenido completo de la unidad de compresión `k`.
    fn unit(&self, k: u64) -> Result<Builder, NtfsError> {
        let cluster = u64::from(self.layout.cluster_size);
        let n = self.layout.compression_unit_clusters;
        let cu_bytes = (n * cluster) as usize;
        let start = k * n * cluster;
        let mut u = Builder::new(start, cu_bytes, self.fill);
        // Clústeres de la unidad: tramos en disco (en orden de VCN), dispersos y sin mapa.
        let (v0, v1) = (k * n, (k + 1) * n);
        let mut disk: Vec<(u64, u64)> = Vec::new();
        let (mut sparse, mut unmapped) = (0u64, 0u64);
        let mut vcn = v0;
        while vcn < v1 {
            let Some(i) = self.segment_at(vcn) else {
                unmapped += v1 - vcn;
                break;
            };
            let seg = self.layout.segments[i];
            let take = seg.end_vcn().min(v1) - vcn;
            match seg.kind {
                SegmentKind::Disk(lcn) => {
                    let l = lcn + (vcn - seg.vcn);
                    match disk.last_mut() {
                        Some((pl, pn)) if *pl + *pn == l => *pn += take,
                        _ => disk.push((l, take)),
                    }
                }
                SegmentKind::Sparse => sparse += take,
                SegmentKind::Unmapped => unmapped += take,
            }
            vcn += take;
        }
        let real: u64 = disk.iter().map(|d| d.1).sum();
        if unmapped > 0 {
            u.pattern(cu_bytes as u64, RangeState::Unmapped, None);
            return Ok(u);
        }
        if sparse == n {
            u.zeros(cu_bytes as u64, RangeState::Sparse);
            return Ok(u);
        }
        if real == n {
            // Unidad sin comprimir: se lee tal cual.
            for (lcn, len) in disk {
                self.read_disk(lcn * cluster, len * cluster, &mut u)?;
            }
            return Ok(u);
        }
        // Unidad comprimida: sus clústeres en disco, en orden, son el flujo LZNT1.
        let mut raw = Builder::new(0, (real * cluster) as usize, self.fill);
        for (lcn, len) in disk {
            self.read_disk(lcn * cluster, len * cluster, &mut raw)?;
        }
        let first_bad = raw.ranges.iter().find(|r| r.state.is_filled()).copied();
        let trusted = first_bad.map_or(raw.data.len(), |r| r.offset as usize);
        // Un solo clúster sospechoso hace sospechosa toda la unidad descomprimida (no se puede
        // saber qué bytes de la salida salen de él).
        let ok = if raw
            .ranges
            .iter()
            .any(|r| r.state == RangeState::PossiblyOverwritten)
        {
            RangeState::PossiblyOverwritten
        } else {
            RangeState::Data
        };
        let mut out = Vec::with_capacity(cu_bytes);
        let result = decompress_into(&raw.data[..trusted], &mut out, cu_bytes);
        u.bytes(&out, ok);
        let rest = (cu_bytes - out.len()) as u64;
        match (first_bad, result) {
            // Terminó dentro de la parte fiable: el resto de la unidad son ceros del formato.
            (None, Ok(_)) => u.zeros(rest, ok),
            (Some(_), Ok(Lznt1End::Terminator(_))) => u.zeros(rest, ok),
            (Some(bad), _) => {
                let last = raw.ranges.iter().rev().find(|r| r.state.is_filled());
                let lbas = match (bad.lbas, last.and_then(|r| r.lbas)) {
                    (Some((a, _)), Some((z, m))) => Some((a, z + m - a)),
                    (x, _) => x,
                };
                u.pattern(rest, bad.state, lbas);
            }
            (None, Err(_)) => u.pattern(rest, RangeState::Corrupt, None),
        }
        Ok(u)
    }

    /// Copia el flujo entero a `w` en trozos de [`COPY_CHUNK`] (memoria acotada). Los tramos no
    /// fiables se escriben con el patrón de relleno y se informan; los `possibly_overwritten` se
    /// escriben tal como están en el disco y también se informan.
    pub fn copy_to(&self, w: &mut dyn std::io::Write) -> Result<CopyReport, CopyError> {
        let mut report = CopyReport {
            counts: RangeState::ALL.iter().map(|&s| (s, 0)).collect(),
            ..CopyReport::default()
        };
        let mut offset = 0u64;
        while offset < self.size() {
            let c = self.read(offset, COPY_CHUNK)?;
            w.write_all(&c.data)?;
            for r in &c.ranges {
                if let Some(x) = report.counts.iter_mut().find(|(s, _)| *s == r.state) {
                    x.1 += r.length;
                }
                if !r.state.is_trustworthy() {
                    report.add_untrusted(*r);
                }
            }
            report.bytes += c.data.len() as u64;
            offset += c.data.len() as u64;
            if c.data.is_empty() {
                break;
            }
        }
        Ok(report)
    }
}
