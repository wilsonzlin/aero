use aero_machine::{Machine, MachineConfig};

#[test]
fn virtio_input_snapshot_is_ignored_when_virtio_input_is_disabled() {
    let cfg_with_virtio_input = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep the machine minimal.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_uhci: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg_with_virtio_input).unwrap();
    let snap = vm.take_snapshot_full().unwrap();

    // Restore into a machine with virtio-input disabled. The snapshot should restore successfully
    // and ignore virtio-input device payloads (config mismatch), similar to AeroGPU/VGA behavior.
    let cfg_without_virtio_input = MachineConfig {
        enable_virtio_input: false,
        ..vm.config().clone()
    };
    let mut vm2 = Machine::new(cfg_without_virtio_input).unwrap();
    vm2.reset();
    assert!(
        vm2.virtio_input_keyboard().is_none() && vm2.virtio_input_mouse().is_none(),
        "sanity: expected no virtio-input devices when enable_virtio_input=false"
    );

    vm2.restore_snapshot_bytes(&snap).unwrap();
    assert!(
        vm2.virtio_input_keyboard().is_none() && vm2.virtio_input_mouse().is_none(),
        "virtio-input should remain absent after restoring a snapshot taken with enable_virtio_input=true"
    );
}

