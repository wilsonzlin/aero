use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

const VBE_LFB_OFFSET: u64 = aero_machine::VBE_LFB_OFFSET as u64;
const WAIT_FLAG_PADDR: u64 = 0x0500;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn build_boot_sector_vbe_then_wait_then_text_mode() -> [u8; 512] {
    let mut sector = [0u8; 512];
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
    i += 1;

    // Patch the JNE to loop_start.
    let rel = loop_start as isize - i as isize;
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

    // Wait for the guest to reach the wait loop after setting VBE mode.
    run_until_flag(&mut m, 0x01);

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

    let (bar0_base, bar1_base, command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        // The WDDM scanout path reads from guest RAM (device-initiated DMA), so it requires PCI Bus
        // Master Enable (BME). BIOS POST intentionally leaves BME disabled by default.
        cfg.set_command(cfg.command() | (1 << 2));
        (
            cfg.bar_range(0).expect("AeroGPU BAR0 must exist").base,
            cfg.bar_range(1).expect("AeroGPU BAR1 must exist").base,
            cfg.command(),
        )
    };
    assert_ne!(bar0_base, 0);
    assert_ne!(bar1_base, 0);

    // Scanout reads behave like device-initiated DMA; enable PCI Bus Master Enable (BME) so the
    // host-side display_present path can legally read the scanout buffer.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .write_config(profile::AEROGPU.bdf, 0x04, 2, u32::from(command | (1 << 2)));
    }

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), width);
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
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    m.display_present();
    assert_eq!(m.display_resolution(), (u32::from(width), u32::from(height)));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Scribble into the legacy VBE LFB (BAR1 + fixed offset) to ensure it does not steal scanout
    // after WDDM has claimed it.
    let legacy_lfb_base = bar1_base + VBE_LFB_OFFSET;
    m.write_physical_u8(legacy_lfb_base, 0x00);
    m.write_physical_u8(legacy_lfb_base + 1, 0x00);
    m.write_physical_u8(legacy_lfb_base + 2, 0xFF);
    m.write_physical_u8(legacy_lfb_base + 3, 0x00);

    // Let the guest continue. It will call INT10 to switch back to text mode, which should update
    // the BIOS legacy mode state, but must *not* steal the active WDDM scanout.
    m.write_physical_u8(WAIT_FLAG_PADDR, 0x02);
    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.display_resolution(), (u32::from(width), u32::from(height)));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);
}
