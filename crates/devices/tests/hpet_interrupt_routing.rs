use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::{GsiEvent, IoApic};

const REG_GENERAL_CONFIG: u64 = 0x010;
const REG_GENERAL_INT_STATUS: u64 = 0x020;

const REG_TIMER0_BASE: u64 = 0x100;
const TIMER_STRIDE: u64 = 0x20;
const REG_TIMER_CONFIG: u64 = 0x00;
const REG_TIMER_COMPARATOR: u64 = 0x08;

const GEN_CONF_ENABLE: u64 = 1 << 0;
const GEN_CONF_LEGACY_ROUTE: u64 = 1 << 1;

const TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const TIMER_CFG_INT_ENABLE: u64 = 1 << 2;
const TIMER_CFG_PERIODIC: u64 = 1 << 3;
const TIMER_CFG_INT_ROUTE_SHIFT: u64 = 9;
const TIMER_CFG_INT_ROUTE_MASK: u64 = 0x1F << TIMER_CFG_INT_ROUTE_SHIFT;

#[test]
fn timer_route_selection_delivers_to_programmed_gsi() {
    let clock = ManualClock::new();
    let mut ioapic = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut ioapic);

    // Timer0: level-triggered, interrupts enabled, route=5.
    let mut timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut ioapic);
    timer0_cfg |= TIMER_CFG_INT_ENABLE | TIMER_CFG_INT_LEVEL;
    timer0_cfg = (timer0_cfg & !TIMER_CFG_INT_ROUTE_MASK) | (5u64 << TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_CONFIG,
        8,
        timer0_cfg,
        &mut ioapic,
    );

    // Fire at main_counter == 1 (100ns at 100ns/tick).
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_COMPARATOR,
        8,
        1,
        &mut ioapic,
    );

    clock.advance_ns(100);
    hpet.poll(&mut ioapic);

    assert!(ioapic.is_asserted(5));
    assert_eq!(ioapic.take_events(), vec![GsiEvent::Raise(5)]);
}

#[test]
fn legacy_replacement_forces_timer0_timer1_routes() {
    let clock = ManualClock::new();
    let mut ioapic = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(
        REG_GENERAL_CONFIG,
        8,
        GEN_CONF_ENABLE | GEN_CONF_LEGACY_ROUTE,
        &mut ioapic,
    );

    // Program routes that should be ignored due to legacy replacement.
    let mut timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut ioapic);
    timer0_cfg |= TIMER_CFG_INT_ENABLE | TIMER_CFG_INT_LEVEL;
    timer0_cfg = (timer0_cfg & !TIMER_CFG_INT_ROUTE_MASK) | (5u64 << TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_CONFIG,
        8,
        timer0_cfg,
        &mut ioapic,
    );

    let timer1_base = REG_TIMER0_BASE + TIMER_STRIDE;
    let mut timer1_cfg = hpet.mmio_read(timer1_base + REG_TIMER_CONFIG, 8, &mut ioapic);
    timer1_cfg |= TIMER_CFG_INT_ENABLE | TIMER_CFG_INT_LEVEL;
    timer1_cfg = (timer1_cfg & !TIMER_CFG_INT_ROUTE_MASK) | (7u64 << TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(timer1_base + REG_TIMER_CONFIG, 8, timer1_cfg, &mut ioapic);

    // Fire timer0 at t=1 and timer1 at t=2.
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_COMPARATOR,
        8,
        1,
        &mut ioapic,
    );
    hpet.mmio_write(timer1_base + REG_TIMER_COMPARATOR, 8, 2, &mut ioapic);

    clock.advance_ns(200);
    hpet.poll(&mut ioapic);

    // Legacy replacement routes timer0 to the PIT line (GSI2) and timer1 to RTC (GSI8).
    assert!(ioapic.is_asserted(2));
    assert!(ioapic.is_asserted(8));
    assert!(!ioapic.is_asserted(5));
    assert!(!ioapic.is_asserted(7));
}

#[test]
fn level_triggered_timer_does_not_storm_without_clear() {
    let clock = ManualClock::new();
    let mut ioapic = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut ioapic);

    // Timer0: periodic, level-triggered, route=5.
    let mut timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut ioapic);
    timer0_cfg |= TIMER_CFG_INT_ENABLE | TIMER_CFG_INT_LEVEL | TIMER_CFG_PERIODIC;
    timer0_cfg = (timer0_cfg & !TIMER_CFG_INT_ROUTE_MASK) | (5u64 << TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_CONFIG,
        8,
        timer0_cfg,
        &mut ioapic,
    );

    // First comparator write in periodic mode programs the period and schedules
    // comparator relative to current main_counter.
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_COMPARATOR,
        8,
        1,
        &mut ioapic,
    );

    clock.advance_ns(100);
    hpet.poll(&mut ioapic);
    assert_eq!(ioapic.take_events(), vec![GsiEvent::Raise(5)]);

    // Advance multiple periods without clearing the HPET interrupt status.
    clock.advance_ns(500);
    hpet.poll(&mut ioapic);
    assert!(ioapic.take_events().is_empty());
    assert!(ioapic.is_asserted(5));

    // Clearing the interrupt status should deassert the line, and the next period
    // should be deliverable again.
    hpet.mmio_write(REG_GENERAL_INT_STATUS, 8, 1, &mut ioapic);
    assert_eq!(ioapic.take_events(), vec![GsiEvent::Lower(5)]);
    assert!(!ioapic.is_asserted(5));

    clock.advance_ns(100);
    hpet.poll(&mut ioapic);
    assert_eq!(ioapic.take_events(), vec![GsiEvent::Raise(5)]);
}

