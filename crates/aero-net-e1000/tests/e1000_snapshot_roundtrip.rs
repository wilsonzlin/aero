#![cfg(feature = "io-snapshot")]

use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_net_e1000::{E1000Device, ICR_RXT0, ICR_TXDW, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

const REG_ICR: u32 = 0x00C0;
const REG_ICS: u32 = 0x00C8;
const REG_IMS: u32 = 0x00D0;

const REG_RCTL: u32 = 0x0100;
const REG_RDBAL: u32 = 0x2800;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;

const REG_TCTL: u32 = 0x0400;
const REG_TDBAL: u32 = 0x3800;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH: u32 = 0x3810;
const REG_TDT: u32 = 0x3818;

const RCTL_EN: u32 = 1 << 1;
const TCTL_EN: u32 = 1 << 1;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_RS: u8 = 1 << 3;

const TAG_RX_PENDING: u16 = 91;

#[derive(Clone)]
struct TestMem {
    mem: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let addr = addr as usize;
        self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
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

#[derive(Clone, Copy, Debug)]
struct TxDesc {
    buffer_addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

impl TxDesc {
    const LEN: usize = 16;

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.buffer_addr.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.length.to_le_bytes());
        bytes[10] = self.cso;
        bytes[11] = self.cmd;
        bytes[12] = self.status;
        bytes[13] = self.css;
        bytes[14..16].copy_from_slice(&self.special.to_le_bytes());
        bytes
    }
}

#[derive(Clone, Copy, Debug)]
struct RxDesc {
    buffer_addr: u64,
}

impl RxDesc {
    const LEN: usize = 16;

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.buffer_addr.to_le_bytes());
        bytes
    }
}

#[test]
fn snapshot_roundtrip_preserves_state() {
    const REG_OTHER: u32 = 0x1234;

    let mut mem = TestMem::new(0x40_000);
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut dev = E1000Device::new(mac);
    // Real hardware requires PCI Bus Master Enable before the NIC may DMA descriptors/buffers.
    dev.pci_config_write(0x04, 2, 0x4);

    // Touch a non-modeled register so it lands in `other_regs`.
    dev.mmio_write_u32_reg(REG_OTHER, 0xDEAD_BEEF);

    // Enable interrupt masks and set a pending cause.
    dev.mmio_write_u32_reg(REG_IMS, ICR_RXT0 | ICR_TXDW);
    dev.mmio_write_u32_reg(REG_ICS, ICR_RXT0);

    // Enable RX/TX, but do not configure RX ring yet so frames remain in rx_pending.
    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);
    dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

    let rx1 = vec![0xAA; MIN_L2_FRAME_LEN];
    let rx2 = vec![0xBB; MIN_L2_FRAME_LEN];
    dev.enqueue_rx_frame(rx1.clone());
    dev.enqueue_rx_frame(rx2.clone());

    // Configure TX ring with 4 descriptors at 0x1000.
    dev.mmio_write_u32_reg(REG_TDBAL, 0x1000);
    dev.mmio_write_u32_reg(REG_TDLEN, (TxDesc::LEN as u32) * 4);
    dev.mmio_write_u32_reg(REG_TDH, 0);
    dev.mmio_write_u32_reg(REG_TDT, 0);

    // First packet is a complete frame in descriptor 0 (should end up in tx_out before snapshot).
    let tx1 = vec![0x11; MIN_L2_FRAME_LEN];
    mem.write_bytes(0x2000, &tx1);
    let desc0 = TxDesc {
        buffer_addr: 0x2000,
        length: tx1.len() as u16,
        cso: 0,
        cmd: TXD_CMD_EOP | TXD_CMD_RS,
        status: 0,
        css: 0,
        special: 0,
    };

    // Second packet is split across descriptor 1 (no EOP) and descriptor 2 (EOP).
    let tx2_part1 = vec![0x22; 8];
    let tx2_part2 = vec![0x33; MIN_L2_FRAME_LEN - tx2_part1.len()];
    let tx2_expected: Vec<u8> = tx2_part1
        .iter()
        .copied()
        .chain(tx2_part2.iter().copied())
        .collect();
    mem.write_bytes(0x2100, &tx2_part1);
    mem.write_bytes(0x2200, &tx2_part2);
    let desc1 = TxDesc {
        buffer_addr: 0x2100,
        length: tx2_part1.len() as u16,
        cso: 0,
        cmd: TXD_CMD_RS, // no EOP
        status: 0,
        css: 0,
        special: 0,
    };
    let desc2 = TxDesc {
        buffer_addr: 0x2200,
        length: tx2_part2.len() as u16,
        cso: 0,
        cmd: TXD_CMD_EOP | TXD_CMD_RS,
        status: 0,
        css: 0,
        special: 0,
    };

    mem.write_bytes(0x1000, &desc0.to_bytes());
    mem.write_bytes(0x1010, &desc1.to_bytes());
    mem.write_bytes(0x1020, &desc2.to_bytes());

    // Process descriptors 0 and 1, leaving descriptor 2 pending so we snapshot in-progress TX state.
    dev.mmio_write_u32_reg(REG_TDT, 2);
    dev.poll(&mut mem);

    let snapshot = dev.save_state();

    let mut restored = E1000Device::new([0, 1, 2, 3, 4, 5]);
    restored.load_state(&snapshot).unwrap();

    // Basic roundtrip determinism check: saving the restored state should produce identical bytes.
    assert_eq!(restored.save_state(), snapshot);

    assert_eq!(restored.mac_addr(), mac);
    assert_eq!(restored.mmio_read_u32(REG_IMS), ICR_RXT0 | ICR_TXDW);
    assert_eq!(restored.mmio_read_u32(REG_OTHER), 0xDEAD_BEEF);

    // Interrupt state should be preserved.
    assert!(restored.irq_level());
    let icr = restored.mmio_read_u32(REG_ICR);
    assert_eq!(icr & (ICR_RXT0 | ICR_TXDW), ICR_RXT0 | ICR_TXDW);
    assert!(!restored.irq_level());

    // TX output queue should be restored.
    assert_eq!(restored.pop_tx_frame(), Some(tx1));

    // Finish the in-progress packet by advancing the tail to include descriptor 2.
    restored.mmio_write_u32_reg(REG_TDT, 3);
    restored.poll(&mut mem);
    assert_eq!(restored.pop_tx_frame(), Some(tx2_expected));

    // Configure an RX ring and ensure the pending frames flush into guest memory.
    let rx_desc0 = RxDesc {
        buffer_addr: 0x4000,
    };
    let rx_desc1 = RxDesc {
        buffer_addr: 0x5000,
    };
    let rx_desc2 = RxDesc {
        buffer_addr: 0x6000,
    };
    let rx_desc3 = RxDesc {
        buffer_addr: 0x7000,
    };
    mem.write_bytes(0x3000, &rx_desc0.to_bytes());
    mem.write_bytes(0x3010, &rx_desc1.to_bytes());
    mem.write_bytes(0x3020, &rx_desc2.to_bytes());
    mem.write_bytes(0x3030, &rx_desc3.to_bytes());

    restored.mmio_write_u32_reg(REG_RDBAL, 0x3000);
    restored.mmio_write_u32_reg(REG_RDLEN, (RxDesc::LEN as u32) * 4);
    restored.mmio_write_u32_reg(REG_RDH, 0);
    restored.mmio_write_u32_reg(REG_RDT, 3);
    restored.poll(&mut mem);

    assert_eq!(mem.read_bytes(0x4000, rx1.len()), rx1);
    assert_eq!(mem.read_bytes(0x5000, rx2.len()), rx2);
}

#[test]
fn snapshot_rejects_oversized_rx_frame() {
    let oversized = vec![0u8; MAX_L2_FRAME_LEN + 1];
    let bytes = Encoder::new()
        .u32(1)
        .u32(oversized.len() as u32)
        .bytes(&oversized)
        .finish();

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(TAG_RX_PENDING, bytes);
    let snapshot = w.finish();

    let mut dev = E1000Device::new([0, 0, 0, 0, 0, 0]);
    let err = dev.load_state(&snapshot).unwrap_err();
    assert!(matches!(err, SnapshotError::InvalidFieldEncoding(_)));
}
