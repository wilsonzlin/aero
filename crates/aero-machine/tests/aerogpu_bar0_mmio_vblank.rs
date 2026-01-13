use aero_devices::pci::{PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as proto;
use pretty_assertions::assert_eq;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn build_hlt_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    // sti; hlt; jmp short $-3 (back to hlt)
    sector[0] = 0xFB;
    sector[1] = 0xF4;
    sector[2..4].copy_from_slice(&[0xEB, 0xFD]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn aerogpu_bar0_mmio_magic_and_vblank_irq() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(build_hlt_boot_sector().to_vec()).unwrap();
    m.reset();

    let bdf = aero_devices::pci::profile::AEROGPU.bdf;

    // Ensure PCI MEM decoding is enabled for BAR0 MMIO.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command | 0x0002));

    let bar0 = cfg_read(&mut m, bdf, 0x10, 4) & 0xffff_fff0;
    assert_ne!(
        bar0, 0,
        "AeroGPU BAR0 base should be allocated by BIOS POST"
    );
    let bar0 = u64::from(bar0);

    // BAR0 MMIO identity.
    let magic = m.read_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_MAGIC));
    assert_eq!(magic, proto::AEROGPU_MMIO_MAGIC);

    // Program scanout0 + enable vblank IRQ delivery.
    let width = 64u32;
    let height = 64u32;
    let pitch = width * 4;
    let fb_gpa = 0x0010_0000u64;

    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        proto::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ENABLE),
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );
    m.write_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // The PCI INTx router uses a swizzle to map device/pin pairs onto GSIs.
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    let interrupts = m.platform_interrupts().expect("pc platform enabled");

    // Advance time deterministically via the machine's HLT idle tick path until a vblank edge
    // arrives.
    for _ in 0..64 {
        let _ = m.run_slice(1_000);
        if interrupts.borrow().gsi_level(gsi) {
            return;
        }
    }

    panic!("AeroGPU vblank IRQ never asserted (GSI={gsi})");
}
