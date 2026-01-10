use std::sync::atomic::Ordering;

use emulator::devices::vga::{Vga, VgaSharedFramebufferOutput};
use emulator::io::PortIO;

fn program_mode13h_registers(vga: &mut Vga) {
    // Sequencer Memory Mode (index 0x04):
    // - bit 3: chain-4 enable
    // - bit 2: odd/even disable (so `odd_even` becomes false in derived state)
    vga.port_write(0x3C4, 1, 0x04);
    vga.port_write(0x3C5, 1, 0x0C);

    // Graphics Controller Misc register (index 0x06) bit 0 = graphics mode.
    vga.port_write(0x3CE, 1, 0x06);
    vga.port_write(0x3CF, 1, 0x01);
}

fn program_minimal_palette(vga: &mut Vga) {
    // Program DAC entry 0 as black and entry 1 as red.
    vga.port_write(0x3C8, 1, 0);

    // Entry 0.
    vga.port_write(0x3C9, 1, 0);
    vga.port_write(0x3C9, 1, 0);
    vga.port_write(0x3C9, 1, 0);

    // Entry 1 (63,0,0 in 6-bit space).
    vga.port_write(0x3C9, 1, 63);
    vga.port_write(0x3C9, 1, 0);
    vga.port_write(0x3C9, 1, 0);
}

#[test]
fn vga_present_to_shared_framebuffer_updates_header_and_frame_counter() {
    let mut vga = Vga::new();
    program_mode13h_registers(&mut vga);
    program_minimal_palette(&mut vga);
    vga.write_vram_u8(0, 1);

    let mut output = VgaSharedFramebufferOutput::new(1024, 768).expect("allocate shared framebuffer");

    {
        let view = output.view_mut();
        let header = view.header();
        assert_eq!(header.config_counter.load(Ordering::Relaxed), 0);
        assert_eq!(header.frame_counter.load(Ordering::Relaxed), 0);
        assert_eq!(header.width.load(Ordering::Relaxed), 0);
        assert_eq!(header.height.load(Ordering::Relaxed), 0);
    }

    assert_eq!(
        vga.present_to_shared_framebuffer(&mut output).expect("present 1"),
        true
    );

    {
        let view = output.view_mut();
        let header = view.header();
        assert_eq!(header.width.load(Ordering::Relaxed), 320);
        assert_eq!(header.height.load(Ordering::Relaxed), 200);
        assert_eq!(header.stride_bytes.load(Ordering::Relaxed), 320 * 4);
        assert_eq!(header.config_counter.load(Ordering::Relaxed), 1);
        assert_eq!(header.frame_counter.load(Ordering::Relaxed), 1);

        let px0 = u32::from_le_bytes(view.pixels()[0..4].try_into().unwrap());
        assert_eq!(px0, u32::from_le_bytes([255, 0, 0, 255]));
    }

    assert_eq!(
        vga.present_to_shared_framebuffer(&mut output).expect("present 2"),
        true
    );

    {
        let view = output.view_mut();
        let header = view.header();
        assert_eq!(header.config_counter.load(Ordering::Relaxed), 1);
        assert_eq!(header.frame_counter.load(Ordering::Relaxed), 2);
    }
}

