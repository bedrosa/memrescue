//! Reproducción del registro pendiente de VHDX contra `qemu-img` como referencia independiente.
//!
//! qemu reproduce el registro al abrir un VHDX en lectura y escritura y se niega a abrirlo en
//! solo lectura si encuentra un registro pendiente válido («opened read-only, but contains a log
//! that needs to be replayed»). Se aprovechan las dos cosas:
//! - `qemu-img info` sobre la imagen con registro pendiente debe fallar con ese mensaje: qemu
//!   también considera válido el registro.
//! - `qemu-img convert --image-opts driver=vhdx,file.filename=COPIA,read-only=off -O raw` sobre
//!   una **copia desechable** hace que qemu reproduzca el registro en la copia y convierta el
//!   resultado: es el contenido de referencia. El VHDX que lee MemRescue nunca se toca (se
//!   comprueba su SHA-256, tamaño y fecha).
//!
//! Casos: entradas escritas por el propio qemu (al convertir un RAW a VHDX dinámico qemu actualiza
//! la BAT a través del registro) vueltas a dejar pendientes, y entradas sintéticas construidas
//! aquí según [MS-VHDX] sobre un VHDX de qemu (descriptores de datos y de ceros, varias entradas,
//! vuelta al principio del registro, entrada posterior a medio escribir).
//!
//! Ejecución: `cargo test -p mr-image --test vhdx_log_qemu -- --ignored`.

use mr_image::crc32c::crc32c;
use mr_image::vhdx::VhdxImage;
use mr_io::BlockDevice;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MIB: u64 = 1024 * 1024;
const RUTA_LOCAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    r"\..\..\.tools\qemu\bin\qemu-img.exe"
);
const GUID_BAT: [u8; 16] = [
    0x66, 0x77, 0xC2, 0x2D, 0x23, 0xF6, 0x00, 0x42, 0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08,
];
const AVISO_QEMU: &str = "log that needs to be replayed";

fn qemu_img() -> PathBuf {
    let p = std::env::var_os("MR_QEMU_IMG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(RUTA_LOCAL));
    assert!(p.is_file(), "no se encuentra qemu-img en {}", p.display());
    p
}

fn qemu_salida(args: &[&str]) -> Output {
    Command::new(qemu_img())
        .args(args)
        .output()
        .expect("no se pudo ejecutar qemu-img")
}

fn qemu(args: &[&str]) {
    let out = qemu_salida(args);
    assert!(
        out.status.success(),
        "qemu-img {args:?} falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sha256(d: &[u8]) -> String {
    hex::encode(Sha256::digest(d))
}

fn huella(p: &Path) -> (String, u64, std::time::SystemTime) {
    let md = std::fs::metadata(p).unwrap();
    (
        sha256(&std::fs::read(p).unwrap()),
        md.len(),
        md.modified().unwrap(),
    )
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn sellar(b: &mut [u8], o: usize, len: usize) {
    b[o + 4..o + 8].fill(0);
    let c = crc32c(&b[o..o + len]);
    b[o + 4..o + 8].copy_from_slice(&c.to_le_bytes());
}

/// RAW de 64 MiB con datos pseudoaleatorios deterministas en varios tramos.
fn crear_raw(ruta: &Path) -> Vec<u8> {
    let mut d = vec![0u8; 64 * MIB as usize];
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for (ini, len) in [
        (0, 3 * MIB),
        (10 * MIB, MIB),
        (17 * MIB + 4096, 3 * MIB),
        (63 * MIB, MIB),
    ] {
        for b in &mut d[ini as usize..(ini + len) as usize] {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            *b = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }
    std::fs::write(ruta, &d).unwrap();
    d
}

/// Disposición de un VHDX de qemu: cabecera activa, registro y BAT.
struct Disposicion {
    cabecera: usize,
    off_registro: u64,
    long_registro: u64,
    off_bat: u64,
}

fn disposicion(img: &[u8]) -> Disposicion {
    let (h0, h1) = (64 * 1024usize, 128 * 1024usize);
    let cabecera = if le64(img, h0 + 8) > le64(img, h1 + 8) {
        h0
    } else {
        h1
    };
    let regiones = 192 * 1024usize;
    let n = le32(img, regiones + 8) as usize;
    let off_bat = (0..n)
        .map(|i| regiones + 16 + 32 * i)
        .find(|&e| img[e..e + 16] == GUID_BAT)
        .map(|e| le64(img, e + 16))
        .expect("región BAT");
    Disposicion {
        cabecera,
        off_registro: le64(img, cabecera + 72),
        long_registro: u64::from(le32(img, cabecera + 68)),
        off_bat,
    }
}

/// Deja pendiente el registro `guid` en la cabecera activa.
fn activar(img: &mut [u8], d: &Disposicion, guid: &[u8; 16]) {
    img[d.cabecera + 48..d.cabecera + 64].copy_from_slice(guid);
    sellar(img, d.cabecera, 4096);
}

/// Lee el disco virtual completo con MemRescue.
fn leer_memrescue(vhdx: &Path) -> (Vec<u8>, String, u64) {
    let img = VhdxImage::open(File::open(vhdx).unwrap()).unwrap();
    let mut b = vec![0u8; img.size() as usize];
    // En trozos no alineados con los sectores de 4 KiB del registro.
    let trozo = 3 * MIB as usize + 512;
    for (i, c) in b.chunks_mut(trozo).enumerate() {
        img.read_at((i * trozo) as u64, c).unwrap();
    }
    (b, img.describe(), img.replayed_log_entries())
}

/// Compara la lectura de MemRescue del VHDX con registro pendiente `pendiente` con la referencia
/// de qemu (que reproduce el registro en una copia desechable). Devuelve la lectura.
fn contrastar(dir: &Path, nombre: &str, pendiente: &[u8], entradas: u64) -> Vec<u8> {
    let vhdx = dir.join(format!("{nombre}.vhdx"));
    std::fs::write(&vhdx, pendiente).unwrap();
    let antes = huella(&vhdx);

    // qemu ve el registro pendiente y no abre en solo lectura.
    let info = qemu_salida(&["info", "-f", "vhdx", vhdx.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&info.stderr);
    assert!(
        !info.status.success() && err.contains(AVISO_QEMU),
        "{nombre}: qemu-img info debería exigir reproducir el registro: {err}"
    );
    assert_eq!(
        huella(&vhdx),
        antes,
        "{nombre}: qemu-img info cambió el VHDX"
    );

    let (leido, desc, n) = leer_memrescue(&vhdx);
    assert_eq!(n, entradas, "{nombre}: {desc}");
    let fin = if entradas == 1 { "entrada" } else { "entradas" };
    assert!(
        desc.ends_with(&format!(
            "; registro reproducido en memoria ({entradas} {fin})"
        )),
        "{nombre}: {desc}"
    );
    assert_eq!(huella(&vhdx), antes, "{nombre}: el VHDX cambió");

    // Referencia: qemu reproduce el registro en una copia y la convierte a RAW.
    let copia = dir.join(format!("{nombre}.copia.vhdx"));
    std::fs::write(&copia, pendiente).unwrap();
    let raw = dir.join(format!("{nombre}.qemu.raw"));
    let opts = format!(
        "driver=vhdx,file.filename={},read-only=off",
        copia.to_str().unwrap()
    );
    qemu(&[
        "convert",
        "--image-opts",
        &opts,
        "-O",
        "raw",
        raw.to_str().unwrap(),
    ]);
    let referencia = std::fs::read(&raw).unwrap();
    assert_ne!(
        sha256(&std::fs::read(&copia).unwrap()),
        antes.0,
        "{nombre}: qemu debía reproducir el registro en la copia"
    );
    assert_eq!(leido.len(), referencia.len(), "{nombre}: tamaño virtual");
    assert_eq!(
        sha256(&leido),
        sha256(&referencia),
        "{nombre}: la lectura no coincide con qemu tras reproducir el registro"
    );
    println!(
        "{nombre}: {desc}; SHA-256 {} igual que qemu",
        &sha256(&leido)[..16]
    );
    assert_eq!(huella(&vhdx), antes, "{nombre}: el VHDX cambió");
    leido
}

fn vhdx_de_qemu(dir: &Path, opciones: &str) -> (Vec<u8>, Vec<u8>) {
    let raw = dir.join("base.raw");
    let datos = crear_raw(&raw);
    let vhdx = dir.join("base.vhdx");
    qemu(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vhdx",
        "-o",
        opciones,
        raw.to_str().unwrap(),
        vhdx.to_str().unwrap(),
    ]);
    (std::fs::read(&vhdx).unwrap(), datos)
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn registro_escrito_por_qemu_vuelto_a_dejar_pendiente() {
    let dir = tempfile::tempdir().unwrap();
    for opciones in ["subformat=dynamic", "subformat=dynamic,block_size=1M"] {
        let (base, datos) = vhdx_de_qemu(dir.path(), opciones);
        let d = disposicion(&base);
        assert_eq!(base[d.cabecera + 48..d.cabecera + 64], [0u8; 16]);
        // Entrada de qemu con el mayor número de secuencia.
        let reg = &base[d.off_registro as usize..(d.off_registro + d.long_registro) as usize];
        let ultima = (0..reg.len())
            .step_by(4096)
            .filter(|&p| &reg[p..p + 4] == b"loge")
            .max_by_key(|&p| le64(reg, p + 16))
            .expect("qemu escribe entradas en el registro al convertir a VHDX dinámico");
        assert_eq!(le32(reg, ultima + 12) as usize, ultima, "Tail de qemu");
        let guid: [u8; 16] = reg[ultima + 32..ultima + 48].try_into().unwrap();
        let mut img = base.clone();
        activar(&mut img, &d, &guid);
        // Idempotente: lo que escribe el registro ya está en el fichero.
        let leido = contrastar(dir.path(), "qemu_idempotente", &img, 1);
        assert!(leido == datos);
        // No idempotente: los sectores que escribe la entrada se ponen a ceros en el fichero
        // (la BAT queda vacía); solo reproduciendo el registro se recupera el contenido.
        for i in 0..le32(reg, ultima + 24) as usize {
            let o = ultima + 64 + 32 * i;
            assert_eq!(&reg[o..o + 4], b"desc");
            let destino = le64(reg, o + 16) as usize;
            img[destino..destino + 4096].fill(0);
        }
        let mut sin_registro = img.clone();
        sin_registro[d.cabecera + 48..d.cabecera + 64].fill(0);
        sellar(&mut sin_registro, d.cabecera, 4096);
        let p = dir.path().join("sin_registro.vhdx");
        std::fs::write(&p, &sin_registro).unwrap();
        assert!(
            leer_memrescue(&p).0 != datos,
            "la entrada debe ser necesaria"
        );
        let leido = contrastar(dir.path(), "qemu_revertido", &img, 1);
        assert!(leido == datos, "{opciones}");
    }
}

enum Desc {
    Datos(u64, Vec<u8>),
    Ceros(u64, u64),
}

/// Entrada del registro según [MS-VHDX] con LogGuid `guid`.
fn entrada(guid: &[u8; 16], seq: u64, cola: u64, tam: u64, descs: &[Desc]) -> Vec<u8> {
    let sd = (64 + 32 * descs.len()).div_ceil(4096);
    let nd = descs
        .iter()
        .filter(|d| matches!(d, Desc::Datos(..)))
        .count();
    let len = (sd + nd) * 4096;
    let mut e = vec![0u8; len];
    e[..4].copy_from_slice(b"loge");
    e[8..12].copy_from_slice(&(len as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(cola as u32).to_le_bytes());
    e[16..24].copy_from_slice(&seq.to_le_bytes());
    e[24..28].copy_from_slice(&(descs.len() as u32).to_le_bytes());
    e[32..48].copy_from_slice(guid);
    e[48..56].copy_from_slice(&tam.to_le_bytes());
    e[56..64].copy_from_slice(&tam.to_le_bytes());
    let mut k = sd * 4096;
    for (i, d) in descs.iter().enumerate() {
        let o = 64 + 32 * i;
        match d {
            Desc::Datos(at, s) => {
                e[o..o + 4].copy_from_slice(b"desc");
                e[o + 4..o + 8].copy_from_slice(&s[4092..]);
                e[o + 8..o + 16].copy_from_slice(&s[..8]);
                e[o + 16..o + 24].copy_from_slice(&at.to_le_bytes());
                e[k..k + 4].copy_from_slice(b"data");
                e[k + 4..k + 8].copy_from_slice(&((seq >> 32) as u32).to_le_bytes());
                e[k + 8..k + 4092].copy_from_slice(&s[8..4092]);
                e[k + 4092..k + 4096].copy_from_slice(&(seq as u32).to_le_bytes());
                k += 4096;
            }
            Desc::Ceros(at, n) => {
                e[o..o + 4].copy_from_slice(b"zero");
                e[o + 8..o + 16].copy_from_slice(&n.to_le_bytes());
                e[o + 16..o + 24].copy_from_slice(&at.to_le_bytes());
            }
        }
        e[o + 24..o + 32].copy_from_slice(&seq.to_le_bytes());
    }
    let c = crc32c(&e);
    e[4..8].copy_from_slice(&c.to_le_bytes());
    e
}

/// Escribe `e` en el registro en `pos`, con vuelta al principio.
fn escribir(img: &mut [u8], d: &Disposicion, pos: u64, e: &[u8]) -> u64 {
    for (i, &x) in e.iter().enumerate() {
        img[(d.off_registro + (pos + i as u64) % d.long_registro) as usize] = x;
    }
    (pos + e.len() as u64) % d.long_registro
}

fn marca(x: u8) -> Vec<u8> {
    (0..4096u32).map(|i| x ^ (i as u8).rotate_left(3)).collect()
}

#[test]
#[ignore = "requiere qemu-img; lo ejecuta scripts/test.ps1"]
fn registros_sinteticos_contra_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let (base, datos) = vhdx_de_qemu(dir.path(), "subformat=dynamic,block_size=1M");
    let d = disposicion(&base);
    let tam = base.len() as u64;
    assert_eq!(tam % MIB, 0);
    let guid = [0x6B; 16];
    let l = d.long_registro;
    // Bloques presentes de qemu (bloques de 1 MiB, sin entradas de mapa de bits en 64 MiB).
    let entrada_bat = |b: u64| le64(&base, (d.off_bat + 8 * b) as usize);
    let off_bloque = |b: u64| {
        let e = entrada_bat(b);
        assert_eq!(e & 7, 6, "bloque {b} presente");
        (e >> 20) * MIB
    };
    let (b0, b10, b18) = (off_bloque(0), off_bloque(10), off_bloque(18));
    let sector_bat = base[d.off_bat as usize..d.off_bat as usize + 4096].to_vec();
    let mut bat_sin_10 = sector_bat.clone();
    bat_sin_10[80..88].fill(0); // bloque 10 → NOT_PRESENT

    // 1. Datos y ceros en una entrada: sectores de los bloques 0 y 18, y la BAT.
    let e = entrada(
        &guid,
        1,
        0,
        tam,
        &[
            Desc::Datos(b0 + 4096, marca(0x31)),
            Desc::Ceros(b18 + 64 * 1024, 128 * 1024),
            Desc::Datos(d.off_bat, bat_sin_10.clone()),
        ],
    );
    let mut img = base.clone();
    escribir(&mut img, &d, 0, &e);
    activar(&mut img, &d, &guid);
    let leido = contrastar(dir.path(), "datos_y_ceros", &img, 1);
    assert_eq!(&leido[4096..8192], &marca(0x31)[..]);
    assert!(leido[(10 * MIB) as usize..(11 * MIB) as usize]
        .iter()
        .all(|&x| x == 0));
    assert!(leido != datos);

    // 2. Tres entradas con vuelta al principio del registro; la última vuelve a escribir un
    //    sector de la primera y pone a ceros el de la segunda.
    let p1 = l - 3 * 4096;
    let e1 = entrada(
        &guid,
        0x77,
        p1,
        tam,
        &[Desc::Datos(b10 + 8192, marca(0x41))],
    );
    let e2 = entrada(
        &guid,
        0x78,
        p1,
        tam,
        &[Desc::Datos(b10 + 3 * 4096, marca(0x42))],
    );
    let e3 = entrada(
        &guid,
        0x79,
        p1,
        tam,
        &[
            Desc::Datos(b10 + 8192, marca(0x43)),
            Desc::Ceros(b10 + 3 * 4096, 4096),
            Desc::Datos(d.off_bat, bat_sin_10.clone()),
        ],
    );
    let mut img = base.clone();
    let p2 = escribir(&mut img, &d, p1, &e1);
    let p3 = escribir(&mut img, &d, p2, &e2);
    assert_eq!(p3, 4096, "la segunda entrada cruza el final del registro");
    escribir(&mut img, &d, p3, &e3);
    activar(&mut img, &d, &guid);
    let leido = contrastar(dir.path(), "vuelta", &img, 3);
    assert!(leido[(10 * MIB) as usize..(11 * MIB) as usize]
        .iter()
        .all(|&x| x == 0));

    // Lo mismo sin quitar el bloque 10: se ven las escrituras de la última entrada.
    let e3 = entrada(
        &guid,
        0x79,
        p1,
        tam,
        &[
            Desc::Datos(b10 + 8192, marca(0x43)),
            Desc::Ceros(b10 + 3 * 4096, 4096),
        ],
    );
    let mut img = base.clone();
    let p2 = escribir(&mut img, &d, p1, &e1);
    let p3 = escribir(&mut img, &d, p2, &e2);
    let p4 = escribir(&mut img, &d, p3, &e3);
    activar(&mut img, &d, &guid);
    let pendiente_vuelta = img.clone();
    let leido = contrastar(dir.path(), "vuelta_datos", &img, 3);
    let v10 = (10 * MIB) as usize;
    assert_eq!(&leido[v10 + 8192..v10 + 12288], &marca(0x43)[..]);
    assert!(leido[v10 + 12288..v10 + 16384].iter().all(|&x| x == 0));

    // 3. Entrada posterior a medio escribir (CRC incorrecto): se ignora.
    let mut img = pendiente_vuelta;
    let e4 = entrada(&guid, 0x7A, p1, tam, &[Desc::Datos(d.off_bat, bat_sin_10)]);
    let fin = escribir(&mut img, &d, p4, &e4);
    let ultimo = (d.off_registro + (fin + l - 1) % l) as usize;
    img[ultimo] ^= 0x10;
    let leido2 = contrastar(dir.path(), "entrada_a_medias", &img, 3);
    assert!(leido2 == leido);
}
