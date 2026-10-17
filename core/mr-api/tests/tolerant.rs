//! Modo tolerante de los contenedores (ADR-020) a través de la API: apertura explícita,
//! descripción, verificación con mapa de validez, hash estricto que falla e inmutabilidad.

use mr_api::task::TaskContext;
use mr_api::{hash_image, HashOptions, OpenOptions, VerifyOptions};
use mr_io::resilient::{OnError, ReadPolicy};
use mr_io::SectorState;
use std::path::Path;

/// Suma de control del VHD: complemento a uno de la suma de los bytes, sin el propio campo.
fn seal(b: &mut [u8], field: usize) {
    let sum = b
        .iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |a, (_, &x)| a.wrapping_add(u32::from(x)));
    b[field..field + 4].copy_from_slice(&(!sum).to_be_bytes());
}

/// VHD dinámico según la especificación pública: 4 bloques de 4 KiB (0, 1 y 2 asignados, 3 sin
/// asignar) con la entrada de la BAT del bloque 1 apuntando fuera del fichero. Devuelve el fichero
/// y el contenido virtual esperado de los bloques sanos (el bloque 1 queda a cero en la referencia).
fn damaged_vhd() -> (Vec<u8>, Vec<u8>) {
    const BS: usize = 4096;
    let size = 4 * BS as u64;
    let mut footer = vec![0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&512u64.to_be_bytes());
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[60..64].copy_from_slice(&3u32.to_be_bytes());
    seal(&mut footer, 64);
    let mut header = vec![0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    header[16..24].copy_from_slice(&1536u64.to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&4u32.to_be_bytes());
    header[32..36].copy_from_slice(&(BS as u32).to_be_bytes());
    seal(&mut header, 36);
    let mut bat = vec![0xFFu8; 512];
    let mut body = Vec::new();
    let mut expect = vec![0u8; size as usize];
    for b in 0..3usize {
        let sector = ((2048 + body.len()) / 512) as u32;
        let entry = if b == 1 { 0x00FF_0000 } else { sector };
        bat[b * 4..b * 4 + 4].copy_from_slice(&entry.to_be_bytes());
        let mut bitmap = vec![0u8; 512];
        bitmap[0] = 0xFF;
        body.extend_from_slice(&bitmap);
        let data: Vec<u8> = (0..BS)
            .map(|i| (i as u8).wrapping_add(b as u8 * 7) | 1)
            .collect();
        if b != 1 {
            expect[b * BS..(b + 1) * BS].copy_from_slice(&data);
        }
        body.extend_from_slice(&data);
    }
    let mut img = footer.clone();
    img.extend_from_slice(&header);
    img.extend_from_slice(&bat);
    img.extend_from_slice(&body);
    img.extend_from_slice(&footer);
    (img, expect)
}

fn snapshot(p: &Path) -> (Vec<u8>, u64, std::time::SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (std::fs::read(p).unwrap(), m.len(), m.modified().unwrap())
}

#[test]
fn tolerant_open_is_explicit_and_damaged_blocks_are_untrusted() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("dañado.vhd");
    let (bytes, expect) = damaged_vhd();
    std::fs::write(&p, &bytes).unwrap();
    let before = snapshot(&p);

    // Por defecto (y con las opciones por defecto): estricto, se rechaza.
    assert!(!OpenOptions::default().tolerant);
    match mr_api::open_source(&p, 0) {
        Err(mr_api::ApiError::Io(mr_io::IoError::Corrupt { format: "VHD", .. })) => {}
        Err(e) => panic!("se esperaba Corrupt, no {e}"),
        Ok(_) => panic!("el modo estricto debía rechazar la imagen"),
    }

    let src = mr_api::open_with(
        &p,
        OpenOptions {
            tolerant: true,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let d = &src.info().description;
    assert!(
        d.contains("modo tolerante: 1 bloque descartado"),
        "descripción: {d}"
    );

    // Hash estricto: falla (nunca se calcula sobre bytes sustituidos).
    assert!(hash_image(
        src.as_ref(),
        &HashOptions::default(),
        &TaskContext::detached()
    )
    .is_err());

    // Verificación: los 8 sectores del bloque 1 son no fiables; el resto, idéntico a la referencia.
    let opts = VerifyOptions {
        policy: ReadPolicy {
            on_error: OnError::FillAndContinue,
            ..ReadPolicy::default()
        },
        ..VerifyOptions::default()
    };
    let r = mr_api::start_verify_task(src.clone(), opts)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(r.map.count(SectorState::Unreadable), 8);
    assert_eq!(
        r.map.untrusted_runs().collect::<Vec<_>>(),
        vec![(8, 8, SectorState::Unreadable)]
    );
    let (data, states) = mr_api::read_sectors_mapped(src.as_ref(), 0, 32, 0).unwrap();
    for (l, st) in states.iter().enumerate() {
        let got = &data[l * 512..(l + 1) * 512];
        if (8..16).contains(&l) {
            assert_eq!(*st, SectorState::Unreadable, "LBA {l}");
            assert!(got.chunks(4).all(|c| c == b"BAD!"), "LBA {l}");
        } else {
            assert!(st.is_trustworthy(), "LBA {l}");
            assert_eq!(got, &expect[l * 512..(l + 1) * 512], "LBA {l}");
        }
    }
    // Lectura estricta del bloque dañado: error.
    assert!(mr_api::read_sectors(src.as_ref(), 8, 1).is_err());
    drop(src);
    assert_eq!(snapshot(&p), before, "el origen no debe cambiar");
}
