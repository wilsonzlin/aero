use std::sync::{Arc, Mutex};

use aero_devices::apic::{
    IoApic, IoApicId, LocalApic, IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE,
    LAPIC_MMIO_SIZE,
};
use emulator::devices::ioapic::IoApicMmio;
use emulator::devices::lapic::LapicMmio;
use emulator::memory_bus::MemoryBus;
use memory::DenseMemory;

fn write_u32(bus: &mut MemoryBus, paddr: u64, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        bus.write_physical_u8(paddr + i as u64, byte).unwrap();
    }
}

#[test]
fn ioapic_mmio_programming_via_system_bus() {
    let ram = DenseMemory::new(0x4000).unwrap();
    let mut bus = MemoryBus::new(Box::new(ram));

    let lapic = Arc::new(LocalApic::new(0));
    let ioapic = Arc::new(Mutex::new(IoApic::new(IoApicId(0), lapic.clone())));

    bus.add_mmio_region(
        LAPIC_MMIO_BASE,
        LAPIC_MMIO_SIZE,
        Box::new(LapicMmio::new(lapic.clone())),
    );
    bus.add_mmio_region(
        IOAPIC_MMIO_BASE,
        IOAPIC_MMIO_SIZE,
        Box::new(IoApicMmio::new(ioapic.clone())),
    );

    // Enable the LAPIC (SVR[8] = 1).
    write_u32(&mut bus, LAPIC_MMIO_BASE + 0xF0, 1 << 8);

    // Configure GSI 5 -> vector 0x45, unmasked.
    let gsi = 5u32;
    let vector = 0x45u32;
    let redtbl_low = 0x10u32 + (gsi * 2);

    write_u32(&mut bus, IOAPIC_MMIO_BASE + 0x00, redtbl_low);
    write_u32(&mut bus, IOAPIC_MMIO_BASE + 0x10, vector);

    ioapic.lock().unwrap().set_irq_level(gsi, true);
    assert_eq!(lapic.get_pending_vector(), Some(vector as u8));
}
