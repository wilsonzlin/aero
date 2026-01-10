use crate::{
    bios::{int10_vbe::VbeServices, int10_vga},
    cpu::CpuState,
    devices::vga::VgaDevice,
    memory::MemoryBus,
};

pub fn handle_int10(
    cpu: &mut CpuState,
    mem: &mut impl MemoryBus,
    vga: &mut VgaDevice,
    vbe: &mut impl VbeServices,
) {
    if cpu.ah() == 0x4F {
        vbe.handle_int10(cpu, mem, vga);
        return;
    }

    match cpu.ah() {
        0x00 => int10_vga::set_video_mode(cpu, mem, vga),
        0x01 => int10_vga::set_cursor_shape(cpu, mem),
        0x02 => int10_vga::set_cursor_position(cpu, mem),
        0x03 => int10_vga::get_cursor_position(cpu, mem),
        0x06 => int10_vga::scroll_up(cpu, mem, vga),
        0x09 => int10_vga::write_char_attr(cpu, mem, vga),
        0x0A => int10_vga::write_char_only(cpu, mem, vga),
        0x0E => int10_vga::tty_output(cpu, mem, vga),
        0x0F => int10_vga::get_video_mode(cpu, mem),
        0x13 => int10_vga::write_string(cpu, mem, vga),
        _ => {}
    }
}
