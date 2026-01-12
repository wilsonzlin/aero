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
fn pending_level_irq_is_asserted_immediately_after_restore_sync() {
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

    // Fire the timer so the level interrupt is pending and asserted.
    clock.advance_ns(100);
    hpet.poll(&mut sink);
    assert!(sink.is_asserted(2));

    let snap = hpet.save_state();

    // Restore into a fresh sink and re-drive pending levels without calling poll().
    let clock2 = ManualClock::new();
    clock2.set_ns(5_000_000);
    let mut sink2 = IoApic::default();
    let mut restored = Hpet::new_default(clock2);
    restored.load_state(&snap).unwrap();

    assert!(!sink2.is_asserted(2));
    restored.sync_levels_to_sink(&mut sink2);
    assert!(sink2.is_asserted(2));
    assert_eq!(sink2.take_events(), vec![GsiEvent::Raise(2)]);

    // Ensure `irq_asserted` bookkeeping is coherent: clearing status should lower the line.
    restored.mmio_write(HPET_REG_GENERAL_INT_STATUS, 8, 1, &mut sink2);
    assert!(!sink2.is_asserted(2));
    assert_eq!(sink2.take_events(), vec![GsiEvent::Lower(2)]);
}

#[test]
fn restore_sync_does_not_assert_when_no_interrupt_is_pending() {
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
    // Arm the timer far enough in the future that the status bit stays clear.
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        10,
        &mut sink,
    );

    let snap = hpet.save_state();

    let mut sink2 = IoApic::default();
    let mut restored = Hpet::new_default(ManualClock::new());
    restored.load_state(&snap).unwrap();

    restored.sync_levels_to_sink(&mut sink2);
    assert!(!sink2.is_asserted(2));
    assert!(sink2.take_events().is_empty());
}
