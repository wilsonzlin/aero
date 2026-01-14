use aero_devices::clock::Clock as _;
use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

fn read_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32) -> u64 {
    let lo = m.read_physical_u32(base + u64::from(lo));
    let hi = m.read_physical_u32(base + u64::from(hi));
    u64::from(lo) | (u64::from(hi) << 32)
}

#[test]
fn enabling_scanout_does_not_retroactively_catch_up_vblank_ticks() {
    // Vblank ticks should only occur while scanout is enabled. If the deterministic platform clock
    // advances while scanout is disabled (e.g. a test calls `platform_clock().advance_ns(...)`
    // directly), enabling scanout must not immediately "catch up" all elapsed vblanks.
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    let bdf = m.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI MMIO decode + bus mastering so BAR0 accesses route to the device and it can DMA.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 1) | (1 << 2));
    }

    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    assert_ne!(period_ns, 0, "test requires vblank pacing support");

    // Advance the shared deterministic platform clock without ticking the machine.
    let clock = m.platform_clock().expect("pc platform enabled");
    clock.advance_ns(period_ns * 10);

    // Enable scanout. This must not retroactively catch up vblank ticks.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.process_aerogpu();

    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        0
    );
    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        0
    );

    // Advance to just before the first post-enable vblank.
    clock.advance_ns(period_ns - 1);
    m.process_aerogpu();
    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        0
    );

    // Cross the first vblank boundary and ensure the counter advances exactly once.
    clock.advance_ns(1);
    m.process_aerogpu();
    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        1
    );
    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        clock.now_ns()
    );
}
