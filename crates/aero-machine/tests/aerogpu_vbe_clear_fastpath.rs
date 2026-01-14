use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};

fn build_int10_vbe_set_mode_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested, clear requested)
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

fn build_int10_vbe_set_mode_no_clear_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0xC118 (mode 0x118 + LFB requested, no-clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0xC1]);
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

fn build_int10_vbe_failed_mode_set_does_not_clear_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0xC118 (mode 0x118 + LFB requested + no-clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0xC1]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Attempt to set an invalid mode (should fail). Use no-clear=0 so the machine-side
    // synchronization must not accidentally clear the *current* framebuffer when the BIOS call
    // fails.
    //
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x3FFF (invalid mode; no-clear bit 15 is clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0xFF, 0x3F]);
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

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn aerogpu_vbe_mode_set_with_clear_fast_clears_vram_backing() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Resolve the AeroGPU BAR1 base assigned by BIOS POST and compute the VBE LFB physical base.
    let bdf = m
        .aerogpu_bdf()
        .expect("AeroGPU should be present when enable_aerogpu=true");
    let bar1_base = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .unwrap_or(0);
    assert_ne!(bar1_base, 0, "missing/zero AeroGPU BAR1 base");
    let lfb_base = m.vbe_lfb_base();
    assert_eq!(lfb_base, bar1_base + VBE_LFB_OFFSET as u64);

    // Mode 0x118 is 1024x768x32bpp. Pre-fill a few bytes within the VBE LFB region with a
    // non-zero pattern so the test detects whether the region was cleared.
    const WIDTH: usize = 1024;
    const HEIGHT: usize = 768;
    const BYTES_PER_PIXEL: usize = 4;
    let clear_len = WIDTH * HEIGHT * BYTES_PER_PIXEL;

    const PATTERN: [u8; 16] = [0xA5; 16];
    // Also fill a prefix in BAR1 before the LFB offset; this must *not* be affected by the clear.
    let legacy_addr = bar1_base;
    let end_addr = lfb_base + clear_len as u64 - PATTERN.len() as u64;
    let mid_addr = lfb_base + (clear_len as u64 / 2);
    m.write_physical(legacy_addr, &PATTERN);
    m.write_physical(lfb_base, &PATTERN);
    m.write_physical(mid_addr, &PATTERN);
    m.write_physical(end_addr, &PATTERN);

    run_until_halt(&mut m);

    assert_eq!(
        m.read_physical_bytes(legacy_addr, PATTERN.len()),
        PATTERN,
        "expected VRAM region before VBE_LFB_OFFSET to be preserved"
    );
    assert_eq!(
        m.read_physical_bytes(lfb_base, PATTERN.len()),
        vec![0; PATTERN.len()],
        "expected AeroGPU VBE LFB start to be cleared to zeros"
    );
    assert_eq!(
        m.read_physical_bytes(mid_addr, PATTERN.len()),
        vec![0; PATTERN.len()],
        "expected AeroGPU VBE LFB middle to be cleared to zeros"
    );
    assert_eq!(
        m.read_physical_bytes(end_addr, PATTERN.len()),
        vec![0; PATTERN.len()],
        "expected AeroGPU VBE LFB end to be cleared to zeros"
    );
}

#[test]
fn aerogpu_vbe_failed_mode_set_does_not_clear_existing_framebuffer() {
    let boot = build_int10_vbe_failed_mode_set_does_not_clear_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Resolve the AeroGPU BAR1 base assigned by BIOS POST and compute the VBE LFB physical base.
    let bdf = m
        .aerogpu_bdf()
        .expect("AeroGPU should be present when enable_aerogpu=true");
    let bar1_base = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .unwrap_or(0);
    assert_ne!(bar1_base, 0, "missing/zero AeroGPU BAR1 base");
    let lfb_base = m.vbe_lfb_base();
    assert_eq!(lfb_base, bar1_base + VBE_LFB_OFFSET as u64);

    const PATTERN: [u8; 16] = [0xA5; 16];
    m.write_physical(lfb_base, &PATTERN);

    run_until_halt(&mut m);

    assert_eq!(
        m.read_physical_bytes(lfb_base, PATTERN.len()),
        PATTERN,
        "expected failing VBE mode set to preserve existing VRAM framebuffer contents"
    );
}

#[test]
fn aerogpu_vbe_mode_set_with_no_clear_preserves_existing_framebuffer() {
    let boot = build_int10_vbe_set_mode_no_clear_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Resolve the AeroGPU BAR1 base assigned by BIOS POST and compute the VBE LFB physical base.
    let bdf = m
        .aerogpu_bdf()
        .expect("AeroGPU should be present when enable_aerogpu=true");
    let bar1_base = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .unwrap_or(0);
    assert_ne!(bar1_base, 0, "missing/zero AeroGPU BAR1 base");
    let lfb_base = m.vbe_lfb_base();
    assert_eq!(lfb_base, bar1_base + VBE_LFB_OFFSET as u64);

    const PATTERN: [u8; 16] = [0xA5; 16];
    m.write_physical(lfb_base, &PATTERN);

    run_until_halt(&mut m);

    assert_eq!(
        m.read_physical_bytes(lfb_base, PATTERN.len()),
        PATTERN,
        "expected VBE mode set with no-clear to preserve existing VRAM framebuffer contents"
    );
}
