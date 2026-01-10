use aero_emulator::bios::int10;
use aero_emulator::cpu::CpuState;
use aero_emulator::devices::vbe::{
    VbeDevice, VBE_BANK_WINDOW_BASE, VBE_BANK_WINDOW_SIZE, VBE_LFB_BASE, VBE_LFB_SIZE,
};
use aero_emulator::devices::vga::VgaDevice;
use aero_emulator::firmware::e820::{build_e820_map, E820Type};
use aero_emulator::memory::mmio::MmioMemory;
use aero_emulator::memory::MemoryBus;

fn setup_machine() -> (MmioMemory, VbeDevice) {
    let vbe = VbeDevice::new();
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
    (mem, vbe)
}

#[test]
fn e820_reserves_vbe_lfb_region() {
    let map = build_e820_map(512 * 1024 * 1024);
    assert!(map.iter().any(|e| {
        e.addr == VBE_LFB_BASE as u64
            && e.len == VBE_LFB_SIZE as u64
            && e.entry_type == E820Type::Reserved
    }));
}

#[test]
fn vbe_int10_smoke() {
    let (mut mem, mut vbe) = setup_machine();
    let mut cpu = CpuState::default();
    let mut vga = VgaDevice::default();

    // --- 4F00: Controller Info ---
    let buf_addr = 0x0500u64;
    mem.write_physical(buf_addr, b"VBE2");

    cpu.es.selector = 0x0000;
    cpu.rdi = buf_addr;
    cpu.set_ax(0x4F00);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mut info = [0u8; 512];
    mem.read_physical(buf_addr, &mut info);
    assert_eq!(&info[0..4], b"VESA");
    let version = u16::from_le_bytes([info[4], info[5]]);
    assert!(version >= 0x0200, "expected VBE >= 2.0, got {version:#06x}");

    let mode_ptr = u32::from_le_bytes([info[14], info[15], info[16], info[17]]);
    let mode_seg = (mode_ptr >> 16) as u16;
    let mode_off = (mode_ptr & 0xFFFF) as u16;
    let mode_addr = ((mode_seg as u64) << 4) + mode_off as u64;

    let mut modes = Vec::new();
    for i in 0..128u64 {
        let mode = mem.read_u16(mode_addr + i * 2);
        if mode == 0xFFFF {
            break;
        }
        modes.push(mode);
    }

    assert!(modes.contains(&0x115), "missing 800x600x32 (0x115)");
    assert!(modes.contains(&0x118), "missing 1024x768x32 (0x118)");

    // --- 4F01: Mode Info ---
    let mode_info_addr = 0x0700u64;
    cpu.es.selector = 0x0000;
    cpu.rdi = mode_info_addr;
    cpu.set_ax(0x4F01);
    cpu.set_cx(0x118);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mut mode_info = [0u8; 256];
    mem.read_physical(mode_info_addr, &mut mode_info);
    assert_eq!(mode_info[25], 32, "expected 32bpp");

    let physbase = u32::from_le_bytes([mode_info[40], mode_info[41], mode_info[42], mode_info[43]]);
    assert_eq!(physbase, VBE_LFB_BASE);

    // --- 4F02: Set Mode (LFB) ---
    cpu.set_ax(0x4F02);
    cpu.set_cx(0x118 | 0x4000);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    // --- 4F03: Get current mode ---
    cpu.set_ax(0x4F03);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.bx() & 0x3FFF, 0x118);
    assert_eq!(cpu.bx() & 0x4000, 0x4000);

    // --- Bank switching sanity ---
    cpu.set_ax(0x4F05);
    cpu.set_bx(0x0100); // BH=1 get, BL=0 window A
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.dx(), 0);

    cpu.set_ax(0x4F05);
    cpu.set_bx(0x0000); // BH=0 set, BL=0 window A
    cpu.set_dx(1);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
    assert_eq!(cpu.ax(), 0x004F);

    // Write a single pixel through the banked window (bank 1, offset 0).
    mem.write_physical(VBE_BANK_WINDOW_BASE as u64, &[0x11, 0x22, 0x33, 0x00]);
    let mut probe = [0u8; 4];
    mem.read_physical(
        VBE_LFB_BASE as u64 + VBE_BANK_WINDOW_SIZE as u64,
        &mut probe,
    );
    assert_eq!(probe, [0x11, 0x22, 0x33, 0x00]);

    // --- Draw a simple pattern into the LFB and ensure it presents as RGBA ---
    let (width, height) = (1024usize, 768usize);
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

        let sample = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * width + x) * 4;
            [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
        };

        // Four color bars (samples taken away from diagonal).
        assert_eq!(sample(128, 384), [255, 0, 0, 255]); // red
        assert_eq!(sample(384, 384), [0, 255, 0, 255]); // green
        assert_eq!(sample(640, 384), [0, 0, 255, 255]); // blue
        assert_eq!(sample(896, 384), [255, 255, 255, 255]); // white

        // Diagonal line (overrides bar color).
        assert_eq!(sample(0, 0), [255, 255, 0, 255]);
        assert_eq!(sample(512, 384), [255, 255, 0, 255]);
    })
    .expect("expected mode set");
}
