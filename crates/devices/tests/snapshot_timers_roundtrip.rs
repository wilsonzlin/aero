use std::cell::Cell;
use std::rc::Rc;

use aero_devices::a20_gate::A20Gate;
use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::{GsiEvent, IoApic};
use aero_devices::irq::IrqLine;
use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD};
use aero_devices::rtc_cmos::RtcCmos;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::chipset::ChipsetState;
use aero_platform::io::PortIoDevice;

const RTC_PORT_INDEX: u16 = 0x70;
const RTC_PORT_DATA: u16 = 0x71;

const RTC_REG_SECONDS: u8 = 0x00;
const RTC_REG_STATUS_A: u8 = 0x0A;
const RTC_REG_STATUS_B: u8 = 0x0B;
const RTC_REG_STATUS_C: u8 = 0x0C;

const RTC_REG_B_UIE: u8 = 1 << 4;
const RTC_REG_B_PIE: u8 = 1 << 6;
const RTC_REG_B_24H: u8 = 1 << 1;

const RTC_REG_C_IRQF: u8 = 1 << 7;
const RTC_REG_C_PF: u8 = 1 << 6;
const RTC_REG_C_UF: u8 = 1 << 4;

const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;

const HPET_REG_TIMER0_BASE: u64 = 0x100;
const HPET_REG_TIMER_CONFIG: u64 = 0x00;
const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

#[derive(Clone)]
struct TestIrq(Rc<Cell<bool>>);

impl TestIrq {
    fn new() -> Self {
        Self(Rc::new(Cell::new(false)))
    }

    fn level(&self) -> bool {
        self.0.get()
    }
}

impl IrqLine for TestIrq {
    fn set_level(&self, level: bool) {
        self.0.set(level);
    }
}

fn rtc_read_reg(rtc: &mut impl PortIoDevice, idx: u8) -> u8 {
    rtc.write(RTC_PORT_INDEX, 1, idx as u32);
    rtc.read(RTC_PORT_DATA, 1) as u8
}

fn rtc_write_reg(rtc: &mut impl PortIoDevice, idx: u8, value: u8) {
    rtc.write(RTC_PORT_INDEX, 1, idx as u32);
    rtc.write(RTC_PORT_DATA, 1, value as u32);
}

fn program_pit_divisor(pit: &mut Pit8254, divisor: u16) {
    // ch0, lobyte/hibyte, mode2, binary
    pit.port_write(PIT_CMD, 1, 0x34);
    pit.port_write(PIT_CH0, 1, (divisor & 0xFF) as u32);
    pit.port_write(PIT_CH0, 1, (divisor >> 8) as u32);
}

#[test]
fn pit_snapshot_roundtrip_preserves_pulse_phase_and_is_deterministic() {
    let mut pit = Pit8254::new();
    program_pit_divisor(&mut pit, 4);

    pit.advance_ns(1_000_000);
    pit.take_irq0_pulses();

    let snap1 = pit.save_state();
    let snap2 = pit.save_state();
    assert_eq!(snap1, snap2);

    let mut restored = Pit8254::new();
    restored.load_state(&snap1).unwrap();

    pit.advance_ns(1_000_000);
    restored.advance_ns(1_000_000);
    assert_eq!(pit.take_irq0_pulses(), restored.take_irq0_pulses());
}

#[test]
fn rtc_snapshot_roundtrip_re_drives_irq_and_preserves_periodic_phase() {
    let clock = ManualClock::new();

    let irq0 = TestIrq::new();
    let mut rtc = RtcCmos::new(clock.clone(), irq0.clone());

    // Explicitly select a deterministic periodic rate (default value is already 0x26).
    rtc_write_reg(&mut rtc, RTC_REG_STATUS_A, 0x26);
    rtc_write_reg(
        &mut rtc,
        RTC_REG_STATUS_B,
        RTC_REG_B_24H | RTC_REG_B_PIE | RTC_REG_B_UIE,
    );

    // Advance past the first periodic tick but not a second boundary.
    clock.advance_ns(1_000_000);
    rtc.tick();
    assert!(irq0.level(), "periodic interrupt should assert IRQ8");

    let snap = rtc.save_state();
    assert_eq!(snap, rtc.save_state());

    let irq1 = TestIrq::new();
    let mut restored = RtcCmos::new(clock.clone(), irq1.clone());
    restored.load_state(&snap).unwrap();

    assert!(irq1.level(), "restored RTC should re-drive IRQ8 level");

    let c0 = rtc_read_reg(&mut rtc, RTC_REG_STATUS_C);
    let c1 = rtc_read_reg(&mut restored, RTC_REG_STATUS_C);
    assert_eq!(c0, c1);
    assert_ne!(c1 & RTC_REG_C_IRQF, 0);
    assert_ne!(c1 & RTC_REG_C_PF, 0);
    assert_eq!(c1 & RTC_REG_C_UF, 0);
    assert!(!irq1.level(), "status C read should clear IRQ8");

    // Jump to the 1-second boundary: should raise update-ended + periodic flags deterministically.
    clock.advance_ns(999_000_000);
    rtc.tick();
    restored.tick();
    assert!(irq0.level());
    assert!(irq1.level());

    let c0 = rtc_read_reg(&mut rtc, RTC_REG_STATUS_C);
    let c1 = rtc_read_reg(&mut restored, RTC_REG_STATUS_C);
    assert_eq!(c0, c1);
    assert_eq!(c1 & (RTC_REG_C_IRQF | RTC_REG_C_PF | RTC_REG_C_UF), RTC_REG_C_IRQF | RTC_REG_C_PF | RTC_REG_C_UF);

    // Time reads should match across snapshot/restore.
    let s0 = rtc_read_reg(&mut rtc, RTC_REG_SECONDS);
    let s1 = rtc_read_reg(&mut restored, RTC_REG_SECONDS);
    assert_eq!(s0, s1);
    assert_eq!(s0, 0x01);
}

#[test]
fn hpet_snapshot_roundtrip_edge_timer_produces_same_events() {
    let clock = ManualClock::new();
    let mut ioapic0 = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(HPET_REG_GENERAL_CONFIG, 8, HPET_GEN_CONF_ENABLE, &mut ioapic0);
    let timer0_cfg = hpet.mmio_read(HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG, 8, &mut ioapic0);
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg | HPET_TIMER_CFG_INT_ENABLE,
        &mut ioapic0,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        5,
        &mut ioapic0,
    );
    ioapic0.take_events();

    clock.advance_ns(300);
    hpet.poll(&mut ioapic0);
    assert!(ioapic0.take_events().is_empty());

    let snap = hpet.save_state();
    assert_eq!(snap, hpet.save_state());

    let mut ioapic1 = IoApic::default();
    let mut restored = Hpet::new_default(clock.clone());
    restored.load_state(&snap).unwrap();

    clock.advance_ns(200);
    hpet.poll(&mut ioapic0);
    restored.poll(&mut ioapic1);

    assert_eq!(ioapic0.take_events(), ioapic1.take_events());
}

#[test]
fn hpet_snapshot_roundtrip_level_pending_interrupt_reasserts_after_restore() {
    let clock = ManualClock::new();
    let mut ioapic0 = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(HPET_REG_GENERAL_CONFIG, 8, HPET_GEN_CONF_ENABLE, &mut ioapic0);
    let timer0_cfg = hpet.mmio_read(HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG, 8, &mut ioapic0);
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg | HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL,
        &mut ioapic0,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        1,
        &mut ioapic0,
    );

    clock.advance_ns(100);
    hpet.poll(&mut ioapic0);
    assert!(ioapic0.is_asserted(2));
    ioapic0.take_events();

    let snap = hpet.save_state();

    // Restore into a fresh sink (line low) and ensure the asserted interrupt is re-driven.
    let mut ioapic1 = IoApic::default();
    let mut restored = Hpet::new_default(clock.clone());
    restored.load_state(&snap).unwrap();

    restored.poll(&mut ioapic1);
    assert!(
        ioapic1.is_asserted(2),
        "level-triggered interrupt should be reasserted based on general_int_status"
    );
    assert_eq!(ioapic1.take_events(), vec![GsiEvent::Raise(2)]);

    // Clearing interrupt status should lower the line deterministically.
    restored.mmio_write(HPET_REG_GENERAL_INT_STATUS, 8, 1, &mut ioapic1);
    assert!(!ioapic1.is_asserted(2));
    assert_eq!(ioapic1.take_events(), vec![GsiEvent::Lower(2)]);
}

#[test]
fn a20_gate_snapshot_roundtrip_preserves_latch_and_enabled_state() {
    let chipset0 = ChipsetState::new(false);
    let a20 = chipset0.a20();
    let mut gate = A20Gate::new(a20.clone());

    // Set A20 and also pulse reset (bit0 should self-clear in the latched value).
    gate.write(0, 1, 0x03);
    assert!(a20.enabled());
    assert_eq!(gate.read(0, 1), 0x02);

    let snap = gate.save_state();
    assert_eq!(snap, gate.save_state());

    let chipset1 = ChipsetState::new(false);
    let a20_1 = chipset1.a20();
    let mut restored = A20Gate::new(a20_1.clone());
    restored.load_state(&snap).unwrap();

    assert!(a20_1.enabled());
    assert_eq!(restored.read(0, 1), 0x02);
}
