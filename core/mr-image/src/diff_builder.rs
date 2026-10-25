//! Constructor de imágenes sintéticas VHD y VHDX (dinámicas y diferenciales) según las
//! especificaciones públicas, para las pruebas de la incidencia 8. Solo usa `std`: lo incluyen las
//! pruebas unitarias del crate y las de integración (`#[path]`). El CRC-32C se recibe como función
//! para no depender de la ruta del crate.
//!
//! Las escrituras de una capa se dan como `sector → 512 bytes`. Los sectores de un bloque asignado
//! que la capa **no** escribe se rellenan con basura (`0xEE`) y su bit queda a 0: si el lector los
//! entregara en lugar de los del padre (o de los ceros), las pruebas lo detectarían.

#![allow(dead_code)]

use std::collections::BTreeMap;

pub const SECTOR: u64 = 512;
const MIB: u64 = 1024 * 1024;
pub const GARBAGE: u8 = 0xEE;

/// Escrituras de una capa: sector (de 512 B) → contenido.
pub type Writes = BTreeMap<u64, Vec<u8>>;

/// Contenido determinista de un sector escrito por la capa `layer`.
pub fn sector_data(layer: u8, sector: u64) -> Vec<u8> {
    (0..SECTOR)
        .map(|i| {
            let x = sector
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(i * 31)
                .wrapping_add(u64::from(layer) << 40);
            ((x >> 24) as u8) | 1
        })
        .collect()
}

/// Escrituras con [`sector_data`] en los sectores indicados.
pub fn writes(layer: u8, sectors: impl IntoIterator<Item = u64>) -> Writes {
    sectors
        .into_iter()
        .map(|s| (s, sector_data(layer, s)))
        .collect()
}

/// Aplica las escrituras sobre una imagen plana (la referencia esperada).
pub fn apply(flat: &mut [u8], w: &Writes) {
    for (&s, d) in w {
        let o = (s * SECTOR) as usize;
        flat[o..o + d.len()].copy_from_slice(d);
    }
}

// ---------------------------------------------------------------------------------------------
// VHD
// ---------------------------------------------------------------------------------------------

/// Padre de un VHD diferencial.
#[derive(Debug, Clone)]
pub struct VhdParent {
    pub id: [u8; 16],
    pub timestamp: u32,
    pub name: String,
    /// Localizadores (código de plataforma, datos ya codificados).
    pub locators: Vec<([u8; 4], Vec<u8>)>,
}

impl VhdParent {
    pub fn new(id: [u8; 16], name: &str) -> Self {
        Self {
            id,
            timestamp: 0x1234_5678,
            name: name.to_string(),
            locators: vec![
                (*b"W2ru", utf16le(&format!(".\\{name}"))),
                (*b"W2ku", utf16le(&format!("C:\\vm\\{name}"))),
            ],
        }
    }
}

pub fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn vhd_checksum(b: &[u8], field: usize) -> u32 {
    !b.iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |a, (_, &x)| a.wrapping_add(u32::from(x)))
}

/// Recalcula la suma de control de una estructura VHD (pie: campo 64; cabecera dinámica: 36).
pub fn vhd_seal(b: &mut [u8], field: usize) {
    let c = vhd_checksum(b, field);
    b[field..field + 4].copy_from_slice(&c.to_be_bytes());
}

fn vhd_footer(disk_type: u32, size: u64, data_offset: u64, unique_id: [u8; 16]) -> [u8; 512] {
    let mut f = [0u8; 512];
    f[..8].copy_from_slice(b"conectix");
    f[8..12].copy_from_slice(&2u32.to_be_bytes());
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[16..24].copy_from_slice(&data_offset.to_be_bytes());
    f[28..32].copy_from_slice(b"mrsc");
    f[40..48].copy_from_slice(&size.to_be_bytes());
    f[48..56].copy_from_slice(&size.to_be_bytes());
    f[60..64].copy_from_slice(&disk_type.to_be_bytes());
    f[68..84].copy_from_slice(&unique_id);
    vhd_seal(&mut f, 64);
    f
}

/// VHD dinámico (`parent = None`) o diferencial con bloques de `block_size` bytes.
pub fn vhd_layer(
    size: u64,
    block_size: u32,
    unique_id: [u8; 16],
    parent: Option<&VhdParent>,
    w: &Writes,
) -> Vec<u8> {
    let bs = u64::from(block_size);
    let entries = size.div_ceil(bs) as usize;
    let disk_type = if parent.is_some() { 4u32 } else { 3 };
    let bat_off = 1536u64;
    let bat_len = (entries * 4).div_ceil(512) * 512;
    let mut img = vec![0u8; bat_off as usize + bat_len];
    let ft = vhd_footer(disk_type, size, 512, unique_id);
    img[..512].copy_from_slice(&ft);
    // Datos de los localizadores tras la BAT.
    let mut locs: Vec<([u8; 4], u64, u32)> = Vec::new();
    if let Some(p) = parent {
        for (code, data) in &p.locators {
            let off = img.len() as u64;
            let mut d = data.clone();
            d.resize(data.len().div_ceil(512) * 512, 0);
            img.extend_from_slice(&d);
            locs.push((*code, off, data.len() as u32));
        }
    }
    let mut h = vec![0u8; 1024];
    h[..8].copy_from_slice(b"cxsparse");
    h[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    h[16..24].copy_from_slice(&bat_off.to_be_bytes());
    h[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    h[28..32].copy_from_slice(&(entries as u32).to_be_bytes());
    h[32..36].copy_from_slice(&block_size.to_be_bytes());
    if let Some(p) = parent {
        h[40..56].copy_from_slice(&p.id);
        h[56..60].copy_from_slice(&p.timestamp.to_be_bytes());
        for (i, u) in p.name.encode_utf16().take(255).enumerate() {
            h[64 + 2 * i..66 + 2 * i].copy_from_slice(&u.to_be_bytes());
        }
        for (i, (code, off, len)) in locs.iter().enumerate() {
            let o = 576 + i * 24;
            h[o..o + 4].copy_from_slice(code);
            h[o + 4..o + 8].copy_from_slice(&len.div_ceil(512).to_be_bytes());
            h[o + 8..o + 12].copy_from_slice(&len.to_be_bytes());
            h[o + 16..o + 24].copy_from_slice(&off.to_be_bytes());
        }
    }
    vhd_seal(&mut h, 36);
    img[512..1536].copy_from_slice(&h);
    let mut bat = vec![0xFFu8; bat_len];
    let spb = bs / SECTOR;
    let bitmap_len = (spb.div_ceil(8).div_ceil(512) * 512) as usize;
    let blocks: std::collections::BTreeSet<u64> = w.keys().map(|s| s / spb).collect();
    for b in blocks {
        let sector = (img.len() as u64 / 512) as u32;
        bat[b as usize * 4..b as usize * 4 + 4].copy_from_slice(&sector.to_be_bytes());
        let mut map = vec![0u8; bitmap_len];
        let mut data = vec![GARBAGE; bs as usize];
        for k in 0..spb {
            // Bit a 0: ceros (dinámico) o padre (diferencial); se deja basura para comprobar que
            // no se lee.
            if let Some(d) = w.get(&(b * spb + k)) {
                map[(k / 8) as usize] |= 0x80 >> (k % 8);
                data[(k * SECTOR) as usize..((k + 1) * SECTOR) as usize].copy_from_slice(d);
            }
        }
        img.extend_from_slice(&map);
        img.extend_from_slice(&data);
    }
    img[bat_off as usize..bat_off as usize + bat_len].copy_from_slice(&bat);
    img.extend_from_slice(&ft);
    img
}

/// Id único del pie de un VHD.
pub fn vhd_unique_id(img: &[u8]) -> [u8; 16] {
    let f = &img[img.len() - 512..];
    let mut id = [0u8; 16];
    id.copy_from_slice(&f[68..84]);
    id
}

// ---------------------------------------------------------------------------------------------
// VHDX
// ---------------------------------------------------------------------------------------------

/// Padre de un VHDX diferencial (valores del localizador).
#[derive(Debug, Clone)]
pub struct VhdxParent {
    /// Pares clave/valor del localizador, en orden.
    pub entries: Vec<(String, String)>,
}

impl VhdxParent {
    /// Localizador habitual: `parent_linkage` con el DataWriteGuid del padre y rutas.
    pub fn new(data_write_guid: [u8; 16], name: &str) -> Self {
        Self {
            entries: vec![
                ("parent_linkage".into(), guid_text(&data_write_guid)),
                ("relative_path".into(), format!(".\\{name}")),
                ("volume_path".into(), format!("\\\\?\\Volume{{0}}\\{name}")),
                ("absolute_win32_path".into(), format!("C:\\vm\\{name}")),
            ],
        }
    }
}

/// GUID en orden de disco como texto `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

const fn g(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const REGION_BAT: [u8; 16] = g(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const REGION_META: [u8; 16] = g(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const META_PARAMS: [u8; 16] = g(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const META_SIZE: [u8; 16] = g(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const META_DISK_ID: [u8; 16] = g(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const META_LOGICAL: [u8; 16] = g(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const META_PHYSICAL: [u8; 16] = g(
    0xCDA3_48C7,
    0x445D,
    0x4471,
    [0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56],
);
const META_LOCATOR: [u8; 16] = g(
    0xA8D3_5F2D,
    0xB30B,
    0x454D,
    [0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C],
);
/// Tipo de localizador de padre de VHDX.
pub const LOCATOR_TYPE_VHDX: [u8; 16] = g(
    0xB04A_EFB7,
    0xD19E,
    0x4A81,
    [0xB7, 0x89, 0x25, 0xB8, 0xE9, 0x44, 0x59, 0x13],
);

pub const VHDX_META_OFF: u64 = 2 * MIB;
pub const VHDX_BAT_OFF: u64 = 3 * MIB;

/// Estados de bloque que se pueden forzar (bloques sin escrituras).
pub const PAYLOAD_UNDEFINED: u64 = 1;
pub const PAYLOAD_ZERO: u64 = 2;
pub const PAYLOAD_UNMAPPED: u64 = 3;

/// Elemento de metadatos del localizador de padre.
pub fn vhdx_locator_item(p: &VhdxParent) -> Vec<u8> {
    let mut v = LOCATOR_TYPE_VHDX.to_vec();
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&(p.entries.len() as u16).to_le_bytes());
    let mut strings = Vec::new();
    let base = 20 + 12 * p.entries.len();
    for (k, x) in &p.entries {
        let (k, x) = (utf16le(k), utf16le(x));
        let ko = base + strings.len();
        strings.extend_from_slice(&k);
        let vo = base + strings.len();
        strings.extend_from_slice(&x);
        v.extend_from_slice(&(ko as u32).to_le_bytes());
        v.extend_from_slice(&(vo as u32).to_le_bytes());
        v.extend_from_slice(&(k.len() as u16).to_le_bytes());
        v.extend_from_slice(&(x.len() as u16).to_le_bytes());
    }
    v.extend_from_slice(&strings);
    v
}

fn put(img: &mut [u8], off: u64, d: &[u8]) {
    img[off as usize..off as usize + d.len()].copy_from_slice(d);
}

fn seal_crc(img: &mut [u8], off: u64, len: usize, crc: fn(&[u8]) -> u32) {
    let o = off as usize;
    img[o + 4..o + 8].fill(0);
    let c = crc(&img[o..o + len]);
    img[o + 4..o + 8].copy_from_slice(&c.to_le_bytes());
}

/// VHDX dinámico (`parent = None`) o diferencial, sector lógico de 512 B y bloques de
/// `block_size` (≥ 1 MiB). Bloques con escrituras: FULLY_PRESENT si la capa escribe todos sus
/// sectores (o no tiene padre), si no PARTIALLY_PRESENT con su mapa de bits; sin escrituras,
/// NOT_PRESENT salvo lo indicado en `states`.
#[allow(clippy::too_many_arguments)]
pub fn vhdx_layer(
    size: u64,
    block_size: u32,
    data_write_guid: [u8; 16],
    parent: Option<&VhdxParent>,
    w: &Writes,
    states: &[(u64, u64)],
    crc: fn(&[u8]) -> u32,
) -> Vec<u8> {
    let tb = u64::from(block_size);
    let n = size.div_ceil(tb);
    let ratio = (1u64 << 23) * SECTOR / tb;
    let entries = if parent.is_some() {
        n.div_ceil(ratio) * (ratio + 1)
    } else {
        n + (n - 1) / ratio
    };
    let bat_len = (entries * 8).div_ceil(MIB).max(1) * MIB;
    let mut next = VHDX_BAT_OFF + bat_len;
    let spb = tb / SECTOR;
    let mut bat = vec![0u64; entries as usize];
    // Mapas de bits por grupo (solo si hacen falta) y bloques.
    let mut bitmaps: BTreeMap<u64, (u64, Vec<u8>)> = BTreeMap::new();
    let mut blocks: Vec<(u64, Vec<u8>)> = Vec::new();
    let written: std::collections::BTreeSet<u64> = w.keys().map(|s| s / spb).collect();
    for b in 0..n {
        let idx = (b + b / ratio) as usize;
        if !written.contains(&b) {
            if let Some(&(_, st)) = states.iter().find(|(x, _)| *x == b) {
                bat[idx] = st;
            }
            continue;
        }
        let mut data = vec![GARBAGE; tb as usize];
        let mut all = true;
        for k in 0..spb {
            match w.get(&(b * spb + k)) {
                Some(d) => {
                    data[(k * SECTOR) as usize..((k + 1) * SECTOR) as usize].copy_from_slice(d)
                }
                None if parent.is_none() => {
                    data[(k * SECTOR) as usize..((k + 1) * SECTOR) as usize].fill(0)
                }
                None => all = false,
            }
        }
        let off = next;
        next += tb;
        blocks.push((off, data));
        if all {
            bat[idx] = 6 | ((off / MIB) << 20);
        } else {
            bat[idx] = 7 | ((off / MIB) << 20);
            let c = b / ratio;
            let entry = bitmaps
                .entry(c)
                .or_insert_with(|| (0, vec![0u8; MIB as usize]));
            for k in 0..spb {
                if w.contains_key(&(b * spb + k)) {
                    let bit = (b % ratio) * spb + k;
                    entry.1[(bit / 8) as usize] |= 1 << (bit % 8);
                }
            }
        }
    }
    for (c, e) in bitmaps.iter_mut() {
        e.0 = next;
        next += MIB;
        let idx = (c * (ratio + 1) + ratio) as usize;
        bat[idx] = 6 | ((e.0 / MIB) << 20);
    }
    let mut img = vec![0u8; next as usize];
    put(&mut img, 0, b"vhdxfile");
    for (i, off) in [64 * 1024u64, 128 * 1024].into_iter().enumerate() {
        put(&mut img, off, b"head");
        put(&mut img, off + 8, &(i as u64 + 1).to_le_bytes());
        put(&mut img, off + 16, &[0x11; 16]);
        put(&mut img, off + 32, &data_write_guid);
        put(&mut img, off + 66, &1u16.to_le_bytes());
        put(&mut img, off + 68, &(MIB as u32).to_le_bytes());
        put(&mut img, off + 72, &MIB.to_le_bytes());
        seal_crc(&mut img, off, 4096, crc);
    }
    for off in [192 * 1024u64, 256 * 1024] {
        put(&mut img, off, b"regi");
        put(&mut img, off + 8, &2u32.to_le_bytes());
        put(&mut img, off + 16, &REGION_BAT);
        put(&mut img, off + 32, &VHDX_BAT_OFF.to_le_bytes());
        put(&mut img, off + 40, &(bat_len as u32).to_le_bytes());
        put(&mut img, off + 44, &1u32.to_le_bytes());
        put(&mut img, off + 48, &REGION_META);
        put(&mut img, off + 64, &VHDX_META_OFF.to_le_bytes());
        put(&mut img, off + 72, &(MIB as u32).to_le_bytes());
        put(&mut img, off + 76, &1u32.to_le_bytes());
        seal_crc(&mut img, off, 64 * 1024, crc);
    }
    let flags: u32 = if parent.is_some() { 2 } else { 0 };
    let mut params = block_size.to_le_bytes().to_vec();
    params.extend_from_slice(&flags.to_le_bytes());
    let mut items: Vec<([u8; 16], u32, Vec<u8>)> = vec![
        (META_PARAMS, 4, params),
        (META_SIZE, 6, size.to_le_bytes().to_vec()),
        (META_LOGICAL, 6, 512u32.to_le_bytes().to_vec()),
        (META_PHYSICAL, 6, 4096u32.to_le_bytes().to_vec()),
        (META_DISK_ID, 6, vec![0x33; 16]),
    ];
    if let Some(p) = parent {
        items.push((META_LOCATOR, 4, vhdx_locator_item(p)));
    }
    put(&mut img, VHDX_META_OFF, b"metadata");
    put(
        &mut img,
        VHDX_META_OFF + 10,
        &(items.len() as u16).to_le_bytes(),
    );
    let mut pos = 64 * 1024u64;
    for (i, (id, fl, content)) in items.iter().enumerate() {
        let e = VHDX_META_OFF + 32 + i as u64 * 32;
        put(&mut img, e, id);
        put(&mut img, e + 16, &(pos as u32).to_le_bytes());
        put(&mut img, e + 20, &(content.len() as u32).to_le_bytes());
        put(&mut img, e + 24, &fl.to_le_bytes());
        put(&mut img, VHDX_META_OFF + pos, content);
        pos += content.len().div_ceil(8) as u64 * 8;
    }
    for (i, e) in bat.iter().enumerate() {
        put(&mut img, VHDX_BAT_OFF + i as u64 * 8, &e.to_le_bytes());
    }
    for (off, data) in &blocks {
        put(&mut img, *off, data);
    }
    for (off, map) in bitmaps.values() {
        put(&mut img, *off, map);
    }
    img
}

/// Desplazamiento en el fichero del elemento de metadatos `i` de una imagen de [`vhdx_layer`].
pub fn vhdx_meta_item(img: &[u8], i: u64) -> u64 {
    let e = (VHDX_META_OFF + 32 + i * 32) as usize;
    VHDX_META_OFF
        + u64::from(u32::from_le_bytes([
            img[e + 16],
            img[e + 17],
            img[e + 18],
            img[e + 19],
        ]))
}

/// DataWriteGuid de la cabecera activa (la válida de mayor secuencia; sin comprobar el CRC).
pub fn vhdx_data_write_guid(img: &[u8]) -> [u8; 16] {
    let seq = |o: usize| u64::from_le_bytes(img[o + 8..o + 16].try_into().unwrap());
    let o = if seq(128 * 1024) > seq(64 * 1024) {
        128 * 1024
    } else {
        64 * 1024
    };
    img[o + 32..o + 48].try_into().unwrap()
}
