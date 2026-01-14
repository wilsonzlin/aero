use aero_devices::a20_gate::A20_GATE_PORT;
use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT};
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

fn build_int10_vbe_set_mode_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn program_vbe_linear_64x64x32(m: &mut Machine) {
    // Match the programming sequence used by `aero-gpu-vga`'s
    // `vbe_linear_framebuffer_write_shows_up_in_output` test.
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 64);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 64);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 32);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 0x0041);
}

#[test]
fn vga_vbe_lfb_is_reachable_via_pci_mmio_router() {
    // Use a non-default base *outside the BIOS PCI BAR allocator default window*
    // (`PciResourceAllocatorConfig::default().mmio_base..+mmio_size`, currently 0xE000_0000..0xF000_0000)
    // to ensure the PCI MMIO router path doesn't have a hidden dependency on
    // `aero_gpu_vga::SVGA_LFB_BASE` or the allocator window.
    let lfb_base: u32 = 0xD000_0000;
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(lfb_base),
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();
    // This test covers the standalone legacy VGA/VBE device model wired into the canonical PC
    // platform. If a machine configuration uses AeroGPU-owned boot display (no standalone VGA
    // device), skip.
    if m.vga().is_none() {
        return;
    }

    program_vbe_linear_64x64x32(&mut m);

    // Always use the firmware-reported VBE PhysBasePtr so this test stays robust if the LFB base
    // changes (e.g. config-driven legacy VGA LFB vs AeroGPU BAR1-backed legacy VBE).
    let base = m.vbe_lfb_base();
    assert_eq!(base, u64::from(lfb_base));
    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}

#[test]
fn vga_vbe_lfb_base_can_be_derived_from_vram_bar_base_and_lfb_offset() {
    // Exercise the optional VRAM layout knobs:
    // - `vga_vram_bar_base`: conceptual VRAM aperture base (vram[0])
    // - `vga_lfb_offset`: offset within VRAM where the packed-pixel VBE framebuffer begins
    //
    // The effective VBE LFB base should be:
    // `lfb_base = vga_vram_bar_base + vga_lfb_offset`.
    // Use a derived base *outside* the BIOS PCI BAR allocator default window
    // (`PciResourceAllocatorConfig::default().mmio_base..+mmio_size`, currently
    // `0xE000_0000..0xF000_0000`) to ensure we exercise the full PCI MMIO router mapping rather
    // than accidentally relying on the allocator sub-window.
    let lfb_offset: u32 = 0x0002_0000; // 128KiB
    let vram_bar_base: u32 = 0xCFFE_0000;
    let expected_lfb_base: u32 = 0xD000_0000;
    assert_eq!(
        vram_bar_base.wrapping_add(lfb_offset),
        expected_lfb_base,
        "test invariant: derived LFB base mismatch"
    );

    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_offset: Some(lfb_offset),
        vga_vram_bar_base: Some(vram_bar_base),
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();
    // This test covers the standalone legacy VGA/VBE device model wired into the canonical PC
    // platform. If a machine configuration uses AeroGPU-owned boot display (no standalone VGA
    // device), skip.
    if m.vga().is_none() {
        return;
    }

    let vga_cfg = m.vga().expect("VGA enabled").borrow().config();
    assert_eq!(vga_cfg.lfb_offset, lfb_offset);
    assert_eq!(vga_cfg.vram_bar_base, vram_bar_base);
    assert_eq!(vga_cfg.lfb_base(), expected_lfb_base);

    program_vbe_linear_64x64x32(&mut m);

    // Always use the firmware-reported VBE PhysBasePtr so this test stays robust if the LFB base
    // changes (e.g. standalone VGA stub vs AeroGPU BAR1-backed legacy VBE).
    let base = m.vbe_lfb_base();
    assert_eq!(base, u64::from(expected_lfb_base));

    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}

#[test]
fn aerogpu_vbe_lfb_is_reachable_via_pci_mmio_router() {
    let boot = build_int10_vbe_set_mode_boot_sector();
    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    enable_a20(&mut m);

    // Always use the firmware-reported VBE PhysBasePtr so this test stays robust if the LFB base
    // changes (e.g. config-driven legacy VGA LFB vs AeroGPU BAR1-backed legacy VBE).
    let base = m.vbe_lfb_base();
    let bar1_base = m
        .aerogpu_vram_bar_base()
        .expect("expected AeroGPU BAR1 to be present");
    assert_eq!(base, bar1_base + VBE_LFB_OFFSET as u64);
    m.write_physical_u32(base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
