use std::cell::RefCell;

use aero_jit_x86::Tier1Bus;
use aero_types::Width;

#[derive(Default)]
struct RecordingBus {
    reads: RefCell<Vec<u64>>,
    writes: Vec<(u64, u8)>,
}

impl Tier1Bus for RecordingBus {
    fn read_u8(&self, addr: u64) -> u8 {
        self.reads.borrow_mut().push(addr);
        addr as u8
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.writes.push((addr, value));
    }
}

#[test]
fn tier1_bus_helpers_use_wrapping_address_arithmetic() {
    let bus = RecordingBus::default();

    // u16 at u64::MAX should read the first byte at u64::MAX and wrap for the second byte.
    assert_eq!(
        bus.read(u64::MAX, Width::W16),
        u64::from(u16::from_le_bytes([0xFF, 0x00]))
    );
    assert_eq!(&*bus.reads.borrow(), &[u64::MAX, 0]);
    bus.reads.borrow_mut().clear();

    // u32 at u64::MAX-1 wraps twice: MAX-1, MAX, 0, 1.
    assert_eq!(
        bus.read(u64::MAX - 1, Width::W32),
        u64::from(u32::from_le_bytes([0xFE, 0xFF, 0x00, 0x01]))
    );
    assert_eq!(&*bus.reads.borrow(), &[u64::MAX - 1, u64::MAX, 0, 1]);
    bus.reads.borrow_mut().clear();

    // Fetch helper must use the same wrapping arithmetic.
    assert_eq!(bus.fetch(u64::MAX - 1, 4), vec![0xFE, 0xFF, 0x00, 0x01]);
    assert_eq!(&*bus.reads.borrow(), &[u64::MAX - 1, u64::MAX, 0, 1]);
    bus.reads.borrow_mut().clear();

    // Fixed 15-byte decode window helper should match `fetch` semantics (and also wrap).
    let window = bus.fetch_window(u64::MAX - 1);
    assert_eq!(&window[..4], &[0xFE, 0xFF, 0x00, 0x01]);
    assert_eq!(
        &*bus.reads.borrow(),
        &[
            u64::MAX - 1,
            u64::MAX,
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
            12
        ]
    );
}

#[test]
fn tier1_bus_write_wraps_u64_addresses() {
    let mut bus = RecordingBus::default();
    bus.write(u64::MAX - 1, Width::W32, 0x1122_3344);
    assert_eq!(
        bus.writes,
        vec![(u64::MAX - 1, 0x44), (u64::MAX, 0x33), (0, 0x22), (1, 0x11)]
    );
}
