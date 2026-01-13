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

    // Also exercise a wider read spanning the boundary.
    bus.write_u8(0x0FFE, 0x34);
    bus.write_u8(0x0FFF, 0x56);
    assert_eq!(
        aero_mmu::MemoryBus::read_u32(&mut bus, 0x0FFE),
        0xFF12_5634
    );
}
