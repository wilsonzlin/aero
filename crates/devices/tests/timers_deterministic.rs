use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::IoApic;
use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD, PIT_HZ};

#[test]
fn timers_advance_deterministically_without_wall_clock() {
    // PIT: program channel 0 for mode 2 with a divisor of 1 so that we get one
    // IRQ0 pulse per PIT input tick.
    let mut pit = Pit8254::new();
    pit.port_write(PIT_CMD, 1, 0x34); // ch0, lobyte/hibyte, mode2
    pit.port_write(PIT_CH0, 1, 0x01); // reload low
    pit.port_write(PIT_CH0, 1, 0x00); // reload high

    pit.advance_ns(1_000_000_000);
    assert_eq!(pit.take_irq0_pulses(), PIT_HZ);

    // HPET: uses the manual clock to advance time deterministically.
    let clock = ManualClock::new();
    let mut ioapic = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    // Reset counter (while disabled) and then enable.
    hpet.mmio_write(0x0F0, 8, 0, &mut ioapic);
    hpet.mmio_write(0x010, 8, 1, &mut ioapic);

    clock.advance_ns(1_000_000_000);
    let counter = hpet.mmio_read(0x0F0, 8, &mut ioapic);
    assert_eq!(counter, 10_000_000);
}

