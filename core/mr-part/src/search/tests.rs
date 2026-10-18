//! Pruebas unitarias de la búsqueda con discos sintéticos construidos en memoria según las
//! especificaciones públicas (los mismos campos que escribe el generador del corpus).

use super::fs::{exfat_checksum, ExFat, Fat, Ntfs, Recognizer};
use super::*;
use crate::read_partition_table;
use mr_io::{FaultyDevice, MemDevice};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------------------------
// Constructores
// ---------------------------------------------------------------------------------------------

fn put(d: &mut [u8], at: usize, b: &[u8]) {
    d[at..at + b.len()].copy_from_slice(b);
}

/// Sector de arranque NTFS: total = `sectors - 1` (la copia ocupa el último sector).
fn ntfs_boot(ss: usize, hidden: u32, sectors: u64, serial: u64) -> Vec<u8> {
    let spc: u8 = if ss == 512 { 8 } else { 1 };
    let clusters = sectors / u64::from(spc);
    let mut s = vec![0u8; ss];
    put(&mut s, 0, &[0xEB, 0x52, 0x90]);
    put(&mut s, 3, b"NTFS    ");
    put(&mut s, 0x0B, &(ss as u16).to_le_bytes());
    s[0x0D] = spc;
    s[0x15] = 0xF8;
    put(&mut s, 0x18, &63u16.to_le_bytes());
    put(&mut s, 0x1A, &255u16.to_le_bytes());
    put(&mut s, 0x1C, &hidden.to_le_bytes());
    put(&mut s, 0x24, &0x0080_0080u32.to_le_bytes());
    put(&mut s, 0x28, &(sectors - 1).to_le_bytes());
    put(&mut s, 0x30, &4u64.to_le_bytes());
    put(&mut s, 0x38, &(clusters / 2).to_le_bytes());
    s[0x40] = (-10i8) as u8;
    s[0x44] = 1;
    put(&mut s, 0x48, &serial.to_le_bytes());
    put(&mut s, 0x1FE, &[0x55, 0xAA]);
    s
}

/// Escribe un NTFS en `[start, start + sectors)` con copia en el último sector.
fn ntfs(d: &mut [u8], ss: usize, start: usize, sectors: usize, hidden: u32) {
    let b = ntfs_boot(ss, hidden, sectors as u64, 0x1234_5678_9ABC_DEF0);
    put(d, start * ss, &b);
    put(d, (start + sectors - 1) * ss, &b);
}

/// Sector de arranque FAT32 o FAT12/16 (`fat32 = false`) según fatgen103.
fn fat_boot(fat32: bool, sectors: u32, hidden: u32) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    put(&mut b, 0, &[0xEB, 0x58, 0x90]);
    put(&mut b, 3, b"MSWIN4.1");
    put(&mut b, 0x0B, &512u16.to_le_bytes());
    b[0x0D] = 1;
    let rsvd: u16 = if fat32 { 32 } else { 1 };
    put(&mut b, 0x0E, &rsvd.to_le_bytes());
    b[0x10] = 2;
    b[0x15] = 0xF8;
    put(&mut b, 0x1C, &hidden.to_le_bytes());
    if fat32 {
        let fatsz = (sectors - 32).div_ceil((256 + 2) / 2);
        put(&mut b, 0x20, &sectors.to_le_bytes());
        put(&mut b, 0x24, &fatsz.to_le_bytes());
        put(&mut b, 0x2C, &2u32.to_le_bytes());
        put(&mut b, 0x30, &1u16.to_le_bytes());
        put(&mut b, 0x32, &6u16.to_le_bytes());
        b[0x42] = 0x29;
        put(&mut b, 0x43, &0xCAFE_F00Du32.to_le_bytes());
        put(&mut b, 0x52, b"FAT32   ");
    } else {
        put(&mut b, 0x11, &512u16.to_le_bytes());
        put(&mut b, 0x13, &(sectors as u16).to_le_bytes());
        // Un sector por clúster y 2 bytes por entrada (sobra para FAT12).
        let fatsz = (sectors / 256 + 1) as u16;
        put(&mut b, 0x16, &fatsz.to_le_bytes());
        b[0x26] = 0x29;
        put(&mut b, 0x27, &0xBEEF_0016u32.to_le_bytes());
        put(&mut b, 0x36, b"FAT16   ");
    }
    put(&mut b, 0x1FE, &[0x55, 0xAA]);
    b
}

/// Región de arranque exFAT de 12 sectores (con su suma en el sector 11).
fn exfat_region(ss: usize, start: u64, sectors: u64) -> Vec<u8> {
    let shift = ss.trailing_zeros() as u8;
    let spc_shift: u8 = if ss == 512 { 3 } else { 0 };
    let clusters = ((sectors - 64) >> spc_shift) as u32;
    let mut b = vec![0u8; ss];
    put(&mut b, 0, &[0xEB, 0x76, 0x90]);
    put(&mut b, 3, b"EXFAT   ");
    put(&mut b, 64, &start.to_le_bytes());
    put(&mut b, 72, &sectors.to_le_bytes());
    put(&mut b, 80, &32u32.to_le_bytes());
    put(&mut b, 84, &8u32.to_le_bytes());
    put(&mut b, 88, &64u32.to_le_bytes());
    put(&mut b, 92, &clusters.to_le_bytes());
    put(&mut b, 96, &4u32.to_le_bytes());
    put(&mut b, 100, &0x4D52_3003u32.to_le_bytes());
    put(&mut b, 104, &0x0100u16.to_le_bytes());
    b[108] = shift;
    b[109] = spc_shift;
    b[110] = 1;
    b[111] = 0x80;
    put(&mut b, 510, &[0x55, 0xAA]);
    let mut region = vec![0u8; 11 * ss];
    put(&mut region, 0, &b);
    for i in 1..9 {
        put(&mut region, (i + 1) * ss - 4, &0xAA55_0000u32.to_le_bytes());
    }
    let sum = exfat_checksum(&region);
    for _ in 0..ss / 4 {
        region.extend_from_slice(&sum.to_le_bytes());
    }
    region
}

fn exfat(d: &mut [u8], ss: usize, start: usize, sectors: usize) {
    let r = exfat_region(ss, start as u64, sectors as u64);
    put(d, start * ss, &r);
    put(d, (start + 12) * ss, &r);
    put(
        d,
        (start + 32) * ss,
        &[0xF8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    );
}

fn mbr(d: &mut [u8], at: usize, entries: &[(u8, u8, u32, u32)]) {
    for (i, &(boot, kind, start, count)) in entries.iter().enumerate() {
        let e = at + 0x1BE + 16 * i;
        d[e] = boot;
        d[e + 4] = kind;
        put(d, e + 8, &start.to_le_bytes());
        put(d, e + 12, &count.to_le_bytes());
    }
    put(d, at + 0x1FE, &[0x55, 0xAA]);
}

fn find(dev: &dyn BlockDevice, opts: &SearchOptions) -> SearchResult {
    let t = read_partition_table(dev).unwrap();
    search(dev, Some(&t), opts, &NoObserver).unwrap()
}

fn mem(d: Vec<u8>, ss: u32) -> MemDevice {
    MemDevice::new(d, ss).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Reconocedores
// ---------------------------------------------------------------------------------------------

#[test]
fn ntfs_recognizer_accepts_valid_and_rejects_each_field() {
    let b = ntfs_boot(512, 2048, 2048, 7);
    let p = Ntfs.parse(&b).unwrap();
    assert_eq!(
        (p.fs, p.sector_size, p.length),
        (FsKind::Ntfs, 512, 2048 * 512)
    );
    assert_eq!(
        p.backups,
        vec![(2047 * 512, 2048 * 512), (2048 * 512, 2049 * 512)]
    );
    assert_eq!((p.declared_start, p.serial), (2048, 7));
    let bad: [(usize, &[u8]); 11] = [
        (0, &[0x00]),
        (3, b"NTFX"),
        (0x0B, &[0x00, 0x03]),
        (0x0D, &[3]),
        (0x0E, &[1]),
        (0x10, &[2]),
        (0x13, &[1]),
        (0x28, &[0; 8]),
        (0x30, &[0xFF; 8]),
        (0x40, &[0]),
        (0x1FE, &[0]),
    ];
    for (at, v) in bad {
        let mut x = b.clone();
        put(&mut x, at, v);
        assert!(Ntfs.parse(&x).is_none(), "campo {at:#x}");
    }
    assert!(Ntfs.parse(&b[..511]).is_none());
    // Clústeres grandes (2^(256 - 0xF4) sectores) y registro expresado en clústeres.
    let mut x = b.clone();
    x[0x0D] = 0xF4;
    put(&mut x, 0x28, &(1u64 << 20).to_le_bytes());
    assert!(Ntfs.parse(&x).is_some());
    x[0x40] = 1;
    assert!(Ntfs.parse(&x).is_some());
    // Copias coherentes: mismos campos clave.
    let mut y = b.clone();
    assert!(Ntfs.consistent(&p, &Ntfs.parse(&y).unwrap()));
    put(&mut y, 0x48, &8u64.to_le_bytes());
    assert!(!Ntfs.consistent(&p, &Ntfs.parse(&y).unwrap()));
}

#[test]
fn fat_recognizer_types_by_cluster_count() {
    let p = Fat.parse(&fat_boot(true, 67584, 63)).unwrap();
    assert_eq!(
        (p.fs, p.length, p.serial),
        (FsKind::Fat32, 67584 * 512, 0xCAFE_F00D)
    );
    assert_eq!(p.backups, vec![(6 * 512, 67584 * 512)]);
    // Disposición FAT32 con pocos clústeres: incoherente.
    assert!(Fat.parse(&fat_boot(true, 20000, 0)).is_none());
    let p = Fat.parse(&fat_boot(false, 20000, 0)).unwrap();
    assert_eq!((p.fs, p.backups.len()), (FsKind::Fat16, 0));
    let p = Fat.parse(&fat_boot(false, 3000, 0)).unwrap();
    assert_eq!(p.fs, FsKind::Fat12);
    for (at, v) in [
        (0x0B, &[0x00, 0x01][..]),
        (0x0D, &[3]),
        (0x0E, &[0, 0]),
        (0x10, &[3]),
        (0x15, &[0x12]),
        (0x1FE, &[0]),
    ] {
        let mut x = fat_boot(true, 67584, 63);
        put(&mut x, at, v);
        assert!(Fat.parse(&x).is_none(), "campo {at:#x}");
    }
    // FAT demasiado pequeña para los clústeres.
    let mut x = fat_boot(true, 67584, 63);
    put(&mut x, 0x24, &10u32.to_le_bytes());
    assert!(Fat.parse(&x).is_none());
    // NTFS y exFAT no son FAT.
    assert!(Fat.parse(&ntfs_boot(512, 0, 2048, 1)).is_none());
    assert!(Fat.parse(&exfat_region(512, 0, 4096)[..512]).is_none());
}

#[test]
fn exfat_recognizer_checks_fields_and_checksum() {
    let r = exfat_region(512, 2048, 2048);
    let p = ExFat.parse(&r).unwrap();
    assert_eq!(
        (p.fs, p.sector_size, p.declared_start),
        (FsKind::ExFat, 512, 2048)
    );
    assert_eq!(ExFat.copy_len(&p), 12 * 512);
    assert!(ExFat.copy_ok(&p, &r));
    let mut bad = r.clone();
    bad[600] ^= 1;
    assert!(!ExFat.copy_ok(&p, &bad));
    // VolumeFlags y PercentInUse no cuentan en la suma ni en la coherencia.
    let mut flags = r.clone();
    flags[106] = 1;
    flags[112] = 50;
    assert!(ExFat.copy_ok(&p, &flags));
    assert!(ExFat.consistent(&p, &ExFat.parse(&flags).unwrap()));
    for (at, v) in [
        (11, &[1u8][..]),
        (80, &[1, 0, 0, 0]),
        (104, &[0, 2]),
        (108, &[13]),
        (110, &[3]),
        (96, &[0xFF, 0xFF, 0, 0]),
        (510, &[0]),
    ] {
        let mut x = r.clone();
        put(&mut x, at, v);
        assert!(ExFat.parse(&x).is_none(), "campo {at}");
    }
}

/// Fuzzing acotado de los reconocedores: sin pánicos con bytes arbitrarios ni con mutaciones de
/// sectores válidos.
#[test]
fn recognizers_never_panic_on_mutations() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let bases = [
        ntfs_boot(512, 0, 4096, 1),
        fat_boot(true, 67584, 0),
        fat_boot(false, 20000, 0),
        exfat_region(512, 0, 4096),
    ];
    for i in 0..6000 {
        let mut s = bases[i % bases.len()].clone();
        if i % 7 == 0 {
            s.iter_mut().for_each(|b| *b = next() as u8);
        }
        for _ in 0..(next() % 6) {
            let at = (next() % 512) as usize;
            s[at] = next() as u8;
        }
        for r in fs::RECOGNIZERS {
            if let Some(b) = r.parse(&s) {
                let _ = r.copy_ok(&b, &s);
                let _ = r.structures(&b);
                let _ = r.consistent(&b, &b);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Búsqueda
// ---------------------------------------------------------------------------------------------

#[test]
fn options_are_validated() {
    assert!(SearchOptions::default().validate().is_ok());
    for o in [
        SearchOptions {
            max_reads: 0,
            ..SearchOptions::default()
        },
        SearchOptions {
            probe_window: 1000,
            ..SearchOptions::default()
        },
        SearchOptions {
            families: 0,
            ..SearchOptions::default()
        },
        SearchOptions {
            families: 64,
            ..SearchOptions::default()
        },
        SearchOptions {
            max_bytes: LIMIT_MAX_BYTES + 1,
            ..SearchOptions::default()
        },
    ] {
        let d = mem(vec![0; 4096], 512);
        assert!(search(&d, None, &o, &NoObserver).is_err(), "{o:?}");
    }
}

/// Tabla intacta: el volumen que describe una entrada se marca con ella, no se duplica.
#[test]
fn table_partition_is_marked_not_duplicated() {
    let mut d = vec![0u8; 8192 * 512];
    mbr(&mut d, 0, &[(0x80, 0x07, 2048, 4096)]);
    ntfs(&mut d, 512, 2048, 4096, 2048);
    let r = find(&mem(d, 512), &SearchOptions::default());
    assert_eq!(r.volumes.len(), 1, "{:#?}", r.volumes);
    let v = &r.volumes[0];
    assert_eq!(
        (v.table_partition, v.entry, v.tier),
        (Some(0), Check::Yes, Tier::Declared)
    );
    assert!(v.listed());
    assert_eq!(v.confidence, Confidence::Valid);
    assert!(r.tables.is_empty());
}

/// FAT16 (sin copia): válido con principal y FAT coherente; sin la FAT, sospechoso.
#[test]
fn fat16_without_backup_needs_structures() {
    let mut d = vec![0u8; 24000 * 512];
    put(&mut d, 2048 * 512, &fat_boot(false, 20000, 2048));
    let r = find(&mem(d.clone(), 512), &SearchOptions::default());
    let v = &r.volumes[0];
    assert_eq!(
        (v.fs, v.backup, v.structures),
        (FsKind::Fat16, Check::NotApplicable, Check::No)
    );
    assert!(v.anomalies.contains(&VolumeAnomaly::Unconfirmed));
    assert!(matches!(v.confidence, Confidence::Suspicious(_)));
    put(&mut d, (2048 + 1) * 512, &[0xF8, 0xFF, 0xFF, 0xFF]);
    let r = find(&mem(d, 512), &SearchOptions::default());
    let v = &r.volumes[0];
    assert_eq!(
        (v.structures, &v.confidence),
        (Check::Yes, &Confidence::Valid)
    );
    assert_eq!(v.indicators(), "Ex P+ C- S+ U+ A+");
}

/// Un sector ilegible cuyo contenido (relleno) es un arranque NTFS perfecto nunca produce un
/// volumen; con la copia legible, el volumen sale de la copia con la principal «?».
#[test]
fn unreadable_fill_that_mimics_a_boot_sector_is_never_evidence() {
    let mut d = vec![0u8; 4096 * 512];
    ntfs(&mut d, 512, 2048, 2048, 2048);
    let both = FaultyDevice::new(mem(d.clone(), 512)).with_bad([2048, 4095]);
    let r = find(&both, &SearchOptions::default());
    assert!(r.volumes.is_empty(), "{:#?}", r.volumes);
    assert!(r.stats.unverifiable_sectors >= 1);
    let one = FaultyDevice::new(mem(d, 512)).with_bad([2048]);
    let r = find(&one, &SearchOptions::default());
    assert_eq!(r.volumes.len(), 1);
    assert_eq!(r.volumes[0].primary, Check::Unverifiable);
}

/// Un sector con aspecto de MBR dentro de una partición válida no es una tabla; el mismo sector
/// en un disco sin tabla sí supera el modo estricto (la regla es la que lo descarta).
#[test]
fn decoy_table_inside_valid_partition_is_ignored() {
    let mut d = vec![0u8; 16384 * 512];
    let decoy = 4096 * 512; // en la rejilla de 1 MiB
    mbr(
        &mut d,
        decoy,
        &[(0x80, 0x07, 100, 500), (0, 0x0C, 700, 300)],
    );
    let mut with_table = d.clone();
    mbr(&mut with_table, 0, &[(0, 0x07, 2048, 12288)]);
    let r = find(&mem(with_table, 512), &SearchOptions::default());
    assert!(r.tables.is_empty(), "{:?}", r.tables);
    let r = find(&mem(d, 512), &SearchOptions::default());
    assert_eq!(r.tables.len(), 1);
    assert_eq!(r.tables[0].lba, 4096);
}

/// Una tabla encontrada en una posición candidata propone sus entradas (P1) y marca los
/// volúmenes que describe; una cadena de EBR se sigue con los enlaces relativos a la extendida.
#[test]
fn found_mbr_and_ebr_chain_propose_candidates() {
    let mut d = vec![0u8; 40960 * 512];
    // Disco interno en la LBA 4096 (rejilla): una extendida con un EBR.
    let base = 4096;
    mbr(&mut d, base * 512, &[(0, 0x0F, 8192, 16384)]);
    // EBR en base + 8192 con una lógica en +1000 relativa al EBR (fuera de cualquier rejilla).
    mbr(&mut d, (base + 8192) * 512, &[(0, 0x07, 1000, 4096)]);
    let logical = base + 8192 + 1000;
    exfat(&mut d, 512, logical, 4096);
    let r = find(&mem(d, 512), &SearchOptions::default());
    assert_eq!(r.tables.len(), 2, "{:?}", r.tables);
    assert_eq!(r.volumes.len(), 1, "{:#?}", r.volumes);
    let ex = &r.volumes[0];
    assert_eq!(ex.start_byte, logical as i64 * 512);
    assert_eq!((ex.entry, ex.found_table), (Check::Yes, Some(1)));
    assert_eq!(ex.tier, Tier::Derived);
}

/// GPT de respaldo en una posición que no es el último sector (disco ampliado): cabecera
/// encontrada en P4 con su array de entradas, y el volumen que describe.
#[test]
fn found_gpt_header_proposes_its_entries() {
    let total = 4096usize;
    let extra = 2048usize;
    let mut d = vec![0u8; (total + extra) * 512];
    let mut entries = vec![0u8; 128 * 128];
    entries[..16].fill(0x42);
    entries[16..32].fill(0x5A);
    put(&mut entries, 32, &1000u64.to_le_bytes());
    put(&mut entries, 40, &2023u64.to_le_bytes());
    let ecrc = crc32fast::hash(&entries);
    let mut h = vec![0u8; 512];
    put(&mut h, 0, b"EFI PART");
    put(&mut h, 8, &0x0001_0000u32.to_le_bytes());
    put(&mut h, 12, &92u32.to_le_bytes());
    put(&mut h, 24, &((total - 1) as u64).to_le_bytes());
    put(&mut h, 32, &1u64.to_le_bytes());
    put(&mut h, 40, &34u64.to_le_bytes());
    put(&mut h, 48, &((total - 34) as u64).to_le_bytes());
    put(&mut h, 72, &((total - 33) as u64).to_le_bytes());
    put(&mut h, 80, &128u32.to_le_bytes());
    put(&mut h, 84, &128u32.to_le_bytes());
    put(&mut h, 88, &ecrc.to_le_bytes());
    let crc = crc32fast::hash(&h[..92]);
    put(&mut h, 16, &crc.to_le_bytes());
    put(&mut d, (total - 33) * 512, &entries);
    put(&mut d, (total - 1) * 512, &h);
    // NTFS en la entrada, sin alinear a ninguna rejilla: solo la entrada lo propone.
    ntfs(&mut d, 512, 1000, 1024, 1000);
    let r = find(&mem(d, 512), &SearchOptions::default());
    let t = r
        .tables
        .iter()
        .find(|t| t.kind == FoundTableKind::Gpt)
        .expect("cabecera GPT encontrada");
    assert_eq!((t.lba, t.declared_lba, t.entries.len()), (4095, 4095, 1));
    assert_eq!(r.volumes.len(), 1, "{:#?}", r.volumes);
    assert_eq!(r.volumes[0].entry, Check::Yes);
    assert_eq!(r.volumes[0].tier, Tier::Derived);
    assert!(r.shift_bytes.is_none());
}

/// Dispositivo virtual enorme con unos pocos tramos de datos (declarados `Data`) que cuenta sus
/// lecturas: escala sin reservar memoria.
struct Sparse {
    size: u64,
    chunks: Vec<(u64, Vec<u8>)>,
    reads: AtomicU64,
}

impl BlockDevice for Sparse {
    fn size(&self) -> u64 {
        self.size
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> mr_io::Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        buf.fill(0);
        let end = offset + buf.len() as u64;
        for (at, data) in &self.chunks {
            let (a, b) = (*at, at + data.len() as u64);
            if a < end && offset < b {
                let lo = a.max(offset);
                let hi = b.min(end);
                buf[(lo - offset) as usize..(hi - offset) as usize]
                    .copy_from_slice(&data[(lo - a) as usize..(hi - a) as usize]);
            }
        }
        Ok(())
    }
}

fn sparse(size: u64) -> Sparse {
    let boot = ntfs_boot(512, 2048, 2048, 0x55);
    Sparse {
        size,
        chunks: vec![(2048 * 512, boot.clone()), (4095 * 512, boot)],
        reads: AtomicU64::new(0),
    }
}

/// Escala (INV-05b §9): el mismo contenido en orígenes de 1 GiB y de 2 TiB da el mismo resultado
/// con prácticamente las mismas lecturas, siempre por debajo del presupuesto.
#[test]
fn reads_do_not_grow_with_disk_size() {
    let mut counts = Vec::new();
    for size in [1u64 << 30, 2u64 << 40] {
        let dev = sparse(size);
        let t = read_partition_table(&dev).unwrap();
        dev.reads.store(0, Ordering::Relaxed);
        let r = search(&dev, Some(&t), &SearchOptions::default(), &NoObserver).unwrap();
        assert_eq!(r.status, SearchStatus::Complete);
        assert_eq!(r.volumes.len(), 1, "{:#?}", r.volumes);
        assert_eq!(r.volumes[0].start_byte, 2048 * 512);
        assert_eq!(dev.reads.load(Ordering::Relaxed), r.stats.reads);
        assert!(r.stats.reads < 400, "{}", r.stats.reads);
        counts.push(r.stats.reads);
    }
    assert!(counts[0].abs_diff(counts[1]) <= 4, "{counts:?}");
}

/// Imagen desplazada sin tabla: un volumen que declara otro inicio no basta; dos que coinciden
/// hacen aceptar la hipótesis, que se comunica y nunca se aplica en silencio.
#[test]
fn shift_needs_two_independent_pieces_of_evidence() {
    // Disco de 6144 sectores con 16 añadidos al inicio: los volúmenes declaran 2048 y 4096. El
    // NTFS se encuentra por su copia (final de MiB contado desde el final, P4).
    let mut d = vec![0u8; (6144 + 16) * 512];
    ntfs(&mut d, 512, 2048 + 16, 2048, 2048);
    let r = find(&mem(d.clone(), 512), &SearchOptions::default());
    assert_eq!(r.volumes.len(), 1);
    assert!(r.shift_bytes.is_none());
    assert!(r.volumes[0]
        .anomalies
        .iter()
        .any(|a| matches!(a, VolumeAnomaly::SelfLocationMismatch { .. })));
    assert_eq!(r.volumes[0].confidence, Confidence::Valid);
    exfat(&mut d, 512, 4096 + 16, 2048);
    // El exFAT declara su inicio en el disco; corregido al de la imagen para que coincida.
    put(&mut d, (4096 + 16) * 512 + 64, &4096u64.to_le_bytes());
    put(&mut d, (4096 + 16 + 12) * 512 + 64, &4096u64.to_le_bytes());
    let fix = |d: &mut Vec<u8>, at: usize| {
        let sum = exfat_checksum(&d[at..at + 11 * 512]);
        for i in 0..128 {
            put(d, at + 11 * 512 + 4 * i, &sum.to_le_bytes());
        }
    };
    fix(&mut d, (4096 + 16) * 512);
    fix(&mut d, (4096 + 16 + 12) * 512);
    let r = find(&mem(d, 512), &SearchOptions::default());
    assert_eq!(r.shift_bytes, Some(16 * 512), "{r:#?}");
    assert!(r.volumes.iter().all(FoundVolume::requires_shift));
    assert_eq!(
        r.anomalies[0],
        SearchAnomaly::ImageShifted {
            delta_bytes: 16 * 512,
            sector_size: 512,
            evidence: 2
        }
    );
    assert_eq!(
        (r.anomalies[0].lba(), r.anomalies[0].value()),
        (16, Some(-16))
    );
}

/// Cupos por nivel: con un presupuesto pequeño, la rejilla no consume la reserva de P0 y P1.
#[test]
fn grid_tiers_respect_their_quota() {
    let d = mem(vec![0u8; 300_000 * 512], 512);
    let opts = SearchOptions {
        max_reads: 40,
        ..SearchOptions::default()
    };
    let r = find(&d, &opts);
    assert_eq!(r.status, SearchStatus::BudgetExhausted);
    for t in [Tier::StartGrid, Tier::Gaps, Tier::EndGrid] {
        assert!(r.stats.reads_by_tier[t.index()] <= 16, "{:?}", r.stats);
    }
    // Reserva del 25 % para P0 y P1 (10 lecturas): la rejilla no la invade.
    let grid: u64 = r.stats.reads_by_tier[2..].iter().sum();
    assert!(grid <= 30, "{:?}", r.stats);
    assert!(r.pending[Tier::StartGrid.index()] > 0);
}

/// Límite de volúmenes y familias desactivadas.
#[test]
fn volume_limit_and_families() {
    let mut d = vec![0u8; 12288 * 512];
    ntfs(&mut d, 512, 2048, 2048, 2048);
    exfat(&mut d, 512, 4096, 2048);
    ntfs(&mut d, 512, 6144, 2048, 6144);
    let r = find(
        &mem(d.clone(), 512),
        &SearchOptions {
            max_volumes: 2,
            ..SearchOptions::default()
        },
    );
    assert!(r.volume_limit_reached);
    assert_eq!(
        (r.volumes.len(), r.status),
        (2, SearchStatus::BudgetExhausted)
    );
    let r = find(
        &mem(d, 512),
        &SearchOptions {
            families: family::EXFAT,
            ..SearchOptions::default()
        },
    );
    assert_eq!(r.volumes.len(), 1);
    assert_eq!(r.volumes[0].fs, FsKind::ExFat);
}

/// Fuzzing acotado de la búsqueda completa: discos pequeños con tablas y estructuras válidas
/// mutadas al azar (semilla fija). Sin pánicos, presupuesto respetado y resultado determinista.
#[test]
fn search_fuzz_is_bounded() {
    let mut seed = 0x0123_4567_89AB_CDEFu64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut base = vec![0u8; 12288 * 512];
    mbr(
        &mut base,
        0,
        &[(0, 0x07, 2048, 2048), (0, 0x0F, 6144, 6144)],
    );
    mbr(&mut base, 6144 * 512, &[(0, 0x07, 63, 2048)]);
    ntfs(&mut base, 512, 2048, 2048, 2048);
    exfat(&mut base, 512, 4096, 2048);
    for _ in 0..150 {
        let mut d = base.clone();
        for _ in 0..(1 + next() % 40) {
            let sector = [0usize, 2048, 4095, 4096, 4108, 6144, 4096 + 32][(next() % 7) as usize];
            let at = sector * 512 + (next() % 512) as usize;
            d[at] = next() as u8;
        }
        let opts = SearchOptions {
            max_reads: 64 + next() % 512,
            ..SearchOptions::default()
        };
        let dev = mem(d, 512);
        let a = find(&dev, &opts);
        assert!(a.stats.reads <= opts.max_reads);
        assert_eq!(a, find(&dev, &opts));
    }
}
