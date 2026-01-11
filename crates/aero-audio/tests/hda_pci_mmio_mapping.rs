use aero_audio::hda_pci::HdaPciDevice;
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
