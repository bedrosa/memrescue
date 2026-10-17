//! Pruebas cruzadas del lector VHD contra `qemu-img` como referencia independiente.
//!
//! Requieren qemu-img: variable de entorno `MR_QEMU_IMG` o la copia del proyecto en
//! `.tools\qemu\bin\qemu-img.exe` (relativa a la raíz del repositorio). Se ejecutan con `--ignored`.
//! Solo se usan las órdenes `convert`, `info` y `map` sobre ficheros sintéticos en un directorio
//! temporal; nunca opciones de reparación.

use mr_image::vhd::VhdImage;
use mr_io::{Allocation, BlockDevice};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

const MIB: usize = 1024 * 1024;
const BLOCK: usize = 2 * MIB;

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
    String::from_utf8(out.stdout).expect("salida de qemu-img no UTF-8")
}

/// Generador pseudoaleatorio determinista (xorshift64).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for c in buf.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            c.copy_from_slice(&v[..c.len()]);
        }
    }
}

/// RAW de 16 MiB: bloques de 2 MiB con datos o ceros, más un bloque con un único sector escrito
/// y otro medio lleno.
fn raw_16m() -> Vec<u8> {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut v = vec![0u8; 16 * MIB];
    for (i, block) in v.chunks_mut(BLOCK).enumerate() {
        match i {
            0 | 2 | 5 => rng.fill(block),
            3 => rng.fill(&mut block[..BLOCK / 2]),
            6 => rng.fill(&mut block[777 * 512..778 * 512]),
            _ => {} // 1, 4, 7: ceros
        }
    }
    v
}

/// RAW de 10 MiB + 3 sectores (no múltiplo del bloque): datos al principio, ceros en medio y
/// datos en el bloque final incompleto (incluidos los 3 últimos sectores).
fn raw_odd() -> Vec<u8> {
    let mut rng = Rng(0x0BAD_C0DE_F00D_1234);
    let len = 10 * MIB + 3 * 512;
    let mut v = vec![0u8; len];
    rng.fill(&mut v[..BLOCK + 4096]);
    rng.fill(&mut v[len - 4 * 512..]);
    v
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn fingerprint(p: &Path) -> (String, u64, SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (
        sha256(&std::fs::read(p).unwrap()),
        m.len(),
        m.modified().unwrap(),
    )
}

/// Extrae `"clave": valor` (número o booleano) de un objeto JSON plano.
fn json_value<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let i = obj.find(&pat)? + pat.len();
    let rest = obj[i..].trim_start();
    let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Sectores con datos según `qemu-img map --output=json`.
fn qemu_data_sectors(vhd: &str, sectors: usize) -> Vec<bool> {
    let out = qemu(&["map", "--output=json", "-f", "vpc", vhd]);
    let mut data = vec![false; sectors];
    let mut covered = 0usize;
    for obj in out.split('{').skip(1) {
        let start: usize = json_value(obj, "start").unwrap().parse().unwrap();
        let length: usize = json_value(obj, "length").unwrap().parse().unwrap();
        let is_data = json_value(obj, "data").unwrap() == "true";
        assert_eq!(start % 512, 0);
        assert_eq!(length % 512, 0);
        for d in &mut data[start / 512..(start + length) / 512] {
            *d = is_data;
        }
        covered += length;
    }
    assert_eq!(
        covered,
        sectors * 512,
        "qemu-img map no cubre todo el disco"
    );
    data
}

fn check_case(dir: &Path, name: &str, raw: &[u8], subformat: &str) {
    let raw_path = dir.join(format!("{name}.raw"));
    std::fs::write(&raw_path, raw).unwrap();
    let vhd_path = dir.join(format!("{name}-{subformat}.vhd"));
    let ref_path = dir.join(format!("{name}-{subformat}.ref.raw"));
    let (raw_s, vhd_s, ref_s) = (
        raw_path.to_str().unwrap(),
        vhd_path.to_str().unwrap(),
        ref_path.to_str().unwrap(),
    );
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vpc",
        "-o",
        &format!("subformat={subformat},force_size=on"),
        raw_s,
        vhd_s,
    ]);
    // Referencia independiente: qemu-img reconstruye el RAW a partir del VHD.
    qemu(&["convert", "-f", "vpc", "-O", "raw", vhd_s, ref_s]);
    let reference = std::fs::read(&ref_path).unwrap();
    let info = qemu(&["info", "--output=json", "-f", "vpc", vhd_s]);
    // La última «virtual-size» es la del nivel superior (formato vpc).
    let qemu_size: u64 = info
        .rmatch_indices("\"virtual-size\":")
        .next()
        .map(|(i, _)| json_value(&info[i..], "virtual-size").unwrap())
        .unwrap()
        .parse()
        .unwrap();
    let qemu_data = qemu_data_sectors(vhd_s, reference.len() / 512);

    let before = fingerprint(&vhd_path);
    {
        let img = VhdImage::open(File::open(&vhd_path).unwrap()).unwrap();
        let desc = img.describe();
        match subformat {
            "fixed" => assert_eq!(desc, "VHD fijo"),
            _ => assert_eq!(desc, "VHD dinámico, bloques de 2 MiB"),
        }
        assert_eq!(img.size(), reference.len() as u64, "{name}/{subformat}");
        assert_eq!(img.size(), qemu_size);
        assert_eq!(img.size(), raw.len() as u64);
        assert_eq!(img.sector_size(), 512);

        // SHA-256 de la lectura completa, en trozos que no coinciden con los bloques.
        let mut hasher = Sha256::new();
        let chunk = MIB + 3 * 512 + 7;
        let mut buf = vec![0u8; chunk];
        let mut off = 0u64;
        while off < img.size() {
            let n = chunk.min((img.size() - off) as usize);
            img.read_at(off, &mut buf[..n]).unwrap();
            hasher.update(&buf[..n]);
            off += n as u64;
        }
        let ours = hex::encode(hasher.finalize());
        assert_eq!(ours, sha256(&reference), "{name}/{subformat}: SHA-256");
        assert_eq!(
            ours,
            sha256(raw),
            "{name}/{subformat}: SHA-256 del RAW original"
        );

        // Rangos aleatorios deterministas.
        let mut rng = Rng(0xFEED_FACE_0000_0001 ^ raw.len() as u64);
        for _ in 0..300 {
            let len = 1 + (rng.next() % (300 * 1024)) as usize;
            let len = len.min(reference.len());
            let start = (rng.next() % (reference.len() - len + 1) as u64) as usize;
            let mut b = vec![0xA5u8; len];
            img.read_at(start as u64, &mut b).unwrap();
            assert!(
                b == reference[start..start + len],
                "{name}/{subformat}: rango {start}+{len}"
            );
        }
        assert!(img.read_at(img.size() - 1, &mut [0u8; 2]).is_err());

        // allocation() frente a qemu-img map: Sparse ⇔ "data": false.
        let runs = img.allocation(0, img.sector_count()).unwrap();
        let mut next = 0u64;
        let mut sparse_sectors = 0u64;
        for (lba, n, kind) in runs {
            assert_eq!(lba, next, "tramos no contiguos");
            for s in lba..lba + n {
                let sparse = kind == Allocation::Sparse;
                assert_eq!(
                    sparse, !qemu_data[s as usize],
                    "{name}/{subformat}: sector {s} {kind:?} frente a qemu-img map"
                );
                if sparse {
                    sparse_sectors += 1;
                    let o = s as usize * 512;
                    assert!(reference[o..o + 512].iter().all(|&b| b == 0));
                }
            }
            next = lba + n;
        }
        assert_eq!(next, img.sector_count());
        if subformat == "dynamic" {
            assert!(
                sparse_sectors > 0,
                "el dinámico debería tener zonas dispersas"
            );
        } else {
            assert_eq!(sparse_sectors, 0);
        }
        println!(
            "{name}/{subformat}: {desc}, {} bytes, {} sectores dispersos, sha256 {ours}",
            img.size(),
            sparse_sectors
        );
    }
    assert_eq!(
        before,
        fingerprint(&vhd_path),
        "el VHD se modificó al leerlo"
    );
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn qemu_fixed_and_dynamic_16m() {
    let dir = tempfile::tempdir().unwrap();
    let raw = raw_16m();
    check_case(dir.path(), "r16m", &raw, "fixed");
    check_case(dir.path(), "r16m", &raw, "dynamic");
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn qemu_fixed_and_dynamic_odd_size() {
    let dir = tempfile::tempdir().unwrap();
    let raw = raw_odd();
    check_case(dir.path(), "odd", &raw, "fixed");
    check_case(dir.path(), "odd", &raw, "dynamic");
}
