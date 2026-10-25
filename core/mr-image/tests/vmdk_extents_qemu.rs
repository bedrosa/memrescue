//! Pruebas cruzadas de los VMDK con descriptor separado (`monolithicFlat`, `twoGbMaxExtentFlat`,
//! `twoGbMaxExtentSparse` y descriptor que cita un único fichero disperso) contra `qemu-img`
//! (incidencia 11).
//!
//! `qemu-img` se toma de `MR_QEMU_IMG` o de `.tools\qemu\bin\qemu-img.exe`. Solo se usan
//! `convert`, `compare` y `map` sobre ficheros temporales sintéticos. Los RAW de origen de los
//! discos de más de 2 GiB se crean **dispersos** (`fsutil sparse setflag`) con unos pocos MiB de
//! datos, de modo que ocupan poco en disco; qemu-img parte las variantes twoGb* en trozos de
//! exactamente 2 GiB (4 194 304 sectores) y no tiene opción para cambiarlo.

use mr_image::external::ApprovedFiles;
use mr_image::vmdk_extents::IssueKind;
use mr_image::{list_vmdk_extents, open_vmdk_extents, OpenOptions};
use mr_io::{Allocation, BlockDevice, ImageFormat, IoError};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_QEMU: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    r"\..\..\.tools\qemu\bin\qemu-img.exe"
);
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

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
        "qemu-img {args:?} falló: {}{}",
        String::from_utf8_lossy(&out.stdout),
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

/// RAW disperso de `len` bytes con datos pseudoaleatorios en `regions` (desplazamiento, bytes).
fn sparse_raw(path: &Path, len: u64, regions: &[(u64, u64)], seed: u64) {
    File::create(path).unwrap();
    let st = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(path)
        .output()
        .expect("fsutil");
    assert!(
        st.status.success(),
        "no se pudo marcar el RAW como disperso"
    );
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_len(len).unwrap();
    let mut rng = Rng(seed);
    for &(off, n) in regions {
        let data: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        f.seek(SeekFrom::Start(off)).unwrap();
        f.write_all(&data).unwrap();
    }
    f.sync_all().unwrap();
}

/// SHA-256 de un fichero leído por trozos.
fn sha256_file(p: &Path) -> String {
    let mut f = File::open(p).unwrap();
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 * MIB as usize];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    hex::encode(h.finalize())
}

/// SHA-256 de la lectura completa del dispositivo.
fn sha256_device(d: &dyn BlockDevice) -> String {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 4 * MIB as usize];
    let mut pos = 0;
    while pos < d.size() {
        let n = (d.size() - pos).min(buf.len() as u64) as usize;
        d.read_at(pos, &mut buf[..n]).unwrap();
        h.update(&buf[..n]);
        pos += n as u64;
    }
    hex::encode(h.finalize())
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

fn push_run(runs: &mut Vec<(u64, u64, bool)>, start: u64, len: u64, data: bool) {
    match runs.last_mut() {
        Some((s0, l0, d0)) if *d0 == data && *s0 + *l0 == start => *l0 += len,
        _ => runs.push((start, len, data)),
    }
}

/// Tramos (inicio, longitud, ¿datos?) en bytes según `qemu-img map`, fusionados.
fn qemu_map(img: &Path) -> Vec<(u64, u64, bool)> {
    let out = qemu(&["map", "--output=json", "-f", "vmdk", s(img)]);
    let mut runs: Vec<(u64, u64, bool)> = Vec::new();
    for obj in out.split('}').filter(|o| o.contains("\"start\"")) {
        let (start, len) = (json_u64(obj, "start"), json_u64(obj, "length"));
        push_run(&mut runs, start, len, obj.contains("\"data\": true"));
    }
    runs
}

/// Huella de un fichero para comprobar que no cambia: SHA-256, tamaño y fecha.
fn fingerprint(p: &Path) -> (String, u64, std::time::SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (sha256_file(p), m.len(), m.modified().unwrap())
}

/// Convierte `raw` con qemu-img a VMDK `subformat`, comprueba con `qemu-img compare` que qemu
/// lo lee igual que el RAW, lista sus extensiones sin abrirlas, comprueba que sin aprobarlas
/// no se abre y lo abre aprobando **cada** fichero por su nombre declarado; compara SHA-256
/// completo y rangos aleatorios con el RAW, `allocation` con `qemu-img map` (solo dispersos) y
/// la inmutabilidad de todos los ficheros. Devuelve el número de extensiones.
fn check(dir: &Path, name: &str, raw: &Path, subformat: &str, compare_map: bool) -> usize {
    let vmdk = dir.join(format!("{name}.vmdk"));
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        &format!("subformat={subformat}"),
        s(raw),
        s(&vmdk),
    ]);
    qemu(&["compare", "-f", "raw", "-F", "vmdk", s(raw), s(&vmdk)]);
    let raw_len = std::fs::metadata(raw).unwrap().len();
    let raw_sha = sha256_file(raw);

    let plan = list_vmdk_extents(&vmdk).unwrap();
    assert!(plan.is_openable(), "{:?}", plan.first_issue());
    assert_eq!(plan.size, raw_len);
    assert_eq!(plan.create_type.as_deref(), Some(subformat));
    let names: Vec<String> = plan.required_names().map(str::to_string).collect();
    eprintln!("{name}: {} -> {names:?}", plan.summary());
    assert_eq!(names.len(), plan.extents.len());
    let files: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
    let mut all = vec![vmdk.clone()];
    all.extend(files.iter().cloned());
    let before: Vec<_> = all.iter().map(|p| fingerprint(p)).collect();

    // Sin aprobar nada no se abre ninguna extensión, aunque estén al lado.
    assert!(matches!(
        open_vmdk_extents(&vmdk, &ApprovedFiles::empty(), OpenOptions::default()),
        Err(IoError::ExternalReference { .. })
    ));
    // La apertura automática tampoco las sigue.
    assert!(matches!(
        mr_image::open_image(&vmdk, OpenOptions::default()),
        Err(IoError::ExternalReference { .. })
    ));
    // Falta la última.
    if names.len() > 1 {
        let partial = ApprovedFiles::new(
            names
                .iter()
                .zip(&files)
                .take(names.len() - 1)
                .map(|(n, p)| (n.as_str(), p)),
        )
        .unwrap();
        match open_vmdk_extents(&vmdk, &partial, OpenOptions::default()) {
            Err(IoError::ExternalReference { reference, .. }) => {
                assert_eq!(&reference, names.last().unwrap())
            }
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("se abrió sin todas las extensiones"),
        }
    }

    let approved =
        ApprovedFiles::new(names.iter().zip(&files).map(|(n, p)| (n.as_str(), p))).unwrap();
    let o = open_vmdk_extents(&vmdk, &approved, OpenOptions::default()).unwrap();
    assert_eq!(o.format, ImageFormat::VmdkDescriptor);
    assert_eq!(o.segments.len(), files.len());
    let img = o.device;
    eprintln!("{name}: {}", o.description);
    assert!(o.description.contains(subformat), "{}", o.description);
    assert_eq!(img.size(), raw_len);
    assert_eq!(sha256_device(img.as_ref()), raw_sha, "{name}: SHA-256");

    // Rangos aleatorios deterministas (y alrededor de cada frontera de extensión).
    let mut src = File::open(raw).unwrap();
    let mut offsets: Vec<u64> = Vec::new();
    let mut rng = Rng(0xe47e ^ raw_len);
    for _ in 0..200 {
        offsets.push(rng.below(img.size()));
    }
    for e in &plan.extents {
        let b = e.start_sector * 512;
        offsets.push(b.saturating_sub(700));
        offsets.push(b);
    }
    for off in offsets {
        let max = (img.size() - off).min(300_000);
        let len = 1 + rng.below(max) as usize;
        let mut a = vec![0xeeu8; len];
        img.read_at(off, &mut a).unwrap();
        let mut b = vec![0u8; len];
        src.seek(SeekFrom::Start(off)).unwrap();
        src.read_exact(&mut b).unwrap();
        assert!(a == b, "{name}: rango {off}+{len} distinto");
    }
    assert!(img.read_at(img.size() - 1, &mut [0u8; 2]).is_err());

    let ours: Vec<(u64, u64, bool)> = img
        .allocation(0, img.sector_count())
        .unwrap()
        .into_iter()
        .fold(Vec::new(), |mut acc, (lba, n, a)| {
            push_run(&mut acc, lba * 512, n * 512, a == Allocation::Data);
            acc
        });
    if compare_map {
        assert_eq!(
            ours,
            qemu_map(&vmdk),
            "{name}: mapa distinto de qemu-img map"
        );
    } else {
        // FLAT: todo son datos.
        assert_eq!(ours, vec![(0, raw_len, true)]);
    }
    drop(img);
    drop(approved);
    drop(src);
    for (p, b) in all.iter().zip(before) {
        assert_eq!(fingerprint(p), b, "{} cambió", p.display());
    }
    assert_eq!(sha256_file(raw), raw_sha);
    names.len()
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn monolithic_flat_and_small_two_gb_variants_match_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("p.raw");
    let len = 16 * MIB + 37 * 512;
    sparse_raw(
        &raw,
        len,
        &[
            (0, 3 * MIB),
            (5 * MIB + 4097, 2 * MIB),
            (len - 70_000, 70_000),
        ],
        1,
    );
    assert_eq!(check(dir.path(), "mf", &raw, "monolithicFlat", false), 1);
    assert_eq!(
        check(dir.path(), "tf", &raw, "twoGbMaxExtentFlat", false),
        1
    );
    // Descriptor separado que cita un único fichero SPARSE.
    assert_eq!(
        check(dir.path(), "ts", &raw, "twoGbMaxExtentSparse", true),
        1
    );
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn two_gb_max_extent_sparse_with_three_extents_matches_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("g.raw");
    let len = 4 * GIB + 3 * MIB + 7 * 512;
    sparse_raw(
        &raw,
        len,
        &[
            (0, MIB),
            (2 * GIB - 200_000, 400_000),
            (3 * GIB + 12345, MIB),
            (4 * GIB - 4096, 8192),
            (len - 65_536, 65_536),
        ],
        2,
    );
    assert_eq!(
        check(dir.path(), "big", &raw, "twoGbMaxExtentSparse", true),
        3
    );
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn two_gb_max_extent_flat_with_two_extents_matches_qemu() {
    // Los ficheros FLAT de qemu-img no son dispersos: unos 2 GiB en disco mientras dura la prueba.
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("h.raw");
    let len = 2 * GIB + 3 * MIB + 7 * 512;
    sparse_raw(
        &raw,
        len,
        &[
            (0, MIB),
            (2 * GIB - 200_000, 400_000),
            (len - 65_536, 65_536),
        ],
        3,
    );
    assert_eq!(
        check(dir.path(), "hf", &raw, "twoGbMaxExtentFlat", false),
        2
    );
}

/// Descriptor escrito a mano (createType monolithicSparse) que cita un monolithicSparse y otro
/// que cita un streamOptimized de qemu-img: el fichero se lee con su lector, como extensión.
#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn separate_descriptor_with_one_sparse_or_stream_extent() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("o.raw");
    let len = 8 * MIB + 3 * 512;
    sparse_raw(&raw, len, &[(MIB, 2 * MIB), (len - 4096, 4096)], 4);
    let raw_sha = sha256_file(&raw);
    for (sub, create) in [
        ("monolithicSparse", "monolithicSparse"),
        ("streamOptimized", "twoGbMaxExtentSparse"),
    ] {
        let ext = dir.path().join(format!("datos-{sub}.vmdk"));
        qemu(&[
            "convert",
            "-f",
            "raw",
            "-O",
            "vmdk",
            "-o",
            &format!("subformat={sub}"),
            s(&raw),
            s(&ext),
        ]);
        let name = ext.file_name().unwrap().to_str().unwrap().to_string();
        let desc = dir.path().join(format!("d-{sub}.vmdk"));
        std::fs::write(
            &desc,
            format!(
                "# Disk DescriptorFile\nversion=1\nCID=1\nparentCID=ffffffff\ncreateType=\"{create}\"\n\nRW {} SPARSE \"{name}\"\n",
                len / 512
            ),
        )
        .unwrap();
        let before = fingerprint(&ext);
        let ap = ApprovedFiles::new([(name.as_str(), &ext)]).unwrap();
        let o = open_vmdk_extents(&desc, &ap, OpenOptions::default()).unwrap();
        assert_eq!(sha256_device(o.device.as_ref()), raw_sha, "{sub}");
        // Tamaño declarado distinto de la capacidad de la extensión.
        std::fs::write(
            &desc,
            format!(
                "# Disk DescriptorFile\ncreateType=\"{create}\"\nRW {} SPARSE \"{name}\"\n",
                len / 512 - 1
            ),
        )
        .unwrap();
        assert!(matches!(
            open_vmdk_extents(&desc, &ap, OpenOptions::default()),
            Err(IoError::Corrupt { .. })
        ));
        drop(o);
        drop(ap);
        assert_eq!(fingerprint(&ext), before);
    }
    // Un descriptor con padre sigue siendo una referencia externa (fuera del alcance).
    let child = dir.path().join("hijo.vmdk");
    std::fs::write(
        &child,
        "# Disk DescriptorFile\nCID=2\nparentCID=1\nparentFileNameHint=\"base.vmdk\"\ncreateType=\"monolithicFlat\"\nRW 8 FLAT \"h-flat.vmdk\" 0\n",
    )
    .unwrap();
    let plan = list_vmdk_extents(&child).unwrap();
    assert_eq!(plan.first_issue().unwrap().kind, IssueKind::Parent);
    assert!(matches!(
        open_vmdk_extents(&child, &ApprovedFiles::empty(), OpenOptions::default()),
        Err(IoError::ExternalReference { .. })
    ));
    assert_eq!(sha256_file(&raw), raw_sha);
}
