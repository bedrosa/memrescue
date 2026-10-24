//! Pruebas de la reproducción en memoria del registro pendiente (`vhdx_log.rs`) con VHDX
//! sintéticos: entradas construidas aquí según [MS-VHDX] dentro del registro de 1 MiB que deja
//! `construir` en el desplazamiento 1 MiB.
//!
//! Modelo independiente: cada escritura del registro se aplica también, a mano, sobre una copia
//! del fichero sin registro pendiente; abrir la imagen con el registro pendiente debe leer
//! exactamente lo mismo que abrir esa copia ya «reproducida».

use super::*;

const L: u64 = MIB;
const GUID_LOG: Guid = [0xA7; 16];

#[derive(Clone)]
enum Desc {
    /// Sector de 4 KiB completo en un desplazamiento del fichero.
    Datos(u64, Vec<u8>),
    /// Tramo a ceros (desplazamiento, longitud).
    Ceros(u64, u64),
}

#[derive(Clone)]
struct Ent {
    seq: u64,
    cola: u64,
    descs: Vec<Desc>,
    volcado: u64,
    ultimo: u64,
    guid: Guid,
}

impl Ent {
    fn new(seq: u64, cola: u64, tam: u64, descs: Vec<Desc>) -> Self {
        Self {
            seq,
            cola,
            descs,
            volcado: tam,
            ultimo: tam,
            guid: GUID_LOG,
        }
    }
}

/// Bytes de una entrada del registro: cabecera, descriptores, sectores de datos y CRC-32C.
fn bytes_entrada(e: &Ent) -> Vec<u8> {
    let n = e.descs.len();
    let sd = (64 + 32 * n).div_ceil(4096);
    let n_datos = e
        .descs
        .iter()
        .filter(|d| matches!(d, Desc::Datos(..)))
        .count();
    let total = (sd + n_datos) * 4096;
    let mut b = vec![0u8; total];
    b[..4].copy_from_slice(b"loge");
    b[8..12].copy_from_slice(&(total as u32).to_le_bytes());
    b[12..16].copy_from_slice(&(e.cola as u32).to_le_bytes());
    b[16..24].copy_from_slice(&e.seq.to_le_bytes());
    b[24..28].copy_from_slice(&(n as u32).to_le_bytes());
    b[32..48].copy_from_slice(&e.guid);
    b[48..56].copy_from_slice(&e.volcado.to_le_bytes());
    b[56..64].copy_from_slice(&e.ultimo.to_le_bytes());
    let mut k = sd;
    for (i, d) in e.descs.iter().enumerate() {
        let o = 64 + 32 * i;
        match d {
            Desc::Datos(off, s) => {
                assert_eq!(s.len(), 4096);
                b[o..o + 4].copy_from_slice(b"desc");
                b[o + 4..o + 8].copy_from_slice(&s[4092..]);
                b[o + 8..o + 16].copy_from_slice(&s[..8]);
                b[o + 16..o + 24].copy_from_slice(&off.to_le_bytes());
                let p = k * 4096;
                b[p..p + 4].copy_from_slice(b"data");
                b[p + 4..p + 8].copy_from_slice(&((e.seq >> 32) as u32).to_le_bytes());
                b[p + 8..p + 4092].copy_from_slice(&s[8..4092]);
                b[p + 4092..p + 4096].copy_from_slice(&(e.seq as u32).to_le_bytes());
                k += 1;
            }
            Desc::Ceros(off, len) => {
                b[o..o + 4].copy_from_slice(b"zero");
                b[o + 8..o + 16].copy_from_slice(&len.to_le_bytes());
                b[o + 16..o + 24].copy_from_slice(&off.to_le_bytes());
            }
        }
        b[o + 24..o + 32].copy_from_slice(&e.seq.to_le_bytes());
    }
    let c = crc32c(&b);
    b[4..8].copy_from_slice(&c.to_le_bytes());
    b
}

/// Escribe `bytes` en el registro de `img` a partir de `pos`, con vuelta al principio.
fn escribir_circular(img: &mut [u8], pos: u64, bytes: &[u8]) {
    for (i, &x) in bytes.iter().enumerate() {
        img[(OFF_REGISTRO + (pos + i as u64) % L) as usize] = x;
    }
}

fn escribir(img: &mut [u8], pos: u64, e: &Ent) -> u64 {
    let b = bytes_entrada(e);
    escribir_circular(img, pos, &b);
    b.len() as u64
}

/// Pone el LogGuid en las dos cabeceras.
fn activar(img: &mut [u8], guid: Guid) {
    for off in OFF_CABECERAS {
        poner(img, off + 48, &guid);
    }
    sellar_cabeceras(img);
}

/// Modelo: aplica las escrituras de `ents`, en orden, sobre una copia del fichero.
fn aplicar(img: &[u8], ents: &[&Ent]) -> Vec<u8> {
    let mut f = img.to_vec();
    for e in ents {
        if e.ultimo as usize > f.len() {
            f.resize(e.ultimo as usize, 0);
        }
        for d in &e.descs {
            match d {
                Desc::Datos(off, s) => f[*off as usize..*off as usize + 4096].copy_from_slice(s),
                Desc::Ceros(off, len) => f[*off as usize..(off + len) as usize].fill(0),
            }
        }
    }
    f
}

fn leer_todo(v: &VhdxImage) -> Vec<u8> {
    let mut b = vec![0u8; v.size() as usize];
    v.read_at(0, &mut b).unwrap();
    b
}

/// Abre `img` (registro pendiente) y `modelo` (ya aplicado, sin registro) y compara la lectura
/// completa, el tamaño, la asignación y la inmutabilidad del fichero.
fn coincide_con_modelo(img: &[u8], modelo: &[u8], entradas: u64, caso: &str) -> VhdxImage {
    let t = fichero(img);
    let antes = crate::tolerant::check::Snapshot::of(t.path());
    let v =
        VhdxImage::open(File::open(t.path()).unwrap()).unwrap_or_else(|e| panic!("{caso}: {e}"));
    let m = abrir(modelo).unwrap_or_else(|e| panic!("{caso} (modelo): {e}"));
    assert_eq!(v.replayed_log_entries(), entradas, "{caso}");
    assert_eq!(m.replayed_log_entries(), 0);
    let fin = if entradas == 1 { "entrada" } else { "entradas" };
    assert!(
        v.describe().ends_with(&format!(
            "; registro reproducido en memoria ({entradas} {fin})"
        )),
        "{caso}: {}",
        v.describe()
    );
    assert_eq!(v.size(), m.size(), "{caso}");
    assert!(leer_todo(&v) == leer_todo(&m), "{caso}: contenido");
    let n = v.sector_count();
    assert_eq!(
        v.allocation(0, n).unwrap(),
        m.allocation(0, n).unwrap(),
        "{caso}"
    );
    // Lecturas cortas que cruzan sectores de 4 KiB del fichero.
    for (off, len) in [(4095u64, 2usize), (MIB - 1, 4098), (12345, 9000)] {
        if off + len as u64 <= v.size() {
            let (mut a, mut b) = (vec![1u8; len], vec![2u8; len]);
            v.read_at(off, &mut a).unwrap();
            m.read_at(off, &mut b).unwrap();
            assert_eq!(a, b, "{caso}: {off}+{len}");
        }
    }
    assert_eq!(
        crate::tolerant::check::Snapshot::of(t.path()),
        antes,
        "{caso}"
    );
    drop(t);
    v
}

/// Sector de 4 KiB del fichero en `off`, con la entrada de BAT del bloque `b` cambiada.
fn sector_bat_con(img: &[u8], cambios: &[(u64, u64)]) -> Vec<u8> {
    let mut s = img[OFF_BAT as usize..OFF_BAT as usize + 4096].to_vec();
    for &(b, e) in cambios {
        s[(8 * b) as usize..(8 * b + 8) as usize].copy_from_slice(&e.to_le_bytes());
    }
    s
}

fn marca(x: u8) -> Vec<u8> {
    (0..4096u32)
        .map(|i| x ^ (i as u8) ^ (i >> 8) as u8)
        .collect()
}

/// Imagen básica con 1 MiB más al final con datos del bloque 1 (aún no referenciado en la BAT)
/// y 1 MiB más con datos del bloque 2. Devuelve la imagen y el desplazamiento de esos datos.
fn base_con_bloques_nuevos() -> (Vec<u8>, u64, u64) {
    let mut img = construir(&Modelo::basico());
    let b1 = img.len() as u64;
    img.extend((0..MIB).map(|k| patron(MIB + k)));
    let b2 = img.len() as u64;
    img.extend((0..MIB).map(|k| patron(2 * MIB + k) ^ 0x5A));
    (img, b1, b2)
}

const FP: u64 = PAYLOAD_FULLY_PRESENT;

#[test]
fn reproduce_datos_y_ceros_sobre_la_bat() {
    let (base, b1, _) = base_con_bloques_nuevos();
    let tam = base.len() as u64;
    let off0 = 4 * MIB; // datos del bloque 0
    let e = Ent::new(
        1,
        0,
        tam,
        vec![
            Desc::Datos(
                OFF_BAT,
                sector_bat_con(&base, &[(1, FP | (b1 / MIB) << 20)]),
            ),
            Desc::Ceros(off0 + 4096, 8192),
        ],
    );
    let modelo = aplicar(&base, &[&e]);
    let mut img = base.clone();
    escribir(&mut img, 0, &e);
    // Sin LogGuid el registro no está pendiente y no se reproduce.
    let sin = abrir(&img).unwrap();
    assert_eq!(sin.replayed_log_entries(), 0);
    let mut b = vec![0u8; 4096];
    sin.read_at(MIB, &mut b).unwrap();
    assert!(b.iter().all(|&x| x == 0));
    activar(&mut img, GUID_LOG);
    let v = coincide_con_modelo(&img, &modelo, 1, "datos y ceros");
    // El bloque 1 aparece y el bloque 0 tiene 8 KiB a ceros.
    v.read_at(MIB, &mut b).unwrap();
    assert!(b
        .iter()
        .enumerate()
        .all(|(i, &x)| x == patron(MIB + i as u64)));
    let mut c = vec![0xFFu8; 8192];
    v.read_at(4096, &mut c).unwrap();
    assert!(c.iter().all(|&x| x == 0));
    v.read_at(0, &mut b).unwrap();
    assert!(b.iter().enumerate().all(|(i, &x)| x == patron(i as u64)));
    let spb = MIB / 512;
    assert_eq!(
        v.allocation(0, 3 * spb).unwrap(),
        vec![
            (0, 2 * spb, Allocation::Data),
            (2 * spb, spb, Allocation::Sparse)
        ]
    );
    // En modo tolerante, igual.
    let t = fichero(&img);
    let vt = VhdxImage::open_with(File::open(t.path()).unwrap(), true).unwrap();
    assert!(leer_todo(&vt) == leer_todo(&v));
    assert!(vt.describe().contains(
        "; registro reproducido en memoria (1 entrada); modo tolerante: 0 bloques descartados"
    ));
}

#[test]
fn reproduce_cambios_de_metadatos() {
    // El registro cambia el tamaño virtual (3,5 MiB → 3 MiB) y el sector lógico no.
    let base = construir(&Modelo::basico());
    let off_tam = item_meta(&base, 1);
    let sector = off_tam / 4096 * 4096;
    let mut s = base[sector as usize..sector as usize + 4096].to_vec();
    let rel = (off_tam - sector) as usize;
    s[rel..rel + 8].copy_from_slice(&(3 * MIB).to_le_bytes());
    let e = Ent::new(9, 4096, base.len() as u64, vec![Desc::Datos(sector, s)]);
    let modelo = aplicar(&base, &[&e]);
    let mut img = base.clone();
    escribir(&mut img, 4096, &e);
    activar(&mut img, GUID_LOG);
    let v = coincide_con_modelo(&img, &modelo, 1, "metadatos");
    assert_eq!(v.size(), 3 * MIB);
}

#[test]
fn varias_entradas_con_vuelta_al_principio_del_registro() {
    let (base, b1, b2) = base_con_bloques_nuevos();
    let tam = base.len() as u64;
    let off3 = 5 * MIB; // datos del bloque 3
                        // e1 (12 KiB antes del final): BAT con el bloque 1 y un sector del bloque 3.
    let e1 = Ent::new(
        0x1_0000_0007,
        L - 12288,
        tam,
        vec![
            Desc::Datos(
                OFF_BAT,
                sector_bat_con(&base, &[(1, FP | (b1 / MIB) << 20)]),
            ),
            Desc::Datos(off3 + 8192, marca(0x11)),
        ],
    );
    let p1 = L - 16384;
    let n1 = bytes_entrada(&e1).len() as u64;
    assert_eq!(n1, 12288);
    // e2 cruza el final del registro: su sector de datos queda al principio.
    let e2 = Ent::new(
        0x1_0000_0008,
        p1,
        tam,
        vec![Desc::Datos(off3 + 4096, marca(0x22))],
    );
    let p2 = p1 + n1; // L - 4096
    let n2 = bytes_entrada(&e2).len() as u64;
    // e3: vuelve a escribir la BAT (bloques 1 y 2), pone a ceros lo que escribió e2 y vuelve a
    // escribir el sector de e1 en el bloque 3 (prevalece la escritura posterior).
    let e3 = Ent::new(
        0x1_0000_0009,
        p1,
        tam,
        vec![
            Desc::Ceros(off3, 3 * 4096),
            Desc::Datos(
                OFF_BAT,
                sector_bat_con(
                    &base,
                    &[(1, FP | (b1 / MIB) << 20), (2, FP | (b2 / MIB) << 20)],
                ),
            ),
            Desc::Datos(off3 + 8192, marca(0x33)),
        ],
    );
    let p3 = (p2 + n2) % L;
    assert_eq!(p3, 4096);
    let modelo = aplicar(&base, &[&e1, &e2, &e3]);
    let mut img = base.clone();
    escribir(&mut img, p1, &e1);
    escribir(&mut img, p2, &e2);
    escribir(&mut img, p3, &e3);
    activar(&mut img, GUID_LOG);
    let v = coincide_con_modelo(&img, &modelo, 3, "vuelta al principio");
    let mut b = vec![0u8; 3 * 4096];
    v.read_at(3 * MIB, &mut b).unwrap();
    assert!(b[..8192].iter().all(|&x| x == 0));
    assert_eq!(&b[8192..], &marca(0x33)[..]);
}

#[test]
fn ceros_posteriores_anulan_datos_anteriores_del_mismo_sector() {
    let base = construir(&Modelo::basico());
    let tam = base.len() as u64;
    let off0 = 4 * MIB;
    let e1 = Ent::new(3, 0, tam, vec![Desc::Datos(off0, marca(0x44))]);
    let e2 = Ent::new(
        4,
        0,
        tam,
        vec![
            Desc::Ceros(off0, 4096),
            Desc::Datos(off0 + 4096, marca(0x55)),
        ],
    );
    // Dentro de la misma entrada, el orden de los descriptores también cuenta.
    let e3 = Ent::new(
        5,
        0,
        tam,
        vec![
            Desc::Datos(off0 + 8192, marca(0x66)),
            Desc::Ceros(off0 + 8192, 4096),
        ],
    );
    let modelo = aplicar(&base, &[&e1, &e2, &e3]);
    let mut img = base.clone();
    let n1 = escribir(&mut img, 0, &e1);
    let n2 = escribir(&mut img, n1, &e2);
    escribir(&mut img, n1 + n2, &e3);
    activar(&mut img, GUID_LOG);
    let v = coincide_con_modelo(&img, &modelo, 3, "ceros posteriores");
    let mut b = vec![0u8; 3 * 4096];
    v.read_at(0, &mut b).unwrap();
    assert!(b[..4096].iter().all(|&x| x == 0));
    assert_eq!(&b[4096..8192], &marca(0x55)[..]);
    assert!(b[8192..].iter().all(|&x| x == 0));
}

#[test]
fn secuencia_mas_reciente_incompleta_y_entradas_ajenas_se_ignoran() {
    let (base, b1, b2) = base_con_bloques_nuevos();
    let tam = base.len() as u64;
    let e1 = Ent::new(
        10,
        0,
        tam,
        vec![Desc::Datos(
            OFF_BAT,
            sector_bat_con(&base, &[(1, FP | (b1 / MIB) << 20)]),
        )],
    );
    let e2 = Ent::new(11, 0, tam, vec![Desc::Ceros(4 * MIB, 4096)]);
    let modelo = aplicar(&base, &[&e1, &e2]);
    let mut img = base.clone();
    let n1 = escribir(&mut img, 0, &e1);
    let n2 = escribir(&mut img, n1, &e2);
    // e3 (secuencia 12) quedó a medias: CRC incorrecto. No se reproduce.
    let e3 = Ent::new(
        12,
        0,
        tam,
        vec![Desc::Datos(
            OFF_BAT,
            sector_bat_con(&base, &[(2, FP | (b2 / MIB) << 20)]),
        )],
    );
    let n3 = escribir(&mut img, n1 + n2, &e3);
    img[(OFF_REGISTRO + n1 + n2 + n3 - 100) as usize] ^= 1;
    // Una secuencia con números mayores cuyo Tail señala una entrada que no está: incompleta.
    let huerfana = Ent::new(40, 256 * KIB, tam, vec![Desc::Ceros(4 * MIB, MIB)]);
    escribir(&mut img, 512 * KIB, &huerfana);
    let siguiente = Ent::new(41, 256 * KIB, tam, vec![Desc::Ceros(5 * MIB, MIB)]);
    escribir(&mut img, 512 * KIB + 4096, &siguiente);
    // Entradas de otro registro (otro LogGuid) válidas por lo demás.
    let mut ajena = Ent::new(99, 768 * KIB, tam, vec![Desc::Ceros(4 * MIB, MIB)]);
    ajena.guid = [0x5C; 16];
    escribir(&mut img, 768 * KIB, &ajena);
    activar(&mut img, GUID_LOG);
    coincide_con_modelo(&img, &modelo, 2, "secuencias ignoradas");
}

#[test]
fn la_cola_de_la_cabeza_limita_lo_que_se_reproduce() {
    let base = construir(&Modelo::basico());
    let tam = base.len() as u64;
    let e1 = Ent::new(5, 0, tam, vec![Desc::Datos(4 * MIB, marca(0x77))]);
    let mut img = base.clone();
    let n1 = escribir(&mut img, 0, &e1);
    // La cabeza dice que e1 ya se aplicó: solo se reproduce ella.
    let e2 = Ent::new(6, n1, tam, vec![Desc::Ceros(5 * MIB, 4096)]);
    escribir(&mut img, n1, &e2);
    let modelo = aplicar(&base, &[&e2]);
    activar(&mut img, GUID_LOG);
    coincide_con_modelo(&img, &modelo, 1, "cola");
}

#[test]
fn saltos_y_discrepancias_de_secuencia_cortan_la_secuencia() {
    let base = construir(&Modelo::basico());
    let tam = base.len() as u64;
    let e1 = Ent::new(5, 0, tam, vec![Desc::Ceros(4 * MIB, 4096)]);
    let modelo = aplicar(&base, &[&e1]);
    // (caso, entrada siguiente, alteración de sus bytes)
    type Alterar = fn(&mut Vec<u8>);
    let casos: [(&str, u64, Alterar); 4] = [
        ("salto de número de secuencia", 7, |_| {}),
        ("SequenceLow del sector de datos", 6, |b| {
            b[4096 + 4092] ^= 1
        }),
        ("SequenceHigh del sector de datos", 6, |b| b[4096 + 4] ^= 1),
        ("número de secuencia del descriptor", 6, |b| {
            b[64 + 24] ^= 1
        }),
    ];
    for (caso, seq, alterar) in casos {
        let mut img = base.clone();
        let n1 = escribir(&mut img, 0, &e1);
        let e2 = Ent::new(seq, 0, tam, vec![Desc::Datos(5 * MIB, marca(9))]);
        let mut b = bytes_entrada(&e2);
        alterar(&mut b);
        // CRC recalculado: la entrada solo falla por la secuencia.
        b[4..8].fill(0);
        let c = crc32c(&b);
        b[4..8].copy_from_slice(&c.to_le_bytes());
        escribir_circular(&mut img, n1, &b);
        activar(&mut img, GUID_LOG);
        coincide_con_modelo(&img, &modelo, 1, caso);
    }
}

#[test]
fn registro_no_valido_es_corrupto_en_ambos_modos() {
    let (base, b1, _) = base_con_bloques_nuevos();
    let tam = base.len() as u64;
    let bat = sector_bat_con(&base, &[(1, FP | (b1 / MIB) << 20)]);
    let buena = Ent::new(1, 0, tam, vec![Desc::Datos(OFF_BAT, bat.clone())]);
    let con = |f: &dyn Fn(&mut Ent)| {
        let mut e = buena.clone();
        f(&mut e);
        e
    };
    let mut casos: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut img = base.clone();
    let n = escribir(&mut img, 0, &buena);
    img[(OFF_REGISTRO + n - 1) as usize] ^= 0x80;
    activar(&mut img, GUID_LOG);
    casos.push(("CRC de la única entrada", img));
    let mut img = base.clone();
    escribir(&mut img, 0, &buena);
    activar(&mut img, [0x3C; 16]);
    casos.push(("LogGuid distinto del de la cabecera", img));
    let entradas: Vec<(&str, Ent)> = vec![
        ("Tail fuera de la secuencia", con(&|e| e.cola = 8192)),
        (
            "FlushedFileOffset mayor que el fichero",
            con(&|e| e.volcado = tam + MIB),
        ),
        (
            "FlushedFileOffset no alineado",
            con(&|e| e.volcado = tam - 4096),
        ),
        (
            "LastFileOffset no alineado",
            con(&|e| e.ultimo = tam + 4096),
        ),
        (
            "escritura en las cabeceras",
            con(&|e| e.descs = vec![Desc::Datos(128 * KIB, bat.clone())]),
        ),
        (
            "escritura en el identificador",
            con(&|e| e.descs = vec![Desc::Ceros(0, 4096)]),
        ),
        (
            "escritura en el registro",
            con(&|e| e.descs = vec![Desc::Ceros(OFF_REGISTRO + L - 4096, 8192)]),
        ),
        (
            "desplazamiento no alineado",
            con(&|e| e.descs = vec![Desc::Datos(OFF_BAT + 512, bat.clone())]),
        ),
        (
            "longitud de ceros no alineada",
            con(&|e| e.descs = vec![Desc::Ceros(4 * MIB, 1000)]),
        ),
        (
            "escritura más allá del tamaño efectivo",
            con(&|e| e.descs = vec![Desc::Datos(tam, bat.clone())]),
        ),
        (
            "ceros desbordados",
            con(&|e| e.descs = vec![Desc::Ceros(4 * MIB, u64::MAX - 4095)]),
        ),
    ];
    for (caso, e) in &entradas {
        let mut img = base.clone();
        escribir(&mut img, 0, e);
        activar(&mut img, GUID_LOG);
        casos.push((caso, img));
    }
    // Más descriptores de los que caben en la entrada.
    let mut img = base.clone();
    let mut b = bytes_entrada(&buena);
    b[24..28].copy_from_slice(&1000u32.to_le_bytes());
    b[4..8].fill(0);
    let c = crc32c(&b);
    b[4..8].copy_from_slice(&c.to_le_bytes());
    escribir_circular(&mut img, 0, &b);
    activar(&mut img, GUID_LOG);
    casos.push(("demasiados descriptores", img));
    // Longitud de entrada no múltiplo de 4 KiB.
    let mut img = base.clone();
    let mut b = bytes_entrada(&buena);
    b[8..12].copy_from_slice(&8000u32.to_le_bytes());
    b[4..8].fill(0);
    let c = crc32c(&b);
    b[4..8].copy_from_slice(&c.to_le_bytes());
    escribir_circular(&mut img, 0, &b);
    activar(&mut img, GUID_LOG);
    casos.push(("longitud de entrada", img));
    // Registro vacío con LogGuid.
    let mut img = base.clone();
    activar(&mut img, GUID_LOG);
    casos.push(("registro vacío", img));
    // LogGuid sin región de registro.
    let mut img = base.clone();
    for off in OFF_CABECERAS {
        poner(&mut img, off + 68, &0u32.to_le_bytes());
    }
    activar(&mut img, GUID_LOG);
    casos.push(("LogLength 0", img));
    // Fichero truncado por debajo de FlushedFileOffset.
    let mut img = base.clone();
    escribir(&mut img, 0, &buena);
    activar(&mut img, GUID_LOG);
    img.truncate((tam - MIB) as usize);
    casos.push(("fichero truncado", img));

    for (caso, img) in casos {
        for tolerante in [false, true] {
            let (t, r) = abrir_modo(&img, tolerante);
            assert!(
                matches!(&r, Err(IoError::Corrupt { format: "VHDX", detail }) if detail.contains("registro")),
                "{caso} (tolerante: {tolerante}): {r:?}"
            );
            assert!(std::fs::read(t.path()).unwrap() == img, "{caso}");
        }
    }
}

#[test]
fn last_file_offset_amplia_el_fichero_con_ceros() {
    let base = construir(&Modelo::basico());
    let tam = base.len() as u64;
    // El bloque 1 pasa a estar al final del fichero, que aún no existe en el disco: según
    // LastFileOffset el fichero mide 1 MiB más (ceros), y el registro escribe un sector allí.
    let e = Ent {
        ultimo: tam + MIB,
        ..Ent::new(
            1,
            0,
            tam,
            vec![
                Desc::Datos(
                    OFF_BAT,
                    sector_bat_con(&base, &[(1, FP | (tam / MIB) << 20)]),
                ),
                Desc::Datos(tam + 4096, marca(0x99)),
            ],
        )
    };
    let modelo = aplicar(&base, &[&e]);
    assert_eq!(modelo.len() as u64, tam + MIB);
    let mut img = base.clone();
    escribir(&mut img, 0, &e);
    activar(&mut img, GUID_LOG);
    let v = coincide_con_modelo(&img, &modelo, 1, "LastFileOffset");
    let mut b = vec![0u8; 3 * 4096];
    v.read_at(MIB, &mut b).unwrap();
    assert!(b[..4096].iter().all(|&x| x == 0));
    assert_eq!(&b[4096..8192], &marca(0x99)[..]);
    assert!(b[8192..].iter().all(|&x| x == 0));
    // Con LastFileOffset igual al fichero, el bloque queda fuera: la escritura no es válida.
    let e = Ent { ultimo: tam, ..e };
    let mut img = base.clone();
    escribir(&mut img, 0, &e);
    activar(&mut img, GUID_LOG);
    assert!(es_corrupto(abrir(&img)));
}

#[test]
fn modo_tolerante_con_registro_y_bat_danada() {
    let (base, b1, _) = base_con_bloques_nuevos();
    let tam = base.len() as u64;
    // El registro deja el bloque 1 presente y el 3 con un estado no válido.
    let e = Ent::new(
        1,
        0,
        tam,
        vec![Desc::Datos(
            OFF_BAT,
            sector_bat_con(&base, &[(1, FP | (b1 / MIB) << 20), (3, 5)]),
        )],
    );
    let mut img = base.clone();
    escribir(&mut img, 0, &e);
    activar(&mut img, GUID_LOG);
    assert!(es_corrupto(abrir_modo(&img, false).1));
    let (_t, v) = abrir_modo(&img, true);
    let v = v.unwrap();
    assert_eq!(v.discarded_blocks(), 1);
    assert_eq!(v.replayed_log_entries(), 1);
    let mut esp = esperado(&Modelo::basico());
    for k in 0..MIB {
        esp[(MIB + k) as usize] = patron(MIB + k);
    }
    let spb = MIB / 512;
    crate::tolerant::check::tolerant_read_matches(&v, &esp, &[(3 * spb, spb / 2)], "tolerante");
}
