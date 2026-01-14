use aero_virtio::devices::blk::{
    MemDisk, VirtioBlk, VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_DISCARD, VIRTIO_BLK_F_FLUSH,
    VIRTIO_BLK_F_SEG_MAX, VIRTIO_BLK_F_WRITE_ZEROES,
};
use aero_virtio::devices::gpu::{NullScanoutSink, VirtioGpu2d};
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::devices::net::{LoopbackNet, VirtioNet, VIRTIO_NET_F_MAC, VIRTIO_NET_F_STATUS};
use aero_virtio::devices::VirtioDevice;
use aero_virtio::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};

#[cfg(feature = "snd")]
use aero_virtio::devices::snd::VirtioSnd;

const VIRTIO_F_RING_PACKED: u64 = 1 << 34;

fn assert_win7_contract_ring_features(device_name: &str, features: u64) {
    assert_ne!(
        features & VIRTIO_F_VERSION_1,
        0,
        "{device_name}: must offer VIRTIO_F_VERSION_1"
    );
    assert_ne!(
        features & VIRTIO_F_RING_INDIRECT_DESC,
        0,
        "{device_name}: must offer VIRTIO_F_RING_INDIRECT_DESC"
    );
    assert_eq!(
        features & VIRTIO_F_RING_EVENT_IDX,
        0,
        "{device_name}: must NOT offer VIRTIO_F_RING_EVENT_IDX (Win7 contract v1 requires fixed ring layout without used_event/avail_event)"
    );
    assert_eq!(
        features & VIRTIO_F_RING_PACKED,
        0,
        "{device_name}: must NOT offer VIRTIO_F_RING_PACKED (split rings only)"
    );
}

fn assert_win7_contract_features_exact(device_name: &str, features: u64, expected: u64) {
    assert_eq!(
        features, expected,
        "{device_name}: unexpected feature set (expected {expected:#x}, got {features:#x})"
    );
}

#[test]
fn win7_contract_ring_features_are_consistent_across_devices() {
    let blk = VirtioBlk::new(MemDisk::new(4096));
    assert_win7_contract_ring_features("virtio-blk", blk.device_features());
    assert_win7_contract_features_exact(
        "virtio-blk",
        blk.device_features(),
        VIRTIO_F_VERSION_1
            | VIRTIO_F_RING_INDIRECT_DESC
            | VIRTIO_BLK_F_SEG_MAX
            | VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
            | VIRTIO_BLK_F_DISCARD
            | VIRTIO_BLK_F_WRITE_ZEROES,
    );

    let net = VirtioNet::new(LoopbackNet::default(), [0; 6]);
    assert_win7_contract_ring_features("virtio-net", net.device_features());
    assert_win7_contract_features_exact(
        "virtio-net",
        net.device_features(),
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS,
    );

    let keyboard = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    assert_win7_contract_ring_features("virtio-input (keyboard)", keyboard.device_features());
    assert_win7_contract_features_exact(
        "virtio-input (keyboard)",
        keyboard.device_features(),
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC,
    );

    let mouse = VirtioInput::new(VirtioInputDeviceKind::Mouse);
    assert_win7_contract_ring_features("virtio-input (mouse)", mouse.device_features());
    assert_win7_contract_features_exact(
        "virtio-input (mouse)",
        mouse.device_features(),
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC,
    );

    #[cfg(feature = "snd")]
    {
        let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        assert_win7_contract_ring_features("virtio-snd", snd.device_features());
        assert_win7_contract_features_exact(
            "virtio-snd",
            snd.device_features(),
            VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC,
        );
    }

    let gpu = VirtioGpu2d::new(4, 4, NullScanoutSink);
    assert_win7_contract_ring_features("virtio-gpu", gpu.device_features());
}
