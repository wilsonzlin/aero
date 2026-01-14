use aero_devices::pci::profile::{
    PciDeviceProfile, PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL, VIRTIO_BLK, VIRTIO_CAP_COMMON,
    VIRTIO_CAP_DEVICE, VIRTIO_CAP_ISR, VIRTIO_CAP_NOTIFY, VIRTIO_INPUT_KEYBOARD,
    VIRTIO_INPUT_MOUSE, VIRTIO_INPUT_TABLET, VIRTIO_NET,
};

use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::pci::{InterruptLog, VirtioPciDevice};

#[cfg(feature = "snd")]
use aero_devices::pci::profile::VIRTIO_SND;

#[cfg(feature = "snd")]
use aero_virtio::devices::snd::VirtioSnd;

fn read_config(dev: &mut VirtioPciDevice, offset: u16, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    dev.config_read(offset, &mut buf);
    buf
}

fn read_u8(dev: &mut VirtioPciDevice, offset: u16) -> u8 {
    read_config(dev, offset, 1)[0]
}

fn read_u16(dev: &mut VirtioPciDevice, offset: u16) -> u16 {
    let bytes = read_config(dev, offset, 2);
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(dev: &mut VirtioPciDevice, offset: u16) -> u32 {
    let bytes = read_config(dev, offset, 4);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn assert_virtio_identity_matches_profile(dev: &mut VirtioPciDevice, profile: PciDeviceProfile) {
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

    if let Some(pin) = profile.interrupt_pin {
        assert_eq!(
            read_u8(dev, 0x3d),
            pin.to_config_u8(),
            "{} interrupt pin",
            profile.name
        );
    }

    let status = read_u16(dev, 0x06);
    assert_ne!(
        status & (1 << 4),
        0,
        "capability list bit not set for {}",
        profile.name
    );

    // Ensure the expected BARs are present with the correct kind/type bits.
    for bar in profile.bars {
        let offset = 0x10u16 + u16::from(bar.index) * 4;
        assert_eq!(
            read_u32(dev, offset),
            bar.initial_register_value(),
            "{} BAR{} initial value",
            profile.name,
            bar.index
        );
        if bar.kind == aero_devices::pci::profile::PciBarKind::Mem64 {
            assert_eq!(
                read_u32(dev, offset + 4),
                0,
                "{} BAR{} high dword initial value",
                profile.name,
                bar.index
            );
        }
    }
}

fn read_cap_bytes(dev: &mut VirtioPciDevice, cap_offset: u16, len: usize) -> Vec<u8> {
    read_config(dev, cap_offset, len)
}

#[test]
fn virtio_pci_device_ids_match_canonical_profile() {
    let mut net = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 1],
        )),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&mut net, VIRTIO_NET);

    let mut blk = VirtioPciDevice::new(
        Box::new(VirtioBlk::new(Box::new(MemDisk::new(512 * 1024)))),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&mut blk, VIRTIO_BLK);

    let mut keyboard = VirtioPciDevice::new(
        Box::new(VirtioInput::new(VirtioInputDeviceKind::Keyboard)),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&mut keyboard, VIRTIO_INPUT_KEYBOARD);

    let mut mouse = VirtioPciDevice::new(
        Box::new(VirtioInput::new(VirtioInputDeviceKind::Mouse)),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&mut mouse, VIRTIO_INPUT_MOUSE);

    let mut tablet = VirtioPciDevice::new(
        Box::new(VirtioInput::new(VirtioInputDeviceKind::Tablet)),
        Box::new(InterruptLog::default()),
    );
    assert_virtio_identity_matches_profile(&mut tablet, VIRTIO_INPUT_TABLET);

    #[cfg(feature = "snd")]
    {
        let mut snd = VirtioPciDevice::new(
            Box::new(VirtioSnd::new(
                aero_audio::ring::AudioRingBuffer::new_stereo(8),
            )),
            Box::new(InterruptLog::default()),
        );
        assert_virtio_identity_matches_profile(&mut snd, VIRTIO_SND);
    }
}

#[test]
fn virtio_vendor_specific_capabilities_match_expected_layout() {
    let mut dev = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 2],
        )),
        Box::new(InterruptLog::default()),
    );

    // Cap pointer is part of the device's stable config-space contract.
    assert_eq!(read_u8(&mut dev, 0x34), 0x40);

    // Common capability @0x40, cap_len 16, next 0x50.
    let cap0 = read_cap_bytes(&mut dev, 0x40, 16);
    assert_eq!(cap0[0], 0x09);
    assert_eq!(cap0[1], 0x50);
    assert_eq!(&cap0[2..], &VIRTIO_CAP_COMMON);

    // Notify capability @0x50, cap_len 20, next 0x64.
    let cap1 = read_cap_bytes(&mut dev, 0x50, 20);
    assert_eq!(cap1[0], 0x09);
    assert_eq!(cap1[1], 0x64);
    assert_eq!(&cap1[2..], &VIRTIO_CAP_NOTIFY);

    // ISR capability @0x64, cap_len 16, next 0x74.
    let cap2 = read_cap_bytes(&mut dev, 0x64, 16);
    assert_eq!(cap2[0], 0x09);
    assert_eq!(cap2[1], 0x74);
    assert_eq!(&cap2[2..], &VIRTIO_CAP_ISR);

    // Device capability @0x74, cap_len 16, next 0x84.
    let cap3 = read_cap_bytes(&mut dev, 0x74, 16);
    assert_eq!(cap3[0], 0x09);
    assert_eq!(cap3[1], 0x84);
    assert_eq!(&cap3[2..], &VIRTIO_CAP_DEVICE);

    // MSI-X capability @0x84, cap_len 12, next 0x00.
    let cap4 = read_cap_bytes(&mut dev, 0x84, 12);
    assert_eq!(cap4[0], 0x11);
    assert_eq!(cap4[1], 0x00);

    // Table size is N-1 in bits 0..=10. virtio-net has 2 virtqueues, and we expose one extra
    // vector for config changes => 3 total.
    let msg_ctl = u16::from_le_bytes([cap4[2], cap4[3]]);
    assert_eq!(msg_ctl & 0x07ff, 2);

    let table = u32::from_le_bytes(cap4[4..8].try_into().unwrap());
    assert_eq!(table & 0x7, 0);
    assert_eq!(table & !0x7, 0x3100);

    let pba = u32::from_le_bytes(cap4[8..12].try_into().unwrap());
    assert_eq!(pba & 0x7, 0);
    assert_eq!(pba & !0x7, 0x3130);
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
    assert_virtio_identity_matches_profile(&mut dev, expected);

    // BAR2 should be present as an I/O BAR for the legacy register block.
    dev.config_write(0x18, &0xffff_ffffu32.to_le_bytes());
    assert_eq!(read_u32(&mut dev, 0x18), 0xffff_ff01);
}

#[test]
fn virtio_pci_bar0_size_probe_reports_contract_len() {
    let mut dev = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 4],
        )),
        Box::new(InterruptLog::default()),
    );

    // BAR0 is 64-bit MMIO. Probe the size via the standard PCI mechanism and verify we report
    // the contract-required 0x4000 byte mapping.
    dev.config_write(0x10, &0xffff_ffffu32.to_le_bytes());
    dev.config_write(0x14, &0xffff_ffffu32.to_le_bytes());

    let lo = read_u32(&mut dev, 0x10);
    let hi = read_u32(&mut dev, 0x14);

    // Mask out the BAR type bits (low nibble), then compute size from the returned mask.
    let mask = (u64::from(hi) << 32) | u64::from(lo & 0xffff_fff0);
    let size = (!mask).wrapping_add(1);
    assert_eq!(size, 0x4000);
}

#[test]
fn virtio_pci_bar_subword_write_updates_bar0_base_without_triggering_probe() {
    let mut dev = VirtioPciDevice::new(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 5],
        )),
        Box::new(InterruptLog::default()),
    );

    // Program BAR0 (64-bit MMIO) via a 16-bit write. This should not panic, and should update the
    // base after clamping to BAR alignment (0x4000).
    dev.config_write(0x10, &0xc000u16.to_le_bytes());
    assert_eq!(read_u32(&mut dev, 0x10), 0x0000_c004);
    assert_eq!(read_u32(&mut dev, 0x14), 0);
}

#[test]
fn virtio_pci_legacy_io_bar_subword_write_updates_base() {
    let mut dev = VirtioPciDevice::new_transitional(
        Box::new(VirtioNet::new(
            LoopbackNet::default(),
            [0x52, 0x54, 0x00, 0, 0, 6],
        )),
        Box::new(InterruptLog::default()),
    );

    // Program BAR2 (legacy I/O register block) via a 16-bit write. The base should be clamped to
    // the 0x100-byte BAR size and keep the I/O BAR flag bit.
    dev.config_write(0x18, &0x1235u16.to_le_bytes());
    assert_eq!(read_u32(&mut dev, 0x18), 0x0000_1201);
    assert_eq!(dev.legacy_io_base(), 0x0000_1200);
}
