use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::devices::snd::VirtioSnd;
use aero_virtio::pci::{InterruptLog, VirtioPciDevice, VIRTIO_PCI_CAP_COMMON_CFG};

#[derive(Default)]
struct Caps {
    common: u64,
}

fn parse_caps(dev: &mut VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        assert_eq!(cfg[ptr], 0x09);
        let next = cfg[ptr + 1] as usize;
        let cap_len = cfg[ptr + 2] as usize;
        let cfg_type = cfg[ptr + 3];
        let bar = cfg[ptr + 4];
        let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;

        assert_eq!(bar, 0, "virtio capabilities must reference BAR0");
        assert!(cap_len >= 16, "virtio_pci_cap too small");

        if cfg_type == VIRTIO_PCI_CAP_COMMON_CFG {
            caps.common = offset;
        }

        ptr = next;
    }

    caps
}

fn bar_read_u16(dev: &mut VirtioPciDevice, off: u64) -> u16 {
    let mut buf = [0u8; 2];
    dev.bar0_read(off, &mut buf);
    u16::from_le_bytes(buf)
}

fn bar_write_u16(dev: &mut VirtioPciDevice, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn assert_queue_layout(
    dev: &mut VirtioPciDevice,
    expected_num_queues: u16,
    expected_sizes: &[u16],
) {
    let caps = parse_caps(dev);

    // Common cfg: num_queues.
    assert_eq!(
        bar_read_u16(dev, caps.common + 0x12),
        expected_num_queues,
        "num_queues mismatch"
    );

    for (q, &expected_size) in expected_sizes.iter().enumerate() {
        let q = q as u16;
        bar_write_u16(dev, caps.common + 0x16, q);

        // Contract v1 treats queue_size as read-only and fixed per queue/device.
        assert_eq!(
            bar_read_u16(dev, caps.common + 0x18),
            expected_size,
            "queue {q} size mismatch"
        );
        // Contract v1: queue_notify_off(q) == q.
        assert_eq!(
            bar_read_u16(dev, caps.common + 0x1e),
            q,
            "queue {q} notify_off mismatch"
        );
    }

    // Out-of-range queue_select must read queue_size and notify_off as 0 and ignore writes.
    bar_write_u16(dev, caps.common + 0x16, expected_num_queues.wrapping_add(3));
    assert_eq!(bar_read_u16(dev, caps.common + 0x18), 0);
    assert_eq!(bar_read_u16(dev, caps.common + 0x1e), 0);
}

#[test]
fn win7_contract_common_cfg_queue_sizes_match_spec() {
    let blk = VirtioBlk::new(MemDisk::new(4096));
    let mut blk_pci = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
    assert_queue_layout(&mut blk_pci, 1, &[128]);

    let net = VirtioNet::new(LoopbackNet::default(), [0; 6]);
    let mut net_pci = VirtioPciDevice::new(Box::new(net), Box::new(InterruptLog::default()));
    assert_queue_layout(&mut net_pci, 2, &[256, 256]);

    let keyboard = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut keyboard_pci =
        VirtioPciDevice::new(Box::new(keyboard), Box::new(InterruptLog::default()));
    assert_queue_layout(&mut keyboard_pci, 2, &[64, 64]);

    let mouse = VirtioInput::new(VirtioInputDeviceKind::Mouse);
    let mut mouse_pci = VirtioPciDevice::new(Box::new(mouse), Box::new(InterruptLog::default()));
    assert_queue_layout(&mut mouse_pci, 2, &[64, 64]);

    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut snd_pci = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    assert_queue_layout(&mut snd_pci, 4, &[64, 64, 256, 64]);
}
