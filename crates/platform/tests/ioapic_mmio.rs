use std::sync::{Arc, Mutex};

use aero_interrupts::apic::{
    IoApic, IoApicId, LocalApic, IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE,
    LAPIC_MMIO_SIZE,
};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::interrupts::mmio::{IoApicMmio, LapicMmio};
use aero_platform::memory::MemoryBus;
use memory::MemoryBus as _;

#[test]
fn ioapic_mmio_programming_via_system_bus() {
    let filter = AddressFilter::new(ChipsetState::new(true).a20());
    let mut bus = MemoryBus::new(filter, 0x4000);

    let lapic = Arc::new(LocalApic::new(0));
    let ioapic = Arc::new(Mutex::new(IoApic::new(IoApicId(0), lapic.clone())));

    bus.map_mmio(
        LAPIC_MMIO_BASE,
        LAPIC_MMIO_SIZE,
        Box::new(LapicMmio::new(lapic.clone())),
    )
    .unwrap();
    bus.map_mmio(
        IOAPIC_MMIO_BASE,
        IOAPIC_MMIO_SIZE,
        Box::new(IoApicMmio::new(ioapic.clone())),
    )
    .unwrap();

    // Enable the LAPIC (SVR[8] = 1) with a valid spurious interrupt vector (0xFF).
    bus.write_u32(LAPIC_MMIO_BASE + 0xF0, 0x1FF);

    // Configure GSI 5 -> vector 0x45, unmasked.
    let gsi = 5u32;
    let vector = 0x45u32;
    let redtbl_low = 0x10u32 + (gsi * 2);

    bus.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    bus.write_u32(IOAPIC_MMIO_BASE + 0x10, vector);

    ioapic.lock().unwrap().set_irq_level(gsi, true);
    assert_eq!(lapic.get_pending_vector(), Some(vector as u8));
}
