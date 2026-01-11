use aero_devices::pci::profile::{
    PciDeviceProfile, PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL, VIRTIO_BLK, VIRTIO_CAP_COMMON,
    VIRTIO_CAP_DEVICE, VIRTIO_CAP_ISR, VIRTIO_CAP_NOTIFY, VIRTIO_INPUT, VIRTIO_NET, VIRTIO_SND,
};

use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::devices::input::VirtioInput;
use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::devices::snd::VirtioSnd;
use aero_virtio::pci::{InterruptLog, VirtioPciDevice};

fn read_config(dev: &VirtioPciDevice, offset: u16, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    dev.config_read(offset, &mut buf);
    buf
}

fn read_u8(dev: &VirtioPciDevice, offset: u16) -> u8 {
    read_config(dev, offset, 1)[0]
}

fn read_u16(dev: &VirtioPciDevice, offset: u16) -> u16 {
    let bytes = read_config(dev, offset, 2);
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(dev: &VirtioPciDevice, offset: u16) -> u32 {
    let bytes = read_config(dev, offset, 4);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn assert_virtio_identity_matches_profile(dev: &VirtioPciDevice, profile: PciDeviceProfile) {
    assert_eq!(read_u16(dev, 0x00), profile.vendor_id, "{}", profile.name);
    assert_eq!(read_u16(dev, 0x02), profile.device_id, "{}", profile.name);
    assert_eq!(read_u8(dev, 0x08), profile.revision_id, "{}", profile.name);

    assert_eq!(
        read_u8(dev, 0x09),
        profile.class.prog_if,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u8(dev, 0x0a),
        profile.class.sub_class,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u8(dev, 0x0b),
        profile.class.base_class,
        "{}",
        profile.name
    );

    assert_eq!(read_u8(dev, 0x0e), profile.header_type, "{}", profile.name);
    assert_eq!(
        read_u16(dev, 0x2c),
        profile.subsystem_vendor_id,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u16(dev, 0x2e),
        profile.subsystem_id,
        "{}",
        profile.name
    );

    let status = read_u16(dev, 0x06);
    assert_ne!(
        status & (1 << 4),
        0,
        "capability list bit not set for {}",
        profile.name
    );
}

fn read_cap_bytes(dev: &VirtioPciDevice, cap_offset: u16, len: usize) -> Vec<u8> {
    read_config(dev, cap_offset, len)
}

#[test]
fn virtio_pci_device_ids_match_canonical_profile() {
    let net = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 1],
        )),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&net, VIRTIO_NET);

    let blk = VirtioPciDevice::new(
        Box::new(VirtioBlk::new(MemDisk::new(512 * 1024))),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&blk, VIRTIO_BLK);

    let input = VirtioPciDevice::new(
        Box::new(VirtioInput::new()),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&input, VIRTIO_INPUT);

    let snd = VirtioPciDevice::new(
        Box::new(VirtioSnd::new(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
        )),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&snd, VIRTIO_SND);
}

#[test]
fn virtio_vendor_specific_capabilities_match_expected_layout() {
    let dev = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 2],
        )),
        Box::new(InterruptLog::default()),
    );

    // Cap pointer is part of the device's stable config-space contract.
    assert_eq!(read_u8(&dev, 0x34), 0x50);

    // Common capability @0x50, cap_len 16, next 0x60.
    let cap0 = read_cap_bytes(&dev, 0x50, 16);
    assert_eq!(cap0[0], 0x09);
    assert_eq!(cap0[1], 0x60);
    assert_eq!(&cap0[2..], &VIRTIO_CAP_COMMON);

    // Notify capability @0x60, cap_len 20, next 0x74.
    let cap1 = read_cap_bytes(&dev, 0x60, 20);
    assert_eq!(cap1[0], 0x09);
    assert_eq!(cap1[1], 0x74);
    assert_eq!(&cap1[2..], &VIRTIO_CAP_NOTIFY);

    // ISR capability @0x74, cap_len 16, next 0x84.
    let cap2 = read_cap_bytes(&dev, 0x74, 16);
    assert_eq!(cap2[0], 0x09);
    assert_eq!(cap2[1], 0x84);
    assert_eq!(&cap2[2..], &VIRTIO_CAP_ISR);

    // Device capability @0x84, cap_len 16, next 0x00.
    let cap3 = read_cap_bytes(&dev, 0x84, 16);
    assert_eq!(cap3[0], 0x09);
    assert_eq!(cap3[1], 0x00);
    assert_eq!(&cap3[2..], &VIRTIO_CAP_DEVICE);
}

#[test]
fn virtio_pci_transitional_exposes_legacy_io_bar_and_device_id() {
    let mut dev = VirtioPciDevice::new_transitional(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 3],
        )),
        Box::new(InterruptLog::default()),
    );

    // Transitional virtio-net device ID should match QEMU convention, but the rest of
    // the PCI identity (class codes, subsystem IDs) should still match the canonical
    // virtio-net profile.
    let mut expected = VIRTIO_NET;
    expected.device_id = PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL;
    assert_virtio_identity_matches_profile(&dev, expected);

    // BAR1 should be present as an I/O BAR for the legacy register block.
    dev.config_write(0x14, &0xffff_ffffu32.to_le_bytes());
    assert_eq!(read_u32(&dev, 0x14), 0xffff_ff01);
}
