#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::uhci::regs;
use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidKeyboardHandle;

#[test]
fn uhci_tick_increments_frnum_when_running() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let bar4_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    m.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    m.tick_platform(1_000_000);
    let after = m.io_read(base + regs::REG_FRNUM, 2) as u16;

    assert_eq!(after, (before.wrapping_add(1)) & 0x07ff);
}

#[test]
fn uhci_portsc_reflects_device_attach_and_detach() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    let base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    let portsc_before = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_eq!(
        portsc_before & 0x0003,
        0,
        "PORTSC1 should start disconnected"
    );

    // Attach a built-in USB HID keyboard directly to UHCI root port 0.
    let keyboard = UsbHidKeyboardHandle::new();
    let uhci = m.uhci().expect("UHCI device should exist");
    uhci.borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard));

    let portsc_attached = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc_attached & 0x0001,
        0,
        "CCS should be set after attach"
    );
    assert_ne!(
        portsc_attached & 0x0002,
        0,
        "CSC should be set after attach"
    );

    // Detach and ensure connect status clears but change bit remains latched.
    let uhci = m.uhci().expect("UHCI device should exist");
    uhci.borrow_mut().controller_mut().hub_mut().detach(0);
    let portsc_detached = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_eq!(portsc_detached & 0x0001, 0, "CCS should clear after detach");
    assert_ne!(portsc_detached & 0x0002, 0, "CSC should latch after detach");
}

#[test]
fn uhci_tick_remainder_roundtrips_through_snapshot_restore() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let bar4_base = {
        let pci_cfg = src
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    src.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = src.io_read(base + regs::REG_FRNUM, 2) as u16;
    // Advance by half a frame; the controller should not increment FRNUM yet.
    src.tick_platform(500_000);
    let mid = src.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(mid, before);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let bar4_base_restored = {
        let pci_cfg = restored
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base_restored, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base_restored =
        u16::try_from(bar4_base_restored).expect("UHCI BAR4 base should fit in u16");

    let after_restore = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_restore, before);

    // Advance by the remaining half-frame; if the machine's UHCI tick remainder is included in
    // snapshots, this should now increment FRNUM by 1.
    restored.tick_platform(500_000);
    let after_tick = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_tick, (before.wrapping_add(1)) & 0x07ff);
}

#[test]
fn uhci_snapshot_restore_roundtrips_controller_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    // Start the controller and advance a handful of frames.
    m.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
    let frnum_start = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    for _ in 0..5 {
        m.tick_platform(1_000_000);
    }
    let frnum_before = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_before, (frnum_start.wrapping_add(5)) & 0x07ff);

    let snap = m.take_snapshot_full().expect("take_snapshot_full");

    // Mutate state after snapshot so we can observe restoration. Tick 1.5ms so we advance a full
    // frame and leave a 0.5ms remainder in the machine.
    m.tick_platform(1_500_000);
    let frnum_mutated = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_mutated, (frnum_before.wrapping_add(1)) & 0x07ff);

    m.restore_snapshot_bytes(&snap).expect("restore_snapshot");

    let frnum_restored = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_restored, frnum_before);

    // Ensure snapshot restore does not preserve stale host-side sub-ms UHCI tick remainder.
    m.tick_platform(500_000);
    let frnum_after_half_ms = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_after_half_ms, frnum_before);

    // Ticking should continue from the restored value.
    m.tick_platform(500_000);
    let frnum_after = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_after, frnum_mutated);
}
