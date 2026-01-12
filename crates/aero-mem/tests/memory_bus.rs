use aero_mem::{MemoryBus, MmioHandler, PhysicalMemory, PhysicalMemoryOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[derive(Default)]
struct CountingMmio {
    reads: AtomicUsize,
    writes: AtomicUsize,
}

impl CountingMmio {
    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::Relaxed)
    }
}

impl MmioHandler for CountingMmio {
    fn read(&self, _offset: u64, data: &mut [u8]) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        data.fill(0xCC);
    }

    fn write(&self, _offset: u64, _data: &[u8]) {
        self.writes.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn dma_bulk_read_write_within_ram() {
    let ram = make_ram(0x4000);
    let bus = MemoryBus::new(ram.clone());

    let src: Vec<u8> = (0..128).map(|i| i as u8).collect();
    bus.write_physical_from(0x1000, &src).unwrap();

    let mut dst = vec![0u8; src.len()];
    bus.read_physical_into(0x1000, &mut dst).unwrap();
    assert_eq!(dst, src);
}

#[test]
fn memcpy_from_guest_allocation_failure_returns_error_instead_of_panicking() {
    let ram = make_ram(0x1000);
    let bus = MemoryBus::new(ram);

    let err = bus.memcpy_from_guest(0, usize::MAX).unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::OutOfMemory { .. }));
}

#[test]
fn dma_bulk_crosses_chunk_boundary() {
    let ram = make_ram(0x9000);
    let bus = MemoryBus::new(ram.clone());

    // Chunk size is 4096; cross from 0x0FF0..0x1010.
    let start = 0x0ff0u64;
    let src: Vec<u8> = (0..64).map(|i| (0xa0 + i) as u8).collect();
    bus.write_physical_from(start, &src).unwrap();

    let mut dst = vec![0u8; src.len()];
    bus.read_physical_into(start, &mut dst).unwrap();
    assert_eq!(dst, src);
}

#[test]
fn dma_scatter_gather_roundtrip() {
    let ram = make_ram(0x8000);
    let bus = MemoryBus::new(ram.clone());

    let segments = &[
        (0x1000u64, 16usize),
        (0x2000u64, 8usize),
        (0x3000u64, 32usize),
    ];
    let total: usize = segments.iter().map(|(_, len)| *len).sum();
    let src: Vec<u8> = (0..total).map(|i| (i ^ 0x5a) as u8).collect();

    bus.write_sg(segments, &src).unwrap();
    let mut dst = vec![0u8; total];
    bus.read_sg(segments, &mut dst).unwrap();
    assert_eq!(dst, src);
}

#[test]
fn dma_scatter_gather_length_mismatch_errors() {
    let ram = make_ram(0x1000);
    let bus = MemoryBus::new(ram.clone());

    let segments = &[(0x0u64, 4usize), (0x10u64, 4usize)];
    let mut dst = [0u8; 7];
    let err = bus.read_sg(segments, &mut dst).unwrap_err();
    assert!(matches!(
        err,
        aero_mem::MemoryBusError::LengthMismatch { .. }
    ));
}

#[test]
fn dma_rejects_mmio_without_side_effects_or_partial_write() {
    let ram = make_ram(0x4000);
    let mut bus = MemoryBus::new(ram.clone());

    let mmio = Arc::new(CountingMmio::default());
    bus.register_mmio(0x2000..0x2100, mmio.clone()).unwrap();

    // Pre-fill RAM so we can detect partial writes.
    ram.write_bytes(0x1FF0, &[0xAA; 16]);

    let err = bus
        .try_write_sg(&[(0x1FF0, 16), (0x2000, 4)], &[0x55; 20])
        .unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::MmioAccess { .. }));

    // The MMIO handler must not have been called.
    assert_eq!(mmio.reads(), 0);
    assert_eq!(mmio.writes(), 0);

    // No partial write into RAM.
    let mut buf = [0u8; 16];
    ram.read_bytes(0x1FF0, &mut buf);
    assert_eq!(buf, [0xAA; 16]);
}

#[test]
fn dma_rejects_rom_without_partial_read() {
    let ram = make_ram(0x4000);
    let mut bus = MemoryBus::new(ram.clone());

    bus.register_rom(0x3000, Arc::from([1u8, 2, 3, 4])).unwrap();

    let mut dst = [0xAAu8; 8];
    let err = bus.try_read_ram_bytes(0x2FFC, &mut dst).unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::RomAccess { .. }));
    assert_eq!(dst, [0xAAu8; 8]);
}

#[test]
fn overlapping_mappings_are_rejected() {
    let ram = make_ram(0x1000);
    let mut bus = MemoryBus::new(ram);

    bus.register_rom(0x100, Arc::from([0u8; 16])).unwrap();

    let err = bus
        .register_mmio(0x108..0x110, Arc::new(CountingMmio::default()))
        .unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::Overlap { .. }));

    // Non-overlapping mapping should succeed.
    bus.register_mmio(0x200..0x210, Arc::new(CountingMmio::default()))
        .unwrap();
}

#[test]
fn invalid_ranges_are_rejected() {
    let ram = make_ram(0x1000);
    let mut bus = MemoryBus::new(ram);

    let err = bus
        .register_mmio(0x200..0x200, Arc::new(CountingMmio::default()))
        .unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::InvalidRange { .. }));

    let err = bus.register_rom(0x300, Arc::from([])).unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::InvalidRange { .. }));
}

#[test]
fn bulk_address_overflow_is_reported() {
    let ram = make_ram(0x1000);
    let bus = MemoryBus::new(ram);

    let mut dst = [0u8; 2];
    let err = bus.try_read_bytes(u64::MAX - 1, &mut dst).unwrap_err();
    assert!(matches!(
        err,
        aero_mem::MemoryBusError::AddressOverflow { .. }
    ));

    let err = bus.try_write_bytes(u64::MAX - 1, &[0u8; 2]).unwrap_err();
    assert!(matches!(
        err,
        aero_mem::MemoryBusError::AddressOverflow { .. }
    ));
}

#[test]
fn typed_read_crossing_ram_to_rom_boundary() {
    let ram = make_ram(0x210);
    let mut bus = MemoryBus::new(ram.clone());

    // Place ROM immediately after RAM.
    bus.register_rom(0x200, Arc::from([0xFEu8, 0xED])).unwrap();

    // Last byte in RAM + first byte in ROM.
    ram.write_u8(0x1FF, 0xAA);
    assert_eq!(bus.read_u16(0x1FF), 0xFEAA);
}

#[test]
fn register_mmio_fn_works() {
    let ram = make_ram(0x100);
    let mut bus = MemoryBus::new(ram);

    let writes = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
    let writes_clone = writes.clone();

    bus.register_mmio_fn(
        0x20..0x30,
        |offset, data| {
            for (i, b) in data.iter_mut().enumerate() {
                *b = 0xF0u8.wrapping_add(offset as u8).wrapping_add(i as u8);
            }
        },
        move |offset, data| {
            writes_clone.lock().unwrap().push((offset, data.to_vec()));
        },
    )
    .unwrap();

    assert_eq!(bus.read_u8(0x20), 0xF0);
    assert_eq!(bus.read_u8(0x2F), 0xF0 + 0x0F);

    bus.write_u16(0x22, 0xBEEF);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, 0x02);
    assert_eq!(writes[0].1, vec![0xEF, 0xBE]);
}

#[test]
fn open_bus_reads_as_ff_and_ignores_writes() {
    let ram = make_ram(0x100);
    let mut bus = MemoryBus::new(ram.clone());

    ram.write_u8(0x50, 0x12);
    bus.register_open_bus(0x50..0x60).unwrap();

    assert_eq!(bus.read_u8(0x4F), 0x00);
    assert_eq!(bus.read_u8(0x50), 0xFF);
    assert_eq!(bus.read_u8(0x5F), 0xFF);
    assert_eq!(bus.read_u8(0x60), 0x00);

    bus.write_u8(0x50, 0x34);
    assert_eq!(ram.read_u8(0x50), 0x12);
}

#[test]
fn dma_bulk_read_rejects_mmio_without_side_effects() {
    let ram = make_ram(0x4000);
    let mut bus = MemoryBus::new(ram.clone());

    let mmio = Arc::new(CountingMmio::default());
    bus.register_mmio(0x2000..0x2100, mmio.clone()).unwrap();

    let mut dst = [0xAAu8; 8];
    let err = bus.read_physical_into(0x1FFC, &mut dst).unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::MmioAccess { .. }));

    assert_eq!(mmio.reads(), 0);
    assert_eq!(mmio.writes(), 0);
    assert_eq!(dst, [0xAAu8; 8]);
}

#[test]
fn dma_bulk_write_rejects_rom_without_partial_write() {
    let ram = make_ram(0x4000);
    let mut bus = MemoryBus::new(ram.clone());

    bus.register_rom(0x3000, Arc::from([1u8, 2, 3, 4])).unwrap();

    ram.write_bytes(0x2FFC, &[0xAA; 4]);
    let err = bus.write_physical_from(0x2FFC, &[0x55; 8]).unwrap_err();
    assert!(matches!(err, aero_mem::MemoryBusError::RomAccess { .. }));

    let mut buf = [0u8; 4];
    ram.read_bytes(0x2FFC, &mut buf);
    assert_eq!(buf, [0xAA; 4]);
}
