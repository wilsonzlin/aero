use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_snapshot_roundtrip_preserves_extended_vga_regs() {
    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();

    // Write a handful of "extended" indices (beyond base VGA register ranges).
    // These are commonly probed by firmware/bootloaders/Windows and should not alias/wrap into the
    // base arrays. Snapshots should preserve them too.
    vm.io_write(0x3C4, 1, 0x06);
    vm.io_write(0x3C5, 1, 0x11);

    vm.io_write(0x3CE, 1, 0x10);
    vm.io_write(0x3CF, 1, 0x22);

    vm.io_write(0x3D4, 1, 0x30);
    vm.io_write(0x3D5, 1, 0x33);

    // Attribute controller uses the 0x1F-masked index space. Use 0x15 (just above the standard
    // 0x14 range) as an "extended" probe index.
    vm.io_read(0x3DA, 1);
    vm.io_write(0x3C0, 1, 0x15);
    vm.io_write(0x3C0, 1, 0x44);

    let snap = vm.take_snapshot_full().unwrap();

    let mut vm2 = Machine::new(cfg).unwrap();
    vm2.reset();
    vm2.restore_snapshot_bytes(&snap).unwrap();

    // Read back the extended values.
    vm2.io_write(0x3C4, 1, 0x06);
    assert_eq!(vm2.io_read(0x3C5, 1) as u8, 0x11);

    vm2.io_write(0x3CE, 1, 0x10);
    assert_eq!(vm2.io_read(0x3CF, 1) as u8, 0x22);

    vm2.io_write(0x3D4, 1, 0x30);
    assert_eq!(vm2.io_read(0x3D5, 1) as u8, 0x33);

    vm2.io_read(0x3DA, 1);
    vm2.io_write(0x3C0, 1, 0x15);
    assert_eq!(vm2.io_read(0x3C1, 1) as u8, 0x44);
}
