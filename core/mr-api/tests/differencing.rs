//! Discos diferenciales VHD/VHDX a través de la API (incidencia 8): indicaciones sin abrir el
//! padre, apertura con la cadena explícita, verificación con mapa de validez, vínculo que no
//! coincide e inmutabilidad de los ficheros.

#[path = "../../mr-image/src/diff_builder.rs"]
mod diff_builder;

use diff_builder::{self as db, Writes};
use mr_api::task::TaskContext;
use mr_api::{ApiError, OpenOptions, VerifyOptions};
use mr_io::{BlockDevice, IoError, SectorState};

const SIZE: u64 = 32 * 4096;

#[test]
fn differencing_chain_through_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let w0 = db::writes(0, 0..64);
    let w1 = db::writes(1, [3, 40, 100]);
    let base_b = db::vhd_layer(SIZE, 4096, [0xAB; 16], None, &w0);
    let child_b = db::vhd_layer(
        SIZE,
        4096,
        [0xCD; 16],
        Some(&db::VhdParent::new([0xAB; 16], "base.vhd")),
        &w1,
    );
    let base = dir.path().join("base.vhd");
    let child = dir.path().join("child.vhd");
    std::fs::write(&base, &base_b).unwrap();
    std::fs::write(&child, &child_b).unwrap();

    // Sin padres: referencia externa; las indicaciones no abren nada.
    assert!(matches!(
        mr_api::open_with(&child, OpenOptions::default()),
        Err(ApiError::Io(IoError::ExternalReference { .. }))
    ));
    let hints = mr_api::external_hints(&child).unwrap();
    assert!(hints
        .iter()
        .any(|h| h.key == "parent_name" && h.value == "base.vhd"));
    assert!(mr_api::external_hints(&base).unwrap().is_empty());

    let src = mr_api::open_with_parents(&child, OpenOptions::default(), &[&base], false).unwrap();
    assert_eq!(src.info().external_files.len(), 1);
    assert!(src.info().external_files[0].ends_with("base.vhd"));
    assert!(src.info().description.starts_with("VHD diferencial"));
    let mut expect = vec![0u8; SIZE as usize];
    db::apply(&mut expect, &w0);
    db::apply(&mut expect, &w1);
    let mut got = vec![0u8; SIZE as usize];
    src.read_at(0, &mut got).unwrap();
    assert!(got == expect);
    // Un rango del origen conserva la cadena en su información.
    let r = mr_api::open_range(&src, 8, 8).unwrap();
    assert_eq!(r.info().external_files, src.info().external_files);

    // Verificación: los sectores de cualquier capa se leen; el resto es disperso.
    let report = mr_api::verify(
        src.as_ref(),
        &VerifyOptions::default(),
        &TaskContext::detached(),
    )
    .unwrap();
    let runs = report.map.runs().to_vec();
    assert_eq!(
        runs,
        vec![
            (0, 64, SectorState::Read),
            (64, 36, SectorState::Sparse),
            (100, 1, SectorState::Read),
            (101, 155, SectorState::Sparse),
        ]
    );

    // Vínculo que no coincide: error tipado salvo petición explícita.
    let other = dir.path().join("other.vhd");
    std::fs::write(
        &other,
        db::vhd_layer(SIZE, 4096, [0x11; 16], None, &Writes::new()),
    )
    .unwrap();
    assert!(matches!(
        mr_api::open_with_parents(&child, OpenOptions::default(), &[&other], false),
        Err(ApiError::Io(IoError::ExternalMismatch { .. }))
    ));
    let s = mr_api::open_with_parents(&child, OpenOptions::default(), &[&other], true).unwrap();
    assert!(s.info().description.contains("NO verificado"));
    // Una extracción nunca puede escribir sobre un padre de la cadena.
    assert!(matches!(
        mr_api::check_output_path(&src, &base),
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(mr_api::check_output_path(&src, &dir.path().join("salida.bin")).is_ok());
    drop((src, r, s));
    assert_eq!(std::fs::read(&base).unwrap(), base_b);
    assert_eq!(std::fs::read(&child).unwrap(), child_b);
}
