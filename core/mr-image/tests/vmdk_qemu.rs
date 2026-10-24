//! Pruebas cruzadas del lector VMDK (monolithicSparse y streamOptimized) contra `qemu-img` como
//! referencia independiente.
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
    let vmdk = convert_raw(
        dir,
        name,
        raw,
        &format!("subformat=monolithicSparse{extra_opts}"),
    );
    check_vmdk_against_qemu(&vmdk, name, raw, "monolithicSparse");
}

/// Escribe `raw` y lo convierte con qemu-img a VMDK con las opciones `opts`.
fn convert_raw(dir: &Path, name: &str, raw: &[u8], opts: &str) -> PathBuf {
    let raw_path = dir.join(format!("{name}.raw"));
    std::fs::write(&raw_path, raw).unwrap();
    let vmdk = dir.join(format!("{name}.vmdk"));
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        opts,
        s(&raw_path),
        s(&vmdk),
    ]);
    vmdk
}

/// RAW de referencia de un VMDK según qemu-img.
fn qemu_to_raw(vmdk: &Path) -> Vec<u8> {
    let reference = vmdk.with_extension("ref.raw");
    qemu(&["convert", "-f", "vmdk", "-O", "raw", s(vmdk), s(&reference)]);
    std::fs::read(&reference).unwrap()
}

/// Lectura completa (SHA-256), rangos aleatorios, `allocation` frente a `qemu-img map` e
/// inmutabilidad del fichero; `raw` es el contenido original y `kind`, la variante esperada.
fn check_vmdk_against_qemu(vmdk: &Path, name: &str, raw: &[u8], kind: &str) {
    let vmdk = vmdk.to_path_buf();
    let reference = vmdk.with_extension("ref.raw");
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
    assert!(img.describe().contains(kind), "{}", img.describe());
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

/// RAW pseudoaleatorio (incompresible: cada grano comprimido mide más que el grano) salvo un hueco
/// de 256 KiB a cero a partir de 1 MiB.
fn random_raw(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    v[MIB..MIB + 256 * 1024].fill(0);
    v
}

fn convert_stream(dir: &Path, name: &str, raw: &[u8]) -> PathBuf {
    convert_raw(dir, name, raw, "subformat=streamOptimized")
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn stream_optimized_matches_qemu() {
    let dir = tempfile::tempdir().unwrap();
    for (name, raw) in [
        ("so16", make_raw(16 * MIB, 11)),
        // Tamaño no múltiplo del grano: el último grano se comprime solo hasta el final del disco.
        ("so-impar", make_raw(5 * MIB + 37 * 512, 12)),
        // Ni siquiera múltiplo del sector (qemu-img lo redondea al sector).
        ("so-suelto", make_raw(3_000_000, 13)),
        // Incompresible, con un hueco.
        ("so-azar", random_raw(4 * MIB + 512, 14)),
    ] {
        let vmdk = convert_stream(dir.path(), name, &raw);
        check_vmdk_against_qemu(&vmdk, name, &raw, "streamOptimized");
        assert!(!VmdkImage::open(File::open(&vmdk).unwrap())
            .unwrap()
            .describe()
            .contains("pie"));
    }
}

/// Campos de la cabecera (sector 0) de un VMDK.
fn header_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Granos de un streamOptimized de qemu: (grano, byte del marcador, bytes comprimidos), en orden
/// de fichero. qemu-img deja el GD y las GT al principio (`gdOffset` en la cabecera).
fn stream_grains(b: &[u8]) -> Vec<(u64, usize, usize)> {
    let capacity = header_u64(b, 12);
    let grain = header_u64(b, 20);
    let gd = header_u64(b, 56) as usize * 512;
    assert_ne!(
        header_u64(b, 56),
        u64::MAX,
        "qemu-img deja el GD en la cabecera"
    );
    let grains = capacity.div_ceil(grain);
    let mut out = Vec::new();
    for i in 0..grains.div_ceil(512) {
        let gt = le32(b, gd + 4 * i as usize) as usize * 512;
        if gt == 0 {
            continue;
        }
        for j in 0..512u64 {
            let g = i * 512 + j;
            if g >= grains {
                break;
            }
            let sector = le32(b, gt + 4 * j as usize) as usize;
            if sector != 0 {
                let at = sector * 512;
                assert_eq!(
                    header_u64(b, at),
                    g * grain,
                    "LBA del marcador del grano {g}"
                );
                out.push((g, at, le32(b, at + 8) as usize));
            }
        }
    }
    out.sort_by_key(|x| x.1);
    out
}

/// Reescribe un streamOptimized de qemu-img con la disposición de VMware: granos, marcador + GT,
/// marcador + GD, marcador + pie con el `gdOffset` real, fin de flujo; la cabecera lleva
/// `gdOffset` = GD_AT_END y sin GD redundante.
fn restream_vmware_layout(b: &[u8]) -> Vec<u8> {
    let capacity = header_u64(b, 12);
    let grain = header_u64(b, 20);
    let desc_sectors = header_u64(b, 36) as usize;
    let grains = capacity.div_ceil(grain);
    let gd_entries = grains.div_ceil(512) as usize;
    let mut f = b[..(1 + desc_sectors) * 512].to_vec();
    let mut gt = vec![0u32; gd_entries * 512];
    for (g, at, size) in stream_grains(b) {
        gt[g as usize] = (f.len() / 512) as u32;
        f.extend_from_slice(&b[at..at + 12 + size]);
        f.resize(f.len().div_ceil(512) * 512, 0);
    }
    let marker = |f: &mut Vec<u8>, sectors: u64, kind: u32| {
        let mut m = [0u8; 512];
        m[..8].copy_from_slice(&sectors.to_le_bytes());
        m[12..16].copy_from_slice(&kind.to_le_bytes());
        f.extend_from_slice(&m);
    };
    let mut gd = Vec::new();
    for i in 0..gd_entries {
        marker(&mut f, 4, 1);
        gd.push((f.len() / 512) as u32);
        for v in &gt[i * 512..(i + 1) * 512] {
            f.extend_from_slice(&v.to_le_bytes());
        }
    }
    marker(&mut f, (gd_entries * 4).div_ceil(512) as u64, 2);
    let gd_sector = (f.len() / 512) as u64;
    for v in &gd {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.resize(f.len().div_ceil(512) * 512, 0);
    let mut header = b[..512].to_vec();
    let flags = le32(&header, 8) & !(1 << 1); // sin GD redundante
    header[8..12].copy_from_slice(&flags.to_le_bytes());
    header[48..56].copy_from_slice(&0u64.to_le_bytes());
    header[64..72].copy_from_slice(&(1 + desc_sectors as u64).to_le_bytes());
    let mut footer = header.clone();
    header[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    footer[56..64].copy_from_slice(&gd_sector.to_le_bytes());
    f[..512].copy_from_slice(&header);
    marker(&mut f, 1, 3);
    f.extend_from_slice(&footer);
    f.extend_from_slice(&[0u8; 512]);
    f
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn stream_optimized_with_grain_directory_in_footer_matches_qemu() {
    let dir = tempfile::tempdir().unwrap();
    for (name, raw) in [
        ("pie16", make_raw(16 * MIB, 21)),
        ("pie-impar", make_raw(5 * MIB + 37 * 512, 22)),
    ] {
        let qemu_stream = convert_stream(dir.path(), name, &raw);
        let bytes = restream_vmware_layout(&std::fs::read(&qemu_stream).unwrap());
        let vmdk = dir.path().join(format!("{name}-vmware.vmdk"));
        std::fs::write(&vmdk, &bytes).unwrap();
        // qemu-img también lee el pie: su conversión a RAW es la referencia y debe ser el original.
        let reference = qemu_to_raw(&vmdk);
        assert_eq!(
            &reference[..raw.len()],
            &raw[..],
            "{name}: qemu-img y el original"
        );
        check_vmdk_against_qemu(&vmdk, name, &raw, "streamOptimized");
        let img = VmdkImage::open(File::open(&vmdk).unwrap()).unwrap();
        assert!(img.describe().contains("directorio de granos en el pie"));
    }
}

/// Lectura del grano `g` (o de su parte dentro del disco).
fn read_grain(img: &VmdkImage, g: u64, grain_bytes: u64) -> mr_io::Result<Vec<u8>> {
    let start = g * grain_bytes;
    let mut buf = vec![0u8; grain_bytes.min(img.size() - start) as usize];
    img.read_at(start, &mut buf).map(|()| buf)
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn stream_optimized_with_damaged_compressed_grains_never_returns_wrong_data() {
    let dir = tempfile::tempdir().unwrap();
    let raw = make_raw(8 * MIB, 31);
    let vmdk = convert_stream(dir.path(), "dañado", &raw);
    let reference = qemu_to_raw(&vmdk);
    let original = std::fs::read(&vmdk).unwrap();
    let grain_bytes = header_u64(&original, 20) * 512;
    let grains = stream_grains(&original);
    assert!(grains.len() > 20);
    let mut rng = Rng(0xDA5A_0001);
    let (mut failed, mut intact) = (0u32, 0u32);
    for it in 0..120 {
        // 1 a 3 bytes alterados dentro de los datos comprimidos de 1 a 3 granos.
        let mut bytes = original.clone();
        let mut touched = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let (g, at, size) = grains[rng.below(grains.len() as u64) as usize];
            for _ in 0..1 + rng.below(3) {
                let p = at + 12 + rng.below(size as u64) as usize;
                bytes[p] ^= 1 + rng.below(255) as u8;
            }
            touched.push(g);
        }
        let path = dir.path().join(format!("dañado-{it}.vmdk"));
        std::fs::write(&path, &bytes).unwrap();
        let sha = sha256_file(&path);
        for tolerant in [false, true] {
            // Los marcadores siguen intactos: se abre en ambos modos sin descartes.
            let img = VmdkImage::open_with(File::open(&path).unwrap(), tolerant).unwrap();
            assert_eq!(img.discarded_blocks(), 0);
            for &(g, _, _) in &grains {
                let want = &reference[(g * grain_bytes) as usize..];
                match read_grain(&img, g, grain_bytes) {
                    Ok(got) => {
                        assert!(
                            got[..] == want[..got.len()],
                            "iteración {it}: el grano {g} se leyó con datos distintos de los originales"
                        );
                        if touched.contains(&g) {
                            intact += 1;
                        }
                    }
                    Err(IoError::Read { .. }) => {
                        assert!(
                            touched.contains(&g),
                            "iteración {it}: grano {g} sano ilegible"
                        );
                        failed += 1;
                    }
                    Err(e) => panic!("iteración {it}: error inesperado {e:?}"),
                }
            }
        }
        assert_eq!(sha256_file(&path), sha, "el VMDK se modificó al leerlo");
        std::fs::remove_file(&path).unwrap();
    }
    eprintln!("granos alterados: {failed} ilegibles, {intact} leídos (idénticos al original)");
    assert!(failed > 0);
    // Comparación sector a sector con el lector tolerante en un caso concreto: el grano 0 dañado.
    let mut bytes = original.clone();
    let (_, at0, size0) = *grains.iter().find(|x| x.0 == 0).unwrap();
    bytes[at0 + 12 + size0 / 2] ^= 0x40;
    let path = dir.path().join("dañado-0.vmdk");
    std::fs::write(&path, &bytes).unwrap();
    let img = VmdkImage::open_with(File::open(&path).unwrap(), true).unwrap();
    assert_tolerant_matches(&img, &reference, (0, grain_bytes / 512));
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn stream_optimized_with_bad_marker_or_truncated_in_tolerant_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut raw = make_raw(8 * MIB, 41);
    // Los tres primeros granos (64 KiB) con datos.
    for (i, b) in raw[..3 * 64 * 1024].iter_mut().enumerate() {
        *b = (i % 249) as u8 | 1;
    }
    let vmdk = convert_stream(dir.path(), "marcador", &raw);
    let reference = qemu_to_raw(&vmdk);
    let original = std::fs::read(&vmdk).unwrap();
    let grain_sectors = header_u64(&original, 20);
    let grains = stream_grains(&original);

    // LBA del marcador del grano 1 alterado: estricto rechaza; tolerante descarta solo ese grano.
    let (_, at1, _) = *grains.iter().find(|x| x.0 == 1).unwrap();
    let mut bytes = original.clone();
    bytes[at1..at1 + 8].copy_from_slice(&(5 * grain_sectors).to_le_bytes());
    let path = dir.path().join("lba.vmdk");
    std::fs::write(&path, &bytes).unwrap();
    let sha = sha256_file(&path);
    assert!(matches!(
        VmdkImage::open(File::open(&path).unwrap()),
        Err(IoError::Corrupt { .. })
    ));
    let img = VmdkImage::open_with(File::open(&path).unwrap(), true).unwrap();
    assert_eq!(img.discarded_blocks(), 1);
    assert!(img
        .describe()
        .contains("modo tolerante: 1 grano descartado"));
    assert_tolerant_matches(&img, &reference, (grain_sectors, grain_sectors));
    drop(img);
    assert_eq!(sha256_file(&path), sha);

    // Truncado a mitad del último grano: estricto rechaza; tolerante lee el resto idéntico.
    let &(g_last, at_last, size_last) = grains.last().unwrap();
    let cut = at_last + 12 + size_last / 2;
    let path = dir.path().join("truncado.vmdk");
    std::fs::write(&path, &original[..cut]).unwrap();
    let sha = sha256_file(&path);
    assert!(matches!(
        VmdkImage::open(File::open(&path).unwrap()),
        Err(IoError::Corrupt { .. })
    ));
    let img = VmdkImage::open_with(File::open(&path).unwrap(), true).unwrap();
    assert_eq!(img.discarded_blocks(), 1);
    let span = (
        g_last * grain_sectors,
        grain_sectors.min(img.sector_count() - g_last * grain_sectors),
    );
    assert_tolerant_matches(&img, &reference, span);
    drop(img);
    assert_eq!(sha256_file(&path), sha);

    // Cortado justo tras los datos del último grano (lo que haya detrás no hace falta para leer):
    // se abre en ambos modos sin descartes y se lee entero.
    let path = dir.path().join("sin-fin.vmdk");
    let end = at_last + 12 + size_last;
    assert!(end <= original.len());
    std::fs::write(&path, &original[..end]).unwrap();
    for tolerant in [false, true] {
        let img = VmdkImage::open_with(File::open(&path).unwrap(), tolerant).unwrap();
        assert_eq!(img.discarded_blocks(), 0);
        assert!(read_all(&img) == reference);
    }
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

/// Lectura tolerante completa comparada **sector a sector** con la referencia de qemu-img: los
/// sectores de `bad` son `Unreadable` con el patrón de relleno; los demás, idénticos a la
/// referencia. La lectura estricta completa falla.
fn assert_tolerant_matches(dev: &dyn BlockDevice, reference: &[u8], bad: (u64, u64)) {
    use mr_io::resilient::{read_range, OnError, ReadPolicy};
    use mr_io::SectorState;
    let policy = ReadPolicy {
        on_error: OnError::FillAndContinue,
        ..ReadPolicy::default()
    };
    let total = dev.sector_count();
    let mut data = Vec::new();
    let mut states = Vec::new();
    read_range(
        dev,
        0,
        total,
        &policy,
        |_, d, s| {
            data.extend_from_slice(d);
            states.extend_from_slice(s);
            Ok(())
        },
        |_, _| true,
    )
    .unwrap();
    for lba in 0..total {
        let i = lba as usize;
        let got = &data[i * 512..(i + 1) * 512];
        if lba >= bad.0 && lba < bad.0 + bad.1 {
            assert_eq!(states[i], SectorState::Unreadable, "LBA {lba}");
            assert!(got.chunks(4).all(|c| c == b"BAD!"), "LBA {lba}");
        } else {
            assert!(states[i].is_trustworthy(), "LBA {lba}");
            assert!(got == &reference[i * 512..(i + 1) * 512], "LBA {lba}");
        }
    }
    let mut all = vec![0u8; dev.size() as usize];
    assert!(dev.read_at(0, &mut all).is_err());
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn monolithic_sparse_with_duplicated_grain_in_tolerant_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut raw = make_raw(8 * MIB, 0x7011_e2a7);
    // Los dos primeros granos (64 KiB por defecto) con datos.
    for (i, b) in raw[..128 * 1024].iter_mut().enumerate() {
        *b = (i % 251) as u8 | 1;
    }
    let raw_path = dir.path().join("t.raw");
    std::fs::write(&raw_path, &raw).unwrap();
    let vmdk = dir.path().join("t.vmdk");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=monolithicSparse",
        s(&raw_path),
        s(&vmdk),
    ]);
    let reference = dir.path().join("t.ref.raw");
    qemu(&[
        "convert",
        "-f",
        "vmdk",
        "-O",
        "raw",
        s(&vmdk),
        s(&reference),
    ]);
    let reference = std::fs::read(&reference).unwrap();
    // La entrada del grano 1 pasa a ser la del grano 0: ambos quedan ambiguos.
    let mut bytes = std::fs::read(&vmdk).unwrap();
    let le = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let grain_sectors = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
    let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
    let gt = le(&bytes, gd) as usize * 512;
    let (g0, g1) = (le(&bytes, gt), le(&bytes, gt + 4));
    assert!(g0 > 1 && g1 > 1, "los granos 0 y 1 deben tener datos");
    bytes[gt + 4..gt + 8].copy_from_slice(&g0.to_le_bytes());
    std::fs::write(&vmdk, &bytes).unwrap();
    let sha_before = sha256_file(&vmdk);

    assert!(matches!(
        VmdkImage::open(File::open(&vmdk).unwrap()),
        Err(IoError::Corrupt { .. })
    ));
    let img = VmdkImage::open_with(File::open(&vmdk).unwrap(), true).unwrap();
    assert_eq!(img.discarded_blocks(), 2);
    assert!(img
        .describe()
        .contains("modo tolerante: 2 granos descartados"));
    assert_tolerant_matches(&img, &reference, (0, 2 * grain_sectors));
    assert_eq!(
        img.allocation(0, 2 * grain_sectors).unwrap(),
        vec![(0, 2 * grain_sectors, Allocation::Data)]
    );
    drop(img);
    assert_eq!(
        sha256_file(&vmdk),
        sha_before,
        "el VMDK se modificó al leerlo"
    );
}
