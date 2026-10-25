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

/// Atributo no residente con indicadores, unidad de compresión y tamaño comprimido (cabecera de
/// 0x48 bytes si lo lleva).
#[allow(clippy::too_many_arguments)]
pub fn non_resident_ex(
    type_code: u32,
    name: &str,
    flags: u16,
    start_vcn: u64,
    last_vcn: u64,
    pairs: &[u8],
    cu: u16,
    sizes: (u64, u64, u64),
    compressed_size: Option<u64>,
) -> Vec<u8> {
    let n = utf16(name);
    let header = if compressed_size.is_some() {
        0x48
    } else {
        0x40
    };
    let roff = align8(header + n.len());
    let len = align8(roff + pairs.len());
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[8] = 1;
    a[9] = (n.len() / 2) as u8;
    a[0x0A..0x0C].copy_from_slice(&(header as u16).to_le_bytes());
    a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
    a[0x10..0x18].copy_from_slice(&start_vcn.to_le_bytes());
    a[0x18..0x20].copy_from_slice(&last_vcn.to_le_bytes());
    a[0x20..0x22].copy_from_slice(&(roff as u16).to_le_bytes());
    a[0x22..0x24].copy_from_slice(&cu.to_le_bytes());
    a[0x28..0x30].copy_from_slice(&sizes.0.to_le_bytes());
    a[0x30..0x38].copy_from_slice(&sizes.1.to_le_bytes());
    a[0x38..0x40].copy_from_slice(&sizes.2.to_le_bytes());
    if let Some(c) = compressed_size {
        a[0x40..0x48].copy_from_slice(&c.to_le_bytes());
    }
    a[header..header + n.len()].copy_from_slice(&n);
    a[roff..roff + pairs.len()].copy_from_slice(pairs);
    a
}

/// Cambia el identificador de un atributo construido con estas funciones.
pub fn with_id(mut a: Vec<u8>, id: u16) -> Vec<u8> {
    a[0x0E..0x10].copy_from_slice(&id.to_le_bytes());
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
    record_with_base(number, seq, flags, 0, attrs, size)
}

/// Registro FILE con referencia a un registro base (`base` = número | secuencia << 48).
pub fn record_with_base(
    number: u32,
    seq: u16,
    flags: u16,
    base: u64,
    attrs: &[Vec<u8>],
    size: usize,
) -> Vec<u8> {
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
    r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
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
    /// Runs de la MFT `(LCN, clústeres)`.
    pub mft_runs: [(u64, u64); 2],
}

/// Registros de la MFT sintética y los inicializados.
pub const SYNTH_RECORDS: u64 = 32;
pub const SYNTH_INITIALIZED: u64 = 28;

impl Synth {
    /// Volumen con clústeres de `512 × spc` bytes, registros de 1 KiB, MFT de 32 registros en dos
    /// runs (con clústeres de 512 B, el registro 15 queda partido entre ellos), `$MFTMirr` de 4
    /// registros y la copia del arranque en el último sector.
    pub fn new(spc: u8) -> Synth {
        Synth::with_records(spc, SYNTH_RECORDS, SYNTH_INITIALIZED)
    }

    /// Como [`Synth::new`] con `records` registros en la MFT, de los que `initialized` están
    /// dentro del tamaño inicializado (los 4 siguientes son basura que nunca debe leerse).
    pub fn with_records(spc: u8, records: u64, initialized: u64) -> Synth {
        let c = 512 * u64::from(spc);
        let total_sectors = 2047u64;
        let rs = 1024u64;
        let mft_bytes = records * rs;
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
        for n in 0..records as u32 {
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
                            initialized * rs,
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
                // Más allá del tamaño inicializado: basura que nunca debe leerse.
                n if u64::from(n) >= initialized => Some(vec![0xEE; 1024]),
                24..=27 => Some(record(n, 1, 0, &[], 1024)),
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
        for n in 0..records {
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
            mft_runs: runs,
        }
    }

    /// Sustituye el registro `n` (ya protegido) en la MFT y, si es de los 4 primeros, en la copia.
    pub fn put_record(&mut self, n: u64, rec: &[u8]) {
        let mut at = 0usize;
        for &(d, l) in &self.pieces[n as usize] {
            self.image[d as usize..(d + l) as usize].copy_from_slice(&rec[at..at + l as usize]);
            at += l as usize;
        }
        if n < 4 {
            let m = (self.mirror_offset + n * 1024) as usize;
            self.image[m..m + 1024].copy_from_slice(rec);
        }
    }

    /// Escribe `data` en el clúster `lcn` (y los siguientes).
    pub fn put_clusters(&mut self, lcn: u64, data: &[u8]) {
        let at = (lcn * self.cluster) as usize;
        self.image[at..at + data.len()].copy_from_slice(data);
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

// ---------------------------------------------------------------------------------------------
// Índices de directorio y volumen con árbol (#22)
// ---------------------------------------------------------------------------------------------

/// Valor `$FILE_NAME` con el nombre en unidades UTF-16 (admite sustitutos sueltos).
pub fn file_name_units(parent: u64, units: &[u16], ns: u8, attrs: u32, size: u64) -> Vec<u8> {
    let mut v = vec![0u8; 0x42 + 2 * units.len()];
    v[0..8].copy_from_slice(&parent.to_le_bytes());
    for (i, t) in [11u64, 22, 33, 44].iter().enumerate() {
        v[8 + 8 * i..16 + 8 * i].copy_from_slice(&t.to_le_bytes());
    }
    v[0x28..0x30].copy_from_slice(&size.next_multiple_of(8).to_le_bytes());
    v[0x30..0x38].copy_from_slice(&size.to_le_bytes());
    v[0x38..0x3C].copy_from_slice(&attrs.to_le_bytes());
    v[0x40] = units.len() as u8;
    v[0x41] = ns;
    for (i, u) in units.iter().enumerate() {
        v[0x42 + 2 * i..0x44 + 2 * i].copy_from_slice(&u.to_le_bytes());
    }
    v
}

/// `$FILE_NAME` de un nombre.
pub fn file_name(parent: u64, name: &str, ns: u8, attrs: u32, size: u64) -> Vec<u8> {
    let u: Vec<u16> = name.encode_utf16().collect();
    file_name_units(parent, &u, ns, attrs, size)
}

/// Referencia en disco (número | secuencia << 48).
pub fn fref(record: u64, sequence: u16) -> u64 {
    record | (u64::from(sequence) << 48)
}

/// Entrada de índice: referencia, clave (`None` = última entrada) y subnodo.
pub fn index_entry(reference: u64, key: Option<&[u8]>, subnode: Option<u64>) -> Vec<u8> {
    let kl = key.map_or(0, <[u8]>::len);
    let len = align8(0x10 + kl) + if subnode.is_some() { 8 } else { 0 };
    let mut e = vec![0u8; len];
    if key.is_some() {
        e[0..8].copy_from_slice(&reference.to_le_bytes());
    }
    e[8..10].copy_from_slice(&(len as u16).to_le_bytes());
    e[0x0A..0x0C].copy_from_slice(&(kl as u16).to_le_bytes());
    let mut flags = 0u16;
    if subnode.is_some() {
        flags |= 1;
    }
    if key.is_none() {
        flags |= 2;
    }
    e[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
    if let Some(k) = key {
        e[0x10..0x10 + kl].copy_from_slice(k);
    }
    if let Some(v) = subnode {
        e[len - 8..].copy_from_slice(&v.to_le_bytes());
    }
    e
}

/// Entrada con clave `$FILE_NAME` (nombre, espacio de nombres, padre).
pub fn named_entry(
    reference: u64,
    parent: u64,
    name: &str,
    ns: u8,
    subnode: Option<u64>,
) -> Vec<u8> {
    index_entry(
        reference,
        Some(&file_name(parent, name, ns, 0x20, 0)),
        subnode,
    )
}

/// Valor de `$INDEX_ROOT` de `$I30` con las entradas dadas (la última debe ser la final).
pub fn index_root_value(entries: &[Vec<u8>], block_size: u32, large: bool) -> Vec<u8> {
    let body: Vec<u8> = entries.concat();
    let mut v = vec![0u8; 0x20];
    v[0..4].copy_from_slice(&0x30u32.to_le_bytes());
    v[4..8].copy_from_slice(&1u32.to_le_bytes());
    v[8..12].copy_from_slice(&block_size.to_le_bytes());
    v[0x0C] = 1;
    let len = (0x10 + body.len()) as u32;
    v[0x10..0x14].copy_from_slice(&0x10u32.to_le_bytes());
    v[0x14..0x18].copy_from_slice(&len.to_le_bytes());
    v[0x18..0x1C].copy_from_slice(&len.to_le_bytes());
    v[0x1C] = u8::from(large);
    v.extend_from_slice(&body);
    v
}

/// Nodo `INDX` protegido (VCN, entradas, tamaño, indicador de subnodos).
pub fn indx_block(vcn: u64, entries: &[Vec<u8>], block_size: usize, has_subnodes: bool) -> Vec<u8> {
    let mut b = vec![0u8; block_size];
    let count = block_size / 512 + 1;
    b[0..4].copy_from_slice(b"INDX");
    b[4..6].copy_from_slice(&0x28u16.to_le_bytes());
    b[6..8].copy_from_slice(&(count as u16).to_le_bytes());
    b[0x10..0x18].copy_from_slice(&vcn.to_le_bytes());
    let first = align8(0x28 + 2 * count);
    let eo = (first - 0x18) as u32;
    let body: Vec<u8> = entries.concat();
    let il = eo + body.len() as u32;
    b[0x18..0x1C].copy_from_slice(&eo.to_le_bytes());
    b[0x1C..0x20].copy_from_slice(&il.to_le_bytes());
    b[0x20..0x24].copy_from_slice(&((block_size - 0x18) as u32).to_le_bytes());
    b[0x24] = u8::from(has_subnodes);
    b[0x28..0x2A].copy_from_slice(&0x0101u16.to_le_bytes());
    b[first..first + body.len()].copy_from_slice(&body);
    protect(&mut b);
    b
}

/// Atributo residente con identificador.
pub fn res(type_code: u32, name: &str, value: &[u8], id: u16) -> Vec<u8> {
    with_id(resident(type_code, name, value), id)
}

/// Clúster de los nodos `INDX` de la raíz en [`tree_volume`] (VCN 0 y 1).
pub const TREE_ROOT_INDX_LCN: u64 = 200;
/// Clúster del nodo `INDX` (VCN 0) de `/Docs` en [`tree_volume`].
pub const TREE_DOCS_INDX_LCN: u64 = 202;
/// Clúster de `ñandú.bin` en [`tree_volume`].
pub const TREE_DATA_LCN: u64 = 210;
/// Clúster de `$UpCase` en [`tree_volume`] (32 clústeres).
pub const TREE_UPCASE_LCN: u64 = 220;

/// Contenido de `ñandú.bin`.
pub fn tree_data() -> Vec<u8> {
    (0..3000u32).map(|i| (i * 7 % 251) as u8).collect()
}

/// Nodos `INDX` de la raíz de [`tree_volume`]: VCN 0 (interior) y VCN 1 (hoja).
pub fn tree_root_nodes() -> [Vec<u8>; 2] {
    let root = fref(5, 5);
    let vcn0 = indx_block(
        0,
        &[
            named_entry(fref(18, 1), root, "Medio.txt", 1, Some(1)),
            named_entry(fref(18, 1), root, "MEDIO~1.TXT", 2, None),
            named_entry(fref(28, 1), root, "Ágil.txt", 3, None),
            index_entry(0, None, None),
        ],
        4096,
        true,
    );
    let vcn1 = indx_block(
        1,
        &[
            named_entry(root, root, ".", 3, None),
            named_entry(fref(17, 1), root, "a.txt", 1, None),
            index_entry(
                fref(16, 1),
                Some(&file_name(root, "Docs", 3, 0x1000_0000, 0)),
                None,
            ),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    [vcn0, vcn1]
}

/// Atributo `$FILE_NAME` residente (identificador 1).
fn fname_attr(parent: u64, name: &str, ns: u8, dir: bool, id: u16) -> Vec<u8> {
    let attrs = if dir { 0x1000_0000 } else { 0x20 };
    res(0x30, "", &file_name(parent, name, ns, attrs, 0), id)
}

/// Volumen sintético con árbol de directorios (clústeres de 4 KiB, nodos de 4 KiB, 40 registros):
///
/// ```text
/// /                     raíz: $INDEX_ROOT + 2 nodos INDX (VCN 0 interior, VCN 1 hoja), $BITMAP
/// /a.txt          (17)  enlace duro con /Docs/Informe.txt
/// /Docs           (16)  $INDEX_ROOT y un nodo INDX hoja
/// /Docs/b<D800>c  (22)  nombre con un sustituto suelto
/// /Docs/fuera.txt       → registro 99 (fuera de la MFT): referencia no válida
/// /Docs/Informe.txt (17, POSIX)
/// /Docs/reusado.txt     → registro 23 con secuencia 1; el registro tiene la 2: reutilizado
/// /Docs/roto.txt        → registro 25 (BAAD): no utilizable
/// /Docs/Sub       (19)
/// /Docs/Sub/deep.txt (21)
/// /Docs/viejo.txt       → registro 24 (no en uso): residual
/// /Docs/ñandú.bin (20)  no residente (3000 bytes en el clúster 210)
/// /Medio.txt      (18)  con nombre DOS MEDIO~1.TXT (entrada aparte)
/// /Ágil.txt       (28)  Win32 y DOS en una entrada
/// huérfanos: 23 «otro.txt» (su padre /Docs no lo lista), 26 (padre 21, que no es un
///            directorio), 27 (padre 24, no en uso)
/// ```
///
/// Con `with_upcase`, `$UpCase` (registro 10) es la tabla de [`upcase_table`]; si no, el
/// registro 10 no tiene datos y se usa el plegado simple.
pub fn tree_volume(with_upcase: bool) -> Synth {
    use crate::record::flags::{DIRECTORY, IN_USE};
    let mut s = Synth::with_records(8, 40, 36);
    let si = || res(0x10, "", &[0x10; 72], 0);
    let root = fref(5, 5);
    let docs = fref(16, 1);
    let [vcn0, vcn1] = tree_root_nodes();
    s.put_clusters(TREE_ROOT_INDX_LCN, &vcn0);
    s.put_clusters(TREE_ROOT_INDX_LCN + 1, &vcn1);
    let root_rec = record(
        5,
        5,
        IN_USE | DIRECTORY,
        &[
            si(),
            fname_attr(root, ".", 3, true, 1),
            res(
                0x90,
                "$I30",
                &index_root_value(&[index_entry(0, None, Some(0))], 4096, true),
                2,
            ),
            with_id(
                non_resident(
                    0xA0,
                    "$I30",
                    0,
                    1,
                    &encode_runs(&[(Some(TREE_ROOT_INDX_LCN), 2)]),
                    8192,
                    8192,
                    8192,
                ),
                3,
            ),
            res(0xB0, "$I30", &[0b11, 0, 0, 0, 0, 0, 0, 0], 4),
        ],
        1024,
    );
    s.put_record(5, &root_rec);
    let docs_node = indx_block(
        0,
        &[
            index_entry(
                fref(22, 1),
                Some(&file_name_units(docs, &[0x62, 0xD800, 0x63], 1, 0x20, 0)),
                None,
            ),
            named_entry(fref(99, 1), docs, "fuera.txt", 1, None),
            index_entry(
                fref(17, 1),
                Some(&file_name(docs, "Informe.txt", 0, 0x20, 4)),
                None,
            ),
            named_entry(fref(23, 1), docs, "reusado.txt", 1, None),
            named_entry(fref(25, 1), docs, "roto.txt", 1, None),
            index_entry(
                fref(19, 1),
                Some(&file_name(docs, "Sub", 3, 0x1000_0000, 0)),
                None,
            ),
            named_entry(fref(24, 1), docs, "viejo.txt", 1, None),
            named_entry(fref(20, 1), docs, "ñandú.bin", 3, None),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    s.put_clusters(TREE_DOCS_INDX_LCN, &docs_node);
    let docs_index = index_root_value(&[index_entry(0, None, Some(0))], 4096, true);

    s.put_record(
        16,
        &record(
            16,
            1,
            IN_USE | DIRECTORY,
            &[
                si(),
                fname_attr(root, "Docs", 3, true, 1),
                res(0x90, "$I30", &docs_index, 2),
                with_id(
                    non_resident(
                        0xA0,
                        "$I30",
                        0,
                        0,
                        &encode_runs(&[(Some(TREE_DOCS_INDX_LCN), 1)]),
                        4096,
                        4096,
                        4096,
                    ),
                    3,
                ),
                res(0xB0, "$I30", &[1, 0, 0, 0, 0, 0, 0, 0], 4),
            ],
            1024,
        ),
    );
    let mut hard = record(
        17,
        1,
        IN_USE,
        &[
            si(),
            fname_attr(root, "a.txt", 1, false, 1),
            fname_attr(docs, "Informe.txt", 0, false, 2),
            res(0x80, "", b"hola", 3),
        ],
        1024,
    );
    // Dos enlaces.
    crate::record::apply_fixups(&mut hard).unwrap();
    hard[0x12] = 2;
    protect(&mut hard);
    s.put_record(17, &hard);
    s.put_record(
        18,
        &record(
            18,
            1,
            IN_USE,
            &[
                si(),
                fname_attr(root, "Medio.txt", 1, false, 1),
                fname_attr(root, "MEDIO~1.TXT", 2, false, 2),
                res(0x80, "", b"medio", 3),
            ],
            1024,
        ),
    );
    let sub_index = index_root_value(
        &[
            named_entry(fref(21, 1), fref(19, 1), "deep.txt", 3, None),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    s.put_record(
        19,
        &record(
            19,
            1,
            IN_USE | DIRECTORY,
            &[
                si(),
                fname_attr(docs, "Sub", 3, true, 1),
                res(0x90, "$I30", &sub_index, 2),
            ],
            1024,
        ),
    );
    let data = tree_data();
    let mut cl = data.clone();
    cl.resize(4096, 0);
    s.put_clusters(TREE_DATA_LCN, &cl);
    s.put_record(
        20,
        &record(
            20,
            1,
            IN_USE,
            &[
                si(),
                fname_attr(docs, "ñandú.bin", 3, false, 1),
                with_id(
                    non_resident(
                        0x80,
                        "",
                        0,
                        0,
                        &encode_runs(&[(Some(TREE_DATA_LCN), 1)]),
                        4096,
                        data.len() as u64,
                        data.len() as u64,
                    ),
                    2,
                ),
            ],
            1024,
        ),
    );
    s.put_record(
        21,
        &record(
            21,
            1,
            IN_USE,
            &[
                si(),
                fname_attr(fref(19, 1), "deep.txt", 3, false, 1),
                res(0x80, "", b"profundo", 2),
            ],
            1024,
        ),
    );
    s.put_record(
        22,
        &record(
            22,
            1,
            IN_USE,
            &[
                si(),
                res(
                    0x30,
                    "",
                    &file_name_units(docs, &[0x62, 0xD800, 0x63], 1, 0x20, 0),
                    1,
                ),
                res(0x80, "", b"sustituto", 2),
            ],
            1024,
        ),
    );
    s.put_record(
        23,
        &record(
            23,
            2,
            IN_USE,
            &[
                si(),
                fname_attr(docs, "otro.txt", 1, false, 1),
                res(0x80, "", b"otro", 2),
            ],
            1024,
        ),
    );
    s.put_record(
        24,
        &record(
            24,
            2,
            0,
            &[si(), fname_attr(docs, "viejo.txt", 1, false, 1)],
            1024,
        ),
    );
    let mut baad = record(25, 1, IN_USE, &[si()], 1024);
    baad[..4].copy_from_slice(b"BAAD");
    s.put_record(25, &baad);
    s.put_record(
        26,
        &record(
            26,
            1,
            IN_USE,
            &[si(), fname_attr(fref(21, 1), "colgado.txt", 1, false, 1)],
            1024,
        ),
    );
    s.put_record(
        27,
        &record(
            27,
            1,
            IN_USE,
            &[si(), fname_attr(fref(24, 1), "perdido.txt", 1, false, 1)],
            1024,
        ),
    );
    s.put_record(
        28,
        &record(
            28,
            1,
            IN_USE,
            &[
                si(),
                fname_attr(root, "Ágil.txt", 3, false, 1),
                res(0x80, "", b"agil", 2),
            ],
            1024,
        ),
    );
    if with_upcase {
        s.put_clusters(TREE_UPCASE_LCN, &upcase_table());
        s.put_record(
            10,
            &record(
                10,
                10,
                IN_USE,
                &[
                    si(),
                    fname_attr(root, "$UpCase", 3, false, 1),
                    with_id(
                        non_resident(
                            0x80,
                            "",
                            0,
                            31,
                            &encode_runs(&[(Some(TREE_UPCASE_LCN), 32)]),
                            131_072,
                            131_072,
                            131_072,
                        ),
                        2,
                    ),
                ],
                1024,
            ),
        );
    }
    s
}

/// Tabla `$UpCase` de pruebas: identidad salvo ASCII, `á`, `ñ` y `ú`.
pub fn upcase_table() -> Vec<u8> {
    let mut t: Vec<u16> = (0..=u16::MAX).collect();
    for c in u16::from(b'a')..=u16::from(b'z') {
        t[usize::from(c)] = c - 32;
    }
    t[0xE1] = 0xC1;
    t[0xF1] = 0xD1;
    t[0xFA] = 0xDA;
    t.iter().flat_map(|x| x.to_le_bytes()).collect()
}

// ---------------------------------------------------------------------------------------------
// Entradas residuales de los índices (#23)
// ---------------------------------------------------------------------------------------------

/// Nodo `INDX` como [`indx_block`] con `slack` justo después de los bytes en uso (dentro de los
/// asignados), antes de proteger el nodo.
pub fn indx_block_with_slack(
    vcn: u64,
    entries: &[Vec<u8>],
    slack: &[u8],
    block_size: usize,
    has_subnodes: bool,
) -> Vec<u8> {
    let mut b = indx_block(vcn, entries, block_size, has_subnodes);
    crate::record::apply_fixups(&mut b).expect("nodo de prueba");
    let used = 0x18 + u32::from_le_bytes([b[0x1C], b[0x1D], b[0x1E], b[0x1F]]) as usize;
    b[used..used + slack.len()].copy_from_slice(slack);
    protect(&mut b);
    b
}

/// Primer clúster de los 4 nodos `INDX` de la raíz en [`residual_volume`].
pub const RESIDUAL_INDX_LCN: u64 = 230;

/// Entradas mal formadas para el espacio libre del directorio `dir`: longitud incoherente, padre
/// de otro directorio, referencia fuera de la MFT y a un registro del sistema (las dos únicas
/// rechazadas: el resto no son candidatas), clave más larga que el nombre, indicadores
/// desconocidos y nombre con una unidad nula.
pub fn malformed_slack(dir: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = named_entry(fref(30, 1), dir, "largo.txt", 1, None);
    e[8] = e[8].wrapping_add(8);
    e.extend_from_slice(&[0; 8]);
    out.extend(e);
    out.extend(named_entry(fref(31, 1), fref(16, 2), "ajeno.txt", 1, None));
    out.extend(named_entry(fref(999, 1), dir, "fuera.txt", 1, None));
    out.extend(named_entry(fref(3, 3), dir, "sistema.txt", 1, None));
    let mut k = file_name(dir, "corto.txt", 1, 0x20, 0);
    k.extend_from_slice(&[0; 8]);
    out.extend(index_entry(fref(32, 1), Some(&k), None));
    let mut e = named_entry(fref(33, 1), dir, "raro.txt", 1, None);
    e[0x0C] = 0x10;
    out.extend(e);
    out.extend(index_entry(
        fref(34, 1),
        Some(&file_name_units(dir, &[0x61, 0, 0x62], 1, 0x20, 0)),
        None,
    ));
    out
}

/// Volumen con entradas residuales (clústeres de 4 KiB, nodos de 4 KiB, 40 registros):
///
/// ```text
/// raíz (5): $INDEX_ROOT grande → VCN 0; $INDEX_ALLOCATION de 4 nodos en el clúster 230;
///           $BITMAP 0b0001 (solo el VCN 0 asignado)
///   VCN 0 (asignado): en uso «vivo.txt» (17); espacio libre: «borrado.txt» (18) dos veces,
///         «reusado.txt» (19 sec. 1), «dañado.txt» (20), copia de «vivo.txt» y entradas mal
///         formadas (malformed_slack)
///   VCN 1 (libre, nodo válido): «borrado.txt» (18), «libre.txt» (21 sec. 3),
///         «DOSLAR~1.TXT» y «dos largo.txt» (23)
///   VCN 2 (libre): «secreto.txt» (26); las pruebas lo hacen ilegible con FaultyDevice
///   VCN 3 (libre, fixups que no cuadran): «roto.txt» (27)
/// 16 «Viejo»: directorio borrado (sec. 2); su $INDEX_ROOT lista «hijo.txt» (24)
/// 17 «vivo.txt» en uso; 18 «borrado.txt» no en uso (sec. 2, residente); 19 en uso con sec. 2
/// («nuevo.txt»); 20 y 23 BAAD; 21 no en uso sec. 4 sin $FILE_NAME; 24 «hijo.txt» no en uso
/// (sec. 2, padre 16)
/// ```
pub fn residual_volume() -> Synth {
    use crate::record::flags::{DIRECTORY, IN_USE};
    let mut s = Synth::with_records(8, 40, 36);
    let si = || res(0x10, "", &[0x10; 72], 0);
    let root = fref(5, 5);
    let vivo = named_entry(fref(17, 1), root, "vivo.txt", 1, None);
    let borrado = named_entry(fref(18, 1), root, "borrado.txt", 1, None);
    let mut slack = Vec::new();
    slack.extend_from_slice(&borrado);
    slack.extend_from_slice(&borrado);
    slack.extend(named_entry(fref(19, 1), root, "reusado.txt", 1, None));
    slack.extend(named_entry(fref(20, 1), root, "dañado.txt", 1, None));
    slack.extend_from_slice(&vivo);
    slack.extend(malformed_slack(root));
    let vcn0 = indx_block_with_slack(0, &[vivo, index_entry(0, None, None)], &slack, 4096, false);
    let vcn1 = indx_block(
        1,
        &[
            borrado.clone(),
            named_entry(fref(21, 3), root, "libre.txt", 1, None),
            named_entry(fref(23, 1), root, "DOSLAR~1.TXT", 2, None),
            named_entry(fref(23, 1), root, "dos largo.txt", 1, None),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    let vcn2 = indx_block(
        2,
        &[
            named_entry(fref(26, 1), root, "secreto.txt", 1, None),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    let mut vcn3 = indx_block(
        3,
        &[
            named_entry(fref(27, 1), root, "roto.txt", 1, None),
            index_entry(0, None, None),
        ],
        4096,
        false,
    );
    vcn3[1022] ^= 0xFF;
    for (i, n) in [vcn0, vcn1, vcn2, vcn3].iter().enumerate() {
        s.put_clusters(RESIDUAL_INDX_LCN + i as u64, n);
    }
    s.put_record(
        5,
        &record(
            5,
            5,
            IN_USE | DIRECTORY,
            &[
                si(),
                fname_attr(root, ".", 3, true, 1),
                res(
                    0x90,
                    "$I30",
                    &index_root_value(&[index_entry(0, None, Some(0))], 4096, true),
                    2,
                ),
                with_id(
                    non_resident(
                        0xA0,
                        "$I30",
                        0,
                        3,
                        &encode_runs(&[(Some(RESIDUAL_INDX_LCN), 4)]),
                        16384,
                        16384,
                        16384,
                    ),
                    3,
                ),
                res(0xB0, "$I30", &[0b0001, 0, 0, 0, 0, 0, 0, 0], 4),
            ],
            1024,
        ),
    );
    let viejo = fref(16, 1);
    s.put_record(
        16,
        &record(
            16,
            2,
            DIRECTORY,
            &[
                si(),
                fname_attr(root, "Viejo", 3, true, 1),
                res(
                    0x90,
                    "$I30",
                    &index_root_value(
                        &[
                            named_entry(fref(24, 1), viejo, "hijo.txt", 1, None),
                            index_entry(0, None, None),
                        ],
                        4096,
                        false,
                    ),
                    2,
                ),
            ],
            1024,
        ),
    );
    let file = |n: u32, seq: u16, flags: u16, parent: u64, name: &str, data: &[u8]| {
        record(
            n,
            seq,
            flags,
            &[
                si(),
                fname_attr(parent, name, 1, false, 1),
                res(0x80, "", data, 2),
            ],
            1024,
        )
    };
    s.put_record(17, &file(17, 1, IN_USE, root, "vivo.txt", b"vivo"));
    s.put_record(18, &file(18, 2, 0, root, "borrado.txt", b"hola borrado"));
    s.put_record(19, &file(19, 2, IN_USE, root, "nuevo.txt", b"de otro"));
    for n in [20u32, 23] {
        let mut baad = record(n, 1, IN_USE, &[si()], 1024);
        baad[..4].copy_from_slice(b"BAAD");
        s.put_record(u64::from(n), &baad);
    }
    s.put_record(21, &record(21, 4, 0, &[si()], 1024));
    s.put_record(24, &file(24, 2, 0, viejo, "hijo.txt", b"hijo"));
    s
}
