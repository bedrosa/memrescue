//! Lectura de rangos de sectores tolerante a errores, con progreso y cancelación.
//!
//! Estrategia: se lee por bloques; si un bloque falla, se relee **sector a sector** con reintentos.
//! Los sectores que siguen fallando se tratan según [`OnError`]: abortar con la posición exacta, o
//! rellenarlos con un patrón y continuar registrándolos en el informe. Nunca se presentan datos
//! inventados como leídos: el informe enumera exactamente los sectores rellenados.

use crate::{sector_span, BlockDevice, IoError, Result, MAX_READ_REQUEST};

/// Qué hacer con un sector que no se puede leer tras los reintentos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Abortar la operación (p. ej. hashes forenses: un hash con relleno no sería válido).
    Fail,
    /// Rellenar con el patrón y continuar (p. ej. verificación, clonado).
    FillAndContinue,
}

#[derive(Debug, Clone)]
pub struct ReadPolicy {
    /// Sectores por petición de lectura normal.
    pub chunk_sectors: u32,
    /// Reintentos adicionales por sector tras un fallo.
    pub retries: u32,
    pub on_error: OnError,
    /// Patrón de 4 bytes con que se rellenan los sectores ilegibles.
    pub bad_fill: [u8; 4],
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            chunk_sectors: 2048,
            retries: 1,
            on_error: OnError::Fail,
            bad_fill: *b"BAD!",
        }
    }
}

/// Máximo de rangos dañados que se guardan en memoria (el recuento total siempre es exacto).
pub const MAX_BAD_RANGES: usize = 100_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub sectors_requested: u64,
    pub sectors_read_ok: u64,
    pub sectors_bad: u64,
    /// Rangos ilegibles `(lba, nº de sectores)`, fusionados y en orden.
    pub bad_ranges: Vec<(u64, u64)>,
    /// `true` si se alcanzó [`MAX_BAD_RANGES`] y la lista está incompleta.
    pub bad_ranges_truncated: bool,
    /// Lecturas repetidas por errores (bloques releídos sector a sector y reintentos).
    pub retry_reads: u64,
}

impl ReadReport {
    fn add_bad(&mut self, lba: u64) {
        self.sectors_bad += 1;
        if let Some(last) = self.bad_ranges.last_mut() {
            if last.0 + last.1 == lba {
                last.1 += 1;
                return;
            }
        }
        if self.bad_ranges.len() < MAX_BAD_RANGES {
            self.bad_ranges.push((lba, 1));
        } else {
            self.bad_ranges_truncated = true;
        }
    }
}

/// Lee los sectores `[start, start + count)` y entrega cada bloque a `sink(lba_inicial, datos)`.
///
/// `progress(hechos, total)` se llama tras cada bloque; si devuelve `false` la operación se
/// cancela con [`IoError::Cancelled`].
pub fn read_range(
    dev: &(impl BlockDevice + ?Sized),
    start: u64,
    count: u64,
    policy: &ReadPolicy,
    mut sink: impl FnMut(u64, &[u8]) -> Result<()>,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<ReadReport> {
    let ss = dev.sector_size() as usize;
    let chunk = u64::from(policy.chunk_sectors.max(1)).min((MAX_READ_REQUEST / ss) as u64);
    // Valida el rango completo (sin reservar memoria para él).
    let total = dev.sector_count();
    if count == 0 || start.checked_add(count).is_none_or(|e| e > total) {
        return Err(IoError::InvalidRange {
            lba: start,
            count,
            total,
        });
    }
    let mut report = ReadReport {
        sectors_requested: count,
        ..ReadReport::default()
    };
    let mut buf = vec![0u8; chunk as usize * ss];
    let end = start + count;
    let mut lba = start;
    while lba < end {
        if !progress(lba - start, count) {
            return Err(IoError::Cancelled);
        }
        let n = chunk.min(end - lba);
        let bytes = &mut buf[..n as usize * ss];
        sector_span(dev, lba, n)?;
        if dev.read_sectors(lba, n, bytes).is_err() {
            report.retry_reads += 1;
            for i in 0..n {
                let s = lba + i;
                let sec = &mut bytes[i as usize * ss..(i as usize + 1) * ss];
                let mut last_err = None;
                for attempt in 0..=policy.retries {
                    if attempt > 0 {
                        report.retry_reads += 1;
                    }
                    match dev.read_sectors(s, 1, sec) {
                        Ok(()) => {
                            last_err = None;
                            break;
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                if let Some(e) = last_err {
                    match policy.on_error {
                        OnError::Fail => return Err(e),
                        OnError::FillAndContinue => {
                            for (j, b) in sec.iter_mut().enumerate() {
                                *b = policy.bad_fill[j % 4];
                            }
                            report.add_bad(s);
                        }
                    }
                } else {
                    report.sectors_read_ok += 1;
                }
            }
        } else {
            report.sectors_read_ok += n;
        }
        sink(lba, bytes)?;
        lba += n;
    }
    progress(count, count);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FaultyDevice, MemDevice};

    fn mem(sectors: u64, ss: u32) -> MemDevice {
        let data: Vec<u8> = (0..sectors * u64::from(ss))
            .map(|i| (i / u64::from(ss)) as u8)
            .collect();
        MemDevice::new(data, ss).unwrap()
    }

    fn collect(
        dev: &(impl BlockDevice + ?Sized),
        start: u64,
        count: u64,
        p: &ReadPolicy,
    ) -> (Result<ReadReport>, Vec<u8>) {
        let mut out = Vec::new();
        let r = read_range(
            dev,
            start,
            count,
            p,
            |_, d| {
                out.extend_from_slice(d);
                Ok(())
            },
            |_, _| true,
        );
        (r, out)
    }

    #[test]
    fn clean_read_matches_source_for_every_common_sector_size() {
        for ss in crate::COMMON_SECTOR_SIZES {
            let d = mem(100, ss);
            let p = ReadPolicy {
                chunk_sectors: 7,
                ..ReadPolicy::default()
            };
            let (r, out) = collect(&d, 10, 50, &p);
            let r = r.unwrap();
            assert_eq!(
                (r.sectors_read_ok, r.sectors_bad, r.retry_reads),
                (50, 0, 0)
            );
            let mut expect = vec![0u8; 50 * ss as usize];
            d.read_sectors(10, 50, &mut expect).unwrap();
            assert_eq!(out, expect);
        }
    }

    #[test]
    fn strict_policy_fails_with_exact_position() {
        let d = FaultyDevice::new(mem(64, 512)).with_bad([37]);
        let (r, _) = collect(&d, 0, 64, &ReadPolicy::default());
        match r {
            Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (37 * 512, 512)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fill_policy_fills_only_bad_sectors_and_reports_them() {
        let d = FaultyDevice::new(mem(64, 512)).with_bad([5, 6, 7, 40]);
        let p = ReadPolicy {
            chunk_sectors: 16,
            retries: 2,
            on_error: OnError::FillAndContinue,
            bad_fill: [0xDE, 0xAD, 0xBE, 0xEF],
        };
        let (r, out) = collect(&d, 0, 64, &p);
        let r = r.unwrap();
        assert_eq!(r.sectors_bad, 4);
        assert_eq!(r.sectors_read_ok, 60);
        assert_eq!(r.bad_ranges, vec![(5, 3), (40, 1)]);
        for lba in 0..64u64 {
            let s = &out[lba as usize * 512..(lba as usize + 1) * 512];
            if [5, 6, 7, 40].contains(&lba) {
                assert_eq!(&s[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
            } else {
                assert!(s.iter().all(|&b| b == lba as u8), "sector {lba} alterado");
            }
        }
    }

    #[test]
    fn retries_recover_flaky_sectors() {
        let d = FaultyDevice::new(mem(16, 512)).with_flaky(3, 2);
        let p = ReadPolicy {
            chunk_sectors: 8,
            retries: 2,
            on_error: OnError::Fail,
            ..ReadPolicy::default()
        };
        let (r, out) = collect(&d, 0, 16, &p);
        let r = r.unwrap();
        assert_eq!((r.sectors_read_ok, r.sectors_bad), (16, 0));
        assert!(r.retry_reads >= 2);
        assert!(out[3 * 512..4 * 512].iter().all(|&b| b == 3));
        // La lectura del bloque consume un fallo y cada intento por sector otro: con 4 fallos
        // programados y 1 + 2 intentos, el sector sigue fallando.
        let d = FaultyDevice::new(mem(16, 512)).with_flaky(3, 4);
        let (r, _) = collect(&d, 0, 16, &p);
        assert!(r.is_err());
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
            |_, _| Ok(()),
            |done, _| {
                calls += 1;
                done < 300
            },
        );
        assert!(matches!(r, Err(IoError::Cancelled)));
        assert!(calls <= 32);
        assert!(matches!(
            collect(&d, 990, 20, &p).0,
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            collect(&d, 0, 0, &p).0,
            Err(IoError::InvalidRange { .. })
        ));
    }

    #[test]
    fn bad_range_list_is_bounded() {
        let mut r = ReadReport::default();
        for i in 0..(MAX_BAD_RANGES as u64 + 10) {
            r.add_bad(i * 2); // no contiguos: un rango por sector
        }
        assert_eq!(r.bad_ranges.len(), MAX_BAD_RANGES);
        assert!(r.bad_ranges_truncated);
        assert_eq!(r.sectors_bad, MAX_BAD_RANGES as u64 + 10);
    }
}
