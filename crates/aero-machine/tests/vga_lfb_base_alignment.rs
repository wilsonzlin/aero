use aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET;
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_lfb_base_is_aligned_down_to_vga_pci_bar_size() {
    // Deliberately pick an *unaligned* base inside the PCI MMIO window so the machine must mask it
    // down to satisfy the VGA PCI BAR alignment requirement.
    let requested_lfb_base: u32 = 0xD000_1000;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(requested_lfb_base),
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let vga = m.vga().expect("VGA enabled");
    let bar_size_bytes: u32 = vga
        .borrow()
        .vram_size()
        .try_into()
        .expect("VRAM size fits in u32");
    assert!(bar_size_bytes.is_power_of_two());

    let expected_aligned_base = requested_lfb_base & !(bar_size_bytes - 1);
    assert_eq!(m.vbe_lfb_base_u32(), expected_aligned_base);
    assert_eq!(vga.borrow().lfb_base(), expected_aligned_base);

    // Program Bochs VBE_DISPI to 64x64x32 with LFB enabled.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(u64::from(expected_aligned_base), 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}

#[test]
fn vga_lfb_base_alignment_respects_rounded_vram_size() {
    // Request a VRAM size that is *not* a power of two; the machine should round it up for PCI
    // BAR sizing and align the LFB base accordingly.
    let requested_vram_size_bytes: usize = 24 * 1024 * 1024;
    let requested_lfb_base: u32 = 0xD234_5678;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(requested_lfb_base),
        vga_vram_size_bytes: Some(requested_vram_size_bytes),
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let vga = m.vga().expect("VGA enabled");
    let bar_size_bytes: u32 = vga
        .borrow()
        .vram_size()
        .try_into()
        .expect("VRAM size fits in u32");
    assert!(bar_size_bytes.is_power_of_two());
    assert_eq!(bar_size_bytes, 32 * 1024 * 1024);

    let expected_aligned_base = requested_lfb_base & !(bar_size_bytes - 1);
    assert_eq!(m.vbe_lfb_base_u32(), expected_aligned_base);
    assert_eq!(vga.borrow().lfb_base(), expected_aligned_base);

    // Program Bochs VBE_DISPI to 64x64x32 with LFB enabled.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    m.write_physical_u32(u64::from(expected_aligned_base), 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}

#[test]
fn vga_lfb_base_alignment_applies_to_vram_bar_base_plus_lfb_offset() {
    // When the LFB base is derived as (vram_bar_base + lfb_offset), the machine should still align
    // the resulting LFB base down to the legacy VGA PCI BAR size.
    let requested_vram_bar_base: u32 = 0xCFFC_1000;
    let lfb_offset: u32 = VBE_FRAMEBUFFER_OFFSET as u32;
    let requested_lfb_base = requested_vram_bar_base.wrapping_add(lfb_offset);
    assert_eq!(requested_lfb_base, 0xD000_1000);

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_vram_bar_base: Some(requested_vram_bar_base),
        vga_lfb_offset: Some(lfb_offset),
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let vga = m.vga().expect("VGA enabled");
    let bar_size_bytes: u32 = vga
        .borrow()
        .vram_size()
        .try_into()
        .expect("VRAM size fits in u32");
    assert!(bar_size_bytes.is_power_of_two());

    let expected_aligned_base = requested_lfb_base & !(bar_size_bytes - 1);
    assert_eq!(m.vbe_lfb_base_u32(), expected_aligned_base);
    assert_eq!(vga.borrow().lfb_base(), expected_aligned_base);

    // Ensure the device's derived VRAM layout stays coherent even after alignment masking.
    let vga_cfg = vga.borrow().config();
    assert_eq!(vga_cfg.lfb_offset, lfb_offset);
    assert_eq!(vga_cfg.lfb_base(), expected_aligned_base);
    assert_eq!(
        vga_cfg.vram_bar_base,
        expected_aligned_base.wrapping_sub(lfb_offset)
    );
    assert_ne!(vga_cfg.vram_bar_base, requested_vram_bar_base);

    // Program Bochs VBE_DISPI to 64x64x32 with LFB enabled.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    m.write_physical_u32(u64::from(expected_aligned_base), 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
