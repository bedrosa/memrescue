//! Lectura de rangos de sectores tolerante a errores, con **mapa explícito de validez**, progreso y
//! cancelación.
//!
//! Cada sector entregado lleva su [`SectorState`]. El contenido de un sector `Unreadable` o
//! `Skipped` es un patrón de relleno y **nunca** se presenta como dato: el estado acompaña a los
//! bytes en cada entrega al consumidor (`sink`) y queda resumido en el [`ValidityMap`] del informe.
//!
//! Estrategia:
//! 1. Se consulta [`BlockDevice::allocation`] por bloque: los sectores dispersos no se leen; se
//!    entregan ceros con estado `Sparse`.
//! 2. Los sectores con datos se leen por bloques; si un bloque falla, se relee **sector a sector**
//!    con reintentos (`Read` si el primer intento individual funciona, `Recovered` si hizo falta
//!    reintentar).
//! 3. Un sector que sigue fallando aborta la operación con su posición exacta ([`OnError::Fail`]) o
//!    se rellena con `bad_fill` y se marca `Unreadable` ([`OnError::FillAndContinue`]).
//! 4. Opcionalmente, tras un ilegible se **saltan** `jump_sectors` sectores sin leerlos (`Skipped`,
//!    relleno `skip_fill`) y, si `read_back`, se lee hacia atrás desde el final del salto hasta
//!    encontrar el siguiente error, para recuperar el borde legible de la zona dañada.
//!
//! Memoria: como máximo dos búferes de `chunk` sectores (salida y lectura hacia atrás), con
//! `chunk × sector ≤ MAX_READ_REQUEST`, sea cual sea el tamaño del rango o del salto.
//! Ver `docs/formatos/mapa-de-validez.md`.

use crate::{
    sector_span, Allocation, AllocationRun, BlockDevice, IoError, Result, SectorState,
    MAX_READ_REQUEST,
};

/// Qué hacer con un sector que no se puede leer tras los reintentos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Abortar la operación (p. ej. hashes forenses: un hash con relleno no sería válido).
    Fail,
    /// Rellenar con el patrón, marcar el sector `Unreadable` y continuar (verificación, clonado).
    FillAndContinue,
}

#[derive(Debug, Clone)]
pub struct ReadPolicy {
    /// Sectores por petición de lectura normal (se acota a `MAX_READ_REQUEST`).
    pub chunk_sectors: u32,
    /// Reintentos adicionales por sector tras un fallo.
    pub retries: u32,
    pub on_error: OnError,
    /// Patrón de 4 bytes con que se rellenan los sectores ilegibles (`Unreadable`).
    pub bad_fill: [u8; 4],
    /// Patrón de 4 bytes con que se rellenan los sectores saltados (`Skipped`).
    pub skip_fill: [u8; 4],
    /// Tras un sector ilegible, saltar estos sectores sin leerlos (0 = no saltar). Solo se aplica
    /// con [`OnError::FillAndContinue`].
    pub jump_sectors: u64,
    /// Tras un salto, leer hacia atrás desde su último sector hasta el siguiente error.
    pub read_back: bool,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            chunk_sectors: 2048,
            retries: 1,
            on_error: OnError::Fail,
            bad_fill: *b"BAD!",
            skip_fill: *b"SKIP",
            jump_sectors: 0,
            read_back: false,
        }
    }
}

/// Tamaño máximo del búfer de la lectura hacia atrás tras un salto. Si se llena sin encontrar
/// un error, la lectura hacia atrás termina ahí y el resto del salto sigue `Skipped`.
pub const READ_BACK_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Máximo de tramos que guarda un [`ValidityMap`] (los recuentos por estado siempre son exactos).
pub const MAX_MAP_RUNS: usize = 100_000;

/// Mapa de validez: tramos `(lba, nº de sectores, estado)` ordenados, sin solapes y fusionados
/// (dos tramos contiguos nunca tienen el mismo estado), más recuentos exactos por estado.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidityMap {
    runs: Vec<(u64, u64, SectorState)>,
    counts: [u64; 5],
    truncated: bool,
    /// Primer LBA admisible para el siguiente `push` (fin del último tramo añadido).
    next: u64,
}

impl ValidityMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Añade `count` sectores desde `lba` con `state`. Se fusiona con el tramo anterior si es
    /// contiguo y del mismo estado; puede haber huecos (sectores no cubiertos) entre tramos.
    /// `count == 0` no tiene efecto.
    ///
    /// # Panics
    /// Si el tramo empieza antes del final del último añadido (desordenado o solapado) o si
    /// `lba + count` desborda `u64`. Es un error de programación: aceptarlo duplicaría o
    /// reordenaría sectores y los recuentos dejarían de ser exactos.
    pub fn push(&mut self, lba: u64, count: u64, state: SectorState) {
        if count == 0 {
            return;
        }
        let end = lba
            .checked_add(count)
            .expect("ValidityMap::push: el tramo desborda u64");
        assert!(
            lba >= self.next,
            "ValidityMap::push: tramo desordenado o solapado (LBA {lba}, se esperaba ≥ {})",
            self.next
        );
        self.counts[state as usize] += count;
        self.next = end;
        if let Some(last) = self.runs.last_mut() {
            if last.2 == state && last.0 + last.1 == lba {
                last.1 += count;
                return;
            }
        }
        if self.runs.len() < MAX_MAP_RUNS {
            self.runs.push((lba, count, state));
        } else {
            self.truncated = true;
        }
    }

    /// Tramos guardados (completos salvo que [`Self::truncated`] sea `true`).
    pub fn runs(&self) -> &[(u64, u64, SectorState)] {
        &self.runs
    }

    /// `true` si se superó [`MAX_MAP_RUNS`]: faltan tramos, pero los recuentos siguen siendo exactos.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Número exacto de sectores con `state`.
    pub fn count(&self, state: SectorState) -> u64 {
        self.counts[state as usize]
    }

    /// Número total de sectores registrados.
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// `true` si ningún sector es `Unreadable` ni `Skipped` (exacto aunque el mapa esté truncado).
    pub fn all_trustworthy(&self) -> bool {
        SectorState::ALL
            .iter()
            .all(|s| s.is_trustworthy() || self.count(*s) == 0)
    }

    /// Tramos guardados cuyo contenido **no** es fiable (`Unreadable` o `Skipped`).
    pub fn untrusted_runs(&self) -> impl Iterator<Item = (u64, u64, SectorState)> + '_ {
        self.runs.iter().copied().filter(|r| !r.2.is_trustworthy())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub sectors_requested: u64,
    /// Lecturas repetidas por errores (bloques releídos sector a sector y reintentos).
    pub retry_reads: u64,
    /// Estado de cada sector del rango.
    pub map: ValidityMap,
}

/// Rellena `buf` repitiendo `pattern` (alineado al inicio de `buf`).
fn fill(buf: &mut [u8], pattern: [u8; 4]) {
    let n = buf.len().min(4);
    buf[..n].copy_from_slice(&pattern[..n]);
    let mut done = n;
    while done < buf.len() {
        let k = done.min(buf.len() - done);
        buf.copy_within(..k, done);
        done += k;
    }
}

/// Mapa de asignación validado: tramos contiguos que cubren exactamente `[lba, lba + n)`.
fn allocation_checked(
    dev: &(impl BlockDevice + ?Sized),
    lba: u64,
    n: u64,
) -> Result<Vec<AllocationRun>> {
    let runs = dev.allocation(lba, n)?;
    let mut next = lba;
    for &(a, c, _) in &runs {
        if a != next || c == 0 {
            next = u64::MAX;
            break;
        }
        next = a.saturating_add(c);
    }
    if next != lba + n {
        return Err(IoError::Corrupt {
            format: "mapa de asignación",
            detail: format!("los tramos devueltos no cubren exactamente LBA {lba} + {n}"),
        });
    }
    Ok(runs)
}

/// Búfer de salida: acumula sectores consecutivos con su estado y los entrega en orden creciente.
struct Out<S> {
    sink: S,
    ss: usize,
    cap: u64,
    first: u64,
    data: Vec<u8>,
    states: Vec<SectorState>,
    map: ValidityMap,
}

impl<S: FnMut(u64, &[u8], &[SectorState]) -> Result<()>> Out<S> {
    fn new(sink: S, ss: usize, cap: u64, first: u64) -> Self {
        Self {
            sink,
            ss,
            cap,
            first,
            data: Vec::with_capacity(cap as usize * ss),
            states: Vec::with_capacity(cap as usize),
            map: ValidityMap::new(),
        }
    }

    fn next_lba(&self) -> u64 {
        self.first + self.states.len() as u64
    }

    fn flush(&mut self) -> Result<()> {
        if self.states.is_empty() {
            return Ok(());
        }
        (self.sink)(self.first, &self.data, &self.states)?;
        let mut i = 0;
        while i < self.states.len() {
            let st = self.states[i];
            let mut j = i + 1;
            while j < self.states.len() && self.states[j] == st {
                j += 1;
            }
            self.map.push(self.first + i as u64, (j - i) as u64, st);
            i = j;
        }
        self.first = self.next_lba();
        self.data.clear();
        self.states.clear();
        Ok(())
    }

    /// Añade `c ≤ cap` sectores desde `lba` (que debe ser el siguiente) con `state`, rellenos con
    /// `pattern`, y devuelve su zona de datos.
    fn append(
        &mut self,
        lba: u64,
        c: u64,
        state: SectorState,
        pattern: [u8; 4],
    ) -> Result<&mut [u8]> {
        assert!(c <= self.cap, "tramo mayor que el búfer de salida");
        assert_eq!(lba, self.next_lba(), "entrega fuera de orden");
        if self.states.len() as u64 + c > self.cap {
            self.flush()?;
        }
        let at = self.data.len();
        self.data.resize(at + c as usize * self.ss, 0);
        if pattern != [0; 4] {
            fill(&mut self.data[at..], pattern);
        }
        self.states.resize(self.states.len() + c as usize, state);
        Ok(&mut self.data[at..])
    }

    fn index(&self, lba: u64) -> usize {
        usize::try_from(lba - self.first).expect("índice de búfer")
    }

    fn sector_mut(&mut self, lba: u64) -> &mut [u8] {
        let i = self.index(lba);
        &mut self.data[i * self.ss..(i + 1) * self.ss]
    }

    fn set_state(&mut self, lba: u64, st: SectorState) {
        let i = self.index(lba);
        self.states[i] = st;
    }

    /// Descarta lo añadido a partir de `lba` (aún no entregado).
    fn truncate(&mut self, lba: u64) {
        let keep = self.index(lba);
        self.states.truncate(keep);
        self.data.truncate(keep * self.ss);
    }
}

/// Resultado de la lectura hacia atrás de un salto.
struct ReadBack {
    /// Primer LBA de la zona leída con éxito `[lo, fin del salto)`.
    lo: u64,
    /// Sector que falló al leer hacia atrás (queda `Unreadable`).
    bad: Option<u64>,
    /// LBA del primer sector de `buf`.
    base: u64,
    buf: Vec<u8>,
    sparse: Vec<bool>,
}

struct Reader<'a, D: ?Sized, S> {
    dev: &'a D,
    policy: &'a ReadPolicy,
    ss: usize,
    chunk: u64,
    start: u64,
    end: u64,
    retry_reads: u64,
    out: Out<S>,
}

impl<D, S> Reader<'_, D, S>
where
    D: BlockDevice + ?Sized,
    S: FnMut(u64, &[u8], &[SectorState]) -> Result<()>,
{
    fn jumps(&self) -> bool {
        self.policy.on_error == OnError::FillAndContinue && self.policy.jump_sectors > 0
    }

    /// Procesa `[lba, lba + n)`. Devuelve `Some(s)` si el sector `s` quedó ilegible y hay que
    /// saltar a continuación (lo posterior a `s` en este bloque se descarta y lo trata el salto).
    fn block(&mut self, lba: u64, n: u64) -> Result<Option<u64>> {
        for (a, c, kind) in allocation_checked(self.dev, lba, n)? {
            match kind {
                Allocation::Sparse => {
                    self.out.append(a, c, SectorState::Sparse, [0; 4])?;
                }
                Allocation::Data => {
                    if let Some(bad) = self.data_run(a, c)? {
                        return Ok(Some(bad));
                    }
                }
            }
        }
        Ok(None)
    }

    fn data_run(&mut self, lba: u64, c: u64) -> Result<Option<u64>> {
        let dev = self.dev;
        let policy = self.policy;
        let buf = self.out.append(lba, c, SectorState::Read, [0; 4])?;
        if dev.read_sectors(lba, c, buf).is_ok() {
            return Ok(None);
        }
        self.retry_reads += 1;
        for s in lba..lba + c {
            let mut res = dev
                .read_sectors(s, 1, self.out.sector_mut(s))
                .map(|()| SectorState::Read);
            let mut attempt = 0;
            while res.is_err() && attempt < policy.retries {
                attempt += 1;
                self.retry_reads += 1;
                res = dev
                    .read_sectors(s, 1, self.out.sector_mut(s))
                    .map(|()| SectorState::Recovered);
            }
            match res {
                Ok(st) => self.out.set_state(s, st),
                Err(e) => match policy.on_error {
                    OnError::Fail => return Err(e),
                    OnError::FillAndContinue => {
                        fill(self.out.sector_mut(s), policy.bad_fill);
                        self.out.set_state(s, SectorState::Unreadable);
                        if self.jumps() {
                            self.out.truncate(s + 1);
                            return Ok(Some(s));
                        }
                    }
                },
            }
        }
        Ok(None)
    }

    /// Lee hacia atrás `[from, jend)` desde `jend - 1`, un intento por sector, hasta el primer
    /// error o hasta llenar un búfer de [`READ_BACK_MAX_BYTES`].
    fn read_back(&mut self, from: u64, jend: u64) -> Result<ReadBack> {
        let cap = (READ_BACK_MAX_BYTES / self.ss).max(1) as u64;
        let base = jend.saturating_sub(cap).max(from);
        let n = jend - base;
        let mut rb = ReadBack {
            lo: jend,
            bad: None,
            base,
            buf: vec![0u8; n as usize * self.ss],
            sparse: vec![false; n as usize],
        };
        for (a, c, kind) in allocation_checked(self.dev, base, n)? {
            if kind == Allocation::Sparse {
                for l in a..a + c {
                    rb.sparse[(l - base) as usize] = true;
                }
            }
        }
        for s in (base..jend).rev() {
            let i = (s - base) as usize;
            if !rb.sparse[i] {
                let sec = &mut rb.buf[i * self.ss..(i + 1) * self.ss];
                if self.dev.read_sectors(s, 1, sec).is_err() {
                    rb.bad = Some(s);
                    break;
                }
            }
            rb.lo = s;
        }
        Ok(rb)
    }

    /// Salta los sectores posteriores a un ilegible. Devuelve el LBA donde se reanuda la lectura.
    fn jump(
        &mut self,
        from: u64,
        count: u64,
        progress: &mut impl FnMut(u64, u64) -> bool,
    ) -> Result<u64> {
        let jend = from.saturating_add(self.policy.jump_sectors).min(self.end);
        if from >= jend {
            return Ok(from);
        }
        let rb = if self.policy.read_back {
            Some(self.read_back(from, jend)?)
        } else {
            None
        };
        // [from, skip_end) queda saltado; después, el sector fallido de la lectura hacia atrás y
        // la zona leída hacia atrás.
        let skip_end = rb.as_ref().map_or(jend, |r| r.bad.unwrap_or(r.lo));
        let mut p = from;
        while p < skip_end {
            if !progress(p - self.start, count) {
                return Err(IoError::Cancelled);
            }
            let n = self.chunk.min(skip_end - p);
            for (a, c, kind) in allocation_checked(self.dev, p, n)? {
                match kind {
                    Allocation::Sparse => {
                        self.out.append(a, c, SectorState::Sparse, [0; 4])?;
                    }
                    Allocation::Data => {
                        self.out
                            .append(a, c, SectorState::Skipped, self.policy.skip_fill)?;
                    }
                }
            }
            p += n;
        }
        if let Some(rb) = rb {
            if let Some(b) = rb.bad {
                self.out
                    .append(b, 1, SectorState::Unreadable, self.policy.bad_fill)?;
            }
            let mut p = rb.lo;
            while p < jend {
                let n = self.chunk.min(jend - p);
                let off = (p - rb.base) as usize;
                let src = &rb.buf[off * self.ss..(off + n as usize) * self.ss];
                self.out
                    .append(p, n, SectorState::Read, [0; 4])?
                    .copy_from_slice(src);
                for (i, &sp) in rb.sparse[off..off + n as usize].iter().enumerate() {
                    if sp {
                        self.out.set_state(p + i as u64, SectorState::Sparse);
                    }
                }
                p += n;
            }
        }
        Ok(jend)
    }
}

/// Lee los sectores `[start, start + count)` y entrega cada tramo a
/// `sink(lba_inicial, datos, estados)`, con un estado por sector, **siempre en orden creciente y
/// sin huecos**.
///
/// `progress(hechos, total)` (en sectores) se llama antes de cada bloque; si devuelve `false` la
/// operación se cancela con [`IoError::Cancelled`].
pub fn read_range(
    dev: &(impl BlockDevice + ?Sized),
    start: u64,
    count: u64,
    policy: &ReadPolicy,
    sink: impl FnMut(u64, &[u8], &[SectorState]) -> Result<()>,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<ReadReport> {
    let ss = dev.sector_size() as usize;
    let chunk = u64::from(policy.chunk_sectors.max(1)).min((MAX_READ_REQUEST / ss).max(1) as u64);
    // Valida el rango completo (sin reservar memoria para él).
    let total = dev.sector_count();
    if count == 0 || start.checked_add(count).is_none_or(|e| e > total) {
        return Err(IoError::InvalidRange {
            lba: start,
            count,
            total,
        });
    }
    let end = start + count;
    let mut r = Reader {
        dev,
        policy,
        ss,
        chunk,
        start,
        end,
        retry_reads: 0,
        out: Out::new(sink, ss, chunk, start),
    };
    let mut lba = start;
    while lba < end {
        if !progress(lba - start, count) {
            return Err(IoError::Cancelled);
        }
        let n = chunk.min(end - lba);
        lba = match r.block(lba, n)? {
            None => lba + n,
            Some(bad) => r.jump(bad + 1, count, &mut progress)?,
        };
    }
    r.out.flush()?;
    debug_assert_eq!(r.out.map.total(), count);
    progress(count, count);
    Ok(ReadReport {
        sectors_requested: count,
        retry_reads: r.retry_reads,
        map: r.out.map,
    })
}

/// Lectura acotada (≤ [`MAX_READ_REQUEST`]) con estados por sector, para el visor hexadecimal.
/// Devuelve `count × sector` bytes y `count` estados.
pub fn read_sectors_mapped(
    dev: &(impl BlockDevice + ?Sized),
    lba: u64,
    count: u64,
    policy: &ReadPolicy,
) -> Result<(Vec<u8>, Vec<SectorState>)> {
    let (_, len) = sector_span(dev, lba, count)?;
    let mut data = Vec::with_capacity(len);
    let mut states = Vec::with_capacity(count as usize);
    read_range(
        dev,
        lba,
        count,
        policy,
        |_, d, s| {
            data.extend_from_slice(d);
            states.extend_from_slice(s);
            Ok(())
        },
        |_, _| true,
    )?;
    Ok((data, states))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FaultyDevice, MemDevice};
    use SectorState::*;

    /// Origen sintético: cada byte del sector `l` vale `l as u8` (nunca coincide con un patrón).
    fn mem(sectors: u64, ss: u32) -> MemDevice {
        let data: Vec<u8> = (0..sectors * u64::from(ss))
            .map(|i| (i / u64::from(ss)) as u8)
            .collect();
        MemDevice::new(data, ss).unwrap()
    }

    fn faulty(sectors: u64) -> FaultyDevice<MemDevice> {
        FaultyDevice::new(mem(sectors, 512))
    }

    fn fill_policy(chunk: u32, jump: u64, read_back: bool) -> ReadPolicy {
        ReadPolicy {
            chunk_sectors: chunk,
            on_error: OnError::FillAndContinue,
            jump_sectors: jump,
            read_back,
            ..ReadPolicy::default()
        }
    }

    struct Got {
        r: Result<ReadReport>,
        data: Vec<u8>,
        states: Vec<SectorState>,
    }

    /// Ejecuta `read_range` comprobando que las entregas son ordenadas, contiguas y coherentes.
    fn collect(dev: &(impl BlockDevice + ?Sized), start: u64, count: u64, p: &ReadPolicy) -> Got {
        let ss = dev.sector_size() as usize;
        let mut data = Vec::new();
        let mut states = Vec::new();
        let mut next = start;
        let r = read_range(
            dev,
            start,
            count,
            p,
            |lba, d, s| {
                assert_eq!(lba, next, "entrega desordenada o con huecos");
                assert_eq!(d.len(), s.len() * ss);
                assert!(!s.is_empty());
                next += s.len() as u64;
                data.extend_from_slice(d);
                states.extend_from_slice(s);
                Ok(())
            },
            |_, _| true,
        );
        Got { r, data, states }
    }

    /// Garantía central: el contenido de cada sector corresponde exactamente a su estado. Los
    /// datos del origen (`l as u8`) solo aparecen con `Read`/`Recovered`; los patrones solo con
    /// `Unreadable`/`Skipped`; los ceros de `Sparse` solo con `Sparse`.
    fn assert_honest(g: &Got, start: u64, ss: usize, p: &ReadPolicy) {
        let r = g.r.as_ref().expect("lectura correcta");
        assert_eq!(g.states.len() as u64, r.sectors_requested);
        let pat = |p4: [u8; 4]| {
            let mut v = vec![0u8; ss];
            fill(&mut v, p4);
            v
        };
        let (bad, skip) = (pat(p.bad_fill), pat(p.skip_fill));
        for (i, st) in g.states.iter().enumerate() {
            let l = start + i as u64;
            let sec = &g.data[i * ss..(i + 1) * ss];
            match st {
                Read | Recovered => {
                    assert!(sec.iter().all(|&b| b == l as u8), "sector {l} alterado");
                    assert_ne!(sec, bad.as_slice());
                    assert_ne!(sec, skip.as_slice());
                }
                Unreadable => assert_eq!(sec, bad.as_slice(), "sector {l}"),
                Skipped => assert_eq!(sec, skip.as_slice(), "sector {l}"),
                Sparse => assert!(sec.iter().all(|&b| b == 0), "sector {l}"),
            }
        }
        // El mapa coincide sector a sector con los estados entregados.
        let mut expanded = Vec::new();
        let mut cur = start;
        for &(l, c, s) in r.map.runs() {
            assert_eq!(l, cur);
            expanded.extend(std::iter::repeat_n(s, c as usize));
            cur += c;
        }
        assert_eq!(expanded, g.states);
        for s in SectorState::ALL {
            let n = g.states.iter().filter(|&&x| x == s).count() as u64;
            assert_eq!(r.map.count(s), n, "recuento de {s:?}");
        }
        let trust = g.states.iter().all(|s| s.is_trustworthy());
        assert_eq!(r.map.all_trustworthy(), trust);
    }

    fn runs(g: &Got) -> Vec<(u64, u64, SectorState)> {
        g.r.as_ref().unwrap().map.runs().to_vec()
    }

    #[test]
    fn clean_read_matches_source_for_every_common_sector_size() {
        for ss in crate::COMMON_SECTOR_SIZES {
            let d = mem(100, ss);
            let p = ReadPolicy {
                chunk_sectors: 7,
                ..ReadPolicy::default()
            };
            let g = collect(&d, 10, 50, &p);
            assert_honest(&g, 10, ss as usize, &p);
            let r = g.r.as_ref().unwrap();
            assert_eq!(r.map.runs(), &[(10, 50, Read)]);
            assert_eq!(r.retry_reads, 0);
            assert!(r.map.all_trustworthy());
            let mut expect = vec![0u8; 50 * ss as usize];
            d.read_sectors(10, 50, &mut expect).unwrap();
            assert_eq!(g.data, expect);
        }
    }

    #[test]
    fn strict_policy_fails_with_exact_position() {
        let d = faulty(64).with_bad([37]);
        let g = collect(&d, 0, 64, &ReadPolicy::default());
        match g.r {
            Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (37 * 512, 512)),
            other => panic!("{other:?}"),
        }
        // Con dispersos delante y salto configurado (ignorado en modo estricto), igual.
        let d = faulty(64).with_bad([37]).with_sparse(10, 5);
        let p = ReadPolicy {
            jump_sectors: 100,
            read_back: true,
            ..ReadPolicy::default()
        };
        assert!(matches!(
            collect(&d, 0, 64, &p).r,
            Err(IoError::Read { offset: 18944, .. })
        ));
    }

    #[test]
    fn fill_marks_unreadable_sectors_and_never_reports_them_as_read() {
        let d = faulty(64).with_bad([5, 6, 7, 40]);
        let p = ReadPolicy {
            retries: 2,
            bad_fill: [0xDE, 0xAD, 0xBE, 0xEF],
            ..fill_policy(16, 0, false)
        };
        let g = collect(&d, 0, 64, &p);
        assert_honest(&g, 0, 512, &p);
        let r = g.r.as_ref().unwrap();
        assert_eq!(
            r.map.runs(),
            &[
                (0, 5, Read),
                (5, 3, Unreadable),
                (8, 32, Read),
                (40, 1, Unreadable),
                (41, 23, Read)
            ]
        );
        assert_eq!((r.map.count(Read), r.map.count(Unreadable)), (60, 4));
        assert!(!r.map.all_trustworthy());
        assert_eq!(
            r.map.untrusted_runs().collect::<Vec<_>>(),
            vec![(5, 3, Unreadable), (40, 1, Unreadable)]
        );
        // Un fallo definitivo cuesta 1 + 2 lecturas individuales por sector.
        assert_eq!(d.reads_of(5), 1 + 3);
    }

    #[test]
    fn fill_pattern_in_source_does_not_change_states() {
        // Aunque el origen contenga literalmente el patrón, el estado dice la verdad: `Read`.
        let mut data = vec![0u8; 512 * 8];
        fill(&mut data, *b"BAD!");
        let d = FaultyDevice::new(MemDevice::new(data, 512).unwrap()).with_bad([3]);
        let g = collect(&d, 0, 8, &fill_policy(8, 0, false));
        let r = g.r.unwrap();
        assert_eq!(
            r.map.runs(),
            &[(0, 3, Read), (3, 1, Unreadable), (4, 4, Read)]
        );
    }

    #[test]
    fn retries_recover_flaky_sectors() {
        let d = faulty(16).with_flaky(3, 2);
        let p = ReadPolicy {
            chunk_sectors: 8,
            retries: 2,
            ..ReadPolicy::default()
        };
        let g = collect(&d, 0, 16, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![(0, 3, Read), (3, 1, Recovered), (4, 12, Read)]
        );
        assert!(g.r.as_ref().unwrap().retry_reads >= 2);
        assert!(g.r.as_ref().unwrap().map.all_trustworthy());
        // El fallo lo consume la lectura del bloque y la primera lectura individual funciona:
        // el sector es `Read` (el primer intento propio no necesitó reintento).
        let d = faulty(16).with_flaky(3, 1);
        assert_eq!(runs(&collect(&d, 0, 16, &p)), vec![(0, 16, Read)]);
        // Con 4 fallos programados y 1 + 2 intentos, el sector sigue fallando.
        let d = faulty(16).with_flaky(3, 4);
        assert!(collect(&d, 0, 16, &p).r.is_err());
        let d = faulty(16).with_flaky(3, 4);
        let pf = ReadPolicy {
            retries: 2,
            ..fill_policy(8, 0, false)
        };
        let g = collect(&d, 0, 16, &pf);
        assert_honest(&g, 0, 512, &pf);
        assert_eq!(g.r.unwrap().map.count(Unreadable), 1);
    }

    #[test]
    fn jump_without_read_back_contiguous_damage() {
        let d = faulty(100).with_bad(20..=24);
        let p = fill_policy(1, 10, false);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 10, Skipped),
                (31, 69, Read)
            ]
        );
        // Con bloques de 1 sector, ningún sector saltado se ha leído.
        assert!(d.sectors_touched(21..31).is_empty());
        assert!(!g.r.unwrap().map.all_trustworthy());
    }

    #[test]
    fn jump_with_read_back_contiguous_damage() {
        let d = faulty(100).with_bad(20..=24);
        let p = fill_policy(16, 10, true);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 3, Skipped),
                (24, 1, Unreadable),
                (25, 75, Read)
            ]
        );
        // 21–23 solo los tocó la lectura del bloque 16–31 que falló; nunca individualmente.
        for l in 21..24 {
            assert_eq!(d.reads_of(l), 1, "sector {l}");
        }
        // La lectura hacia atrás es de un intento: bloque + 1 lectura individual.
        assert_eq!(d.reads_of(24), 2);
    }

    #[test]
    fn jump_with_scattered_damage_and_repeated_jumps() {
        let d = faulty(100).with_bad([20, 23, 27, 45]);
        let p = fill_policy(1, 10, true);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 6, Skipped),
                (27, 1, Unreadable),
                (28, 17, Read),
                (45, 1, Unreadable),
                (46, 54, Read)
            ]
        );
        // Sin lectura hacia atrás: todo el salto queda `Skipped` (el 23 y el 27 quedan ocultos
        // dentro del salto, pero marcados como no fiables).
        let d = faulty(100).with_bad([20, 23, 27, 45]);
        let p = fill_policy(1, 10, false);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 10, Skipped),
                (31, 14, Read),
                (45, 1, Unreadable),
                (46, 10, Skipped),
                (56, 44, Read)
            ]
        );
        let r = g.r.unwrap();
        assert_eq!((r.map.count(Unreadable), r.map.count(Skipped)), (2, 20));
        assert!(d.sectors_touched(21..31).is_empty());
        assert!(d.sectors_touched(46..56).is_empty());
    }

    #[test]
    fn jump_is_bounded_by_the_end_of_the_range() {
        for rb in [false, true] {
            let d = faulty(64).with_bad([60]);
            let p = fill_policy(1, 1000, rb);
            let g = collect(&d, 0, 64, &p);
            assert_honest(&g, 0, 512, &p);
            let tail = if rb { Read } else { Skipped };
            assert_eq!(
                runs(&g),
                vec![(0, 60, Read), (60, 1, Unreadable), (61, 3, tail)]
            );
            // El rango no es todo el dispositivo: el salto no pasa del final del rango.
            let d = faulty(64).with_bad([55]);
            let g = collect(&d, 10, 50, &p);
            assert_honest(&g, 10, 512, &p);
            assert_eq!(
                runs(&g),
                vec![(10, 45, Read), (55, 1, Unreadable), (56, 4, tail)]
            );
            assert!(d.sectors_touched(60..64).is_empty());
            // Ilegible en el último sector: no hay nada que saltar.
            let d = faulty(64).with_bad([63]);
            let g = collect(&d, 0, 64, &p);
            assert_eq!(runs(&g), vec![(0, 63, Read), (63, 1, Unreadable)]);
        }
    }

    #[test]
    fn sparse_regions_are_not_read_and_are_zeros() {
        let d = faulty(300).with_sparse(100, 50).with_sparse(290, 10);
        let p = fill_policy(16, 0, false);
        let g = collect(&d, 0, 300, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 100, Read),
                (100, 50, Sparse),
                (150, 140, Read),
                (290, 10, Sparse)
            ]
        );
        let r = g.r.unwrap();
        assert_eq!(r.map.count(Sparse), 60);
        assert!(r.map.all_trustworthy());
        assert!(d.sectors_touched(100..150).is_empty());
        assert!(d.sectors_touched(290..300).is_empty());
    }

    #[test]
    fn sparse_inside_a_jump_stays_sparse() {
        // Salto 21–35 con 25–29 dispersos.
        let d = faulty(100).with_bad([20]).with_sparse(25, 5);
        let p = fill_policy(1, 15, false);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 4, Skipped),
                (25, 5, Sparse),
                (30, 6, Skipped),
                (36, 64, Read)
            ]
        );
        assert!(d.sectors_touched(21..36).is_empty());
        // Con lectura hacia atrás todo el salto es legible; los dispersos no se leen.
        let d = faulty(100).with_bad([20, 27]).with_sparse(25, 5);
        let p = fill_policy(1, 15, true);
        let g = collect(&d, 0, 100, &p);
        assert_honest(&g, 0, 512, &p);
        assert_eq!(
            runs(&g),
            vec![
                (0, 20, Read),
                (20, 1, Unreadable),
                (21, 4, Read),
                (25, 5, Sparse),
                (30, 70, Read)
            ]
        );
        assert!(d.sectors_touched(25..30).is_empty());
        // El 27 es ilegible pero disperso: nunca se lee, así que no puede fallar.
        assert_eq!(d.reads_of(27), 0);
    }

    /// Dispositivo virtual de `n` sectores de 512 bytes sin datos en memoria (sector `l` = `l as u8`).
    struct Virtual(u64);
    impl BlockDevice for Virtual {
        fn size(&self) -> u64 {
            self.0 * 512
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            crate::check_bounds(offset, buf.len(), self.size())?;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = ((offset + i as u64) / 512) as u8;
            }
            Ok(())
        }
    }

    #[test]
    fn read_back_stops_at_buffer_limit_and_leaves_rest_skipped() {
        // Salto de 50 000 sectores; el búfer hacia atrás admite 8 MiB = 16 384 sectores.
        let rb = (READ_BACK_MAX_BYTES / 512) as u64;
        let d = FaultyDevice::new(Virtual(100_000)).with_bad([10]);
        let p = fill_policy(8, 50_000, true);
        let r = read_range(&d, 0, 100_000, &p, |_, _, _| Ok(()), |_, _| true).unwrap();
        let jend = 11 + 50_000;
        assert_eq!(
            r.map.runs(),
            &[
                (0, 10, Read),
                (10, 1, Unreadable),
                (11, jend - rb - 11, Skipped),
                (jend - rb, 100_000 - (jend - rb), Read)
            ]
        );
        assert!(d.sectors_touched(16..jend - rb).is_empty());
    }

    #[test]
    fn huge_jump_uses_bounded_memory() {
        let total = 4u64 << 20; // 2 GiB virtuales
        let d = FaultyDevice::new(Virtual(total)).with_bad([10]);
        let rb = (READ_BACK_MAX_BYTES / 512) as u64;
        let p = fill_policy(4096, u64::MAX, true);
        let mut max_call = 0usize;
        let mut calls = 0u64;
        let r = read_range(
            &d,
            0,
            total,
            &p,
            |_, data, _| {
                max_call = max_call.max(data.len());
                calls += 1;
                Ok(())
            },
            |_, _| true,
        )
        .unwrap();
        assert!(max_call <= 4096 * 512);
        assert!(calls >= total / 4096);
        assert_eq!(
            r.map.runs(),
            &[
                (0, 10, Read),
                (10, 1, Unreadable),
                (11, total - 11 - rb, Skipped),
                (total - rb, rb, Read)
            ]
        );
        // Solo se leyeron el primer bloque y la ventana de lectura hacia atrás.
        assert!(d.sectors_touched(4096..total - rb).is_empty());
        // `chunk_sectors` desmesurado se acota a MAX_READ_REQUEST.
        let p = fill_policy(u32::MAX, u64::MAX, false);
        let mut max_call = 0usize;
        read_range(
            &d,
            0,
            total,
            &p,
            |_, data, _| {
                max_call = max_call.max(data.len());
                Ok(())
            },
            |_, _| true,
        )
        .unwrap();
        assert!(max_call <= MAX_READ_REQUEST);
    }

    #[test]
    fn cancellation_and_invalid_ranges() {
        let d = mem(1000, 512);
        let p = ReadPolicy {
            chunk_sectors: 10,
            ..ReadPolicy::default()
        };
        let mut calls = 0;
        let r = read_range(
            &d,
            0,
            1000,
            &p,
            |_, _, _| Ok(()),
            |done, _| {
                calls += 1;
                done < 300
            },
        );
        assert!(matches!(r, Err(IoError::Cancelled)));
        assert!(calls <= 32);
        assert!(matches!(
            collect(&d, 990, 20, &p).r,
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            collect(&d, 0, 0, &p).r,
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            collect(&d, u64::MAX, 2, &p).r,
            Err(IoError::InvalidRange { .. })
        ));
        // Cancelación en medio de un salto largo.
        let d = faulty(10_000).with_bad([5]);
        let r = read_range(
            &d,
            0,
            10_000,
            &fill_policy(10, u64::MAX, false),
            |_, _, _| Ok(()),
            |done, _| done < 5_000,
        );
        assert!(matches!(r, Err(IoError::Cancelled)));
        // Un error del consumidor se propaga.
        let r = read_range(
            &d,
            0,
            100,
            &fill_policy(10, 0, false),
            |_, _, _| Err(IoError::Cancelled),
            |_, _| true,
        );
        assert!(matches!(r, Err(IoError::Cancelled)));
    }

    #[test]
    fn incoherent_allocation_is_rejected() {
        struct Liar;
        impl BlockDevice for Liar {
            fn size(&self) -> u64 {
                512 * 10
            }
            fn sector_size(&self) -> u32 {
                512
            }
            fn read_at(&self, _: u64, b: &mut [u8]) -> Result<()> {
                b.fill(1);
                Ok(())
            }
            fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
                Ok(vec![(lba, count - 1, Allocation::Data)])
            }
        }
        assert!(matches!(
            collect(&Liar, 0, 10, &ReadPolicy::default()).r,
            Err(IoError::Corrupt { .. })
        ));
    }

    #[test]
    fn validity_map_merges_orders_and_counts() {
        let mut m = ValidityMap::new();
        m.push(0, 5, Read);
        m.push(5, 3, Read);
        m.push(8, 0, Unreadable);
        m.push(8, 1, Unreadable);
        m.push(9, 2, Recovered);
        m.push(20, 4, Read); // hueco permitido: no se fusiona con nada
        m.push(24, 1, Read);
        assert_eq!(
            m.runs(),
            &[
                (0, 8, Read),
                (8, 1, Unreadable),
                (9, 2, Recovered),
                (20, 5, Read)
            ]
        );
        assert_eq!(m.count(Read), 13);
        assert_eq!(m.total(), 16);
        assert!(!m.all_trustworthy());
        assert!(!m.truncated());
        let mut ok = ValidityMap::new();
        ok.push(0, 3, Sparse);
        ok.push(3, 3, Recovered);
        assert!(ok.all_trustworthy());
        let mut sk = ValidityMap::new();
        sk.push(7, 1, Skipped);
        assert!(!sk.all_trustworthy());
    }

    #[test]
    #[should_panic(expected = "desordenado")]
    fn validity_map_rejects_out_of_order_push() {
        let mut m = ValidityMap::new();
        m.push(10, 5, Read);
        m.push(12, 1, Unreadable);
    }

    #[test]
    #[should_panic(expected = "desborda")]
    fn validity_map_rejects_overflow() {
        ValidityMap::new().push(u64::MAX, 2, Read);
    }

    #[test]
    fn validity_map_truncates_runs_but_keeps_exact_counts() {
        let mut m = ValidityMap::new();
        let n = MAX_MAP_RUNS as u64 + 10;
        for i in 0..n {
            let st = if i % 2 == 0 { Read } else { Unreadable };
            m.push(i, 1, st);
        }
        assert_eq!(m.runs().len(), MAX_MAP_RUNS);
        assert!(m.truncated());
        assert_eq!(m.count(Read) + m.count(Unreadable), n);
        assert_eq!(m.count(Unreadable), n / 2);
        assert!(!m.all_trustworthy());
    }

    #[test]
    fn read_sectors_mapped_for_every_common_sector_size() {
        for ss in crate::COMMON_SECTOR_SIZES {
            let d = FaultyDevice::new(mem(64, ss))
                .with_bad([9])
                .with_sparse(12, 2);
            let p = fill_policy(4, 0, false);
            let (data, states) = read_sectors_mapped(&d, 8, 8, &p).unwrap();
            assert_eq!(data.len(), 8 * ss as usize);
            assert_eq!(
                states,
                vec![Read, Unreadable, Read, Read, Sparse, Sparse, Read, Read]
            );
            let g = Got {
                r: Ok(ReadReport {
                    sectors_requested: 8,
                    retry_reads: 0,
                    map: {
                        let mut m = ValidityMap::new();
                        for (i, s) in states.iter().enumerate() {
                            m.push(8 + i as u64, 1, *s);
                        }
                        m
                    },
                }),
                data,
                states,
            };
            assert_honest(&g, 8, ss as usize, &p);
            // Estricto: el error sale con su posición.
            assert!(matches!(
                read_sectors_mapped(&d, 8, 8, &ReadPolicy::default()),
                Err(IoError::Read { offset, .. }) if offset == 9 * u64::from(ss)
            ));
            assert!(matches!(
                read_sectors_mapped(&d, 60, 8, &p),
                Err(IoError::InvalidRange { .. })
            ));
        }
        struct Huge;
        impl BlockDevice for Huge {
            fn size(&self) -> u64 {
                u64::MAX
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> Result<()> {
                unreachable!()
            }
        }
        assert!(matches!(
            read_sectors_mapped(&Huge, 0, 1 << 40, &ReadPolicy::default()),
            Err(IoError::RequestTooLarge { .. })
        ));
    }
}
