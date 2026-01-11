use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::Exception;

#[test]
fn flat_test_bus_bulk_copy_memmove_semantics() -> Result<(), Exception> {
    let data: Vec<u8> = (0..64u8).collect();

    // Non-overlapping copy.
    let mut bus = FlatTestBus::new(64);
    bus.load(0, &data);
    assert!(bus.supports_bulk_copy());
    assert!(bus.bulk_copy(32, 0, 16)?);
    assert_eq!(bus.slice(32, 16), &data[0..16]);

    // Overlapping copy (dst inside src, dst > src).
    let mut bus = FlatTestBus::new(64);
    bus.load(0, &data);
    let mut expected = data.clone();
    expected.copy_within(0..32, 8);
    assert!(bus.bulk_copy(8, 0, 32)?);
    assert_eq!(bus.slice(0, 64), expected.as_slice());

    // Overlapping copy (src inside dst, dst < src).
    let mut bus = FlatTestBus::new(64);
    bus.load(0, &data);
    let mut expected = data.clone();
    expected.copy_within(8..40, 0);
    assert!(bus.bulk_copy(0, 8, 32)?);
    assert_eq!(bus.slice(0, 64), expected.as_slice());

    Ok(())
}

#[test]
fn flat_test_bus_bulk_set_patterns() -> Result<(), Exception> {
    let mut bus = FlatTestBus::new(128);
    let init = vec![0xCCu8; 128];
    bus.load(0, &init);

    assert!(bus.supports_bulk_set());

    // repeat == 0 is a no-op.
    let before = bus.slice(0, 128).to_vec();
    assert!(bus.bulk_set(10, &[0xAA], 0)?);
    assert_eq!(bus.slice(0, 128), before.as_slice());

    // Pattern sizes 1/2/4/8 with repeat counts 1 and N.
    assert!(bus.bulk_set(0, &[0x11], 4)?);
    assert_eq!(bus.slice(0, 4), &[0x11, 0x11, 0x11, 0x11]);

    let pat2 = [0xDE, 0xAD];
    let exp2 = pat2.repeat(3);
    assert!(bus.bulk_set(16, &pat2, 3)?);
    assert_eq!(bus.slice(16, exp2.len()), exp2.as_slice());

    let pat4 = [0x01, 0x23, 0x45, 0x67];
    let exp4 = pat4.repeat(2);
    assert!(bus.bulk_set(32, &pat4, 2)?);
    assert_eq!(bus.slice(32, exp4.len()), exp4.as_slice());

    let pat8 = [0, 1, 2, 3, 4, 5, 6, 7];
    let exp8 = pat8.repeat(3);
    assert!(bus.bulk_set(48, &pat8, 3)?);
    assert_eq!(bus.slice(48, exp8.len()), exp8.as_slice());

    // repeat == 1 should write exactly one pattern.
    let pat1 = [0xFE, 0xED, 0xFA, 0xCE];
    assert!(bus.bulk_set(100, &pat1, 1)?);
    assert_eq!(bus.slice(100, pat1.len()), &pat1[..]);

    Ok(())
}

#[derive(Clone, Debug)]
struct CountingBus {
    inner: FlatTestBus,
    writes: usize,
}

impl CountingBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            writes: 0,
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        self.inner.load(addr, data);
    }

    fn slice(&self, addr: u64, len: usize) -> &[u8] {
        self.inner.slice(addr, len)
    }
}

impl CpuBus for CountingBus {
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
        self.writes += 1;
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.writes += 1;
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.writes += 1;
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.writes += 1;
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.writes += 1;
        self.inner.write_u128(vaddr, val)
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
fn atomic_rmw_default_is_read_then_conditional_write() -> Result<(), Exception> {
    let mut bus = CountingBus::new(16);
    bus.write_u32(0, 0x1234_5678)?;
    bus.writes = 0;

    // If the closure returns the same value, no write should be performed.
    let old = bus.atomic_rmw::<u32, u32>(0, |old| (old, old))?;
    assert_eq!(old, 0x1234_5678);
    assert_eq!(bus.writes, 0);
    assert_eq!(bus.read_u32(0)?, 0x1234_5678);

    // If the closure changes the value, it should be written back.
    let old = bus.atomic_rmw::<u32, u32>(0, |old| (old.wrapping_add(1), old))?;
    assert_eq!(old, 0x1234_5678);
    assert_eq!(bus.writes, 1);
    assert_eq!(bus.read_u32(0)?, 0x1234_5679);

    Ok(())
}

#[test]
fn bulk_ops_default_fallback_works_without_support_flags() -> Result<(), Exception> {
    let data: Vec<u8> = (0..64u8).collect();
    let mut bus = CountingBus::new(64);
    bus.load(0, &data);

    assert!(!bus.supports_bulk_copy());
    assert!(!bus.supports_bulk_set());

    let mut expected = data.clone();
    expected.copy_within(0..32, 8);
    assert!(bus.bulk_copy(8, 0, 32)?);
    assert_eq!(bus.slice(0, 64), expected.as_slice());

    let pat = [0xAA, 0xBB, 0xCC, 0xDD];
    let exp = pat.repeat(3);
    let mut expected = expected.clone();
    expected[40..40 + exp.len()].copy_from_slice(&exp);
    assert!(bus.bulk_set(40, &pat, 3)?);
    assert_eq!(bus.slice(0, 64), expected.as_slice());

    Ok(())
}
