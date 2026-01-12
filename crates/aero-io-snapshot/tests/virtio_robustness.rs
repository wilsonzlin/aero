use aero_io_snapshot::io::state::SnapshotError;
use aero_io_snapshot::io::virtio::state::{
    PciConfigSpaceState, VirtQueueProgressState, VirtioPciQueueState, VirtioPciTransportState,
    MAX_VIRTIO_QUEUES,
};

#[test]
fn virtio_pci_transport_state_roundtrip() {
    let state = VirtioPciTransportState {
        device_status: 0x4f,
        negotiated_features: 0x1234_5678_9abc_def0,
        device_feature_select: 1,
        driver_feature_select: 2,
        driver_features: 0xfedc_ba98_7654_3210,
        msix_config_vector: 0x55aa,
        queue_select: 3,
        isr_status: 0x03,
        legacy_intx_level: true,
        queues: vec![VirtioPciQueueState {
            desc_addr: 0x1000,
            avail_addr: 0x2000,
            used_addr: 0x3000,
            enable: true,
            msix_vector: 0xffff,
            notify_off: 0,
            progress: VirtQueueProgressState {
                next_avail: 7,
                next_used: 5,
                event_idx: false,
            },
        }],
    };

    let bytes = state.encode();
    let decoded = VirtioPciTransportState::decode(&bytes).unwrap();
    assert_eq!(state, decoded);
}

#[test]
fn virtio_pci_transport_rejects_excessive_queue_count() {
    let mut queues = Vec::new();
    for i in 0..(MAX_VIRTIO_QUEUES + 1) {
        queues.push(VirtioPciQueueState {
            desc_addr: 0x1000 + (i as u64) * 0x100,
            avail_addr: 0x2000 + (i as u64) * 0x100,
            used_addr: 0x3000 + (i as u64) * 0x100,
            enable: false,
            msix_vector: 0xffff,
            notify_off: i as u16,
            progress: VirtQueueProgressState {
                next_avail: 0,
                next_used: 0,
                event_idx: false,
            },
        });
    }

    let state = VirtioPciTransportState {
        device_status: 0,
        negotiated_features: 0,
        device_feature_select: 0,
        driver_feature_select: 0,
        driver_features: 0,
        msix_config_vector: 0xffff,
        queue_select: 0,
        isr_status: 0,
        legacy_intx_level: false,
        queues,
    };

    let bytes = state.encode();
    let err = VirtioPciTransportState::decode(&bytes).unwrap_err();
    assert!(matches!(err, SnapshotError::InvalidFieldEncoding(_)));
}

#[test]
fn virtio_pci_transport_rejects_length_mismatch() {
    let state = VirtioPciTransportState {
        device_status: 0,
        negotiated_features: 0,
        device_feature_select: 0,
        driver_feature_select: 0,
        driver_features: 0,
        msix_config_vector: 0xffff,
        queue_select: 0,
        isr_status: 0,
        legacy_intx_level: false,
        queues: vec![VirtioPciQueueState {
            desc_addr: 0,
            avail_addr: 0,
            used_addr: 0,
            enable: false,
            msix_vector: 0xffff,
            notify_off: 0,
            progress: VirtQueueProgressState {
                next_avail: 0,
                next_used: 0,
                event_idx: false,
            },
        }],
    };
    let mut bytes = state.encode();
    bytes.pop(); // truncate
    let err = VirtioPciTransportState::decode(&bytes).unwrap_err();
    assert!(matches!(err, SnapshotError::InvalidFieldEncoding(_)));
}

#[test]
fn pci_config_state_roundtrip_and_bounds() {
    let state = PciConfigSpaceState {
        bytes: [0xa5; 256],
        bar_base: [1, 2, 3, 4, 5, 6],
        bar_probe: [false, true, false, true, false, true],
    };

    let bytes = state.encode();
    let decoded = PciConfigSpaceState::decode(&bytes).unwrap();
    assert_eq!(state, decoded);

    let err = PciConfigSpaceState::decode(&bytes[..bytes.len() - 1]).unwrap_err();
    assert!(matches!(err, SnapshotError::InvalidFieldEncoding(_)));
}
