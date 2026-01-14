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

#[test]
fn enabling_vblank_irq_does_not_deliver_stale_interrupt_from_catchup_ticks() {
    // Mirror emulator semantics: enabling vblank IRQ delivery must not immediately assert due to
    // catch-up vblank ticks that occurred while masked.
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

    m.reset();

    let bdf = aero_devices::pci::profile::AEROGPU.bdf;

    // Enable PCI MEM decoding for BAR0 MMIO.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command | 0x0002));

    let bar0 = cfg_read(&mut m, bdf, 0x10, 4) & 0xffff_fff0;
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");
    let bar0 = u64::from(bar0);

    // Enable scanout to start the vblank scheduler.
    m.write_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.poll_pci_intx_lines();

    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    assert_ne!(period_ns, 0, "test requires vblank pacing to be active");

    // Advance guest time well past the next-vblank deadline without polling vblank.
    m.tick_platform(period_ns * 3 + 1);

    // Enable vblank IRQ delivery. The device must catch up its vblank clock *before* enabling
    // IRQ latching so old vblanks while masked do not immediately appear as a pending IRQ.
    m.write_physical_u32(
        bar0 + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ENABLE),
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    let interrupts = m.platform_interrupts().expect("pc platform enabled");

    // Polling immediately after enable must not deliver a stale IRQ.
    m.poll_pci_intx_lines();
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "vblank IRQ should not assert immediately after enabling"
    );
    let irq_status = m.read_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & proto::AEROGPU_IRQ_SCANOUT_VBLANK, 0);

    // Once time advances past the next vblank edge, a vblank IRQ should be delivered.
    m.tick_platform(period_ns);
    m.poll_pci_intx_lines();
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected vblank IRQ to assert after the next vblank"
    );
    let irq_status = m.read_physical_u32(bar0 + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & proto::AEROGPU_IRQ_SCANOUT_VBLANK, 0);
}
