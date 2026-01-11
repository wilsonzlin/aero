use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::Exception;

#[derive(Debug, Clone)]
struct ScalarOnlyBus {
    inner: FlatTestBus,
}

impl ScalarOnlyBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
    }

    fn slice(&self, addr: u64, len: usize) -> &[u8] {
        self.inner.slice(addr, len)
    }
}

impl CpuBus for ScalarOnlyBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, aero_cpu_core::Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, aero_cpu_core::Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, aero_cpu_core::Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(
        &mut self,
        port: u16,
        size: u32,
        val: u64,
    ) -> Result<(), aero_cpu_core::Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn atomic_rmw_updates_value_and_returns_result() {
    let mut bus = FlatTestBus::new(0x1000);

    // u8
    {
        let addr = 0x10;
        bus.write_u8(addr, 0x41).unwrap();
        let ret = bus
            .atomic_rmw::<u8, _>(addr, |old| (old.wrapping_add(1), (old, old.wrapping_add(1))))
            .unwrap();
        assert_eq!(ret, (0x41, 0x42));
        assert_eq!(bus.read_u8(addr).unwrap(), 0x42);
    }

    // u16
    {
        let addr = 0x20;
        bus.write_u16(addr, 0x1234).unwrap();
        let old = bus
            .atomic_rmw::<u16, _>(addr, |old| (old.wrapping_add(1), old))
            .unwrap();
        assert_eq!(old, 0x1234);
        assert_eq!(bus.read_u16(addr).unwrap(), 0x1235);
    }

    // u32
    {
        let addr = 0x30;
        bus.write_u32(addr, 0x1122_3344).unwrap();
        let old = bus
            .atomic_rmw::<u32, _>(addr, |old| (old ^ 0xFFFF_FFFF, old))
            .unwrap();
        assert_eq!(old, 0x1122_3344);
        assert_eq!(bus.read_u32(addr).unwrap(), 0xEEDD_CCBB);
    }

    // u64
    {
        let addr = 0x40;
        bus.write_u64(addr, 0x1111_2222_3333_4444).unwrap();
        let old = bus
            .atomic_rmw::<u64, _>(addr, |old| (old.wrapping_add(0x10), old))
            .unwrap();
        assert_eq!(old, 0x1111_2222_3333_4444);
        assert_eq!(bus.read_u64(addr).unwrap(), 0x1111_2222_3333_4454);
    }

    // u128
    {
        let addr = 0x80;
        let init = 0x0011_2233_4455_6677_8899_AABB_CCDD_EEFFu128;
        bus.write_u128(addr, init).unwrap();
        let old = bus
            .atomic_rmw::<u128, _>(addr, |old| (old + 1, old))
            .unwrap();
        assert_eq!(old, init);
        assert_eq!(bus.read_u128(addr).unwrap(), init + 1);
    }
}

#[test]
fn bulk_copy_memmove_overlap_semantics() {
    let mut bus = FlatTestBus::new(64);
    let data: Vec<u8> = (0u8..16).collect();
    bus.load(0, &data);

    // Overlapping copy where `dst > src` would be corrupted by a naive forward copy.
    assert!(bus.bulk_copy(2, 0, 8).unwrap());

    let expected = [0u8, 1, 0, 1, 2, 3, 4, 5, 6, 7];
    assert_eq!(bus.slice(0, expected.len()), expected);
}

#[test]
fn bulk_set_repeats_pattern_for_common_sizes() {
    let mut bus = FlatTestBus::new(0x200);

    for (dst, pattern, repeat) in [
        (0x00, vec![0xAA], 17usize),
        (0x40, vec![0x11, 0x22], 9usize),
        (0x80, vec![0, 1, 2, 3], 7usize),
        (0xC0, (0u8..8).collect::<Vec<_>>(), 5usize),
        (0x100, (0u8..16).collect::<Vec<_>>(), 3usize),
    ] {
        assert!(bus.bulk_set(dst, &pattern, repeat).unwrap());
        let expected: Vec<u8> = pattern
            .iter()
            .copied()
            .cycle()
            .take(pattern.len() * repeat)
            .collect();
        assert_eq!(bus.slice(dst, expected.len()), expected.as_slice());
    }
}

#[test]
fn bulk_ops_fallback_work_when_unsupported() {
    let mut bus = ScalarOnlyBus::new(0x100);
    assert!(!bus.supports_bulk_copy());
    assert!(!bus.supports_bulk_set());

    // `atomic_rmw` should also work without requiring bus-specific overrides.
    bus.write_u32(0x20, 0x1234_5678).unwrap();
    let old = bus
        .atomic_rmw::<u32, _>(0x20, |old| (old.wrapping_add(1), old))
        .unwrap();
    assert_eq!(old, 0x1234_5678);
    assert_eq!(bus.read_u32(0x20).unwrap(), 0x1234_5679);

    let data: Vec<u8> = (0u8..16).collect();
    bus.load(0, &data);

    // Ensure the default `bulk_copy` fallback still provides memmove semantics.
    assert!(bus.bulk_copy(2, 0, 8).unwrap());
    let expected = [0u8, 1, 0, 1, 2, 3, 4, 5, 6, 7];
    assert_eq!(bus.slice(0, expected.len()), expected);

    // And `bulk_set` falls back to scalar writes correctly.
    let pattern = [0xDEu8, 0xAD, 0xBE, 0xEF];
    assert!(bus.bulk_set(0x40, &pattern, 4).unwrap());
    let expected: Vec<u8> = pattern.iter().copied().cycle().take(16).collect();
    assert_eq!(bus.slice(0x40, expected.len()), expected.as_slice());
}

#[test]
fn read_write_bytes_roundtrip_flat_test_bus() -> Result<(), Exception> {
    let mut bus = FlatTestBus::new(0x40);
    let data: Vec<u8> = (0u8..16).collect();
    bus.write_bytes(0x10, &data)?;

    let mut out = vec![0u8; data.len()];
    bus.read_bytes(0x10, &mut out)?;
    assert_eq!(out, data);
    Ok(())
}

#[test]
fn read_write_bytes_oob_is_memory_fault() {
    let mut bus = FlatTestBus::new(16);
    let mut out = [0u8; 4];
    assert_eq!(bus.read_bytes(13, &mut out), Err(Exception::MemoryFault));
    assert_eq!(bus.write_bytes(13, &[1, 2, 3, 4]), Err(Exception::MemoryFault));
}

#[test]
fn read_write_bytes_fallback_work_when_not_overridden() -> Result<(), Exception> {
    let mut bus = ScalarOnlyBus::new(0x40);
    let data = [0xDE, 0xAD, 0xBE, 0xEF];
    bus.write_bytes(0x20, &data)?;
    assert_eq!(bus.slice(0x20, data.len()), &data);

    let mut out = [0u8; 4];
    bus.read_bytes(0x20, &mut out)?;
    assert_eq!(out, data);
    Ok(())
}
