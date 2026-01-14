use aero_cpu_core::state::gpr;
use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bda::BDA_CURSOR_SHAPE_ADDR;
use pretty_assertions::assert_eq;

fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    if hash == 0 {
        hash = FNV_OFFSET;
    }
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn framebuffer_hash(framebuffer: &[u32]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &px in framebuffer {
        hash = fnv1a64(hash, &px.to_ne_bytes());
    }
    hash
}

fn aerogpu_bar1_base(m: &Machine) -> u64 {
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();
    let cfg = bus
        .device_config(aero_devices::pci::profile::AEROGPU.bdf)
        .expect("AeroGPU device should be present when enable_aerogpu=true");
    cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .map(|range| range.base)
        .unwrap_or(0)
}

#[test]
fn aerogpu_legacy_text_window_is_aliased_into_bar1_and_renders() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    assert!(
        m.vga().is_none(),
        "AeroGPU path should not expose the standalone VGA device"
    );

    // Clear the full 32KiB legacy text buffer for a deterministic baseline.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);

    // Disable the cursor for deterministic output (cursor-start register bit 5) by patching the
    // BIOS Data Area cursor shape word.
    //
    // BDA cursor shape layout: [start: u8][end: u8] (start in high byte).
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x20_00);

    // Write "A" at the top-left cell with light grey on blue through legacy text memory.
    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);

    // The legacy VGA window (`0xA0000..0xC0000`) is expected to alias `BAR1_VRAM[0..128KiB]`.
    let bar1_base = aerogpu_bar1_base(&m);
    assert_ne!(bar1_base, 0, "AeroGPU BAR1 should have a non-zero base");
    let alias_off = 0xB8000u64 - 0xA0000u64;
    assert_eq!(m.read_physical_u8(bar1_base + alias_off), b'A');
    assert_eq!(m.read_physical_u8(bar1_base + alias_off + 1), 0x1F);

    m.display_present();
    assert_eq!(m.display_resolution(), (720, 400));
    assert_eq!(framebuffer_hash(m.display_framebuffer()), 0x5cfe440e33546065);
}

fn build_int10_vbe_set_mode_boot_sector(vbe_mode_with_flags: u16) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, imm16 (requested VBE mode + flags, e.g. LFB bit)
    sector[i..i + 3].copy_from_slice(&[
        0xBB,
        (vbe_mode_with_flags & 0x00FF) as u8,
        (vbe_mode_with_flags >> 8) as u8,
    ]);
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
fn aerogpu_vbe_lfb_is_visible_via_bar1_after_int10_set_mode() {
    // Mode 0x118 + LFB requested (1024x768x32).
    let boot = build_int10_vbe_set_mode_boot_sector(0x4118);

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let vbe_status = (m.cpu().gpr[gpr::RAX] & 0xFFFF) as u16;
    assert_eq!(
        vbe_status, 0x004F,
        "VBE set-mode should return AX=0x004F (success)"
    );

    let bar1_base = aerogpu_bar1_base(&m);
    assert_ne!(bar1_base, 0, "AeroGPU BAR1 should have a non-zero base");

    // The BIOS should report the VBE LFB within BAR1, offset past the legacy VGA alias window.
    const VBE_LFB_OFFSET: u64 = aero_machine::VBE_LFB_OFFSET as u64;
    let lfb_base = u64::from(m.vbe_lfb_base());
    assert_eq!(lfb_base, bar1_base + VBE_LFB_OFFSET);

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    m.write_physical_u32(lfb_base, 0x00FF_0000);

    // Ensure the write landed in BAR1 VRAM.
    assert_eq!(m.read_physical_u8(bar1_base + VBE_LFB_OFFSET + 2), 0xFF);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
