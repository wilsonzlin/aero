#![cfg(target_arch = "wasm32")]

use aero_emulator::bios::int10;
use aero_emulator::cpu::CpuState;
use aero_emulator::devices::vbe::{
    VbeDevice, VBE_BANK_WINDOW_BASE, VBE_BANK_WINDOW_SIZE, VBE_LFB_BASE, VBE_LFB_SIZE,
};
use aero_emulator::devices::vga::VgaDevice;
use aero_emulator::memory::mmio::MmioMemory;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn vbe_smoke_wasm() {
    let mut vbe = VbeDevice::new();
    let mut mem = MmioMemory::new(2 * 1024 * 1024);
    mem.map_mmio(
        VBE_BANK_WINDOW_BASE as u64,
        VBE_BANK_WINDOW_SIZE as u64,
        Box::new(vbe.mmio_bank_window()),
    );
    mem.map_mmio(
        VBE_LFB_BASE as u64,
        VBE_LFB_SIZE as u64,
        Box::new(vbe.mmio_lfb()),
    );
    let mut cpu = CpuState::default();
    let mut vga = VgaDevice::default();

    // Controller info.
    let buf_addr = 0x0500u64;
    mem.write_physical(buf_addr, b"VBE2");
    cpu.es.selector = 0;
    cpu.rdi = buf_addr;
    cpu.set_ax(0x4F00);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);

    // Pick 800x600x32 (0x115) for faster pixel fill on wasm.
    cpu.set_ax(0x4F02);
    cpu.set_cx(0x115 | 0x4000);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);

    let (width, height) = (800usize, 600usize);
    let stride = width * 4;
    let mut pattern = vec![0u8; stride * height];
    for y in 0..height {
        let diag_x = (y * width) / height;
        for x in 0..width {
            let (mut r, mut g, mut b) = match (x * 4) / width {
                0 => (255, 0, 0),
                1 => (0, 255, 0),
                2 => (0, 0, 255),
                _ => (255, 255, 255),
            };
            if x == diag_x {
                (r, g, b) = (255, 255, 0);
            }
            let off = y * stride + x * 4;
            pattern[off] = b;
            pattern[off + 1] = g;
            pattern[off + 2] = r;
            pattern[off + 3] = 0;
        }
    }
    mem.write_physical(VBE_LFB_BASE as u64, &pattern);

    vbe.with_framebuffer_rgba(|w, h, rgba| {
        assert_eq!((w as usize, h as usize), (width, height));
        let idx = (300 * width + 400) * 4;
        // This point is on the diagonal for 800x600 (400,300).
        assert_eq!(&rgba[idx..idx + 4], &[255, 255, 0, 255]);
    })
    .unwrap();
}
