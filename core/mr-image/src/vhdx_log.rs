//! Registro (log) de VHDX: validación y reproducción **en memoria** de las entradas pendientes,
//! según la especificación pública [MS-VHDX] «Virtual Hard Disk v2 (VHDX) File Format»
//! (apartado «Log»). El fichero nunca se escribe: el resultado es una superposición
//! (desplazamiento del fichero → sector de 4 KiB reproducido o tramo a ceros) que se aplica sobre
//! las lecturas del fichero.
//!
//! Estructuras (el registro es un búfer circular de sectores de 4 KiB):
//! - Cabecera de entrada (64 bytes): firma `loge`, CRC-32C de toda la entrada (con el campo de
//!   suma a cero), EntryLength (múltiplo de 4 KiB), Tail (desplazamiento en el registro de la
//!   entrada más antigua que hay que reproducir), SequenceNumber, DescriptorCount, Reserved,
//!   LogGuid, FlushedFileOffset y LastFileOffset. Le siguen los descriptores de 32 bytes;
//!   cabecera y descriptores ocupan ⌈(64 + 32·n) / 4096⌉ sectores.
//! - Descriptor de datos `desc`: TrailingBytes (4), LeadingBytes (8), FileOffset y
//!   SequenceNumber. Descriptor de ceros `zero`: Reserved (4), ZeroLength, FileOffset y
//!   SequenceNumber.
//! - Sector de datos `data` (uno por descriptor de datos, en el mismo orden, tras los sectores de
//!   descriptores): SequenceHigh, 4084 bytes y SequenceLow. El sector reproducido es
//!   LeadingBytes + los 4084 bytes + TrailingBytes.
//!
//! Una entrada es **válida** si su firma, longitud, Tail, LogGuid (el de la cabecera activa),
//! descriptores (firma y número de secuencia), sectores de datos (firma, SequenceHigh/Low) y
//! CRC-32C son correctos y sus sectores son exactamente los de descriptores más los de datos.
//! Primero se comprueba la estructura y solo después el CRC: dos entradas estructuralmente
//! válidas no pueden solaparse (cada sector interior empieza por `desc`, `zero` o `data`, nunca
//! por `loge`), así que el coste total del CRC está acotado por unas pocas veces el tamaño del
//! registro aunque el fichero sea hostil.
//!
//! Secuencia activa (algoritmo de la especificación): se recorre el registro desde su principio;
//! en cada posición con una entrada válida se forma la secuencia más larga de entradas válidas
//! contiguas (circularmente, sin pasar de la longitud del registro) con números de secuencia
//! consecutivos. La secuencia está completa si el Tail de su última entrada (la cabeza) señala una
//! entrada de la propia secuencia; la activa es la completa cuya cabeza tiene el mayor número de
//! secuencia. Si no hay ninguna, el registro pendiente no se puede reproducir: `Corrupt`. Las
//! entradas posteriores incompletas (escritura interrumpida) quedan fuera de la secuencia activa
//! y se ignoran, como manda la especificación.
//!
//! Reproducción: las entradas desde la señalada por el Tail de la cabeza hasta la cabeza, en
//! orden; una escritura posterior prevalece sobre una anterior. Además:
//! - FlushedFileOffset de la cabeza mayor que el fichero → fichero truncado: `Corrupt`.
//! - LastFileOffset de la cabeza mayor que el fichero → el fichero se considera ampliado con
//!   ceros hasta ese tamaño (tamaño efectivo).
//! - FlushedFileOffset y LastFileOffset múltiplos de 1 MiB; FileOffset y ZeroLength múltiplos de
//!   4 KiB; ninguna escritura en el identificador ni en las cabeceras (primeros 192 KiB), en el
//!   propio registro ni más allá del tamaño efectivo. Si no: `Corrupt`.
//!
//! Memoria: el registro se lee entero (≤ [`MAX_REGISTRO`]; uno mayor es `UnsupportedFeature`) y
//! se conserva para los sectores reproducidos; las listas de escrituras se reservan en el
//! [`Budget`] de la apertura, ampliado con el presupuesto de 2 × la longitud del registro.

use crate::budget::Budget;
use crate::crc32c::Crc32c;
use mr_io::{IoError, Result};
use std::collections::BinaryHeap;
use std::fs::File;

const FORMATO: &str = "VHDX";

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
/// Sector del registro.
const SECTOR: u64 = 4096;
const SECTOR_US: usize = 4096;
/// Longitud máxima de un registro que se reproduce (Hyper-V y qemu usan 1 MiB).
pub(crate) const MAX_REGISTRO: u64 = 64 * MIB;
/// Zona que el registro no puede escribir: identificador y las dos copias de la cabecera (la
/// cabecera activa ya se ha elegido para localizar el registro).
const ZONA_PROTEGIDA: u64 = 192 * KIB;

const FIRMA_ENTRADA: &[u8; 4] = b"loge";
const FIRMA_DESC_DATOS: &[u8; 4] = b"desc";
const FIRMA_DESC_CEROS: &[u8; 4] = b"zero";
const FIRMA_SECTOR_DATOS: &[u8; 4] = b"data";
const TAM_CABECERA_ENTRADA: u64 = 64;
const TAM_DESCRIPTOR: u64 = 32;

fn corrupto(detalle: impl Into<String>) -> IoError {
    IoError::Corrupt {
        format: FORMATO,
        detail: detalle.into(),
    }
}

fn no_soportado(detalle: impl Into<String>) -> IoError {
    IoError::UnsupportedFeature {
        format: FORMATO,
        detail: detalle.into(),
    }
}

fn u32_en(b: &[u8], o: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[o..o + 4]);
    u32::from_le_bytes(a)
}

fn u64_en(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Sector del registro que empieza en `pos` (múltiplo de 4 KiB, menor que la longitud).
fn sector(log: &[u8], pos: u64) -> &[u8] {
    let p = pos as usize;
    &log[p..p + SECTOR_US]
}

/// Entrada válida del registro.
#[derive(Debug, Clone, Copy)]
struct Entrada {
    pos: u64,
    longitud: u64,
    cola: u64,
    secuencia: u64,
    descriptores: u32,
    sectores_desc: u64,
    volcado: u64,
    ultimo: u64,
}

impl Entrada {
    /// Posición en el registro del descriptor `i`. Los descriptores están alineados a 32 bytes
    /// y nunca cruzan un sector.
    fn descriptor(&self, l: u64, i: u32) -> u64 {
        let off = TAM_CABECERA_ENTRADA + TAM_DESCRIPTOR * u64::from(i);
        (self.pos + off / SECTOR * SECTOR) % l + off % SECTOR
    }

    /// Posición en el registro del sector `k` de la entrada.
    fn sector(&self, l: u64, k: u64) -> u64 {
        (self.pos + k * SECTOR) % l
    }
}

/// Lee y valida la entrada que empieza en `pos`; `None` si no es válida.
fn leer_entrada(log: &[u8], pos: u64, guid: &[u8; 16]) -> Option<Entrada> {
    let l = log.len() as u64;
    let s0 = sector(log, pos);
    if &s0[..4] != FIRMA_ENTRADA || s0[32..48] != guid[..] {
        return None;
    }
    let suma = u32_en(s0, 4);
    let longitud = u64::from(u32_en(s0, 8));
    let cola = u64::from(u32_en(s0, 12));
    let secuencia = u64_en(s0, 16);
    let descriptores = u32_en(s0, 24);
    if longitud == 0
        || !longitud.is_multiple_of(SECTOR)
        || longitud > l
        || !cola.is_multiple_of(SECTOR)
        || cola >= l
    {
        return None;
    }
    let n = longitud / SECTOR;
    let sectores_desc =
        (TAM_CABECERA_ENTRADA + TAM_DESCRIPTOR * u64::from(descriptores)).div_ceil(SECTOR);
    if sectores_desc > n {
        return None;
    }
    let e = Entrada {
        pos,
        longitud,
        cola,
        secuencia,
        descriptores,
        sectores_desc,
        volcado: u64_en(s0, 48),
        ultimo: u64_en(s0, 56),
    };
    // Estructura: descriptores y sectores de datos (comprobación barata, antes del CRC).
    let mut datos = 0u64;
    for i in 0..descriptores {
        let d = e.descriptor(l, i) as usize;
        let d = &log[d..d + TAM_DESCRIPTOR as usize];
        if u64_en(d, 24) != secuencia {
            return None;
        }
        if &d[..4] == FIRMA_DESC_DATOS {
            let k = sectores_desc + datos;
            if k >= n {
                return None;
            }
            let s = sector(log, e.sector(l, k));
            if &s[..4] != FIRMA_SECTOR_DATOS
                || u32_en(s, 4) != (secuencia >> 32) as u32
                || u32_en(s, 4092) != secuencia as u32
            {
                return None;
            }
            datos += 1;
        } else if &d[..4] != FIRMA_DESC_CEROS {
            return None;
        }
    }
    if sectores_desc + datos != n {
        return None;
    }
    // CRC-32C de toda la entrada con el campo de suma a cero.
    let mut c = Crc32c::new();
    let mut primero = [0u8; SECTOR_US];
    primero.copy_from_slice(s0);
    primero[4..8].fill(0);
    c.update(&primero);
    for k in 1..n {
        c.update(sector(log, e.sector(l, k)));
    }
    (c.finish() == suma).then_some(e)
}

/// Sector de datos reproducido: posiciones en el registro de su descriptor y su sector de datos.
#[derive(Debug, Clone, Copy)]
struct SectorReproducido {
    off: u64,
    orden: u32,
    desc: u32,
    datos: u32,
}

/// Tramo a ceros `[inicio, fin)` del fichero y orden de su escritura.
#[derive(Debug, Clone, Copy)]
struct TramoCeros {
    inicio: u64,
    fin: u64,
    orden: u32,
}

/// Registro reproducido en memoria: superposición sobre el fichero.
pub(crate) struct Registro {
    /// Copia del registro (de ella salen los sectores reproducidos).
    log: Vec<u8>,
    /// Sectores de datos vigentes, ordenados por desplazamiento y sin repetidos.
    sectores: Vec<SectorReproducido>,
    /// Tramos a ceros vigentes, ordenados y fusionados (los sectores de datos, más recientes,
    /// prevalecen sobre ellos).
    ceros: Vec<(u64, u64)>,
    tam_real: u64,
    tam_efectivo: u64,
    entradas: u64,
}

impl std::fmt::Debug for Registro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registro")
            .field("entradas", &self.entradas)
            .field("sectores", &self.sectores.len())
            .field("ceros", &self.ceros.len())
            .field("tam_efectivo", &self.tam_efectivo)
            .finish()
    }
}

/// Valida el registro pendiente y lo reproduce en memoria. `off_registro` y `long_registro` ya
/// están validados (alineados a 1 MiB y dentro del fichero); `guid` es el LogGuid (no nulo) de la
/// cabecera activa.
pub(crate) fn reproducir(
    file: &File,
    tam_fichero: u64,
    off_registro: u64,
    long_registro: u64,
    guid: &[u8; 16],
    presupuesto: &mut Budget,
) -> Result<Registro> {
    let l = long_registro;
    if l == 0 {
        return Err(corrupto(
            "registro pendiente (LogGuid no nulo) sin región de registro (LogLength 0)",
        ));
    }
    if l > MAX_REGISTRO {
        return Err(no_soportado(format!(
            "registro pendiente de {l} bytes (se reproducen hasta {MAX_REGISTRO})"
        )));
    }
    presupuesto.add_tables(2 * l);
    let mut log: Vec<u8> = presupuesto.vec(l, "copia del registro")?;
    log.resize(l as usize, 0);
    mr_io::read_exact_at(file, off_registro, &mut log).map_err(|source| IoError::Read {
        offset: off_registro,
        len: l as usize,
        source,
    })?;

    // Secuencia activa.
    let mut posiciones: Vec<u64> = presupuesto.vec(l / SECTOR, "posiciones del registro")?;
    let mut activa: Option<(u64, u64, Entrada)> = None; // (primera, entradas, cabeza)
    let mut actual = 0u64;
    while actual < l {
        let Some(inicio) = leer_entrada(&log, actual, guid) else {
            actual += SECTOR;
            continue;
        };
        posiciones.clear();
        posiciones.push(actual);
        let mut cabeza = inicio;
        let mut total = inicio.longitud;
        while total < l {
            let siguiente = (cabeza.pos + cabeza.longitud) % l;
            match leer_entrada(&log, siguiente, guid) {
                Some(e)
                    if Some(e.secuencia) == cabeza.secuencia.checked_add(1)
                        && total + e.longitud <= l =>
                {
                    posiciones.push(siguiente);
                    total += e.longitud;
                    cabeza = e;
                }
                _ => break,
            }
        }
        if let Some(i) = posiciones.iter().position(|&p| p == cabeza.cola) {
            if activa
                .as_ref()
                .is_none_or(|a| cabeza.secuencia > a.2.secuencia)
            {
                activa = Some((posiciones[i], (posiciones.len() - i) as u64, cabeza));
            }
        }
        actual += total;
    }
    presupuesto.release(posiciones);
    let Some((primera, n_entradas, cabeza)) = activa else {
        return Err(corrupto(
            "registro pendiente sin ninguna secuencia de entradas válida y completa (firma, LogGuid, CRC-32C, descriptores, números de secuencia o Tail)",
        ));
    };

    // Tamaño del fichero según la cabeza.
    if cabeza.volcado > tam_fichero {
        return Err(corrupto(format!(
            "el fichero ({tam_fichero} bytes) es menor que el FlushedFileOffset del registro ({}): fichero truncado",
            cabeza.volcado
        )));
    }
    let tam_efectivo = tam_fichero.max(cabeza.ultimo);

    // Entradas que se reproducen, en orden; recuento de descriptores.
    let mut entradas_rep: Vec<Entrada> = presupuesto.vec(n_entradas, "entradas del registro")?;
    let mut pos = primera;
    let (mut n_datos, mut n_ceros) = (0u64, 0u64);
    for _ in 0..n_entradas {
        let e = leer_entrada(&log, pos, guid)
            .ok_or_else(|| corrupto("entrada del registro inestable al releerla"))?;
        if !e.volcado.is_multiple_of(MIB) || !e.ultimo.is_multiple_of(MIB) {
            return Err(corrupto(format!(
                "entrada {} del registro: FlushedFileOffset {} o LastFileOffset {} no múltiplos de 1 MiB",
                e.secuencia, e.volcado, e.ultimo
            )));
        }
        for i in 0..e.descriptores {
            let d = e.descriptor(l, i) as usize;
            if &log[d..d + 4] == FIRMA_DESC_DATOS {
                n_datos += 1;
            } else {
                n_ceros += 1;
            }
        }
        entradas_rep.push(e);
        pos = (pos + e.longitud) % l;
    }

    // Escrituras, en orden de reproducción.
    let fin_registro = off_registro + l;
    let comprobar = |e: &Entrada, que: &str, ini: u64, len: u64| -> Result<()> {
        let fin = ini.checked_add(len);
        if !ini.is_multiple_of(SECTOR)
            || !len.is_multiple_of(SECTOR)
            || ini < ZONA_PROTEGIDA
            || fin.is_none_or(|f| f > tam_efectivo || (ini < fin_registro && f > off_registro))
        {
            return Err(corrupto(format!(
                "entrada {} del registro: {que} en el desplazamiento {ini} con {len} bytes no válido (alineación a 4 KiB, cabeceras, el propio registro o más allá del tamaño efectivo {tam_efectivo})",
                e.secuencia
            )));
        }
        Ok(())
    };
    let mut sectores: Vec<SectorReproducido> = presupuesto.vec(n_datos, "sectores del registro")?;
    let mut ceros: Vec<TramoCeros> = presupuesto.vec(n_ceros, "tramos a ceros del registro")?;
    let mut orden = 0u32;
    for e in &entradas_rep {
        let mut k_datos = e.sectores_desc;
        for i in 0..e.descriptores {
            let pd = e.descriptor(l, i);
            let d = &log[pd as usize..(pd + TAM_DESCRIPTOR) as usize];
            let ini = u64_en(d, 16);
            if &d[..4] == FIRMA_DESC_DATOS {
                comprobar(e, "descriptor de datos", ini, SECTOR)?;
                sectores.push(SectorReproducido {
                    off: ini,
                    orden,
                    desc: pd as u32,
                    datos: e.sector(l, k_datos) as u32,
                });
                k_datos += 1;
            } else {
                let len = u64_en(d, 8);
                comprobar(e, "descriptor de ceros", ini, len)?;
                if len > 0 {
                    ceros.push(TramoCeros {
                        inicio: ini,
                        fin: ini + len,
                        orden,
                    });
                }
            }
            orden += 1;
        }
    }
    presupuesto.release(entradas_rep);

    // Sectores vigentes: el más reciente de cada desplazamiento, salvo que lo cubra un tramo a
    // ceros posterior.
    sectores.sort_unstable_by_key(|s| (s.off, u32::MAX - s.orden));
    sectores.dedup_by_key(|s| s.off);
    ceros.sort_unstable_by_key(|c| c.inicio);
    let mut monton: BinaryHeap<(u32, u64)> = BinaryHeap::from(
        presupuesto.vec::<(u32, u64)>(ceros.len() as u64, "tramos a ceros activos")?,
    );
    let mut siguiente = 0usize;
    sectores.retain(|s| {
        while let Some(c) = ceros.get(siguiente).filter(|c| c.inicio <= s.off) {
            monton.push((c.orden, c.fin));
            siguiente += 1;
        }
        while monton.peek().is_some_and(|&(_, fin)| fin <= s.off) {
            monton.pop();
        }
        monton.peek().is_none_or(|&(o, _)| o < s.orden)
    });
    presupuesto.release(monton.into_vec());
    // Tramos a ceros fusionados.
    let mut fusion: Vec<(u64, u64)> = presupuesto.vec(ceros.len() as u64, "tramos a ceros")?;
    for c in &ceros {
        match fusion.last_mut() {
            Some((_, fin)) if c.inicio <= *fin => *fin = (*fin).max(c.fin),
            _ => fusion.push((c.inicio, c.fin)),
        }
    }
    presupuesto.release(ceros);

    Ok(Registro {
        log,
        sectores,
        ceros: fusion,
        tam_real: tam_fichero,
        tam_efectivo,
        entradas: n_entradas,
    })
}

impl Registro {
    /// Tamaño del fichero tras la reproducción (el real o LastFileOffset, el mayor).
    pub(crate) fn tam_efectivo(&self) -> u64 {
        self.tam_efectivo
    }

    /// Entradas reproducidas.
    pub(crate) fn entradas(&self) -> u64 {
        self.entradas
    }

    /// Sector reproducido: LeadingBytes + datos + TrailingBytes.
    fn reconstruir(&self, s: &SectorReproducido) -> [u8; SECTOR_US] {
        let d = s.desc as usize;
        let p = s.datos as usize;
        let mut out = [0u8; SECTOR_US];
        out[..8].copy_from_slice(&self.log[d + 8..d + 16]);
        out[8..4092].copy_from_slice(&self.log[p + 8..p + 4092]);
        out[4092..].copy_from_slice(&self.log[d + 4..d + 8]);
        out
    }

    /// Lee `buf.len()` bytes del fichero tal como queda tras reproducir el registro: el fichero
    /// real, ceros entre su final y el tamaño efectivo, y encima los tramos a ceros y los sectores
    /// reproducidos.
    pub(crate) fn leer(&self, file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
        let fin = offset
            .checked_add(buf.len() as u64)
            .filter(|&f| f <= self.tam_efectivo)
            .ok_or_else(|| IoError::Read {
                offset,
                len: buf.len(),
                source: std::io::ErrorKind::UnexpectedEof.into(),
            })?;
        let fin_real = fin.min(self.tam_real).max(offset);
        let (real, resto) = buf.split_at_mut((fin_real - offset) as usize);
        if !real.is_empty() {
            mr_io::read_exact_at(file, offset, real).map_err(|source| IoError::Read {
                offset,
                len: real.len(),
                source,
            })?;
        }
        resto.fill(0);
        let i = self.ceros.partition_point(|&(_, f)| f <= offset);
        for &(a, b) in self.ceros[i..].iter().take_while(|&&(a, _)| a < fin) {
            let (x, y) = (a.max(offset), b.min(fin));
            buf[(x - offset) as usize..(y - offset) as usize].fill(0);
        }
        let i = self.sectores.partition_point(|s| s.off + SECTOR <= offset);
        for s in self.sectores[i..].iter().take_while(|s| s.off < fin) {
            let datos = self.reconstruir(s);
            let (x, y) = (s.off.max(offset), (s.off + SECTOR).min(fin));
            buf[(x - offset) as usize..(y - offset) as usize]
                .copy_from_slice(&datos[(x - s.off) as usize..(y - s.off) as usize]);
        }
        Ok(())
    }
}
