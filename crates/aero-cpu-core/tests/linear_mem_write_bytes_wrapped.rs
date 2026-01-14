use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::thread_local;

use aero_cpu_core::linear_mem::{fetch_wrapped, read_bytes_wrapped, write_bytes_wrapped};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;

// --- Allocation regression harness ------------------------------------------

struct CountingAlloc;

// Use thread-local counters/enablement so the allocation assertions are not
// affected by the Rust test runner executing other tests in parallel.
thread_local! {
    static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOC_CALLS: Cell<usize> = const { Cell::new(0) };
    static REALLOC_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            COUNT_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOC_CALLS.with(|calls| calls.set(calls.get() + 1));
                }
            });
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = System.realloc(ptr, layout, new_size);
        if !ptr.is_null() {
            COUNT_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    REALLOC_CALLS.with(|calls| calls.set(calls.get() + 1));
                }
            });
        }
        ptr
    }
}

struct AllocationCountGuard;

impl AllocationCountGuard {
    fn begin() -> Self {
        COUNT_ALLOCATIONS.with(|enabled| {
            assert!(
                !enabled.get(),
                "nested AllocationCountGuard::begin calls are not supported"
            );
            enabled.set(true);
        });
        reset_alloc_counters();
        Self
    }
}

impl Drop for AllocationCountGuard {
    fn drop(&mut self) {
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    }
}

fn reset_alloc_counters() {
    ALLOC_CALLS.with(|calls| calls.set(0));
    REALLOC_CALLS.with(|calls| calls.set(0));
}

fn alloc_calls() -> usize {
    ALLOC_CALLS.with(|calls| calls.get()) + REALLOC_CALLS.with(|calls| calls.get())
}

// --- Test busses ------------------------------------------------------------

/// A sparse bus with exactly two contiguous regions: a low window and a high window.
///
/// Used to test 32-bit linear wrapping without allocating a 4GiB backing store.
struct SplitTestBus {
    low: Vec<u8>,
    high_base: u64,
    high: Vec<u8>,
}

impl SplitTestBus {
    fn new(low_len: usize, high_base: u64, high_len: usize) -> Self {
        Self {
            low: vec![0; low_len],
            high_base,
            high: vec![0; high_len],
        }
    }

    fn region(&self, vaddr: u64, len: usize) -> Result<&[u8], Exception> {
        if vaddr < self.low.len() as u64 {
            let start = usize::try_from(vaddr).map_err(|_| Exception::MemoryFault)?;
            let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
            if end > self.low.len() {
                return Err(Exception::MemoryFault);
            }
            return Ok(&self.low[start..end]);
        }

        let off = vaddr.checked_sub(self.high_base).ok_or(Exception::MemoryFault)?;
        let start = usize::try_from(off).map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.high.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(&self.high[start..end])
    }

    fn region_mut(&mut self, vaddr: u64, len: usize) -> Result<&mut [u8], Exception> {
        if vaddr < self.low.len() as u64 {
            let start = usize::try_from(vaddr).map_err(|_| Exception::MemoryFault)?;
            let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
            if end > self.low.len() {
                return Err(Exception::MemoryFault);
            }
            return Ok(&mut self.low[start..end]);
        }

        let off = vaddr.checked_sub(self.high_base).ok_or(Exception::MemoryFault)?;
        let start = usize::try_from(off).map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.high.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(&mut self.high[start..end])
    }
}

impl CpuBus for SplitTestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        Ok(self.region(vaddr, 1)?[0])
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let mut buf = [0u8; 2];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.region_mut(vaddr, 1)?[0] = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        dst.copy_from_slice(self.region(vaddr, dst.len())?);
        Ok(())
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.region_mut(vaddr, src.len())?.copy_from_slice(src);
        Ok(())
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.region(vaddr, len).map(|_| ())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        self.read_bytes(vaddr, &mut buf[..len])?;
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}

struct CountingReadBus {
    inner: SplitTestBus,
    read_u8_calls: usize,
    read_bytes_calls: usize,
}

impl CountingReadBus {
    fn new(inner: SplitTestBus) -> Self {
        Self {
            inner,
            read_u8_calls: 0,
            read_bytes_calls: 0,
        }
    }
}

impl CpuBus for CountingReadBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.read_u8_calls += 1;
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

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.read_bytes_calls += 1;
        self.inner.read_bytes(vaddr, dst)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.inner.write_bytes(vaddr, src)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.inner.preflight_write_bytes(vaddr, len)
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

struct CountingFetchBus {
    inner: SplitTestBus,
    fetch_calls: usize,
}

impl CountingFetchBus {
    fn new(inner: SplitTestBus) -> Self {
        Self {
            inner,
            fetch_calls: 0,
        }
    }
}

impl CpuBus for CountingFetchBus {
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

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.inner.read_bytes(vaddr, dst)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.inner.write_bytes(vaddr, src)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.inner.preflight_write_bytes(vaddr, len)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.fetch_calls += 1;
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[derive(Clone)]
struct FaultingPreflightBus {
    inner: FlatTestBus,
    fault_vaddr: u64,
}

impl FaultingPreflightBus {
    fn new(size: usize, fault_vaddr: u64) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            fault_vaddr,
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

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.inner.read_bytes(vaddr, dst)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.inner.write_bytes(vaddr, src)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        if vaddr == self.fault_vaddr {
            return Err(Exception::MemoryFault);
        }
        self.inner.preflight_write_bytes(vaddr, len)
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

// --- Tests ------------------------------------------------------------------

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

#[test]
fn read_bytes_wrapped_large_32bit_wrap_uses_segment_reads() {
    let state = CpuState::new(CpuMode::Bit32);

    let mut inner = SplitTestBus::new(0x200000, 0xFFFF_F000, 0x1000);
    inner.low.fill(0x22);
    inner.high.fill(0x11);
    let mut bus = CountingReadBus::new(inner);

    let mut dst = vec![0u8; 0x100000];
    read_bytes_wrapped(&state, &mut bus, 0xFFFF_F000, &mut dst).unwrap();

    assert!(dst[..0x1000].iter().all(|&b| b == 0x11));
    assert!(dst[0x1000..].iter().all(|&b| b == 0x22));

    assert_eq!(bus.read_bytes_calls, 2);
    assert_eq!(bus.read_u8_calls, 0);
}

#[test]
fn read_bytes_wrapped_long_u64_wrap_uses_segment_reads() {
    let state = CpuState::new(CpuMode::Bit64);

    let high_base = u64::MAX - 0xF;
    let mut inner = SplitTestBus::new(0x100, high_base, 0x10);
    inner.high.fill(0x11);
    inner.low.fill(0x22);
    let mut bus = CountingReadBus::new(inner);

    // Read across u64 wrap: [u64::MAX-7..=u64::MAX] then [0..=7].
    let mut dst = [0u8; 16];
    read_bytes_wrapped(&state, &mut bus, u64::MAX - 7, &mut dst).unwrap();

    assert!(dst[..8].iter().all(|&b| b == 0x11));
    assert!(dst[8..].iter().all(|&b| b == 0x22));

    assert_eq!(bus.read_bytes_calls, 2);
    assert_eq!(bus.read_u8_calls, 0);
}

#[test]
fn read_bytes_wrapped_a20_wrap_uses_segment_reads() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    let mut inner = SplitTestBus::new(0x100000, u64::MAX, 0);
    inner.low[0x0FF000..0x100000].fill(0x11);
    inner.low[0x000000..0x001000].fill(0x22);
    let mut bus = CountingReadBus::new(inner);

    let mut dst = [0u8; 0x2000];
    read_bytes_wrapped(&state, &mut bus, 0x0FF000, &mut dst).unwrap();

    assert!(dst[..0x1000].iter().all(|&b| b == 0x11));
    assert!(dst[0x1000..].iter().all(|&b| b == 0x22));

    assert_eq!(bus.read_bytes_calls, 2);
    assert_eq!(bus.read_u8_calls, 0);
}

#[test]
fn fetch_wrapped_large_32bit_wrap_uses_segment_fetches() {
    let state = CpuState::new(CpuMode::Bit32);

    let mut inner = SplitTestBus::new(0x100, 0xFFFF_FFF0, 0x40);
    for i in 0..inner.high.len() {
        inner.high[i] = 0xD0 | (i as u8);
    }
    inner.low[0] = 0xAA;

    let mut bus = CountingFetchBus::new(inner);

    // Start close enough to 4GiB so that a 15-byte fetch wraps. This should split into 2 segments:
    // [0xFFFF_FFF2..=0xFFFF_FFFF] then [0x0].
    let buf = fetch_wrapped(&state, &mut bus, 0xFFFF_FFF2, 15).unwrap();

    let mut expected = [0u8; 15];
    for i in 0..14 {
        expected[i] = 0xD0 | ((i + 2) as u8);
    }
    expected[14] = 0xAA;

    assert_eq!(&buf[..15], &expected);
    assert_eq!(bus.fetch_calls, 2);
}

#[test]
fn fetch_wrapped_long_u64_wrap_uses_segment_fetches() {
    let state = CpuState::new(CpuMode::Bit64);

    let high_base = u64::MAX - 0xF;
    let mut inner = SplitTestBus::new(0x100, high_base, 0x10);
    for i in 0..inner.high.len() {
        inner.high[i] = 0xD0 | (i as u8);
    }
    for i in 0..8 {
        inner.low[i] = 0xA0 | (i as u8);
    }

    let mut bus = CountingFetchBus::new(inner);

    let buf = fetch_wrapped(&state, &mut bus, u64::MAX - 7, 15).unwrap();

    let mut expected = [0u8; 15];
    // First segment: high[8..16]
    for i in 0..8 {
        expected[i] = 0xD0 | ((i + 8) as u8);
    }
    // Second segment: low[0..7]
    for i in 0..7 {
        expected[8 + i] = 0xA0 | (i as u8);
    }

    assert_eq!(&buf[..15], &expected);
    assert_eq!(bus.fetch_calls, 2);
}

#[test]
fn fetch_wrapped_a20_wrap_uses_segment_fetches() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    let mut inner = SplitTestBus::new(0x100000, u64::MAX, 0);
    for i in 0..14 {
        inner.low[0x0FFFF2 + i] = 0xC0 | (i as u8);
    }
    inner.low[0] = 0xAA;

    let mut bus = CountingFetchBus::new(inner);

    let buf = fetch_wrapped(&state, &mut bus, 0x0FFFF2, 15).unwrap();

    let mut expected = [0u8; 15];
    for i in 0..14 {
        expected[i] = 0xC0 | (i as u8);
    }
    expected[14] = 0xAA;

    assert_eq!(&buf[..15], &expected);
    assert_eq!(bus.fetch_calls, 2);
}

#[test]
fn write_bytes_wrapped_large_a20_wrap_does_not_allocate() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    let mut bus = FlatTestBus::new(0x100000);
    let src = vec![0xA5u8; 1 << 20];

    // Ensure we only count allocations made by `write_bytes_wrapped`, not by setup.
    {
        let _guard = AllocationCountGuard::begin();
        write_bytes_wrapped(&state, &mut bus, 0x000F_F000, &src).unwrap();
        assert_eq!(alloc_calls(), 0, "write_bytes_wrapped allocated memory");
    }

    // Spot-check that the write wrapped across the 1MiB alias boundary.
    assert_eq!(bus.read_u8(0x0FF000).unwrap(), 0xA5);
    assert_eq!(bus.read_u8(0x0FFFFF).unwrap(), 0xA5);
    assert_eq!(bus.read_u8(0x000000).unwrap(), 0xA5);
    assert_eq!(bus.read_u8(0x0FEFFF).unwrap(), 0xA5);
}

#[test]
fn write_bytes_wrapped_large_32bit_wrap_does_not_allocate() {
    let state = CpuState::new(CpuMode::Bit32);

    let mut bus = SplitTestBus::new(0x100000, 0xFFFF_F000, 0x1000);
    let src = vec![0x5Au8; 1 << 20];

    {
        let _guard = AllocationCountGuard::begin();
        write_bytes_wrapped(&state, &mut bus, 0xFFFF_F000, &src).unwrap();
        assert_eq!(alloc_calls(), 0, "write_bytes_wrapped allocated memory");
    }

    // First chunk: 0xFFFF_F000..=0xFFFF_FFFF
    assert_eq!(bus.read_u8(0xFFFF_F000).unwrap(), 0x5A);
    assert_eq!(bus.read_u8(0xFFFF_FFFF).unwrap(), 0x5A);

    // Second chunk starts at 0.
    assert_eq!(bus.read_u8(0x0000_0000).unwrap(), 0x5A);
    assert_eq!(bus.read_u8(0x000F_EFFF).unwrap(), 0x5A);
}

#[test]
fn write_bytes_wrapped_is_fault_atomic_across_segments() {
    let mut state = CpuState::new(CpuMode::Bit16);
    state.a20_enabled = false;

    // Fail preflight for any segment that starts at 0. A 2-byte write starting at 0xFFFFF with
    // A20 disabled will split into two segments: [0xFFFFF] and [0x0].
    let mut bus = FaultingPreflightBus::new(0x100000, 0);
    bus.write_u8(0xFFFFF, 0xAA).unwrap();
    bus.write_u8(0x0000, 0xBB).unwrap();

    let res = write_bytes_wrapped(&state, &mut bus, 0x000F_FFFF, &[0x11, 0x22]);
    assert!(res.is_err());

    // No bytes should have been committed because preflight failed for the second segment.
    assert_eq!(bus.read_u8(0x0FFFFF).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x0000_0000).unwrap(), 0xBB);
}
