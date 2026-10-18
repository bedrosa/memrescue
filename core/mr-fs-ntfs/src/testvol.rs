//! Estructuras NTFS sintéticas mínimas para las pruebas unitarias (solo pruebas).
//!
//! Se construyen a partir de la disposición pública del formato, igual que los discos MBR/GPT
//! sintéticos de `mr-part`. No sustituyen al corpus independiente (ADR-024): solo contienen lo que
//! cada regla necesita (arranque, registros FILE con su secuencia de actualización, atributos
//! residentes y no residentes, `$MFT`, `$MFTMirr` y `$Volume`).

/// Generador pseudoaleatorio determinista (xorshift64*).
pub fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed.max(1);
    move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Sector de arranque NTFS con los campos dados.
#[allow(clippy::too_many_arguments)]
pub fn boot_sector(
    bps: u16,
    spc_raw: u8,
    total: u64,
    mft_lcn: u64,
    mirr_lcn: u64,
    mft_raw: u8,
    idx_raw: u8,
    serial: u64,
) -> Vec<u8> {
    let mut s = vec![0u8; 512];
    s[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    s[3..11].copy_from_slice(b"NTFS    ");
    s[0x0B..0x0D].copy_from_slice(&bps.to_le_bytes());
    s[0x0D] = spc_raw;
    s[0x15] = 0xF8;
    s[0x28..0x30].copy_from_slice(&total.to_le_bytes());
    s[0x30..0x38].copy_from_slice(&mft_lcn.to_le_bytes());
    s[0x38..0x40].copy_from_slice(&mirr_lcn.to_le_bytes());
    s[0x40] = mft_raw;
    s[0x44] = idx_raw;
    s[0x48..0x50].copy_from_slice(&serial.to_le_bytes());
    s[0x1FE] = 0x55;
    s[0x1FF] = 0xAA;
    s
}

fn align8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

fn utf16(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Atributo residente.
pub fn resident(type_code: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let n = utf16(name);
    let voff = align8(0x18 + n.len());
    let len = align8(voff + value.len());
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[9] = (n.len() / 2) as u8;
    a[0x0A..0x0C].copy_from_slice(&0x18u16.to_le_bytes());
    a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[0x14..0x16].copy_from_slice(&(voff as u16).to_le_bytes());
    a[0x18..0x18 + n.len()].copy_from_slice(&n);
    a[voff..voff + value.len()].copy_from_slice(value);
    a
}

/// Atributo no residente (`pairs` incluye el byte 0 final).
#[allow(clippy::too_many_arguments)]
pub fn non_resident(
    type_code: u32,
    name: &str,
    start_vcn: u64,
    last_vcn: u64,
    pairs: &[u8],
    allocated: u64,
    size: u64,
    init: u64,
) -> Vec<u8> {
    let n = utf16(name);
    let roff = align8(0x40 + n.len());
    let len = align8(roff + pairs.len());
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[8] = 1;
    a[9] = (n.len() / 2) as u8;
    a[0x0A..0x0C].copy_from_slice(&0x40u16.to_le_bytes());
    a[0x10..0x18].copy_from_slice(&start_vcn.to_le_bytes());
    a[0x18..0x20].copy_from_slice(&last_vcn.to_le_bytes());
    a[0x20..0x22].copy_from_slice(&(roff as u16).to_le_bytes());
    a[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
    a[0x30..0x38].copy_from_slice(&size.to_le_bytes());
    a[0x38..0x40].copy_from_slice(&init.to_le_bytes());
    a[0x40..0x40 + n.len()].copy_from_slice(&n);
    a[roff..roff + pairs.len()].copy_from_slice(pairs);
    a
}

/// Entero con signo en el mínimo de bytes little-endian.
fn signed_bytes(v: i64) -> Vec<u8> {
    let b = v.to_le_bytes();
    for n in 1..=8 {
        let lo = if n == 8 {
            v
        } else {
            (v << (64 - 8 * n)) >> (64 - 8 * n)
        };
        if lo == v {
            return b[..n].to_vec();
        }
    }
    b.to_vec()
}

/// Codifica runs `(LCN o disperso, clústeres)` como mapping pairs (con el 0 final).
pub fn encode_runs(runs: &[(Option<u64>, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev = 0i64;
    for &(lcn, len) in runs {
        let l = signed_bytes(len as i64);
        let o = match lcn {
            Some(x) => {
                let d = signed_bytes(x as i64 - prev);
                prev = x as i64;
                d
            }
            None => Vec::new(),
        };
        out.push(((o.len() as u8) << 4) | l.len() as u8);
        out.extend(l);
        out.extend(o);
    }
    out.push(0);
    out
}

/// Aplica la secuencia de actualización a un registro sin proteger (los dos últimos bytes de cada
/// tramo pasan a la USA y se sustituyen por el USN). No hace nada si la USA no es coherente.
pub fn protect(buf: &mut [u8]) {
    let off = usize::from(u16::from_le_bytes([buf[4], buf[5]]));
    let count = usize::from(u16::from_le_bytes([buf[6], buf[7]]));
    let strides = buf.len() / 512;
    if count != strides + 1 || off + 2 * count > buf.len() || off < 8 {
        return;
    }
    let usn = [buf[off], buf[off + 1]];
    for i in 0..strides {
        let end = (i + 1) * 512 - 2;
        let saved = [buf[end], buf[end + 1]];
        buf[off + 2 * (i + 1)..off + 2 * (i + 2)].copy_from_slice(&saved);
        buf[end..end + 2].copy_from_slice(&usn);
    }
}

/// Registro FILE protegido con los atributos dados.
pub fn record(number: u32, seq: u16, flags: u16, attrs: &[Vec<u8>], size: usize) -> Vec<u8> {
    let mut r = vec![0u8; size];
    let count = size / 512 + 1;
    let first = align8(0x30 + 2 * count);
    r[0..4].copy_from_slice(b"FILE");
    r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
    r[6..8].copy_from_slice(&(count as u16).to_le_bytes());
    r[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
    r[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
    r[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
    r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
    r[0x1C..0x20].copy_from_slice(&(size as u32).to_le_bytes());
    r[0x28..0x2A].copy_from_slice(&(attrs.len() as u16).to_le_bytes());
    r[0x2C..0x30].copy_from_slice(&number.to_le_bytes());
    r[0x30..0x32].copy_from_slice(&3u16.to_le_bytes());
    let mut at = first;
    for a in attrs {
        r[at..at + a.len()].copy_from_slice(a);
        at += a.len();
    }
    r[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let in_use = at + 8;
    r[0x18..0x1C].copy_from_slice(&(in_use as u32).to_le_bytes());
    // Valores reconocibles al final de cada tramo libre (vuelven tras deshacer los fixups).
    for i in 0..size / 512 {
        let end = (i + 1) * 512 - 2;
        if end >= in_use {
            let v = match i {
                0 => [0xA5, 0x5A],
                1 => [0xC3, 0x3C],
                _ => [i as u8, 0x77],
            };
            r[end..end + 2].copy_from_slice(&v);
        }
    }
    protect(&mut r);
    r
}

/// Registro con varios atributos (base del fuzzing de registros).
pub fn rich_record() -> Vec<u8> {
    record(
        9,
        4,
        crate::record::flags::IN_USE,
        &[
            resident(0x10, "", &[0x11; 48]),
            resident(0x30, "", &[0x22; 74]),
            resident(0x80, "ads", b"contenido alternativo"),
            non_resident(
                0x80,
                "",
                0,
                9,
                &encode_runs(&[(Some(40), 4), (None, 2), (Some(60), 4)]),
                10240,
                10000,
                9000,
            ),
        ],
        1024,
    )
}

/// Volumen NTFS sintético mínimo (ver el módulo).
pub struct Synth {
    pub image: Vec<u8>,
    pub cluster: u64,
    pub total_sectors: u64,
    pub mft_lcn: u64,
    pub mirr_lcn: u64,
    /// Tramos en disco `(byte, bytes)` de cada registro de la MFT.
    pub pieces: Vec<Vec<(u64, u64)>>,
    pub mirror_offset: u64,
}

/// Registros de la MFT sintética y los inicializados.
pub const SYNTH_RECORDS: u64 = 32;
pub const SYNTH_INITIALIZED: u64 = 28;

impl Synth {
    /// Volumen con clústeres de `512 × spc` bytes, registros de 1 KiB, MFT de 32 registros en dos
    /// runs (con clústeres de 512 B, el registro 15 queda partido entre ellos), `$MFTMirr` de 4
    /// registros y la copia del arranque en el último sector.
    pub fn new(spc: u8) -> Synth {
        let c = 512 * u64::from(spc);
        let total_sectors = 2047u64;
        let rs = 1024u64;
        let mft_bytes = SYNTH_RECORDS * rs;
        let clusters = mft_bytes / c;
        let first = (clusters / 2).max(1) - u64::from(c == 512);
        let second = clusters - first;
        let lcn_a = 65536 / c;
        let lcn_b = 524_288 / c;
        let mirr = 262_144 / c;
        let mirr_clusters = (4 * rs).div_ceil(c);
        let mut img = vec![0u8; ((total_sectors + 1) * 512) as usize];
        let boot = boot_sector(
            512,
            spc,
            total_sectors,
            lcn_a,
            mirr,
            0xF6,
            0x01,
            0x0123_4567_89AB_CDEF,
        );
        img[..512].copy_from_slice(&boot);
        let last = (total_sectors * 512) as usize;
        img[last..last + 512].copy_from_slice(&boot);
        use crate::record::flags::{DIRECTORY, IN_USE};
        let si = || resident(0x10, "", &[0x10; 48]);
        let mut recs: Vec<Option<Vec<u8>>> = Vec::new();
        for n in 0..SYNTH_RECORDS as u32 {
            let r = match n {
                0 => Some(record(
                    0,
                    1,
                    IN_USE,
                    &[
                        si(),
                        non_resident(
                            0x80,
                            "",
                            0,
                            clusters - 1,
                            &encode_runs(&[(Some(lcn_a), first), (Some(lcn_b), second)]),
                            clusters * c,
                            mft_bytes,
                            SYNTH_INITIALIZED * rs,
                        ),
                    ],
                    1024,
                )),
                1 => Some(record(
                    1,
                    1,
                    IN_USE,
                    &[
                        si(),
                        non_resident(
                            0x80,
                            "",
                            0,
                            mirr_clusters - 1,
                            &encode_runs(&[(Some(mirr), mirr_clusters)]),
                            mirr_clusters * c,
                            4 * rs,
                            4 * rs,
                        ),
                    ],
                    1024,
                )),
                3 => {
                    let mut info = vec![0u8; 12];
                    info[8] = 3;
                    info[9] = 1;
                    Some(record(
                        3,
                        3,
                        IN_USE,
                        &[
                            si(),
                            resident(0x60, "", &utf16("SINTÉTICO")),
                            resident(0x70, "", &info),
                        ],
                        1024,
                    ))
                }
                2 | 4..=11 => Some(record(n, n as u16, IN_USE, &[si()], 1024)),
                16 => Some(record(16, 1, IN_USE | DIRECTORY, &[si()], 1024)),
                17 => Some(record(
                    17,
                    1,
                    IN_USE,
                    &[si(), resident(0x80, "", b"datos")],
                    1024,
                )),
                18 => Some(record(18, 2, 0, &[si()], 1024)),
                19 => Some(record(19, 3, DIRECTORY, &[si()], 1024)),
                20 => Some(record(20, 5, IN_USE, &[si()], 1024)),
                24..=27 => Some(record(n, 1, 0, &[], 1024)),
                // Más allá del tamaño inicializado: basura que nunca debe leerse.
                28..=31 => Some(vec![0xEE; 1024]),
                _ => None,
            };
            recs.push(r);
        }
        // Tramos de cada registro según los runs.
        let runs = [(lcn_a, first), (lcn_b, second)];
        let map = |byte: u64| -> u64 {
            let vcn = byte / c;
            let mut v0 = 0;
            for &(l, n) in &runs {
                if vcn < v0 + n {
                    return (l + vcn - v0) * c + byte % c;
                }
                v0 += n;
            }
            unreachable!()
        };
        let mut pieces = Vec::new();
        for n in 0..SYNTH_RECORDS {
            let mut p: Vec<(u64, u64)> = Vec::new();
            let mut b = n * rs;
            while b < (n + 1) * rs {
                let d = map(b);
                let take = (c - b % c).min((n + 1) * rs - b);
                match p.last_mut() {
                    Some((pd, pl)) if *pd + *pl == d => *pl += take,
                    _ => p.push((d, take)),
                }
                b += take;
            }
            pieces.push(p);
        }
        for (n, r) in recs.iter().enumerate() {
            if let Some(r) = r {
                let mut at = 0usize;
                for &(d, l) in &pieces[n] {
                    img[d as usize..(d + l) as usize].copy_from_slice(&r[at..at + l as usize]);
                    at += l as usize;
                }
            }
        }
        let mirror_offset = mirr * c;
        for (n, r) in recs.iter().take(4).enumerate() {
            let r = r.as_ref().unwrap();
            let at = mirror_offset as usize + n * 1024;
            img[at..at + 1024].copy_from_slice(r);
        }
        Synth {
            image: img,
            cluster: c,
            total_sectors,
            mft_lcn: lcn_a,
            mirr_lcn: mirr,
            pieces,
            mirror_offset,
        }
    }

    /// Primer byte del registro `n` en el volumen.
    pub fn record_offset(&self, n: u64) -> u64 {
        self.pieces[n as usize][0].0
    }

    /// LBA (sectores de 512 B) de los sectores del registro `n`.
    pub fn record_lbas(&self, n: u64) -> Vec<u64> {
        self.pieces[n as usize]
            .iter()
            .flat_map(|&(d, l)| d / 512..(d + l) / 512)
            .collect()
    }
}
