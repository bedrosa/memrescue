//! Extremo a extremo: las imágenes del corpus sintético convertidas por `qemu-img` a VHD, VHDX y
//! VMDK (monolithicSparse y streamOptimized) se abren con la **detección automática** de `mr_api`
//! y deben dar exactamente los mismos bytes que el RAW original (hash completo, MBR protector y
//! cabeceras GPT en su LBA).
//!
//! Requiere qemu-img (`MR_QEMU_IMG` o `.tools/qemu/bin/qemu-img.exe`) y el corpus generado en
//! `tests/corpus/generated` (`MR_CORPUS_DIR` para otra ubicación). Lo ejecuta `scripts/test.ps1`.

use mr_api::task::TaskContext;
use mr_api::{hash_image, open_source, read_sectors, HashOptions};
use mr_io::{BlockDevice, ImageFormat};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn qemu_img() -> PathBuf {
    let p = std::env::var_os("MR_QEMU_IMG")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(".tools/qemu/bin/qemu-img.exe"));
    assert!(p.is_file(), "no se encuentra qemu-img en {}", p.display());
    p
}

fn corpus(name: &str) -> PathBuf {
    let dir = std::env::var_os("MR_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/corpus/generated"));
    let p = dir.join(name);
    assert!(
        p.is_file(),
        "falta {} (genere el corpus con scripts/test.ps1)",
        p.display()
    );
    p
}

fn convert(src: &Path, dst: &Path, fmt: &str, opts: &str) {
    let mut c = Command::new(qemu_img());
    c.args(["convert", "-f", "raw", "-O", fmt]);
    if !opts.is_empty() {
        c.args(["-o", opts]);
    }
    let out = c
        .arg(src)
        .arg(dst)
        .output()
        .expect("no se pudo ejecutar qemu-img");
    assert!(
        out.status.success(),
        "qemu-img convert {fmt} {opts}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sha256_of(dev: &dyn BlockDevice) -> String {
    let r = hash_image(dev, &HashOptions::default(), &TaskContext::detached()).unwrap();
    r.digests[0].1.clone()
}

fn file_sha256(p: &Path) -> String {
    let src = mr_api::open_with(
        p,
        mr_api::OpenOptions {
            sector_size: 512,
            force_raw: true,
            ..Default::default()
        },
    )
    .unwrap();
    sha256_of(src.as_ref())
}

const VARIANTS: [(&str, &str, &str, ImageFormat); 6] = [
    (
        "vpc",
        "subformat=fixed,force_size=on",
        "vhd",
        ImageFormat::Vhd,
    ),
    (
        "vpc",
        "subformat=dynamic,force_size=on",
        "vhd",
        ImageFormat::Vhd,
    ),
    ("vhdx", "subformat=fixed", "vhdx", ImageFormat::Vhdx),
    (
        "vhdx",
        "subformat=dynamic,block_size=1M",
        "vhdx",
        ImageFormat::Vhdx,
    ),
    (
        "vmdk",
        "subformat=monolithicSparse",
        "vmdk",
        ImageFormat::Vmdk,
    ),
    (
        "vmdk",
        "subformat=streamOptimized",
        "vmdk",
        ImageFormat::Vmdk,
    ),
];

#[test]
#[ignore = "requiere qemu-img y el corpus; lo ejecuta scripts/test.ps1"]
fn corpus_images_read_identically_through_every_container() {
    let dir = tempfile::tempdir().unwrap();
    for (name, ss) in [
        ("gpt_basic.img", 512u32),
        ("mbr_basic.img", 512),
        ("gpt_4kn.img", 4096),
    ] {
        let raw = corpus(name);
        let raw_hash_before = file_sha256(&raw);
        let raw_src = open_source(&raw, ss).unwrap();
        let expected = sha256_of(raw_src.as_ref());
        assert_eq!(expected, raw_hash_before);
        let total = raw_src.info().sector_count;
        for (i, (fmt, opts, ext, kind)) in VARIANTS.iter().enumerate() {
            let out = dir.path().join(format!("{name}.{i}.{ext}"));
            convert(&raw, &out, fmt, opts);
            let container_before = file_sha256(&out);
            // Detección automática; el tamaño de sector se fija al del disco contenido.
            let src = open_source(&out, ss).unwrap();
            let info = src.info();
            assert_eq!(info.format, *kind, "{name} → {fmt} {opts}");
            assert_eq!(info.sector_size, ss);
            assert_eq!(info.sector_count, total, "{name} → {fmt} {opts}");
            assert_eq!(sha256_of(src.as_ref()), expected, "{name} → {fmt} {opts}");
            // Estructuras de particiones en su sitio.
            let s0 = read_sectors(src.as_ref(), 0, 1).unwrap();
            assert_eq!(&s0[0x1FE..0x200], b"\x55\xaa", "{name} {fmt}");
            if name.starts_with("gpt") {
                assert_eq!(&read_sectors(src.as_ref(), 1, 1).unwrap()[..8], b"EFI PART");
                assert_eq!(
                    &read_sectors(src.as_ref(), total - 1, 1).unwrap()[..8],
                    b"EFI PART"
                );
            }
            // Lectura con estados: todo fiable (leído o disperso), nunca relleno.
            let (_, states) =
                mr_api::read_sectors_mapped(src.as_ref(), 0, total.min(4096), 0).unwrap();
            assert!(states.iter().all(|s| s.is_trustworthy()));
            drop(src);
            assert_eq!(file_sha256(&out), container_before, "contenedor modificado");
        }
        drop(raw_src);
        assert_eq!(
            file_sha256(&raw),
            raw_hash_before,
            "RAW original modificado"
        );
    }
}
