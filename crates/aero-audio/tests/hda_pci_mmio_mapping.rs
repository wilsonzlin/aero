use aero_audio::hda_pci::HdaPciDevice;
use memory::MmioHandler;
use memory::{DenseMemory, PhysicalMemoryBus};

const HDA_BASE: u64 = 0x1000_0000;

const REG_GCTL: u64 = 0x08;
const REG_STATESTS: u64 = 0x0e;

#[test]
fn hda_pci_mmio_routes_through_physical_memory_bus() {
    let ram = DenseMemory::new(0x20000).expect("failed to allocate guest RAM");
    let mut bus = PhysicalMemoryBus::new(Box::new(ram));

    bus.map_mmio(
        HDA_BASE,
        u64::from(HdaPciDevice::MMIO_BAR_SIZE),
        Box::new(HdaPciDevice::new()),
    )
    .expect("failed to map HDA MMIO");

    // Bring the controller out of reset via the bus and verify the state sticks.
    bus.write_physical_u32(HDA_BASE + REG_GCTL, 0x1);
    assert_eq!(bus.read_physical_u32(HDA_BASE + REG_GCTL) & 0x1, 0x1);

    // Codec 0 should report present after reset.
    assert_eq!(bus.read_physical_u16(HDA_BASE + REG_STATESTS) & 0x1, 0x1);

    // 64-bit MMIO reads are split into 32-bit accesses by the wrapper (PhysicalMemoryBus reads
    // up to 8 bytes per handler call).
    assert_eq!(
        bus.read_physical_u64(HDA_BASE + REG_GCTL),
        0x0001_0000_0000_0001
    );
}

#[test]
fn hda_pci_mmio_does_not_panic_on_offset_add_overflow() {
    let mut dev = HdaPciDevice::new();

    // The MMIO handler should be defensive even if a caller passes a pathological offset that would
    // overflow when splitting an 8-byte access into 32-bit pieces.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = dev.read(u64::MAX - 2, 8);
        dev.write(u64::MAX - 2, 8, 0xdead_beef_dead_beef);
        let _ = dev.read(u64::MAX - 2, 16);
        dev.write(u64::MAX - 2, 16, 0x0123_4567_89ab_cdef);
    }));

    assert!(
        result.is_ok(),
        "HDA PCI MMIO handler panicked on offset arithmetic overflow"
    );
}
