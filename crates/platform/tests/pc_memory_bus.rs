use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_platform::address_filter::AddressFilter;
use aero_platform::dirty_memory::DEFAULT_DIRTY_PAGE_SIZE;
use aero_platform::memory::{MemoryBus, BIOS_RESET_VECTOR_PHYS, BIOS_ROM_BASE, BIOS_ROM_SIZE};
use aero_platform::ChipsetState;
use memory::MapError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct MmioState {
    mem: Vec<u8>,
    reads: Vec<(u64, usize)>,
    writes: Vec<(u64, usize, u64)>,
}

#[derive(Clone)]
struct RecordingMmio {
    state: Arc<Mutex<MmioState>>,
}

impl RecordingMmio {
    fn new(mem: Vec<u8>) -> (Self, Arc<Mutex<MmioState>>) {
        let state = Arc::new(Mutex::new(MmioState {
            mem,
            ..Default::default()
        }));
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl memory::MmioHandler for RecordingMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.reads.push((offset, size));

        let mut buf = [0xFFu8; 8];
        let off = offset as usize;
        for (i, dst) in buf.iter_mut().enumerate().take(size.min(8)) {
            *dst = state.mem.get(off + i).copied().unwrap_or(0xFF);
        }
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut state = self.state.lock().unwrap();
        state.writes.push((offset, size, value));

        let bytes = value.to_le_bytes();
        let off = offset as usize;
        for (i, &byte) in bytes.iter().enumerate().take(size.min(8)) {
            if let Some(dst) = state.mem.get_mut(off + i) {
                *dst = byte;
            }
        }
    }
}

fn new_bus(a20_enabled: bool, ram_size: usize) -> MemoryBus {
    let chipset = ChipsetState::new(a20_enabled);
    let filter = AddressFilter::new(chipset.a20());
    MemoryBus::new(filter, ram_size)
}

#[test]
fn bios_rom_aliasing_maps_reset_vector_at_top_of_4gib() {
    let mut bus = new_bus(true, 2 * 1024 * 1024);

    let mut rom = vec![0u8; BIOS_ROM_SIZE];
    for (i, byte) in rom.iter_mut().enumerate() {
        *byte = (i & 0xFF) as u8;
    }
    let rom = Arc::<[u8]>::from(rom);
    bus.map_system_bios_rom(Arc::clone(&rom)).unwrap();

    let reset_off = (BIOS_ROM_SIZE - 16) as u64;
    let low_addr = BIOS_ROM_BASE + reset_off;

    let mut low = [0u8; 16];
    bus.read_physical(low_addr, &mut low);

    let mut high = [0u8; 16];
    bus.read_physical(BIOS_RESET_VECTOR_PHYS, &mut high);

    assert_eq!(high, low);
    assert_eq!(high, rom[reset_off as usize..reset_off as usize + 16]);
}

#[test]
fn map_system_bios_rom_is_idempotent_for_identical_remaps() {
    let mut bus = new_bus(true, 0);
    let rom = Arc::<[u8]>::from(vec![0u8; BIOS_ROM_SIZE]);

    assert_eq!(bus.map_system_bios_rom(Arc::clone(&rom)), Ok(()));
    assert_eq!(bus.map_system_bios_rom(rom), Ok(()));
}

#[test]
fn rom_is_read_only_and_does_not_write_through_to_ram() {
    let mut bus = new_bus(true, 2 * 1024 * 1024);

    let addr = BIOS_ROM_BASE + 0x1234;
    bus.ram_mut().write_u8_le(addr, 0xAA).unwrap();

    let mut rom = vec![0xFFu8; BIOS_ROM_SIZE];
    rom[0x1234] = 0x11;
    let rom = Arc::<[u8]>::from(rom);
    bus.map_system_bios_rom(rom).unwrap();

    assert_eq!(bus.read_u8(addr), 0x11);
    bus.write_u8(addr, 0x55);
    assert_eq!(bus.read_u8(addr), 0x11);
    assert_eq!(bus.ram().read_u8_le(addr).unwrap(), 0xAA);
}

#[test]
fn mmio_overrides_rom_and_ram() {
    let mut bus = new_bus(true, 2 * 1024 * 1024);
    let addr = 0x1000u64;

    bus.ram_mut()
        .write_from(addr, &[0x01, 0x02, 0x03, 0x04])
        .unwrap();
    bus.map_rom(addr, Arc::from([0x10u8, 0x20, 0x30, 0x40].as_slice()))
        .unwrap();

    let (mmio, mmio_state) = RecordingMmio::new(vec![0xAA, 0xBB, 0xCC, 0xDD]);
    bus.map_mmio(addr, 4, Box::new(mmio)).unwrap();

    let mut readback = [0u8; 4];
    bus.read_physical(addr, &mut readback);
    assert_eq!(readback, [0xAA, 0xBB, 0xCC, 0xDD]);

    bus.write_physical(addr, &[0x11, 0x22, 0x33, 0x44]);

    let state = mmio_state.lock().unwrap();
    assert_eq!(state.writes.len(), 1);
    assert_eq!(state.mem, vec![0x11, 0x22, 0x33, 0x44]);
    drop(state);

    let mut ram_view = [0u8; 4];
    bus.ram().read_into(addr, &mut ram_view).unwrap();
    assert_eq!(ram_view, [0x01, 0x02, 0x03, 0x04]);
}

#[test]
fn a20_masking_aliases_high_mmio_addresses() {
    let mut bus = new_bus(false, 2 * 1024 * 1024);

    // With A20 disabled, 0x1_00000 aliases 0x0.
    bus.write_u8(0x0, 0xAA);
    assert_eq!(bus.read_u8(0x1_00000), 0xAA);

    // Enable A20: now 0x1_00000 is distinct from 0x0.
    bus.a20().set_enabled(true);
    bus.write_u8(0x1_00000, 0xBB);
    assert_eq!(bus.read_u8(0x0), 0xAA);
    assert_eq!(bus.read_u8(0x1_00000), 0xBB);

    // A20 masking applies to *all* physical accesses, including high MMIO addresses.
    //
    // Example: if a device is mapped at 0xFEC0_0000, and A20 is disabled, then a guest
    // access to 0xFED0_0000 (bit 20 set) aliases to 0xFEC0_0000 (bit 20 cleared).
    let mmio_base = IOAPIC_MMIO_BASE;
    let mmio_alias = mmio_base | (1 << 20);

    let (mmio, mmio_state) = RecordingMmio::new(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    bus.map_mmio(mmio_base, 4, Box::new(mmio)).unwrap();

    bus.a20().set_enabled(false);
    let mut buf = [0u8; 4];
    bus.read_physical(mmio_alias, &mut buf);
    assert_eq!(buf, [0xDE, 0xAD, 0xBE, 0xEF]);

    bus.a20().set_enabled(true);
    buf.fill(0);
    bus.read_physical(mmio_alias, &mut buf);
    assert_eq!(buf, [0xFF; 4]);

    let state = mmio_state.lock().unwrap();
    assert_eq!(state.reads.len(), 1);
    assert_eq!(state.reads[0], (0, 4));
}

#[test]
fn a20_disabled_only_clears_bit20_not_full_20bit_wrap() {
    let mut bus = new_bus(false, 4 * 1024 * 1024);

    bus.write_u8(0x0, 0x11);
    bus.write_u8(0x2_00000, 0x22);

    // Bit 21 should remain significant when A20 is disabled: 0x2_00000 is distinct from 0x0.
    assert_eq!(bus.read_u8(0x0), 0x11);
    assert_eq!(bus.read_u8(0x2_00000), 0x22);

    // But bit 20 is forced low, so 0x3_00000 aliases 0x2_00000.
    assert_eq!(bus.read_u8(0x3_00000), 0x22);
}

#[test]
fn a20_disabled_crossing_2mib_boundary_splits_correctly() {
    // Crossing 0x1F_FFFF -> 0x20_0000 flips bit 20 from 1 to 0. With A20 disabled, reads/writes
    // must therefore jump from the low 1MiB alias back to the true 0x2_00000 region.
    let mut bus = new_bus(false, 4 * 1024 * 1024);

    // Seed distinct bytes in true physical RAM around the aliased and non-aliased regions.
    bus.ram_mut().write_u8_le(0x0F_FFFE, 0x11).unwrap();
    bus.ram_mut().write_u8_le(0x0F_FFFF, 0x22).unwrap();
    bus.ram_mut().write_u8_le(0x2_00000, 0x33).unwrap();
    bus.ram_mut().write_u8_le(0x2_00001, 0x44).unwrap();

    let mut buf = [0u8; 4];
    bus.read_physical(0x1F_FFFE, &mut buf);
    assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);

    bus.write_physical(0x1F_FFFE, &[0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(bus.ram().read_u8_le(0x0F_FFFE).unwrap(), 0xAA);
    assert_eq!(bus.ram().read_u8_le(0x0F_FFFF).unwrap(), 0xBB);
    assert_eq!(bus.ram().read_u8_le(0x2_00000).unwrap(), 0xCC);
    assert_eq!(bus.ram().read_u8_le(0x2_00001).unwrap(), 0xDD);
}

struct CountingRam {
    inner: memory::DenseMemory,
    reads: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
}

impl CountingRam {
    fn new(size: u64, reads: Arc<AtomicUsize>, writes: Arc<AtomicUsize>) -> Self {
        Self {
            inner: memory::DenseMemory::new(size).unwrap(),
            reads,
            writes,
        }
    }
}

impl memory::GuestMemory for CountingRam {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> memory::GuestMemoryResult<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> memory::GuestMemoryResult<()> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write_from(paddr, src)
    }
}

#[test]
fn a20_disabled_reads_use_bulk_ram_backend_ops_not_per_byte_loop() {
    let reads = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));

    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());
    let ram = CountingRam::new(2 * 1024 * 1024, reads.clone(), writes.clone());
    let mut bus = MemoryBus::with_ram(filter, Box::new(ram));

    let start = 0x0FFF_00u64;
    let len = 0x3000usize;
    let mut buf = vec![0u8; len];
    bus.read_physical(start, &mut buf);

    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(
        reads.load(Ordering::Relaxed),
        2,
        "expected one RAM read per 1MiB chunk when A20 is disabled"
    );
}

#[test]
fn a20_disabled_writes_use_bulk_ram_backend_ops_not_per_byte_loop() {
    let reads = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));

    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());
    let ram = CountingRam::new(2 * 1024 * 1024, reads.clone(), writes.clone());
    let mut bus = MemoryBus::with_ram(filter, Box::new(ram));

    let start = 0x0FFF_00u64;
    let data = vec![0xAAu8; 0x3000];
    bus.write_physical(start, &data);

    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(
        writes.load(Ordering::Relaxed),
        2,
        "expected one RAM write per 1MiB chunk when A20 is disabled"
    );
}

#[test]
fn a20_disabled_crossing_1mib_boundary_within_mmio_wraps_offsets() {
    // When A20 is disabled, bit 20 is forced low. A read that crosses a 1MiB boundary at a high
    // MMIO address must therefore "wrap" back by 1MiB (i.e. alias), and MUST NOT be treated as a
    // single contiguous physical access at the unmasked address.
    let mut bus = new_bus(false, 0);

    // Large enough to include offsets both before and after the 1MiB boundary.
    let mmio_len = (1 << 20) + 0x40u64;
    let mut mem = vec![0xEEu8; mmio_len as usize];

    // Bytes just before the 1MiB boundary.
    for i in 0..0x10usize {
        mem[0x0F_FFF0 + i] = 0xA0u8.wrapping_add(i as u8);
    }
    // Bytes at the start of the region (where the wrapped portion should land).
    for i in 0..0x30usize {
        mem[i] = 0xB0u8.wrapping_add(i as u8);
    }

    // Use IOAPIC MMIO base (bit 20 clear) so crossing +1MiB flips bit 20.
    let mmio_base = IOAPIC_MMIO_BASE;
    let (mmio, mmio_state) = RecordingMmio::new(mem);
    bus.map_mmio(mmio_base, mmio_len, Box::new(mmio)).unwrap();

    let start = mmio_base + 0x0F_FFF0;
    let mut buf = [0u8; 0x40];
    bus.read_physical(start, &mut buf);

    let mut expected = [0u8; 0x40];
    for i in 0..0x10usize {
        expected[i] = 0xA0u8.wrapping_add(i as u8);
    }
    for i in 0..0x30usize {
        expected[0x10 + i] = 0xB0u8.wrapping_add(i as u8);
    }
    assert_eq!(buf, expected);

    // Ensure we did not accidentally perform a linear read past the 1MiB boundary (which would
    // have hit offsets >= 0x1_00000 inside the MMIO handler).
    let state = mmio_state.lock().unwrap();
    assert!(
        state.reads.iter().all(|(off, _)| *off < (1 << 20)),
        "unexpected MMIO reads past 1MiB boundary: {:?}",
        state.reads
    );
}

#[test]
fn a20_disabled_crossing_1mib_boundary_within_mmio_wraps_writes() {
    let mut bus = new_bus(false, 0);

    let mmio_len = (1 << 20) + 0x40u64;
    let (mmio, mmio_state) = RecordingMmio::new(vec![0xEEu8; mmio_len as usize]);

    let mmio_base = IOAPIC_MMIO_BASE;
    bus.map_mmio(mmio_base, mmio_len, Box::new(mmio)).unwrap();

    let start = mmio_base + 0x0F_FFF0;
    let data: Vec<u8> = (0..0x40u8).map(|v| v.wrapping_add(0x10)).collect();
    bus.write_physical(start, &data);

    // Writes should wrap, so offsets >= 1MiB should remain untouched.
    let state = mmio_state.lock().unwrap();

    assert!(
        state.writes.iter().all(|(off, _, _)| *off < (1 << 20)),
        "unexpected MMIO writes past 1MiB boundary: {:?}",
        state.writes
    );

    // First 0x10 bytes land just before the boundary.
    assert_eq!(&state.mem[0x0F_FFF0..0x1_00000], &data[..0x10]);
    // Remaining bytes wrap to the start of the region.
    assert_eq!(&state.mem[0..0x30], &data[0x10..]);

    // Ensure we did not write linearly into offsets >= 1MiB.
    assert_eq!(&state.mem[0x1_00000..0x1_00030], &[0xEEu8; 0x30]);
}

#[test]
fn a20_disabled_crossing_1mib_boundary_within_rom_wraps_reads() {
    let mut bus = new_bus(false, 0);

    // Map a ROM region that spans past a 1MiB boundary. With A20 disabled, accesses that cross
    // the boundary must wrap back by 1MiB (bit20 forced low), even if the underlying ROM mapping
    // is contiguous across that boundary.
    let rom_len = (1 << 20) + 0x40usize;
    let mut rom = vec![0xEEu8; rom_len];

    // Bytes just before the 1MiB boundary within the ROM.
    for i in 0..0x10usize {
        rom[0x0F_FFF0 + i] = 0xA0u8.wrapping_add(i as u8);
    }
    // Bytes at the start of the ROM (where the wrapped portion should land).
    for i in 0..0x30usize {
        rom[i] = 0xB0u8.wrapping_add(i as u8);
    }
    // Bytes immediately after the boundary that would be read if the implementation incorrectly
    // performed a linear read past the 1MiB boundary.
    for i in 0..0x30usize {
        rom[0x1_00000 + i] = 0xC0u8.wrapping_add(i as u8);
    }

    let rom_base = IOAPIC_MMIO_BASE;
    bus.map_rom(rom_base, Arc::<[u8]>::from(rom)).unwrap();

    let start = rom_base + 0x0F_FFF0;
    let mut buf = [0u8; 0x40];
    bus.read_physical(start, &mut buf);

    let mut expected = [0u8; 0x40];
    for i in 0..0x10usize {
        expected[i] = 0xA0u8.wrapping_add(i as u8);
    }
    for i in 0..0x30usize {
        expected[0x10 + i] = 0xB0u8.wrapping_add(i as u8);
    }
    assert_eq!(buf, expected);
}

#[test]
fn a20_masking_does_not_apply_to_direct_ram_backend_access() {
    let mut bus = new_bus(false, 2 * 1024 * 1024);

    // Seed distinct bytes in true physical RAM.
    bus.ram_mut().write_u8_le(0x0, 0x11).unwrap();
    bus.ram_mut().write_u8_le(0x1_00000, 0x22).unwrap();

    // Guest-visible physical reads are A20-masked, so 0x1_00000 aliases to 0x0.
    assert_eq!(bus.read_u8(0x0), 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x11);

    // Snapshot/restore logic must bypass A20 masking and use the underlying RAM backend directly.
    assert_eq!(bus.ram().read_u8_le(0x0).unwrap(), 0x11);
    assert_eq!(bus.ram().read_u8_le(0x1_00000).unwrap(), 0x22);

    // Guest-visible writes are also masked.
    bus.write_u8(0x1_00000, 0x33);
    assert_eq!(bus.ram().read_u8_le(0x0).unwrap(), 0x33);
    assert_eq!(bus.ram().read_u8_le(0x1_00000).unwrap(), 0x22);

    // Direct RAM backend writes still target the true physical address.
    bus.ram_mut().write_u8_le(0x1_00000, 0x44).unwrap();
    assert_eq!(bus.ram().read_u8_le(0x1_00000).unwrap(), 0x44);
}

#[test]
fn a20_disabled_bulk_reads_match_byte_wise_across_1mib_boundary() {
    let mut bus = new_bus(false, 2 * 1024 * 1024);

    // Fill the first MiB with a deterministic pattern and the second MiB with a different one so
    // incorrect non-wrapping reads are visible.
    let mut low = vec![0u8; 1 << 20];
    for (i, byte) in low.iter_mut().enumerate() {
        *byte = i as u8;
    }
    bus.ram_mut().write_from(0, &low).unwrap();
    bus.ram_mut().write_from(1 << 20, &vec![0xEEu8; 1 << 20]).unwrap();

    // Start just below the 1MiB boundary so the access wraps when A20 is disabled.
    let start = 0x0FFF_00u64;
    let len = 0x3000usize;

    let mut bulk = vec![0u8; len];
    bus.read_physical(start, &mut bulk);

    // Reference behaviour: byte-wise reads with per-address A20 masking.
    let mut expected = vec![0u8; len];
    for i in 0..len {
        expected[i] = bus.read_u8(start + i as u64);
    }

    assert_eq!(bulk, expected);
}

#[test]
fn a20_disabled_bulk_writes_match_byte_wise_across_1mib_boundary() {
    let start = 0x0FFF_00u64;
    let len = 0x3000usize;
    let data: Vec<u8> = (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect();

    let mut bulk = new_bus(false, 2 * 1024 * 1024);
    let mut bytewise = new_bus(false, 2 * 1024 * 1024);

    // Pre-fill the second MiB with a sentinel so incorrect non-aliased writes are visible.
    bulk.ram_mut()
        .write_from(1 << 20, &vec![0xEEu8; 1 << 20])
        .unwrap();
    bytewise
        .ram_mut()
        .write_from(1 << 20, &vec![0xEEu8; 1 << 20])
        .unwrap();

    bulk.write_physical(start, &data);
    for (i, byte) in data.iter().copied().enumerate() {
        bytewise.write_u8(start + i as u64, byte);
    }

    // Compare true physical RAM contents. The guest-visible behaviour should be identical, even
    // though the bulk path uses chunked reads/writes instead of a byte loop.
    let mut bulk_snapshot = vec![0u8; 2 << 20];
    let mut byte_snapshot = vec![0u8; 2 << 20];
    bulk.ram().read_into(0, &mut bulk_snapshot).unwrap();
    bytewise.ram().read_into(0, &mut byte_snapshot).unwrap();
    assert_eq!(bulk_snapshot, byte_snapshot);
}

#[test]
fn a20_disabled_wraparound_preserves_mmio_and_rom_priority() {
    let mut bus = new_bus(false, 2 * 1024 * 1024);

    // Seed low RAM so ROM reads are distinguishable from the underlying RAM contents.
    bus.ram_mut().write_from(0, &[0xAA; 0x100]).unwrap();
    // Also seed the second MiB with a different value so incorrect non-wrapping accesses show up.
    bus.ram_mut()
        .write_from(1 << 20, &[0xEE; 0x100])
        .unwrap();

    // Map a ROM window and overlay part of it with MMIO. Priority must remain:
    // MMIO > ROM > RAM.
    let rom = (0..0x20u8).map(|v| v.wrapping_add(0x10)).collect::<Vec<_>>();
    bus.map_rom(0x10, Arc::<[u8]>::from(rom)).unwrap();

    let (mmio, mmio_state) = RecordingMmio::new(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    bus.map_mmio(0x18, 4, Box::new(mmio)).unwrap();

    // Bulk read across the 1MiB boundary (wrap-around in the A20-disabled case).
    let start = 0x0FFF_F0u64;
    let len = 0x40usize;
    let mut bulk = vec![0u8; len];
    bus.read_physical(start, &mut bulk);

    // Verify against byte-granular reads, which naturally apply A20 masking per address.
    let mut expected = vec![0u8; len];
    for i in 0..len {
        expected[i] = bus.read_u8(start + i as u64);
    }
    assert_eq!(bulk, expected);

    // The bytes after wrap-around should observe MMIO and ROM priority.
    // - 0x10..0x18: ROM
    // - 0x18..0x1C: MMIO overrides ROM
    assert_eq!(bus.read_u8(0x1_00010), bus.read_u8(0x10));
    assert_eq!(bus.read_u8(0x1_00018), 0xDE);
    assert_eq!(bus.read_u8(0x1_0001B), 0xEF);

    // Bulk writes must route to MMIO (not ROM/RAM), ignore ROM, and update RAM elsewhere, even
    // when the write wraps at 1MiB.
    let write_len = 0x100usize;
    let data: Vec<u8> = (0..write_len).map(|v| v as u8).collect();
    let write_start = 0x0FFF_F0u64;
    bus.write_physical(write_start, &data);

    // Wrapped portion lands at 0.. in the A20-masked address space.
    // Offsets relative to 0 correspond to indices starting at `0x10` (because the first 0x10 bytes
    // of the write are still in the high region below 1MiB).
    let wrapped_base = 0x10usize;

    // RAM region at 0x00..0x10 should be written.
    let mut ram_low = [0u8; 0x10];
    bus.ram().read_into(0, &mut ram_low).unwrap();
    assert_eq!(&ram_low, &data[wrapped_base..wrapped_base + 0x10]);

    // ROM region (0x10..0x18) must not write through to RAM.
    let mut rom_ram_view = [0u8; 0x08];
    bus.ram().read_into(0x10, &mut rom_ram_view).unwrap();
    assert_eq!(&rom_ram_view, &[0xAA; 0x08]);

    // MMIO region (0x18..0x1C) should receive the write.
    let state = mmio_state.lock().unwrap();
    assert_eq!(state.mem, data[wrapped_base + 0x18..wrapped_base + 0x1C]);
}

#[test]
fn dirty_tracking_marks_ram_writes_from_non_cpu_paths() {
    let chipset = ChipsetState::new(true);
    let filter = AddressFilter::new(chipset.a20());
    let mut bus =
        MemoryBus::new_with_dirty_tracking(filter, 16 * 1024 * 1024, DEFAULT_DIRTY_PAGE_SIZE);

    // Start clean.
    assert!(bus.take_dirty_pages().unwrap().is_empty());

    // "Device-like" path: write through the platform physical bus.
    bus.write_physical(0x2000, &[0xAA, 0xBB, 0xCC, 0xDD]);

    // Another non-CPU path: direct access to the RAM backend.
    bus.ram_mut().write_u8_le(0x3000, 0xEE).unwrap();

    let mut pages = bus.take_dirty_pages().unwrap();
    pages.sort_unstable();
    assert_eq!(pages, vec![2, 3]);

    // Drain semantics.
    assert!(bus.take_dirty_pages().unwrap().is_empty());
}

#[test]
fn map_rom_is_idempotent_for_identical_remaps() {
    let mut bus = new_bus(true, 0);
    let start = 0x1000u64;
    let rom = Arc::<[u8]>::from([0xAAu8, 0xBB, 0xCC, 0xDD].as_slice());

    assert_eq!(bus.map_rom(start, Arc::clone(&rom)), Ok(()));
    assert_eq!(bus.map_rom(start, rom), Ok(()));
}

#[test]
fn map_rom_still_errors_on_conflicting_overlaps() {
    let mut bus = new_bus(true, 0);
    let start = 0x2000u64;

    bus.map_rom(start, Arc::<[u8]>::from([0u8; 4].as_slice()))
        .unwrap();

    // Same start but different length is not considered an idempotent re-map.
    assert_eq!(
        bus.map_rom(start, Arc::<[u8]>::from([0u8; 5].as_slice())),
        Err(MapError::Overlap)
    );
}
