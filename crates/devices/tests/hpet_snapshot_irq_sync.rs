use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::{GsiEvent, IoApic};
use aero_io_snapshot::io::state::IoSnapshot;

const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
const HPET_REG_TIMER0_BASE: u64 = 0x100;
const HPET_REG_TIMER_CONFIG: u64 = 0x00;
const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

#[test]
fn hpet_snapshot_restore_sync_levels_to_sink_redrives_pending_level_irq() {
    let clock = ManualClock::new();
    let mut sink = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(HPET_REG_GENERAL_CONFIG, 8, HPET_GEN_CONF_ENABLE, &mut sink);
    let timer0_cfg = hpet.mmio_read(HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG, 8, &mut sink);
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg | HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL,
        &mut sink,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        1,
        &mut sink,
    );

    // Fire the interrupt so the status bit is set and the GSI is asserted.
    clock.advance_ns(100);
    hpet.poll(&mut sink);
    assert!(sink.is_asserted(2));

    let snap = hpet.save_state();

    let clock2 = ManualClock::new();
    clock2.set_ns(42_000_000);
    let mut sink2 = IoApic::default();
    let mut restored = Hpet::new_default(clock2.clone());
    restored.load_state(&snap).unwrap();

    assert!(
        !sink2.is_asserted(2),
        "restore does not have access to the sink, so the line starts deasserted"
    );

    restored.sync_levels_to_sink(&mut sink2);
    assert!(sink2.is_asserted(2));
    assert_eq!(sink2.take_events(), vec![GsiEvent::Raise(2)]);

    // Ensure the helper is idempotent (does not toggle already-correct lines).
    restored.sync_levels_to_sink(&mut sink2);
    assert!(sink2.is_asserted(2));
    assert!(
        sink2.take_events().is_empty(),
        "second sync should not generate additional GSI events"
    );

    // Ensure `sync_levels_to_sink()` updates the HPET's internal handshake (`irq_asserted`):
    // clearing the interrupt status should deassert the line.
    restored.mmio_write(HPET_REG_GENERAL_INT_STATUS, 8, 1, &mut sink2);
    assert!(!sink2.is_asserted(2));
    assert_eq!(sink2.take_events(), vec![GsiEvent::Lower(2)]);
}

