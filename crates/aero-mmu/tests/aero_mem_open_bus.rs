#![cfg(feature = "aero-mem-bus")]

use std::sync::Arc;

fn assert_open_bus_reads<B: aero_mmu::MemoryBus>(bus: &mut B, addr: u64) {
    assert_eq!(bus.read_u8(addr), 0xFF);
    assert_eq!(bus.read_u16(addr), 0xFFFF);
    assert_eq!(bus.read_u32(addr), 0xFFFF_FFFF);
    assert_eq!(bus.read_u64(addr), 0xFFFF_FFFF_FFFF_FFFF);

    let mut buf = [0u8; 32];
    bus.read_bytes(addr, &mut buf);
    assert!(buf.iter().all(|&b| b == 0xFF));

    // Unmapped writes must not panic and are ignored.
    bus.write_u8(addr, 0x12);
    bus.write_u16(addr, 0x1234);
    bus.write_u32(addr, 0x1234_5678);
    bus.write_u64(addr, 0x1122_3344_5566_7788);

    bus.write_bytes(addr, &[0x12, 0x34, 0x56, 0x78]);
}

fn assert_cross_boundary_bulk_semantics<B: aero_mmu::MemoryBus>(bus: &mut B, ram_end: u64) {
    // Initialize the last 4 bytes of RAM.
    bus.write_bytes(ram_end - 4, &[0xAA, 0xBB, 0xCC, 0xDD]);

    // Bulk read spans the end of RAM; the in-RAM portion should be returned and the remainder
    // must be open bus (0xFF).
    let mut buf = [0u8; 8];
    bus.read_bytes(ram_end - 4, &mut buf);
    assert_eq!(&buf[..], &[0xAA, 0xBB, 0xCC, 0xDD, 0xFF, 0xFF, 0xFF, 0xFF]);

    // Bulk write spanning the end of RAM must not panic and should update the RAM-backed
    // portion only.
    bus.write_bytes(ram_end - 2, &[0x11, 0x22, 0x33, 0x44]);
    assert_eq!(bus.read_u8(ram_end - 2), 0x11);
    assert_eq!(bus.read_u8(ram_end - 1), 0x22);
    assert_eq!(bus.read_u8(ram_end), 0xFF);
}

#[test]
fn aero_mem_bus_unmapped_reads_are_open_bus() {
    let ram = Arc::new(aero_mem::PhysicalMemory::new(4096).unwrap());
    let mut bus = aero_mem::MemoryBus::new(ram);

    // Past the end of RAM with no overlays mapped.
    assert_open_bus_reads(&mut bus, 0x2000);
}

#[test]
fn aero_mem_bus_typed_reads_past_end_of_ram_preserve_partial_bytes() {
    // 0x1001 bytes of RAM: last valid address is 0x1000.
    let ram = Arc::new(aero_mem::PhysicalMemory::new(0x1001).unwrap());
    let mut bus = aero_mem::MemoryBus::new(ram);

    // Write a byte at the last RAM address.
    bus.write_u8(0x1000, 0x12);

    // Typed reads that cross beyond RAM must keep the in-RAM bytes and fill the rest with 0xFF.
    assert_eq!(aero_mmu::MemoryBus::read_u16(&mut bus, 0x1000), 0xFF12);

    let mut buf = [0u8; 2];
    aero_mmu::MemoryBus::read_bytes(&mut bus, 0x1000, &mut buf);
    assert_eq!(buf, [0x12, 0xFF]);

    // Also exercise a wider read spanning the boundary.
    bus.write_u8(0x0FFE, 0x34);
    bus.write_u8(0x0FFF, 0x56);
    assert_eq!(aero_mmu::MemoryBus::read_u32(&mut bus, 0x0FFE), 0xFF12_5634);
}

#[test]
fn aero_mem_bus_bulk_reads_past_ram_end_are_open_bus() {
    let ram = Arc::new(aero_mem::PhysicalMemory::new(4096).unwrap());
    let mut bus = aero_mem::MemoryBus::new(ram);

    // Make sure bulk operations behave like scalar operations when they cross the end of guest
    // RAM.
    assert_cross_boundary_bulk_semantics(&mut bus, 0x1000);
}
