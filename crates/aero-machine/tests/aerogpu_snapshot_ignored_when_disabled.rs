use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_snapshot_is_ignored_when_aerogpu_is_disabled() {
    let cfg_with_aerogpu = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the machine minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg_with_aerogpu).unwrap();
    let snap = vm.take_snapshot_full().unwrap();

    // Restore into a machine with AeroGPU disabled. The snapshot should restore successfully and
    // ignore the AeroGPU device payloads (config mismatch), similar to legacy VGA snapshots.
    let cfg_without_aerogpu = MachineConfig {
        enable_aerogpu: false,
        ..vm.config().clone()
    };
    let mut vm2 = Machine::new(cfg_without_aerogpu).unwrap();
    vm2.reset();
    assert!(
        vm2.aerogpu().is_none(),
        "sanity: expected no AeroGPU device when enable_aerogpu=false"
    );
    vm2.restore_snapshot_bytes(&snap).unwrap();
    assert!(
        vm2.aerogpu().is_none(),
        "AeroGPU should still be absent after restoring a snapshot taken with enable_aerogpu=true"
    );
}
