use aero_mem::{MemoryBus, MmioHandler, PhysicalMemory, PhysicalMemoryOptions};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct TestMmio {
    writes: Mutex<Vec<(u64, Vec<u8>)>>,
}

impl MmioHandler for TestMmio {
    fn read(&self, offset: u64, data: &mut [u8]) {
        for (i, b) in data.iter_mut().enumerate() {
            *b = 0xA0u8.wrapping_add(offset as u8).wrapping_add(i as u8);
        }
    }

    fn write(&self, offset: u64, data: &[u8]) {
        self.writes.lock().unwrap().push((offset, data.to_vec()));
    }
}

fn make_ram(size: u64) -> Arc<PhysicalMemory> {
    Arc::new(
        PhysicalMemory::with_options(size, PhysicalMemoryOptions { chunk_size: 4096 }).unwrap(),
    )
}

#[test]
fn mmio_mapping_boundaries() {
    let ram = make_ram(0x200);
    ram.write_u8(0x7F, 0x11);
    ram.write_u8(0x90, 0x22);

    let mut bus = MemoryBus::new(ram.clone());
    let mmio = Arc::new(TestMmio::default());
    bus.register_mmio(0x80..0x90, mmio.clone()).unwrap();

    // Just before MMIO region: RAM
    assert_eq!(bus.read_u8(0x7F), 0x11);

    // Start of MMIO region
    assert_eq!(bus.read_u8(0x80), 0xA0);
    // Last byte of MMIO region
    assert_eq!(bus.read_u8(0x8F), 0xA0 + 0x0F);

    // Immediately after MMIO region: RAM
    assert_eq!(bus.read_u8(0x90), 0x22);

    // Ensure writes are routed to the MMIO handler with LE bytes.
    bus.write_u32(0x84, 0x1122_3344);
    let writes = mmio.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, 0x04);
    assert_eq!(writes[0].1, vec![0x44, 0x33, 0x22, 0x11]);

    // MMIO writes must not touch underlying RAM.
    assert_eq!(ram.read_u32(0x84), 0);
}

#[test]
fn rom_writes_are_ignored() {
    let ram = make_ram(0x200);
    let mut bus = MemoryBus::new(ram.clone());

    bus.register_rom(0x40, Arc::from([0xDE, 0xAD, 0xBE, 0xEF]))
        .unwrap();

    bus.write_u32(0x40, 0x1122_3344);

    let mut buf = [0u8; 4];
    bus.read_bytes(0x40, &mut buf);
    assert_eq!(buf, [0xDE, 0xAD, 0xBE, 0xEF]);

    // Verify the write didn't fall through into RAM.
    ram.read_bytes(0x40, &mut buf);
    assert_eq!(buf, [0, 0, 0, 0]);
}

#[test]
fn little_endian_typed_accesses() {
    let ram = make_ram(0x200);
    let bus = MemoryBus::new(ram.clone());

    bus.write_u32(0x10, 0x1122_3344);
    assert_eq!(bus.read_u32(0x10), 0x1122_3344);

    let mut buf = [0u8; 4];
    bus.read_bytes(0x10, &mut buf);
    assert_eq!(buf, [0x44, 0x33, 0x22, 0x11]);

    bus.write_u128(0x20, 0x0011_2233_4455_6677_8899_AABB_CCDD_EEFFu128);
    assert_eq!(
        bus.read_u128(0x20),
        0x0011_2233_4455_6677_8899_AABB_CCDD_EEFFu128
    );
}
