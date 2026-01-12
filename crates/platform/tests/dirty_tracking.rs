use aero_platform::address_filter::AddressFilter;
use aero_platform::memory::MemoryBus;
use aero_platform::ChipsetState;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct MmioState {
    writes: Vec<(u64, usize, u64)>,
}

#[derive(Clone)]
struct RecordingMmio {
    state: Arc<Mutex<MmioState>>,
}

impl RecordingMmio {
    fn new() -> (Self, Arc<Mutex<MmioState>>) {
        let state = Arc::new(Mutex::new(MmioState::default()));
        (Self { state: state.clone() }, state)
    }
}

impl memory::MmioHandler for RecordingMmio {
    fn read(&mut self, _offset: u64, _size: usize) -> u64 {
        u64::MAX
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.state.lock().unwrap().writes.push((offset, size, value));
    }
}

fn new_bus_with_dirty_tracking(ram_size: usize) -> MemoryBus {
    let chipset = ChipsetState::new(true);
    let filter = AddressFilter::new(chipset.a20());
    MemoryBus::new_with_dirty_tracking(filter, ram_size, 4096)
}

#[test]
fn writes_through_bus_mark_expected_pages() {
    let mut bus = new_bus_with_dirty_tracking(3 * 4096);

    // Touch page 2 first to ensure we get a sorted result later.
    bus.write_physical(0x2000, &[1, 2, 3, 4]);

    // Cross a page boundary: last byte of page 0 + first byte of page 1.
    bus.write_physical(0x0FFF, &[0xAA, 0xBB]);

    let dirty = bus.take_dirty_pages().unwrap();
    assert_eq!(dirty, vec![0, 1, 2]);
}

#[test]
fn mmio_writes_do_not_mark_ram_pages() {
    let mut bus = new_bus_with_dirty_tracking(2 * 4096);

    // Map an MMIO region that overlaps guest RAM. MMIO must take precedence.
    let (mmio, state) = RecordingMmio::new();
    bus.map_mmio(0x1000, 8, Box::new(mmio)).unwrap();

    bus.write_physical(0x1000, &[0x11, 0x22, 0x33, 0x44]);

    // MMIO write was observed...
    let state = state.lock().unwrap();
    assert_eq!(state.writes.len(), 1);
    drop(state);

    // ...but guest RAM dirty tracking did not record any pages.
    assert_eq!(bus.take_dirty_pages().unwrap(), Vec::<u64>::new());
}

#[test]
fn take_dirty_pages_returns_sorted_deduped_and_clears() {
    let mut bus = new_bus_with_dirty_tracking(4 * 4096);

    // Duplicate writes to page 3, out-of-order writes overall.
    bus.write_physical(0x3000, &[0xAA]);
    bus.write_physical(0x0000, &[0xBB]);
    bus.write_physical(0x2000, &[0xCC]);
    bus.write_physical(0x3001, &[0xDD]);

    let dirty = bus.take_dirty_pages().unwrap();
    assert_eq!(dirty, vec![0, 2, 3]);

    // `take_dirty_pages` clears the bitmap.
    assert_eq!(bus.take_dirty_pages().unwrap(), Vec::<u64>::new());
}

#[test]
fn get_slice_mut_marks_pages_dirty_conservatively() {
    let mut bus = new_bus_with_dirty_tracking(2 * 4096);

    // Borrowing a mutable slice is treated as a potential write, so we mark pages as dirty
    // immediately even if the caller performs no actual mutation.
    let slice = bus.ram_mut().get_slice_mut(0x0FFF, 2).unwrap();
    slice.copy_from_slice(&[0xAA, 0xBB]);

    let dirty = bus.take_dirty_pages().unwrap();
    assert_eq!(dirty, vec![0, 1]);
}

#[test]
fn clear_dirty_discards_accumulated_pages() {
    let mut bus = new_bus_with_dirty_tracking(2 * 4096);

    bus.write_physical(0x100, &[0x11, 0x22, 0x33]);
    bus.clear_dirty();

    assert_eq!(bus.take_dirty_pages().unwrap(), Vec::<u64>::new());
}
