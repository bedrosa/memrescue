//! Decodificación de *mapping pairs* (lista de runs de un atributo no residente).
//!
//! [`decode_runs`] decodifica con aritmética comprobada y valida cada run contra el tamaño del
//! volumen; [`decode_extent`] además exige que los runs de una extensión (un atributo no residente
//! dentro de un registro) cubran exactamente su tramo de VCN. La unión de extensiones de varios
//! registros, los tamaños y las unidades de compresión se comprueban en [`crate::file`].
//!
//! Formato público: cada run empieza por un byte de cabecera; su nibble bajo es el número de bytes
//! de la longitud (1 a 8, sin signo) y el alto el del desplazamiento de LCN (0 a 8, con signo,
//! relativo al LCN del run anterior con LCN). Un desplazamiento de 0 bytes es un run disperso (sin
//! clústeres en disco). Un byte de cabecera 0 termina la lista.

use std::fmt;

/// Máximo de runs por atributo (cada run ocupa al menos 2 bytes de un registro de 64 KiB).
pub const MAX_RUNS: usize = 32768;

/// Un run: `length` clústeres desde el VCN `vcn`, en el LCN `lcn` (`None` = disperso).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub vcn: u64,
    pub lcn: Option<u64>,
    pub length: u64,
}

impl Run {
    /// VCN siguiente al último del run.
    pub fn end_vcn(&self) -> u64 {
        self.vcn + self.length
    }
}

/// Lista de runs no válida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// La lista se sale de los bytes disponibles (sin byte 0 final o run incompleto).
    Truncated { at: usize },
    /// Tamaño de la longitud 0 o mayor que 8.
    LengthSize { at: usize, size: u8 },
    /// Tamaño del desplazamiento mayor que 8.
    OffsetSize { at: usize, size: u8 },
    /// Longitud 0 o negativa.
    Length { at: usize },
    /// El LCN resultante es negativo.
    NegativeLcn { at: usize },
    /// Desbordamiento de VCN o LCN.
    Overflow { at: usize },
    /// Run que termina fuera del volumen.
    OutsideVolume { at: usize, lcn: u64, length: u64 },
    /// Más de [`MAX_RUNS`] runs.
    TooManyRuns,
    /// Los runs de una extensión no terminan en su VCN final + 1.
    Coverage { runs_end: u64, expected_end: u64 },
}

impl RunError {
    pub fn code(&self) -> &'static str {
        match self {
            RunError::Truncated { .. } => "runs_truncated",
            RunError::LengthSize { .. } => "runs_length_size",
            RunError::OffsetSize { .. } => "runs_offset_size",
            RunError::Length { .. } => "runs_length",
            RunError::NegativeLcn { .. } => "runs_negative_lcn",
            RunError::Overflow { .. } => "runs_overflow",
            RunError::OutsideVolume { .. } => "runs_outside_volume",
            RunError::TooManyRuns => "runs_too_many",
            RunError::Coverage { .. } => "runs_coverage",
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RunError::Truncated { at } => write!(f, "lista de runs truncada en el byte {at}"),
            RunError::LengthSize { at, size } => {
                write!(f, "run en el byte {at}: tamaño de longitud {size}")
            }
            RunError::OffsetSize { at, size } => {
                write!(f, "run en el byte {at}: tamaño de desplazamiento {size}")
            }
            RunError::Length { at } => write!(f, "run en el byte {at}: longitud no válida"),
            RunError::NegativeLcn { at } => write!(f, "run en el byte {at}: LCN negativo"),
            RunError::Overflow { at } => write!(f, "run en el byte {at}: desbordamiento"),
            RunError::OutsideVolume { at, lcn, length } => write!(
                f,
                "run en el byte {at}: {length} clústeres desde el LCN {lcn} fuera del volumen"
            ),
            RunError::TooManyRuns => write!(f, "más de {MAX_RUNS} runs"),
            RunError::Coverage {
                runs_end,
                expected_end,
            } => write!(
                f,
                "los runs terminan en el VCN {runs_end} y el atributo en el {expected_end}"
            ),
        }
    }
}

/// Entero little-endian de `bytes` (1 a 8), con extensión de signo si `signed`.
fn read_int(bytes: &[u8], signed: bool) -> i128 {
    let mut v: u64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        v |= u64::from(*b) << (8 * i);
    }
    let bits = 8 * bytes.len() as u32;
    if signed && bits < 64 && bytes.last().is_some_and(|b| b & 0x80 != 0) {
        i128::from(v) - (1i128 << bits)
    } else if signed {
        i128::from(v as i64)
    } else {
        i128::from(v)
    }
}

/// Decodifica los runs de `pairs` empezando en `start_vcn`. Cada run con clústeres debe quedar
/// dentro de `total_clusters`. Los runs contiguos no se fusionan (se devuelven tal como están).
pub fn decode_runs(
    pairs: &[u8],
    start_vcn: u64,
    total_clusters: u64,
) -> Result<Vec<Run>, RunError> {
    let mut runs = Vec::new();
    let mut at = 0usize;
    let mut vcn = start_vcn;
    let mut lcn: i128 = 0;
    loop {
        let Some(&h) = pairs.get(at) else {
            return Err(RunError::Truncated { at });
        };
        if h == 0 {
            return Ok(runs);
        }
        if runs.len() >= MAX_RUNS {
            return Err(RunError::TooManyRuns);
        }
        let ls = h & 0x0F;
        let os = h >> 4;
        if ls == 0 || ls > 8 {
            return Err(RunError::LengthSize { at, size: ls });
        }
        if os > 8 {
            return Err(RunError::OffsetSize { at, size: os });
        }
        let (ls, os) = (usize::from(ls), usize::from(os));
        let end = at + 1 + ls + os;
        if end > pairs.len() {
            return Err(RunError::Truncated { at });
        }
        let length = read_int(&pairs[at + 1..at + 1 + ls], true);
        if length <= 0 {
            return Err(RunError::Length { at });
        }
        let length = length as u64; // ≤ i64::MAX
        let run_lcn = if os == 0 {
            None
        } else {
            lcn += read_int(&pairs[at + 1 + ls..end], true);
            if lcn < 0 {
                return Err(RunError::NegativeLcn { at });
            }
            let l = u64::try_from(lcn).map_err(|_| RunError::Overflow { at })?;
            if l.checked_add(length).is_none_or(|e| e > total_clusters) {
                return Err(RunError::OutsideVolume { at, lcn: l, length });
            }
            Some(l)
        };
        runs.push(Run {
            vcn,
            lcn: run_lcn,
            length,
        });
        vcn = vcn.checked_add(length).ok_or(RunError::Overflow { at })?;
        at = end;
    }
}

/// Decodifica los runs de una extensión que cubre los VCN `start_vcn..=last_vcn` (atributo vacío:
/// `last_vcn = start_vcn - 1`) y exige que terminen exactamente en `last_vcn + 1`.
pub fn decode_extent(
    pairs: &[u8],
    start_vcn: u64,
    last_vcn: u64,
    total_clusters: u64,
) -> Result<Vec<Run>, RunError> {
    let runs = decode_runs(pairs, start_vcn, total_clusters)?;
    let runs_end = runs.last().map_or(start_vcn, Run::end_vcn);
    let expected_end = last_vcn.wrapping_add(1);
    if runs_end != expected_end {
        return Err(RunError::Coverage {
            runs_end,
            expected_end,
        });
    }
    Ok(runs)
}

/// Tramos dispersos `(vcn, clústeres)` de una lista de runs, con los contiguos unidos.
pub fn sparse_ranges(runs: &[Run]) -> Vec<(u64, u64)> {
    merge_contiguous(runs)
        .iter()
        .filter(|r| r.lcn.is_none())
        .map(|r| (r.vcn, r.length))
        .collect()
}

/// Une los runs contiguos en disco (mismo resultado que muestran otras herramientas).
pub fn merge_contiguous(runs: &[Run]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for r in runs {
        if let Some(last) = out.last_mut() {
            let contiguous = match (last.lcn, r.lcn) {
                (Some(a), Some(b)) => a + last.length == b,
                (None, None) => true,
                _ => false,
            };
            if contiguous && last.end_vcn() == r.vcn {
                last.length += r.length;
                continue;
            }
        }
        out.push(*r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::{encode_runs, xorshift};

    #[test]
    fn decodes_runs_with_signed_deltas_and_sparse() {
        // 0x21: 1 byte de longitud y 2 de desplazamiento.
        let pairs = [
            0x21, 0x10, 0x00, 0x01, // 16 clústeres en 256
            0x01, 0x08, // 8 dispersos
            0x11, 0x04, 0xF0, // 4 clústeres en 256 - 16 = 240
            0x00,
        ];
        let r = decode_runs(&pairs, 0, 1000).unwrap();
        assert_eq!(
            r,
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(256),
                    length: 16
                },
                Run {
                    vcn: 16,
                    lcn: None,
                    length: 8
                },
                Run {
                    vcn: 24,
                    lcn: Some(240),
                    length: 4
                },
            ]
        );
        assert_eq!(r[2].end_vcn(), 28);
        assert_eq!(decode_runs(&[0], 7, 10).unwrap(), vec![]);
        let r = decode_runs(&[0x11, 0x02, 0x05, 0], 40, 10).unwrap();
        assert_eq!(r[0].vcn, 40);
        // Ida y vuelta con el codificador de pruebas.
        let runs = [
            (Some(100u64), 3u64),
            (None, 2),
            (Some(50), 70000),
            (Some(7_000_000), 1),
        ];
        let r = decode_runs(&encode_runs(&runs), 0, 10_000_000).unwrap();
        assert_eq!(
            r.iter().map(|x| (x.lcn, x.length)).collect::<Vec<_>>(),
            runs.to_vec()
        );
    }

    #[test]
    fn invalid_runs_are_rejected() {
        let cases: [(&[u8], &str); 9] = [
            (&[], "runs_truncated"),
            (&[0x11, 0x04, 0x10], "runs_truncated"),
            (&[0x11, 0x04], "runs_truncated"),
            (&[0x10, 0x00], "runs_length_size"),
            (&[0x19, 0x00], "runs_length_size"),
            (&[0x91, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0], "runs_offset_size"),
            (&[0x11, 0x00, 0x01, 0x00], "runs_length"),
            (&[0x11, 0xFF, 0x01, 0x00], "runs_length"),
            (&[0x11, 0x01, 0xFF, 0x00], "runs_negative_lcn"),
        ];
        for (p, code) in cases {
            assert_eq!(decode_runs(p, 0, 1000).unwrap_err().code(), code, "{p:?}");
        }
        assert_eq!(
            decode_runs(&[0x21, 0x10, 0xF0, 0x03, 0x00], 0, 1000).unwrap_err(),
            RunError::OutsideVolume {
                at: 0,
                lcn: 1008,
                length: 16
            }
        );
        // Dos runs dispersos de i64::MAX clústeres: el VCN se desborda.
        let mut near_max = Vec::new();
        for _ in 0..2 {
            near_max.push(0x08);
            near_max.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
        }
        near_max.push(0);
        assert_eq!(
            decode_runs(&near_max, 2, u64::MAX).unwrap_err().code(),
            "runs_overflow"
        );
        let mut many = Vec::new();
        for _ in 0..=MAX_RUNS {
            many.extend_from_slice(&[0x01, 0x01]);
        }
        many.push(0);
        assert_eq!(
            decode_runs(&many, 0, 10).unwrap_err(),
            RunError::TooManyRuns
        );
    }

    #[test]
    fn extents_must_cover_their_vcn_range() {
        let p = encode_runs(&[(Some(100), 4), (None, 4)]);
        assert_eq!(decode_extent(&p, 10, 17, 1000).unwrap().len(), 2);
        assert_eq!(
            decode_extent(&p, 10, 18, 1000).unwrap_err(),
            RunError::Coverage {
                runs_end: 18,
                expected_end: 19
            }
        );
        assert_eq!(
            decode_extent(&p, 10, 16, 1000).unwrap_err().code(),
            "runs_coverage"
        );
        // Atributo vacío.
        assert_eq!(decode_extent(&[0], 0, u64::MAX, 10).unwrap(), vec![]);
        assert_eq!(decode_extent(&[0], 5, 4, 10).unwrap(), vec![]);
        assert!(decode_extent(&[0], 0, 0, 10).is_err());
        let r = decode_runs(&p, 0, 1000).unwrap();
        assert_eq!(sparse_ranges(&r), vec![(4, 4)]);
    }

    #[test]
    fn merge_joins_only_contiguous_runs() {
        let r = [
            Run {
                vcn: 0,
                lcn: Some(10),
                length: 2,
            },
            Run {
                vcn: 2,
                lcn: Some(12),
                length: 3,
            },
            Run {
                vcn: 5,
                lcn: Some(20),
                length: 1,
            },
            Run {
                vcn: 6,
                lcn: None,
                length: 1,
            },
            Run {
                vcn: 7,
                lcn: None,
                length: 2,
            },
        ];
        assert_eq!(
            merge_contiguous(&r),
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(10),
                    length: 5
                },
                Run {
                    vcn: 5,
                    lcn: Some(20),
                    length: 1
                },
                Run {
                    vcn: 6,
                    lcn: None,
                    length: 3
                },
            ]
        );
    }

    #[test]
    fn runs_fuzz_never_panics() {
        let mut rng = xorshift(0x0D0D_0001);
        for _ in 0..50_000 {
            let n = (rng() % 40) as usize;
            let p: Vec<u8> = (0..n).map(|_| rng() as u8).collect();
            if let Ok(runs) = decode_runs(&p, rng() % 1000, 1 << 32) {
                let mut vcn = None;
                for r in runs {
                    assert!(r.length > 0);
                    if let Some(l) = r.lcn {
                        assert!(l + r.length <= 1 << 32);
                    }
                    if let Some(v) = vcn {
                        assert_eq!(v, r.vcn);
                    }
                    vcn = Some(r.end_vcn());
                }
            }
        }
    }
}
