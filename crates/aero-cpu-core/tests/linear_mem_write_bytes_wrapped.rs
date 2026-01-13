use aero_cpu_core::linear_mem::write_bytes_wrapped;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;

#[test]
fn a20_disabled_wrapping_multi_megabyte_write_completes_and_writes_expected_bytes() {
    // With A20 disabled in real mode, bit 20 is forced low. A 2MiB linear write starting at 0
    // aliases every 1MiB and should not panic or allocate unbounded memory.
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    // Only the low 1MiB is addressable after A20 masking for the chosen range.
    let mut bus = FlatTestBus::new(0x100000);

    let mut src = vec![0xAAu8; 0x200000];
    src[0x100000..].fill(0x55);

    write_bytes_wrapped(&state, &mut bus, 0, &src).unwrap();

    // The second MiB aliases and overwrites the first, so RAM should contain src[1MiB..2MiB].
    assert_eq!(bus.slice(0, 0x100000), &src[0x100000..]);
}

#[test]
fn small_contiguous_write_still_uses_normal_semantics() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    let mut bus = FlatTestBus::new(0x100000);
    let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
    write_bytes_wrapped(&state, &mut bus, 0x1234, &src).unwrap();
    assert_eq!(bus.slice(0x1234, src.len()), &src);
}

#[derive(Clone)]
struct FaultingPreflightBus {
    inner: FlatTestBus,
    fail_start: u64,
}

impl FaultingPreflightBus {
    fn new(size: usize, fail_start: u64) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            fail_start,
        }
    }
}

impl CpuBus for FaultingPreflightBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, _len: usize) -> Result<(), Exception> {
        if vaddr == self.fail_start {
            return Err(Exception::MemoryFault);
        }
        self.inner.preflight_write_bytes(vaddr, _len)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn wrapped_write_is_fault_atomic_across_segments() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    // Fail preflight for any segment that starts at 0. A 2-byte write starting at 0xFFFFF with
    // A20 disabled will split into two segments: [0xFFFFF] and [0x0].
    let mut bus = FaultingPreflightBus::new(0x100000, 0);
    bus.write_u8(0xFFFFF, 0xAA).unwrap();
    bus.write_u8(0x0000, 0xBB).unwrap();

    let res = write_bytes_wrapped(&state, &mut bus, 0xFFFFF, &[0x11, 0x22]);
    assert!(res.is_err());

    // No bytes should have been committed because preflight failed for the second segment.
    assert_eq!(bus.read_u8(0xFFFFF).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x0000).unwrap(), 0xBB);
}

