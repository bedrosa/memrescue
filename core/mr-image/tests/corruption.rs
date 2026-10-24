//! Pruebas de corrupción de contenedores con ficheros sintéticos construidos aquí según las
//! especificaciones públicas (independientes de los generadores de las pruebas unitarias):
//!
//! - Inversión aleatoria **determinista** (semilla fija) de 1 a 4 bytes en VHDX, VMDK e imágenes
//!   divididas, dirigida sobre todo a las estructuras (en VHDX, a veces con las sumas CRC-32C
//!   recalculadas para que la alteración llegue más allá de la comprobación de integridad).
//! - Truncado en cada frontera de estructura de VHDX (cabeceras, tablas de regiones, registro,
//!   metadatos, BAT, bloques) y de VMDK (cabecera, descriptor, GD, GT, granos), a ±1 byte.
//! - Fichero acortado después de abrir (VHDX, VMDK y dividida).
//! - VHDX con registro pendiente (reproducido en memoria): inversión aleatoria dirigida también a
//!   las entradas del registro (a veces con sus CRC-32C recalculadas) y fichero acortado después
//!   de abrir.
//!
//! Garantías comprobadas: abrir devuelve `Ok` o un error tipado (nunca pánico ni bloqueo, con un
//! vigilante de tiempo); con la imagen abierta, cada lectura devuelve datos o error; el modo
//! estricto nunca abre lo que no puede leer entero; el modo tolerante solo entrega como datos lo
//! que se lee de verdad (el lector resiliente marca `Unreadable` exactamente lo que falla); lo
//! truncado o acortado nunca se entrega con contenido distinto del original; el fichero de origen
//! queda intacto.

use mr_image::crc32c::crc32c;
use mr_image::split::SplitImage;
use mr_image::vhdx::VhdxImage;
use mr_image::vmdk::VmdkImage;
use mr_io::resilient::{read_range, OnError, ReadPolicy};
use mr_io::{Allocation, BlockDevice, IoError, SectorState};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
/// Bytes virtuales que se leen y contrastan en cada imagen abierta.
const SAMPLE_BYTES: u64 = 3 * MIB;
/// Límite de tiempo de cada prueba: superarlo se trata como un bloqueo.
const WATCHDOG: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------------------------
// Utilidades
// ---------------------------------------------------------------------------------------------

/// Generador determinista (xorshift64*).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Contenido virtual determinista de un byte del disco (distinto de cero casi siempre).
fn pattern(vo: u64) -> u8 {
    let x = vo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (vo >> 9);
    ((x >> 56) as u8 ^ (vo as u8)) | 1
}

/// Ejecuta `f` en otro hilo; si no termina antes de [`WATCHDOG`], la prueba falla por bloqueo
/// (indicando el último contexto anotado). Un pánico del hilo se propaga.
fn with_watchdog(f: impl FnOnce(&dyn Fn(String)) + Send + 'static) {
    let ctx = std::sync::Arc::new(std::sync::Mutex::new(String::from("inicio")));
    let ctx_w = std::sync::Arc::clone(&ctx);
    let (tx, rx) = mpsc::channel::<()>();
    let h = std::thread::spawn(move || {
        let note = move |s: String| *ctx_w.lock().unwrap_or_else(|p| p.into_inner()) = s;
        f(&note);
        let _ = tx.send(());
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(()) => h.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(p) = h.join() {
                std::panic::resume_unwind(p);
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "posible bloqueo: más de {WATCHDOG:?} en «{}»",
            ctx.lock().unwrap_or_else(|p| p.into_inner())
        ),
    }
}

/// Imagen abierta con su número de bloques descartados (0 en modo estricto).
trait Image: BlockDevice {
    fn discarded(&self) -> u64;
}

impl Image for VhdxImage {
    fn discarded(&self) -> u64 {
        self.discarded_blocks()
    }
}

impl Image for VmdkImage {
    fn discarded(&self) -> u64 {
        self.discarded_blocks()
    }
}

type Opener = fn(File, bool) -> mr_io::Result<Box<dyn Image>>;

fn open_vhdx(f: File, tolerant: bool) -> mr_io::Result<Box<dyn Image>> {
    Ok(Box::new(VhdxImage::open_with(f, tolerant)?))
}

fn open_vmdk(f: File, tolerant: bool) -> mr_io::Result<Box<dyn Image>> {
    Ok(Box::new(VmdkImage::open_with(f, tolerant)?))
}

/// Abre `path` en solo lectura con `open`, convirtiendo un pánico en un fallo con contexto y
/// comprobando que el error, si lo hay, es de un tipo esperable al abrir un contenedor.
fn open_checked(open: Opener, path: &Path, tolerant: bool, ctx: &str) -> Option<Box<dyn Image>> {
    let file = File::open(path).unwrap();
    let r = catch_unwind(AssertUnwindSafe(|| open(file, tolerant)))
        .unwrap_or_else(|_| panic!("pánico al abrir ({ctx}, tolerante = {tolerant})"));
    match r {
        Ok(img) => Some(img),
        Err(
            IoError::Corrupt { .. }
            | IoError::UnsupportedFeature { .. }
            | IoError::ExternalReference { .. }
            | IoError::Read { .. }
            | IoError::Io(_),
        ) => None,
        Err(e) => panic!("error inesperado al abrir ({ctx}, tolerante = {tolerant}): {e:?}"),
    }
}

/// Lee los primeros [`SAMPLE_BYTES`] del disco y comprueba su coherencia:
/// - `allocation` cubre el rango (y el disco entero) en tramos ordenados y contiguos;
/// - lo `Sparse` se lee sin error y son ceros;
/// - el lector resiliente (sin reintentos) entrega `Read` con los mismos bytes donde la lectura
///   directa funciona y `Unreadable` exactamente donde falla.
///
/// Devuelve, por sector de la muestra, sus bytes o `None` si no se puede leer.
fn exercise(dev: &dyn Image, ctx: &str) -> Vec<Option<Vec<u8>>> {
    catch_unwind(AssertUnwindSafe(|| exercise_inner(dev, ctx)))
        .unwrap_or_else(|_| panic!("pánico al leer ({ctx})"))
}

fn check_runs(runs: &[(u64, u64, Allocation)], lba: u64, count: u64, ctx: &str) {
    let mut at = lba;
    for &(a, c, _) in runs {
        assert!(
            a == at && c > 0,
            "tramos de asignación incoherentes ({ctx})"
        );
        at += c;
    }
    assert_eq!(at, lba + count, "la asignación no cubre el rango ({ctx})");
}

fn exercise_inner(dev: &dyn Image, ctx: &str) -> Vec<Option<Vec<u8>>> {
    let ss = u64::from(dev.sector_size());
    assert!(ss.is_power_of_two() && (512..=65536).contains(&ss));
    let total = dev.sector_count();
    if total == 0 {
        return Vec::new();
    }
    let whole = dev
        .allocation(0, total)
        .unwrap_or_else(|e| panic!("allocation del disco entero falló ({ctx}): {e}"));
    check_runs(&whole, 0, total, ctx);
    let n = total.min(SAMPLE_BYTES / ss);
    let runs = dev.allocation(0, n).unwrap();
    check_runs(&runs, 0, n, ctx);
    let mut sparse = vec![false; n as usize];
    for &(a, c, k) in &runs {
        if k == Allocation::Sparse {
            sparse[a as usize..(a + c) as usize].fill(true);
        }
    }

    // Lectura directa: por bloques de 64 sectores y, si un bloque falla, sector a sector.
    let mut truth: Vec<Option<Vec<u8>>> = Vec::with_capacity(n as usize);
    let block = 64u64;
    let mut lba = 0;
    while lba < n {
        let c = block.min(n - lba);
        let mut buf = vec![0u8; (c * ss) as usize];
        if dev.read_sectors(lba, c, &mut buf).is_ok() {
            truth.extend(buf.chunks(ss as usize).map(|s| Some(s.to_vec())));
        } else {
            for s in lba..lba + c {
                let mut one = vec![0u8; ss as usize];
                truth.push(dev.read_sectors(s, 1, &mut one).ok().map(|()| one));
            }
        }
        lba += c;
    }
    for (i, t) in truth.iter().enumerate() {
        if sparse[i] {
            let t = t
                .as_ref()
                .unwrap_or_else(|| panic!("sector disperso {i} ilegible ({ctx})"));
            assert!(
                t.iter().all(|&b| b == 0),
                "sector disperso {i} no nulo ({ctx})"
            );
        }
    }

    // Último sector y lectura fuera de límites: nunca pánico.
    let mut one = vec![0u8; ss as usize];
    let _ = dev.read_sectors(total - 1, 1, &mut one);
    assert!(dev.read_sectors(total, 1, &mut one).is_err());

    // Lector resiliente: el estado de cada sector coincide con la lectura directa.
    let policy = ReadPolicy {
        chunk_sectors: 48,
        retries: 0,
        on_error: OnError::FillAndContinue,
        jump_sectors: 0,
        ..ReadPolicy::default()
    };
    let mut got: Vec<(Vec<u8>, SectorState)> = Vec::with_capacity(n as usize);
    let rep = read_range(
        dev,
        0,
        n,
        &policy,
        |_, data, states| {
            for (s, st) in data.chunks(ss as usize).zip(states) {
                got.push((s.to_vec(), *st));
            }
            Ok(())
        },
        |_, _| true,
    )
    .unwrap_or_else(|e| panic!("el lector resiliente falló ({ctx}): {e}"));
    assert_eq!(got.len() as u64, n);
    assert_eq!(rep.map.total(), n);
    for (i, ((data, st), t)) in got.iter().zip(&truth).enumerate() {
        match (st, t) {
            (SectorState::Read, Some(t)) => assert_eq!(data, t, "sector {i} ({ctx})"),
            (SectorState::Sparse, Some(_)) => assert!(sparse[i], "sector {i} ({ctx})"),
            (SectorState::Unreadable, None) => {}
            other => panic!(
                "sector {i}: estado incoherente con la lectura directa ({ctx}): {:?}",
                (other.0, other.1.is_some())
            ),
        }
    }
    truth
}

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn assert_file_is(path: &Path, bytes: &[u8], ctx: &str) {
    let now = std::fs::read(path).unwrap();
    assert!(now == bytes, "el fichero de origen ha cambiado ({ctx})");
}

/// Sustituye en `path` los tramos `ranges` por los bytes de `img` (mismo tamaño que el fichero).
fn patch(path: &Path, img: &[u8], ranges: &[(usize, usize)]) {
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    for &(o, l) in ranges {
        f.seek(SeekFrom::Start(o as u64)).unwrap();
        f.write_all(&img[o..o + l]).unwrap();
    }
    f.flush().unwrap();
}

/// Contenedor sintético: bytes del fichero, contenido virtual esperado, fronteras de estructura,
/// zonas de estructura a las que se dirigen las alteraciones y ubicación de cada tramo de datos
/// `(desplazamiento virtual, desplazamiento en fichero, longitud)`.
struct Synthetic {
    file: Vec<u8>,
    virt: Vec<u8>,
    boundaries: Vec<u64>,
    targets: Vec<(u64, u64)>,
    data_map: Vec<(u64, u64, u64)>,
}

impl Synthetic {
    fn expected_sector(&self, lba: u64, ss: u64) -> &[u8] {
        &self.virt[(lba * ss) as usize..((lba + 1) * ss) as usize]
    }
}

// ---------------------------------------------------------------------------------------------
// VHDX
// ---------------------------------------------------------------------------------------------

type Guid = [u8; 16];

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const VHDX_BAT: Guid = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const VHDX_META: Guid = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const VHDX_FILE_PARAMS: Guid = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const VHDX_VIRTUAL_SIZE: Guid = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const VHDX_DISK_ID: Guid = guid(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const VHDX_LOGICAL_SECTOR: Guid = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const VHDX_PHYSICAL_SECTOR: Guid = guid(
    0xCDA3_48C7,
    0x445D,
    0x4471,
    [0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56],
);

const VHDX_HEADERS: [u64; 2] = [64 * KIB, 128 * KIB];
const VHDX_HEADER_LEN: u64 = 4096;
const VHDX_REGIONS: [u64; 2] = [192 * KIB, 256 * KIB];
const VHDX_REGION_LEN: u64 = 64 * KIB;
const VHDX_LOG: u64 = MIB;
const VHDX_META_OFF: u64 = 2 * MIB;
const VHDX_BAT_OFF: u64 = 3 * MIB;
const VHDX_BLOCK: u64 = MIB;
/// 2,5 MiB virtuales: bloque 0 presente, 1 UNMAPPED, 2 presente (solo su mitad útil en el fichero).
const VHDX_VIRTUAL: u64 = 2 * MIB + 512 * KIB;

fn put(img: &mut [u8], off: u64, data: &[u8]) {
    img[off as usize..off as usize + data.len()].copy_from_slice(data);
}

/// Recalcula las CRC-32C de las dos cabeceras y las dos tablas de regiones.
fn vhdx_seal(img: &mut [u8]) {
    for (off, len) in VHDX_HEADERS
        .iter()
        .map(|&o| (o, VHDX_HEADER_LEN))
        .chain(VHDX_REGIONS.iter().map(|&o| (o, VHDX_REGION_LEN)))
    {
        let (o, l) = (off as usize, len as usize);
        img[o + 4..o + 8].fill(0);
        let c = crc32c(&img[o..o + l]);
        img[o + 4..o + 8].copy_from_slice(&c.to_le_bytes());
    }
}

fn vhdx_crc_fields() -> Vec<(usize, usize)> {
    VHDX_HEADERS
        .iter()
        .chain(&VHDX_REGIONS)
        .map(|&o| (o as usize + 4, 4))
        .collect()
}

fn build_vhdx() -> Synthetic {
    let blocks = VHDX_VIRTUAL.div_ceil(VHDX_BLOCK); // 3; chunk ratio 4096: sin entradas de mapa
    let data0 = VHDX_BAT_OFF + MIB;
    let data2 = data0 + VHDX_BLOCK;
    let tail = VHDX_VIRTUAL - 2 * VHDX_BLOCK;
    let mut img = vec![0u8; (data2 + tail) as usize];
    put(&mut img, 0, b"vhdxfile");
    for (i, off) in VHDX_HEADERS.into_iter().enumerate() {
        put(&mut img, off, b"head");
        put(&mut img, off + 8, &(i as u64 + 1).to_le_bytes());
        put(&mut img, off + 16, &[0x11; 16]);
        put(&mut img, off + 32, &[0x22; 16]);
        put(&mut img, off + 66, &1u16.to_le_bytes());
        put(&mut img, off + 68, &(MIB as u32).to_le_bytes());
        put(&mut img, off + 72, &VHDX_LOG.to_le_bytes());
    }
    for off in VHDX_REGIONS {
        put(&mut img, off, b"regi");
        put(&mut img, off + 8, &2u32.to_le_bytes());
        for (k, (id, at)) in [(VHDX_BAT, VHDX_BAT_OFF), (VHDX_META, VHDX_META_OFF)]
            .into_iter()
            .enumerate()
        {
            let e = off + 16 + 32 * k as u64;
            put(&mut img, e, &id);
            put(&mut img, e + 16, &at.to_le_bytes());
            put(&mut img, e + 24, &(MIB as u32).to_le_bytes());
            put(&mut img, e + 28, &1u32.to_le_bytes());
        }
    }
    // Metadatos: tabla en 2 MiB, elementos desde 2 MiB + 64 KiB.
    let required = 1u32 << 2;
    let items: [(Guid, u32, Vec<u8>); 5] = [
        (VHDX_FILE_PARAMS, required, {
            let mut v = (VHDX_BLOCK as u32).to_le_bytes().to_vec();
            v.extend_from_slice(&0u32.to_le_bytes());
            v
        }),
        (
            VHDX_VIRTUAL_SIZE,
            required | 2,
            VHDX_VIRTUAL.to_le_bytes().to_vec(),
        ),
        (
            VHDX_LOGICAL_SECTOR,
            required | 2,
            512u32.to_le_bytes().to_vec(),
        ),
        (
            VHDX_PHYSICAL_SECTOR,
            required | 2,
            4096u32.to_le_bytes().to_vec(),
        ),
        (VHDX_DISK_ID, required | 2, vec![0x33; 16]),
    ];
    put(&mut img, VHDX_META_OFF, b"metadata");
    put(
        &mut img,
        VHDX_META_OFF + 10,
        &(items.len() as u16).to_le_bytes(),
    );
    let mut boundaries = vec![VHDX_META_OFF + 32 + 32 * items.len() as u64];
    let mut item_at = 64 * KIB;
    for (i, (id, flags, content)) in items.iter().enumerate() {
        let e = VHDX_META_OFF + 32 + 32 * i as u64;
        put(&mut img, e, id);
        put(&mut img, e + 16, &(item_at as u32).to_le_bytes());
        put(&mut img, e + 20, &(content.len() as u32).to_le_bytes());
        put(&mut img, e + 24, &flags.to_le_bytes());
        put(&mut img, VHDX_META_OFF + item_at, content);
        boundaries.push(VHDX_META_OFF + item_at);
        boundaries.push(VHDX_META_OFF + item_at + content.len() as u64);
        item_at += content.len().div_ceil(8) as u64 * 8;
    }
    let items_end = VHDX_META_OFF + item_at;
    // BAT: estados 6 (FULLY_PRESENT, con el desplazamiento en MiB en los bits 20+) y 3 (UNMAPPED).
    let bat = [6 | ((data0 / MIB) << 20), 3, 6 | ((data2 / MIB) << 20)];
    for (i, e) in bat.iter().enumerate() {
        put(&mut img, VHDX_BAT_OFF + 8 * i as u64, &e.to_le_bytes());
    }
    let mut virt = vec![0u8; VHDX_VIRTUAL as usize];
    let mut data_map = Vec::new();
    for (b, at) in [(0u64, data0), (2, data2)] {
        let len = VHDX_BLOCK.min(VHDX_VIRTUAL - b * VHDX_BLOCK);
        for k in 0..len {
            let vo = b * VHDX_BLOCK + k;
            virt[vo as usize] = pattern(vo);
            img[(at + k) as usize] = pattern(vo);
        }
        data_map.push((b * VHDX_BLOCK, at, len));
    }
    vhdx_seal(&mut img);
    let full = img.len() as u64;
    boundaries.extend([
        8,
        VHDX_HEADERS[0],
        VHDX_HEADERS[0] + VHDX_HEADER_LEN,
        VHDX_HEADERS[1],
        VHDX_HEADERS[1] + VHDX_HEADER_LEN,
        VHDX_REGIONS[0],
        VHDX_REGIONS[1],
        VHDX_REGIONS[1] + VHDX_REGION_LEN,
        VHDX_LOG,
        VHDX_META_OFF,
        VHDX_META_OFF + 64 * KIB,
        VHDX_BAT_OFF,
        VHDX_BAT_OFF + 8 * blocks,
        VHDX_BAT_OFF + MIB,
        data0 + VHDX_BLOCK / 2,
        data2,
        data2 + tail / 2,
        full,
    ]);
    let targets = vec![
        (0, 8),
        (VHDX_HEADERS[0], 80),
        (VHDX_HEADERS[1], 80),
        (VHDX_REGIONS[0], 80),
        (VHDX_REGIONS[1], 80),
        (VHDX_META_OFF, 32 + 32 * items.len() as u64),
        (
            VHDX_META_OFF + 64 * KIB,
            items_end - VHDX_META_OFF - 64 * KIB,
        ),
        (VHDX_BAT_OFF, 8 * blocks),
    ];
    Synthetic {
        file: img,
        virt,
        boundaries,
        targets,
        data_map,
    }
}

// ---------------------------------------------------------------------------------------------
// VHDX con registro pendiente
// ---------------------------------------------------------------------------------------------

const VHDX_LOG_GUID: Guid = [0xD4; 16];
/// Entradas del registro `(posición en el registro, longitud)`.
const VHDX_LOG_ENTRIES: [(u64, u64); 2] = [(0, 8192), (8192, 8192)];
/// Tamaño del fichero con registro (FlushedFileOffset y LastFileOffset).
const VHDX_LOG_FILE: u64 = 6 * MIB;

enum LogDesc {
    Data(u64, Vec<u8>),
    Zero(u64, u64),
}

/// Entrada del registro según [MS-VHDX]: cabecera `loge`, descriptores `desc`/`zero`, sectores
/// `data` y CRC-32C de toda la entrada.
fn vhdx_log_entry(seq: u64, tail: u32, descs: &[LogDesc]) -> Vec<u8> {
    let desc_sectors = (64 + 32 * descs.len()).div_ceil(4096);
    let data = descs
        .iter()
        .filter(|d| matches!(d, LogDesc::Data(..)))
        .count();
    let len = (desc_sectors + data) * 4096;
    let mut e = vec![0u8; len];
    put(&mut e, 0, b"loge");
    put(&mut e, 8, &(len as u32).to_le_bytes());
    put(&mut e, 12, &tail.to_le_bytes());
    put(&mut e, 16, &seq.to_le_bytes());
    put(&mut e, 24, &(descs.len() as u32).to_le_bytes());
    put(&mut e, 32, &VHDX_LOG_GUID);
    put(&mut e, 48, &VHDX_LOG_FILE.to_le_bytes());
    put(&mut e, 56, &VHDX_LOG_FILE.to_le_bytes());
    let mut k = desc_sectors as u64;
    for (i, d) in descs.iter().enumerate() {
        let o = 64 + 32 * i as u64;
        match d {
            LogDesc::Data(at, sector) => {
                put(&mut e, o, b"desc");
                put(&mut e, o + 4, &sector[4092..]);
                put(&mut e, o + 8, &sector[..8]);
                put(&mut e, o + 16, &at.to_le_bytes());
                let p = k * 4096;
                put(&mut e, p, b"data");
                put(&mut e, p + 4, &((seq >> 32) as u32).to_le_bytes());
                put(&mut e, p + 8, &sector[8..4092]);
                put(&mut e, p + 4092, &(seq as u32).to_le_bytes());
                k += 1;
            }
            LogDesc::Zero(at, n) => {
                put(&mut e, o, b"zero");
                put(&mut e, o + 8, &n.to_le_bytes());
                put(&mut e, o + 16, &at.to_le_bytes());
            }
        }
        put(&mut e, o + 24, &seq.to_le_bytes());
    }
    let c = crc32c(&e);
    put(&mut e, 4, &c.to_le_bytes());
    e
}

/// Recalcula las CRC-32C de cabeceras, tablas de regiones y entradas del registro.
fn vhdx_log_seal(img: &mut [u8]) {
    vhdx_seal(img);
    for (p, l) in VHDX_LOG_ENTRIES {
        let o = (VHDX_LOG + p) as usize;
        img[o + 4..o + 8].fill(0);
        let c = crc32c(&img[o..o + l as usize]);
        img[o + 4..o + 8].copy_from_slice(&c.to_le_bytes());
    }
}

fn vhdx_log_crc_fields() -> Vec<(usize, usize)> {
    let mut v = vhdx_crc_fields();
    v.extend(
        VHDX_LOG_ENTRIES
            .iter()
            .map(|&(p, _)| ((VHDX_LOG + p) as usize + 4, 4)),
    );
    v
}

/// El VHDX de [`build_vhdx`] con un registro pendiente de dos entradas: la BAT del fichero está
/// desfasada (el bloque 2 figura como NOT_PRESENT) y la primera entrada escribe la correcta; la
/// segunda escribe un sector del bloque 0 y pone otro a ceros. El contenido virtual esperado es
/// el del fichero tras reproducir el registro.
fn build_vhdx_with_log() -> Synthetic {
    let mut s = build_vhdx();
    let bat_sector = s.file[VHDX_BAT_OFF as usize..(VHDX_BAT_OFF + 4096) as usize].to_vec();
    put(&mut s.file, VHDX_BAT_OFF + 16, &0u64.to_le_bytes());
    s.file.resize(VHDX_LOG_FILE as usize, 0);
    let data0 = s.data_map[0].1;
    let mark: Vec<u8> = (0..4096u32).map(|i| (i * 7 + 3) as u8).collect();
    let e1 = vhdx_log_entry(0x1_0000_0020, 0, &[LogDesc::Data(VHDX_BAT_OFF, bat_sector)]);
    let e2 = vhdx_log_entry(
        0x1_0000_0021,
        0,
        &[
            LogDesc::Data(data0 + 8192, mark.clone()),
            LogDesc::Zero(data0 + 16384, 4096),
        ],
    );
    for (e, (p, l)) in [e1, e2].iter().zip(VHDX_LOG_ENTRIES) {
        assert_eq!(e.len() as u64, l);
        put(&mut s.file, VHDX_LOG + p, e);
    }
    for off in VHDX_HEADERS {
        put(&mut s.file, off + 48, &VHDX_LOG_GUID);
    }
    vhdx_seal(&mut s.file);
    s.virt[8192..12288].copy_from_slice(&mark);
    s.virt[16384..20480].fill(0);
    s.targets.push((VHDX_LOG, 16384));
    s.boundaries
        .extend([VHDX_LOG + 8192, VHDX_LOG + 16384, VHDX_LOG_FILE]);
    s
}

// ---------------------------------------------------------------------------------------------
// VMDK (monolithicSparse)
// ---------------------------------------------------------------------------------------------

const VMDK_GRAIN: u64 = 16; // sectores (8 KiB)
const VMDK_CAPACITY: u64 = 10_240; // sectores (5 MiB): 640 granos, 2 GT
/// Granos con datos, en el orden en que se colocan en el fichero (no monótono).
const VMDK_DATA_GRAINS: [u64; 9] = [513, 0, 639, 1, 100, 2, 511, 512, 5];
/// Granos marcados «a cero» (entrada 1 con el indicador zeroed-grain).
const VMDK_ZERO_GRAINS: [u64; 2] = [3, 600];

fn build_vmdk() -> Synthetic {
    let desc_sectors = 4u64;
    let gd_sector = 1 + desc_sectors;
    let grains = VMDK_CAPACITY / VMDK_GRAIN;
    let gd_entries = grains.div_ceil(512);
    let gt_first = gd_sector + 1;
    let data_first = gt_first + 4 * gd_entries;
    let grain_bytes = VMDK_GRAIN * 512;
    let mut f = vec![0u8; (data_first * 512) as usize];
    put(&mut f, 0, b"KDMV");
    put(&mut f, 4, &1u32.to_le_bytes());
    put(&mut f, 8, &(1u32 | 1 << 2).to_le_bytes()); // prueba de fin de línea + zeroed-grain
    put(&mut f, 12, &VMDK_CAPACITY.to_le_bytes());
    put(&mut f, 20, &VMDK_GRAIN.to_le_bytes());
    put(&mut f, 28, &1u64.to_le_bytes());
    put(&mut f, 36, &desc_sectors.to_le_bytes());
    put(&mut f, 44, &512u32.to_le_bytes());
    put(&mut f, 56, &gd_sector.to_le_bytes());
    put(&mut f, 64, &data_first.to_le_bytes());
    put(&mut f, 73, b"\n \r\n");
    let desc = format!(
        "# Disk DescriptorFile\nversion=1\nCID=12345678\nparentCID=ffffffff\n\
         createType=\"monolithicSparse\"\n\n# Extent description\n\
         RW {VMDK_CAPACITY} SPARSE \"corrupcion.vmdk\"\n\n# The Disk Data Base\n#DDB\n\
         ddb.virtualHWVersion = \"4\"\n"
    );
    put(&mut f, 512, desc.as_bytes());
    for i in 0..gd_entries {
        put(
            &mut f,
            gd_sector * 512 + 4 * i,
            &((gt_first + 4 * i) as u32).to_le_bytes(),
        );
    }
    let gte_at = |g: u64| gt_first * 512 + 4 * g; // las GT son contiguas
    let mut virt = vec![0u8; (VMDK_CAPACITY * 512) as usize];
    let mut data_map = Vec::new();
    let mut boundaries = Vec::new();
    for g in VMDK_ZERO_GRAINS {
        put(&mut f, gte_at(g), &1u32.to_le_bytes());
    }
    for g in VMDK_DATA_GRAINS {
        let at = f.len() as u64;
        put(&mut f, gte_at(g), &((at / 512) as u32).to_le_bytes());
        let vo0 = g * grain_bytes;
        f.extend((0..grain_bytes).map(|k| pattern(vo0 + k)));
        for k in 0..grain_bytes {
            virt[(vo0 + k) as usize] = pattern(vo0 + k);
        }
        data_map.push((vo0, at, grain_bytes));
        boundaries.extend([at, at + grain_bytes / 2]);
    }
    let full = f.len() as u64;
    boundaries.extend([
        4,
        80,
        512,
        512 + desc.len() as u64,
        gd_sector * 512,
        gd_sector * 512 + 4 * gd_entries,
        gt_first * 512,
        (gt_first + 4) * 512,
        data_first * 512,
        full,
    ]);
    let mut targets = vec![
        (0, 80),
        (512, desc.len() as u64),
        (gd_sector * 512, 4 * gd_entries),
    ];
    for g in VMDK_DATA_GRAINS.iter().chain(&VMDK_ZERO_GRAINS) {
        targets.push((gte_at(*g), 4));
    }
    Synthetic {
        file: f,
        virt,
        boundaries,
        targets,
        data_map,
    }
}

// ---------------------------------------------------------------------------------------------
// Pruebas genéricas de contenedor
// ---------------------------------------------------------------------------------------------

/// Elige 1 a 4 bytes que alterar: casi siempre dentro de las zonas de estructura (a veces
/// contiguos, para cambiar campos de varios bytes), a veces en cualquier parte del fichero.
fn pick_flips(rng: &mut Rng, s: &Synthetic) -> Vec<(usize, u8)> {
    let n = 1 + rng.below(4) as usize;
    let len = s.file.len() as u64;
    let contiguous = rng.below(2) == 0;
    let mut start: Option<u64> = None;
    (0..n)
        .map(|i| {
            let pos = match (contiguous, start) {
                (true, Some(p)) => (p + i as u64).min(len - 1),
                _ => {
                    let p = if rng.below(8) == 0 {
                        rng.below(len)
                    } else {
                        let (o, l) = s.targets[rng.below(s.targets.len() as u64) as usize];
                        o + rng.below(l)
                    };
                    start = Some(p);
                    p
                }
            };
            // XOR con un valor no nulo: el byte cambia siempre.
            let x = match rng.below(4) {
                0 => 0xFF,
                1 => 1 << rng.below(8),
                _ => (rng.below(255) + 1) as u8,
            };
            (pos as usize, x)
        })
        .collect()
}

/// Abre en ambos modos y comprueba las garantías comunes. Devuelve si cada modo abrió.
fn check_both_modes(open: Opener, path: &Path, ctx: &str) -> (bool, bool) {
    let strict = open_checked(open, path, false, ctx);
    let strict_truth = strict.as_ref().map(|d| {
        assert_eq!(d.discarded(), 0, "el modo estricto no descarta ({ctx})");
        let t = exercise(d.as_ref(), ctx);
        // Lo que el modo estricto acepta se lee entero.
        assert!(
            t.iter().all(Option::is_some),
            "el modo estricto abrió una imagen con sectores ilegibles ({ctx})"
        );
        t
    });
    let tolerant = open_checked(open, path, true, ctx);
    if let Some(d) = &tolerant {
        let t = exercise(d.as_ref(), ctx);
        if d.discarded() == 0 {
            assert!(
                t.iter().all(Option::is_some),
                "sin descartes, todo es legible ({ctx})"
            );
        }
        if let (Some(s), Some(st)) = (&strict, &strict_truth) {
            assert_eq!(
                d.discarded(),
                0,
                "estricto abre, tolerante no descarta ({ctx})"
            );
            assert_eq!((d.size(), d.sector_size()), (s.size(), s.sector_size()));
            assert!(&t == st, "estricto y tolerante leen distinto ({ctx})");
        }
    }
    (strict.is_some(), tolerant.is_some())
}

/// Recalcula las sumas de control (`.0`) y lista los campos que cambia (`.1`).
type Reseal = (fn(&mut [u8]), fn() -> Vec<(usize, usize)>);

fn random_flips(
    name: &'static str,
    build: fn() -> Synthetic,
    open: Opener,
    seed: u64,
    iters: u32,
    reseal: Option<Reseal>,
) {
    with_watchdog(move |note| {
        let s = build();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        write_file(&path, &s.file);
        // La imagen intacta abre en los dos modos y se lee como se construyó.
        assert_eq!(check_both_modes(open, &path, "intacta"), (true, true));
        let d = open_checked(open, &path, false, "intacta").unwrap();
        assert_eq!(d.size(), s.virt.len() as u64);
        let t = exercise(d.as_ref(), "intacta");
        for (i, x) in t.iter().enumerate() {
            assert_eq!(x.as_deref(), Some(s.expected_sector(i as u64, 512)));
        }
        drop(d);

        let mut rng = Rng::new(seed);
        let mut img = s.file.clone();
        let (mut opened_strict, mut opened_tolerant) = (0u32, 0u32);
        for it in 0..iters {
            let flips = pick_flips(&mut rng, &s);
            let mut ranges: Vec<(usize, usize)> = flips.iter().map(|&(p, _)| (p, 1)).collect();
            for &(p, x) in &flips {
                img[p] ^= x;
            }
            if let Some((seal, fields)) = reseal.filter(|_| rng.below(2) == 0) {
                seal(&mut img);
                ranges.extend(fields());
            }
            let ctx = format!("{name}, semilla {seed:#x}, iteración {it}, alteraciones {flips:?}");
            note(ctx.clone());
            patch(&path, &img, &ranges);
            let (a, b) = check_both_modes(open, &path, &ctx);
            opened_strict += u32::from(a);
            opened_tolerant += u32::from(b);
            assert_file_is(&path, &img, &ctx);
            // Se restaura el original para la siguiente iteración.
            for &(o, l) in &ranges {
                img[o..o + l].copy_from_slice(&s.file[o..o + l]);
            }
            patch(&path, &img, &ranges);
        }
        eprintln!(
            "{name}: {iters} iteraciones; abren {opened_strict} en modo estricto y {opened_tolerant} en tolerante"
        );
        // Las alteraciones deben ejercitar los dos caminos (abrir y rechazar).
        assert!(
            opened_strict > 0 && opened_strict < iters,
            "{name}: {opened_strict}/{iters}"
        );
        assert!(opened_tolerant >= opened_strict);
    });
}

fn truncation_at_boundaries(name: &'static str, build: fn() -> Synthetic, open: Opener) {
    with_watchdog(move |note| {
        let s = build();
        let full = s.file.len() as u64;
        let mut cuts: Vec<u64> = s
            .boundaries
            .iter()
            .flat_map(|&b| [b.saturating_sub(1), b, b + 1])
            .chain([0, 1])
            .filter(|&c| c < full)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let (n_cuts, mut tolerant_opens) = (cuts.len(), 0);
        for cut in cuts {
            let ctx = format!("{name} truncado a {cut} de {full} bytes");
            note(ctx.clone());
            let bytes = &s.file[..cut as usize];
            write_file(&path, bytes);
            assert!(
                open_checked(open, &path, false, &ctx).is_none(),
                "el modo estricto abrió un fichero truncado ({ctx})"
            );
            if let Some(d) = open_checked(open, &path, true, &ctx) {
                assert!(d.discarded() > 0, "truncado sin descartes ({ctx})");
                tolerant_opens += 1;
                assert_eq!(d.size(), s.virt.len() as u64);
                // Lo que se lee es el contenido original; lo que falta, error.
                for (i, t) in exercise(d.as_ref(), &ctx).iter().enumerate() {
                    if let Some(t) = t {
                        assert_eq!(t.as_slice(), s.expected_sector(i as u64, 512), "{ctx}");
                    }
                }
            }
            assert_file_is(&path, bytes, &ctx);
        }
        eprintln!(
            "{name}: {n_cuts} truncados; el modo tolerante abre {tolerant_opens} (con descartes)"
        );
        // Truncar dentro de los datos se tolera descartando bloques.
        assert!(tolerant_opens > 0);
    });
}

/// Acorta el fichero con la imagen ya abierta (estricta). Cada sector se lee como el original
/// salvo los que caen fuera del nuevo final, que dan error; los dispersos siguen siendo ceros.
fn shrink_after_open_checks(
    path: &Path,
    s: &Synthetic,
    dev: &dyn BlockDevice,
    cut: u64,
    ctx: &str,
) {
    let ss = 512u64;
    for lba in 0..dev.sector_count() {
        let vo = lba * ss;
        let beyond = s
            .data_map
            .iter()
            .find(|&&(v, _, l)| vo >= v && vo < v + l)
            .is_some_and(|&(v, f, _)| f + (vo - v) + ss > cut);
        let mut buf = vec![0u8; ss as usize];
        let r = catch_unwind(AssertUnwindSafe(|| dev.read_sectors(lba, 1, &mut buf)))
            .unwrap_or_else(|_| panic!("pánico al leer el sector {lba} ({ctx})"));
        if beyond {
            assert!(
                r.is_err(),
                "sector {lba} fuera del fichero leído sin error ({ctx})"
            );
        } else {
            r.unwrap_or_else(|e| panic!("sector {lba} ({ctx}): {e}"));
            assert_eq!(
                buf.as_slice(),
                s.expected_sector(lba, ss),
                "sector {lba} ({ctx})"
            );
        }
    }
    assert_eq!(std::fs::metadata(path).unwrap().len(), cut);
}

fn shrink_after_open(name: &'static str, build: fn() -> Synthetic, open: Opener) {
    with_watchdog(move |note| {
        let s = build();
        let full = s.file.len() as u64;
        let first_data_at = s.data_map.iter().map(|m| m.1).min().unwrap();
        let mut cuts: Vec<u64> = s
            .data_map
            .iter()
            .flat_map(|&(_, f, l)| [f, f + l / 2, f + l - 1])
            .chain([0, 600, first_data_at / 2, full - 1])
            .filter(|&c| c < full)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        for cut in cuts {
            for tolerant in [false, true] {
                let ctx = format!(
                    "{name} acortado a {cut} de {full} bytes tras abrir (tolerante = {tolerant})"
                );
                note(ctx.clone());
                write_file(&path, &s.file);
                let d = open_checked(open, &path, tolerant, &ctx).expect("la imagen intacta abre");
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(cut)
                    .unwrap();
                shrink_after_open_checks(&path, &s, d.as_ref(), cut, &ctx);
                // El lector resiliente marca lo que falta como ilegible, nunca como datos.
                let _ = exercise(d.as_ref(), &ctx);
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Pruebas
// ---------------------------------------------------------------------------------------------

#[test]
fn vhdx_random_byte_flips() {
    random_flips(
        "f.vhdx",
        build_vhdx,
        open_vhdx,
        0x5648_4458_0001,
        300,
        Some((vhdx_seal, vhdx_crc_fields)),
    );
}

#[test]
fn vhdx_with_pending_log_random_byte_flips() {
    random_flips(
        "l.vhdx",
        build_vhdx_with_log,
        open_vhdx,
        0x5648_4458_4C4F_4701,
        300,
        Some((vhdx_log_seal, vhdx_log_crc_fields)),
    );
}

#[test]
fn vhdx_with_pending_log_shrunk_after_open() {
    shrink_after_open("ls.vhdx", build_vhdx_with_log, open_vhdx);
}

#[test]
fn vmdk_random_byte_flips() {
    random_flips("f.vmdk", build_vmdk, open_vmdk, 0x564D_444B_0001, 400, None);
}

#[test]
fn vhdx_truncated_at_every_structure_boundary() {
    truncation_at_boundaries("t.vhdx", build_vhdx, open_vhdx);
}

#[test]
fn vmdk_truncated_at_every_structure_boundary() {
    truncation_at_boundaries("t.vmdk", build_vmdk, open_vmdk);
}

#[test]
fn vhdx_shrunk_after_open() {
    shrink_after_open("s.vhdx", build_vhdx, open_vhdx);
}

#[test]
fn vmdk_shrunk_after_open() {
    shrink_after_open("s.vmdk", build_vmdk, open_vmdk);
}

/// Segmentos de una imagen dividida (el último más corto y no alineado al sector).
fn write_segments(dir: &Path, data: &[u8]) -> Vec<PathBuf> {
    let sizes = [64 * KIB as usize, 64 * KIB as usize];
    let mut paths = Vec::new();
    let mut pos = 0;
    for i in 0..3 {
        let end = sizes.get(i).map_or(data.len(), |&s| pos + s);
        let p = dir.join(format!("d.{:03}", i + 1));
        std::fs::write(&p, &data[pos..end]).unwrap();
        paths.push(p);
        pos = end;
    }
    paths
}

fn split_data() -> Vec<u8> {
    (0..(128 * KIB + 10 * KIB + 100)).map(pattern).collect()
}

#[test]
fn split_random_byte_flips() {
    with_watchdog(|note| {
        let mut data = split_data();
        let dir = tempfile::tempdir().unwrap();
        let paths = write_segments(dir.path(), &data);
        let mut rng = Rng::new(0x5350_4C54_0001);
        for it in 0..200 {
            let n = 1 + rng.below(4);
            for _ in 0..n {
                let p = rng.below(data.len() as u64) as usize;
                data[p] ^= (rng.below(255) + 1) as u8;
            }
            let ctx = format!("dividida, iteración {it}");
            note(ctx.clone());
            // Se reescriben los segmentos con el contenido alterado acumulado.
            let paths2 = write_segments(dir.path(), &data);
            assert_eq!(paths2, paths);
            let img = catch_unwind(AssertUnwindSafe(|| SplitImage::open(&paths, 512)))
                .unwrap_or_else(|_| panic!("pánico al abrir ({ctx})"))
                .unwrap();
            // Sin estructura propia: se lee exactamente lo que hay (los sectores completos).
            let whole = (data.len() as u64 / 512 * 512) as usize;
            let mut buf = vec![0u8; whole];
            img.read_sectors(0, whole as u64 / 512, &mut buf).unwrap();
            assert!(buf == data[..whole], "{ctx}");
            let mut tail = vec![0u8; data.len() - whole];
            img.read_at(whole as u64, &mut tail).unwrap();
            assert_eq!(tail, &data[whole..]);
            drop(img);
            let now: Vec<u8> = paths
                .iter()
                .flat_map(|p| std::fs::read(p).unwrap())
                .collect();
            assert!(now == data, "segmentos modificados ({ctx})");
        }
    });
}

#[test]
fn split_shrunk_after_open() {
    with_watchdog(|note| {
        let data = split_data();
        for (seg, cut) in [
            (0usize, 0u64),
            (0, 1000),
            (1, 32 * KIB + 7),
            (2, 512),
            (2, 0),
        ] {
            let ctx = format!("dividida, segmento {seg} acortado a {cut} bytes tras abrir");
            note(ctx.clone());
            let dir = tempfile::tempdir().unwrap();
            let paths = write_segments(dir.path(), &data);
            let img = SplitImage::open(&paths, 512).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&paths[seg])
                .unwrap()
                .set_len(cut)
                .unwrap();
            let seg_start = seg as u64 * 64 * KIB;
            for lba in 0..img.sector_count() {
                let (a, b) = (lba * 512, lba * 512 + 512);
                // Falla si alguno de sus bytes del segmento acortado queda fuera del nuevo final.
                let seg_end = if seg < 2 {
                    seg_start + 64 * KIB
                } else {
                    data.len() as u64
                };
                let lost = a < seg_end && b > seg_start + cut;
                let mut buf = [0u8; 512];
                let r = img.read_sectors(lba, 1, &mut buf);
                if lost {
                    assert!(r.is_err(), "sector {lba} ({ctx})");
                } else {
                    r.unwrap_or_else(|e| panic!("sector {lba} ({ctx}): {e}"));
                    assert_eq!(&buf[..], &data[a as usize..b as usize], "{ctx}");
                }
            }
        }
    });
}
