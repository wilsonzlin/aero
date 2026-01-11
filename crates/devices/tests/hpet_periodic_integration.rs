use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, PlatformInterrupts,
};

const REG_GENERAL_CONFIG: u64 = 0x010;
const REG_GENERAL_INT_STATUS: u64 = 0x020;

const REG_TIMER0_BASE: u64 = 0x100;
const REG_TIMER_CONFIG: u64 = 0x00;
const REG_TIMER_COMPARATOR: u64 = 0x08;

const GEN_CONF_ENABLE: u64 = 1 << 0;

const TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const TIMER_CFG_INT_ENABLE: u64 = 1 << 2;
const TIMER_CFG_PERIODIC: u64 = 1 << 3;
const TIMER_CFG_INT_ROUTE_SHIFT: u64 = 9;
const TIMER_CFG_INT_ROUTE_MASK: u64 = 0x1F << TIMER_CFG_INT_ROUTE_SHIFT;

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn synthetic_guest_programs_hpet_and_receives_periodic_interrupts() {
    let clock = ManualClock::new();
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    // Route GSI5 to vector 0x40.
    program_ioapic_entry(&mut interrupts, 5, 0x40, 0);

    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut interrupts);

    // Timer0: periodic, edge-triggered, interrupts enabled, route=5.
    let mut timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut interrupts);
    timer0_cfg |= TIMER_CFG_INT_ENABLE | TIMER_CFG_PERIODIC;
    timer0_cfg &= !TIMER_CFG_INT_LEVEL;
    timer0_cfg = (timer0_cfg & !TIMER_CFG_INT_ROUTE_MASK) | (5u64 << TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_CONFIG,
        8,
        timer0_cfg,
        &mut interrupts,
    );

    // First write in periodic mode sets the period and schedules comparator relative to main_counter.
    hpet.mmio_write(
        REG_TIMER0_BASE + REG_TIMER_COMPARATOR,
        8,
        1,
        &mut interrupts,
    );

    for _ in 0..3 {
        clock.advance_ns(100);
        hpet.poll(&mut interrupts);

        let vector = interrupts.get_pending();
        assert_eq!(vector, Some(0x40));

        interrupts.acknowledge(0x40);
        hpet.mmio_write(REG_GENERAL_INT_STATUS, 8, 1, &mut interrupts);
        interrupts.eoi(0x40);
    }
}
