//! VHDX fijo y dinámico (solo lectura), según la especificación pública [MS-VHDX]
//! «Virtual Hard Disk v2 (VHDX) File Format» de Microsoft Open Specifications.
//!
//! Estructura del fichero:
//! - 0 KiB: identificador `vhdxfile` (64 KiB).
//! - 64 KiB y 128 KiB: dos copias de la cabecera (`head`, CRC-32C de 4 KiB); manda la válida con
//!   mayor número de secuencia.
//! - 192 KiB y 256 KiB: dos copias de la tabla de regiones (`regi`, CRC-32C de 64 KiB).
//! - Regiones (alineadas a 1 MiB): registro (log), metadatos y BAT; después, los bloques de datos.
//!
//! Decisiones:
//! - Un registro pendiente (LogGuid ≠ 0) se valida y se reproduce **en memoria** antes de leer
//!   las tablas de regiones, los metadatos y la BAT (módulo `vhdx_log`): todas las lecturas del
//!   fichero pasan por la superposición resultante; el fichero nunca se escribe. Un registro
//!   pendiente que no supera la validación es `Corrupt` en ambos modos.
//! - Los discos diferenciales (HasParent) abiertos con [`VhdxImage::open`] devuelven
//!   `ExternalReference` con la ruta del padre según el localizador; el padre **nunca** se abre
//!   por su cuenta. La cadena solo se abre con los ficheros que indica el usuario
//!   (`crate::differencing`, incidencia 8): los bloques NOT_PRESENT y UNDEFINED y los sectores
//!   de un bloque PARTIALLY_PRESENT con su bit a 0 se leen del padre.
//! - Los bloques NOT_PRESENT, UNDEFINED, ZERO y UNMAPPED se leen como ceros y se informan como
//!   `Allocation::Sparse`; FULLY_PRESENT se lee del fichero; PARTIALLY_PRESENT solo es válido en
//!   discos diferenciales y aquí se considera corrupción.
//! - Modo tolerante ([`VhdxImage::open_with`], ADR-020): las entradas de bloque de la BAT con un
//!   estado no válido (4, 5, PARTIALLY_PRESENT), con el bloque fuera del fichero o en la zona de
//!   cabeceras, o que solapan el registro, los metadatos, la BAT, otra región u otro bloque
//!   (incluidas las duplicadas) se descartan: sus bloques se leen con error y se informan como datos
//!   no fiables. Las entradas de mapa de bits con estado no válido no afectan a la lectura de un
//!   disco sin padre: se cuentan y se ignoran. Identificador, cabeceras, tablas de regiones,
//!   metadatos y la ubicación de la BAT siguen siendo críticos (`Corrupt` en ambos modos).

use crate::budget::Budget;
use crate::crc32c::crc32c;
use crate::differencing::{Layer, Link};
use crate::external::ExternalHint;
use crate::tolerant::{self, Damage, Hit, Zone};
use crate::vhdx_log::{self, Registro};
use crate::MAX_TABLE_BYTES;
use mr_io::{Allocation, AllocationRun, BlockDevice, ImageFormat, IoError, Result};
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const FORMATO: &str = "VHDX";

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const TIB: u64 = 1024 * 1024 * MIB;

const FIRMA_FICHERO: &[u8; 8] = b"vhdxfile";
const FIRMA_CABECERA: &[u8; 4] = b"head";
const FIRMA_REGIONES: &[u8; 4] = b"regi";
const FIRMA_METADATOS: &[u8; 8] = b"metadata";

const OFF_CABECERAS: [u64; 2] = [64 * KIB, 128 * KIB];
const TAM_CABECERA: usize = 4096;
const OFF_TABLAS_REGIONES: [u64; 2] = [192 * KIB, 256 * KIB];
const TAM_TABLA_REGIONES: usize = 64 * 1024;
const MAX_ENTRADAS_REGIONES: u32 = 2047;
const TAM_TABLA_METADATOS: usize = 64 * 1024;
const MAX_ENTRADAS_METADATOS: u16 = 2047;
/// Tamaño máximo de un elemento de metadatos (la especificación lo limita a 1 MiB).
const MAX_ELEMENTO_METADATOS: u32 = 1024 * 1024;
/// Zona fija del principio del fichero (identificador, cabeceras y tablas de regiones).
const ZONA_CABECERAS: u64 = MIB;
/// Tamaño virtual máximo admitido por la especificación.
const MAX_TAMANO_VIRTUAL: u64 = 64 * TIB;
/// Trozo de lectura de la BAT (múltiplo de 8).
const TROZO_BAT: usize = 1024 * 1024;
/// Marca interna de un bloque descartado en modo tolerante (ningún desplazamiento válido, múltiplo
/// de 1 MiB, puede valer `u64::MAX`).
const DESCARTADO: u64 = u64::MAX;
/// Marca interna de un bloque cuyo contenido es el del padre (diferenciales). Los desplazamientos
/// válidos son múltiplos de 1 MiB, así que ninguno vale 1.
const DEL_PADRE: u64 = 1;
/// Bit interno de un bloque PARTIALLY_PRESENT (desplazamiento | `PARCIAL`).
const PARCIAL: u64 = 2;

// Estados de las entradas de bloque de datos de la BAT (bits 0–2).
const PAYLOAD_NOT_PRESENT: u64 = 0;
const PAYLOAD_UNDEFINED: u64 = 1;
const PAYLOAD_ZERO: u64 = 2;
const PAYLOAD_UNMAPPED: u64 = 3;
const PAYLOAD_FULLY_PRESENT: u64 = 6;
const PAYLOAD_PARTIALLY_PRESENT: u64 = 7;
// Estados de las entradas de mapa de bits de sectores.
const BITMAP_NOT_PRESENT: u64 = 0;
const BITMAP_PRESENT: u64 = 6;

// Banderas.
const REGION_REQUERIDA: u32 = 1;
const METADATO_REQUERIDO: u32 = 1 << 2;
const PARAM_DEJAR_BLOQUES_ASIGNADOS: u32 = 1;
const PARAM_TIENE_PADRE: u32 = 1 << 1;

type Guid = [u8; 16];

/// Construye un GUID en el orden de bytes de disco (los tres primeros campos en little endian).
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const GUID_REGION_BAT: Guid = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const GUID_REGION_METADATOS: Guid = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const GUID_PARAMETROS_FICHERO: Guid = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const GUID_TAMANO_VIRTUAL: Guid = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const GUID_ID_DISCO: Guid = guid(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const GUID_SECTOR_LOGICO: Guid = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const GUID_SECTOR_FISICO: Guid = guid(
    0xCDA3_48C7,
    0x445D,
    0x4471,
    [0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56],
);
const GUID_LOCALIZADOR_PADRE: Guid = guid(
    0xA8D3_5F2D,
    0xB30B,
    0x454D,
    [0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C],
);

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

fn trozo<const N: usize>(b: &[u8], o: usize) -> Result<[u8; N]> {
    o.checked_add(N)
        .and_then(|fin| b.get(o..fin))
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .ok_or_else(|| corrupto(format!("campo fuera de la estructura (posición {o})")))
}

fn le16(b: &[u8], o: usize) -> Result<u16> {
    trozo::<2>(b, o).map(u16::from_le_bytes)
}

fn le32(b: &[u8], o: usize) -> Result<u32> {
    trozo::<4>(b, o).map(u32::from_le_bytes)
}

fn le64(b: &[u8], o: usize) -> Result<u64> {
    trozo::<8>(b, o).map(u64::from_le_bytes)
}

fn formatear_guid(g: &Guid) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
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

fn leer(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    mr_io::read_exact_at(file, offset, buf).map_err(|source| IoError::Read {
        offset,
        len: buf.len(),
        source,
    })
}

/// Lee del fichero o, si hay un registro reproducido, del fichero tal como queda tras reproducirlo.
fn leer_en(file: &File, registro: Option<&Registro>, offset: u64, buf: &mut [u8]) -> Result<()> {
    match registro {
        Some(r) => r.leer(file, offset, buf),
        None => leer(file, offset, buf),
    }
}

/// Lee una estructura de tamaño fijo comprobando antes que cabe en el fichero (en su tamaño
/// efectivo, si hay un registro reproducido).
fn leer_estructura(
    file: &File,
    registro: Option<&Registro>,
    tam_fichero: u64,
    offset: u64,
    len: usize,
    nombre: &str,
) -> Result<Vec<u8>> {
    let fin = offset.checked_add(len as u64);
    if fin.is_none_or(|f| f > tam_fichero) {
        return Err(corrupto(format!(
            "{nombre} (desplazamiento {offset}, {len} bytes) fuera del fichero ({tam_fichero} bytes)"
        )));
    }
    let mut buf = vec![0u8; len];
    leer_en(file, registro, offset, &mut buf)?;
    Ok(buf)
}

/// Comprueba la firma y el CRC-32C (con el campo de suma a cero, bytes 4..8) de una estructura.
fn crc_valido(buf: &[u8], firma: &[u8; 4]) -> bool {
    if buf.len() < 8 || &buf[..4] != firma {
        return false;
    }
    let guardado = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let mut copia = buf.to_vec();
    copia[4..8].fill(0);
    crc32c(&copia) == guardado
}

#[derive(Debug, Clone)]
struct Cabecera {
    secuencia: u64,
    guid_datos: Guid,
    guid_registro: Guid,
    version_registro: u16,
    version: u16,
    long_registro: u32,
    off_registro: u64,
}

fn analizar_cabecera(buf: &[u8]) -> Result<Option<Cabecera>> {
    if !crc_valido(buf, FIRMA_CABECERA) {
        return Ok(None);
    }
    Ok(Some(Cabecera {
        secuencia: le64(buf, 8)?,
        guid_datos: trozo::<16>(buf, 32)?,
        guid_registro: trozo::<16>(buf, 48)?,
        version_registro: le16(buf, 64)?,
        version: le16(buf, 66)?,
        long_registro: le32(buf, 68)?,
        off_registro: le64(buf, 72)?,
    }))
}

#[derive(Debug, Clone, Copy)]
struct Region {
    guid: Guid,
    offset: u64,
    longitud: u64,
    requerida: bool,
}

fn analizar_tabla_regiones(buf: &[u8]) -> Result<Option<Vec<Region>>> {
    if !crc_valido(buf, FIRMA_REGIONES) {
        return Ok(None);
    }
    let n = le32(buf, 8)?;
    if n > MAX_ENTRADAS_REGIONES {
        return Err(corrupto(format!(
            "la tabla de regiones declara {n} entradas (máximo {MAX_ENTRADAS_REGIONES})"
        )));
    }
    let mut regiones = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let o = 16 + i * 32;
        regiones.push(Region {
            guid: trozo::<16>(buf, o)?,
            offset: le64(buf, o + 16)?,
            longitud: u64::from(le32(buf, o + 24)?),
            requerida: le32(buf, o + 28)? & REGION_REQUERIDA != 0,
        });
    }
    Ok(Some(regiones))
}

/// Tramo ocupado del fichero `[inicio, fin)`, para detectar solapes.
#[derive(Debug, Clone, Copy)]
struct Tramo {
    inicio: u64,
    fin: u64,
    nombre: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ElementoMetadatos {
    offset: u32,
    longitud: u32,
}

/// Datos de un disco diferencial.
struct Diferencial {
    tipo_localizador: Guid,
    /// Pares clave/valor del localizador del padre (tal como están en el fichero).
    pares: Vec<(String, String)>,
    vinculo: Option<Guid>,
    vinculo2: Option<Guid>,
    /// Desplazamiento del mapa de bits de sectores de cada grupo de `ratio` bloques (0 = no
    /// presente; [`DESCARTADO`] = no válido en modo tolerante).
    mapas: Vec<u64>,
    ratio: u64,
    /// Última porción de mapa de bits leída (bloque, bits de sus sectores).
    cache: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}

/// Disco VHDX fijo, dinámico o diferencial abierto en solo lectura.
pub struct VhdxImage {
    file: File,
    tam_virtual: u64,
    sector_logico: u32,
    sector_fisico: u32,
    tam_bloque: u32,
    dejar_asignados: bool,
    /// Desplazamiento en el fichero de cada bloque de datos (0 = no presente, se lee como ceros;
    /// [`DESCARTADO`] = descartado en modo tolerante).
    bloques: Vec<u64>,
    tolerante: bool,
    /// Bloques descartados en modo tolerante.
    descartados: u64,
    /// Entradas de mapa de bits con estado no válido ignoradas en modo tolerante.
    mapas_ignorados: u64,
    /// Registro pendiente reproducido en memoria, si lo había.
    registro: Option<Registro>,
    /// DataWriteGuid de la cabecera activa (identidad del contenido para los diferenciales).
    guid_datos: Guid,
    diferencial: Option<Diferencial>,
    /// Padre enlazado (solo en discos diferenciales abiertos con su cadena).
    padre: Option<Arc<dyn BlockDevice>>,
}

impl std::fmt::Debug for VhdxImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VhdxImage")
            .field("tam_virtual", &self.tam_virtual)
            .field("sector_logico", &self.sector_logico)
            .field("tam_bloque", &self.tam_bloque)
            .field("bloques", &self.bloques.len())
            .finish()
    }
}

impl VhdxImage {
    /// Interpreta `file` (ya admitido y abierto en solo lectura) como VHDX en modo estricto.
    /// Valida identificador, cabeceras, tablas de regiones, metadatos y BAT contra el tamaño real
    /// del fichero.
    pub fn open(file: File) -> Result<VhdxImage> {
        Self::open_with(file, false)
    }

    /// Como [`Self::open`]; con `tolerante`, las entradas de la BAT inválidas o ambiguas no impiden
    /// abrir la imagen: sus bloques se descartan (ver la documentación del módulo).
    pub fn open_with(file: File, tolerante: bool) -> Result<VhdxImage> {
        Self::abrir(file, tolerante, false)
    }

    /// Como [`Self::open_with`]; con `capa`, un disco diferencial se abre sin padre (para
    /// enlazarlo después con la cadena indicada por el usuario).
    fn abrir(file: File, tolerante: bool, capa: bool) -> Result<VhdxImage> {
        let tam_fichero = file.metadata()?.len();
        if tam_fichero < ZONA_CABECERAS {
            return Err(corrupto(format!(
                "fichero demasiado pequeño ({tam_fichero} bytes; mínimo 1 MiB)"
            )));
        }
        let id = leer_estructura(&file, None, tam_fichero, 0, 8, "identificador")?;
        if id.as_slice() != FIRMA_FICHERO {
            return Err(corrupto("falta el identificador «vhdxfile»"));
        }

        // Cabeceras: la válida con mayor número de secuencia.
        let mut activa: Option<Cabecera> = None;
        for off in OFF_CABECERAS {
            let buf = leer_estructura(&file, None, tam_fichero, off, TAM_CABECERA, "cabecera")?;
            if let Some(c) = analizar_cabecera(&buf)? {
                if activa.as_ref().is_none_or(|a| c.secuencia > a.secuencia) {
                    activa = Some(c);
                }
            }
        }
        let cab = activa.ok_or_else(|| {
            corrupto("ninguna de las dos cabeceras es válida (firma o CRC-32C incorrectos)")
        })?;
        if cab.version != 1 {
            return Err(no_soportado(format!(
                "versión de cabecera {} (se admite la 1)",
                cab.version
            )));
        }
        if cab.version_registro != 0 {
            return Err(no_soportado(format!(
                "versión de registro {} (se admite la 0)",
                cab.version_registro
            )));
        }

        let mut tramos = vec![Tramo {
            inicio: 0,
            fin: ZONA_CABECERAS,
            nombre: "cabeceras",
        }];
        if cab.long_registro != 0 {
            let long = u64::from(cab.long_registro);
            let fin = cab.off_registro.checked_add(long);
            if cab.off_registro % MIB != 0
                || long % MIB != 0
                || cab.off_registro < ZONA_CABECERAS
                || fin.is_none_or(|f| f > tam_fichero)
            {
                return Err(corrupto(format!(
                    "registro mal situado (desplazamiento {}, longitud {long})",
                    cab.off_registro
                )));
            }
            tramos.push(Tramo {
                inicio: cab.off_registro,
                fin: cab.off_registro + long,
                nombre: "registro",
            });
        }

        // Registro pendiente: se valida y se reproduce en memoria antes de leer el resto de
        // estructuras; desde aquí todas las lecturas pasan por él y los límites se comprueban
        // contra el tamaño efectivo del fichero (LastFileOffset, si es mayor).
        let mut presupuesto = Budget::new(FORMATO);
        let registro = if cab.guid_registro != [0u8; 16] {
            Some(vhdx_log::reproducir(
                &file,
                tam_fichero,
                cab.off_registro,
                u64::from(cab.long_registro),
                &cab.guid_registro,
                &mut presupuesto,
            )?)
        } else {
            None
        };
        let reg = registro.as_ref();
        let tam_fichero = reg.map_or(tam_fichero, Registro::tam_efectivo);

        // Tablas de regiones: la primera copia válida.
        let mut regiones = None;
        for off in OFF_TABLAS_REGIONES {
            let buf = leer_estructura(
                &file,
                reg,
                tam_fichero,
                off,
                TAM_TABLA_REGIONES,
                "tabla de regiones",
            )?;
            if let Some(r) = analizar_tabla_regiones(&buf)? {
                regiones = Some(r);
                break;
            }
        }
        let regiones = regiones.ok_or_else(|| {
            corrupto("ninguna de las dos tablas de regiones es válida (firma o CRC-32C)")
        })?;
        let mut bat: Option<Region> = None;
        let mut metadatos: Option<Region> = None;
        for r in &regiones {
            let nombre = if r.guid == GUID_REGION_BAT {
                "BAT"
            } else if r.guid == GUID_REGION_METADATOS {
                "metadatos"
            } else if r.requerida {
                return Err(no_soportado(format!(
                    "región requerida desconocida {}",
                    formatear_guid(&r.guid)
                )));
            } else {
                "región opcional"
            };
            let fin = r.offset.checked_add(r.longitud);
            if r.offset % MIB != 0
                || r.longitud % MIB != 0
                || r.longitud == 0
                || r.offset < ZONA_CABECERAS
                || fin.is_none_or(|f| f > tam_fichero)
            {
                return Err(corrupto(format!(
                    "región {nombre} mal situada (desplazamiento {}, longitud {}, fichero {tam_fichero})",
                    r.offset, r.longitud
                )));
            }
            let destino = match nombre {
                "BAT" => Some(&mut bat),
                "metadatos" => Some(&mut metadatos),
                _ => None,
            };
            if let Some(d) = destino {
                if d.is_some() {
                    return Err(corrupto(format!("región {nombre} duplicada")));
                }
                *d = Some(*r);
            }
            tramos.push(Tramo {
                inicio: r.offset,
                fin: r.offset + r.longitud,
                nombre,
            });
        }
        let bat = bat.ok_or_else(|| corrupto("falta la región BAT"))?;
        let metadatos = metadatos.ok_or_else(|| corrupto("falta la región de metadatos"))?;
        tramos.sort_by_key(|t| t.inicio);
        for par in tramos.windows(2) {
            if par[1].inicio < par[0].fin {
                return Err(corrupto(format!(
                    "las zonas {} y {} se solapan",
                    par[0].nombre, par[1].nombre
                )));
            }
        }

        // Metadatos.
        let tabla = leer_estructura(
            &file,
            reg,
            tam_fichero,
            metadatos.offset,
            TAM_TABLA_METADATOS,
            "tabla de metadatos",
        )?;
        if &tabla[..8] != FIRMA_METADATOS {
            return Err(corrupto(
                "falta la firma «metadata» de la tabla de metadatos",
            ));
        }
        let n = le16(&tabla, 10)?;
        if n > MAX_ENTRADAS_METADATOS {
            return Err(corrupto(format!(
                "la tabla de metadatos declara {n} entradas (máximo {MAX_ENTRADAS_METADATOS})"
            )));
        }
        let conocidos = [
            GUID_PARAMETROS_FICHERO,
            GUID_TAMANO_VIRTUAL,
            GUID_ID_DISCO,
            GUID_SECTOR_LOGICO,
            GUID_SECTOR_FISICO,
            GUID_LOCALIZADOR_PADRE,
        ];
        let mut elementos: [Option<ElementoMetadatos>; 6] = [None; 6];
        for i in 0..usize::from(n) {
            let o = 32 + i * 32;
            let id = trozo::<16>(&tabla, o)?;
            let offset = le32(&tabla, o + 16)?;
            let longitud = le32(&tabla, o + 20)?;
            let banderas = le32(&tabla, o + 24)?;
            let Some(k) = conocidos.iter().position(|g| *g == id) else {
                if banderas & METADATO_REQUERIDO != 0 {
                    return Err(no_soportado(format!(
                        "metadato requerido desconocido {}",
                        formatear_guid(&id)
                    )));
                }
                continue;
            };
            if longitud > MAX_ELEMENTO_METADATOS
                || (longitud > 0
                    && (u64::from(offset) < TAM_TABLA_METADATOS as u64
                        || u64::from(offset) + u64::from(longitud) > metadatos.longitud))
            {
                return Err(corrupto(format!(
                    "metadato {} fuera de la región (desplazamiento {offset}, longitud {longitud})",
                    formatear_guid(&id)
                )));
            }
            if elementos[k].is_some() {
                return Err(corrupto(format!(
                    "metadato {} duplicado",
                    formatear_guid(&id)
                )));
            }
            elementos[k] = Some(ElementoMetadatos { offset, longitud });
        }
        let leer_elemento = |k: usize, nombre: &str, minimo: u32| -> Result<Vec<u8>> {
            let e = elementos[k].ok_or_else(|| corrupto(format!("falta el metadato {nombre}")))?;
            if e.longitud < minimo {
                return Err(corrupto(format!(
                    "metadato {nombre} demasiado corto ({} bytes)",
                    e.longitud
                )));
            }
            leer_estructura(
                &file,
                reg,
                tam_fichero,
                metadatos.offset + u64::from(e.offset),
                e.longitud as usize,
                nombre,
            )
        };
        let params = leer_elemento(0, "File Parameters", 8)?;
        let tam_bloque = le32(&params, 0)?;
        let banderas = le32(&params, 4)?;
        if !tam_bloque.is_power_of_two() || !(MIB..=256 * MIB).contains(&u64::from(tam_bloque)) {
            return Err(corrupto(format!(
                "tamaño de bloque {tam_bloque} no válido (potencia de 2 entre 1 MiB y 256 MiB)"
            )));
        }
        let diferencial = if banderas & PARAM_TIENE_PADRE != 0 {
            if !capa {
                let referencia = match elementos[5] {
                    Some(e) if e.longitud > 0 => {
                        let buf = leer_elemento(5, "Parent Locator", 20)?;
                        describir_padre(&buf)
                    }
                    _ => None,
                };
                return Err(IoError::ExternalReference {
                    format: FORMATO,
                    reference: crate::external::sanitize(
                        &referencia
                            .unwrap_or_else(|| "disco padre (sin localizador legible)".to_string()),
                    ),
                });
            }
            if elementos[5].is_none_or(|e| e.longitud == 0) {
                return Err(corrupto(
                    "disco diferencial sin localizador del padre (Parent Locator)",
                ));
            }
            let buf = leer_elemento(5, "Parent Locator", 20)?;
            Some(analizar_localizador(&buf)?)
        } else {
            None
        };
        let es_diferencial = diferencial.is_some();
        let sector_logico = le32(&leer_elemento(3, "Logical Sector Size", 4)?, 0)?;
        if sector_logico != 512 && sector_logico != 4096 {
            return Err(corrupto(format!(
                "sector lógico de {sector_logico} bytes (debe ser 512 o 4096)"
            )));
        }
        let sector_fisico = le32(&leer_elemento(4, "Physical Sector Size", 4)?, 0)?;
        if sector_fisico != 512 && sector_fisico != 4096 {
            return Err(corrupto(format!(
                "sector físico de {sector_fisico} bytes (debe ser 512 o 4096)"
            )));
        }
        let tam_virtual = le64(&leer_elemento(1, "Virtual Disk Size", 8)?, 0)?;
        if tam_virtual == 0
            || tam_virtual > MAX_TAMANO_VIRTUAL
            || tam_virtual % u64::from(sector_logico) != 0
        {
            return Err(corrupto(format!(
                "tamaño virtual {tam_virtual} no válido (múltiplo del sector lógico, máximo 64 TiB)"
            )));
        }

        // BAT. En un disco diferencial cada grupo de `ratio` bloques lleva su entrada de mapa de
        // bits de sectores, también el último grupo incompleto.
        let tb = u64::from(tam_bloque);
        let n_bloques = tam_virtual.div_ceil(tb);
        let ratio = ((1u64 << 23) * u64::from(sector_logico)) / tb;
        let n_mapas = n_bloques.div_ceil(ratio);
        let entradas = if es_diferencial {
            n_mapas * (ratio + 1)
        } else {
            n_bloques + (n_bloques - 1) / ratio
        };
        let bytes_bat = entradas
            .checked_mul(8)
            .ok_or_else(|| corrupto("BAT demasiado grande"))?;
        if bytes_bat > MAX_TABLE_BYTES {
            return Err(no_soportado(format!(
                "la BAT ocupa {bytes_bat} bytes (límite de memoria {MAX_TABLE_BYTES})"
            )));
        }
        if bytes_bat > bat.longitud {
            return Err(corrupto(format!(
                "la región BAT ({} bytes) no cabe {entradas} entradas",
                bat.longitud
            )));
        }
        // Ya sabemos que la BAT cabe en el fichero: la reserva está acotada por su tamaño y por
        // el presupuesto de la apertura. La BAT se lee por trozos; en modo estricto la apertura
        // falla en la primera entrada inválida.
        presupuesto.add_tables(bytes_bat);
        let mut bloques: Vec<u64> = presupuesto.vec(n_bloques, "BAT")?;
        // Mapas de bits de sectores de un diferencial: desplazamiento (0 = no presente).
        let mut mapas: Vec<u64> = if es_diferencial {
            presupuesto.vec(n_mapas, "mapas de bits de sectores")?
        } else {
            Vec::new()
        };
        let mut dano = Damage::new(FORMATO, n_bloques, tolerante, &mut presupuesto)?;
        let mut presentes = 0u64;
        let mut mapas_ignorados = 0u64;
        let mut buf = vec![0u8; TROZO_BAT.min(bytes_bat as usize)];
        let mut indice = 0u64;
        let mut pos = 0u64;
        while pos < bytes_bat {
            let len = (bytes_bat - pos).min(buf.len() as u64) as usize;
            let trozo_bat = &mut buf[..len];
            leer_en(&file, reg, bat.offset + pos, trozo_bat)?;
            for e in trozo_bat.as_chunks::<8>().0 {
                let entrada = u64::from_le_bytes(*e);
                if (indice + 1).is_multiple_of(ratio + 1) {
                    if es_diferencial {
                        let c = indice / (ratio + 1);
                        match validar_mapa(entrada, c, tam_fichero) {
                            Ok(off) => mapas.push(off),
                            Err(motivo) if !tolerante => return Err(corrupto(motivo)),
                            Err(_) => mapas.push(DESCARTADO),
                        }
                    } else {
                        let estado = entrada & 7;
                        if estado != BITMAP_NOT_PRESENT && estado != BITMAP_PRESENT {
                            if !tolerante {
                                return Err(corrupto(format!(
                                    "estado {estado} no válido en la entrada de mapa de bits {indice} de la BAT"
                                )));
                            }
                            mapas_ignorados += 1;
                        }
                    }
                } else if (bloques.len() as u64) < n_bloques {
                    let b = bloques.len() as u64;
                    match validar_bloque(entrada, b, tb, tam_virtual, tam_fichero, es_diferencial) {
                        Ok(destino) => {
                            if es_fisico(destino) {
                                presentes += 1;
                            }
                            bloques.push(destino);
                        }
                        Err(motivo) => {
                            dano.add(b, || motivo)?;
                            bloques.push(DESCARTADO);
                        }
                    }
                }
                indice += 1;
            }
            pos += len as u64;
        }
        let mapas_esperados = if es_diferencial { n_mapas } else { 0 };
        if bloques.len() as u64 != n_bloques || mapas.len() as u64 != mapas_esperados {
            return Err(corrupto(
                "número de entradas de bloque incoherente en la BAT",
            ));
        }
        // Un bloque PARTIALLY_PRESENT necesita el mapa de bits de su grupo.
        for b in 0..n_bloques {
            let v = bloques[b as usize];
            if es_fisico(v) && v & PARCIAL != 0 {
                let m = mapas[(b / ratio) as usize];
                if m == 0 || m == DESCARTADO {
                    dano.add(b, || {
                        format!(
                            "el bloque {b} es PARTIALLY_PRESENT y el mapa de bits de sectores de su grupo {} no es válido o no está presente",
                            b / ratio
                        )
                    })?;
                }
            }
        }
        // Correspondencia inequívoca: ningún bloque presente solapa las zonas del contenedor
        // (cabeceras, registro, regiones, mapas de bits de sectores) ni otro bloque.
        let tramo = |b: u32| -> (u64, u64) {
            let b = u64::from(b);
            let off = bloques[b as usize] & !(MIB - 1);
            let util = tb.min(tam_virtual - b * tb);
            (off, off + util)
        };
        // Índices `u32`: la BAT tiene como mucho MAX_TABLE_BYTES / 8 = 2^25 entradas.
        let mut candidatos: Vec<u32> = presupuesto.vec(presentes, "lista de bloques presentes")?;
        candidatos.extend(
            (0..n_bloques)
                .filter(|&b| es_fisico(bloques[b as usize]) && !dano.contains(b))
                .map(|b| b as u32),
        );
        let mut zonas: Vec<Zone> = tramos
            .iter()
            .map(|t| Zone::fixed(t.inicio, t.fin - t.inicio))
            .collect();
        for (c, &m) in mapas.iter().enumerate() {
            if m != 0 && m != DESCARTADO {
                zonas.push(Zone {
                    start: m,
                    end: m + MIB,
                    tag: Some(c as u64),
                });
            }
        }
        let mut mapas_solapados: Vec<u64> = Vec::new();
        tolerant::overlaps(&mut candidatos, tramo, &mut zonas, |solape| {
            match solape {
            Hit::Entry(b) => {
                let off = bloques[b as usize] & !(MIB - 1);
                dano.add(u64::from(b), || {
                    format!(
                        "el bloque {b} (desplazamiento {off}) se solapa con las estructuras del contenedor o con otro bloque"
                    )
                })
            }
            Hit::Tag(c) if tolerante => {
                mapas_solapados.push(c);
                Ok(())
            }
            Hit::Tag(c) => Err(corrupto(format!(
                "el mapa de bits de sectores del grupo {c} se solapa con las estructuras del contenedor o con un bloque"
            ))),
        }
        })?;
        presupuesto.release(candidatos);
        for c in mapas_solapados {
            mapas[c as usize] = DESCARTADO;
            let fin = ((c + 1) * ratio).min(n_bloques);
            for b in c * ratio..fin {
                let v = bloques[b as usize];
                if es_fisico(v) && v & PARCIAL != 0 {
                    dano.add(b, String::new)?;
                }
            }
        }
        for b in dano.iter() {
            bloques[b as usize] = DESCARTADO;
        }
        let descartados = dano.count();
        dano.release(&mut presupuesto);

        let diferencial = diferencial.map(|(tipo_localizador, pares)| {
            let vinculo = |clave: &str| {
                pares
                    .iter()
                    .find(|(k, _)| k == clave)
                    .and_then(|(_, v)| analizar_guid_texto(v))
            };
            Diferencial {
                tipo_localizador,
                vinculo: vinculo("parent_linkage"),
                vinculo2: vinculo("parent_linkage2"),
                pares,
                mapas,
                ratio,
                cache: Mutex::new(None),
            }
        });

        Ok(VhdxImage {
            file,
            tam_virtual,
            sector_logico,
            sector_fisico,
            tam_bloque,
            dejar_asignados: banderas & PARAM_DEJAR_BLOQUES_ASIGNADOS != 0,
            bloques,
            tolerante,
            descartados,
            mapas_ignorados,
            registro,
            guid_datos: cab.guid_datos,
            diferencial,
            padre: None,
        })
    }

    /// Descripción legible de la variante.
    pub fn describe(&self) -> String {
        let tipo = if self.diferencial.is_some() {
            "diferencial"
        } else if self.dejar_asignados {
            "fijo"
        } else {
            "dinámico"
        };
        let mut s = format!(
            "VHDX {tipo}, bloques de {} MiB, sector lógico {} B, sector físico {} B",
            u64::from(self.tam_bloque) / MIB,
            self.sector_logico,
            self.sector_fisico
        );
        if let Some(r) = &self.registro {
            let n = r.entradas();
            s.push_str(&format!(
                "; registro reproducido en memoria ({n} {})",
                if n == 1 { "entrada" } else { "entradas" }
            ));
        }
        if self.tolerante {
            s.push_str(&tolerant::describe_suffix(
                self.descartados,
                "bloque",
                "bloques",
            ));
            if self.mapas_ignorados > 0 {
                s.push_str(&format!(
                    ", {} entradas de mapa de bits no válidas ignoradas",
                    self.mapas_ignorados
                ));
            }
        }
        s
    }

    /// `true` si se abrió en modo tolerante.
    pub fn is_tolerant(&self) -> bool {
        self.tolerante
    }

    /// Entradas del registro pendiente reproducidas en memoria (0 si no había registro pendiente).
    pub fn replayed_log_entries(&self) -> u64 {
        self.registro.as_ref().map_or(0, Registro::entradas)
    }

    /// Bloques descartados en modo tolerante (0 en modo estricto).
    pub fn discarded_blocks(&self) -> u64 {
        self.descartados
    }

    /// Tamaño de bloque de datos en bytes.
    pub fn block_size(&self) -> u32 {
        self.tam_bloque
    }

    /// Tamaño de sector físico declarado en los metadatos.
    pub fn physical_sector_size(&self) -> u32 {
        self.sector_fisico
    }

    /// `true` si el fichero declara LeaveBlocksAllocated (lo habitual en discos fijos).
    pub fn is_fixed(&self) -> bool {
        self.dejar_asignados
    }
}

/// Valida el estado de una entrada de bloque de datos y que su bloque cabe en el fichero fuera de la
/// zona de cabeceras; devuelve su desplazamiento en el fichero (0 si el bloque se lee como ceros)
/// o el motivo por el que no es válida. Los solapes se comprueban después, con todos los bloques.
fn validar_bloque(
    entrada: u64,
    b: u64,
    tb: u64,
    tam_virtual: u64,
    tam_fichero: u64,
    diferencial: bool,
) -> std::result::Result<u64, String> {
    match entrada & 7 {
        // En un diferencial, NOT_PRESENT y UNDEFINED remiten al padre; ZERO y UNMAPPED (descartado
        // en esta capa) son ceros.
        PAYLOAD_NOT_PRESENT | PAYLOAD_UNDEFINED if diferencial => Ok(DEL_PADRE),
        PAYLOAD_NOT_PRESENT | PAYLOAD_UNDEFINED | PAYLOAD_ZERO | PAYLOAD_UNMAPPED => Ok(0),
        estado @ (PAYLOAD_FULLY_PRESENT | PAYLOAD_PARTIALLY_PRESENT)
            if estado == PAYLOAD_FULLY_PRESENT || diferencial =>
        {
            let off = (entrada >> 20)
                .checked_mul(MIB)
                .ok_or_else(|| format!("desplazamiento del bloque {b} desbordado"))?;
            // Solo se exige en el fichero la parte del bloque que cubre el disco virtual.
            let inicio_virtual = b * tb;
            let util = tb.min(tam_virtual - inicio_virtual);
            let fin = off.checked_add(util);
            if off < ZONA_CABECERAS || fin.is_none_or(|f| f > tam_fichero) {
                return Err(format!(
                    "el bloque {b} (desplazamiento {off}) queda fuera del fichero ({tam_fichero} bytes) o en la zona de cabeceras"
                ));
            }
            Ok(if estado == PAYLOAD_PARTIALLY_PRESENT {
                off | PARCIAL
            } else {
                off
            })
        }
        PAYLOAD_PARTIALLY_PRESENT => Err(format!(
            "bloque {b} PARTIALLY_PRESENT en un disco sin padre"
        )),
        estado => Err(format!(
            "estado {estado} no válido en la entrada del bloque {b} de la BAT"
        )),
    }
}

/// Extrae la ruta del padre del localizador (UTF-16LE). Preferencia: relative_path,
/// absolute_win32_path, volume_path y, en último término, parent_linkage.
fn describir_padre(buf: &[u8]) -> Option<String> {
    let n = usize::from(le16(buf, 18).ok()?);
    let utf16 = |o: u32, l: u16| -> Option<String> {
        let o = o as usize;
        let s = buf.get(o..o.checked_add(usize::from(l))?)?;
        if s.len() % 2 != 0 {
            return None;
        }
        let u: Vec<u16> = s
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Some(String::from_utf16_lossy(&u))
    };
    let mut pares = Vec::new();
    for i in 0..n {
        let o = 20usize.checked_add(i.checked_mul(12)?)?;
        let ko = le32(buf, o).ok()?;
        let vo = le32(buf, o + 4).ok()?;
        let kl = le16(buf, o + 8).ok()?;
        let vl = le16(buf, o + 10).ok()?;
        if let (Some(k), Some(v)) = (utf16(ko, kl), utf16(vo, vl)) {
            pares.push((k, v));
        }
    }
    for clave in [
        "relative_path",
        "absolute_win32_path",
        "volume_path",
        "parent_linkage",
    ] {
        if let Some((_, v)) = pares.iter().find(|(k, v)| k == clave && !v.is_empty()) {
            return Some(v.clone());
        }
    }
    None
}

/// `true` si el valor interno de un bloque es un desplazamiento en el fichero (FULLY o
/// PARTIALLY_PRESENT).
fn es_fisico(v: u64) -> bool {
    !matches!(v, 0 | DEL_PADRE | DESCARTADO)
}

/// Valida la entrada de mapa de bits de sectores del grupo `c` de un diferencial: NOT_PRESENT
/// (0) o PRESENT con su MiB entero dentro del fichero y fuera de la zona de cabeceras.
fn validar_mapa(entrada: u64, c: u64, tam_fichero: u64) -> std::result::Result<u64, String> {
    match entrada & 7 {
        BITMAP_NOT_PRESENT => Ok(0),
        BITMAP_PRESENT => {
            let off = (entrada >> 20)
                .checked_mul(MIB)
                .ok_or_else(|| format!("desplazamiento del mapa de bits {c} desbordado"))?;
            if off < ZONA_CABECERAS || off.checked_add(MIB).is_none_or(|f| f > tam_fichero) {
                return Err(format!(
                    "el mapa de bits de sectores {c} (desplazamiento {off}) queda fuera del fichero ({tam_fichero} bytes) o en la zona de cabeceras"
                ));
            }
            Ok(off)
        }
        estado => Err(format!(
            "estado {estado} no válido en la entrada del mapa de bits de sectores {c} de la BAT"
        )),
    }
}

/// Interpreta el localizador del padre ([MS-VHDX] «Parent Locator»): tipo (GUID), número de
/// entradas y pares clave/valor en UTF-16LE con desplazamientos relativos al elemento. Toda
/// entrada debe quedar dentro del elemento con longitudes pares y clave no vacía.
fn analizar_localizador(buf: &[u8]) -> Result<(Guid, Vec<(String, String)>)> {
    let tipo = trozo::<16>(buf, 0)?;
    let n = usize::from(le16(buf, 18)?);
    if 20 + n * 12 > buf.len() {
        return Err(corrupto(format!(
            "el localizador del padre declara {n} entradas y mide {} bytes",
            buf.len()
        )));
    }
    let texto = |o: u32, l: u16, que: &str| -> Result<String> {
        let o = o as usize;
        let s = o
            .checked_add(usize::from(l))
            .and_then(|f| buf.get(o..f))
            .filter(|s| s.len() % 2 == 0)
            .ok_or_else(|| {
                corrupto(format!(
                    "{que} del localizador del padre fuera del elemento o de longitud impar"
                ))
            })?;
        let u: Vec<u16> = s
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Ok(String::from_utf16_lossy(&u))
    };
    let mut pares = Vec::with_capacity(n);
    for i in 0..n {
        let o = 20 + i * 12;
        let (ko, vo, kl, vl) = (
            le32(buf, o)?,
            le32(buf, o + 4)?,
            le16(buf, o + 8)?,
            le16(buf, o + 10)?,
        );
        if kl == 0 {
            return Err(corrupto("clave vacía en el localizador del padre"));
        }
        pares.push((texto(ko, kl, "clave")?, texto(vo, vl, "valor")?));
    }
    Ok((tipo, pares))
}

/// GUID escrito como texto `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (con o sin llaves), en el
/// orden de bytes de disco.
fn analizar_guid_texto(s: &str) -> Option<Guid> {
    let s = s.trim();
    let s = s
        .strip_prefix('{')
        .and_then(|x| x.strip_suffix('}'))
        .unwrap_or(s);
    let partes: Vec<&str> = s.split('-').collect();
    if partes.len() != 5
        || partes
            .iter()
            .zip([8, 4, 4, 4, 12])
            .any(|(p, l)| p.len() != l || !p.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return None;
    }
    let d1 = u32::from_str_radix(partes[0], 16).ok()?;
    let d2 = u16::from_str_radix(partes[1], 16).ok()?;
    let d3 = u16::from_str_radix(partes[2], 16).ok()?;
    let resto = format!("{}{}", partes[3], partes[4]);
    let mut d4 = [0u8; 8];
    for (i, d) in d4.iter_mut().enumerate() {
        *d = u8::from_str_radix(&resto[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(guid(d1, d2, d3, d4))
}

impl VhdxImage {
    /// Contenido del padre (diferenciales). `ExternalReference` si no se ha enlazado, lo que no
    /// ocurre en las imágenes que entrega la apertura.
    fn leer_padre(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match &self.padre {
            Some(p) => p.read_at(offset, buf),
            None => Err(IoError::ExternalReference {
                format: FORMATO,
                reference: self.referencia_padre(),
            }),
        }
    }

    /// Tramos de asignación de los sectores `[lba, lba + n)` que se leen del padre (`Data`,
    /// conservador, si el padre no puede informar).
    fn tramos_padre(&self, tramos: &mut Vec<AllocationRun>, lba: u64, n: u64) {
        match self.padre.as_ref().map(|p| p.allocation(lba, n)) {
            Some(Ok(v)) => {
                for (l, c, t) in v {
                    anadir_tramo(tramos, l, c, t);
                }
            }
            _ => anadir_tramo(tramos, lba, n, Allocation::Data),
        }
    }

    /// Texto con el que se nombra el padre (preferencia: relative_path, absolute_win32_path,
    /// volume_path, parent_linkage).
    fn referencia_padre(&self) -> String {
        let Some(d) = &self.diferencial else {
            return String::new();
        };
        let texto = [
            "relative_path",
            "absolute_win32_path",
            "volume_path",
            "parent_linkage",
        ]
        .iter()
        .find_map(|c| {
            d.pares
                .iter()
                .find(|(k, v)| k == c && !v.is_empty())
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|| "disco padre (sin localizador legible)".to_string());
        crate::external::sanitize(&texto)
    }

    /// Bits de los sectores del bloque PARTIALLY_PRESENT `b` (uno por sector lógico, el bit 0 del
    /// byte 0 es el primer sector), con caché de una entrada. Un fallo de lectura se informa con
    /// el rango virtual `(voff, len)` que lo necesitaba.
    fn mapa_bloque(&self, d: &Diferencial, b: u64, voff: u64, len: usize) -> Result<Arc<Vec<u8>>> {
        {
            let g = d.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((x, m)) = g.as_ref() {
                if *x == b {
                    return Ok(Arc::clone(m));
                }
            }
        }
        let spb = u64::from(self.tam_bloque) / u64::from(self.sector_logico);
        let base = d.mapas[(b / d.ratio) as usize] + (b % d.ratio) * spb / 8;
        // spb / 8 ≤ 256 MiB / 512 / 8 = 64 KiB.
        let mut m = vec![0u8; (spb / 8) as usize];
        leer_en(&self.file, self.registro.as_ref(), base, &mut m).map_err(|e| match e {
            IoError::Read { source, .. } => IoError::Read {
                offset: voff,
                len,
                source,
            },
            otro => otro,
        })?;
        let m = Arc::new(m);
        *d.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((b, Arc::clone(&m)));
        Ok(m)
    }

    /// Lee `parte` (bytes `dentro..` del bloque `b`, virtual `pos`) de un bloque PARTIALLY_PRESENT.
    fn leer_parcial(
        &self,
        d: &Diferencial,
        b: u64,
        base: u64,
        dentro: u64,
        pos: u64,
        parte: &mut [u8],
    ) -> Result<()> {
        let ss = u64::from(self.sector_logico);
        let mapa = self.mapa_bloque(d, b, pos, parte.len())?;
        let fin = dentro + parte.len() as u64;
        let mut o = dentro;
        while o < fin {
            let s = o / ss;
            let presente = bit_lsb(&mapa, s);
            let mut t = s + 1;
            while t * ss < fin && bit_lsb(&mapa, t) == presente {
                t += 1;
            }
            let hasta = (t * ss).min(fin);
            let trozo = &mut parte[(o - dentro) as usize..(hasta - dentro) as usize];
            let v = pos + (o - dentro);
            if presente {
                leer_en(&self.file, self.registro.as_ref(), base + o, trozo)?;
            } else {
                self.leer_padre(v, trozo)?;
            }
            o = hasta;
        }
        Ok(())
    }
}

/// Bit `s` de un mapa de bits de sectores de VHDX (el bit 0 de cada byte es el primer sector).
fn bit_lsb(mapa: &[u8], s: u64) -> bool {
    usize::try_from(s / 8)
        .ok()
        .and_then(|i| mapa.get(i))
        .is_some_and(|&x| x & (1 << (s % 8)) != 0)
}

/// Añade un tramo fusionándolo con el anterior si es contiguo y del mismo tipo.
fn anadir_tramo(tramos: &mut Vec<AllocationRun>, lba: u64, n: u64, tipo: Allocation) {
    if n == 0 {
        return;
    }
    match tramos.last_mut() {
        Some((l, c, t)) if *t == tipo && *l + *c == lba => *c += n,
        _ => tramos.push((lba, n, tipo)),
    }
}

impl Layer for VhdxImage {
    const FORMAT: &'static str = FORMATO;
    const IMAGE_FORMAT: ImageFormat = ImageFormat::Vhdx;

    fn open_layer(file: File, tolerant: bool) -> Result<Self> {
        Self::abrir(file, tolerant, true)
    }

    fn is_differencing(&self) -> bool {
        self.diferencial.is_some()
    }

    fn identity(&self) -> [u8; 16] {
        self.guid_datos
    }

    /// Especificación: el DataWriteGuid del padre debe ser `parent_linkage` (o, si existe,
    /// `parent_linkage2`, que la especificación admite como vínculo alternativo).
    fn link(&self, parent: &Self, _parent_mtime: Option<SystemTime>) -> Link {
        let Some(d) = &self.diferencial else {
            return Link::Mismatch("la imagen no es un disco diferencial".into());
        };
        let Some(v) = d.vinculo else {
            return Link::Mismatch(
                "el localizador no tiene un parent_linkage válido: no se puede verificar".into(),
            );
        };
        if v == parent.guid_datos {
            return Link::Verified(Vec::new());
        }
        if d.vinculo2 == Some(parent.guid_datos) {
            return Link::Verified(vec!["vínculo verificado por parent_linkage2".to_string()]);
        }
        Link::Mismatch(format!(
            "el diferencial espera el DataWriteGuid {} y el fichero tiene {}",
            formatear_guid(&v),
            formatear_guid(&parent.guid_datos)
        ))
    }

    fn hints(&self) -> Vec<ExternalHint> {
        let Some(d) = &self.diferencial else {
            return Vec::new();
        };
        let mut v = vec![ExternalHint::new(
            "locator_type",
            &formatear_guid(&d.tipo_localizador),
        )];
        v.extend(d.pares.iter().map(|(k, x)| ExternalHint::new(k, x)));
        v
    }

    fn reference(&self) -> String {
        self.referencia_padre()
    }

    fn attach_parent(&mut self, parent: Arc<dyn BlockDevice>) {
        self.padre = Some(parent);
    }

    fn describe(&self) -> String {
        VhdxImage::describe(self)
    }

    fn discarded_blocks(&self) -> u64 {
        self.descartados
    }
}

impl BlockDevice for VhdxImage {
    fn size(&self) -> u64 {
        self.tam_virtual
    }

    fn sector_size(&self) -> u32 {
        self.sector_logico
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let fin = offset.checked_add(buf.len() as u64);
        if fin.is_none_or(|f| f > self.tam_virtual) {
            return Err(IoError::OutOfBounds {
                offset,
                len: buf.len(),
                size: self.tam_virtual,
            });
        }
        let tb = u64::from(self.tam_bloque);
        let mut pos = offset;
        let mut resto = &mut buf[..];
        while !resto.is_empty() {
            let b = pos / tb;
            let dentro = pos % tb;
            let n = (tb - dentro).min(resto.len() as u64) as usize;
            let (parte, siguiente) = resto.split_at_mut(n);
            let v = self
                .bloques
                .get(b as usize)
                .copied()
                .ok_or_else(|| corrupto(format!("bloque {b} fuera de la BAT")))?;
            match v {
                0 => parte.fill(0),
                DEL_PADRE => self.leer_padre(pos, parte)?,
                DESCARTADO => return Err(tolerant::discarded_read(FORMATO, "bloque", b, pos, n)),
                _ if v & PARCIAL != 0 => {
                    let d = self.diferencial.as_ref().ok_or_else(|| {
                        corrupto(format!("bloque {b} parcial en un disco sin padre"))
                    })?;
                    self.leer_parcial(d, b, v & !(MIB - 1), dentro, pos, parte)?;
                }
                base => leer_en(&self.file, self.registro.as_ref(), base + dentro, parte)?,
            }
            pos += n as u64;
            resto = siguiente;
        }
        Ok(())
    }

    fn allocation(&self, lba: u64, count: u64) -> Result<Vec<AllocationRun>> {
        let total = self.sector_count();
        let fin = lba.checked_add(count);
        if count == 0 || fin.is_none_or(|e| e > total) {
            return Err(IoError::InvalidRange { lba, count, total });
        }
        let fin = lba + count;
        let por_bloque = u64::from(self.tam_bloque) / u64::from(self.sector_logico);
        let mut tramos: Vec<AllocationRun> = Vec::new();
        let mut s = lba;
        while s < fin {
            let b = s / por_bloque;
            let primero = b * por_bloque;
            let limite = ((b + 1) * por_bloque).min(fin);
            // Un bloque descartado es `Data`: hay que «leerlo» para descubrir que no es fiable.
            match self.bloques.get(b as usize).copied() {
                Some(0) | None => anadir_tramo(&mut tramos, s, limite - s, Allocation::Sparse),
                Some(DEL_PADRE) => self.tramos_padre(&mut tramos, s, limite - s),
                Some(v) if v != DESCARTADO && v & PARCIAL != 0 => {
                    let mapa = self.diferencial.as_ref().and_then(|d| {
                        let len = usize::try_from((limite - s) * u64::from(self.sector_logico))
                            .unwrap_or(usize::MAX);
                        let voff = s * u64::from(self.sector_logico);
                        self.mapa_bloque(d, b, voff, len).ok()
                    });
                    let Some(mapa) = mapa else {
                        // Mapa ilegible: no se sabe qué sectores tiene esta capa (conservador).
                        anadir_tramo(&mut tramos, s, limite - s, Allocation::Data);
                        s = limite;
                        continue;
                    };
                    let mut r = s;
                    while r < limite {
                        let presente = bit_lsb(&mapa, r - primero);
                        let mut t = r + 1;
                        while t < limite && bit_lsb(&mapa, t - primero) == presente {
                            t += 1;
                        }
                        if presente {
                            anadir_tramo(&mut tramos, r, t - r, Allocation::Data);
                        } else {
                            self.tramos_padre(&mut tramos, r, t - r);
                        }
                        r = t;
                    }
                }
                Some(_) => anadir_tramo(&mut tramos, s, limite - s, Allocation::Data),
            }
            s = limite;
        }
        Ok(tramos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Patrón determinista para el contenido virtual en `vo`.
    fn patron(vo: u64) -> u8 {
        let x = vo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (vo >> 9);
        (x >> 56) as u8 ^ (vo as u8)
    }

    struct Modelo {
        tam_bloque: u32,
        sector_logico: u32,
        tam_virtual: u64,
        banderas: u32,
        /// (bloque, estado); el resto queda NOT_PRESENT.
        estados: Vec<(u64, u64)>,
        padre: Option<String>,
    }

    impl Modelo {
        fn basico() -> Self {
            // 4 bloques de 1 MiB, el último parcial (3,5 MiB virtuales).
            Self {
                tam_bloque: MIB as u32,
                sector_logico: 512,
                tam_virtual: 3 * MIB + 512 * KIB,
                banderas: 0,
                estados: vec![
                    (0, PAYLOAD_FULLY_PRESENT),
                    (1, PAYLOAD_NOT_PRESENT),
                    (2, PAYLOAD_ZERO),
                    (3, PAYLOAD_FULLY_PRESENT),
                ],
                padre: None,
            }
        }
    }

    // Disposición: registro en 1 MiB, metadatos en 2 MiB, BAT en 3 MiB, datos a continuación.
    const OFF_REGISTRO: u64 = MIB;
    const OFF_META: u64 = 2 * MIB;
    const OFF_BAT: u64 = 3 * MIB;

    fn poner(img: &mut [u8], off: u64, datos: &[u8]) {
        img[off as usize..off as usize + datos.len()].copy_from_slice(datos);
    }

    fn sellar(img: &mut [u8], off: u64, len: usize) {
        let o = off as usize;
        img[o + 4..o + 8].fill(0);
        let c = crc32c(&img[o..o + len]);
        img[o + 4..o + 8].copy_from_slice(&c.to_le_bytes());
    }

    fn sellar_cabeceras(img: &mut [u8]) {
        for off in OFF_CABECERAS {
            sellar(img, off, TAM_CABECERA);
        }
    }

    fn sellar_regiones(img: &mut [u8]) {
        for off in OFF_TABLAS_REGIONES {
            sellar(img, off, TAM_TABLA_REGIONES);
        }
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    fn construir(m: &Modelo) -> Vec<u8> {
        let tb = u64::from(m.tam_bloque);
        let n_bloques = m.tam_virtual.div_ceil(tb);
        let ratio = ((1u64 << 23) * u64::from(m.sector_logico)) / tb;
        let entradas = n_bloques + (n_bloques - 1) / ratio;
        let long_bat = (entradas * 8).div_ceil(MIB).max(1) * MIB;
        let mut siguiente = OFF_BAT + long_bat;
        let mut bat = vec![0u64; entradas as usize];
        let mut datos: Vec<(u64, u64)> = Vec::new(); // (desplazamiento en fichero, bloque)
        for &(b, estado) in &m.estados {
            let mut e = estado;
            if estado == PAYLOAD_FULLY_PRESENT {
                e |= (siguiente / MIB) << 20;
                datos.push((siguiente, b));
                siguiente += tb;
            }
            bat[(b + b / ratio) as usize] = e;
        }
        let mut img = vec![0u8; siguiente as usize];
        poner(&mut img, 0, FIRMA_FICHERO);
        for (i, off) in OFF_CABECERAS.into_iter().enumerate() {
            poner(&mut img, off, FIRMA_CABECERA);
            poner(&mut img, off + 8, &(i as u64 + 1).to_le_bytes());
            poner(&mut img, off + 16, &[0x11; 16]);
            poner(&mut img, off + 32, &[0x22; 16]);
            poner(&mut img, off + 66, &1u16.to_le_bytes());
            poner(&mut img, off + 68, &(MIB as u32).to_le_bytes());
            poner(&mut img, off + 72, &OFF_REGISTRO.to_le_bytes());
        }
        sellar_cabeceras(&mut img);
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off, FIRMA_REGIONES);
            poner(&mut img, off + 8, &2u32.to_le_bytes());
            let e = off + 16;
            poner(&mut img, e, &GUID_REGION_BAT);
            poner(&mut img, e + 16, &OFF_BAT.to_le_bytes());
            poner(&mut img, e + 24, &(long_bat as u32).to_le_bytes());
            poner(&mut img, e + 28, &1u32.to_le_bytes());
            let e = off + 48;
            poner(&mut img, e, &GUID_REGION_METADATOS);
            poner(&mut img, e + 16, &OFF_META.to_le_bytes());
            poner(&mut img, e + 24, &(MIB as u32).to_le_bytes());
            poner(&mut img, e + 28, &1u32.to_le_bytes());
        }
        sellar_regiones(&mut img);

        // Metadatos.
        let mut items: Vec<(Guid, u32, Vec<u8>)> = vec![
            (GUID_PARAMETROS_FICHERO, METADATO_REQUERIDO, {
                let mut v = m.tam_bloque.to_le_bytes().to_vec();
                let mut b = m.banderas;
                if m.padre.is_some() {
                    b |= PARAM_TIENE_PADRE;
                }
                v.extend_from_slice(&b.to_le_bytes());
                v
            }),
            (
                GUID_TAMANO_VIRTUAL,
                METADATO_REQUERIDO | 2,
                m.tam_virtual.to_le_bytes().to_vec(),
            ),
            (
                GUID_SECTOR_LOGICO,
                METADATO_REQUERIDO | 2,
                m.sector_logico.to_le_bytes().to_vec(),
            ),
            (
                GUID_SECTOR_FISICO,
                METADATO_REQUERIDO | 2,
                4096u32.to_le_bytes().to_vec(),
            ),
            (GUID_ID_DISCO, METADATO_REQUERIDO | 2, vec![0x33; 16]),
        ];
        if let Some(p) = &m.padre {
            let clave = utf16("relative_path");
            let valor = utf16(p);
            let mut v = vec![0xAA; 16]; // tipo de localizador (no se interpreta)
            v.extend_from_slice(&0u16.to_le_bytes());
            v.extend_from_slice(&1u16.to_le_bytes());
            let ko = 32u32;
            let vo = ko + clave.len() as u32;
            v.extend_from_slice(&ko.to_le_bytes());
            v.extend_from_slice(&vo.to_le_bytes());
            v.extend_from_slice(&(clave.len() as u16).to_le_bytes());
            v.extend_from_slice(&(valor.len() as u16).to_le_bytes());
            v.resize(32, 0);
            v.extend_from_slice(&clave);
            v.extend_from_slice(&valor);
            items.push((GUID_LOCALIZADOR_PADRE, METADATO_REQUERIDO, v));
        }
        poner(&mut img, OFF_META, FIRMA_METADATOS);
        poner(&mut img, OFF_META + 10, &(items.len() as u16).to_le_bytes());
        let mut pos_item = 64 * KIB;
        for (i, (id, banderas, contenido)) in items.iter().enumerate() {
            let e = OFF_META + 32 + i as u64 * 32;
            poner(&mut img, e, id);
            poner(&mut img, e + 16, &(pos_item as u32).to_le_bytes());
            poner(&mut img, e + 20, &(contenido.len() as u32).to_le_bytes());
            poner(&mut img, e + 24, &banderas.to_le_bytes());
            poner(&mut img, OFF_META + pos_item, contenido);
            pos_item += contenido.len().div_ceil(8) as u64 * 8;
        }

        for (i, e) in bat.iter().enumerate() {
            poner(&mut img, OFF_BAT + i as u64 * 8, &e.to_le_bytes());
        }
        for (off, b) in datos {
            for k in 0..tb {
                let vo = b * tb + k;
                if vo >= m.tam_virtual {
                    break;
                }
                img[(off + k) as usize] = patron(vo);
            }
        }
        img
    }

    fn esperado(m: &Modelo) -> Vec<u8> {
        let tb = u64::from(m.tam_bloque);
        (0..m.tam_virtual)
            .map(|vo| {
                let b = vo / tb;
                if m.estados
                    .iter()
                    .any(|&(x, e)| x == b && e == PAYLOAD_FULLY_PRESENT)
                {
                    patron(vo)
                } else {
                    0
                }
            })
            .collect()
    }

    fn fichero(img: &[u8]) -> tempfile::NamedTempFile {
        let mut t = tempfile::NamedTempFile::new().unwrap();
        t.write_all(img).unwrap();
        t.flush().unwrap();
        t
    }

    fn abrir(img: &[u8]) -> Result<VhdxImage> {
        let t = fichero(img);
        VhdxImage::open(File::open(t.path()).unwrap())
    }

    fn es_corrupto(r: Result<VhdxImage>) -> bool {
        matches!(r, Err(IoError::Corrupt { format: "VHDX", .. }))
    }

    fn es_no_soportado(r: Result<VhdxImage>) -> bool {
        matches!(r, Err(IoError::UnsupportedFeature { format: "VHDX", .. }))
    }

    /// Desplazamiento en el fichero de la entrada de metadatos `i` y de su contenido.
    fn item_meta(img: &[u8], i: u64) -> u64 {
        let e = (OFF_META + 32 + i * 32) as usize;
        OFF_META + u64::from(le32(img, e + 16).unwrap())
    }

    #[test]
    fn lee_vhdx_minimo() {
        let m = Modelo::basico();
        let img = construir(&m);
        let t = fichero(&img);
        let v = VhdxImage::open(File::open(t.path()).unwrap()).unwrap();
        assert_eq!(v.size(), m.tam_virtual);
        assert_eq!(v.sector_size(), 512);
        assert_eq!(v.block_size(), MIB as u32);
        assert_eq!(v.physical_sector_size(), 4096);
        assert!(!v.is_fixed());
        assert_eq!(
            v.describe(),
            "VHDX dinámico, bloques de 1 MiB, sector lógico 512 B, sector físico 4096 B"
        );
        let esp = esperado(&m);
        let mut todo = vec![0u8; m.tam_virtual as usize];
        v.read_at(0, &mut todo).unwrap();
        assert!(todo == esp);
        // Lecturas que cruzan bloques y el final exacto.
        for (off, len) in [
            (MIB - 100, 300usize),
            (3 * MIB - 1, 2),
            (m.tam_virtual - 7, 7),
            (12345, 1),
        ] {
            let mut b = vec![0xEEu8; len];
            v.read_at(off, &mut b).unwrap();
            assert_eq!(&b[..], &esp[off as usize..off as usize + len]);
        }
        assert!(matches!(
            v.read_at(m.tam_virtual - 1, &mut [0u8; 2]),
            Err(IoError::OutOfBounds { .. })
        ));
        assert!(matches!(
            v.read_at(u64::MAX, &mut [0u8; 2]),
            Err(IoError::OutOfBounds { .. })
        ));
        v.read_at(m.tam_virtual, &mut []).unwrap();

        // Asignación: bloque 0 datos, 1–2 dispersos, 3 datos (parcial).
        let spb = MIB / 512;
        assert_eq!(
            v.allocation(0, v.sector_count()).unwrap(),
            vec![
                (0, spb, Allocation::Data),
                (spb, 2 * spb, Allocation::Sparse),
                (3 * spb, spb / 2, Allocation::Data),
            ]
        );
        assert_eq!(
            v.allocation(spb - 1, 3).unwrap(),
            vec![(spb - 1, 1, Allocation::Data), (spb, 2, Allocation::Sparse)]
        );
        let total = v.sector_count();
        assert!(matches!(
            v.allocation(0, 0),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            v.allocation(total - 1, 2),
            Err(IoError::InvalidRange { .. })
        ));
        assert!(matches!(
            v.allocation(u64::MAX, 2),
            Err(IoError::InvalidRange { .. })
        ));
        drop(v);
        assert!(
            std::fs::read(t.path()).unwrap() == img,
            "el fichero no debe cambiar"
        );
    }

    #[test]
    fn fijo_con_sector_de_4096() {
        let m = Modelo {
            sector_logico: 4096,
            tam_bloque: 2 * MIB as u32,
            tam_virtual: 4 * MIB,
            banderas: PARAM_DEJAR_BLOQUES_ASIGNADOS,
            estados: vec![(0, PAYLOAD_FULLY_PRESENT), (1, PAYLOAD_FULLY_PRESENT)],
            padre: None,
        };
        let v = abrir(&construir(&m)).unwrap();
        assert_eq!(v.sector_size(), 4096);
        assert_eq!(v.sector_count(), 1024);
        assert!(v.is_fixed());
        assert!(v
            .describe()
            .starts_with("VHDX fijo, bloques de 2 MiB, sector lógico 4096 B"));
        let mut b = vec![0u8; 4 * MIB as usize];
        v.read_at(0, &mut b).unwrap();
        assert!(b == esperado(&m));
        assert_eq!(
            v.allocation(0, 1024).unwrap(),
            vec![(0, 1024, Allocation::Data)]
        );
    }

    #[test]
    fn entradas_de_mapa_de_bits_intercaladas() {
        // Con bloques de 1 MiB y sector de 512 B el chunk ratio es 4096: el bloque 4096 está en
        // el índice 4097 de la BAT (el 4096 es la entrada de mapa de bits).
        let m = Modelo {
            tam_bloque: MIB as u32,
            sector_logico: 512,
            tam_virtual: 4098 * MIB,
            banderas: 0,
            estados: vec![
                (1, PAYLOAD_FULLY_PRESENT),
                (4095, PAYLOAD_UNMAPPED),
                (4096, PAYLOAD_FULLY_PRESENT),
                (4097, PAYLOAD_UNDEFINED),
            ],
            padre: None,
        };
        let mut img = construir(&m);
        let v = abrir(&img).unwrap();
        let mut b = vec![0u8; 4096];
        v.read_at(4096 * MIB + 1000, &mut b).unwrap();
        assert!(b
            .iter()
            .enumerate()
            .all(|(i, &x)| x == patron(4096 * MIB + 1000 + i as u64)));
        v.read_at(4095 * MIB, &mut b).unwrap();
        assert!(b.iter().all(|&x| x == 0));
        let spb = MIB / 512;
        assert_eq!(
            v.allocation(4095 * spb, 3 * spb).unwrap(),
            vec![
                (4095 * spb, spb, Allocation::Sparse),
                (4096 * spb, spb, Allocation::Data),
                (4097 * spb, spb, Allocation::Sparse),
            ]
        );
        // Entrada de mapa de bits con estado no válido.
        poner(&mut img, OFF_BAT + 4096 * 8, &2u64.to_le_bytes());
        assert!(es_corrupto(abrir(&img)));
        poner(&mut img, OFF_BAT + 4096 * 8, &BITMAP_PRESENT.to_le_bytes());
        assert!(abrir(&img).is_ok());
    }

    #[test]
    fn usa_la_cabecera_valida_de_mayor_secuencia() {
        let mut img = construir(&Modelo::basico());
        // La cabecera menos reciente (secuencia 1) tiene registro pendiente: se ignora.
        poner(&mut img, OFF_CABECERAS[0] + 48, &[0x44; 16]);
        sellar_cabeceras(&mut img);
        assert!(abrir(&img).is_ok());
        // La más reciente (secuencia 2) con registro pendiente y el registro vacío: no hay nada
        // válido que reproducir.
        let mut img = construir(&Modelo::basico());
        poner(&mut img, OFF_CABECERAS[1] + 48, &[0x44; 16]);
        sellar_cabeceras(&mut img);
        let r = abrir(&img);
        assert!(
            matches!(&r, Err(IoError::Corrupt { detail, .. }) if detail.contains("registro pendiente")),
            "{r:?}"
        );
        // Si su CRC es malo se usa la otra.
        img[OFF_CABECERAS[1] as usize + 200] ^= 1;
        assert!(abrir(&img).is_ok());
        // Ambas malas.
        img[OFF_CABECERAS[0] as usize + 200] ^= 1;
        assert!(es_corrupto(abrir(&img)));
    }

    #[test]
    fn cabecera_de_menor_secuencia_si_la_otra_falla() {
        let mut img = construir(&Modelo::basico());
        img[OFF_CABECERAS[0] as usize + 4] ^= 0xFF; // CRC de la primera
        assert!(abrir(&img).is_ok());
        let mut img = construir(&Modelo::basico());
        poner(&mut img, OFF_CABECERAS[1], b"HEAD");
        assert!(abrir(&img).is_ok());
    }

    #[test]
    fn identificador_y_tamano_minimo() {
        let mut img = construir(&Modelo::basico());
        img[0] = b'X';
        assert!(es_corrupto(abrir(&img)));
        assert!(es_corrupto(abrir(&[0u8; 1000])));
        let img = construir(&Modelo::basico());
        assert!(es_corrupto(abrir(&img[..300 * 1024])));
    }

    #[test]
    fn version_de_cabecera() {
        let mut img = construir(&Modelo::basico());
        for off in OFF_CABECERAS {
            poner(&mut img, off + 66, &2u16.to_le_bytes());
        }
        sellar_cabeceras(&mut img);
        assert!(es_no_soportado(abrir(&img)));
    }

    #[test]
    fn tablas_de_regiones() {
        // La primera tabla dañada: se usa la segunda.
        let mut img = construir(&Modelo::basico());
        img[OFF_TABLAS_REGIONES[0] as usize + 20] ^= 1;
        assert!(abrir(&img).is_ok());
        img[OFF_TABLAS_REGIONES[1] as usize + 20] ^= 1;
        assert!(es_corrupto(abrir(&img)));

        // Región requerida desconocida.
        let mut img = construir(&Modelo::basico());
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off + 8, &3u32.to_le_bytes());
            poner(&mut img, off + 80, &[0x5A; 16]);
            poner(&mut img, off + 96, &(64 * MIB).to_le_bytes());
            poner(&mut img, off + 104, &(MIB as u32).to_le_bytes());
            poner(&mut img, off + 108, &1u32.to_le_bytes());
        }
        sellar_regiones(&mut img);
        assert!(es_no_soportado(abrir(&img)));
        // Si no es requerida se ignora, pero debe estar bien situada.
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off + 108, &0u32.to_le_bytes());
        }
        sellar_regiones(&mut img);
        assert!(es_corrupto(abrir(&img)));

        // Demasiadas entradas.
        let mut img = construir(&Modelo::basico());
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off + 8, &2048u32.to_le_bytes());
        }
        sellar_regiones(&mut img);
        assert!(es_corrupto(abrir(&img)));

        // BAT fuera del fichero, desalineada y solapada con los metadatos.
        for (offset, long) in [(1 << 40, MIB), (3 * MIB + 4096, MIB), (2 * MIB, MIB)] {
            let mut img = construir(&Modelo::basico());
            for off in OFF_TABLAS_REGIONES {
                poner(&mut img, off + 32, &offset.to_le_bytes());
                poner(&mut img, off + 40, &(long as u32).to_le_bytes());
            }
            sellar_regiones(&mut img);
            assert!(es_corrupto(abrir(&img)), "BAT en {offset}");
        }

        // Falta la región de metadatos.
        let mut img = construir(&Modelo::basico());
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off + 8, &1u32.to_le_bytes());
        }
        sellar_regiones(&mut img);
        assert!(es_corrupto(abrir(&img)));
    }

    #[test]
    fn registro_mal_situado() {
        let mut img = construir(&Modelo::basico());
        for off in OFF_CABECERAS {
            poner(&mut img, off + 72, &(MIB + 512).to_le_bytes());
        }
        sellar_cabeceras(&mut img);
        assert!(es_corrupto(abrir(&img)));
        // Registro solapado con los metadatos.
        for off in OFF_CABECERAS {
            poner(&mut img, off + 72, &OFF_META.to_le_bytes());
        }
        sellar_cabeceras(&mut img);
        assert!(es_corrupto(abrir(&img)));
    }

    #[test]
    fn metadatos_no_validos() {
        let base = construir(&Modelo::basico());
        let off_params = item_meta(&base, 0);
        let off_sl = item_meta(&base, 2);
        let off_sf = item_meta(&base, 3);
        let off_tam = item_meta(&base, 1);
        for tb in [0u32, 512 * 1024, 3 * MIB as u32, 512 * MIB as u32] {
            let mut img = base.clone();
            poner(&mut img, off_params, &tb.to_le_bytes());
            assert!(es_corrupto(abrir(&img)), "bloque {tb}");
        }
        for sl in [0u32, 1024, 2048, 8192] {
            let mut img = base.clone();
            poner(&mut img, off_sl, &sl.to_le_bytes());
            assert!(es_corrupto(abrir(&img)), "sector lógico {sl}");
        }
        let mut img = base.clone();
        poner(&mut img, off_sf, &1000u32.to_le_bytes());
        assert!(es_corrupto(abrir(&img)));
        for t in [0u64, 1000, 64 * TIB + 512] {
            let mut img = base.clone();
            poner(&mut img, off_tam, &t.to_le_bytes());
            assert!(es_corrupto(abrir(&img)), "tamaño {t}");
        }
        // Firma de la tabla.
        let mut img = base.clone();
        img[OFF_META as usize] = b'X';
        assert!(es_corrupto(abrir(&img)));
        // Metadato requerido desconocido (sustituye el identificador del disco).
        let mut img = base.clone();
        poner(&mut img, OFF_META + 32 + 4 * 32, &[0x77; 16]);
        assert!(es_no_soportado(abrir(&img)));
        // El mismo, opcional: se ignora.
        poner(&mut img, OFF_META + 32 + 4 * 32 + 24, &0u32.to_le_bytes());
        assert!(abrir(&img).is_ok());
        // Falta el tamaño virtual (su entrada pasa a ser un metadato opcional desconocido).
        let mut img = base.clone();
        poner(&mut img, OFF_META + 32 + 32, &[0x78; 16]);
        poner(&mut img, OFF_META + 32 + 32 + 24, &0u32.to_le_bytes());
        assert!(es_corrupto(abrir(&img)));
        // Metadato duplicado.
        let mut img = base.clone();
        let params = GUID_PARAMETROS_FICHERO;
        poner(&mut img, OFF_META + 32 + 4 * 32, &params);
        assert!(es_corrupto(abrir(&img)));
        // Metadato fuera de la región.
        let mut img = base.clone();
        poner(
            &mut img,
            OFF_META + 32 + 16,
            &(MIB as u32 - 4).to_le_bytes(),
        );
        assert!(es_corrupto(abrir(&img)));
        // Demasiadas entradas.
        let mut img = base;
        poner(&mut img, OFF_META + 10, &2048u16.to_le_bytes());
        assert!(es_corrupto(abrir(&img)));
    }

    #[test]
    fn disco_diferencial_es_referencia_externa() {
        let mut m = Modelo::basico();
        m.padre = Some("..\\padre.vhdx".to_string());
        let img = construir(&m);
        match abrir(&img) {
            Err(IoError::ExternalReference { format, reference }) => {
                assert_eq!(format, "VHDX");
                assert_eq!(reference, "..\\padre.vhdx");
            }
            r => panic!("se esperaba ExternalReference: {r:?}"),
        }
        // Sin localizador.
        let mut img = construir(&Modelo::basico());
        let off = item_meta(&img, 0);
        poner(&mut img, off + 4, &PARAM_TIENE_PADRE.to_le_bytes());
        assert!(matches!(
            abrir(&img),
            Err(IoError::ExternalReference { format: "VHDX", .. })
        ));
    }

    #[test]
    fn estados_de_bat_no_validos() {
        for estado in [4u64, 5, PAYLOAD_PARTIALLY_PRESENT] {
            let mut img = construir(&Modelo::basico());
            poner(&mut img, OFF_BAT + 8, &estado.to_le_bytes());
            assert!(es_corrupto(abrir(&img)), "estado {estado}");
        }
        // Bloque presente fuera del fichero, dentro de la zona de cabeceras y sobre la BAT.
        for off_mb in [1u64 << 30, 0, 3, 1 << 43] {
            let mut img = construir(&Modelo::basico());
            let e = PAYLOAD_FULLY_PRESENT | (off_mb << 20);
            poner(&mut img, OFF_BAT + 8, &e.to_le_bytes());
            assert!(es_corrupto(abrir(&img)), "bloque en {off_mb} MiB");
        }
        // Último bloque truncado: solo se exige la parte virtual (0,5 MiB).
        let m = Modelo::basico();
        let img = construir(&m);
        let corto = &img[..img.len() - (MIB as usize / 2)];
        assert!(abrir(corto).is_ok());
        assert!(es_corrupto(abrir(
            &img[..img.len() - (MIB as usize / 2) - 1]
        )));
    }

    #[test]
    fn tamanos_enormes_sin_reservar_memoria() {
        // 64 TiB con bloques de 1 MiB: la BAT ocuparía ~512 MiB, por encima del límite.
        let base = construir(&Modelo::basico());
        let off_tam = item_meta(&base, 1);
        let mut img = base.clone();
        poner(&mut img, off_tam, &(64 * TIB).to_le_bytes());
        assert!(es_no_soportado(abrir(&img)));
        // 1 TiB: la BAT (8 MiB) no cabe en la región de 1 MiB.
        let mut img = base.clone();
        poner(&mut img, off_tam, &TIB.to_le_bytes());
        assert!(es_corrupto(abrir(&img)));
        // Región BAT declarada de 4 GiB − 1 MiB en un fichero pequeño.
        let mut img = base;
        for off in OFF_TABLAS_REGIONES {
            poner(
                &mut img,
                off + 40,
                &(u32::MAX - (MIB as u32 - 1)).to_le_bytes(),
            );
        }
        sellar_regiones(&mut img);
        assert!(es_corrupto(abrir(&img)));
    }

    // ---- modo tolerante (ADR-020) ----

    fn abrir_modo(img: &[u8], tolerante: bool) -> (tempfile::NamedTempFile, Result<VhdxImage>) {
        let t = fichero(img);
        let r = VhdxImage::open_with(File::open(t.path()).unwrap(), tolerante);
        (t, r)
    }

    fn entrada_bat(img: &[u8], b: u64) -> u64 {
        le64(img, (OFF_BAT + 8 * b) as usize).unwrap()
    }

    #[test]
    fn modo_tolerante_descarta_solo_bloques_invalidos_o_ambiguos() {
        let m = Modelo::basico();
        let base = construir(&m);
        let esp = esperado(&m);
        let presente = |mib: u64| PAYLOAD_FULLY_PRESENT | (mib << 20);
        let e0 = entrada_bat(&base, 0);
        let spb = MIB / 512;
        // (caso, entradas alteradas, tramos de sectores dañados). Bloques de 2048 sectores; el
        // bloque 3 solo tiene 1024 en el disco virtual.
        type Caso = (&'static str, Vec<(u64, u64)>, Vec<(u64, u64)>);
        let casos: Vec<Caso> = vec![
            (
                "fuera del fichero",
                vec![(3, presente(1 << 30))],
                vec![(3 * spb, spb / 2)],
            ),
            (
                "desplazamiento desbordado",
                vec![(3, PAYLOAD_FULLY_PRESENT | !0xF_FFFF)],
                vec![(3 * spb, spb / 2)],
            ),
            (
                "en la zona de cabeceras",
                vec![(0, presente(0))],
                vec![(0, spb)],
            ),
            ("sobre el registro", vec![(0, presente(1))], vec![(0, spb)]),
            (
                "sobre los metadatos",
                vec![(3, presente(2))],
                vec![(3 * spb, spb / 2)],
            ),
            ("sobre la BAT", vec![(0, presente(3))], vec![(0, spb)]),
            (
                "duplicada",
                vec![(3, e0)],
                vec![(0, spb), (3 * spb, spb / 2)],
            ),
            ("estado 4", vec![(1, 4)], vec![(spb, spb)]),
            ("estado 5", vec![(2, 5)], vec![(2 * spb, spb)]),
            (
                "PARTIALLY_PRESENT",
                vec![(1, PAYLOAD_PARTIALLY_PRESENT)],
                vec![(spb, spb)],
            ),
            (
                "varias a la vez",
                vec![(1, 5), (3, presente(2))],
                vec![(spb, spb), (3 * spb, spb / 2)],
            ),
        ];
        for (caso, cambios, malos) in casos {
            let mut img = base.clone();
            for &(b, e) in &cambios {
                poner(&mut img, OFF_BAT + 8 * b, &e.to_le_bytes());
            }
            assert!(es_corrupto(abrir_modo(&img, false).1), "{caso}");
            let (t, v) = abrir_modo(&img, true);
            let antes = crate::tolerant::check::Snapshot::of(t.path());
            let v = v.unwrap_or_else(|e| panic!("{caso}: {e}"));
            let n = malos.len() as u64;
            assert_eq!(v.discarded_blocks(), n, "{caso}");
            assert!(v.is_tolerant());
            let d = v.describe();
            assert!(
                d.ends_with(&crate::tolerant::describe_suffix(n, "bloque", "bloques")),
                "{caso}: {d}"
            );
            crate::tolerant::check::tolerant_read_matches(&v, &esp, &malos, caso);
            drop(v);
            assert_eq!(
                crate::tolerant::check::Snapshot::of(t.path()),
                antes,
                "{caso}"
            );
        }
    }

    #[test]
    fn modo_tolerante_sin_danos_y_mapas_de_bits_ignorados() {
        let m = Modelo::basico();
        let img = construir(&m);
        let (_t, v) = abrir_modo(&img, true);
        let v = v.unwrap();
        assert_eq!(v.discarded_blocks(), 0);
        assert!(v
            .describe()
            .ends_with("; modo tolerante: 0 bloques descartados (no fiables)"));
        crate::tolerant::check::tolerant_read_matches(&v, &esperado(&m), &[], "sin daños");
        let (_t, estricto) = abrir_modo(&img, false);
        let estricto = estricto.unwrap();
        assert!(!estricto.is_tolerant());
        assert!(!estricto.describe().contains("tolerante"));

        // Entrada de mapa de bits con estado no válido: no afecta a los datos de un disco sin
        // padre; en modo tolerante se cuenta y se ignora.
        let m = Modelo {
            tam_bloque: MIB as u32,
            sector_logico: 512,
            tam_virtual: 4098 * MIB,
            banderas: 0,
            estados: vec![(4096, PAYLOAD_FULLY_PRESENT)],
            padre: None,
        };
        let mut img = construir(&m);
        poner(&mut img, OFF_BAT + 4096 * 8, &3u64.to_le_bytes());
        assert!(es_corrupto(abrir_modo(&img, false).1));
        let (_t, v) = abrir_modo(&img, true);
        let v = v.unwrap();
        assert_eq!(v.discarded_blocks(), 0);
        assert!(v
            .describe()
            .ends_with(", 1 entradas de mapa de bits no válidas ignoradas"));
        let mut b = vec![0u8; 4096];
        v.read_at(4096 * MIB + 77, &mut b).unwrap();
        assert!(b
            .iter()
            .enumerate()
            .all(|(i, &x)| x == patron(4096 * MIB + 77 + i as u64)));
    }

    #[test]
    fn estructuras_criticas_danadas_en_ambos_modos() {
        let base = construir(&Modelo::basico());
        let mut casos: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut img = base.clone();
        img[0] = b'X';
        casos.push(("identificador", img));
        let mut img = base.clone();
        img[OFF_CABECERAS[0] as usize + 200] ^= 1;
        img[OFF_CABECERAS[1] as usize + 200] ^= 1;
        casos.push(("ambas cabeceras", img));
        let mut img = base.clone();
        img[OFF_TABLAS_REGIONES[0] as usize + 20] ^= 1;
        img[OFF_TABLAS_REGIONES[1] as usize + 20] ^= 1;
        casos.push(("ambas tablas de regiones", img));
        let mut img = base.clone();
        img[OFF_META as usize] = b'X';
        casos.push(("firma de los metadatos", img));
        let mut img = base.clone();
        poner(
            &mut img,
            item_meta(&base, 0),
            &(3 * MIB as u32).to_le_bytes(),
        );
        casos.push(("tamaño de bloque", img));
        let mut img = base.clone();
        for off in OFF_TABLAS_REGIONES {
            poner(&mut img, off + 32, &(2 * MIB).to_le_bytes());
        }
        sellar_regiones(&mut img);
        casos.push(("BAT sobre los metadatos", img));
        for (caso, img) in casos {
            for tolerante in [false, true] {
                let (t, r) = abrir_modo(&img, tolerante);
                assert!(es_corrupto(r), "{caso} (tolerante: {tolerante})");
                assert!(std::fs::read(t.path()).unwrap() == img);
            }
        }
    }

    mod registro;

    #[test]
    fn guid_en_orden_de_disco() {
        assert_eq!(
            formatear_guid(&GUID_REGION_BAT),
            "2DC27766-F623-4200-9D64-115E9BFD4A08"
        );
        assert_eq!(GUID_REGION_BAT[..4], [0x66, 0x77, 0xC2, 0x2D]);
    }
}
