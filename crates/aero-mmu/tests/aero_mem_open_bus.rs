#![cfg(feature = "aero-mem-bus")]

use std::sync::Arc;

fn assert_open_bus_reads<B: aero_mmu::MemoryBus>(bus: &mut B, addr: u64) {
    assert_eq!(bus.read_u8(addr), 0xFF);
    assert_eq!(bus.read_u16(addr), 0xFFFF);
    assert_eq!(bus.read_u32(addr), 0xFFFF_FFFF);
    assert_eq!(bus.read_u64(addr), 0xFFFF_FFFF_FFFF_FFFF);

    // Unmapped writes must not panic and are ignored.
    bus.write_u8(addr, 0x12);
    bus.write_u16(addr, 0x1234);
    bus.write_u32(addr, 0x1234_5678);
    bus.write_u64(addr, 0x1122_3344_5566_7788);
}

#[test]
fn aero_mem_bus_unmapped_reads_are_open_bus() {
    let ram = Arc::new(aero_mem::PhysicalMemory::new(4096).unwrap());
    let mut bus = aero_mem::MemoryBus::new(ram);

    // Past the end of RAM with no overlays mapped.
    assert_open_bus_reads(&mut bus, 0x2000);
}
