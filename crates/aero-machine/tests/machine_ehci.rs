#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::usb::ehci::{EhciPciDevice, CAPLENGTH, HCIVERSION};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn ehci_pci_function_exists_and_capability_registers_are_mmio_readable() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep the machine minimal/deterministic for this PCI/MMIO probe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let (class, command, bar0_base) = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_EHCI_ICH9.bdf)
            .expect("EHCI PCI function should exist");
        (
            cfg.class_code(),
            cfg.command(),
            cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX)
                .map(|range| range.base)
                .unwrap_or(0),
        )
    };

    assert_eq!(class.class, 0x0c, "base class should be Serial Bus");
    assert_eq!(class.subclass, 0x03, "subclass should be USB");
    assert_eq!(class.prog_if, 0x20, "prog-if should be EHCI");
    assert_ne!(
        command & 0x2,
        0,
        "EHCI MEM decoding should be enabled by BIOS POST"
    );
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 base should be assigned by BIOS POST"
    );

    // CAPLENGTH (byte0) + HCIVERSION (u16 at bytes2-3).
    let cap = m.read_physical_u32(bar0_base);
    assert_eq!((cap & 0xFF) as u8, CAPLENGTH);
    assert_eq!((cap >> 16) as u16, HCIVERSION);
}
