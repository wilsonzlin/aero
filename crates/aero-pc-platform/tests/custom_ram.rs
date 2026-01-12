use aero_pc_platform::PcPlatform;
use memory::{DenseMemory, GuestMemory, GuestMemoryResult};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct TrackingRam {
    inner: Rc<RefCell<TrackingRamInner>>,
}

struct TrackingRamInner {
    mem: DenseMemory,
    reads: u64,
    writes: u64,
}

impl TrackingRam {
    fn new(size: u64) -> Self {
        Self {
            inner: Rc::new(RefCell::new(TrackingRamInner {
                mem: DenseMemory::new(size).unwrap(),
                reads: 0,
                writes: 0,
            })),
        }
    }

    fn counts(&self) -> (u64, u64) {
        let inner = self.inner.borrow();
        (inner.reads, inner.writes)
    }

    fn snapshot(&self, paddr: u64, len: usize) -> Vec<u8> {
        let inner = self.inner.borrow();
        let mut buf = vec![0u8; len];
        inner.mem.read_into(paddr, &mut buf).unwrap();
        buf
    }
}

impl GuestMemory for TrackingRam {
    fn size(&self) -> u64 {
        self.inner.borrow().mem.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        let mut inner = self.inner.borrow_mut();
        inner.reads += 1;
        inner.mem.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        let mut inner = self.inner.borrow_mut();
        inner.writes += 1;
        inner.mem.write_from(paddr, src)
    }
}

fn read_cmos_u8(platform: &mut PcPlatform, idx: u8) -> u8 {
    platform.io.write_u8(0x70, idx);
    platform.io.read_u8(0x71)
}

#[test]
fn pc_platform_can_use_custom_ram_backend() {
    let ram_size_bytes = 33 * 1024 * 1024u64;
    let ram = TrackingRam::new(ram_size_bytes);

    let mut platform =
        PcPlatform::new_with_config_and_ram(Box::new(ram.clone()), Default::default());

    assert_eq!(platform.memory.ram().size(), ram_size_bytes);

    let (reads0, writes0) = ram.counts();

    let addr = 0x1000u64;
    let bytes = [0xAAu8, 0xBB, 0xCC, 0xDD];
    platform.memory.write_physical(addr, &bytes);
    assert_eq!(ram.snapshot(addr, bytes.len()), bytes);
    let (reads1, writes1) = ram.counts();
    assert_eq!(reads1, reads0);
    assert!(writes1 > writes0, "expected RAM backend to observe writes");

    let mut dst = [0u8; 4];
    platform.memory.read_physical(addr, &mut dst);
    assert_eq!(dst, bytes);
    let (reads2, writes2) = ram.counts();
    assert!(reads2 > reads1, "expected RAM backend to observe reads");
    assert_eq!(writes2, writes1);

    // Verify RTC memory-size programming reflects the custom RAM size.
    let base_kib = u16::from_le_bytes([
        read_cmos_u8(&mut platform, 0x15),
        read_cmos_u8(&mut platform, 0x16),
    ]);
    assert_eq!(base_kib, 640);

    let ext_kib = u16::from_le_bytes([
        read_cmos_u8(&mut platform, 0x17),
        read_cmos_u8(&mut platform, 0x18),
    ]);
    let ext2_kib = u16::from_le_bytes([
        read_cmos_u8(&mut platform, 0x30),
        read_cmos_u8(&mut platform, 0x31),
    ]);
    let expected_ext_kib =
        ((ram_size_bytes.saturating_sub(1024 * 1024)) / 1024).min(u64::from(u16::MAX)) as u16;
    assert_eq!(ext_kib, expected_ext_kib);
    assert_eq!(ext2_kib, expected_ext_kib);

    let high_blocks = u16::from_le_bytes([
        read_cmos_u8(&mut platform, 0x34),
        read_cmos_u8(&mut platform, 0x35),
    ]);
    let expected_high_blocks = ((ram_size_bytes.saturating_sub(16 * 1024 * 1024)) / (64 * 1024))
        .min(u64::from(u16::MAX)) as u16;
    assert_eq!(high_blocks, expected_high_blocks);
}
