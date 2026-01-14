use aero_devices_gpu::vblank::period_ns_from_hz;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as proto;
use pretty_assertions::assert_eq;

fn read_u32(m: &mut Machine, bar0: u64, reg: u32) -> u32 {
    m.read_physical_u32(bar0 + u64::from(reg))
}

fn write_u32(m: &mut Machine, bar0: u64, reg: u32, value: u32) {
    m.write_physical_u32(bar0 + u64::from(reg), value);
}

fn read_u64_split(m: &mut Machine, bar0: u64, reg_lo: u32, reg_hi: u32) -> u64 {
    let lo = read_u32(m, bar0, reg_lo) as u64;
    let hi = read_u32(m, bar0, reg_hi) as u64;
    lo | (hi << 32)
}

#[test]
fn aerogpu_vblank_is_deterministic_and_survives_snapshot_restore() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the machine minimal and deterministic for the vblank timing test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    let bar0 = m
        .aerogpu_bar0_base()
        .expect("AeroGPU BAR0 should be assigned during PCI BIOS POST");

    let period_ns = read_u32(
        &mut m,
        bar0,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS,
    ) as u64;
    assert_eq!(
        period_ns,
        period_ns_from_hz(Some(60)).expect("60Hz vblank must be enabled by default")
    );

    // Vblank counters start at 0 before scanout is enabled.
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        0
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        0
    );
    assert_eq!(
        read_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_IRQ_STATUS),
        0
    );

    // Enable scanout0 so vblank ticks start.
    write_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE, 1);

    // Advance to just before the first vblank.
    m.tick_platform(period_ns - 1);
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        0
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        0
    );

    // Cross the first vblank boundary.
    m.tick_platform(1);
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        1
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns
    );

    // While vblank IRQ is masked, vblank ticks must not latch a pending status bit.
    m.tick_platform(period_ns);
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        2
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns * 2
    );
    assert_eq!(
        read_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_IRQ_STATUS),
        0
    );

    // Enable vblank IRQ cause bit. This must not immediately latch a stale IRQ.
    write_u32(
        &mut m,
        bar0,
        proto::AEROGPU_MMIO_REG_IRQ_ENABLE,
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );
    assert_eq!(
        read_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_IRQ_STATUS),
        0
    );

    // Next vblank should set IRQ_STATUS.
    m.tick_platform(period_ns);
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        3
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns * 3
    );
    assert_eq!(
        read_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_IRQ_STATUS)
            & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        proto::AEROGPU_IRQ_SCANOUT_VBLANK
    );

    // Ack the IRQ and ensure it clears.
    write_u32(
        &mut m,
        bar0,
        proto::AEROGPU_MMIO_REG_IRQ_ACK,
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );
    assert_eq!(
        read_u32(&mut m, bar0, proto::AEROGPU_MMIO_REG_IRQ_STATUS)
            & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        0
    );

    // Advance half a period (snapshot mid-frame).
    let half = period_ns / 2;
    m.tick_platform(half);
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        3
    );
    assert_eq!(
        read_u64_split(
            &mut m,
            bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns * 3
    );

    // Snapshot + restore into a fresh machine. Vblank timebase must remain coherent.
    let snap = m.take_snapshot_full().unwrap();
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    let restored_bar0 = restored
        .aerogpu_bar0_base()
        .expect("AeroGPU BAR0 should remain assigned after snapshot restore");
    assert_eq!(restored_bar0, bar0);

    assert_eq!(
        read_u64_split(
            &mut restored,
            restored_bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        3
    );
    assert_eq!(
        read_u64_split(
            &mut restored,
            restored_bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns * 3
    );

    // Tick the remaining half period to reach the next vblank boundary.
    restored.tick_platform(period_ns - half);
    assert_eq!(
        read_u64_split(
            &mut restored,
            restored_bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI
        ),
        4
    );
    assert_eq!(
        read_u64_split(
            &mut restored,
            restored_bar0,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI
        ),
        period_ns * 4
    );
    assert_eq!(
        read_u32(
            &mut restored,
            restored_bar0,
            proto::AEROGPU_MMIO_REG_IRQ_STATUS
        ) & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        proto::AEROGPU_IRQ_SCANOUT_VBLANK
    );
}
