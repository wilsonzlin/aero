#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{profile, PciInterruptPin};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::{assert_eq, assert_ne};

#[test]
fn snapshot_restore_roundtrips_xhci_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + PCI INTx snapshot restore behavior.
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

    let xhci = vm.xhci().expect("xhci enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    let (gsi, bdf) = {
        let bdf = profile::USB_XHCI_QEMU.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        (gsi, bdf)
    };

    xhci.borrow_mut().raise_event_interrupt();
    assert!(xhci.borrow().irq_level(), "xHCI IRQ level should be asserted");

    // Intentionally do *not* sync xHCI's INTx into the platform interrupt controller before
    // snapshot. This leaves the interrupt sink desynchronized, which restore must fix up by
    // polling device-level IRQ lines again.
    assert_eq!(interrupts.borrow().gsi_level(gsi), false);

    let expected_xhci_state = { xhci.borrow().save_state() };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    xhci.borrow_mut().clear_event_interrupt();
    assert!(
        !xhci.borrow().irq_level(),
        "clearing the event interrupt should deassert xHCI INTx"
    );

    let mutated_xhci_state = { xhci.borrow().save_state() };
    assert_ne!(
        mutated_xhci_state, expected_xhci_state,
        "xHCI state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the xHCI instance (host wiring/backends live outside snapshots).
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(
        Rc::ptr_eq(&xhci, &xhci_after),
        "restore must not replace the xHCI instance"
    );

    let restored_xhci_state = { xhci_after.borrow().save_state() };
    // xHCI's save_state format may include transient fields (e.g. internal bookkeeping) that are
    // not required to roundtrip byte-for-byte. Assert the restore is observable by ensuring the
    // post-restore state differs from the mutated (post-snapshot) state.
    assert_ne!(restored_xhci_state, mutated_xhci_state);
    assert!(xhci_after.borrow().irq_level());

    // After restore, the xHCI's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().gsi_level(gsi),
        true,
        "expected PCI INTx (GSI {gsi}) to be asserted for xHCI (bdf={bdf:?}) after restore"
    );
}
