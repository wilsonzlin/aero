use firmware::{
    bda::BiosDataArea,
    bios::Bios,
    cpu::CpuState,
    memory::{MemoryBus, VecMemory},
    rtc::{CmosRtc, DateTime},
};

const VGA_FB_BASE: u64 = 0xA0000;
const VGA_FB_SIZE: usize = 64 * 1024;

fn fill_framebuffer(mem: &mut VecMemory, value: u8) {
    const CHUNK_SIZE: usize = 4096;
    let chunk = [value; CHUNK_SIZE];

    let mut addr = VGA_FB_BASE;
    let mut remaining = VGA_FB_SIZE;
    while remaining != 0 {
        let len = remaining.min(CHUNK_SIZE);
        mem.write_physical(addr, &chunk[..len]);
        addr = addr.saturating_add(len as u64);
        remaining -= len;
    }
}

#[test]
fn int10_set_mode_13h_clears_framebuffer_when_clear_requested() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Seed the framebuffer with non-zero data so we can verify the clear path.
    fill_framebuffer(&mut mem, 0xAA);

    // Set mode 13h (clear requested; bit7=0).
    cpu.set_ax(0x0013);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_video_mode(&mut mem), 0x13);
    assert_eq!(mem.read_u8(VGA_FB_BASE), 0);
    assert_eq!(mem.read_u8(VGA_FB_BASE + 0x1234), 0);
    assert_eq!(mem.read_u8(VGA_FB_BASE + VGA_FB_SIZE as u64 - 1), 0);
}

#[test]
fn int10_set_mode_13h_respects_no_clear_bit() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // First set mode 13h with clear so we start from a known state.
    cpu.set_ax(0x0013);
    bios.handle_int10(&mut cpu, &mut mem);

    // Write sentinel bytes into the framebuffer.
    mem.write_u8(VGA_FB_BASE, 0x12);
    mem.write_u8(VGA_FB_BASE + 1, 0x34);
    mem.write_u8(VGA_FB_BASE + VGA_FB_SIZE as u64 - 1, 0x56);

    // Set mode 13h again with "no clear" (bit7=1).
    cpu.set_ax(0x0093);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_video_mode(&mut mem), 0x13);
    assert_eq!(mem.read_u8(VGA_FB_BASE), 0x12);
    assert_eq!(mem.read_u8(VGA_FB_BASE + 1), 0x34);
    assert_eq!(mem.read_u8(VGA_FB_BASE + VGA_FB_SIZE as u64 - 1), 0x56);
}
