//! Pruebas de integración de imágenes divididas con ficheros temporales sintéticos.

use mr_image::split::{discover, SplitImage, MAX_SEGMENTS};
use mr_io::{BlockDevice, IoError};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// RAW determinista (xorshift64*), sin dependencias externas.
fn raw(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        out.extend_from_slice(&x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Divide `data` en segmentos con los tamaños dados (el último se lleva el resto).
fn write_split(dir: &Path, base: &str, data: &[u8], sizes: &[usize]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut pos = 0;
    for (i, &s) in sizes.iter().enumerate() {
        let end = if i + 1 == sizes.len() {
            data.len()
        } else {
            pos + s
        };
        let p = dir.join(format!("{base}.{:03}", i + 1));
        std::fs::write(&p, &data[pos..end]).unwrap();
        paths.push(p);
        pos = end;
    }
    assert_eq!(pos, data.len());
    paths
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Lee el dispositivo completo en bloques de `chunk` bytes.
fn read_all(dev: &dyn BlockDevice, chunk: usize) -> Vec<u8> {
    let mut out = vec![0u8; dev.size() as usize];
    for (i, c) in out.chunks_mut(chunk).enumerate() {
        dev.read_at((i * chunk) as u64, c).unwrap();
    }
    out
}

fn snapshot(paths: &[PathBuf]) -> Vec<(String, SystemTime, u64)> {
    paths
        .iter()
        .map(|p| {
            let md = std::fs::metadata(p).unwrap();
            (
                sha_hex(&std::fs::read(p).unwrap()),
                md.modified().unwrap(),
                md.len(),
            )
        })
        .collect()
}

fn check_layout(sizes: &[usize], total: usize, ss: u32) {
    let dir = tempfile::tempdir().unwrap();
    let data = raw(total, total as u64 ^ 0xC0FFEE);
    let paths = write_split(dir.path(), "disco", &data, sizes);
    let found = discover(&paths[0]).unwrap();
    assert_eq!(found, paths);
    let before = snapshot(&paths);
    let img = SplitImage::open(&found, ss).unwrap();
    assert_eq!(img.size(), total as u64);
    assert_eq!(img.sector_size(), ss);
    assert_eq!(img.segments().len(), paths.len());
    // Lectura completa con varios tamaños de bloque (alineados y no alineados).
    for chunk in [ss as usize, 4096, 65536, 7777, total] {
        let all = read_all(&img, chunk);
        assert_eq!(sha_hex(&all), sha_hex(&data), "bloque {chunk}");
    }
    // Lecturas que cruzan cada frontera (y varias a la vez).
    let mut boundary = 0usize;
    for &s in &sizes[..sizes.len() - 1] {
        boundary += s;
        for (back, len) in [
            (1, 2),
            (1, 1),
            (0, 1),
            (17, 1000),
            (ss as usize, 2 * ss as usize),
        ] {
            let off = boundary.saturating_sub(back);
            let len = len.min(total - off);
            let mut b = vec![0u8; len];
            img.read_at(off as u64, &mut b).unwrap();
            assert_eq!(
                &b[..],
                &data[off..off + len],
                "frontera {boundary}, {back}/{len}"
            );
        }
    }
    // Lectura que abarca todos los segmentos menos los bordes.
    let mut b = vec![0u8; total - 2];
    img.read_at(1, &mut b).unwrap();
    assert_eq!(&b[..], &data[1..total - 1]);
    // Sectores completos.
    let sectors = img.sector_count();
    let mut s = vec![0u8; ss as usize];
    img.read_sectors(sectors - 1, 1, &mut s).unwrap();
    let o = ((sectors - 1) * u64::from(ss)) as usize;
    assert_eq!(&s[..], &data[o..o + ss as usize]);
    assert!(img.read_sectors(sectors, 1, &mut s).is_err());
    // Límites.
    let mut one = [0u8; 1];
    img.read_at(total as u64 - 1, &mut one).unwrap();
    assert_eq!(one[0], data[total - 1]);
    assert!(matches!(
        img.read_at(total as u64, &mut one),
        Err(IoError::OutOfBounds { .. })
    ));
    assert!(matches!(
        img.read_at(u64::MAX, &mut one),
        Err(IoError::OutOfBounds { .. })
    ));
    img.read_at(total as u64, &mut []).unwrap();
    drop(img);
    assert_eq!(snapshot(&paths), before, "los segmentos han cambiado");
}

#[test]
fn equal_segments() {
    check_layout(&[256 * 1024; 4], 1024 * 1024, 512);
}

#[test]
fn last_segment_smaller() {
    check_layout(&[300 * 1024, 300 * 1024, 300 * 1024, 0], 1000 * 1024, 4096);
}

#[test]
fn segments_not_sector_aligned() {
    check_layout(&[100_003, 100_003, 100_003, 0], 333_333, 512);
}

#[test]
fn single_segment() {
    check_layout(&[0], 50_000, 512);
}

#[test]
fn many_small_segments() {
    // Frontera en cada sector o casi: 64 segmentos de 513 bytes.
    check_layout(&[513; 64], 513 * 64, 512);
}

#[test]
fn describe_and_anomalies() {
    let dir = tempfile::tempdir().unwrap();
    let data = raw(3 * 1024 * 1024 + 512 * 1024, 7);
    let paths = write_split(
        dir.path(),
        "x",
        &data,
        &[1024 * 1024, 1024 * 1024, 1024 * 1024, 0],
    );
    let img = SplitImage::open(&paths, 512).unwrap();
    let d = img.describe();
    assert!(
        d.starts_with("Imagen dividida en 4 segmentos (x.001–x.004), 3,5 MiB"),
        "{d}"
    );
    assert!(img.anomalies().is_empty(), "{d}");

    // Intermedio menor que los anteriores.
    let dir2 = tempfile::tempdir().unwrap();
    let paths = write_split(
        dir2.path(),
        "y",
        &data,
        &[1024 * 1024, 4096, 1024 * 1024, 0],
    );
    let img = SplitImage::open(&paths, 512).unwrap();
    assert_eq!(read_all(&img, 65536), data);
    let d = img.describe();
    assert!(d.contains("anomalías") && d.contains("y.002"), "{d}");

    // Último segmento vacío y último mayor que el primero.
    let dir3 = tempfile::tempdir().unwrap();
    let mut paths = write_split(dir3.path(), "z", &data, &[1024, 0]);
    let empty = dir3.path().join("z.003");
    std::fs::write(&empty, []).unwrap();
    paths.push(empty);
    let img = SplitImage::open(&paths, 512).unwrap();
    assert_eq!(img.size(), data.len() as u64);
    assert_eq!(read_all(&img, 100_000), data);
    let d = img.describe();
    assert!(d.contains("z.003") && d.contains("vacío"), "{d}");
    // El segmento z.002 (último con datos, pero no último de la lista) es mayor que el primero.
    assert!(d.contains("z.002"), "{d}");

    // Tamaño total no múltiplo del sector.
    let dir4 = tempfile::tempdir().unwrap();
    let paths = write_split(dir4.path(), "w", &data[..1000], &[600, 0]);
    let img = SplitImage::open(&paths, 512).unwrap();
    assert_eq!(img.sector_count(), 1);
    assert!(img.describe().contains("múltiplo"), "{}", img.describe());
    let one = dir4.path().join("v.001");
    std::fs::write(&one, &data[..1536]).unwrap();
    assert_eq!(
        SplitImage::open(&[one], 512).unwrap().describe(),
        "Imagen dividida en 1 segmento (v.001), 1,5 KiB (1536 bytes)"
    );
}

#[test]
fn open_rejects_invalid_sets() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.001");
    let b = dir.path().join("a.002");
    let c = dir.path().join("a.003");
    std::fs::write(&a, [1u8; 1024]).unwrap();
    std::fs::write(&b, []).unwrap();
    std::fs::write(&c, [2u8; 1024]).unwrap();
    // Intermedio vacío.
    assert!(matches!(
        SplitImage::open(&[a.clone(), b.clone(), c.clone()], 512),
        Err(IoError::Corrupt { .. })
    ));
    // Todo vacío.
    assert!(matches!(
        SplitImage::open(std::slice::from_ref(&b), 512),
        Err(IoError::Corrupt { .. })
    ));
    // Lista vacía.
    assert!(matches!(
        SplitImage::open(&[], 512),
        Err(IoError::Corrupt { .. })
    ));
    // Segmento repetido.
    assert!(matches!(
        SplitImage::open(&[a.clone(), a.clone()], 512),
        Err(IoError::Corrupt { .. })
    ));
    // Sector no válido.
    assert!(matches!(
        SplitImage::open(std::slice::from_ref(&a), 500),
        Err(IoError::InvalidSectorSize(500))
    ));
    // Directorio y ruta de dispositivo.
    let sub = dir.path().join("a.004");
    std::fs::create_dir(&sub).unwrap();
    assert!(matches!(
        SplitImage::open(&[a.clone(), sub], 512),
        Err(IoError::DevicePathRejected(_))
    ));
    assert!(matches!(
        SplitImage::open(&[a.clone(), PathBuf::from(r"\\.\PhysicalDrive0")], 512),
        Err(IoError::DevicePathRejected(_))
    ));
    // Segmento inexistente.
    assert!(SplitImage::open(&[a, dir.path().join("nada.002")], 512).is_err());
}

#[test]
fn truncated_segment_reports_global_offset() {
    let dir = tempfile::tempdir().unwrap();
    let data = raw(4096, 3);
    let paths = write_split(dir.path(), "t", &data, &[2048, 0]);
    let img = SplitImage::open(&paths, 512).unwrap();
    // Otro proceso acorta el segundo segmento tras abrirlo.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&paths[1])
        .unwrap()
        .set_len(1024)
        .unwrap();
    let mut b = vec![0u8; 2048];
    match img.read_at(2048, &mut b) {
        Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (2048, 2048)),
        other => panic!("se esperaba error de lectura, no {other:?}"),
    }
    // Lectura que cruza: el error indica el desplazamiento global del tramo del segundo segmento.
    let mut b = vec![0u8; 2048];
    match img.read_at(1536, &mut b) {
        Err(IoError::Read { offset, len, .. }) => assert_eq!((offset, len), (2048, 1536)),
        other => panic!("se esperaba error de lectura, no {other:?}"),
    }
}

fn touch(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, name.as_bytes()).unwrap();
    p
}

fn names(v: &[PathBuf]) -> Vec<String> {
    v.iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn discover_stops_at_first_gap() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let first = touch(d, "img.001");
    touch(d, "img.002");
    touch(d, "img.003");
    touch(d, "img.005");
    touch(d, "img.006");
    assert_eq!(
        names(&discover(&first).unwrap()),
        ["img.001", "img.002", "img.003"]
    );
    // Todos los resultados están en el mismo directorio que el primero.
    for p in discover(&first).unwrap() {
        assert_eq!(p.parent(), first.parent());
    }
}

#[test]
fn discover_with_multiple_sets_and_odd_names() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for n in [
        "a.001",
        "a.002",
        "a.img.001",
        "a.img.002",
        "a.img.003",
        "b.001",
        "a.0001",
        "a.0002",
        "a.002.bak",
        "a.003x",
        "A copia.001",
        "disco con espacios y ñ.001",
        "disco con espacios y ñ.002",
        "x.e01",
        "y.01",
        "y.02",
    ] {
        touch(d, n);
    }
    assert_eq!(
        names(&discover(&d.join("a.001")).unwrap()),
        ["a.001", "a.002"]
    );
    assert_eq!(
        names(&discover(&d.join("a.img.001")).unwrap()),
        ["a.img.001", "a.img.002", "a.img.003"]
    );
    assert_eq!(names(&discover(&d.join("b.001")).unwrap()), ["b.001"]);
    assert_eq!(
        names(&discover(&d.join("a.0001")).unwrap()),
        ["a.0001", "a.0002"]
    );
    assert_eq!(
        names(&discover(&d.join("disco con espacios y ñ.001")).unwrap()),
        ["disco con espacios y ñ.001", "disco con espacios y ñ.002"]
    );
    // Nombres que no siguen la convención o no son el primer segmento.
    for bad in ["x.e01", "y.01", "a.002", "a.003x", "a.002.bak"] {
        assert!(
            matches!(
                discover(&d.join(bad)),
                Err(IoError::UnsupportedFeature { .. })
            ),
            "{bad}"
        );
    }
    // Primero inexistente.
    assert!(discover(&d.join("zz.001")).is_err());
    // Ruta de dispositivo: se rechaza léxicamente.
    assert!(matches!(
        discover(Path::new(r"\\.\PhysicalDrive0.001")),
        Err(IoError::DevicePathRejected(_))
    ));
}

#[test]
fn discover_zero_based_sets() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    touch(d, "c.000");
    touch(d, "c.001");
    touch(d, "c.002");
    assert_eq!(
        names(&discover(&d.join("c.000")).unwrap()),
        ["c.000", "c.001", "c.002"]
    );
    // Si existe c.000, no se acepta empezar por c.001 (se perdería el principio del disco).
    match discover(&d.join("c.001")) {
        Err(IoError::ExternalReference { reference, .. }) => assert_eq!(reference, "c.000"),
        other => panic!("se esperaba ExternalReference, no {other:?}"),
    }
}

#[test]
fn discover_rejects_directory_segments() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    touch(d, "e.001");
    std::fs::create_dir(d.join("e.002")).unwrap();
    touch(d, "e.003");
    assert!(matches!(
        discover(&d.join("e.001")),
        Err(IoError::DevicePathRejected(_))
    ));
    // El primero también debe ser fichero.
    std::fs::create_dir(d.join("f.001")).unwrap();
    assert!(matches!(
        discover(&d.join("f.001")),
        Err(IoError::DevicePathRejected(_))
    ));
}

#[test]
fn discover_does_not_modify_segments() {
    let dir = tempfile::tempdir().unwrap();
    let data = raw(10_000, 11);
    let paths = write_split(dir.path(), "g", &data, &[3000, 3000, 0]);
    let before = snapshot(&paths);
    assert_eq!(discover(&paths[0]).unwrap(), paths);
    assert_eq!(snapshot(&paths), before);
}

#[test]
fn discover_enforces_segment_limit() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for i in 1..=MAX_SEGMENTS {
        std::fs::write(d.join(format!("h.{i:03}")), []).unwrap();
    }
    assert_eq!(discover(&d.join("h.001")).unwrap().len(), MAX_SEGMENTS);
    // Con el número 10 001 ya hay demasiados.
    std::fs::write(d.join(format!("h.{:03}", MAX_SEGMENTS + 1)), []).unwrap();
    assert!(matches!(
        discover(&d.join("h.001")),
        Err(IoError::UnsupportedFeature { .. })
    ));
}

#[test]
fn width_grows_past_999() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for i in 1..=1001 {
        std::fs::write(d.join(format!("k.{i:03}")), []).unwrap();
    }
    let v = discover(&d.join("k.001")).unwrap();
    assert_eq!(v.len(), 1001);
    assert_eq!(names(&v[999..]), ["k.1000", "k.1001"]);
}

#[test]
fn open_with_max_segments_and_late_duplicate_is_fast() {
    // MAX_SEGMENTS segmentos de un sector: la comprobación de repetidos no debe ser cuadrática.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut paths: Vec<PathBuf> = (1..=MAX_SEGMENTS)
        .map(|i| {
            let p = d.join(format!("m.{i:03}"));
            std::fs::write(&p, [(i % 251) as u8; 512]).unwrap();
            p
        })
        .collect();
    let started = std::time::Instant::now();
    let img = SplitImage::open(&paths, 512).unwrap();
    assert_eq!(img.sector_count(), MAX_SEGMENTS as u64);
    let mut s = [0u8; 512];
    img.read_sectors(MAX_SEGMENTS as u64 - 1, 1, &mut s)
        .unwrap();
    assert_eq!(s, [(MAX_SEGMENTS % 251) as u8; 512]);
    drop(img);
    // El último se sustituye por una repetición del primero: se rechaza.
    *paths.last_mut().unwrap() = paths[0].clone();
    assert!(matches!(
        SplitImage::open(&paths, 512),
        Err(IoError::Corrupt { .. })
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
}
