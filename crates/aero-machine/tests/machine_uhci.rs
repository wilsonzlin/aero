#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::uhci::regs;
use aero_machine::{Machine, MachineConfig};

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
        let pci_cfg = m.pci_config_ports().expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(bar4_base, 0, "UHCI BAR4 base should be assigned by BIOS POST");
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    m.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    m.tick_platform(1_000_000);
    let after = m.io_read(base + regs::REG_FRNUM, 2) as u16;

    assert_eq!(after, (before.wrapping_add(1)) & 0x07ff);
}

