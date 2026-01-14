use aero_devices::pci::profile;
use aero_devices_gpu::regs::{irq_bits, mmio};
use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_vblank_counter_advances_when_platform_time_advances() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal for this unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_vga: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Ensure BARs + command register are programmed so MMIO accesses behave like real PCI hardware
    // (MEM decode + bus mastering enabled).
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        bus.write_config(profile::AEROGPU.bdf, 0x04, 2, (1 << 1) | (1 << 2));

        bus.device_config(profile::AEROGPU.bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .map(|range| range.base)
            .unwrap_or(0)
    };
    assert_ne!(
        bar0_base, 0,
        "expected AeroGPU BAR0 base assigned by BIOS POST"
    );

    // Enable scanout0 so vblank ticks are generated, and enable vblank IRQ latching.
    m.write_physical_u32(bar0_base + mmio::SCANOUT0_ENABLE, 1);
    m.write_physical_u32(bar0_base + mmio::IRQ_ENABLE, irq_bits::SCANOUT_VBLANK);

    let seq_before = {
        let lo = m.read_physical_u32(bar0_base + mmio::SCANOUT0_VBLANK_SEQ_LO) as u64;
        let hi = m.read_physical_u32(bar0_base + mmio::SCANOUT0_VBLANK_SEQ_HI) as u64;
        lo | (hi << 32)
    };

    // Advance deterministic platform time by ~20ms; this should cross at least one 60 Hz vblank
    // period and increment the counter.
    m.tick_platform(20_000_000);

    let seq_after = {
        let lo = m.read_physical_u32(bar0_base + mmio::SCANOUT0_VBLANK_SEQ_LO) as u64;
        let hi = m.read_physical_u32(bar0_base + mmio::SCANOUT0_VBLANK_SEQ_HI) as u64;
        lo | (hi << 32)
    };
    assert!(
        seq_after > seq_before,
        "expected vblank seq to advance (before={seq_before}, after={seq_after})"
    );

    let irq_status = m.read_physical_u32(bar0_base + mmio::IRQ_STATUS);
    assert_ne!(
        irq_status & irq_bits::SCANOUT_VBLANK,
        0,
        "expected vblank IRQ status bit to be set after vblank tick (irq_status=0x{irq_status:08x})"
    );
}
