//! Pruebas cruzadas del lector VHDX contra `qemu-img` como referencia independiente.
//!
//! Requiere `qemu-img` (ruta en `MR_QEMU_IMG` o la copia local de `.tools`). Solo se usan los
//! subcomandos `create`, `convert`, `info` y `map`; nunca `check -r`. Todo ocurre en un directorio
//! temporal con imágenes sintéticas.
//!
//! Ejecución: `cargo test -p mr-image --test vhdx_qemu -- --ignored`.

use mr_image::vhdx::VhdxImage;
use mr_io::{Allocation, BlockDevice};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

const MIB: u64 = 1024 * 1024;
const RUTA_LOCAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    r"\..\..\.tools\qemu\bin\qemu-img.exe"
);

fn qemu_img() -> PathBuf {
    let p = std::env::var_os("MR_QEMU_IMG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(RUTA_LOCAL));
    assert!(p.is_file(), "no se encuentra qemu-img en {}", p.display());
    p
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

/// Generador xorshift64* determinista.
struct Prng(u64);

impl Prng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// RAW de `tam` bytes con datos pseudoaleatorios en los rangos `(inicio, longitud)` y ceros fuera.
fn crear_raw(ruta: &Path, tam: u64, rangos: &[(u64, u64)], semilla: u64) -> Vec<u8> {
    let mut datos = vec![0u8; tam as usize];
    let mut g = Prng(semilla);
    for &(ini, len) in rangos {
        for b in &mut datos[ini as usize..(ini + len) as usize] {
            *b = g.next() as u8;
        }
    }
    std::fs::write(ruta, &datos).unwrap();
    datos
}

fn sha256(d: &[u8]) -> String {
    hex::encode(Sha256::digest(d))
}

fn huella_fichero(p: &Path) -> (String, u64, std::time::SystemTime) {
    let md = std::fs::metadata(p).unwrap();
    (
        sha256(&std::fs::read(p).unwrap()),
        md.len(),
        md.modified().unwrap(),
    )
}

/// Extrae un campo numérico o booleano de un objeto JSON plano de qemu-img.
fn campo<'a>(obj: &'a str, clave: &str) -> Option<&'a str> {
    let k = format!("\"{clave}\"");
    let i = obj.find(&k)? + k.len();
    let resto = obj[i..].trim_start().strip_prefix(':')?.trim_start();
    let fin = resto
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(resto.len());
    Some(&resto[..fin])
}

/// Tramos `(inicio, longitud, data)` de `qemu-img map --output=json`.
fn mapa_qemu(img: &Path) -> Vec<(u64, u64, bool)> {
    let salida = qemu(&["map", "--output=json", "-f", "vhdx", img.to_str().unwrap()]);
    salida
        .split('{')
        .skip(1)
        .map(|obj| {
            (
                campo(obj, "start").unwrap().parse().unwrap(),
                campo(obj, "length").unwrap().parse().unwrap(),
                campo(obj, "data").unwrap() == "true",
            )
        })
        .collect()
}

struct Variante {
    nombre: &'static str,
    opciones: &'static str,
    fijo: bool,
    /// `true` si todos los bloques deben quedar FULLY_PRESENT (fijo sin estado ZERO).
    todo_datos: bool,
}

fn caso(dir: &Path, raw: &Path, datos: &[u8], var: &Variante) {
    let Variante {
        nombre,
        opciones,
        fijo,
        todo_datos,
    } = *var;
    let vhdx = dir.join(format!("{nombre}.vhdx"));
    let referencia = dir.join(format!("{nombre}.ref.raw"));
    let (r, v, rf) = (
        raw.to_str().unwrap(),
        vhdx.to_str().unwrap(),
        referencia.to_str().unwrap(),
    );
    qemu(&["convert", "-f", "raw", "-O", "vhdx", "-o", opciones, r, v]);
    qemu(&["convert", "-f", "vhdx", "-O", "raw", v, rf]);
    let ref_datos = std::fs::read(&referencia).unwrap();
    assert!(
        ref_datos == datos,
        "{nombre}: la referencia de qemu difiere del RAW"
    );
    let info = qemu(&["info", "--output=json", "-f", "vhdx", v]);
    // El último «virtual-size» es el del VHDX (los anteriores son del fichero subyacente).
    let ultimo = info.rfind("\"virtual-size\"").unwrap();
    let tam_qemu: u64 = campo(&info[ultimo..], "virtual-size")
        .unwrap()
        .parse()
        .unwrap();
    let mapa = mapa_qemu(&vhdx);

    let antes = huella_fichero(&vhdx);
    let img = VhdxImage::open(File::open(&vhdx).unwrap()).unwrap();
    let desc = img.describe();
    println!("{nombre} ({opciones}): {desc}; fichero {} bytes", antes.1);
    assert_eq!(img.size(), tam_qemu, "{nombre}: tamaño virtual");
    assert_eq!(img.size(), datos.len() as u64);
    assert_eq!(img.sector_size(), 512);
    assert_eq!(
        img.is_fixed(),
        fijo,
        "{nombre}: variante ({desc}) no coincide"
    );

    // Lectura completa en trozos de 3 MiB + 512 (no alineados con los bloques).
    let mut h = Sha256::new();
    let mut leido = Vec::with_capacity(datos.len());
    let trozo = 3 * MIB as usize + 512;
    let mut pos = 0u64;
    while pos < img.size() {
        let n = trozo.min((img.size() - pos) as usize);
        let mut b = vec![0u8; n];
        img.read_at(pos, &mut b).unwrap();
        h.update(&b);
        leido.extend_from_slice(&b);
        pos += n as u64;
    }
    assert_eq!(
        hex::encode(h.finalize()),
        sha256(&ref_datos),
        "{nombre}: SHA-256 de la lectura completa"
    );

    // Rangos aleatorios deterministas.
    let mut g = Prng(0xC0FF_EE00 ^ datos.len() as u64);
    for _ in 0..300 {
        let off = g.next() % img.size();
        let len = ((g.next() % (2 * MIB)) as usize).min((img.size() - off) as usize);
        let mut b = vec![0xA5u8; len];
        img.read_at(off, &mut b).unwrap();
        assert!(
            b[..] == datos[off as usize..off as usize + len],
            "{nombre}: rango {off}+{len}"
        );
    }
    assert!(img.read_at(img.size(), &mut [0u8; 1]).is_err());

    // Asignación coherente con qemu-img map: lo que qemu no marca como datos debe ser disperso,
    // lo que nosotros marcamos como datos debe serlo también para qemu, y lo disperso es cero.
    let total = img.sector_count();
    let tramos = img.allocation(0, total).unwrap();
    let mut s = 0;
    let mut bytes_datos = 0u64;
    for &(lba, n, a) in &tramos {
        assert_eq!(lba, s, "{nombre}: tramos no contiguos");
        s += n;
        let (ini, fin) = (lba * 512, (lba + n) * 512);
        for &(qs, ql, qdata) in &mapa {
            let (a0, a1) = (qs.max(ini), (qs + ql).min(fin));
            if a0 < a1 && !qdata {
                assert_eq!(
                    a,
                    Allocation::Sparse,
                    "{nombre}: qemu dice sin datos en {a0}..{a1}"
                );
            }
            if a0 < a1 && a == Allocation::Data {
                assert!(qdata, "{nombre}: datos en {a0}..{a1} que qemu no ve");
            }
        }
        match a {
            Allocation::Sparse => assert!(
                datos[ini as usize..fin as usize].iter().all(|&x| x == 0),
                "{nombre}: tramo disperso con datos"
            ),
            Allocation::Data => bytes_datos += fin - ini,
        }
    }
    assert_eq!(s, total, "{nombre}: los tramos no cubren el disco");
    if todo_datos {
        assert_eq!(tramos, vec![(0, total, Allocation::Data)]);
    }
    assert!(
        bytes_datos <= antes.1,
        "{nombre}: más datos asignados que bytes en el fichero"
    );
    // Rango parcial en mitad del disco.
    let parcial = img.allocation(total / 3, total / 3).unwrap();
    assert_eq!(parcial.first().unwrap().0, total / 3);
    assert_eq!(parcial.iter().map(|t| t.1).sum::<u64>(), total / 3);
    println!(
        "  {} tramos, {} MiB con datos",
        tramos.len(),
        bytes_datos / MIB
    );

    drop(img);
    assert_eq!(huella_fichero(&vhdx), antes, "{nombre}: el VHDX cambió");
    assert!(leido == datos);
}

const fn v(nombre: &'static str, opciones: &'static str, fijo: bool, todo: bool) -> Variante {
    Variante {
        nombre,
        opciones,
        fijo,
        todo_datos: todo,
    }
}

/// Variantes que ofrece `qemu-img create -f vhdx -o help` (no hay opción de sector lógico: qemu
/// siempre usa 512 B). Con `block_state_zero=on` (predeterminado) qemu marca como ZERO los bloques
/// nulos incluso en discos fijos; con `off` (solo admisible en fijos) todos quedan FULLY_PRESENT.
const VARIANTES: [Variante; 7] = [
    v("fijo", "subformat=fixed", true, false),
    v("fijo_1m", "subformat=fixed,block_size=1M", true, false),
    v(
        "fijo_sin_zero",
        "subformat=fixed,block_state_zero=off",
        true,
        true,
    ),
    v("dinamico", "subformat=dynamic", false, false),
    v(
        "dinamico_1m",
        "subformat=dynamic,block_size=1M",
        false,
        false,
    ),
    v(
        "dinamico_8m",
        "subformat=dynamic,block_size=8M",
        false,
        false,
    ),
    v(
        "dinamico_256m",
        "subformat=dynamic,block_size=256M",
        false,
        false,
    ),
];

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn vhdx_64_mib_contra_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("base64.raw");
    let rangos = [
        (0, 3 * MIB),
        (10 * MIB, MIB),
        (17 * MIB + 4096, 3 * MIB),
        (40 * MIB + 512, 512),
        (63 * MIB, MIB),
    ];
    let datos = crear_raw(&raw, 64 * MIB, &rangos, 0x1234_5678_9ABC_DEF1);
    let antes = huella_fichero(&raw);
    for var in &VARIANTES {
        caso(dir.path(), &raw, &datos, var);
    }
    assert_eq!(huella_fichero(&raw), antes);
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn vhdx_tamano_no_multiplo_del_bloque_contra_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("impar.raw");
    let tam = 37 * MIB + 7 * 512;
    let rangos = [(MIB, 2 * MIB), (33 * MIB, 3 * MIB), (tam - 3000, 3000)];
    let datos = crear_raw(&raw, tam, &rangos, 0x0BAD_CAFE_F00D_0001);
    for var in &VARIANTES {
        caso(dir.path(), &raw, &datos, var);
    }
}
