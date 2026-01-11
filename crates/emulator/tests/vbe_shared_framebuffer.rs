use std::sync::atomic::Ordering;

use emulator::devices::vga::{Vga, VgaSharedFramebufferOutput};

#[test]
fn vbe_present_to_shared_framebuffer_converts_bgra_to_rgba_and_forces_alpha() {
    let mut vga = Vga::new();
    vga.regs_mut()
        .set_mode(0x112 | 0x4000)
        .expect("set VBE mode");

    // Write a red pixel into the VBE LFB. VBE 32bpp pixels are conventionally little-endian
    // 0xAARRGGBB, i.e. bytes are BGRA. Guest software often leaves A/reserved as 0.
    vga.regs_mut().lfb_write(0, &[0, 0, 255, 0]);

    let mut output =
        VgaSharedFramebufferOutput::new(1024, 768).expect("allocate shared framebuffer");
    assert_eq!(
        vga.present_to_shared_framebuffer(&mut output)
            .expect("present VBE frame"),
        true
    );

    let view = output.view_mut();
    let header = view.header();
    assert_eq!(header.width.load(Ordering::Relaxed), 640);
    assert_eq!(header.height.load(Ordering::Relaxed), 480);
    assert_eq!(header.stride_bytes.load(Ordering::Relaxed), 640 * 4);
    assert_eq!(header.config_counter.load(Ordering::Relaxed), 1);
    assert_eq!(header.frame_counter.load(Ordering::Relaxed), 1);

    let px0 = u32::from_le_bytes(view.pixels()[0..4].try_into().unwrap());
    assert_eq!(px0, u32::from_le_bytes([255, 0, 0, 255]));
}
