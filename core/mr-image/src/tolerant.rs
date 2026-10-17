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

use mr_io::IoError;
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

/// Elementos implicados en algún solape.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Overlaps {
    /// Entradas (índices) cuya zona solapa otra zona; ordenadas y sin repetir.
    pub entries: Vec<u64>,
    /// Etiquetas de las zonas fijas solapadas por otra zona; ordenadas y sin repetir.
    pub tags: Vec<u64>,
}

#[derive(Clone, Copy)]
enum Who {
    Entry(u64),
    Zone(Option<u64>),
}

/// Busca las entradas cuya zona `span(e) = [inicio, fin)` del fichero solapa la de otra entrada
/// o una zona fija. Se recorren las zonas por orden de inicio formando grupos conexos (cada zona
/// nueva que empieza antes del final máximo del grupo se le une): en un grupo de dos o más
/// elementos **todos** solapan al menos a otro, así que todos se invalidan. Las zonas vacías se
/// ignoran. Los solapes entre dos zonas fijas sin etiqueta no se informan (son estructuras del
/// contenedor y el lector las valida antes, con `Corrupt` en ambos modos).
///
/// Memoria: la lista de entradas (8 bytes por entrada) y la de zonas fijas.
pub(crate) fn overlaps(
    mut entries: Vec<u64>,
    span: impl Fn(u64) -> (u64, u64),
    zones: &[Zone],
) -> Overlaps {
    entries.retain(|&e| {
        let (a, b) = span(e);
        a < b
    });
    entries.sort_unstable_by_key(|&e| span(e).0);
    let mut zones: Vec<Zone> = zones.iter().copied().filter(|z| z.start < z.end).collect();
    zones.sort_by_key(|z| z.start);

    let mut out = Overlaps::default();
    let mut group: Vec<Who> = Vec::new();
    let mut max_end = 0u64;
    let flush = |group: &mut Vec<Who>, out: &mut Overlaps| {
        if group.len() >= 2 {
            for w in group.iter() {
                match *w {
                    Who::Entry(e) => out.entries.push(e),
                    Who::Zone(Some(t)) => out.tags.push(t),
                    Who::Zone(None) => {}
                }
            }
        }
        group.clear();
    };
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
        if group.is_empty() || a >= max_end {
            flush(&mut group, &mut out);
            max_end = b;
        } else {
            max_end = max_end.max(b);
        }
        group.push(who);
    }
    flush(&mut group, &mut out);
    out.entries.sort_unstable();
    out.entries.dedup();
    out.tags.sort_unstable();
    out.tags.dedup();
    out
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

/// Primer problema encontrado (para el mensaje de `Corrupt` en modo estricto) y lista de
/// entradas descartadas.
#[derive(Debug, Default)]
pub(crate) struct Damage {
    first: Option<String>,
    bad: Vec<u64>,
}

impl Damage {
    /// Registra la entrada `index` como inválida por `why` (solo se guarda el primer motivo).
    pub(crate) fn add(&mut self, index: u64, why: impl FnOnce() -> String) {
        if self.first.is_none() {
            self.first = Some(why());
        }
        self.bad.push(index);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bad.is_empty()
    }

    /// Primer motivo registrado.
    pub(crate) fn first(&self) -> Option<&str> {
        self.first.as_deref()
    }

    /// Entradas descartadas, ordenadas y sin repetir.
    pub(crate) fn into_sorted(mut self) -> Vec<u64> {
        self.bad.sort_unstable();
        self.bad.dedup();
        self.bad
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

    fn run(spans: &[(u64, u64)], zones: &[Zone]) -> Overlaps {
        let entries = (0..spans.len() as u64).collect();
        overlaps(entries, |e| spans[e as usize], zones)
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
    fn damage_keeps_first_reason() {
        let mut d = Damage::default();
        assert!(d.is_empty());
        d.add(5, || "a".into());
        d.add(2, || "b".into());
        d.add(5, || "c".into());
        assert_eq!(d.first(), Some("a"));
        assert_eq!(d.into_sorted(), vec![2, 5]);
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
