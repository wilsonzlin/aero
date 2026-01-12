use aero_io_snapshot::io::storage::state::{
    attach_aero_storage_disk, DiskBackendState, DiskLayerState, LocalDiskBackendKind,
    LocalDiskBackendState,
};

#[test]
fn disk_layer_state_attach_aero_storage_disk_roundtrip() {
    let sector_size = 512usize;
    let size_bytes = 4096u64;

    let mut state = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "disk0".to_string(),
            overlay: None,
        }),
        size_bytes,
        sector_size,
    );

    let disk = aero_storage::RawDisk::create(aero_storage::MemBackend::new(), size_bytes).unwrap();
    attach_aero_storage_disk(&mut state, Box::new(disk));

    let mut sector = vec![0u8; sector_size];
    for (i, b) in sector.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    state.write_sector(0, &sector);

    let read_back = state.read_sector(0);
    assert_eq!(read_back, sector);
}
