use aero_net_e1000::{E1000Device, MAX_TX_DESCS_PER_POLL};
use memory::MemoryBus;

struct TestMem {
    mem: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { mem: vec![0u8; size] }
    }

    fn read_bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let addr = addr as usize;
        self.mem[addr..addr + len].to_vec()
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.mem[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

#[test]
fn tx_poll_is_bounded_by_max_tx_descs_per_poll() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Enable PCI bus mastering so DMA is permitted.
    dev.pci_config_write(0x04, 2, 1 << 2);

    // Program a TX descriptor ring with more outstanding descriptors than `MAX_TX_DESCS_PER_POLL`,
    // and ensure a single `poll()` call does not process beyond the bound.
    let base: u64 = 0x1000;
    let desc_count: u32 = MAX_TX_DESCS_PER_POLL + 2;
    let tdlen: u32 = desc_count * 16;
    let mut mem = TestMem::new(base as usize + tdlen as usize + 0x100);

    // BAR0 register offsets (subset).
    const REG_TDBAL: u64 = 0x3800;
    const REG_TDBAH: u64 = 0x3804;
    const REG_TDLEN: u64 = 0x3808;
    const REG_TDH: u64 = 0x3810;
    const REG_TDT: u64 = 0x3818;
    const REG_TCTL: u64 = 0x0400;

    dev.mmio_write_reg(REG_TDBAL, 4, base as u32);
    dev.mmio_write_reg(REG_TDBAH, 4, 0);
    dev.mmio_write_reg(REG_TDLEN, 4, tdlen);
    dev.mmio_write_reg(REG_TDH, 4, 0);
    // Leave exactly one descriptor "unprocessed" beyond the poll budget.
    dev.mmio_write_reg(REG_TDT, 4, desc_count - 1);
    dev.mmio_write_reg(REG_TCTL, 4, 1 << 1); // TCTL.EN

    // Populate the ring with simple legacy TX descriptors that have no data payload (length=0).
    // This avoids large guest memory reads while still exercising the descriptor walk.
    let mut desc = [0u8; 16];
    desc[11] = 0x01 | 0x08; // EOP | RS

    for i in 0..(desc_count - 1) {
        let addr = base + u64::from(i) * 16;
        mem.write_physical(addr, &desc);
    }

    dev.poll(&mut mem);

    // TDH should have advanced by at most `MAX_TX_DESCS_PER_POLL`.
    assert_eq!(dev.mmio_read_u32(REG_TDH as u32), MAX_TX_DESCS_PER_POLL);

    // Descriptor 0 should be marked done (DD set), while the first descriptor past the poll budget
    // should still be pending.
    let first = mem.read_bytes(base, 16);
    assert_eq!(first[12] & 0x01, 0x01);
    let pending_addr = base + u64::from(MAX_TX_DESCS_PER_POLL) * 16;
    let pending = mem.read_bytes(pending_addr, 16);
    assert_eq!(pending[12] & 0x01, 0, "descriptor beyond poll budget should remain pending");
}

