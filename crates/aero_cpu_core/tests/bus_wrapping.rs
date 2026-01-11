#![cfg(feature = "legacy-interp")]

use aero_cpu_core::bus::Bus;

#[derive(Default)]
struct RecordingBus {
    reads: Vec<u64>,
    writes: Vec<(u64, u8)>,
}

impl Bus for RecordingBus {
    fn read_u8(&mut self, addr: u64) -> u8 {
        self.reads.push(addr);
        addr as u8
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.writes.push((addr, value));
    }
}

#[test]
fn bus_scalar_reads_use_wrapping_address_arithmetic() {
    let mut bus = RecordingBus::default();

    // u16 at u64::MAX should read the first byte at u64::MAX and wrap for the second byte.
    assert_eq!(bus.read_u16(u64::MAX), u16::from_le_bytes([0xFF, 0x00]));
    assert_eq!(bus.reads, vec![u64::MAX, 0]);

    bus.reads.clear();
    // u32 at u64::MAX-1 wraps twice: MAX-1, MAX, 0, 1.
    assert_eq!(
        bus.read_u32(u64::MAX - 1),
        u32::from_le_bytes([0xFE, 0xFF, 0x00, 0x01])
    );
    assert_eq!(bus.reads, vec![u64::MAX - 1, u64::MAX, 0, 1]);
}

#[test]
fn bus_scalar_writes_use_wrapping_address_arithmetic() {
    let mut bus = RecordingBus::default();

    // u32 at u64::MAX-1 wraps twice: MAX-1, MAX, 0, 1.
    bus.write_u32(u64::MAX - 1, 0x1122_3344);
    assert_eq!(
        bus.writes,
        vec![
            (u64::MAX - 1, 0x44),
            (u64::MAX, 0x33),
            (0, 0x22),
            (1, 0x11),
        ]
    );
}
