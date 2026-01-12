#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{profile, PciDevice, PciInterruptPin};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::{NvmeCompletionQueueState, NvmeControllerState};
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use pretty_assertions::{assert_eq, assert_ne};

#[test]
fn snapshot_restore_roundtrips_nvme_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep this test focused on NVMe + PCI INTx snapshot restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let nvme = vm.nvme().expect("nvme enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = profile::NVME_CONTROLLER.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let gsi_u8 = u8::try_from(gsi).expect("gsi must fit in ISA IRQ range for legacy PIC");
        assert!(
            gsi_u8 < 16,
            "test assumes NVMe routes to a legacy PIC IRQ (0-15); got GSI {gsi}"
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

    // Mirror the canonical PCI config-space state into the NVMe model before taking an expected
    // serialized blob, matching the behavior of `Machine::device_states`.
    {
        let bdf = profile::NVME_CONTROLLER.bdf;
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let (command, bar0_base) = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg
                .and_then(|cfg| cfg.bar_range(0))
                .map(|r| r.base)
                .unwrap_or(0);
            (command, bar0_base)
        };

        let mut dev = nvme.borrow_mut();
        dev.config_mut().set_command(command);
        if bar0_base != 0 {
            dev.config_mut().set_bar_base(0, bar0_base);
        }
    }

    // Mutate NVMe state in a guest-visible way: arrange for the controller to have a pending
    // completion entry so it asserts legacy INTx (level-triggered). This matches the controller's
    // derived `refresh_intx_level` logic, so it survives snapshot/restore.
    {
        let mut dev = nvme.borrow_mut();
        let controller_bytes = dev.controller.save_state();
        let mut controller_state = NvmeControllerState::default();
        controller_state
            .load_state(&controller_bytes)
            .expect("nvme controller state should decode");
        controller_state.intms = 0; // unmask interrupts
        controller_state.admin_cq = Some(NvmeCompletionQueueState {
            qid: 0,
            base: controller_state.acq,
            size: 2,
            head: 0,
            tail: 1,
            phase: false,
            irq_enabled: true,
        });
        dev.controller
            .load_state(&controller_state.save_state())
            .expect("nvme controller state should load");
        assert!(dev.irq_level());
    }

    // The canonical machine snapshots the PCI INTx router, but the NVMe INTx level is surfaced
    // through polling. We intentionally do *not* sync it pre-snapshot, so the platform interrupt
    // controller should not see it yet.
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_nvme_state = nvme.borrow().save_state();
    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind: clear the pending completion
    // so legacy INTx is deasserted.
    {
        let mut dev = nvme.borrow_mut();
        let controller_bytes = dev.controller.save_state();
        let mut controller_state = NvmeControllerState::default();
        controller_state
            .load_state(&controller_bytes)
            .expect("nvme controller state should decode");
        if let Some(ref mut cq) = controller_state.admin_cq {
            cq.head = cq.tail;
            cq.irq_enabled = false;
        }
        controller_state.intms = 0;
        dev.controller
            .load_state(&controller_state.save_state())
            .expect("nvme controller state should load");
        assert!(!dev.irq_level());
    }

    let mutated_nvme_state = nvme.borrow().save_state();
    assert_ne!(
        mutated_nvme_state, expected_nvme_state,
        "NVMe state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the NVMe instance (host wiring/backends live outside snapshots).
    let nvme_after = vm.nvme().expect("nvme still enabled");
    assert!(
        Rc::ptr_eq(&nvme, &nvme_after),
        "restore must not replace the NVMe instance"
    );

    assert_eq!(nvme_after.borrow().save_state(), expected_nvme_state);

    // After restore, the NVMe's asserted INTx level should be re-driven into the platform interrupt
    // sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}
