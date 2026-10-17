//! Memoria máxima al abrir contenedores con tablas de asignación al límite y entradas duplicadas
//! (VHD, VHDX y VMDK): el pico medido durante la apertura no supera el presupuesto documentado en
//! `docs/DECISIONES.md` (ADR-020) y un exceso nunca aborta el proceso.
//!
//! La medida usa un asignador global que cuenta los bytes vivos y su máximo. Está en las pruebas
//! de `mr-ffi` porque es el único crate donde el `unsafe` está permitido (`GlobalAlloc` es un
//! rasgo `unsafe`); el resto del espacio de trabajo lo prohíbe con `forbid`. Las imágenes son
//! sintéticas (unos 260 MiB cada una) y se generan en un directorio temporal que se borra al
//! terminar; las pruebas se ejecutan una tras otra para que la medida de una no incluya a otra.

use mr_image::vhd::VhdImage;
use mr_image::vhdx::VhdxImage;
use mr_image::vmdk::VmdkImage;
use mr_io::{BlockDevice, IoError};
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------------------------------------
// Asignador que cuenta
// ---------------------------------------------------------------------------------------------

/// Delega en el asignador del sistema y lleva la cuenta de los bytes vivos y de su máximo.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let now = LIVE.fetch_add(n, Ordering::SeqCst) + n;
    PEAK.fetch_max(now, Ordering::SeqCst);
}

fn shrink(n: usize) {
    LIVE.fetch_sub(n, Ordering::SeqCst);
}

// SAFETY: solo se añade contabilidad; cada operación delega en `System` con los mismos
// argumentos, así que se cumple el contrato de `GlobalAlloc` que cumple `System`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: mismo contrato que el llamador garantiza para `alloc`.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: mismo contrato que el llamador garantiza para `alloc_zeroed`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` y `layout` vienen de una asignación de este mismo asignador.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Durante la copia conviven el bloque viejo y el nuevo: se cuenta el nuevo antes de
        // descontar el viejo para no subestimar el pico.
        grow(new_size);
        // SAFETY: mismo contrato que el llamador garantiza para `realloc`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            shrink(new_size);
        } else {
            shrink(layout.size());
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Las medidas no pueden solaparse: el contador es global.
static SERIAL: Mutex<()> = Mutex::new(());

/// Ejecuta `f` y devuelve su resultado y el pico de bytes vivos por encima de los que había al
/// empezar.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let r = f();
    let peak = PEAK.load(Ordering::SeqCst) - base;
    (r, peak)
}

const MIB: u64 = 1024 * 1024;
const MAX_TABLE: u64 = mr_image::MAX_TABLE_BYTES;

/// Comprueba el pico frente al presupuesto de la apertura para `table_bytes` bytes de tablas.
fn assert_within_budget(what: &str, peak: usize, table_bytes: u64) {
    let budget = mr_image::open_memory_budget(table_bytes);
    // Margen fijo para lo que no es tabla: búfer de lectura por trozos, descriptor, mensajes…
    let slack = 4 * MIB;
    eprintln!(
        "{what}: pico {:.1} MiB (tablas {:.1} MiB, presupuesto {:.1} MiB)",
        peak as f64 / MIB as f64,
        table_bytes as f64 / MIB as f64,
        budget as f64 / MIB as f64
    );
    assert!(
        peak as u64 <= budget + slack,
        "{what}: pico de {peak} bytes por encima del presupuesto {budget} + {slack}"
    );
}

fn is_corrupt(r: &Result<impl Sized, IoError>) -> bool {
    matches!(r, Err(IoError::Corrupt { .. }))
}

/// Escribe `count` veces la entrada `entry` (ya codificada) con un búfer de 1 MiB.
fn write_repeated(w: &mut impl Write, entry: &[u8], count: u64) {
    let per_chunk = (MIB as usize / entry.len()) as u64;
    let chunk: Vec<u8> = entry.repeat(per_chunk as usize);
    let mut left = count;
    while left > 0 {
        let n = left.min(per_chunk);
        w.write_all(&chunk[..n as usize * entry.len()]).unwrap();
        left -= n;
    }
}

// ---------------------------------------------------------------------------------------------
// VHD dinámico: BAT de 256 MiB (bloques de 512 B), todas las entradas al mismo bloque
// ---------------------------------------------------------------------------------------------

fn vhd_checksum(b: &[u8], field: usize) -> u32 {
    let sum = b
        .iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |acc, (_, &x)| acc.wrapping_add(u32::from(x)));
    !sum
}

/// Devuelve los bytes de tablas leídos al abrir (la BAT).
fn write_vhd(path: &Path) -> u64 {
    let entries = MAX_TABLE / 4;
    let block = 512u64;
    let size = entries * block;
    let bat_off = 1536u64;
    let block_sector = (bat_off + entries * 4) / 512;

    let mut footer = [0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&512u64.to_be_bytes());
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[60..64].copy_from_slice(&3u32.to_be_bytes());
    let c = vhd_checksum(&footer, 64);
    footer[64..68].copy_from_slice(&c.to_be_bytes());

    let mut header = [0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    header[16..24].copy_from_slice(&bat_off.to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&(entries as u32).to_be_bytes());
    header[32..36].copy_from_slice(&(block as u32).to_be_bytes());
    let c = vhd_checksum(&header, 36);
    header[36..40].copy_from_slice(&c.to_be_bytes());

    let mut w = BufWriter::with_capacity(MIB as usize, File::create(path).unwrap());
    w.write_all(&footer).unwrap();
    w.write_all(&header).unwrap();
    write_repeated(&mut w, &(block_sector as u32).to_be_bytes(), entries);
    // Mapa de bits (512 B) y datos (512 B) del único bloque.
    w.write_all(&[0xFF; 512]).unwrap();
    w.write_all(&[0xAB; 512]).unwrap();
    w.write_all(&footer).unwrap();
    w.into_inner().unwrap().sync_all().unwrap();
    entries * 4
}

// ---------------------------------------------------------------------------------------------
// VHDX: BAT de 256 MiB (bloques de 1 MiB), todas las entradas de bloque al mismo bloque
// ---------------------------------------------------------------------------------------------

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const GUID_BAT: [u8; 16] = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const GUID_METADATA: [u8; 16] = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const GUID_FILE_PARAMS: [u8; 16] = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const GUID_VIRTUAL_SIZE: [u8; 16] = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const GUID_DISK_ID: [u8; 16] = guid(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const GUID_LOGICAL_SECTOR: [u8; 16] = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const GUID_PHYSICAL_SECTOR: [u8; 16] = guid(
    0xCDA3_48C7,
    0x445D,
    0x4471,
    [0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56],
);

fn put(buf: &mut [u8], off: usize, data: &[u8]) {
    buf[off..off + data.len()].copy_from_slice(data);
}

fn seal_crc32c(buf: &mut [u8]) {
    buf[4..8].fill(0);
    let c = mr_image::crc32c::crc32c(buf);
    buf[4..8].copy_from_slice(&c.to_le_bytes());
}

/// Devuelve los bytes de tablas leídos al abrir (la BAT) y el número de bloques.
fn write_vhdx(path: &Path) -> (u64, u64) {
    // Sector lógico de 512 B y bloques de 1 MiB: una entrada de mapa de bits cada 4096 de bloque.
    let ratio = 4096u64;
    let max_entries = MAX_TABLE / 8;
    // Mayor número de bloques cuya BAT (bloques + mapas de bits) cabe en `MAX_TABLE`.
    let mut blocks = max_entries * ratio / (ratio + 1);
    while blocks + 1 + blocks / ratio <= max_entries {
        blocks += 1;
    }
    while blocks + (blocks - 1) / ratio > max_entries {
        blocks -= 1;
    }
    let entries = blocks + (blocks - 1) / ratio;
    let (log_off, meta_off, bat_off) = (MIB, 2 * MIB, 3 * MIB);
    let bat_len = MAX_TABLE;
    let data_off = bat_off + bat_len;

    let f = File::create(path).unwrap();
    let mut w = BufWriter::with_capacity(MIB as usize, f);
    // Identificador, cabeceras y tablas de regiones (primer MiB).
    let mut head = vec![0u8; MIB as usize];
    put(&mut head, 0, b"vhdxfile");
    for (i, off) in [64 * 1024usize, 128 * 1024].into_iter().enumerate() {
        let mut h = vec![0u8; 4096];
        put(&mut h, 0, b"head");
        put(&mut h, 8, &(i as u64 + 1).to_le_bytes());
        put(&mut h, 16, &[0x11; 16]);
        put(&mut h, 32, &[0x22; 16]);
        put(&mut h, 66, &1u16.to_le_bytes());
        put(&mut h, 68, &(MIB as u32).to_le_bytes());
        put(&mut h, 72, &log_off.to_le_bytes());
        seal_crc32c(&mut h);
        put(&mut head, off, &h);
    }
    for off in [192 * 1024usize, 256 * 1024] {
        let mut r = vec![0u8; 64 * 1024];
        put(&mut r, 0, b"regi");
        put(&mut r, 8, &2u32.to_le_bytes());
        put(&mut r, 16, &GUID_BAT);
        put(&mut r, 32, &bat_off.to_le_bytes());
        put(&mut r, 40, &(bat_len as u32).to_le_bytes());
        put(&mut r, 44, &1u32.to_le_bytes());
        put(&mut r, 48, &GUID_METADATA);
        put(&mut r, 64, &meta_off.to_le_bytes());
        put(&mut r, 72, &(MIB as u32).to_le_bytes());
        put(&mut r, 76, &1u32.to_le_bytes());
        seal_crc32c(&mut r);
        put(&mut head, off, &r);
    }
    w.write_all(&head).unwrap();
    // Registro vacío (1 MiB).
    w.write_all(&vec![0u8; MIB as usize]).unwrap();
    // Metadatos.
    let mut meta = vec![0u8; MIB as usize];
    put(&mut meta, 0, b"metadata");
    let mut params = (MIB as u32).to_le_bytes().to_vec();
    params.extend_from_slice(&0u32.to_le_bytes());
    let items: [([u8; 16], u32, Vec<u8>); 5] = [
        (GUID_FILE_PARAMS, 4, params),
        (GUID_VIRTUAL_SIZE, 6, (blocks * MIB).to_le_bytes().to_vec()),
        (GUID_LOGICAL_SECTOR, 6, 512u32.to_le_bytes().to_vec()),
        (GUID_PHYSICAL_SECTOR, 6, 4096u32.to_le_bytes().to_vec()),
        (GUID_DISK_ID, 6, vec![0x33; 16]),
    ];
    put(&mut meta, 10, &(items.len() as u16).to_le_bytes());
    let mut pos = 64 * 1024usize;
    for (i, (id, flags, content)) in items.iter().enumerate() {
        let e = 32 + i * 32;
        put(&mut meta, e, id);
        put(&mut meta, e + 16, &(pos as u32).to_le_bytes());
        put(&mut meta, e + 20, &(content.len() as u32).to_le_bytes());
        put(&mut meta, e + 24, &flags.to_le_bytes());
        put(&mut meta, pos, content);
        pos += content.len().div_ceil(8) * 8;
    }
    w.write_all(&meta).unwrap();
    // BAT: todas las entradas de bloque FULLY_PRESENT en el mismo bloque; mapas de bits a 0.
    let block_entry = 6u64 | ((data_off / MIB) << 20);
    let mut chunk = Vec::with_capacity(MIB as usize);
    for i in 0..bat_len / 8 {
        let e = if i >= entries || (i + 1) % (ratio + 1) == 0 {
            0
        } else {
            block_entry
        };
        chunk.extend_from_slice(&e.to_le_bytes());
        if chunk.len() == MIB as usize {
            w.write_all(&chunk).unwrap();
            chunk.clear();
        }
    }
    w.write_all(&chunk).unwrap();
    // El único bloque de datos.
    w.write_all(&vec![0xCD; MIB as usize]).unwrap();
    w.into_inner().unwrap().sync_all().unwrap();
    (bat_len, blocks)
}

// ---------------------------------------------------------------------------------------------
// VMDK monolithicSparse
// ---------------------------------------------------------------------------------------------

const VMDK_GRAIN_SECTORS: u64 = 8;
const VMDK_DESC_SECTORS: u64 = 4;
const VMDK_GD_SECTOR: u64 = 1 + VMDK_DESC_SECTORS;

/// Cabecera y descriptor (los `VMDK_GD_SECTOR` primeros sectores).
fn vmdk_head(capacity: u64, overhead: u64) -> Vec<u8> {
    let mut f = vec![0u8; (VMDK_GD_SECTOR * 512) as usize];
    put(&mut f, 0, b"KDMV");
    put(&mut f, 4, &1u32.to_le_bytes());
    put(&mut f, 8, &(1u32 | 4).to_le_bytes()); // prueba de fin de línea y granos a cero
    put(&mut f, 12, &capacity.to_le_bytes());
    put(&mut f, 20, &VMDK_GRAIN_SECTORS.to_le_bytes());
    put(&mut f, 28, &1u64.to_le_bytes());
    put(&mut f, 36, &VMDK_DESC_SECTORS.to_le_bytes());
    put(&mut f, 44, &512u32.to_le_bytes());
    put(&mut f, 56, &VMDK_GD_SECTOR.to_le_bytes());
    put(&mut f, 64, &overhead.to_le_bytes());
    put(&mut f, 73, b"\n \r\n");
    let desc = format!(
        "# Disk DescriptorFile\nversion=1\nCID=12345678\nparentCID=ffffffff\n\
         createType=\"monolithicSparse\"\n\n# Extent description\n\
         RW {capacity} SPARSE \"prueba.vmdk\"\n\n# The Disk Data Base\n#DDB\n"
    );
    put(&mut f, 512, desc.as_bytes());
    f
}

/// Tablas de granos al límite (256 MiB en 131 072 GT distintas) con todos los granos en el mismo
/// sector. Devuelve los bytes de tablas leídos al abrir (GD + GT).
fn write_vmdk_max_gts(path: &Path) -> u64 {
    let gts = MAX_TABLE / 2048;
    let grains = gts * 512;
    let capacity = grains * VMDK_GRAIN_SECTORS;
    let gd_sectors = (gts * 4).div_ceil(512);
    let gt_first = VMDK_GD_SECTOR + gd_sectors;
    let data_sector = gt_first + gts * 4;
    let mut w = BufWriter::with_capacity(MIB as usize, File::create(path).unwrap());
    w.write_all(&vmdk_head(capacity, data_sector)).unwrap();
    let mut gd = Vec::with_capacity((gd_sectors * 512) as usize);
    for i in 0..gts {
        gd.extend_from_slice(&((gt_first + i * 4) as u32).to_le_bytes());
    }
    gd.resize((gd_sectors * 512) as usize, 0);
    w.write_all(&gd).unwrap();
    write_repeated(&mut w, &(data_sector as u32).to_le_bytes(), gts * 512);
    w.write_all(&vec![0xEF; (VMDK_GRAIN_SECTORS * 512) as usize])
        .unwrap();
    w.into_inner().unwrap().sync_all().unwrap();
    gts * 4 + gts * 2048
}

/// Directorio de granos al límite (256 MiB) con todas las entradas en la misma GT. Devuelve los
/// bytes de tablas leídos al abrir (el GD; ninguna GT es válida).
fn write_vmdk_max_gd(path: &Path) -> (u64, u64) {
    let gd_entries = MAX_TABLE / 4;
    let grains = gd_entries * 512;
    let capacity = grains * VMDK_GRAIN_SECTORS;
    let gt_sector = VMDK_GD_SECTOR + gd_entries * 4 / 512;
    let mut f = File::create(path).unwrap();
    let mut w = BufWriter::with_capacity(MIB as usize, &mut f);
    w.write_all(&vmdk_head(capacity, gt_sector + 4)).unwrap();
    write_repeated(&mut w, &(gt_sector as u32).to_le_bytes(), gd_entries);
    w.flush().unwrap();
    drop(w);
    // GT vacía (todos los granos sin asignar).
    f.seek(SeekFrom::Start(gt_sector * 512)).unwrap();
    f.write_all(&[0u8; 2048]).unwrap();
    f.sync_all().unwrap();
    (gd_entries * 4, grains)
}

// ---------------------------------------------------------------------------------------------
// Pruebas
// ---------------------------------------------------------------------------------------------

#[test]
fn vhd_max_bat_with_duplicated_entries_stays_within_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dup.vhd");
    let table = write_vhd(&path);

    let (r, peak) = peak_during(|| VhdImage::open_with(File::open(&path).unwrap(), false));
    assert!(is_corrupt(&r), "estricto: {:?}", r.err());
    assert_within_budget("VHD estricto", peak, table);

    let (r, peak) = peak_during(|| VhdImage::open_with(File::open(&path).unwrap(), true));
    let img = r.unwrap();
    assert_eq!(img.discarded_blocks(), table / 4);
    assert_eq!(img.size(), table / 4 * 512);
    assert_within_budget("VHD tolerante", peak, table);
}

#[test]
fn vhdx_max_bat_with_duplicated_entries_stays_within_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dup.vhdx");
    let (table, blocks) = write_vhdx(&path);

    let (r, peak) = peak_during(|| VhdxImage::open_with(File::open(&path).unwrap(), false));
    assert!(is_corrupt(&r), "estricto: {:?}", r.err());
    assert_within_budget("VHDX estricto", peak, table);

    let (r, peak) = peak_during(|| VhdxImage::open_with(File::open(&path).unwrap(), true));
    let img = r.unwrap();
    assert_eq!(img.discarded_blocks(), blocks);
    assert_within_budget("VHDX tolerante", peak, table);
}

#[test]
fn vmdk_max_grain_tables_with_duplicated_grains_stay_within_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dup_grains.vmdk");
    let table = write_vmdk_max_gts(&path);

    let (r, peak) = peak_during(|| VmdkImage::open_with(File::open(&path).unwrap(), false));
    assert!(is_corrupt(&r), "estricto: {:?}", r.err());
    assert_within_budget("VMDK (GT) estricto", peak, table);

    let (r, peak) = peak_during(|| VmdkImage::open_with(File::open(&path).unwrap(), true));
    let img = r.unwrap();
    assert_eq!(img.discarded_blocks(), (MAX_TABLE / 2048) * 512);
    assert_within_budget("VMDK (GT) tolerante", peak, table);
}

#[test]
fn vmdk_max_grain_directory_with_duplicated_tables_stays_within_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dup_gts.vmdk");
    let (table, grains) = write_vmdk_max_gd(&path);

    let (r, peak) = peak_during(|| VmdkImage::open_with(File::open(&path).unwrap(), false));
    assert!(is_corrupt(&r), "estricto: {:?}", r.err());
    assert_within_budget("VMDK (GD) estricto", peak, table);

    let (r, peak) = peak_during(|| VmdkImage::open_with(File::open(&path).unwrap(), true));
    let img = r.unwrap();
    assert_eq!(img.discarded_blocks(), grains);
    assert_within_budget("VMDK (GD) tolerante", peak, table);
}
