//! VHD (Microsoft Virtual Hard Disk) fijo y dinámico, de **solo lectura**.
//!
//! Referencia: Microsoft «Virtual Hard Disk Image Format Specification», versión 1.0 (pública).
//! Ver `docs/formatos/vhd.md` para las decisiones de diseño y las validaciones.
//!
//! Estructuras (todos los enteros en big-endian):
//! - Pie de 512 bytes al final del fichero (511 en algunos VHD antiguos), cookie `conectix`.
//!   Los discos dinámicos y diferenciales llevan una copia del pie en el byte 0.
//! - Fijo: los datos son `fichero[0..current_size]` y el pie va a continuación.
//! - Dinámico: cabecera dinámica de 1024 bytes (`cxsparse`) en `data_offset`, BAT de
//!   `max_table_entries` entradas u32 (sector del bloque o `0xFFFFFFFF` si no está asignado) y,
//!   por cada bloque asignado, un mapa de bits de sectores seguido de los datos del bloque.
//! - Diferencial: necesita el disco padre; nunca se abre (`IoError::ExternalReference`).
//!
//! Modo tolerante ([`VhdImage::open_with`], ADR-020): las entradas de la BAT cuyo bloque queda
//! fuera de la zona de datos, solapa la copia del pie, la cabecera dinámica, la BAT u otro bloque
//! (incluidas las entradas duplicadas) se descartan: sus bloques se leen con error y se informan
//! como datos no fiables. El pie, la cabecera dinámica y la ubicación de la BAT siguen siendo
//! críticos (`Corrupt` en ambos modos).

use crate::tolerant::{self, Damage, Zone};
use mr_io::{Allocation, AllocationRun, BlockDevice, IoError, Result};
use std::fs::File;
use std::sync::{Arc, Mutex};

const FORMAT: &str = "VHD";
const SECTOR: u64 = 512;
const FOOTER_LEN: usize = 512;
const OLD_FOOTER_LEN: usize = 511;
const FOOTER_COOKIE: &[u8; 8] = b"conectix";
const DYN_COOKIE: &[u8; 8] = b"cxsparse";
const DYN_HEADER_LEN: usize = 1024;
/// Entrada de BAT de un bloque no asignado.
const UNALLOCATED: u32 = 0xFFFF_FFFF;
/// Marca interna de un bloque descartado en modo tolerante. El sector 0 del fichero es siempre la
/// copia del pie en un disco dinámico, así que ninguna entrada válida puede valer 0.
const DISCARDED: u32 = 0;
const MIN_BLOCK_SIZE: u64 = 512;
const MAX_BLOCK_SIZE: u64 = 256 * 1024 * 1024;

// Desplazamientos de los campos del pie.
const F_VERSION: usize = 12;
const F_DATA_OFFSET: usize = 16;
const F_CURRENT_SIZE: usize = 48;
const F_DISK_TYPE: usize = 60;
const F_CHECKSUM: usize = 64;

// Desplazamientos de los campos de la cabecera dinámica.
const H_TABLE_OFFSET: usize = 16;
const H_VERSION: usize = 24;
const H_MAX_ENTRIES: usize = 28;
const H_BLOCK_SIZE: usize = 32;
const H_CHECKSUM: usize = 36;
const H_PARENT_ID: usize = 40;
const H_PARENT_NAME: usize = 64;
const H_PARENT_NAME_LEN: usize = 512;

const DISK_FIXED: u32 = 2;
const DISK_DYNAMIC: u32 = 3;
const DISK_DIFFERENCING: u32 = 4;

fn corrupt(detail: impl Into<String>) -> IoError {
    IoError::Corrupt {
        format: FORMAT,
        detail: detail.into(),
    }
}

fn unsupported(detail: impl Into<String>) -> IoError {
    IoError::UnsupportedFeature {
        format: FORMAT,
        detail: detail.into(),
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[o..o + 4]);
    u32::from_be_bytes(a)
}

fn be64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

/// Suma de control del VHD: complemento a uno de la suma de todos los bytes de la estructura,
/// excluyendo los 4 bytes del propio campo de suma de control.
fn vhd_checksum(buf: &[u8], field: usize) -> u32 {
    let sum = buf
        .iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |acc, (_, &b)| acc.wrapping_add(u32::from(b)));
    !sum
}

/// Lee estructuras del contenedor; el error lleva el desplazamiento dentro del fichero.
fn read_meta(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    mr_io::read_exact_at(file, offset, buf).map_err(|source| IoError::Read {
        offset,
        len: buf.len(),
        source,
    })
}

/// `true` si los intervalos semiabiertos `[a, a+al)` y `[b, b+bl)` se solapan.
fn overlaps(a: u64, al: u64, b: u64, bl: u64) -> bool {
    a < b.saturating_add(bl) && b < a.saturating_add(al)
}

/// Tamaño legible (2 MiB, 512 KiB, 4096 bytes…).
fn human_size(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if n >= MIB && n.is_multiple_of(MIB) {
        format!("{} MiB", n / MIB)
    } else if n >= KIB && n.is_multiple_of(KIB) {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} bytes")
    }
}

/// Campos del pie que se usan.
#[derive(Debug, Clone, Copy)]
struct Footer {
    data_offset: u64,
    current_size: u64,
    disk_type: u32,
}

/// Valida cookie, suma de control y versión de un pie de 512 bytes.
fn parse_footer(b: &[u8; FOOTER_LEN]) -> std::result::Result<Footer, String> {
    if &b[..8] != FOOTER_COOKIE {
        return Err("falta la cookie «conectix»".into());
    }
    let stored = be32(b, F_CHECKSUM);
    let computed = vhd_checksum(b, F_CHECKSUM);
    if stored != computed {
        return Err(format!(
            "suma de control incorrecta (guardada {stored:#010x}, calculada {computed:#010x})"
        ));
    }
    let version = be32(b, F_VERSION);
    if version >> 16 != 1 {
        return Err(format!("versión de formato no reconocida {version:#010x}"));
    }
    Ok(Footer {
        data_offset: be64(b, F_DATA_OFFSET),
        current_size: be64(b, F_CURRENT_SIZE),
        disk_type: be32(b, F_DISK_TYPE),
    })
}

/// Origen del pie que se ha aceptado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterSource {
    /// Pie de 512 bytes al final del fichero.
    End512,
    /// Pie antiguo de 511 bytes al final del fichero.
    End511,
    /// Copia del pie en el byte 0 (solo dinámicos/diferenciales), porque el del final está dañado.
    Copy,
}

/// Cabecera dinámica validada.
struct DynHeader {
    table_offset: u64,
    max_entries: u32,
    block_size: u32,
    parent_id: [u8; 16],
    parent_name: String,
}

fn parse_dyn_header(b: &[u8; DYN_HEADER_LEN]) -> Result<DynHeader> {
    if &b[..8] != DYN_COOKIE {
        return Err(corrupt(
            "la cabecera dinámica no tiene la cookie «cxsparse»",
        ));
    }
    let stored = be32(b, H_CHECKSUM);
    let computed = vhd_checksum(b, H_CHECKSUM);
    if stored != computed {
        return Err(corrupt(format!(
            "suma de control de la cabecera dinámica incorrecta (guardada {stored:#010x}, calculada {computed:#010x})"
        )));
    }
    let version = be32(b, H_VERSION);
    if version >> 16 != 1 {
        return Err(unsupported(format!(
            "versión de cabecera dinámica {version:#010x}"
        )));
    }
    let mut parent_id = [0u8; 16];
    parent_id.copy_from_slice(&b[H_PARENT_ID..H_PARENT_ID + 16]);
    let units: Vec<u16> = b[H_PARENT_NAME..H_PARENT_NAME + H_PARENT_NAME_LEN]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_be_bytes(c))
        .take_while(|&u| u != 0)
        .collect();
    Ok(DynHeader {
        table_offset: be64(b, H_TABLE_OFFSET),
        max_entries: be32(b, H_MAX_ENTRIES),
        block_size: be32(b, H_BLOCK_SIZE),
        parent_id,
        parent_name: String::from_utf16_lossy(&units),
    })
}

fn format_uuid(id: &[u8; 16]) -> String {
    let h: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Disposición de un VHD dinámico.
struct Dynamic {
    block_size: u64,
    /// Bytes del mapa de bits de sectores al inicio de cada bloque (múltiplo de 512).
    bitmap_bytes: u64,
    /// Entradas de la BAT necesarias para cubrir el tamaño virtual ([`DISCARDED`] = bloque
    /// descartado en modo tolerante).
    bat: Vec<u32>,
    /// Bloques descartados en modo tolerante.
    discarded: u64,
    /// Último mapa de bits leído (índice de bloque, contenido).
    cache: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}

enum Layout {
    Fixed,
    Dynamic(Dynamic),
}

/// Imagen VHD fija o dinámica abierta en solo lectura.
pub struct VhdImage {
    file: File,
    size: u64,
    layout: Layout,
    footer_source: FooterSource,
    tolerant: bool,
}

/// Estado del sector `s` en el mapa de bits (bit más significativo = primer sector del byte).
fn bit(bitmap: &[u8], s: u64) -> bool {
    usize::try_from(s / 8)
        .ok()
        .and_then(|i| bitmap.get(i))
        .is_some_and(|&b| b & (0x80 >> (s % 8)) != 0)
}

/// Fin (exclusivo) del tramo de sectores con el mismo estado `set` que empieza en `start`,
/// sin pasar de `limit`. Avanza byte a byte cuando el byte entero coincide.
fn run_end(bitmap: &[u8], start: u64, limit: u64, set: bool) -> u64 {
    let full = if set { 0xFF } else { 0x00 };
    let mut s = start;
    while s < limit {
        if s.is_multiple_of(8) && s + 8 <= limit {
            let byte = usize::try_from(s / 8).ok().and_then(|i| bitmap.get(i));
            if byte == Some(&full) {
                s += 8;
                continue;
            }
        }
        if bit(bitmap, s) != set {
            break;
        }
        s += 1;
    }
    s
}

/// Añade un tramo fusionándolo con el anterior si es contiguo y del mismo tipo.
fn push_run(runs: &mut Vec<AllocationRun>, lba: u64, n: u64, kind: Allocation) {
    if n == 0 {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.2 == kind && last.0 + last.1 == lba {
            last.1 += n;
            return;
        }
    }
    runs.push((lba, n, kind));
}

impl VhdImage {
    /// Interpreta un VHD ya admitido por la política de rutas y abierto en solo lectura, en modo
    /// estricto.
    pub fn open(file: File) -> Result<VhdImage> {
        Self::open_with(file, false)
    }

    /// Como [`Self::open`]; con `tolerant`, las entradas de la BAT inválidas o ambiguas no impiden
    /// abrir la imagen: sus bloques se descartan (ver la documentación del módulo).
    pub fn open_with(file: File, tolerant: bool) -> Result<VhdImage> {
        let file_size = file.metadata()?.len();
        if file_size < OLD_FOOTER_LEN as u64 {
            return Err(corrupt(format!(
                "fichero demasiado pequeño para contener un pie VHD ({file_size} bytes)"
            )));
        }

        // 1) Pie al final: 512 bytes o, en VHD antiguos, 511 (falta el último byte reservado,
        //    que no interviene en la suma de control).
        let mut problems: Vec<String> = Vec::new();
        let mut found: Option<(Footer, FooterSource)> = None;
        if file_size >= FOOTER_LEN as u64 {
            let mut b = [0u8; FOOTER_LEN];
            read_meta(&file, file_size - FOOTER_LEN as u64, &mut b)?;
            match parse_footer(&b) {
                Ok(f) => found = Some((f, FooterSource::End512)),
                Err(e) => problems.push(format!("pie final: {e}")),
            }
        }
        if found.is_none() {
            let mut b = [0u8; FOOTER_LEN];
            read_meta(
                &file,
                file_size - OLD_FOOTER_LEN as u64,
                &mut b[..OLD_FOOTER_LEN],
            )?;
            if &b[..8] == FOOTER_COOKIE {
                match parse_footer(&b) {
                    Ok(f) => found = Some((f, FooterSource::End511)),
                    Err(e) => problems.push(format!("pie final de 511 bytes: {e}")),
                }
            }
        }
        // 2) Copia del pie en el byte 0: solo existe en dinámicos y diferenciales.
        if found.is_none() && file_size >= FOOTER_LEN as u64 {
            let mut b = [0u8; FOOTER_LEN];
            read_meta(&file, 0, &mut b)?;
            match parse_footer(&b) {
                Ok(f) if matches!(f.disk_type, DISK_DYNAMIC | DISK_DIFFERENCING) => {
                    found = Some((f, FooterSource::Copy))
                }
                Ok(f) => problems.push(format!(
                    "la copia del byte 0 declara el tipo {} (solo los dinámicos tienen copia)",
                    f.disk_type
                )),
                Err(e) => problems.push(format!("copia del pie en el byte 0: {e}")),
            }
        }
        let Some((footer, footer_source)) = found else {
            return Err(corrupt(format!(
                "no hay ningún pie válido ({})",
                problems.join("; ")
            )));
        };

        // Fin de la zona de datos: antes del pie final. Si el pie final está dañado se acepta
        // todo el fichero (no se sabe si ese pie llegó a escribirse).
        let data_end = match footer_source {
            FooterSource::End512 => file_size - FOOTER_LEN as u64,
            FooterSource::End511 => file_size - OLD_FOOTER_LEN as u64,
            FooterSource::Copy => file_size,
        };

        let size = footer.current_size;
        if !size.is_multiple_of(SECTOR) {
            return Err(corrupt(format!(
                "el tamaño virtual ({size} bytes) no es múltiplo de 512"
            )));
        }

        let layout = match footer.disk_type {
            DISK_FIXED => {
                if size != data_end {
                    return Err(corrupt(format!(
                        "VHD fijo incoherente: el pie declara {size} bytes de datos pero el fichero mide {file_size} bytes (se esperaba tamaño + pie)"
                    )));
                }
                Layout::Fixed
            }
            DISK_DYNAMIC => {
                Layout::Dynamic(Self::open_dynamic(&file, &footer, data_end, tolerant)?)
            }
            DISK_DIFFERENCING => return Err(Self::differencing_error(&file, &footer, data_end)),
            t @ (0 | 1 | 5 | 6) => {
                return Err(unsupported(format!(
                    "tipo de disco {t} (reservado o «ninguno»)"
                )))
            }
            t => return Err(corrupt(format!("tipo de disco desconocido {t}"))),
        };

        Ok(VhdImage {
            file,
            size,
            layout,
            footer_source,
            tolerant,
        })
    }

    /// Lee y valida la cabecera dinámica situada en `footer.data_offset`.
    fn read_dyn_header(file: &File, footer: &Footer, data_end: u64) -> Result<DynHeader> {
        let off = footer.data_offset;
        let end = off.checked_add(DYN_HEADER_LEN as u64);
        if off < FOOTER_LEN as u64 || end.is_none_or(|e| e > data_end) {
            return Err(corrupt(format!(
                "la cabecera dinámica ({off:#x}) queda fuera del fichero o solapa la copia del pie"
            )));
        }
        let mut b = [0u8; DYN_HEADER_LEN];
        read_meta(file, off, &mut b)?;
        parse_dyn_header(&b)
    }

    /// Error para discos diferenciales: nunca se abre el padre.
    fn differencing_error(file: &File, footer: &Footer, data_end: u64) -> IoError {
        let reference = match Self::read_dyn_header(file, footer, data_end) {
            Ok(h) if !h.parent_name.is_empty() => h.parent_name,
            Ok(h) => format!("disco padre con id {}", format_uuid(&h.parent_id)),
            Err(e) => format!("disco padre desconocido (cabecera dinámica ilegible: {e})"),
        };
        IoError::ExternalReference {
            format: FORMAT,
            reference,
        }
    }

    fn open_dynamic(
        file: &File,
        footer: &Footer,
        data_end: u64,
        tolerant: bool,
    ) -> Result<Dynamic> {
        let h = Self::read_dyn_header(file, footer, data_end)?;
        let size = footer.current_size;

        let block_size = u64::from(h.block_size);
        if !block_size.is_power_of_two() || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
        {
            return Err(corrupt(format!(
                "tamaño de bloque no válido: {block_size} bytes (potencia de 2 entre 512 B y 256 MiB)"
            )));
        }
        let needed = size.div_ceil(block_size);
        if u64::from(h.max_entries) < needed {
            return Err(corrupt(format!(
                "la BAT tiene {} entradas de {} pero el tamaño virtual ({size} bytes) necesita {needed}",
                h.max_entries,
                human_size(block_size)
            )));
        }

        // BAT: dentro del fichero, sin solapar la cabecera, y con memoria acotada.
        let bat_bytes = u64::from(h.max_entries) * 4;
        let bat_end = h.table_offset.checked_add(bat_bytes);
        if h.table_offset < FOOTER_LEN as u64 || bat_end.is_none_or(|e| e > data_end) {
            return Err(corrupt(format!(
                "la BAT ({} entradas en {:#x}) queda fuera del fichero",
                h.max_entries, h.table_offset
            )));
        }
        if overlaps(
            h.table_offset,
            bat_bytes,
            footer.data_offset,
            DYN_HEADER_LEN as u64,
        ) {
            return Err(corrupt("la BAT solapa la cabecera dinámica"));
        }
        if bat_bytes > crate::MAX_TABLE_BYTES {
            return Err(unsupported(format!(
                "BAT de {bat_bytes} bytes (límite {} bytes)",
                crate::MAX_TABLE_BYTES
            )));
        }
        // Solo se leen las entradas que cubren el tamaño virtual (needed ≤ max_entries).
        let needed_bytes = usize::try_from(needed * 4)
            .map_err(|_| unsupported("BAT demasiado grande para esta plataforma"))?;
        let mut raw = vec![0u8; needed_bytes];
        read_meta(file, h.table_offset, &mut raw)?;
        let mut bat: Vec<u32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_be_bytes(c))
            .collect();

        // Mapa de bits: un bit por sector, redondeado a múltiplo de 512 bytes.
        let bitmap_bytes = (block_size / SECTOR).div_ceil(8).div_ceil(SECTOR) * SECTOR;

        // Correspondencia inequívoca de cada bloque asignado: mapa de bits + parte útil dentro de la
        // zona de datos, sin solapar la copia del pie, la cabecera dinámica, la BAT ni otro bloque.
        let span = |i: u64| -> (u64, u64) {
            let start = u64::from(bat[i as usize]) * SECTOR;
            // El último bloque puede cubrir más que el tamaño virtual: solo se exige la parte útil.
            let useful = block_size.min(size - i * block_size);
            (start, start.saturating_add(bitmap_bytes + useful))
        };
        let mut damage = Damage::default();
        let mut candidates = Vec::new();
        for (i, &entry) in bat.iter().enumerate() {
            if entry == UNALLOCATED {
                continue;
            }
            let i = i as u64;
            let (_, end) = span(i);
            if end > data_end {
                damage.add(i, || {
                    format!("el bloque {i} (sector {entry:#x} del fichero) queda fuera del fichero")
                });
            } else {
                candidates.push(i);
            }
        }
        let zones = [
            Zone::fixed(0, FOOTER_LEN as u64),
            Zone::fixed(footer.data_offset, DYN_HEADER_LEN as u64),
            Zone::fixed(h.table_offset, bat_bytes),
        ];
        for i in tolerant::overlaps(candidates, span, &zones).entries {
            let entry = bat[i as usize];
            damage.add(i, || {
                format!(
                    "el bloque {i} (sector {entry:#x} del fichero) solapa las estructuras del contenedor u otro bloque"
                )
            });
        }
        if !damage.is_empty() && !tolerant {
            return Err(corrupt(damage.first().unwrap_or_default()));
        }
        let bad = damage.into_sorted();
        for &i in &bad {
            bat[i as usize] = DISCARDED;
        }

        Ok(Dynamic {
            block_size,
            bitmap_bytes,
            bat,
            discarded: bad.len() as u64,
            cache: Mutex::new(None),
        })
    }

    /// Variante legible del contenedor.
    pub fn describe(&self) -> String {
        let mut s = match &self.layout {
            Layout::Fixed => "VHD fijo".to_string(),
            Layout::Dynamic(d) => {
                format!("VHD dinámico, bloques de {}", human_size(d.block_size))
            }
        };
        match self.footer_source {
            FooterSource::End512 => {}
            FooterSource::End511 => s.push_str(" (pie antiguo de 511 bytes)"),
            FooterSource::Copy => s.push_str(" (pie final dañado: se usa la copia del byte 0)"),
        }
        if self.tolerant {
            s.push_str(&tolerant::describe_suffix(
                self.discarded_blocks(),
                "bloque",
                "bloques",
            ));
        }
        s
    }

    /// `true` si se abrió en modo tolerante.
    pub fn is_tolerant(&self) -> bool {
        self.tolerant
    }

    /// Bloques descartados en modo tolerante (0 en modo estricto o en un VHD fijo).
    pub fn discarded_blocks(&self) -> u64 {
        match &self.layout {
            Layout::Fixed => 0,
            Layout::Dynamic(d) => d.discarded,
        }
    }

    /// Mapa de bits del bloque `block` (asignado, en `base` del fichero), con caché de una entrada.
    /// Un fallo de lectura se informa con el rango **virtual** `(voff, len)` que lo necesitaba.
    fn bitmap(
        &self,
        d: &Dynamic,
        block: u64,
        base: u64,
        voff: u64,
        len: usize,
    ) -> Result<Arc<Vec<u8>>> {
        {
            let guard = d.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((b, map)) = guard.as_ref() {
                if *b == block {
                    return Ok(Arc::clone(map));
                }
            }
        }
        // bitmap_bytes ≤ 64 KiB (bloques ≤ 256 MiB).
        let mut map = vec![0u8; d.bitmap_bytes as usize];
        mr_io::read_exact_at(&self.file, base, &mut map).map_err(|source| IoError::Read {
            offset: voff,
            len,
            source,
        })?;
        let map = Arc::new(map);
        *d.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((block, Arc::clone(&map)));
        Ok(map)
    }

    /// Lee del fichero; los errores se informan con el desplazamiento **virtual**.
    fn read_file(&self, file_offset: u64, buf: &mut [u8], virtual_offset: u64) -> Result<()> {
        mr_io::read_exact_at(&self.file, file_offset, buf).map_err(|source| IoError::Read {
            offset: virtual_offset,
            len: buf.len(),
            source,
        })
    }

    fn read_dynamic(&self, d: &Dynamic, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bs = d.block_size;
        let mut done = 0usize;
        while done < buf.len() {
            let voff = offset + done as u64;
            let block = voff / bs;
            let in_block = voff % bs;
            let n = (buf.len() - done).min((bs - in_block) as usize);
            let chunk = &mut buf[done..done + n];
            // block < bat.len() porque voff < size ≤ bat.len() × bs.
            let entry = d.bat.get(block as usize).copied().unwrap_or(UNALLOCATED);
            if entry == UNALLOCATED {
                chunk.fill(0);
            } else if entry == DISCARDED {
                return Err(tolerant::discarded_read(FORMAT, "bloque", block, voff, n));
            } else {
                let base = u64::from(entry) * SECTOR;
                let data = base + d.bitmap_bytes;
                let map = self.bitmap(d, block, base, voff, n)?;
                let last_sector = (in_block + n as u64 - 1) / SECTOR;
                let mut pos = 0usize;
                while pos < n {
                    let o = in_block + pos as u64;
                    let sec = o / SECTOR;
                    let set = bit(&map, sec);
                    let end_sec = run_end(&map, sec, last_sector + 1, set);
                    let piece_end = ((end_sec * SECTOR - in_block) as usize).min(n);
                    let piece = &mut chunk[pos..piece_end];
                    if set {
                        self.read_file(data + o, piece, voff + pos as u64)?;
                    } else {
                        // Bit a 0: sector no escrito, ceros por definición del formato.
                        piece.fill(0);
                    }
                    pos = piece_end;
                }
            }
            done += n;
        }
        Ok(())
    }
}

impl BlockDevice for VhdImage {
    fn size(&self) -> u64 {
        self.size
    }

    fn sector_size(&self) -> u32 {
        SECTOR as u32
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        match offset.checked_add(len as u64) {
            Some(e) if e <= self.size => {}
            _ => {
                return Err(IoError::OutOfBounds {
                    offset,
                    len,
                    size: self.size,
                })
            }
        }
        match &self.layout {
            Layout::Fixed => self.read_file(offset, buf, offset),
            Layout::Dynamic(d) => self.read_dynamic(d, offset, buf),
        }
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        let end = match lba.checked_add(count) {
            Some(e) if count > 0 && e <= total => e,
            _ => return Err(IoError::InvalidRange { lba, count, total }),
        };
        let d = match &self.layout {
            Layout::Fixed => return Ok(vec![(lba, count, Allocation::Data)]),
            Layout::Dynamic(d) => d,
        };
        let spb = d.block_size / SECTOR;
        let mut runs = Vec::new();
        let mut s = lba;
        while s < end {
            let block = s / spb;
            let block_first = block * spb;
            let stop = end.min(block_first + spb);
            let entry = d.bat.get(block as usize).copied().unwrap_or(UNALLOCATED);
            if entry == UNALLOCATED {
                push_run(&mut runs, s, stop - s, Allocation::Sparse);
            } else if entry == DISCARDED {
                // Hay que «leerlo» para descubrir que no es fiable: nunca se presenta como disperso.
                push_run(&mut runs, s, stop - s, Allocation::Data);
            } else {
                let len = usize::try_from((stop - s) * SECTOR).unwrap_or(usize::MAX);
                let Ok(map) = self.bitmap(d, block, u64::from(entry) * SECTOR, s * SECTOR, len)
                else {
                    // Mapa de bits ilegible: no se sabe qué sectores están escritos. Se informa
                    // `Data` (conservador) en vez de fallar toda la consulta; la lectura de esos
                    // sectores volverá a necesitar el mapa y fallará con su posición exacta.
                    push_run(&mut runs, s, stop - s, Allocation::Data);
                    s = stop;
                    continue;
                };
                let mut rel = s - block_first;
                let rel_stop = stop - block_first;
                while rel < rel_stop {
                    let set = bit(&map, rel);
                    let e = run_end(&map, rel, rel_stop, set);
                    let kind = if set {
                        Allocation::Data
                    } else {
                        Allocation::Sparse
                    };
                    push_run(&mut runs, block_first + rel, e - rel, kind);
                    rel = e;
                }
            }
            s = stop;
        }
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const BS: u32 = 4096; // 8 sectores por bloque en las imágenes sintéticas

    fn seal(b: &mut [u8], field: usize) {
        let c = vhd_checksum(b, field);
        b[field..field + 4].copy_from_slice(&c.to_be_bytes());
    }

    fn footer(disk_type: u32, current_size: u64, data_offset: u64) -> [u8; 512] {
        let mut f = [0u8; 512];
        f[..8].copy_from_slice(FOOTER_COOKIE);
        f[8..12].copy_from_slice(&2u32.to_be_bytes());
        f[F_VERSION..F_VERSION + 4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        f[F_DATA_OFFSET..F_DATA_OFFSET + 8].copy_from_slice(&data_offset.to_be_bytes());
        f[28..32].copy_from_slice(b"mrsc");
        f[40..48].copy_from_slice(&current_size.to_be_bytes());
        f[F_CURRENT_SIZE..F_CURRENT_SIZE + 8].copy_from_slice(&current_size.to_be_bytes());
        f[F_DISK_TYPE..F_DISK_TYPE + 4].copy_from_slice(&disk_type.to_be_bytes());
        f[68..84].copy_from_slice(&[0x5A; 16]);
        seal(&mut f, F_CHECKSUM);
        f
    }

    fn dyn_header(table_offset: u64, max_entries: u32, block_size: u32, parent: &str) -> Vec<u8> {
        let mut h = vec![0u8; DYN_HEADER_LEN];
        h[..8].copy_from_slice(DYN_COOKIE);
        h[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
        h[H_TABLE_OFFSET..H_TABLE_OFFSET + 8].copy_from_slice(&table_offset.to_be_bytes());
        h[H_VERSION..H_VERSION + 4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        h[H_MAX_ENTRIES..H_MAX_ENTRIES + 4].copy_from_slice(&max_entries.to_be_bytes());
        h[H_BLOCK_SIZE..H_BLOCK_SIZE + 4].copy_from_slice(&block_size.to_be_bytes());
        h[H_PARENT_ID..H_PARENT_ID + 16].copy_from_slice(&[0x11; 16]);
        for (i, u) in parent.encode_utf16().take(255).enumerate() {
            let o = H_PARENT_NAME + 2 * i;
            h[o..o + 2].copy_from_slice(&u.to_be_bytes());
        }
        seal(&mut h, H_CHECKSUM);
        h
    }

    /// Bloque asignado: (índice, primer byte del mapa de bits, 4096 bytes de datos).
    type Block = (u32, u8, Vec<u8>);

    /// Construye un VHD dinámico (o diferencial) según la especificación:
    /// copia del pie | cabecera dinámica (512) | BAT (1536) | bloques | pie.
    fn dynamic_image(disk_type: u32, size: u64, max_entries: u32, blocks: &[Block]) -> Vec<u8> {
        let mut img = Vec::new();
        let ft = footer(disk_type, size, 512);
        img.extend_from_slice(&ft);
        img.extend_from_slice(&dyn_header(1536, max_entries, BS, "C:\\vm\\padre.vhd"));
        let bat_len = (max_entries as usize * 4).div_ceil(512) * 512;
        let mut bat = vec![0xFFu8; bat_len];
        let mut body = Vec::new();
        let first = 1536 + bat_len;
        for (idx, map, data) in blocks {
            let sector = ((first + body.len()) / 512) as u32;
            let i = *idx as usize * 4;
            bat[i..i + 4].copy_from_slice(&sector.to_be_bytes());
            let mut bm = vec![0u8; 512];
            bm[0] = *map;
            body.extend_from_slice(&bm);
            body.extend_from_slice(data);
        }
        img.extend_from_slice(&bat);
        img.extend_from_slice(&body);
        img.extend_from_slice(&ft);
        img
    }

    fn pattern(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) | 1)
            .collect()
    }

    fn open_bytes(bytes: &[u8]) -> (tempfile::NamedTempFile, Result<VhdImage>) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        let f = File::open(tmp.path()).unwrap();
        let r = VhdImage::open(f);
        (tmp, r)
    }

    fn assert_corrupt(r: Result<VhdImage>, what: &str) {
        match r {
            Err(IoError::Corrupt { format: "VHD", .. }) => {}
            Err(e) => panic!("{what}: se esperaba Corrupt, no {e:?}"),
            Ok(_) => panic!("{what}: se esperaba Corrupt y se abrió"),
        }
    }

    fn read_all(img: &VhdImage) -> Vec<u8> {
        let mut v = vec![0u8; img.size() as usize];
        img.read_at(0, &mut v).unwrap();
        v
    }

    /// Imagen dinámica de referencia: 4 bloques + 2 sectores (5 entradas). Bloque 0 completo,
    /// 1 sin asignar, 2 con mapa parcial (sectores 0, 2 y 7), 3 completo, 4 (parcial al final) completo.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let size = 4 * BS as u64 + 1024;
        let b0 = pattern(1, 4096);
        let b2 = pattern(2, 4096);
        let b3 = pattern(3, 4096);
        let b4 = pattern(4, 4096);
        let img = dynamic_image(
            DISK_DYNAMIC,
            size,
            5,
            &[
                (0, 0xFF, b0.clone()),
                (2, 0b1010_0001, b2.clone()),
                (3, 0xFF, b3.clone()),
                (4, 0xFF, b4.clone()),
            ],
        );
        let mut expect = vec![0u8; size as usize];
        expect[..4096].copy_from_slice(&b0);
        for s in [0usize, 2, 7] {
            expect[2 * 4096 + s * 512..2 * 4096 + (s + 1) * 512]
                .copy_from_slice(&b2[s * 512..(s + 1) * 512]);
        }
        expect[3 * 4096..4 * 4096].copy_from_slice(&b3);
        expect[4 * 4096..].copy_from_slice(&b4[..1024]);
        (img, expect)
    }

    #[test]
    fn checksum_matches_spec_definition() {
        let f = footer(DISK_FIXED, 4096, u64::MAX);
        let sum: u32 = f
            .iter()
            .enumerate()
            .filter(|(i, _)| !(64..68).contains(i))
            .map(|(_, &b)| u32::from(b))
            .sum();
        assert_eq!(be32(&f, 64), !sum);
        assert!(parse_footer(&f).is_ok());
    }

    #[test]
    fn fixed_reads_and_reports() {
        let data = pattern(9, 8 * 512);
        let mut file = data.clone();
        file.extend_from_slice(&footer(DISK_FIXED, data.len() as u64, u64::MAX));
        let (_t, img) = open_bytes(&file);
        let img = img.unwrap();
        assert_eq!(img.size(), 4096);
        assert_eq!(img.sector_size(), 512);
        assert_eq!(img.describe(), "VHD fijo");
        assert_eq!(read_all(&img), data);
        let mut b = vec![0u8; 700];
        img.read_at(1000, &mut b).unwrap();
        assert_eq!(&b[..], &data[1000..1700]);
        assert!(matches!(
            img.read_at(4000, &mut [0u8; 97]),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            img.read_at(u64::MAX, &mut [0u8; 2]),
            Err(IoError::OutOfBounds { .. })
        ));
        assert_eq!(
            img.allocation(2, 6).unwrap(),
            vec![(2, 6, Allocation::Data)]
        );
        for (lba, count) in [(7, 2), (0, 0), (u64::MAX, 2)] {
            assert!(matches!(
                img.allocation(lba, count),
                Err(IoError::InvalidRange { .. })
            ));
        }
    }

    #[test]
    fn fixed_with_old_511_byte_footer() {
        let data = pattern(5, 4 * 512);
        let mut file = data.clone();
        file.extend_from_slice(&footer(DISK_FIXED, data.len() as u64, u64::MAX)[..511]);
        let (_t, img) = open_bytes(&file);
        let img = img.unwrap();
        assert_eq!(img.size(), 2048);
        assert!(img.describe().contains("511"));
        assert_eq!(read_all(&img), data);
    }

    #[test]
    fn bad_checksum_and_bad_cookie_are_corrupt() {
        let data = vec![0u8; 1024];
        let mut f = footer(DISK_FIXED, 1024, u64::MAX);
        f[100] ^= 0x40; // daña un byte reservado sin recalcular la suma
        let mut file = data.clone();
        file.extend_from_slice(&f);
        assert_corrupt(open_bytes(&file).1, "suma de control");

        let mut f = footer(DISK_FIXED, 1024, u64::MAX);
        f[..8].copy_from_slice(b"conectiX");
        seal(&mut f, F_CHECKSUM);
        let mut file = data;
        file.extend_from_slice(&f);
        assert_corrupt(open_bytes(&file).1, "cookie");

        // Fichero mínimo sin estructura.
        assert_corrupt(open_bytes(&[0u8; 100]).1, "fichero diminuto");
        assert_corrupt(open_bytes(&[0u8; 4096]).1, "ceros");
    }

    #[test]
    fn unknown_or_reserved_disk_types_are_rejected() {
        for (t, unsupported_expected) in [(0u32, true), (1, true), (5, true), (99, false)] {
            let mut file = vec![0u8; 1024];
            file.extend_from_slice(&footer(t, 1024, u64::MAX));
            match open_bytes(&file).1 {
                Err(IoError::UnsupportedFeature { .. }) if unsupported_expected => {}
                Err(IoError::Corrupt { .. }) if !unsupported_expected => {}
                Err(e) => panic!("tipo {t}: error inesperado {e:?}"),
                Ok(_) => panic!("tipo {t}: no debería abrirse"),
            }
        }
    }

    #[test]
    fn inconsistent_current_size_is_corrupt() {
        // Fijo: el fichero no mide current_size + 512.
        let mut file = vec![0u8; 2048];
        file.extend_from_slice(&footer(DISK_FIXED, 4096, u64::MAX));
        assert_corrupt(open_bytes(&file).1, "fijo más corto");
        let mut file = vec![0u8; 2048];
        file.extend_from_slice(&footer(DISK_FIXED, 1024, u64::MAX));
        assert_corrupt(open_bytes(&file).1, "fijo más largo");
        // Tamaño no múltiplo de sector.
        let mut file = vec![0u8; 1000];
        file.extend_from_slice(&footer(DISK_FIXED, 1000, u64::MAX));
        assert_corrupt(open_bytes(&file).1, "tamaño no múltiplo de 512");
        // Dinámico: max_table_entries × block_size < current_size.
        let img = dynamic_image(DISK_DYNAMIC, 3 * BS as u64, 2, &[]);
        assert_corrupt(open_bytes(&img).1, "BAT insuficiente");
    }

    #[test]
    fn dynamic_reads_across_allocated_unallocated_and_partial_blocks() {
        let (bytes, expect) = sample();
        let (_t, img) = open_bytes(&bytes);
        let img = img.unwrap();
        assert_eq!(img.size(), expect.len() as u64);
        assert_eq!(img.sector_count(), 34);
        assert_eq!(img.describe(), "VHD dinámico, bloques de 4 KiB");
        assert_eq!(read_all(&img), expect);
        // Inicios y longitudes irregulares que cruzan sectores y bloques.
        for start in (0..expect.len()).step_by(333) {
            for len in [1usize, 511, 512, 513, 4096, 5000, 9001] {
                let len = len.min(expect.len() - start);
                let mut b = vec![0xEEu8; len];
                img.read_at(start as u64, &mut b).unwrap();
                assert_eq!(&b[..], &expect[start..start + len], "{start}+{len}");
            }
        }
        assert!(matches!(
            img.read_at(expect.len() as u64 - 10, &mut [0u8; 11]),
            Err(IoError::OutOfBounds { .. })
        ));
        use Allocation::*;
        assert_eq!(
            img.allocation(0, 34).unwrap(),
            vec![
                (0, 8, Data),
                (8, 8, Sparse),
                (16, 1, Data),
                (17, 1, Sparse),
                (18, 1, Data),
                (19, 4, Sparse),
                (23, 11, Data),
            ]
        );
        assert_eq!(
            img.allocation(6, 12).unwrap(),
            vec![(6, 2, Data), (8, 8, Sparse), (16, 1, Data), (17, 1, Sparse)]
        );
        assert!(matches!(
            img.allocation(30, 5),
            Err(IoError::InvalidRange { .. })
        ));
        // Lo que dice allocation() coincide con los datos: Sparse ⇒ ceros.
        for (lba, n, kind) in img.allocation(0, 34).unwrap() {
            if kind == Sparse {
                let r = &expect[(lba * 512) as usize..((lba + n) * 512) as usize];
                assert!(r.iter().all(|&b| b == 0));
            }
        }
    }

    #[test]
    fn damaged_end_footer_falls_back_to_copy() {
        let (mut bytes, expect) = sample();
        let n = bytes.len();
        bytes[n - 512 + 70] ^= 0xFF; // suma de control del pie final rota
        let (_t, img) = open_bytes(&bytes);
        let img = img.unwrap();
        assert!(img.describe().contains("copia"));
        assert_eq!(read_all(&img), expect);
        // Si la copia también está dañada, no hay pie válido.
        bytes[70] ^= 0xFF;
        assert_corrupt(open_bytes(&bytes).1, "ambos pies dañados");
        // Un VHD fijo no tiene copia: un pie de tipo fijo en el byte 0 no se acepta.
        let mut fixed = footer(DISK_FIXED, 1024, u64::MAX).to_vec();
        fixed.extend_from_slice(&[0u8; 1024]);
        assert_corrupt(open_bytes(&fixed).1, "copia de un fijo");
    }

    #[test]
    fn dynamic_header_must_be_valid() {
        let (bytes, _) = sample();
        let mut b = bytes.clone();
        b[512 + 200] ^= 1; // suma de control de la cabecera dinámica
        assert_corrupt(open_bytes(&b).1, "suma de la cabecera dinámica");
        let mut b = bytes.clone();
        b[512..520].copy_from_slice(b"cxsparsX");
        assert_corrupt(open_bytes(&b).1, "cookie de la cabecera dinámica");
        // data_offset fuera del fichero.
        let mut b = bytes;
        let n = b.len();
        let ft = footer(DISK_DYNAMIC, 4 * BS as u64 + 1024, 1 << 40);
        b[..512].copy_from_slice(&ft);
        b[n - 512..].copy_from_slice(&ft);
        assert_corrupt(open_bytes(&b).1, "cabecera fuera del fichero");
    }

    /// Reescribe la cabecera dinámica de `img` con otros parámetros.
    fn with_header(mut img: Vec<u8>, table: u64, entries: u32, bs: u32) -> Vec<u8> {
        img[512..1536].copy_from_slice(&dyn_header(table, entries, bs, ""));
        img
    }

    #[test]
    fn bat_outside_file_is_corrupt() {
        let (bytes, _) = sample();
        let n = bytes.len() as u64;
        assert_corrupt(
            open_bytes(&with_header(bytes.clone(), n + 4096, 5, BS)).1,
            "BAT después del fin",
        );
        assert_corrupt(
            open_bytes(&with_header(bytes.clone(), u64::MAX - 4, 5, BS)).1,
            "BAT con desbordamiento",
        );
        assert_corrupt(
            open_bytes(&with_header(bytes.clone(), 0, 5, BS)).1,
            "BAT sobre la copia del pie",
        );
        assert_corrupt(
            open_bytes(&with_header(bytes, 1024, 5, BS)).1,
            "BAT sobre la cabecera",
        );
    }

    #[test]
    fn absurd_block_sizes_are_corrupt() {
        let (bytes, _) = sample();
        for bs in [0u32, 1, 256, 3000, 4097, 512 * 1024 * 1024, 0x8000_0000] {
            assert_corrupt(
                open_bytes(&with_header(bytes.clone(), 1536, 5, bs)).1,
                &format!("bloque de {bs}"),
            );
        }
    }

    #[test]
    fn huge_max_table_entries_fail_without_allocating() {
        let (bytes, _) = sample();
        // 0xFFFFFFFF entradas = 16 GiB de BAT: se rechaza por tamaño del fichero antes de reservar.
        for entries in [0xFFFF_FFFFu32, 0x1000_0000, 1 << 20] {
            let r = open_bytes(&with_header(bytes.clone(), 1536, entries, BS)).1;
            assert!(
                matches!(
                    r,
                    Err(IoError::Corrupt { .. } | IoError::UnsupportedFeature { .. })
                ),
                "{entries} entradas"
            );
        }
    }

    #[test]
    fn block_outside_file_is_corrupt() {
        let (mut bytes, _) = sample();
        // Entrada 3 de la BAT apuntando más allá del fin del fichero.
        bytes[1536 + 12..1536 + 16].copy_from_slice(&0x00FF_0000u32.to_be_bytes());
        assert_corrupt(open_bytes(&bytes).1, "bloque fuera");
        // Entrada apuntando a la cabecera dinámica.
        let (mut bytes, _) = sample();
        bytes[1536..1540].copy_from_slice(&1u32.to_be_bytes());
        assert_corrupt(open_bytes(&bytes).1, "bloque sobre la cabecera");
    }

    #[test]
    fn truncated_files_are_detected() {
        let (bytes, expect) = sample();
        // Truncado: el pie final desaparece, la copia sigue siendo válida, pero el último bloque
        // ya no cabe en el fichero.
        let cut = &bytes[..bytes.len() - 512 - 5000];
        assert_corrupt(open_bytes(cut).1, "dinámico truncado");
        // Fijo truncado: el pie ya no está al final.
        let mut fixed = vec![0u8; 4096];
        fixed.extend_from_slice(&footer(DISK_FIXED, 4096, u64::MAX));
        assert_corrupt(open_bytes(&fixed[..3000]).1, "fijo truncado");
        // Acortado por otro proceso después de abrirlo: error de lectura, nunca datos inventados.
        let (t, img) = open_bytes(&bytes);
        let img = img.unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .unwrap()
            .set_len(4000)
            .unwrap();
        let mut b = vec![0u8; 512];
        match img.read_at(3 * 4096, &mut b) {
            Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (3 * 4096, 512)),
            other => panic!("se esperaba error de lectura, no {other:?}"),
        }
        // Las zonas no asignadas siguen leyéndose como ceros.
        img.read_at(4096, &mut b).unwrap();
        assert_eq!(&b[..], &expect[4096..4608]);
    }

    #[test]
    fn differencing_disk_reports_parent_without_opening_it() {
        let img = dynamic_image(DISK_DIFFERENCING, 2 * BS as u64, 2, &[]);
        match open_bytes(&img).1 {
            Err(IoError::ExternalReference { format, reference }) => {
                assert_eq!(format, "VHD");
                assert_eq!(reference, "C:\\vm\\padre.vhd");
            }
            Err(e) => panic!("se esperaba ExternalReference, no {e:?}"),
            Ok(_) => panic!("un diferencial no debe abrirse"),
        }
        // Sin nombre de padre: se identifica por su id.
        let mut img = img;
        img[512..1536].copy_from_slice(&dyn_header(1536, 2, BS, ""));
        match open_bytes(&img).1 {
            Err(IoError::ExternalReference { reference, .. }) => {
                assert!(reference.contains("11111111-1111"), "{reference}")
            }
            other => panic!("inesperado: {:?}", other.err()),
        }
        // Cabecera dañada: sigue siendo una referencia externa, con el motivo.
        img[600] ^= 1;
        assert!(matches!(
            open_bytes(&img).1,
            Err(IoError::ExternalReference { .. })
        ));
    }

    /// Como `sample`, pero con el bloque 2 (mapa parcial) al final del fichero, para poder dejar
    /// ilegible solo su mapa de bits truncando el fichero.
    fn sample_block2_last() -> (Vec<u8>, Vec<u8>) {
        let (_, expect) = sample();
        let size = 4 * BS as u64 + 1024;
        let img = dynamic_image(
            DISK_DYNAMIC,
            size,
            5,
            &[
                (0, 0xFF, pattern(1, 4096)),
                (3, 0xFF, pattern(3, 4096)),
                (4, 0xFF, pattern(4, 4096)),
                (2, 0b1010_0001, pattern(2, 4096)),
            ],
        );
        (img, expect)
    }

    #[test]
    fn unreadable_bitmap_reports_data_and_verify_marks_it_unreadable() {
        use mr_io::resilient::{read_range, OnError, ReadPolicy};
        use mr_io::SectorState as St;
        let (bytes, expect) = sample_block2_last();
        let (tmp, img) = open_bytes(&bytes);
        let img = img.unwrap();
        assert_eq!(read_all(&img), expect);
        // Tras abrir, el soporte deja de devolver el mapa de bits del bloque 2 (y lo que le
        // sigue): se trunca el fichero en mitad de ese mapa.
        let cut = u64::from(bat_entry(&bytes, 2)) * 512 + 100;
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .unwrap()
            .set_len(cut)
            .unwrap();
        // La consulta no falla: el bloque 2 pasa a `Data` (nunca `Sparse`); el resto es exacto.
        assert_eq!(
            img.allocation(0, 34).unwrap(),
            vec![
                (0, 8, Allocation::Data),
                (8, 8, Allocation::Sparse),
                (16, 18, Allocation::Data)
            ]
        );
        assert_eq!(
            img.allocation(17, 2).unwrap(),
            vec![(17, 2, Allocation::Data)]
        );
        // Leer el bloque sigue fallando con la posición virtual exacta.
        assert!(matches!(
            img.read_at(17 * 512, &mut [0u8; 512]),
            Err(IoError::Read { offset, .. }) if offset == 17 * 512
        ));

        // Verificación con relleno: termina y el bloque 2 queda `Unreadable`.
        let p = ReadPolicy {
            chunk_sectors: 8,
            on_error: OnError::FillAndContinue,
            ..ReadPolicy::default()
        };
        let mut data = Vec::new();
        let mut states: Vec<St> = Vec::new();
        let r = read_range(
            &img,
            0,
            34,
            &p,
            |_, d, s| {
                data.extend_from_slice(d);
                states.extend_from_slice(s);
                Ok(())
            },
            |_, _| true,
        )
        .unwrap();
        assert_eq!(
            r.map.runs(),
            &[
                (0, 8, St::Read),
                (8, 8, St::Sparse),
                (16, 8, St::Unreadable),
                (24, 10, St::Read)
            ]
        );
        for (i, st) in states.iter().enumerate() {
            let sec = &data[i * 512..(i + 1) * 512];
            match st {
                St::Unreadable => assert!(sec.chunks(4).all(|c| c == p.bad_fill)),
                _ => assert_eq!(sec, &expect[i * 512..(i + 1) * 512], "sector {i}"),
            }
        }
        // Estricto (hashes): el error sigue abortando con la posición del primer sector afectado.
        let r = read_range(
            &img,
            0,
            34,
            &ReadPolicy::default(),
            |_, _, _| Ok(()),
            |_, _| true,
        );
        assert!(matches!(r, Err(IoError::Read { offset, .. }) if offset == 16 * 512));
    }

    // ---- modo tolerante (ADR-020) ----

    fn open_mode(bytes: &[u8], tolerant: bool) -> (tempfile::NamedTempFile, Result<VhdImage>) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        let r = VhdImage::open_with(File::open(tmp.path()).unwrap(), tolerant);
        (tmp, r)
    }

    /// Entrada `i` de la BAT de las imágenes de `dynamic_image` (BAT en el byte 1536).
    fn bat_entry(img: &[u8], i: usize) -> u32 {
        be32(img, 1536 + 4 * i)
    }

    fn set_bat_entry(img: &mut [u8], i: usize, v: u32) {
        img[1536 + 4 * i..1540 + 4 * i].copy_from_slice(&v.to_be_bytes());
    }

    #[test]
    fn tolerant_mode_discards_only_invalid_or_ambiguous_blocks() {
        let (good, expect) = sample();
        // La referencia es la lectura estricta de la imagen sin dañar.
        let (_t, reference) = open_mode(&good, false);
        assert_eq!(read_all(&reference.unwrap()), expect);
        let n = good.len() as u64;
        let s2 = bat_entry(&good, 2);
        // (caso, entradas alteradas, tramos de sectores dañados). 8 sectores por bloque.
        type Case = (&'static str, Vec<(usize, u32)>, Vec<(u64, u64)>);
        let cases: Vec<Case> = vec![
            ("fuera del fichero", vec![(3, 0x00FF_0000)], vec![(24, 8)]),
            (
                "sobre el pie final",
                vec![(4, ((n - 1024) / 512) as u32)],
                vec![(32, 2)],
            ),
            ("sobre la copia del pie", vec![(4, 0)], vec![(32, 2)]),
            ("sobre la cabecera dinámica", vec![(0, 1)], vec![(0, 8)]),
            ("sobre la BAT", vec![(0, 3)], vec![(0, 8)]),
            ("duplicada", vec![(3, s2)], vec![(16, 8), (24, 8)]),
            (
                "solapada con otro bloque",
                vec![(3, s2 + 4)],
                vec![(16, 8), (24, 8)],
            ),
            (
                "varias a la vez",
                vec![(0, 1), (4, 0x00FF_0000)],
                vec![(0, 8), (32, 2)],
            ),
        ];
        for (what, edits, bad) in cases {
            let mut bytes = good.clone();
            for &(i, v) in &edits {
                set_bat_entry(&mut bytes, i, v);
            }
            assert_corrupt(open_mode(&bytes, false).1, what);
            let (t, img) = open_mode(&bytes, true);
            let before = crate::tolerant::check::Snapshot::of(t.path());
            let img = img.unwrap_or_else(|e| panic!("{what}: {e}"));
            let blocks = bad.len() as u64;
            assert_eq!(img.discarded_blocks(), blocks, "{what}");
            assert!(img.is_tolerant());
            let d = img.describe();
            assert!(
                d.ends_with(&crate::tolerant::describe_suffix(
                    blocks, "bloque", "bloques"
                )),
                "{what}: {d}"
            );
            crate::tolerant::check::tolerant_read_matches(&img, &expect, &bad, what);
            drop(img);
            assert_eq!(
                crate::tolerant::check::Snapshot::of(t.path()),
                before,
                "{what}"
            );
        }
    }

    #[test]
    fn tolerant_mode_without_damage_matches_strict() {
        let (good, expect) = sample();
        let (_t, img) = open_mode(&good, true);
        let img = img.unwrap();
        assert_eq!(img.discarded_blocks(), 0);
        assert_eq!(
            img.describe(),
            "VHD dinámico, bloques de 4 KiB; modo tolerante: 0 bloques descartados (no fiables)"
        );
        crate::tolerant::check::tolerant_read_matches(&img, &expect, &[], "sin daños");
        let (_t, strict) = open_mode(&good, false);
        let strict = strict.unwrap();
        assert!(!strict.is_tolerant());
        assert_eq!(strict.describe(), "VHD dinámico, bloques de 4 KiB");
        assert_eq!(
            strict.allocation(0, 34).unwrap(),
            img.allocation(0, 34).unwrap()
        );
        // Un VHD fijo no tiene tablas de asignación: nada que descartar.
        let data = pattern(9, 8 * 512);
        let mut file = data.clone();
        file.extend_from_slice(&footer(DISK_FIXED, data.len() as u64, u64::MAX));
        let (_t, fixed) = open_mode(&file, true);
        let fixed = fixed.unwrap();
        assert_eq!(fixed.discarded_blocks(), 0);
        assert!(fixed.describe().starts_with("VHD fijo; modo tolerante"));
        assert_eq!(read_all(&fixed), data);
    }

    #[test]
    fn critical_structures_are_corrupt_in_both_modes() {
        let (good, _) = sample();
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut b = good.clone();
        b[512 + 200] ^= 1;
        cases.push(("suma de la cabecera dinámica", b));
        let mut b = good.clone();
        let n = b.len();
        b[70] ^= 0xFF;
        b[n - 512 + 70] ^= 0xFF;
        cases.push(("ambos pies", b));
        let n = good.len() as u64;
        cases.push((
            "BAT fuera del fichero",
            with_header(good.clone(), n + 4096, 5, BS),
        ));
        cases.push((
            "BAT sobre la cabecera",
            with_header(good.clone(), 1024, 5, BS),
        ));
        cases.push(("tamaño de bloque", with_header(good.clone(), 1536, 5, 3000)));
        cases.push(("BAT insuficiente", with_header(good, 1536, 2, BS)));
        for (what, bytes) in cases {
            for tolerant in [false, true] {
                let (t, r) = open_mode(&bytes, tolerant);
                assert_corrupt(r, &format!("{what} (tolerante: {tolerant})"));
                assert_eq!(std::fs::read(t.path()).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn random_corruption_never_panics() {
        let (bytes, _) = sample();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..300 {
            let mut b = bytes.clone();
            for _ in 0..1 + next() % 4 {
                let pos = (next() % b.len() as u64) as usize;
                b[pos] = next() as u8;
            }
            // A veces se recalculan las sumas para llegar más lejos en la validación.
            if next() % 2 == 0 {
                seal(&mut b[512..1536], H_CHECKSUM);
                let n = b.len();
                seal(&mut b[n - 512..], F_CHECKSUM);
            }
            for tolerant in [false, true] {
                if let (_t, Ok(img)) = open_mode(&b, tolerant) {
                    let _ = img.allocation(0, img.sector_count().max(1));
                    if img.size() <= 1 << 20 {
                        let mut v = vec![0u8; img.size() as usize];
                        let _ = img.read_at(0, &mut v);
                    }
                }
            }
        }
    }
}
