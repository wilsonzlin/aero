use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::{Exception, IoBus, PagingBus};
use aero_mmu::MemoryBus;

#[derive(Debug, Default, Clone)]
struct TestMem;

impl MemoryBus for TestMem {
    fn read_u8(&mut self, _paddr: u64) -> u8 {
        0
    }

    fn read_u16(&mut self, _paddr: u64) -> u16 {
        0
    }

    fn read_u32(&mut self, _paddr: u64) -> u32 {
        0
    }

    fn read_u64(&mut self, _paddr: u64) -> u64 {
        0
    }

    fn write_u8(&mut self, _paddr: u64, _value: u8) {}
    fn write_u16(&mut self, _paddr: u64, _value: u16) {}
    fn write_u32(&mut self, _paddr: u64, _value: u32) {}
    fn write_u64(&mut self, _paddr: u64, _value: u64) {}
}

#[derive(Debug, Default)]
struct TestIo {
    reads: Vec<(u16, u32)>,
    writes: Vec<(u16, u32, u64)>,
    read_value: u64,
}

impl IoBus for TestIo {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.reads.push((port, size));
        Ok(self.read_value)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.writes.push((port, size, val));
        Ok(())
    }
}

#[test]
fn paging_bus_forwards_port_io_to_backend() {
    let phys = TestMem;
    let io = TestIo {
        read_value: 0x1122_3344_5566_7788,
        ..TestIo::default()
    };
    let mut bus = PagingBus::new_with_io(phys, io);

    assert_eq!(bus.io_read(0x3f8, 1).unwrap(), 0x1122_3344_5566_7788);
    bus.io_write(0x3f8, 1, 0x41).unwrap();

    assert_eq!(bus.io().reads, vec![(0x3f8, 1)]);
    assert_eq!(bus.io().writes, vec![(0x3f8, 1, 0x41)]);
}

#[test]
fn paging_bus_accepts_mut_io_reference() {
    let phys = TestMem;
    let mut io = TestIo {
        read_value: 0x1122_3344_5566_7788,
        ..TestIo::default()
    };

    {
        let mut bus = PagingBus::new_with_io(phys, &mut io);
        assert_eq!(bus.io_read(0x3f8, 1).unwrap(), 0x1122_3344_5566_7788);
        bus.io_write(0x3f8, 1, 0x41).unwrap();
    }

    assert_eq!(io.reads, vec![(0x3f8, 1)]);
    assert_eq!(io.writes, vec![(0x3f8, 1, 0x41)]);
}

#[test]
fn paging_bus_accepts_boxed_io_backend() {
    let phys = TestMem;
    let io = Box::new(TestIo {
        read_value: 0x1122_3344_5566_7788,
        ..TestIo::default()
    });
    let mut bus = PagingBus::new_with_io(phys, io);

    assert_eq!(bus.io_read(0x3f8, 1).unwrap(), 0x1122_3344_5566_7788);
    bus.io_write(0x3f8, 1, 0x41).unwrap();

    assert_eq!(bus.io().reads, vec![(0x3f8, 1)]);
    assert_eq!(bus.io().writes, vec![(0x3f8, 1, 0x41)]);
}
