use std::cell::Cell;
use std::rc::Rc;

use aero_devices::a20_gate::A20Gate;
use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::IoApic;
use aero_devices::irq::IrqLine;
use aero_devices::pci::{
    GsiLevelSink, MsiCapability, PciBarDefinition, PciBdf, PciBus, PciBusSnapshot,
    PciConfigMechanism1, PciConfigSpace, PciDevice, PciInterruptPin, PciIntxRouter,
    PciIntxRouterConfig, PCI_CFG_ADDR_PORT,
};
use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD};
use aero_devices::rtc_cmos::RtcCmos;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::chipset::ChipsetState;
use aero_platform::interrupts::{PlatformInterruptMode, PlatformInterrupts};
use aero_platform::io::PortIoDevice;

const RTC_PORT_INDEX: u16 = 0x70;
const RTC_PORT_DATA: u16 = 0x71;
const RTC_REG_STATUS_B: u8 = 0x0B;
const RTC_REG_STATUS_C: u8 = 0x0C;

const RTC_REG_B_24H: u8 = 1 << 1;
const RTC_REG_B_PIE: u8 = 1 << 6;
const RTC_REG_C_PF: u8 = 1 << 6;

const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
const HPET_REG_MAIN_COUNTER: u64 = 0x0F0;
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

fn rtc_write_reg(dev: &mut impl PortIoDevice, reg: u8, value: u8) {
    dev.write(RTC_PORT_INDEX, 1, u32::from(reg));
    dev.write(RTC_PORT_DATA, 1, u32::from(value));
}

fn rtc_read_reg(dev: &mut impl PortIoDevice, reg: u8) -> u8 {
    dev.write(RTC_PORT_INDEX, 1, u32::from(reg));
    dev.read(RTC_PORT_DATA, 1) as u8
}

struct StubPciDev {
    cfg: PciConfigSpace,
}

impl StubPciDev {
    fn new(vendor: u16, device: u16) -> Self {
        Self {
            cfg: PciConfigSpace::new(vendor, device),
        }
    }
}

impl PciDevice for StubPciDev {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[test]
fn snapshot_bytes_are_deterministic() {
    let pit = Pit8254::new();
    assert_eq!(pit.save_state(), pit.save_state());

    let clock = ManualClock::new();
    clock.set_ns(123_456_789);
    let irq = TestIrq::new();
    let rtc = RtcCmos::new(clock, irq);
    assert_eq!(rtc.save_state(), rtc.save_state());

    let clock = ManualClock::new();
    let hpet = Hpet::new_default(clock);
    assert_eq!(hpet.save_state(), hpet.save_state());

    let chipset = ChipsetState::new(false);
    let a20 = A20Gate::new(chipset.a20());
    assert_eq!(a20.save_state(), a20.save_state());

    let mut bus = PciBus::new();
    bus.add_device(
        PciBdf::new(0, 1, 0),
        Box::new(StubPciDev::new(0x1234, 0x0001)),
    );
    let bus_snapshot = PciBusSnapshot::save_from(&bus);
    assert_eq!(bus_snapshot.save_state(), bus_snapshot.save_state());

    let mut cfg = PciConfigMechanism1::new();
    cfg.io_write(&mut bus, PCI_CFG_ADDR_PORT, 4, 0x8000_1234);
    assert_eq!(cfg.save_state(), cfg.save_state());

    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut sink = MockGsiSink::default();
    router.assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut sink);
    assert_eq!(router.save_state(), router.save_state());
}

#[test]
fn pit_snapshot_restore_preserves_write_phase_and_irq_pulses() {
    let mut pit = Pit8254::new();

    // ch0, lobyte/hibyte, mode2, binary
    pit.port_write(PIT_CMD, 1, 0x34);
    pit.port_write(PIT_CH0, 1, 10);

    // Without the high byte, the divisor is not committed and the PIT should not count.
    pit.advance_ticks(100);
    assert_eq!(pit.take_irq0_pulses(), 0);

    let snap = pit.save_state();

    let mut restored = Pit8254::new();
    restored.load_state(&snap).unwrap();

    // Complete the divisor (10) and ensure both models start counting identically.
    pit.port_write(PIT_CH0, 1, 0);
    restored.port_write(PIT_CH0, 1, 0);

    pit.advance_ticks(10);
    restored.advance_ticks(10);
    assert_eq!(pit.take_irq0_pulses(), 1);
    assert_eq!(restored.take_irq0_pulses(), 1);
}

#[test]
fn pit_snapshot_restore_preserves_latched_read_phase() {
    let mut pit = Pit8254::new();
    pit.port_write(PIT_CMD, 1, 0x34);
    pit.port_write(PIT_CH0, 1, 10);
    pit.port_write(PIT_CH0, 1, 0);
    pit.advance_ticks(3);

    // Latch count and consume only the low byte.
    pit.port_write(PIT_CMD, 1, 0x00);
    let _lo = pit.port_read(PIT_CH0, 1) as u8;

    let snap = pit.save_state();

    let mut restored = Pit8254::new();
    restored.load_state(&snap).unwrap();

    let hi_live = pit.port_read(PIT_CH0, 1) as u8;
    let hi_restored = restored.port_read(PIT_CH0, 1) as u8;
    assert_eq!(hi_live, hi_restored);

    // After restoring with a partially-consumed latch, subsequent live reads should match.
    let live_lo = pit.port_read(PIT_CH0, 1) as u8;
    let live_hi = pit.port_read(PIT_CH0, 1) as u8;
    let live_lo2 = restored.port_read(PIT_CH0, 1) as u8;
    let live_hi2 = restored.port_read(PIT_CH0, 1) as u8;
    assert_eq!(
        u16::from_le_bytes([live_lo, live_hi]),
        u16::from_le_bytes([live_lo2, live_hi2])
    );
}

#[test]
fn rtc_snapshot_restore_preserves_pending_irq_and_next_periodic_tick() {
    let clock = ManualClock::new();
    let irq = TestIrq::new();
    let mut rtc = RtcCmos::new(clock.clone(), irq.clone());

    // Enable periodic interrupts (default rate select is 1024Hz, interval 976562ns).
    rtc_write_reg(&mut rtc, RTC_REG_STATUS_B, RTC_REG_B_24H | RTC_REG_B_PIE);

    const INTERVAL_NS: u64 = 1_000_000_000 / 1024;
    const DELTA_NS: u64 = 123;

    clock.advance_ns(INTERVAL_NS + DELTA_NS);
    rtc.tick();
    assert!(irq.level(), "periodic IRQ should be asserted after tick");

    let snap = rtc.save_state();

    let clock2 = ManualClock::new();
    clock2.set_ns(5_000_000_000);
    let irq2 = TestIrq::new();
    let mut rtc2 = RtcCmos::new(clock2.clone(), irq2.clone());
    rtc2.load_state(&snap).unwrap();

    assert!(
        irq2.level(),
        "restored RTC should re-drive IRQ8 when flags are pending"
    );

    let status_c = rtc_read_reg(&mut rtc2, RTC_REG_STATUS_C);
    assert_ne!(status_c & RTC_REG_C_PF, 0);
    assert!(!irq2.level(), "reading Status C clears the IRQ latch");

    let remaining = INTERVAL_NS - DELTA_NS;
    clock2.advance_ns(remaining - 1);
    rtc2.tick();
    assert!(
        !irq2.level(),
        "periodic IRQ should not fire before the remaining interval"
    );

    clock2.advance_ns(1);
    rtc2.tick();
    assert!(
        irq2.level(),
        "periodic IRQ should fire when the remaining interval elapses"
    );
}

#[test]
fn hpet_snapshot_restore_keeps_edge_interrupt_latched_without_refiring() {
    let clock = ManualClock::new();
    let mut sink = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(HPET_REG_GENERAL_CONFIG, 8, HPET_GEN_CONF_ENABLE, &mut sink);
    let timer0_cfg = hpet.mmio_read(HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG, 8, &mut sink);
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg | HPET_TIMER_CFG_INT_ENABLE,
        &mut sink,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        1,
        &mut sink,
    );

    clock.advance_ns(100);
    hpet.poll(&mut sink);
    assert!(!sink.take_events().is_empty());

    let snap = hpet.save_state();

    let clock2 = ManualClock::new();
    clock2.set_ns(9_000_000);
    let mut sink2 = IoApic::default();
    let mut restored = Hpet::new_default(clock2.clone());
    restored.load_state(&snap).unwrap();

    restored.poll(&mut sink2);
    assert!(
        sink2.take_events().is_empty(),
        "edge-triggered HPET interrupts should not re-fire on restore"
    );
    assert_eq!(restored.mmio_read(HPET_REG_MAIN_COUNTER, 8, &mut sink2), 1);
    assert_ne!(
        restored.mmio_read(HPET_REG_GENERAL_INT_STATUS, 8, &mut sink2) & 1,
        0
    );
}

#[test]
fn hpet_snapshot_restore_reasserts_pending_level_interrupt() {
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

    clock.advance_ns(100);
    hpet.poll(&mut sink);
    assert!(sink.is_asserted(2));

    let snap = hpet.save_state();

    let clock2 = ManualClock::new();
    clock2.set_ns(42_000_000);
    let mut sink2 = IoApic::default();
    let mut restored = Hpet::new_default(clock2);
    restored.load_state(&snap).unwrap();

    assert!(
        !sink2.is_asserted(2),
        "restore does not have access to the sink, so the line starts deasserted"
    );
    restored.poll(&mut sink2);
    assert!(
        sink2.is_asserted(2),
        "pending level-triggered HPET interrupts must be reasserted on first poll"
    );
}

#[test]
fn pci_snapshot_restore_preserves_config_space_and_mappings() {
    let bdf = PciBdf::new(0, 1, 0);

    let mut bus = PciBus::new();
    let mut dev = StubPciDev::new(0x1234, 0x0001);
    dev.cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    dev.cfg.set_bar_base(0, 0xE000_0000);
    bus.add_device(bdf, Box::new(dev));

    // Enable memory decoding (should populate mapped_bars).
    bus.write_config(bdf, 0x04, 2, 0x0002);
    assert_eq!(bus.mapped_mmio_bars().len(), 1);

    // Put BAR0 into probe mode and ensure the read-back depends on the probe flag.
    bus.write_config(bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(bus.read_config(bdf, 0x10, 4), 0xFFFF_F000);

    let baseline_mapped = bus.mapped_bars();
    let snap = PciBusSnapshot::save_from(&bus).save_state();
    let mut decoded = PciBusSnapshot::default();
    decoded.load_state(&snap).unwrap();

    let mut bus2 = PciBus::new();
    let mut dev2 = StubPciDev::new(0x1234, 0x0001);
    dev2.cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    bus2.add_device(bdf, Box::new(dev2));
    decoded.restore_into(&mut bus2).unwrap();

    assert_eq!(bus2.mapped_bars(), baseline_mapped);
    assert_eq!(bus2.read_config(bdf, 0x10, 4), 0xFFFF_F000);
}

#[test]
fn pci_snapshot_restore_preserves_msi_state_and_pending_bits() {
    let bdf = PciBdf::new(0, 2, 0);

    let mut bus = PciBus::new();
    let mut dev = StubPciDev::new(0x1234, 0x0002);
    dev.cfg.add_capability(Box::new(MsiCapability::new()));
    let cap_offset = dev.cfg.find_capability(0x05).unwrap() as u16;
    bus.add_device(bdf, Box::new(dev));

    // Program MSI address/data and enable it.
    bus.write_config(bdf, cap_offset + 0x04, 4, 0xfee0_0000);
    bus.write_config(bdf, cap_offset + 0x08, 4, 0);
    bus.write_config(bdf, cap_offset + 0x0c, 2, 0x0045);
    let ctrl = bus.read_config(bdf, cap_offset + 0x02, 2) as u16;
    bus.write_config(bdf, cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

    // Mask vector 0 and trigger so it becomes pending.
    bus.write_config(bdf, cap_offset + 0x10, 4, 1);
    let mut platform = PlatformInterrupts::new();
    platform.set_mode(PlatformInterruptMode::Apic);
    let cfg = bus.device_config_mut(bdf).unwrap();
    let msi = cfg.capability_mut::<MsiCapability>().unwrap();
    assert!(!msi.trigger(&mut platform));

    let snap = PciBusSnapshot::save_from(&bus).save_state();
    let mut decoded = PciBusSnapshot::default();
    decoded.load_state(&snap).unwrap();

    let mut bus2 = PciBus::new();
    let mut dev2 = StubPciDev::new(0x1234, 0x0002);
    dev2.cfg.add_capability(Box::new(MsiCapability::new()));
    bus2.add_device(bdf, Box::new(dev2));
    decoded.restore_into(&mut bus2).unwrap();

    let cfg2 = bus2.device_config_mut(bdf).unwrap();
    let msi2 = cfg2.capability::<MsiCapability>().unwrap();
    assert!(msi2.enabled());
    assert_eq!(msi2.message_address(), 0xfee0_0000);
    assert_eq!(msi2.message_data(), 0x0045);

    let pending = cfg2.read(cap_offset + 0x14, 4);
    assert_eq!(
        pending & 1,
        1,
        "MSI pending bit should survive snapshot/restore"
    );
}

#[derive(Default)]
struct MockGsiSink {
    events: Vec<(u32, bool)>,
}

impl GsiLevelSink for MockGsiSink {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.events.push((gsi, level));
    }
}

#[test]
fn pci_intx_router_snapshot_restore_preserves_reference_counts() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut sink = MockGsiSink::default();

    let dev0 = PciBdf::new(0, 0, 0);
    let dev4 = PciBdf::new(0, 4, 0);
    let gsi = router.gsi_for_intx(dev0, PciInterruptPin::IntA);
    router.assert_intx(dev0, PciInterruptPin::IntA, &mut sink);
    router.assert_intx(dev4, PciInterruptPin::IntA, &mut sink);
    assert_eq!(sink.events, vec![(gsi, true)]);

    let snap = router.save_state();

    let mut restored = PciIntxRouter::new(PciIntxRouterConfig::default());
    restored.load_state(&snap).unwrap();

    let mut sink2 = MockGsiSink::default();
    restored.deassert_intx(dev0, PciInterruptPin::IntA, &mut sink2);
    assert!(
        sink2.events.is_empty(),
        "first deassert should not lower shared line"
    );
    restored.deassert_intx(dev4, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(sink2.events, vec![(gsi, false)]);
}
