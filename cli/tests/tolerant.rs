//! Opción `--tolerant` de la CLI: apertura explícita de contenedores con tablas de asignación
//! dañadas (ADR-020).

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_memrescue-cli"))
}

/// Suma de control del VHD: complemento a uno de la suma de los bytes, sin el propio campo.
fn vhd_seal(b: &mut [u8], field: usize) {
    let sum = b
        .iter()
        .enumerate()
        .filter(|(i, _)| !(field..field + 4).contains(i))
        .fold(0u32, |a, (_, &x)| a.wrapping_add(u32::from(x)));
    b[field..field + 4].copy_from_slice(&(!sum).to_be_bytes());
}

/// VHD dinámico de 4 bloques de 4 KiB (0, 1 y 2 asignados) con la entrada de la BAT del bloque 1
/// fuera del fichero. Cada byte de datos del bloque `b` vale `0x10 + b`.
fn damaged_dynamic_vhd() -> Vec<u8> {
    let size = 4 * 4096u64;
    let mut footer = vec![0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&512u64.to_be_bytes());
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[60..64].copy_from_slice(&3u32.to_be_bytes());
    vhd_seal(&mut footer, 64);
    let mut header = vec![0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    header[16..24].copy_from_slice(&1536u64.to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&4u32.to_be_bytes());
    header[32..36].copy_from_slice(&4096u32.to_be_bytes());
    vhd_seal(&mut header, 36);
    let mut bat = vec![0xFFu8; 512];
    let mut body = Vec::new();
    for b in 0..3usize {
        let sector = ((2048 + body.len()) / 512) as u32;
        let entry = if b == 1 { 0x00FF_0000 } else { sector };
        bat[b * 4..b * 4 + 4].copy_from_slice(&entry.to_be_bytes());
        let mut bitmap = vec![0u8; 512];
        bitmap[0] = 0xFF;
        body.extend_from_slice(&bitmap);
        body.extend(std::iter::repeat_n(0x10 + b as u8, 4096));
    }
    let mut img = footer.clone();
    img.extend_from_slice(&header);
    img.extend_from_slice(&bat);
    img.extend_from_slice(&body);
    img.extend_from_slice(&footer);
    img
}

#[test]
fn tolerant_option_is_explicit_and_reports_untrusted_sectors() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("t.vhd");
    let bytes = damaged_dynamic_vhd();
    std::fs::write(&img, &bytes).unwrap();
    let modified = std::fs::metadata(&img).unwrap().modified().unwrap();
    let p = img.to_str().unwrap();

    // Sin la opción: estricto, la imagen se rechaza (código 1).
    let o = cli().args(["image-info", p]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("VHD"));

    // Con la opción: se abre, la descripción lo indica y hay un aviso visible.
    let o = cli()
        .args(["image-info", p, "--tolerant"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("modo tolerante: 1 bloque descartado"), "{out}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("AVISO"));

    // Verificación: 8 sectores no fiables (código 3).
    let o = cli()
        .args(["verify", p, "--tolerant", "--json"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&o.stdout).contains("[8,8]"));

    // Volcado de un sector descartado: marcado como no fiable (código 3).
    let o = cli()
        .args(["sectors", p, "--lba", "8", "--tolerant"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&o.stdout).contains("NO FIABLE"));

    // Hash estricto: falla.
    let o = cli().args(["hash", p, "--tolerant"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));

    assert_eq!(std::fs::read(&img).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&img).unwrap().modified().unwrap(),
        modified
    );
}
