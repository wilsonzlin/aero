use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::devices::gpu::{NullScanoutSink, VirtioGpu2d};
use aero_virtio::devices::input::VirtioInput;
use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::devices::snd::VirtioSnd;
use aero_virtio::devices::VirtioDevice;
use aero_virtio::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};

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

#[test]
fn win7_contract_ring_features_are_consistent_across_devices() {
    let blk = VirtioBlk::new(MemDisk::new(4096));
    assert_win7_contract_ring_features("virtio-blk", blk.device_features());

    let net = VirtioNet::new(LoopbackNet::default(), [0; 6]);
    assert_win7_contract_ring_features("virtio-net", net.device_features());

    let input = VirtioInput::new();
    assert_win7_contract_ring_features("virtio-input", input.device_features());

    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    assert_win7_contract_ring_features("virtio-snd", snd.device_features());

    let gpu = VirtioGpu2d::new(4, 4, NullScanoutSink);
    assert_win7_contract_ring_features("virtio-gpu", gpu.device_features());
}
