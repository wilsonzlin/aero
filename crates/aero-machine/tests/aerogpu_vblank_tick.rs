use aero_devices::a20_gate::A20_GATE_PORT;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_protocol::aerogpu::aerogpu_pci as proto;

fn read_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32) -> u64 {
    let lo = m.read_physical_u32(base + u64::from(lo));
    let hi = m.read_physical_u32(base + u64::from(hi));
    u64::from(lo) | (u64::from(hi) << 32)
}

#[test]
fn aerogpu_vblank_ticks_while_halted_and_irq_is_latched_only_when_enabled() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for vblank timing.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Boot sector: STI; HLT; JMP $-3 (halt loop).
    let mut sector = [0u8; 512];
    sector[0] = 0xFB; // sti
    sector[1] = 0xF4; // hlt
    sector[2] = 0xEB; // jmp short
    sector[3] = 0xFD; // -3 (back to hlt)
    sector[510] = 0x55;
    sector[511] = 0xAA;
    m.set_disk_image(sector.to_vec()).unwrap();
    m.reset();

    // Enable A20 so MMIO addresses with bit20 set are not masked.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Discover AeroGPU BAR0 base (assigned by `bios_post`).
    let aerogpu_bdf = aero_devices::pci::profile::AEROGPU.bdf;
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(aerogpu_bdf)
            .expect("AeroGPU device missing from PCI bus");
        cfg.bar_range(0)
            .map(|range| range.base)
            .expect("AeroGPU BAR0 missing")
    };
    assert_ne!(
        bar0_base, 0,
        "expected AeroGPU BAR0 to be assigned by PCI POST"
    );

    // Enable scanout0 and ensure vblank IRQ delivery is disabled initially.
    m.write_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );
    m.write_physical_u32(bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ENABLE), 0);
    // Clear any latched status from earlier ticks.
    m.write_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ACK),
        u32::MAX,
    );

    // Run until the CPU reaches the HLT loop so `run_slice(1)` advances time via
    // `idle_tick_platform_1ms` (deterministically).
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for HLT: {other:?}"),
        }
    }

    let seq0 = read_mmio_u64(
        &mut m,
        bar0_base,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    let irq0 = m.read_physical_u32(bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(
        irq0 & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        0,
        "expected vblank IRQ_STATUS bit to remain clear while vblank IRQ is disabled"
    );

    // Advance enough time to cross at least one vblank interval while the CPU is halted.
    let period_ns = u64::from(m.read_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS),
    ));
    assert_ne!(period_ns, 0, "test requires vblank pacing to be active");
    const NS_PER_MS: u64 = 1_000_000;
    let ticks_needed = period_ns.div_ceil(NS_PER_MS) as usize;
    for _ in 0..ticks_needed {
        assert!(
            matches!(m.run_slice(1), RunExit::Halted { executed: 0 }),
            "expected CPU to remain halted while advancing platform time"
        );
    }

    let seq1 = read_mmio_u64(
        &mut m,
        bar0_base,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    assert!(
        seq1 > seq0,
        "expected vblank sequence to advance while halted (before={seq0}, after={seq1})"
    );
    let irq1 = m.read_physical_u32(bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(
        irq1 & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        0,
        "expected vblank IRQ_STATUS bit to remain clear while vblank IRQ is disabled"
    );

    // Enable vblank IRQ delivery. The device should only latch IRQ_STATUS on vblank edges that
    // occur while enabled.
    m.write_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ENABLE),
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );

    for _ in 0..ticks_needed {
        assert!(
            matches!(m.run_slice(1), RunExit::Halted { executed: 0 }),
            "expected CPU to remain halted while advancing platform time"
        );
    }

    let seq2 = read_mmio_u64(
        &mut m,
        bar0_base,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    assert!(
        seq2 > seq1,
        "expected vblank sequence to keep advancing while halted (before={seq1}, after={seq2})"
    );
    let irq2 = m.read_physical_u32(bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(
        irq2 & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        0,
        "expected vblank IRQ_STATUS bit to latch after enabling vblank IRQ delivery"
    );
}
