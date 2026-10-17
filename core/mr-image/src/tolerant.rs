//! Apoyo común del **modo tolerante** de los contenedores (ADR-020).
//!
//! En modo estricto (por defecto) una entrada de asignación inválida hace que la apertura falle con
//! `IoError::Corrupt`. En modo tolerante (activación explícita) cada lector registra las entradas
//! inválidas y entrega sus bloques o granos como **no fiables**:
//! - `read_at` sobre un bloque descartado devuelve `IoError::Read` con el rango virtual afectado,
//!   de modo que el lector tolerante (`mr_io::resilient`) lo marca `Unreadable` con el patrón de
//!   relleno y las lecturas estrictas y los hashes fallan;
//! - `allocation` lo informa como `Allocation::Data` (hay que «leerlo» para descubrir que falla;
//!   nunca se presenta como disperso ni como ceros).
//!
//! La correspondencia entre una entrada y su zona del fichero solo se acepta si es **inequívoca**:
//! la zona está entera dentro del fichero, no solapa ninguna estructura del contenedor ni la zona
//! de otra entrada y el estado de la entrada es válido. Cuando dos o más zonas se solapan entre sí
//! se invalidan **todas** las implicadas: nunca se elige una de las interpretaciones.

use crate::budget::Budget;
use mr_io::{IoError, Result};
use std::io;

/// Zona fija del fichero: estructura del contenedor (`tag = None`) o tabla secundaria que se puede
/// invalidar si algo la solapa (`tag = Some(índice)`, p. ej. una tabla de granos de VMDK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Zone {
    pub start: u64,
    pub end: u64,
    pub tag: Option<u64>,
}

impl Zone {
    /// Estructura del contenedor `[start, start + len)` (sin desbordar).
    pub(crate) fn fixed(start: u64, len: u64) -> Self {
        Self {
            start,
            end: start.saturating_add(len),
            tag: None,
        }
    }
}

/// Elemento implicado en un solape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hit {
    /// Entrada (índice) cuya zona solapa otra zona.
    Entry(u32),
    /// Etiqueta de una zona fija solapada por otra zona.
    Tag(u64),
}

#[derive(Clone, Copy)]
enum Who {
    Entry(u32),
    Zone(Option<u64>),
}

/// Busca las entradas cuya zona `span(e) = [inicio, fin)` del fichero solapa la de otra entrada
/// o una zona fija. Se recorren las zonas por orden de inicio formando grupos conexos (cada zona
/// nueva que empieza antes del final máximo del grupo se le une): en un grupo de dos o más
/// elementos **todos** solapan al menos a otro, así que todos se invalidan. Las zonas vacías se
/// ignoran. Los solapes entre dos zonas fijas sin etiqueta no se informan (son estructuras del
/// contenedor y el lector las valida antes, con `Corrupt` en ambos modos).
///
/// Los grupos se procesan en flujo: cada elemento implicado se entrega **una sola vez** a `hit`,
/// por orden de inicio en el fichero, en cuanto su grupo tiene dos elementos. Si `hit` devuelve
/// un error, el recorrido se detiene y lo devuelve (el modo estricto falla en el primer solape).
///
/// Memoria: ninguna reserva propia. `entries` y `zones` se filtran y ordenan en su sitio y de
/// cada grupo solo se guarda el primer elemento mientras el grupo no tiene un segundo. Los
/// índices son `u32`: ninguna tabla acotada por [`crate::MAX_TABLE_BYTES`] tiene más de 2^26
/// entradas.
pub(crate) fn overlaps(
    entries: &mut Vec<u32>,
    span: impl Fn(u32) -> (u64, u64),
    zones: &mut Vec<Zone>,
    mut hit: impl FnMut(Hit) -> Result<()>,
) -> Result<()> {
    entries.retain(|&e| {
        let (a, b) = span(e);
        a < b
    });
    entries.sort_unstable_by_key(|&e| span(e).0);
    zones.retain(|z| z.start < z.end);
    zones.sort_by_key(|z| z.start);

    let mut emit = |w: Who| match w {
        Who::Entry(e) => hit(Hit::Entry(e)),
        Who::Zone(Some(t)) => hit(Hit::Tag(t)),
        Who::Zone(None) => Ok(()),
    };
    // Primer elemento del grupo en curso, mientras el grupo no tiene un segundo.
    let mut pending: Option<Who> = None;
    let mut in_group = false;
    let mut max_end = 0u64;
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        let next_entry = entries.get(i).map(|&e| (span(e), e));
        let next_zone = zones.get(j);
        let (who, a, b) = match (next_entry, next_zone) {
            (None, None) => break,
            (Some(((a, b), e)), Some(z)) if a < z.start => {
                i += 1;
                (Who::Entry(e), a, b)
            }
            (Some(((a, b), e)), None) => {
                i += 1;
                (Who::Entry(e), a, b)
            }
            (_, Some(z)) => {
                j += 1;
                (Who::Zone(z.tag), z.start, z.end)
            }
        };
        if !in_group || a >= max_end {
            // Empieza un grupo nuevo.
            in_group = true;
            pending = Some(who);
            max_end = b;
            continue;
        }
        max_end = max_end.max(b);
        if let Some(first) = pending.take() {
            emit(first)?;
        }
        emit(who)?;
    }
    Ok(())
}

/// Error de lectura de un bloque o grano descartado en modo tolerante. Lleva el rango **virtual**
/// pedido, como cualquier otro error de lectura, para que el lector tolerante lo localice.
pub(crate) fn discarded_read(
    format: &str,
    unit: &str,
    index: u64,
    offset: u64,
    len: usize,
) -> IoError {
    IoError::Read {
        offset,
        len,
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{format}: {unit} {index} descartado en modo tolerante (su entrada de asignación no es válida o es ambigua)"
            ),
        ),
    }
}

/// Sufijo de la descripción de un contenedor abierto en modo tolerante.
pub(crate) fn describe_suffix(discarded: u64, singular: &str, plural: &str) -> String {
    if discarded == 1 {
        format!("; modo tolerante: 1 {singular} descartado (no fiable)")
    } else {
        format!("; modo tolerante: {discarded} {plural} descartados (no fiables)")
    }
}

/// Registro de las entradas de asignación inválidas.
///
/// En modo estricto no guarda nada: la primera entrada inválida hace fallar la apertura con
/// `Corrupt` y su motivo. En modo tolerante guarda un bit por entrada (reservado en el
/// [`Budget`]) y el número de entradas marcadas; nunca una copia de las entradas.
#[derive(Debug)]
pub(crate) struct Damage {
    format: &'static str,
    tolerant: bool,
    bits: Vec<u64>,
    count: u64,
}

impl Damage {
    /// Registro para las entradas `0..entries`.
    pub(crate) fn new(
        format: &'static str,
        entries: u64,
        tolerant: bool,
        budget: &mut Budget,
    ) -> Result<Self> {
        let mut bits = Vec::new();
        if tolerant {
            let words = entries.div_ceil(64);
            bits = budget.vec(words, "registro de entradas descartadas")?;
            bits.resize(bits.capacity(), 0);
        }
        Ok(Self {
            format,
            tolerant,
            bits,
            count: 0,
        })
    }

    /// Registra la entrada `index` como inválida por `why`. En modo estricto devuelve `Corrupt`
    /// con ese motivo (la apertura falla en la primera entrada inválida).
    pub(crate) fn add(&mut self, index: u64, why: impl FnOnce() -> String) -> Result<()> {
        if !self.tolerant {
            return Err(IoError::Corrupt {
                format: self.format,
                detail: why(),
            });
        }
        let word = usize::try_from(index / 64)
            .ok()
            .and_then(|w| self.bits.get_mut(w));
        if let Some(word) = word {
            let bit = 1u64 << (index % 64);
            if *word & bit == 0 {
                *word |= bit;
                self.count += 1;
            }
        }
        Ok(())
    }

    /// `true` si la entrada `index` está registrada.
    pub(crate) fn contains(&self, index: u64) -> bool {
        usize::try_from(index / 64)
            .ok()
            .and_then(|w| self.bits.get(w))
            .is_some_and(|&word| word & (1u64 << (index % 64)) != 0)
    }

    /// Número de entradas registradas (sin repetir).
    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    /// Entradas registradas, en orden creciente.
    pub(crate) fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.bits.iter().enumerate().flat_map(|(w, &word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let bit = u64::from(rest.trailing_zeros());
                rest &= rest - 1;
                Some(w as u64 * 64 + bit)
            })
        })
    }

    /// Libera el registro y descuenta su reserva del presupuesto.
    pub(crate) fn release(self, budget: &mut Budget) {
        budget.release(self.bits);
    }
}

/// Comprobaciones comunes de las pruebas del modo tolerante de cada formato.
#[cfg(test)]
pub(crate) mod check {
    use mr_io::resilient::{read_range, OnError, ReadPolicy};
    use mr_io::{Allocation, BlockDevice, IoError, SectorState};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::time::SystemTime;

    /// Hash, tamaño y fecha de modificación de un fichero (para comprobar que no cambia).
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct Snapshot {
        sha256: Vec<u8>,
        len: u64,
        modified: SystemTime,
    }

    impl Snapshot {
        pub(crate) fn of(p: &Path) -> Self {
            let data = std::fs::read(p).unwrap();
            let meta = std::fs::metadata(p).unwrap();
            Self {
                sha256: Sha256::digest(&data).to_vec(),
                len: meta.len(),
                modified: meta.modified().unwrap(),
            }
        }
    }

    /// `true` si el sector `lba` está en alguno de los tramos `(lba, nº de sectores)`.
    fn inside(ranges: &[(u64, u64)], lba: u64) -> bool {
        ranges.iter().any(|&(a, n)| lba >= a && lba < a + n)
    }

    /// Lectura completa con el lector tolerante y comparación **sector a sector** contenido ↔
    /// estado: los sectores de `bad` son `Unreadable` con el patrón de relleno; todos los demás
    /// son `Read` o `Sparse` e idénticos a `expected` (la imagen sin dañar). Comprueba además que
    /// la lectura estricta y la lectura que aborta ante errores fallan si hay sectores dañados, y
    /// que `allocation` informa los sectores dañados como `Data`.
    pub(crate) fn tolerant_read_matches(
        dev: &dyn BlockDevice,
        expected: &[u8],
        bad: &[(u64, u64)],
        what: &str,
    ) {
        assert_eq!(dev.size() as usize, expected.len(), "{what}: tamaño");
        let ss = dev.sector_size() as usize;
        let total = dev.sector_count();
        let policy = ReadPolicy {
            chunk_sectors: 16,
            on_error: OnError::FillAndContinue,
            ..ReadPolicy::default()
        };
        let mut data = Vec::new();
        let mut states = Vec::new();
        let report = read_range(
            dev,
            0,
            total,
            &policy,
            |_, d, s| {
                data.extend_from_slice(d);
                states.extend_from_slice(s);
                Ok(())
            },
            |_, _| true,
        )
        .unwrap_or_else(|e| panic!("{what}: lectura tolerante: {e}"));
        let mut fill = vec![0u8; ss];
        for (i, b) in fill.iter_mut().enumerate() {
            *b = policy.bad_fill[i % 4];
        }
        let mut unreadable = 0u64;
        for lba in 0..total {
            let i = lba as usize;
            let got = &data[i * ss..(i + 1) * ss];
            let st = states[i];
            if inside(bad, lba) {
                assert_eq!(st, SectorState::Unreadable, "{what}: LBA {lba} dañado");
                assert_eq!(got, &fill[..], "{what}: LBA {lba} sin el patrón de relleno");
                unreadable += 1;
            } else {
                assert!(
                    matches!(st, SectorState::Read | SectorState::Sparse),
                    "{what}: LBA {lba} sano con estado {st:?}"
                );
                assert_eq!(
                    got,
                    &expected[i * ss..(i + 1) * ss],
                    "{what}: LBA {lba} distinto de la imagen sin dañar"
                );
            }
        }
        assert_eq!(report.map.count(SectorState::Unreadable), unreadable);
        assert_eq!(report.map.all_trustworthy(), bad.is_empty(), "{what}");

        // Lecturas estrictas: fallan con un error de lectura en cuanto tocan un sector dañado.
        let mut all = vec![0u8; expected.len()];
        let strict = dev.read_at(0, &mut all);
        if bad.is_empty() {
            strict.unwrap();
            assert!(all == expected, "{what}: lectura estricta");
        } else {
            assert!(
                matches!(strict, Err(IoError::Read { .. })),
                "{what}: la lectura estricta debía fallar: {strict:?}"
            );
            let fail = ReadPolicy {
                on_error: OnError::Fail,
                ..policy.clone()
            };
            assert!(
                read_range(dev, 0, total, &fail, |_, _, _| Ok(()), |_, _| true).is_err(),
                "{what}: la lectura que aborta ante errores debía fallar"
            );
            for &(a, n) in bad {
                for (l, c, k) in dev.allocation(a, n).unwrap() {
                    assert_eq!(k, Allocation::Data, "{what}: LBA {l}+{c} dañado no es Data");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Elementos implicados en algún solape, ordenados.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Overlaps {
        entries: Vec<u32>,
        tags: Vec<u64>,
    }

    fn run(spans: &[(u64, u64)], zones: &[Zone]) -> Overlaps {
        let mut entries = (0..spans.len() as u32).collect();
        let mut zones = zones.to_vec();
        let mut out = Overlaps::default();
        overlaps(
            &mut entries,
            |e| spans[e as usize],
            &mut zones,
            |h| {
                match h {
                    Hit::Entry(e) => out.entries.push(e),
                    Hit::Tag(t) => out.tags.push(t),
                }
                Ok(())
            },
        )
        .unwrap();
        // Cada elemento se entrega una sola vez.
        let delivered = (out.entries.len(), out.tags.len());
        out.entries.sort_unstable();
        out.entries.dedup();
        out.tags.sort_unstable();
        out.tags.dedup();
        assert_eq!(delivered, (out.entries.len(), out.tags.len()), "repetidos");
        out
    }

    #[test]
    fn disjoint_and_touching_spans_do_not_overlap() {
        let r = run(&[(0, 10), (10, 20), (30, 40)], &[Zone::fixed(20, 10)]);
        assert_eq!(r, Overlaps::default());
    }

    #[test]
    fn every_member_of_an_overlapping_group_is_invalidated() {
        // 0 y 1 se solapan; 2 está contenido en 1; 3 es idéntico a 4 (duplicado); 5 aislado.
        let r = run(
            &[
                (0, 10),
                (5, 30),
                (12, 13),
                (100, 110),
                (100, 110),
                (200, 210),
            ],
            &[],
        );
        assert_eq!(r.entries, vec![0, 1, 2, 3, 4]);
        // Una zona contenida en otra mayor también cuenta, aunque no sea la que acaba más tarde.
        let r = run(&[(0, 100), (10, 20), (30, 40)], &[]);
        assert_eq!(r.entries, vec![0, 1, 2]);
    }

    #[test]
    fn zones_invalidate_entries_and_tagged_zones() {
        let zones = [
            Zone::fixed(0, 512),
            Zone {
                start: 1000,
                end: 1100,
                tag: Some(7),
            },
            Zone {
                start: 5000,
                end: 5100,
                tag: Some(8),
            },
        ];
        let r = run(&[(100, 200), (1050, 1200), (2000, 2100), (0, 0)], &zones);
        assert_eq!(r.entries, vec![0, 1]);
        assert_eq!(r.tags, vec![7]);
        // Estructuras que se solapan entre sí: no hay entradas implicadas.
        let r = run(&[], &[Zone::fixed(0, 10), Zone::fixed(5, 10)]);
        assert_eq!(r, Overlaps::default());
    }

    #[test]
    fn overlaps_streams_groups_and_stops_at_the_first_error() {
        // Un millón de zonas idénticas: cada una se entrega una vez, sin acumular el grupo.
        let n = 1_000_000u32;
        let mut entries: Vec<u32> = (0..n).collect();
        let mut seen = 0u32;
        overlaps(
            &mut entries,
            |_| (4096, 8192),
            &mut Vec::new(),
            |_| {
                seen += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, n);
        // Si `hit` falla (modo estricto), el recorrido se detiene en ese elemento.
        let mut entries: Vec<u32> = (0..n).collect();
        let mut calls = 0u32;
        let r = overlaps(
            &mut entries,
            |_| (4096, 8192),
            &mut Vec::new(),
            |_| {
                calls += 1;
                Err(IoError::Corrupt {
                    format: "PRUEBA",
                    detail: "solape".into(),
                })
            },
        );
        assert!(matches!(r, Err(IoError::Corrupt { .. })), "{r:?}");
        assert_eq!(calls, 1);
    }

    #[test]
    fn damage_is_a_bitset_in_tolerant_mode() {
        let mut budget = Budget::new("PRUEBA");
        budget.add_tables(1024);
        let mut d = Damage::new("PRUEBA", 130, true, &mut budget).unwrap();
        assert_eq!(d.count(), 0);
        d.add(5, || "a".into()).unwrap();
        d.add(129, || "b".into()).unwrap();
        d.add(2, || "c".into()).unwrap();
        d.add(5, || "d".into()).unwrap();
        assert_eq!(d.count(), 3);
        assert!(d.contains(5) && d.contains(129) && !d.contains(6));
        assert_eq!(d.iter().collect::<Vec<_>>(), vec![2, 5, 129]);
        d.release(&mut budget);
    }

    #[test]
    fn damage_fails_at_the_first_entry_in_strict_mode() {
        let mut budget = Budget::new("PRUEBA");
        // Sin reserva: en modo estricto no se guarda nada, ni siquiera para 2^40 entradas.
        let mut d = Damage::new("PRUEBA", 1 << 40, false, &mut budget).unwrap();
        match d.add(7, || "primer motivo".into()) {
            Err(IoError::Corrupt { format, detail }) => {
                assert_eq!((format, detail.as_str()), ("PRUEBA", "primer motivo"));
            }
            r => panic!("{r:?}"),
        }
        assert_eq!(d.count(), 0);
        assert!(!d.contains(7));
    }

    #[test]
    fn discarded_read_is_a_read_error_with_virtual_range() {
        match discarded_read("VHD", "bloque", 3, 4096, 512) {
            IoError::Read {
                offset,
                len,
                source,
            } => {
                assert_eq!((offset, len), (4096, 512));
                assert_eq!(source.kind(), io::ErrorKind::InvalidData);
                assert!(source.to_string().contains("modo tolerante"));
            }
            e => panic!("{e:?}"),
        }
        assert!(describe_suffix(1, "bloque", "bloques").contains("1 bloque descartado"));
        assert!(describe_suffix(3, "bloque", "bloques").contains("3 bloques descartados"));
    }
}
