use std::rc::Rc;

use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use pretty_assertions::{assert_eq, assert_ne};

const REG_ICR: u64 = 0x00C0;
const REG_ICS: u64 = 0x00C8;
const REG_IMS: u64 = 0x00D0;

#[test]
fn snapshot_restore_roundtrips_e1000_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: true,
        ..Default::default()
    })
    .unwrap();

    let e1000 = vm.e1000().expect("e1000 enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false); // unmask cascade
        ints.pic_mut()
            .set_masked(gsi as u8, false); // unmask the routed IRQ (GSI 10-13)

        (gsi, vector)
    };

    // Mutate E1000 state in a guest-visible way:
    // - set IMS to enable TXDW interrupts
    // - set ICR (via ICS) to assert INTx
    // - enqueue an RX frame so the RX queue is non-empty.
    {
        let mut dev = e1000.borrow_mut();
        dev.mmio_write_reg(REG_IMS, 4, aero_net_e1000::ICR_TXDW);
        dev.mmio_write_reg(REG_ICS, 4, aero_net_e1000::ICR_TXDW);
        dev.enqueue_rx_frame(vec![0xAB; 60]);
        assert!(dev.irq_level());
    }

    // The canonical machine snapshots the PCI INTx router, but the E1000 INTx level is surfaced
    // through polling. We intentionally do *not* sync it pre-snapshot, so the platform interrupt
    // controller should not see it yet.
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_e1000_state = {
        let dev = e1000.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(aero_snapshot::DeviceId::E1000, &*dev)
    };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    {
        let mut dev = e1000.borrow_mut();
        let _ = dev.mmio_read(REG_ICR, 4); // clears ICR + INTx
        dev.enqueue_rx_frame(vec![0xCD; 60]);
        assert!(!dev.irq_level());
    }

    let mutated_e1000_state = {
        let dev = e1000.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(aero_snapshot::DeviceId::E1000, &*dev)
    };
    assert_ne!(
        mutated_e1000_state.data, expected_e1000_state.data,
        "e1000 state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the E1000 instance (host wiring/backends live outside snapshots).
    let e1000_after = vm.e1000().expect("e1000 still enabled");
    assert!(
        Rc::ptr_eq(&e1000, &e1000_after),
        "restore must not replace the E1000 instance"
    );

    let restored_e1000_state = {
        let dev = e1000_after.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(aero_snapshot::DeviceId::E1000, &*dev)
    };
    assert_eq!(restored_e1000_state.data, expected_e1000_state.data);

    // After restore, the E1000's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}
