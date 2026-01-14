#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciDevice};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn machine_process_ahci_mirrors_bar5_when_guest_clears_it_to_zero() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ahci = vm.ahci().expect("ahci enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // BIOS POST must assign a non-zero base address to BAR5 (ABAR).
    let abar_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("AHCI config function must exist");
        cfg.bar_range(profile::AHCI_ABAR_BAR_INDEX)
            .expect("AHCI BAR5 must exist")
            .base
    };
    assert_ne!(abar_base, 0, "expected AHCI BAR5 base to be assigned");

    // Sync once so the device model observes the assigned BAR5 base.
    vm.process_ahci();
    {
        let dev = ahci.borrow();
        assert_eq!(
            dev.config().bar_range(profile::AHCI_ABAR_BAR_INDEX).unwrap().base,
            abar_base
        );
    }

    // Now simulate a guest unassigning BAR5 by programming it to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .write_config(bdf, u16::from(profile::AHCI_ABAR_CFG_OFFSET), 4, 0);
    }

    // Re-sync: BAR5 base=0 must still be mirrored into the device model (BAR-present-with-base=0).
    vm.process_ahci();
    {
        let dev = ahci.borrow();
        assert_eq!(
            dev.config().bar_range(profile::AHCI_ABAR_BAR_INDEX).unwrap().base,
            0
        );
    }
}
