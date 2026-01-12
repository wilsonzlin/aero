use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG};
use aero_cpu_core::{Exception, IoBus, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;

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

#[derive(Clone, Debug)]
struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }
}

impl MemoryBus for TestMemory {
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.data[paddr as usize]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        let off = paddr as usize;
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        let off = paddr as usize;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        let off = paddr as usize;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.data[paddr as usize] = value;
    }

    fn write_u16(&mut self, paddr: u64, value: u16) {
        let off = paddr as usize;
        self.data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, value: u32) {
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }
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

#[test]
fn pagingbus_io_delegates_when_paging_disabled() {
    let phys = TestMemory::new(0x10000);
    let io = TestIo {
        read_value: 0x1122_3344_5566_7788,
        ..TestIo::default()
    };
    let mut bus = PagingBus::new_with_io(phys, io);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE; // CR0.PG=0
    state.update_mode();
    bus.sync(&state);

    // Memory access still works (identity mapping with 32-bit truncation).
    bus.write_u8(0x1234, 0xAA).unwrap();
    assert_eq!(bus.read_u8(0x1234).unwrap(), 0xAA);

    // Port I/O is delegated to the injected backend.
    bus.io_write(0x80, 1, 0x55).unwrap();
    assert_eq!(bus.io().writes, vec![(0x80, 1, 0x55)]);

    assert_eq!(bus.io_read(0x81, 4).unwrap(), 0x1122_3344_5566_7788);
    assert_eq!(bus.io().reads, vec![(0x81, 4)]);
}

#[test]
fn pagingbus_io_delegates_when_paging_enabled() {
    const PTE_P: u32 = 1 << 0;
    const PTE_RW: u32 = 1 << 1;
    const PTE_US: u32 = 1 << 2;

    let mut phys = TestMemory::new(0x10000);

    // Legacy 32-bit paging (CR4.PAE=0): PD -> PT -> 4KiB page.
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let data_page = 0x3000u64;

    // PDE[0] -> PT
    phys.write_u32(pd_base, (pt_base as u32) | PTE_P | PTE_RW | PTE_US);
    // PTE[0] -> data page
    phys.write_u32(pt_base, (data_page as u32) | PTE_P | PTE_RW | PTE_US);
    phys.write_u8(data_page, 0xCC);

    let io = TestIo {
        read_value: 0xDEAD_BEEF,
        ..TestIo::default()
    };
    let mut bus = PagingBus::new_with_io(phys, io);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pd_base;
    state.update_mode();
    bus.sync(&state);

    // Prove paging translation still works with an injected IO backend.
    assert_eq!(bus.read_u8(0).unwrap(), 0xCC);
    bus.write_u8(0, 0xDD).unwrap();
    assert_eq!(bus.inner_mut().read_u8(data_page), 0xDD);

    // And port I/O still reaches the backend.
    bus.io_write(0x3F8, 1, 0x41).unwrap();
    assert_eq!(bus.io().writes, vec![(0x3F8, 1, 0x41)]);

    assert_eq!(bus.io_read(0x3F8, 1).unwrap(), 0xDEAD_BEEF);
    assert_eq!(bus.io().reads, vec![(0x3F8, 1)]);
}
