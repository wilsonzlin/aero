#![cfg(not(target_arch = "wasm32"))]

use std::io::{Cursor, Read, Seek, SeekFrom};

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::uhci::regs;
use aero_io_snapshot::io::state::SnapshotReader;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

fn snapshot_devices(bytes: &[u8]) -> Vec<snapshot::DeviceState> {
    let index = snapshot::inspect_snapshot(&mut Cursor::new(bytes)).unwrap();
    let devices_section = index
        .sections
        .iter()
        .find(|s| s.id == snapshot::SectionId::DEVICES)
        .expect("missing DEVICES section");

    let mut cursor = Cursor::new(bytes);
    cursor
        .seek(SeekFrom::Start(devices_section.offset))
        .unwrap();
    let mut r = cursor.take(devices_section.len);

    let count = {
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).unwrap();
        u32::from_le_bytes(buf) as usize
    };

    let mut devices = Vec::with_capacity(count);
    for _ in 0..count {
        devices.push(
            snapshot::DeviceState::decode(&mut r, snapshot::limits::MAX_DEVICE_ENTRY_LEN).unwrap(),
        );
    }
    devices
}

#[test]
fn machine_usb_uhci_snapshot_roundtrip_preserves_regs_and_remainder() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let io_base = {
        let pci_cfg = src
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        assert_eq!(
            bar4_base != 0,
            true,
            "UHCI BAR4 base should be assigned by BIOS POST"
        );
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    // -------------------------------------------------------------------------
    // Exercise UHCI register I/O via the PCI I/O BAR window.
    // -------------------------------------------------------------------------

    // Seed FRNUM to a recognizable non-zero value, and enable a couple of interrupt bits.
    let frnum_seed: u16 = 0x123;
    src.io_write(io_base + regs::REG_FRNUM, 2, u32::from(frnum_seed));
    assert_eq!(
        src.io_read(io_base + regs::REG_FRNUM, 2) as u16,
        frnum_seed & 0x07ff
    );

    let usbintr: u16 = regs::USBINTR_IOC | regs::USBINTR_SHORT_PACKET;
    src.io_write(io_base + regs::REG_USBINTR, 2, u32::from(usbintr));
    assert_eq!(src.io_read(io_base + regs::REG_USBINTR, 2) as u16, usbintr);

    // Start the controller (set USBCMD.RS while preserving any default bits like MAXP).
    let usbcmd_before = src.io_read(io_base + regs::REG_USBCMD, 2) as u16;
    let usbcmd_expected = (usbcmd_before | regs::USBCMD_RS) & regs::USBCMD_WRITE_MASK;
    src.io_write(io_base + regs::REG_USBCMD, 2, u32::from(usbcmd_expected));
    assert_eq!(
        src.io_read(io_base + regs::REG_USBCMD, 2) as u16,
        usbcmd_expected
    );

    // Advance by 1.5ms. This should:
    // - tick the UHCI controller once (FRNUM += 1)
    // - leave a 0.5ms machine-level remainder (`uhci_ns_remainder`).
    src.tick_platform(1_500_000);
    let frnum_after_tick = src.io_read(io_base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_after_tick, (frnum_seed.wrapping_add(1)) & 0x07ff);

    let usbsts = src.io_read(io_base + regs::REG_USBSTS, 2) as u16;
    let snap = src.take_snapshot_full().unwrap();

    // -------------------------------------------------------------------------
    // Validate the snapshot uses the machine-level USBC wrapper under DeviceId::USB.
    // -------------------------------------------------------------------------
    let devices = snapshot_devices(&snap);
    let usb_state = devices
        .iter()
        .find(|d| d.id == snapshot::DeviceId::USB)
        .expect("missing DeviceId::USB state");
    assert_eq!(
        usb_state.data.get(8..12).unwrap_or(&[]),
        b"USBC",
        "expected DeviceId::USB to store a USBC wrapper"
    );

    let wrapper_reader = SnapshotReader::parse(&usb_state.data, *b"USBC").unwrap();
    let remainder = wrapper_reader.u64(1).unwrap().unwrap_or(0);
    assert_eq!(
        remainder, 500_000,
        "expected snapshot to store the sub-ms UHCI tick remainder"
    );
    let nested = wrapper_reader
        .bytes(2)
        .expect("missing nested UHCI snapshot in USBC wrapper");
    assert_eq!(
        nested.get(8..12).unwrap_or(&[]),
        b"UHCP",
        "expected USBC wrapper to embed a UHCP UHCI PCI snapshot"
    );

    // -------------------------------------------------------------------------
    // Restore into a new machine and validate that UHCI state + remainder roundtrip.
    // -------------------------------------------------------------------------
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let io_base_restored = {
        let pci_cfg = restored
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        assert_eq!(
            bar4_base != 0,
            true,
            "UHCI BAR4 base should be assigned after restore"
        );
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    assert_eq!(
        restored.io_read(io_base_restored + regs::REG_USBCMD, 2) as u16,
        usbcmd_expected
    );
    assert_eq!(
        restored.io_read(io_base_restored + regs::REG_USBINTR, 2) as u16,
        usbintr
    );
    assert_eq!(
        restored.io_read(io_base_restored + regs::REG_USBSTS, 2) as u16,
        usbsts
    );
    assert_eq!(
        restored.io_read(io_base_restored + regs::REG_FRNUM, 2) as u16,
        frnum_after_tick
    );

    // If the machine-level UHCI remainder is restored, another 0.5ms tick should advance FRNUM by
    // 1 (consuming the saved 0.5ms remainder + new 0.5ms delta).
    restored.tick_platform(500_000);
    let frnum_after_restore_tick = restored.io_read(io_base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(
        frnum_after_restore_tick,
        (frnum_after_tick.wrapping_add(1)) & 0x07ff
    );
}
