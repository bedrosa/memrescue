//! Descompresión LZNT1 (especificación pública [MS-XCA] §2.5), con límites estrictos.
//!
//! Un búfer LZNT1 es una secuencia de bloques (*chunks*) que se descomprimen por separado en
//! tramos de 4096 bytes. Cada bloque empieza por una cabecera de 16 bits: los 12 bits bajos son el
//! tamaño de los datos del bloque menos 1 (sin contar la cabecera), los bits 12 a 14 una firma
//! (3) y el bit 15 indica si el bloque está comprimido. Una cabecera 0 (o el final del búfer)
//! termina la secuencia.
//!
//! - Bloque sin comprimir: los datos se copian tal cual.
//! - Bloque comprimido: grupos de un byte de indicadores y hasta 8 elementos. Indicador 0 = byte
//!   literal; 1 = referencia de 16 bits a lo ya descomprimido **en ese mismo bloque**. La división
//!   de la referencia entre desplazamiento y longitud depende de la posición `p` dentro del bloque
//!   descomprimido: el desplazamiento usa los bits necesarios para representar `p - 1` (como mínimo
//!   4, como máximo 12) y la longitud el resto; desplazamiento = campo + 1, longitud = campo + 3.
//!
//! Comportamiento fijado con el oráculo de Windows (`tests/vectors/lznt1`, generados con
//! `RtlCompressBuffer`/`RtlDecompressBuffer`): un bloque que produce menos de 4096 bytes y va
//! seguido de otro se completa con ceros hasta 4096; la firma de la cabecera no se comprueba; tras
//! una cabecera 0 o un único byte final no se lee nada más.
//!
//! **Límites**: ningún bloque produce más de 4096 bytes; toda referencia apunta dentro del bloque
//! ya descomprimido; un bloque cuyos datos se salen de la entrada es un error; la salida total
//! nunca supera el límite pedido (en NTFS, el tamaño de la unidad de compresión). Ante un error,
//! la salida conserva solo los bloques completos anteriores, que son correctos: quien llama decide
//! qué hacer con el resto (en NTFS se marca como dañado, nunca como datos).

use std::fmt;

/// Tamaño de un bloque descomprimido.
pub const CHUNK_SIZE: usize = 4096;

/// Error de descompresión. `at` es el byte de la entrada donde empieza el bloque afectado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lznt1Error {
    /// La cabecera o los datos del bloque se salen de la entrada (o falta un elemento).
    Truncated { at: usize },
    /// Referencia a una posición anterior al inicio del bloque descomprimido.
    Displacement {
        at: usize,
        displacement: usize,
        position: usize,
    },
    /// El bloque descomprimido superaría 4096 bytes.
    ChunkOverflow { at: usize },
    /// La salida superaría el límite pedido.
    OutputLimit { at: usize, limit: usize },
}

impl Lznt1Error {
    pub fn code(&self) -> &'static str {
        match self {
            Lznt1Error::Truncated { .. } => "lznt1_truncated",
            Lznt1Error::Displacement { .. } => "lznt1_displacement",
            Lznt1Error::ChunkOverflow { .. } => "lznt1_chunk_overflow",
            Lznt1Error::OutputLimit { .. } => "lznt1_output_limit",
        }
    }
}

impl fmt::Display for Lznt1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Lznt1Error::Truncated { at } => {
                write!(f, "LZNT1: bloque en el byte {at} truncado")
            }
            Lznt1Error::Displacement {
                at,
                displacement,
                position,
            } => write!(
                f,
                "LZNT1: bloque en el byte {at}: referencia {displacement} bytes atrás desde la posición {position}"
            ),
            Lznt1Error::ChunkOverflow { at } => {
                write!(f, "LZNT1: el bloque en el byte {at} produce más de 4096 bytes")
            }
            Lznt1Error::OutputLimit { at, limit } => write!(
                f,
                "LZNT1: el bloque en el byte {at} supera el límite de salida ({limit} bytes)"
            ),
        }
    }
}

/// Bits del desplazamiento de una referencia en la posición `pos` (≥ 1) del bloque.
fn displacement_bits(pos: usize) -> u32 {
    let mut bits = 4;
    while (1usize << bits) < pos {
        bits += 1;
    }
    bits
}

/// Descomprime un bloque (`data`, sin cabecera) al final de `out`.
fn chunk(data: &[u8], out: &mut Vec<u8>, at: usize, limit: usize) -> Result<(), Lznt1Error> {
    let start = out.len();
    let mut i = 0;
    while i < data.len() {
        let flags = data[i];
        i += 1;
        for bit in 0..8 {
            if i >= data.len() {
                break;
            }
            let pos = out.len() - start;
            if flags >> bit & 1 == 0 {
                if pos >= CHUNK_SIZE {
                    return Err(Lznt1Error::ChunkOverflow { at });
                }
                if out.len() >= limit {
                    return Err(Lznt1Error::OutputLimit { at, limit });
                }
                out.push(data[i]);
                i += 1;
                continue;
            }
            if i + 2 > data.len() {
                return Err(Lznt1Error::Truncated { at });
            }
            let token = usize::from(u16::from_le_bytes([data[i], data[i + 1]]));
            i += 2;
            let lbits = 16 - displacement_bits(pos.max(1));
            let displacement = (token >> lbits) + 1;
            let length = (token & ((1 << lbits) - 1)) + 3;
            if displacement > pos {
                return Err(Lznt1Error::Displacement {
                    at,
                    displacement,
                    position: pos,
                });
            }
            if pos + length > CHUNK_SIZE {
                return Err(Lznt1Error::ChunkOverflow { at });
            }
            if out.len() + length > limit {
                return Err(Lznt1Error::OutputLimit { at, limit });
            }
            // Copia byte a byte: el origen puede solaparse con lo que se va escribiendo.
            let from = out.len() - displacement;
            for k in 0..length {
                let b = out[from + k];
                out.push(b);
            }
        }
    }
    Ok(())
}

/// Cómo terminó una descompresión correcta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lznt1End {
    /// Cabecera 0 en este byte de la entrada: el contenido termina ahí (lo que sigue no cuenta).
    Terminator(usize),
    /// Se acabó la entrada (o quedaba un único byte).
    EndOfInput,
}

/// Descomprime `src` y añade el resultado a `out` (que debe llegar vacío o terminar en un límite
/// de bloque), sin superar `limit` bytes en total. Ante un error, `out` conserva solo los bloques
/// completos anteriores al que falla.
pub fn decompress_into(
    src: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<Lznt1End, Lznt1Error> {
    let base = out.len();
    let mut at = 0usize;
    let mut previous_short = false;
    loop {
        if src.len().saturating_sub(at) < 2 {
            // Fin del búfer (un byte suelto final no forma una cabecera y se ignora).
            return Ok(Lznt1End::EndOfInput);
        }
        let header = u16::from_le_bytes([src[at], src[at + 1]]);
        if header == 0 {
            return Ok(Lznt1End::Terminator(at));
        }
        let size = usize::from(header & 0x0FFF) + 1;
        let data_start = at + 2;
        let Some(data) = src.get(data_start..data_start + size) else {
            return Err(Lznt1Error::Truncated { at });
        };
        // Un bloque corto seguido de otro se completa con ceros hasta 4096 (como Windows).
        if previous_short {
            let pad = CHUNK_SIZE - (out.len() - base) % CHUNK_SIZE;
            if out.len() + pad > limit {
                return Err(Lznt1Error::OutputLimit { at, limit });
            }
            out.resize(out.len() + pad, 0);
        }
        let chunk_start = out.len();
        let result = if header & 0x8000 == 0 {
            if out.len() + size > limit {
                Err(Lznt1Error::OutputLimit { at, limit })
            } else {
                out.extend_from_slice(data);
                Ok(())
            }
        } else {
            chunk(data, out, at, limit)
        };
        if let Err(e) = result {
            out.truncate(chunk_start);
            return Err(e);
        }
        previous_short = out.len() - chunk_start < CHUNK_SIZE;
        at = data_start + size;
    }
}

/// Descomprime `src` en un búfer nuevo de como mucho `limit` bytes.
pub fn decompress(src: &[u8], limit: usize) -> Result<Vec<u8>, Lznt1Error> {
    let mut out = Vec::new();
    decompress_into(src, &mut out, limit)?;
    Ok(out)
}

/// Compresor LZNT1 sencillo (búsqueda voraz), solo para construir datos de prueba. No es el
/// oráculo: los vectores de Windows lo son.
#[cfg(test)]
pub(crate) fn compress_for_tests(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in data.chunks(CHUNK_SIZE) {
        let mut body = Vec::new();
        let mut pos = 0usize;
        while pos < c.len() {
            let flag_at = body.len();
            body.push(0u8);
            for bit in 0..8 {
                if pos >= c.len() {
                    break;
                }
                let (mut best_len, mut best_disp) = (0usize, 0usize);
                if pos > 0 {
                    let lbits = 16 - displacement_bits(pos);
                    let max_disp = (1usize << (16 - lbits)).min(pos);
                    let max_len = ((1usize << lbits) - 1 + 3).min(c.len() - pos);
                    for disp in 1..=max_disp {
                        let mut l = 0;
                        while l < max_len && c[pos - disp + l] == c[pos + l] {
                            l += 1;
                        }
                        if l > best_len {
                            best_len = l;
                            best_disp = disp;
                        }
                    }
                }
                if best_len >= 3 {
                    let lbits = 16 - displacement_bits(pos);
                    let token = ((best_disp - 1) << lbits) | (best_len - 3);
                    body.extend_from_slice(&(token as u16).to_le_bytes());
                    body[flag_at] |= 1 << bit;
                    pos += best_len;
                } else {
                    body.push(c[pos]);
                    pos += 1;
                }
            }
        }
        if body.len() < c.len() {
            out.extend_from_slice(&(0xB000u16 | (body.len() - 1) as u16).to_le_bytes());
            out.extend_from_slice(&body);
        } else {
            out.extend_from_slice(&(0x3000u16 | (c.len() - 1) as u16).to_le_bytes());
            out.extend_from_slice(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testvol::xorshift;
    use mr_common::{HashAlgorithm, MultiHasher};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn sha256(d: &[u8]) -> String {
        let mut h = MultiHasher::new(&[HashAlgorithm::Sha256]);
        h.update(d);
        h.finish().remove(0).1
    }

    fn vectors_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/lznt1")
    }

    /// `[[vector]]` del manifiesto como tablas clave → valor.
    fn manifest() -> Vec<HashMap<String, String>> {
        let text = std::fs::read_to_string(vectors_dir().join("manifest.toml")).unwrap();
        let mut v: Vec<HashMap<String, String>> = Vec::new();
        for l in text.lines().map(str::trim) {
            if l == "[[vector]]" {
                v.push(HashMap::new());
            } else if let (Some(cur), Some((k, val))) = (v.last_mut(), l.split_once(" = ")) {
                cur.insert(k.to_string(), val.trim_matches('"').to_string());
            }
        }
        v
    }

    fn hex_file(name: &str) -> Vec<u8> {
        let t = std::fs::read_to_string(vectors_dir().join(format!("{name}.hex"))).unwrap();
        let h: String = t.split_whitespace().collect();
        (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Oráculo independiente: vectores comprimidos por Windows (`RtlCompressBuffer`) y casos límite
    /// con el resultado de `RtlDecompressBuffer`.
    #[test]
    fn windows_vectors_decompress_to_the_original() {
        let m = manifest();
        let (mut ok, mut errors, mut multi_cu) = (0, 0, 0);
        for v in &m {
            let name = &v["name"];
            let comp = hex_file(name);
            assert_eq!(comp.len().to_string(), v["compressed_size"], "{name}");
            assert_eq!(sha256(&comp), v["compressed_sha256"], "{name}");
            let limit: usize = match v.get("output_buffer") {
                Some(b) => b.parse().unwrap(),
                None => v["plain_size"].parse().unwrap(),
            };
            let r = decompress(&comp, limit);
            match v["expect"].as_str() {
                "ok" => {
                    let out = r.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(out.len().to_string(), v["plain_size"], "{name}");
                    assert_eq!(sha256(&out), v["plain_sha256"], "{name}");
                    if out.len() > 65536 {
                        multi_cu += 1;
                    }
                    ok += 1;
                }
                "error" => {
                    assert!(r.is_err(), "{name}: {r:?}");
                    errors += 1;
                }
                other => panic!("{name}: {other}"),
            }
        }
        assert_eq!((ok, errors), (23, 3));
        assert!(multi_cu >= 4, "{multi_cu}");
    }

    /// Con un límite menor que la salida, error y solo los bloques completos anteriores.
    #[test]
    fn output_limit_keeps_complete_chunks() {
        let data: Vec<u8> = (0..20000u32)
            .map(|i| (i % 251) as u8 ^ (i / 300) as u8)
            .collect();
        let comp = compress_for_tests(&data);
        assert_eq!(decompress(&comp, 20000).unwrap(), data);
        let mut out = Vec::new();
        let e = decompress_into(&comp, &mut out, 10000).unwrap_err();
        assert_eq!(e.code(), "lznt1_output_limit");
        assert_eq!(out, &data[..8192]);
        assert!(decompress(&comp, 0).is_err());
        assert_eq!(decompress(&[], 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn malformed_chunks_are_typed_errors() {
        // Bloque comprimido que produce más de 4096 bytes: 'a' y referencias de longitud máxima.
        let mut body = vec![0b1111_1110u8, b'a'];
        for _ in 0..7 {
            body.extend_from_slice(&0x0FFFu16.to_le_bytes()); // desplazamiento 1, longitud 4098
        }
        let mut c = (0xB000u16 | (body.len() - 1) as u16).to_le_bytes().to_vec();
        c.extend_from_slice(&body);
        assert_eq!(
            decompress(&c, 1 << 20).unwrap_err().code(),
            "lznt1_chunk_overflow"
        );
        // Literal tras llenar el bloque con una referencia: el bloque pasaría de 4096.
        let body = [0b0000_0010u8, b'a', 0xFC, 0x0F, b'b'];
        let mut c = (0xB000u16 | (body.len() - 1) as u16).to_le_bytes().to_vec();
        c.extend_from_slice(&body);
        assert_eq!(
            decompress(&c, 1 << 20).unwrap_err().code(),
            "lznt1_chunk_overflow"
        );
        // Sin el literal final, el bloque produce exactamente 4096 bytes.
        let out = decompress(&c[..c.len() - 1], 1 << 20);
        assert_eq!(
            out.unwrap_err().code(),
            "lznt1_truncated",
            "tamaño declarado mayor"
        );
        let mut c2 = (0xB000u16 | 3).to_le_bytes().to_vec();
        c2.extend_from_slice(&body[..4]);
        assert_eq!(decompress(&c2, 1 << 20).unwrap(), vec![b'a'; 4096]);
        // Referencia truncada (falta el segundo byte del elemento).
        let c = [0x02, 0xB0, 0b10, b'x', 0x05];
        assert_eq!(decompress(&c, 64).unwrap_err().code(), "lznt1_truncated");
    }

    #[test]
    fn roundtrip_with_the_test_compressor() {
        let mut rng = xorshift(0x1A2B_3C4D);
        for n in [0usize, 1, 2, 3, 17, 4095, 4096, 4097, 8192, 65536, 70001] {
            let alphabet = 1 + rng() % 20;
            let data: Vec<u8> = (0..n).map(|_| (rng() % alphabet) as u8).collect();
            let c = compress_for_tests(&data);
            assert_eq!(decompress(&c, n).unwrap(), data, "{n}");
        }
    }

    /// Fuzzing determinista: entradas aleatorias y vectores mutados; sin pánicos, salida acotada
    /// por el límite y, en error, múltiplo de bloque.
    #[test]
    fn lznt1_fuzz_never_panics() {
        let mut rng = xorshift(0x0711_2026);
        let seeds: Vec<Vec<u8>> = manifest()
            .iter()
            .map(|v| hex_file(&v["name"]))
            .filter(|c| c.len() < 30000)
            .collect();
        for i in 0..30_000u32 {
            let mut src: Vec<u8> = if i % 3 == 0 {
                let n = (rng() % 300) as usize;
                (0..n).map(|_| rng() as u8).collect()
            } else {
                seeds[(rng() as usize) % seeds.len()].clone()
            };
            if !src.is_empty() {
                for _ in 0..1 + rng() % 6 {
                    let at = (rng() as usize) % src.len();
                    src[at] = rng() as u8;
                }
            }
            let limit = (rng() % 300_000) as usize;
            let mut out = Vec::new();
            match decompress_into(&src, &mut out, limit) {
                Ok(_) => assert!(out.len() <= limit),
                Err(_) => assert!(out.len() <= limit),
            }
        }
    }
}
