//! Cadenas de discos diferenciales VHD y VHDX (incidencia 8) con `qemu-img` como referencia
//! independiente del disco base.
//!
//! qemu-img 11.1.2 no crea ni lee diferenciales VHD/VHDX («Backing file not supported for file
//! format 'vpc'/'vhdx'»), así que no puede convertir la cadena entera. La prueba separa las capas:
//! - el **disco base** lo escribe qemu-img (`convert` de un RAW a VHD/VHDX dinámico) y su
//!   contenido de referencia es la conversión de vuelta a RAW, también con qemu-img;
//! - los **diferenciales** (1 y 2 niveles) se construyen según las especificaciones con el
//!   constructor de pruebas (`src/diff_builder.rs`), enlazados al id único (VHD) o al
//!   DataWriteGuid (VHDX) que qemu-img escribió en el base; sus sectores escritos se conocen;
//! - la imagen esperada es la referencia de qemu-img con las escrituras de cada capa encima. Se
//!   comparan el SHA-256 de la lectura completa de la cadena, 300 rangos aleatorios y el mapa de
//!   asignación (sectores escritos por una capa = datos; el resto, lo que diga `qemu-img map` del
//!   base).
//!
//! Solo se usan `convert`, `info` y `map` sobre copias desechables en un directorio temporal;
//! nunca `commit`, `rebase` ni opciones de reparación. Se ejecutan con `--ignored` y
//! `MR_QEMU_IMG` (o la copia del proyecto en `.tools\qemu\bin\qemu-img.exe`).

#[path = "../src/diff_builder.rs"]
mod diff_builder;

use diff_builder::{self as db, Writes, SECTOR};
use mr_image::{open_image_with_parents, parent_role, ApprovedFiles, OpenOptions};
use mr_io::{Allocation, BlockDevice};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 8 * MIB;

fn qemu_img() -> PathBuf {
    let p = std::env::var_os("MR_QEMU_IMG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                r"\..\..\.tools\qemu\bin\qemu-img.exe"
            ))
        });
    assert!(p.is_file(), "no se encuentra qemu-img en {}", p.display());
    p
}

fn qemu(args: &[&str]) -> std::process::Output {
    Command::new(qemu_img())
        .args(args)
        .output()
        .expect("no se pudo ejecutar qemu-img")
}

fn qemu_ok(args: &[&str]) -> String {
    let out = qemu(args);
    assert!(
        out.status.success(),
        "qemu-img {args:?} falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// RAW de 8 MiB: datos pseudoaleatorios en los primeros 3 MiB y en un tramo del final; ceros en
/// medio (bloques dispersos en el base).
fn raw() -> Vec<u8> {
    let mut r = Rng(0x00C0_FFEE_D00D_F00D);
    let mut v = vec![0u8; SIZE as usize];
    for c in v[..3 * MIB as usize].chunks_mut(8) {
        c.copy_from_slice(&r.next().to_le_bytes()[..c.len()]);
    }
    for c in v[(7 * MIB) as usize..(7 * MIB + 4096) as usize].chunks_mut(8) {
        c.copy_from_slice(&r.next().to_le_bytes()[..c.len()]);
    }
    v
}

fn sha(d: &[u8]) -> String {
    hex::encode(Sha256::digest(d))
}

fn fingerprint(p: &Path) -> (String, u64, SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (
        sha(&std::fs::read(p).unwrap()),
        m.len(),
        m.modified().unwrap(),
    )
}

fn json_value<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let i = obj.find(&pat)? + pat.len();
    let rest = obj[i..].trim_start();
    let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Sectores con datos del base según `qemu-img map --output=json`.
fn qemu_data(fmt: &str, img: &Path) -> Vec<bool> {
    let out = qemu_ok(&["map", "--output=json", "-f", fmt, img.to_str().unwrap()]);
    let mut data = vec![false; (SIZE / SECTOR) as usize];
    for obj in out.split('{').skip(1) {
        let s: u64 = json_value(obj, "start").unwrap().parse().unwrap();
        let l: u64 = json_value(obj, "length").unwrap().parse().unwrap();
        let is_data = json_value(obj, "data").unwrap() == "true";
        for d in &mut data[(s / SECTOR) as usize..((s + l) / SECTOR) as usize] {
            *d = is_data;
        }
    }
    data
}

fn read_all(d: &dyn BlockDevice) -> Vec<u8> {
    // Lectura en trozos irregulares (cruzan bloques de todas las capas).
    let mut v = vec![0u8; d.size() as usize];
    let mut pos = 0usize;
    let mut r = Rng(0x1357_9BDF_2468_ACE1);
    while pos < v.len() {
        let n = ((r.next() % (3 * MIB)) as usize + 1).min(v.len() - pos);
        d.read_at(pos as u64, &mut v[pos..pos + n]).unwrap();
        pos += n;
    }
    v
}

fn check_chain(
    top: &Path,
    parents: &[&PathBuf],
    expect: &[u8],
    layers: &[&Writes],
    base_data: &[bool],
) {
    let e = ApprovedFiles::new(
        parents
            .iter()
            .enumerate()
            .map(|(i, p)| (parent_role(i + 1), *p)),
    )
    .unwrap();
    let o = open_image_with_parents(top, OpenOptions::default(), &e, false).unwrap();
    let d = o.device.as_ref();
    assert_eq!(d.size(), SIZE);
    assert_eq!(sha(&read_all(d)), sha(expect), "SHA-256 de la cadena");
    let mut r = Rng(0xDEAD_BEEF_0BAD_F00D);
    for _ in 0..300 {
        let start = r.next() % SIZE;
        let len = (r.next() % 70_000).clamp(1, SIZE - start);
        let mut b = vec![0u8; len as usize];
        d.read_at(start, &mut b).unwrap();
        assert!(
            b == expect[start as usize..(start + len) as usize],
            "rango {start}+{len}"
        );
    }
    let mut lba = 0u64;
    for (l, n, k) in d.allocation(0, d.sector_count()).unwrap() {
        assert_eq!(l, lba);
        for s in l..l + n {
            let want = layers.iter().any(|w| w.contains_key(&s)) || base_data[s as usize];
            assert_eq!(k == Allocation::Data, want, "asignación del sector {s}");
        }
        lba += n;
    }
    assert_eq!(lba, SIZE / SECTOR);
}

/// Escrituras de los diferenciales: sectores sueltos, tramos que cruzan bloques del hijo y del
/// base, zonas dispersas en el base y el último sector.
fn child_writes() -> (Writes, Writes) {
    let last = SIZE / SECTOR - 1;
    let w1 = db::writes(
        1,
        [0, 1, 7, 4095, 4096, 5000, last]
            .into_iter()
            .chain(2040..2200)
            .chain(8192..8192 + 2048),
    );
    let w2 = db::writes(2, [1, 2, 4096, 9000, 12_000, last - 1]);
    (w1, w2)
}

fn run(fmt: &str, ext: &str) {
    let dir = tempfile::tempdir().unwrap();
    let raw_data = raw();
    let raw_p = dir.path().join("orig.raw");
    std::fs::write(&raw_p, &raw_data).unwrap();
    let base = dir.path().join(format!("base.{ext}"));
    let opts = if fmt == "vpc" {
        "subformat=dynamic,force_size=on"
    } else {
        "subformat=dynamic"
    };
    qemu_ok(&[
        "convert",
        "-f",
        "raw",
        "-O",
        fmt,
        "-o",
        opts,
        raw_p.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    let reference = dir.path().join("base.ref.raw");
    qemu_ok(&[
        "convert",
        "-f",
        fmt,
        "-O",
        "raw",
        base.to_str().unwrap(),
        reference.to_str().unwrap(),
    ]);
    let ref_data = std::fs::read(&reference).unwrap();
    assert_eq!(sha(&ref_data), sha(&raw_data));
    let base_bytes = std::fs::read(&base).unwrap();
    let base_data = qemu_data(fmt, &base);

    let (w1, w2) = child_writes();
    let (child, grand) = if fmt == "vpc" {
        let id = db::vhd_unique_id(&base_bytes);
        let c = db::vhd_layer(
            SIZE,
            64 * 1024,
            [0xC1; 16],
            Some(&db::VhdParent::new(id, "base.vhd")),
            &w1,
        );
        let g = db::vhd_layer(
            SIZE,
            2 * MIB as u32,
            [0xC2; 16],
            Some(&db::VhdParent::new([0xC1; 16], "child.vhd")),
            &w2,
        );
        (c, g)
    } else {
        let crc = mr_image::crc32c::crc32c;
        let dwg = db::vhdx_data_write_guid(&base_bytes);
        let c = db::vhdx_layer(
            SIZE,
            MIB as u32,
            [0xC1; 16],
            Some(&db::VhdxParent::new(dwg, "base.vhdx")),
            &w1,
            &[],
            crc,
        );
        let g = db::vhdx_layer(
            SIZE,
            2 * MIB as u32,
            [0xC2; 16],
            Some(&db::VhdxParent::new([0xC1; 16], "child.vhdx")),
            &w2,
            &[],
            crc,
        );
        (c, g)
    };
    let child_p = dir.path().join(format!("child.{ext}"));
    let grand_p = dir.path().join(format!("grand.{ext}"));
    std::fs::write(&child_p, &child).unwrap();
    std::fs::write(&grand_p, &grand).unwrap();
    // qemu-img no sabe leer los diferenciales de este formato (no es una referencia para ellos).
    let info = qemu(&["info", "-f", fmt, child_p.to_str().unwrap()]);
    eprintln!(
        "qemu-img info del diferencial {ext}: correcto={} {}",
        info.status.success(),
        String::from_utf8_lossy(&info.stderr).trim()
    );

    let before: Vec<_> = [&base, &child_p, &grand_p]
        .iter()
        .map(|p| fingerprint(p))
        .collect();
    let mut expect1 = ref_data.clone();
    db::apply(&mut expect1, &w1);
    check_chain(&child_p, &[&base], &expect1, &[&w1], &base_data);
    let mut expect2 = expect1.clone();
    db::apply(&mut expect2, &w2);
    check_chain(
        &grand_p,
        &[&child_p, &base],
        &expect2,
        &[&w1, &w2],
        &base_data,
    );
    eprintln!(
        "{ext}: 1 nivel SHA-256 {}; 2 niveles SHA-256 {}",
        sha(&expect1),
        sha(&expect2)
    );
    let after: Vec<_> = [&base, &child_p, &grand_p]
        .iter()
        .map(|p| fingerprint(p))
        .collect();
    assert_eq!(before, after, "los ficheros no deben cambiar");
}

#[test]
#[ignore = "requiere qemu-img (MR_QEMU_IMG)"]
fn qemu_vhd_base_with_one_and_two_differencing_levels() {
    run("vpc", "vhd");
}

#[test]
#[ignore = "requiere qemu-img (MR_QEMU_IMG)"]
fn qemu_vhdx_base_with_one_and_two_differencing_levels() {
    run("vhdx", "vhdx");
}
