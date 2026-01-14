use std::cell::Cell;
use std::rc::Rc;

use aero_platform::io::{IoPortBus, PortIoDevice};

#[derive(Debug)]
struct RangeEcho {
    base: u16,
    len: u16,
}

impl PortIoDevice for RangeEcho {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        assert_eq!(size, 4);
        let offset = port.wrapping_sub(self.base);
        assert!(offset < self.len);
        0xAA00_0000 | u32::from(offset)
    }

    fn write(&mut self, _port: u16, _size: u8, _value: u32) {}
}

#[derive(Debug)]
struct ExactValue;

impl PortIoDevice for ExactValue {
    fn read(&mut self, _port: u16, _size: u8) -> u32 {
        0xDEAD_BEEF
    }

    fn write(&mut self, _port: u16, _size: u8, _value: u32) {}
}

#[test]
fn exact_port_precedence_over_range() {
    let mut bus = IoPortBus::new();

    const BASE: u16 = 0x3000;
    const LEN: u16 = 4;
    const OVERRIDE_PORT: u16 = BASE + 2;

    bus.register_range(
        BASE,
        LEN,
        Box::new(RangeEcho {
            base: BASE,
            len: LEN,
        }),
    );

    // Baseline: the range device handles all ports.
    assert_eq!(bus.read(BASE, 4), 0xAA00_0000);
    assert_eq!(bus.read(OVERRIDE_PORT, 4), 0xAA00_0002);

    // Exact port mappings must override range dispatch.
    bus.register(OVERRIDE_PORT, Box::new(ExactValue));
    assert_eq!(bus.read(OVERRIDE_PORT, 4), 0xDEAD_BEEF);

    // After unregister, dispatch should fall back to the range again.
    assert!(bus.unregister(OVERRIDE_PORT).is_some());
    assert_eq!(bus.read(OVERRIDE_PORT, 4), 0xAA00_0002);
}

#[test]
fn unregister_and_reregister_exact_port() {
    #[derive(Debug)]
    struct StatePort {
        state: Rc<Cell<u32>>,
    }

    impl PortIoDevice for StatePort {
        fn read(&mut self, _port: u16, size: u8) -> u32 {
            assert_eq!(size, 4);
            self.state.get()
        }

        fn write(&mut self, _port: u16, size: u8, value: u32) {
            assert_eq!(size, 4);
            self.state.set(value);
        }
    }

    let mut bus = IoPortBus::new();
    let port = 0x1234;

    let state1 = Rc::new(Cell::new(0));
    bus.register(
        port,
        Box::new(StatePort {
            state: state1.clone(),
        }),
    );

    bus.write(port, 4, 0x1111_2222);
    assert_eq!(bus.read(port, 4), 0x1111_2222);
    assert_eq!(state1.get(), 0x1111_2222);

    // Unregister should remove the handler and return ownership.
    assert!(bus.unregister(port).is_some());
    assert_eq!(bus.read(port, 4), 0xFFFF_FFFF);

    // Re-registering should install the new device cleanly.
    let state2 = Rc::new(Cell::new(0));
    bus.register(
        port,
        Box::new(StatePort {
            state: state2.clone(),
        }),
    );

    bus.write(port, 4, 0x3333_4444);
    assert_eq!(bus.read(port, 4), 0x3333_4444);
    assert_eq!(state2.get(), 0x3333_4444);
}

#[test]
fn invalid_sizes_float_high_and_are_not_dispatched() {
    #[derive(Debug)]
    struct SpyPort {
        reads: Rc<Cell<u32>>,
        writes: Rc<Cell<u32>>,
    }

    impl PortIoDevice for SpyPort {
        fn read(&mut self, _port: u16, _size: u8) -> u32 {
            self.reads.set(self.reads.get() + 1);
            0x1234_5678
        }

        fn write(&mut self, _port: u16, _size: u8, _value: u32) {
            self.writes.set(self.writes.get() + 1);
        }
    }

    let reads = Rc::new(Cell::new(0));
    let writes = Rc::new(Cell::new(0));

    let mut bus = IoPortBus::new();
    bus.register(
        0x1234,
        Box::new(SpyPort {
            reads: reads.clone(),
            writes: writes.clone(),
        }),
    );

    // Size-0 accesses are true no-ops.
    assert_eq!(bus.read(0x1234, 0), 0);
    bus.write(0x1234, 0, 0xDEAD_BEEF);
    assert_eq!(reads.get(), 0);
    assert_eq!(writes.get(), 0);

    // Invalid-sized reads float high and must not dispatch.
    assert_eq!(bus.read(0x1234, 3), 0xFFFF_FFFF);
    assert_eq!(reads.get(), 0);

    // Invalid-sized writes are ignored and must not dispatch.
    bus.write(0x1234, 3, 0xDEAD_BEEF);
    assert_eq!(writes.get(), 0);

    // Valid sizes still dispatch.
    assert_eq!(bus.read(0x1234, 1), 0x1234_5678);
    bus.write(0x1234, 1, 0);
    assert_eq!(reads.get(), 1);
    assert_eq!(writes.get(), 1);
}
