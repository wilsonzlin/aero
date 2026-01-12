use aero_gpu_vga::{DisplayOutput, SVGA_LFB_BASE};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn framebuffer_hash_rgba8888(framebuffer: &[u32]) -> u64 {
    let mut bytes = Vec::with_capacity(framebuffer.len() * 4);
    for &px in framebuffer {
        bytes.extend_from_slice(&px.to_le_bytes());
    }
    fnv1a64(&bytes)
}

#[test]
fn vga_snapshot_roundtrip_restores_vbe_and_framebuffer() {
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

    // Program Bochs VBE_DISPI to 64x64x32 with LFB enabled.
    vm.io_write(0x01CE, 2, 0x0001);
    vm.io_write(0x01CF, 2, 64);
    vm.io_write(0x01CE, 2, 0x0002);
    vm.io_write(0x01CF, 2, 64);
    vm.io_write(0x01CE, 2, 0x0003);
    vm.io_write(0x01CF, 2, 32);
    vm.io_write(0x01CE, 2, 0x0004);
    vm.io_write(0x01CF, 2, 0x0041);

    // Write a few pixels (packed 32bpp BGRX).
    let base = u64::from(SVGA_LFB_BASE);
    vm.write_physical_u32(base, 0x00FF_0000); // (0,0) red
    vm.write_physical_u32(base + 4, 0x0000_FF00); // (1,0) green
    vm.write_physical_u32(base + 8, 0x0000_00FF); // (2,0) blue
    vm.write_physical_u32(base + 12, 0x00FF_FFFF); // (3,0) white

    let vga = vm.vga().expect("pc platform should include VGA");
    vga.borrow_mut().present();
    let (width, height) = vga.borrow().get_resolution();
    let hash_before = framebuffer_hash_rgba8888(vga.borrow().get_framebuffer());

    let snap = vm.take_snapshot_full().unwrap();

    let mut vm2 = Machine::new(cfg).unwrap();
    vm2.reset();
    vm2.restore_snapshot_bytes(&snap).unwrap();

    let vga2 = vm2
        .vga()
        .expect("pc platform should include VGA after restore");
    vga2.borrow_mut().present();
    assert_eq!(vga2.borrow().get_resolution(), (width, height));
    let hash_after = framebuffer_hash_rgba8888(vga2.borrow().get_framebuffer());
    assert_eq!(hash_after, hash_before);
}
