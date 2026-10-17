//! `NullDevice` y `PatternDevice` presentados con otro tamaño de sector a través de `Reblocked`.

use mr_image::Reblocked;
use mr_io::resilient::{read_range, ReadPolicy};
use mr_io::{Allocation, BlockDevice, NullDevice, PatternDevice, SectorState};
use std::sync::Arc;

#[test]
fn null_device_stays_sparse_with_any_sector_size() {
    let inner: Arc<dyn BlockDevice> = Arc::new(NullDevice::with_sectors(64, 512).unwrap());
    for ss in [512u32, 1024, 2048, 4096] {
        let view = Reblocked::new(Arc::clone(&inner), ss).unwrap();
        let total = view.sector_count();
        assert_eq!(total, 64 * 512 / u64::from(ss));
        assert_eq!(
            view.allocation(0, total).unwrap(),
            vec![(0, total, Allocation::Sparse)]
        );
        let mut states = Vec::new();
        let report = read_range(
            &view,
            0,
            total,
            &ReadPolicy::default(),
            |_, data, st| {
                assert!(data.iter().all(|&b| b == 0));
                states.extend_from_slice(st);
                Ok(())
            },
            |_, _| true,
        )
        .unwrap();
        assert_eq!(report.map.count(SectorState::Sparse), total);
        assert!(states.iter().all(|&s| s == SectorState::Sparse));
    }
}

#[test]
fn pattern_device_keeps_its_bytes_and_data_allocation() {
    let inner: Arc<dyn BlockDevice> =
        Arc::new(PatternDevice::new(16 * 512, 512, *b"MRPT").unwrap());
    let view = Reblocked::new(inner, 4096).unwrap();
    assert_eq!(
        view.allocation(0, 2).unwrap(),
        vec![(0, 2, Allocation::Data)]
    );
    let mut buf = vec![0u8; 4096];
    view.read_sectors(1, 1, &mut buf).unwrap();
    assert!(buf.chunks(4).all(|c| c == b"MRPT"));
    assert!(view.read_sectors(2, 1, &mut buf).is_err());
}
