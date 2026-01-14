use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource, VBE_LFB_OFFSET};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use firmware::bda::{BDA_CURSOR_SHAPE_ADDR, BDA_VIDEO_PAGE_OFFSET_ADDR};
use pretty_assertions::assert_eq;

const WAIT_FLAG_PADDR: u64 = 0x0500;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn build_boot_sector_vbe_then_wait_then_text_mode() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov byte ptr [0x0500], 0x01
    sector[i..i + 2].copy_from_slice(&[0xC6, 0x06]);
    i += 2;
    sector[i..i + 2].copy_from_slice(&(WAIT_FLAG_PADDR as u16).to_le_bytes());
    i += 2;
    sector[i] = 0x01;
    i += 1;

    // wait:
    //   cmp byte ptr [0x0500], 0x02
    //   jne wait
    let loop_start = i;
    sector[i..i + 2].copy_from_slice(&[0x80, 0x3E]);
    i += 2;
    sector[i..i + 2].copy_from_slice(&(WAIT_FLAG_PADDR as u16).to_le_bytes());
    i += 2;
    sector[i] = 0x02;
    i += 1;
    // jne rel8
    sector[i] = 0x75;
    i += 1;
    let rel8_off = i;
    sector[i] = 0x00; // patched below
    i += 1;

    // mov ax, 0x0003 (set text mode 03h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x03, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    // Patch the JNE to loop_start.
    // The `jne rel8` offset is relative to the instruction *after* the displacement byte.
    let ip_after_jne = rel8_off + 1;
    let rel = loop_start as isize - ip_after_jne as isize;
    sector[rel8_off] = i8::try_from(rel).expect("jne rel8 must fit") as u8;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_flag(m: &mut Machine, value: u8) {
    for _ in 0..200 {
        let _ = m.run_slice(50_000);
        if m.read_physical_u8(WAIT_FLAG_PADDR) == value {
            return;
        }
    }
    panic!("guest did not reach flag value {value}");
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

#[test]
fn aerogpu_scanout_handoff_to_wddm_blocks_legacy_int10_steal() {
    let boot = build_boot_sector_vbe_then_wait_then_text_mode();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Legacy boot text path renders via `display_present` before the guest enables AeroGPU WDDM scanout.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    // Disable cursor for deterministic output (cursor start CH bit5 = 1).
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    m.display_present();
    let legacy_text_res = m.display_resolution();
    assert_ne!(legacy_text_res, (0, 0));

    // Wait for the guest to reach the wait loop after setting VBE mode.
    run_until_flag(&mut m, 0x01);
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
    // Ensure the guest is actually waiting (and not already halted due to a malformed loop jump).
    if matches!(m.run_slice(1), RunExit::Halted { .. }) {
        panic!("guest halted unexpectedly before host released the wait loop");
    }

    enable_a20(&mut m);

    // Program a scratch scanout in guest RAM and claim scanout via AeroGPU BAR0.
    let scratch_base = 0x0020_0000u64;
    let width = 64u32;
    let height = 64u32;
    let pitch = width * 4;

    // Pixel (0,0): B,G,R,X = AA,BB,CC,00.
    m.write_physical_u8(scratch_base, 0xAA);
    m.write_physical_u8(scratch_base + 1, 0xBB);
    m.write_physical_u8(scratch_base + 2, 0xCC);
    m.write_physical_u8(scratch_base + 3, 0x00);

    let (bar0_base, bar1_base, _command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        (
            cfg.bar_range(profile::AEROGPU_BAR0_INDEX)
                .expect("AeroGPU BAR0 must exist")
                .base,
            cfg.bar_range(profile::AEROGPU_BAR1_VRAM_INDEX)
                .expect("AeroGPU BAR1 must exist")
                .base,
            cfg.command(),
        )
    };
    assert_ne!(bar0_base, 0);
    assert_ne!(bar1_base, 0);

    // Enable BAR0 decoding + bus mastering (WDDM driver would enable this before programming
    // scanout).
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(profile::AEROGPU.bdf, 0x04, 2) as u16;
        // Enable MEM decode + bus mastering so BAR0 MMIO and scanout DMA behave like a real PCI
        // device.
        bus.write_config(profile::AEROGPU.bdf, 0x04, 2, u32::from(command | 0x0006));
    }
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scratch_base as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scratch_base >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    // Host-side scanout reads are gated by PCI COMMAND.BME to match device DMA semantics; emulate
    // what the WDDM driver does during start-device.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        cfg.set_command(cfg.command() | (1 << 2));
    }

    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Scribble into the legacy VBE LFB (BAR1 + fixed offset) to ensure it does not steal scanout
    // after WDDM has claimed it.
    let legacy_lfb_base = bar1_base + VBE_LFB_OFFSET as u64;
    m.write_physical_u8(legacy_lfb_base, 0x00);
    m.write_physical_u8(legacy_lfb_base + 1, 0x00);
    m.write_physical_u8(legacy_lfb_base + 2, 0xFF);
    m.write_physical_u8(legacy_lfb_base + 3, 0x00);

    // Let the guest continue. It will call INT10 to switch back to text mode, which should update
    // the BIOS legacy mode state, but must *not* steal the active WDDM scanout.
    m.write_physical_u8(WAIT_FLAG_PADDR, 0x02);
    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Legacy text memory changes must remain hidden while WDDM owns scanout.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m.write_physical_u8(0xB8000, b'Z');
    m.write_physical_u8(0xB8001, 0x1F);
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Once WDDM has claimed scanout, legacy VGA/VBE sources are ignored until reset. Disabling
    // scanout (`SCANOUT0_ENABLE=0`) acts as a visibility toggle: the machine presents a blank frame
    // while WDDM ownership remains sticky.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        0,
    );
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // Legacy text memory changes must remain hidden while WDDM owns scanout.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m.write_physical_u8(0xB8000, b'Y');
    m.write_physical_u8(0xB8001, 0x1F);
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // Reset returns scanout ownership to legacy.
    m.reset();
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m.write_physical_u8(0xB8000, b'R');
    m.write_physical_u8(0xB8001, 0x1F);
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    m.display_present();
    assert_eq!(m.display_resolution(), legacy_text_res);
    assert!(!m.display_framebuffer().is_empty());
}
