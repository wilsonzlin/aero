use crate::bus::{MemoryBus, MmioHandler};
use crate::Bus;
use std::sync::{Arc, Mutex};

struct RecordingMmio {
    reads: Arc<Mutex<Vec<u64>>>,
    writes: Arc<Mutex<Vec<(u64, u8)>>>,
    value: u8,
}

impl MmioHandler for RecordingMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        assert_eq!(size, 1);
        self.reads.lock().unwrap().push(offset);
        self.value as u64
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        assert_eq!(size, 1);
        self.writes.lock().unwrap().push((offset, value as u8));
    }
}

#[test]
fn rom_is_read_only_and_does_not_write_through() {
    let mut bus = Bus::new(0x2000);

    bus.write_u8(0x1000, 0x11);
    bus.map_rom(0x1000, vec![0xAA]);

    assert_eq!(bus.read_u8(0x1000), 0xAA);
    bus.write_u8(0x1000, 0x55);
    assert_eq!(bus.read_u8(0x1000), 0xAA);

    assert_eq!(bus.ram_mut()[0x1000], 0x11);
}

#[test]
fn mmio_precedes_rom_and_ram() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let handler = RecordingMmio {
        reads: reads.clone(),
        writes: writes.clone(),
        value: 0xFE,
    };

    let mut bus = Bus::new(0x2000);
    bus.write_u8(0x1000, 0x11);
    bus.map_rom(0x1000, vec![0xAA]);
    bus.map_mmio(0x1000, 1, Box::new(handler));

    assert_eq!(bus.read_u8(0x1000), 0xFE);
    bus.write_u8(0x1000, 0x77);

    assert_eq!(reads.lock().unwrap().as_slice(), &[0]);
    assert_eq!(writes.lock().unwrap().as_slice(), &[(0, 0x77)]);
}

#[test]
fn unmapped_reads_return_all_ones() {
    let mut bus = Bus::new(0x10);

    assert_eq!(bus.read_u8(0x1000), 0xFF);
    assert_eq!(bus.read_u16(0x1000), 0xFFFF);
    assert_eq!(bus.read_u32(0x1000), 0xFFFF_FFFF);
    assert_eq!(bus.read_u64(0x1000), 0xFFFF_FFFF_FFFF_FFFF);
}

#[test]
fn boundary_crossing_reads_and_writes_are_le_correct() {
    let mut bus = Bus::new(2);
    bus.write_u8(0, 0x11);
    bus.write_u8(1, 0x22);

    assert_eq!(bus.read_u16(0), 0x2211);
    assert_eq!(bus.read_u16(1), 0xFF22);

    bus.write_u16(1, 0xBBAA);
    assert_eq!(bus.read_u8(1), 0xAA);
    assert_eq!(bus.read_u8(2), 0xFF);
}

#[test]
fn physical_reads_and_writes_do_not_wrap_past_u64_max() {
    let mut bus = Bus::new(16);
    // Prime low memory with a sentinel to detect unintended wraparound writes.
    bus.write_u8(0, 0x11);
    bus.write_u8(1, 0x22);

    // Reads beyond the u64 address space should behave like unmapped reads (all ones), not wrap to
    // low RAM.
    let mut buf = [0u8; 4];
    bus.read_physical(u64::MAX - 1, &mut buf);
    assert_eq!(buf, [0xFF; 4]);

    // Writes beyond the u64 address space should be ignored (and must not wrap to low RAM).
    bus.write_physical(u64::MAX - 1, &[0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(bus.read_u8(0), 0x11);
    assert_eq!(bus.read_u8(1), 0x22);
}
