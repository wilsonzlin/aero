use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_platform::address_filter::AddressFilter;
use aero_platform::dirty_memory::DEFAULT_DIRTY_PAGE_SIZE;
use aero_platform::memory::{MemoryBus, BIOS_RESET_VECTOR_PHYS, BIOS_ROM_BASE, BIOS_ROM_SIZE};
use aero_platform::ChipsetState;
use memory::MapError;
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
