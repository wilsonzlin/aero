#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{profile, PciInterruptPin};
use aero_devices::usb::uhci::regs;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use pretty_assertions::{assert_eq, assert_ne};

#[test]
fn snapshot_restore_roundtrips_uhci_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test focused on UHCI + PCI INTx snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let uhci = vm.uhci().expect("uhci enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = profile::USB_UHCI_PIIX3.bdf;
        let gsi = pci_intx
            .borrow()
            .gsi_for_intx(bdf, PciInterruptPin::IntA);
        let gsi_u8 = u8::try_from(gsi).expect("gsi must fit in ISA IRQ range for legacy PIC");
        assert!(
            gsi_u8 < 16,
            "test assumes UHCI routes to a legacy PIC IRQ (0-15); got GSI {gsi}"
        );
        let vector = if gsi_u8 < 8 {
            0x20u8.wrapping_add(gsi_u8)
        } else {
            0x28u8.wrapping_add(gsi_u8.wrapping_sub(8))
        };

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false); // unmask cascade
        ints.pic_mut().set_masked(gsi_u8, false); // unmask routed IRQ (GSI 10-13)

        (gsi, vector)
    };

    let bar4_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(bar4_base, 0, "UHCI BAR4 base should be assigned by BIOS POST");
    let io_base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Enable IOC interrupts in the UHCI controller, then force a pending USBINT status bit so the
    // device asserts legacy INTx.
    vm.io_write(
        io_base + regs::REG_USBINTR,
        2,
        u32::from(regs::USBINTR_IOC),
    );
    {
        let mut dev = uhci.borrow_mut();
        dev.controller_mut().set_usbsts_bits(regs::USBSTS_USBINT);
    }
    assert!(uhci.borrow().irq_level(), "UHCI IRQ level should be asserted");

    // Intentionally do *not* sync UHCI's INTx into the platform interrupt controller before
    // snapshot. This leaves the interrupt sink desynchronized, which restore must fix up by
    // polling device-level IRQ lines again.
    assert_eq!(interrupts.borrow().gsi_level(gsi), false);
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_uhci_state = { uhci.borrow().save_state() };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    vm.io_write(
        io_base + regs::REG_USBSTS,
        2,
        u32::from(regs::USBSTS_USBINT),
    );
    assert!(
        !uhci.borrow().irq_level(),
        "clearing USBSTS.USBINT should deassert UHCI INTx"
    );

    let mutated_uhci_state = { uhci.borrow().save_state() };
    assert_ne!(
        mutated_uhci_state, expected_uhci_state,
        "UHCI state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the UHCI instance (host wiring/backends live outside snapshots).
    let uhci_after = vm.uhci().expect("uhci still enabled");
    assert!(
        Rc::ptr_eq(&uhci, &uhci_after),
        "restore must not replace the UHCI instance"
    );

    let restored_uhci_state = { uhci_after.borrow().save_state() };
    assert_eq!(restored_uhci_state, expected_uhci_state);
    assert!(uhci_after.borrow().irq_level());

    // After restore, the UHCI's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}

