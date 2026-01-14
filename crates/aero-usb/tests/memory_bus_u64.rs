use aero_usb::MemoryBus;

#[derive(Default)]
struct VecMemoryBus {
    mem: Vec<u8>,
}

impl VecMemoryBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }
}

impl MemoryBus for VecMemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        buf.copy_from_slice(&self.mem[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        self.mem[start..end].copy_from_slice(buf);
    }
}

#[test]
fn memory_bus_u64_is_little_endian() {
    let mut bus = VecMemoryBus::new(64);

    bus.write_u64(8, 0x1122_3344_5566_7788);

    assert_eq!(
        &bus.mem[8..16],
        &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
    );
    assert_eq!(bus.read_u64(8), 0x1122_3344_5566_7788);
}
