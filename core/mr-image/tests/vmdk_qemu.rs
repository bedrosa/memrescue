//! Pruebas cruzadas del lector VMDK contra `qemu-img` como referencia independiente.
//!
//! `qemu-img` se toma de `MR_QEMU_IMG` o de `.tools\qemu\bin\qemu-img.exe` del repositorio
//! principal. Solo se usan `create`, `convert`, `info` y `map` sobre ficheros temporales sintéticos.

use mr_image::vmdk::VmdkImage;
use mr_io::{Allocation, BlockDevice, IoError};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_QEMU: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    r"\..\..\.tools\qemu\bin\qemu-img.exe"
);
const MIB: usize = 1024 * 1024;

fn qemu_img() -> PathBuf {
    std::env::var_os("MR_QEMU_IMG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_QEMU))
}

fn qemu(args: &[&str]) -> String {
    let out = Command::new(qemu_img())
        .args(args)
        .output()
        .expect("no se pudo ejecutar qemu-img");
    assert!(
        out.status.success(),
        "qemu-img {args:?} falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Generador pseudoaleatorio determinista (splitmix64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// RAW determinista: zonas pseudoaleatorias, zonas a cero y algunos bytes sueltos desalineados.
fn make_raw(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut v = vec![0u8; len];
    let chunk = 48 * 1024; // no alineado con el grano de 64 KiB
    for (i, c) in v.chunks_mut(chunk).enumerate() {
        if i % 5 == 0 || i % 7 == 3 {
            for b in c.iter_mut() {
                *b = rng.next() as u8;
            }
        }
    }
    for _ in 0..12 {
        let p = rng.below(len as u64) as usize;
        v[p] = 0x5a;
    }
    let last = len - 1;
    v[last] = 0xa5;
    v
}

fn sha256_file(p: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(p).unwrap()))
}

/// Tamaño virtual de nivel superior en `qemu-img info --output=json`.
fn qemu_virtual_size(img: &Path) -> u64 {
    let info = qemu(&["info", "--output=json", "-f", "vmdk", s(img)]);
    let key = "\n    \"virtual-size\": ";
    let at = info.find(key).expect("virtual-size") + key.len();
    let digits: String = info[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap()
}

fn json_u64(obj: &str, key: &str) -> u64 {
    let k = format!("\"{key}\": ");
    let at = obj.find(&k).unwrap_or_else(|| panic!("{key} en {obj}")) + k.len();
    obj[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap()
}

/// Tramos (inicio, longitud, ¿datos?) en bytes según `qemu-img map`, fusionados.
fn qemu_map(img: &Path) -> Vec<(u64, u64, bool)> {
    let out = qemu(&["map", "--output=json", "-f", "vmdk", s(img)]);
    let mut runs: Vec<(u64, u64, bool)> = Vec::new();
    for obj in out.split('}').filter(|o| o.contains("\"start\"")) {
        let (start, len) = (json_u64(obj, "start"), json_u64(obj, "length"));
        let data = obj.contains("\"data\": true");
        push_run(&mut runs, start, len, data);
    }
    runs
}

fn push_run(runs: &mut Vec<(u64, u64, bool)>, start: u64, len: u64, data: bool) {
    match runs.last_mut() {
        Some((s0, l0, d0)) if *d0 == data && *s0 + *l0 == start => *l0 += len,
        _ => runs.push((start, len, data)),
    }
}

/// Lee el dispositivo completo por bloques de 1 MiB.
fn read_all(img: &VmdkImage) -> Vec<u8> {
    let mut out = vec![0u8; img.size() as usize];
    for (i, c) in out.chunks_mut(MIB).enumerate() {
        img.read_at((i * MIB) as u64, c).unwrap();
    }
    out
}

fn check_sparse_against_qemu(dir: &Path, name: &str, raw: &[u8], extra_opts: &str) {
    let raw_path = dir.join(format!("{name}.raw"));
    std::fs::write(&raw_path, raw).unwrap();
    let vmdk = dir.join(format!("{name}.vmdk"));
    let opts = format!("subformat=monolithicSparse{extra_opts}");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        &opts,
        s(&raw_path),
        s(&vmdk),
    ]);
    let reference = dir.join(format!("{name}.ref.raw"));
    qemu(&[
        "convert",
        "-f",
        "vmdk",
        "-O",
        "raw",
        s(&vmdk),
        s(&reference),
    ]);
    let ref_bytes = std::fs::read(&reference).unwrap();
    let sha_before = sha256_file(&vmdk);
    let meta_before = std::fs::metadata(&vmdk).unwrap();

    let img = VmdkImage::open(File::open(&vmdk).unwrap()).unwrap();
    eprintln!("{name}: {} ({} bytes)", img.describe(), img.size());
    assert!(img.describe().contains("monolithicSparse"));
    assert_eq!(img.sector_size(), 512);
    // Tamaño virtual.
    assert_eq!(img.size(), qemu_virtual_size(&vmdk));
    assert_eq!(img.size(), ref_bytes.len() as u64);
    assert_eq!(img.size(), (raw.len() as u64).div_ceil(512) * 512);

    // Lectura completa: SHA-256 idéntico a la referencia de qemu-img (y al RAW original).
    let all = read_all(&img);
    let sha_all = hex::encode(Sha256::digest(&all));
    assert_eq!(sha_all, hex::encode(Sha256::digest(&ref_bytes)));
    assert_eq!(&all[..raw.len()], raw);

    // Rangos aleatorios deterministas.
    let mut rng = Rng(0x7e57_0000 ^ raw.len() as u64);
    for _ in 0..300 {
        let off = rng.below(img.size());
        let max = (img.size() - off).min(300_000);
        let len = 1 + rng.below(max) as usize;
        let mut buf = vec![0xeeu8; len];
        img.read_at(off, &mut buf).unwrap();
        assert_eq!(&buf[..], &ref_bytes[off as usize..off as usize + len]);
    }
    // Lecturas fuera de límites.
    assert!(img.read_at(img.size() - 1, &mut [0u8; 2]).is_err());

    // Asignación coherente con `qemu-img map`.
    let ours: Vec<(u64, u64, bool)> = img
        .allocation(0, img.sector_count())
        .unwrap()
        .into_iter()
        .fold(Vec::new(), |mut acc, (lba, n, a)| {
            push_run(&mut acc, lba * 512, n * 512, a == Allocation::Data);
            acc
        });
    let theirs = qemu_map(&vmdk);
    assert_eq!(
        ours, theirs,
        "{name}: mapa de asignación distinto de qemu-img map"
    );
    assert!(ours.iter().any(|r| r.2) && ours.iter().any(|r| !r.2));
    // Los tramos dispersos se leen como ceros.
    for &(start, len, data) in &ours {
        if !data {
            assert!(all[start as usize..(start + len) as usize]
                .iter()
                .all(|&b| b == 0));
        }
    }
    // Subrango arbitrario: tramos ordenados que cubren exactamente el rango.
    let sub = (img.sector_count() - 1001).min(7777);
    let runs = img.allocation(1001, sub).unwrap();
    assert_eq!(runs[0].0, 1001);
    assert_eq!(runs.iter().map(|r| r.1).sum::<u64>(), sub);
    for w in runs.windows(2) {
        assert_eq!(w[0].0 + w[0].1, w[1].0);
    }

    drop(img);
    // Ficheros intactos tras leer.
    assert_eq!(sha256_file(&vmdk), sha_before);
    let meta_after = std::fs::metadata(&vmdk).unwrap();
    assert_eq!(meta_after.len(), meta_before.len());
    assert_eq!(
        meta_after.modified().unwrap(),
        meta_before.modified().unwrap()
    );
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn monolithic_sparse_matches_qemu() {
    let dir = tempfile::tempdir().unwrap();
    // 16 MiB.
    check_sparse_against_qemu(dir.path(), "m16", &make_raw(16 * MIB, 1), "");
    // Tamaño no múltiplo del grano (5 MiB + 37 sectores).
    check_sparse_against_qemu(dir.path(), "impar", &make_raw(5 * MIB + 37 * 512, 2), "");
    // Tamaño ni siquiera múltiplo del sector (qemu-img lo redondea al sector).
    check_sparse_against_qemu(dir.path(), "suelto", &make_raw(3_000_000, 3), "");
    // Con el indicador de granos a cero (cabecera v2).
    check_sparse_against_qemu(
        dir.path(),
        "zeroed",
        &make_raw(16 * MIB, 4),
        ",zeroed_grain=on",
    );
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn stream_optimized_is_rejected_as_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("so.raw");
    std::fs::write(&raw, make_raw(4 * MIB, 5)).unwrap();
    let vmdk = dir.path().join("so.vmdk");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=streamOptimized",
        s(&raw),
        s(&vmdk),
    ]);
    let before = sha256_file(&vmdk);
    match VmdkImage::open(File::open(&vmdk).unwrap()) {
        Err(IoError::UnsupportedFeature { format, detail }) => {
            assert_eq!(format, "VMDK");
            assert!(detail.contains("streamOptimized"), "{detail}");
        }
        other => panic!("se esperaba UnsupportedFeature: {other:?}"),
    }
    assert_eq!(sha256_file(&vmdk), before);
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn monolithic_flat_descriptor_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("fl.raw");
    std::fs::write(&raw, make_raw(2 * MIB, 6)).unwrap();
    let vmdk = dir.path().join("fl.vmdk");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=monolithicFlat",
        s(&raw),
        s(&vmdk),
    ]);
    let flat = dir.path().join("fl-flat.vmdk");
    assert!(flat.exists());
    let flat_before = sha256_file(&flat);
    match VmdkImage::open(File::open(&vmdk).unwrap()) {
        Err(IoError::ExternalReference { format, reference }) => {
            assert_eq!(format, "VMDK");
            assert!(reference.contains("fl-flat.vmdk"), "{reference}");
        }
        other => panic!("se esperaba ExternalReference: {other:?}"),
    }
    // El fichero de datos plano no es una extensión dispersa.
    assert!(matches!(
        VmdkImage::open(File::open(&flat).unwrap()),
        Err(IoError::Corrupt { format: "VMDK", .. })
    ));
    assert_eq!(sha256_file(&flat), flat_before);
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn child_with_backing_file_is_external_reference() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("base.raw");
    std::fs::write(&raw, make_raw(2 * MIB, 7)).unwrap();
    let base = dir.path().join("base.vmdk");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=monolithicSparse",
        s(&raw),
        s(&base),
    ]);
    let child = dir.path().join("hijo.vmdk");
    qemu(&[
        "create",
        "-f",
        "vmdk",
        "-b",
        s(&base),
        "-F",
        "vmdk",
        s(&child),
    ]);
    match VmdkImage::open(File::open(&child).unwrap()) {
        Err(IoError::ExternalReference { format, reference }) => {
            assert_eq!(format, "VMDK");
            assert!(reference.contains("base.vmdk"), "{reference}");
        }
        other => panic!("se esperaba ExternalReference: {other:?}"),
    }
}
