use std::cell::RefCell;
use std::rc::Rc;

use aero_net_e1000::{E1000Device, E1000_MMIO_SIZE, ICR_RXT0, ICR_TXDW};
use memory::{DenseMemory, MemoryBus, MmioHandler, PhysicalMemoryBus};

const REG_ICR: u64 = 0x00C0;
const REG_IMS: u64 = 0x00D0;

const REG_RCTL: u64 = 0x0100;
const REG_RDBAL: u64 = 0x2800;
const REG_RDBAH: u64 = 0x2804;
const REG_RDLEN: u64 = 0x2808;
const REG_RDH: u64 = 0x2810;
const REG_RDT: u64 = 0x2818;

const REG_TCTL: u64 = 0x0400;
const REG_TDBAL: u64 = 0x3800;
const REG_TDBAH: u64 = 0x3804;
const REG_TDLEN: u64 = 0x3808;
const REG_TDH: u64 = 0x3810;
const REG_TDT: u64 = 0x3818;

const RCTL_EN: u32 = 1 << 1;
const TCTL_EN: u32 = 1 << 1;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_RS: u8 = 1 << 3;

fn write_u64_le(mem: &mut dyn MemoryBus, addr: u64, v: u64) {
    mem.write_physical(addr, &v.to_le_bytes());
}

fn write_tx_desc(mem: &mut dyn MemoryBus, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    mem.write_physical(addr + 8, &len.to_le_bytes());
    mem.write_physical(addr + 10, &[0u8]); // cso
    mem.write_physical(addr + 11, &[cmd]);
    mem.write_physical(addr + 12, &[status]);
    mem.write_physical(addr + 13, &[0u8]); // css
    mem.write_physical(addr + 14, &0u16.to_le_bytes()); // special
}

fn write_rx_desc(mem: &mut dyn MemoryBus, addr: u64, buf_addr: u64, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    mem.write_physical(addr + 8, &0u16.to_le_bytes()); // length
    mem.write_physical(addr + 10, &0u16.to_le_bytes()); // checksum
    mem.write_physical(addr + 12, &[status]);
    mem.write_physical(addr + 13, &[0u8]); // errors
    mem.write_physical(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

struct SharedE1000Mmio {
    dev: Rc<RefCell<E1000Device>>,
}

impl MmioHandler for SharedE1000Mmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut dev = self.dev.borrow_mut();
        MmioHandler::read(&mut *dev, offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut dev = self.dev.borrow_mut();
        MmioHandler::write(&mut *dev, offset, size, value)
    }
}

#[test]
fn physical_memory_bus_mmio_defers_dma_until_poll() {
    let ram = DenseMemory::new(0x40_000).unwrap();
    let mut bus = PhysicalMemoryBus::new(Box::new(ram));

    let mmio_base = 0x100_000u64;
    let dev = Rc::new(RefCell::new(E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])));
    dev.borrow_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    bus.map_mmio(
        mmio_base,
        E1000_MMIO_SIZE as u64,
        Box::new(SharedE1000Mmio { dev: dev.clone() }),
    )
    .unwrap();

    // Enable interrupts for both RX and TX.
    bus.write_physical_u32(mmio_base + REG_IMS, ICR_RXT0 | ICR_TXDW);

    // Configure TX ring: 4 descriptors at 0x1000.
    bus.write_physical_u32(mmio_base + REG_TDBAL, 0x1000);
    bus.write_physical_u32(mmio_base + REG_TDBAH, 0);
    bus.write_physical_u32(mmio_base + REG_TDLEN, 4 * 16);
    bus.write_physical_u32(mmio_base + REG_TDH, 0);
    bus.write_physical_u32(mmio_base + REG_TDT, 0);
    bus.write_physical_u32(mmio_base + REG_TCTL, TCTL_EN);

    // Configure RX ring: 2 descriptors at 0x2000.
    bus.write_physical_u32(mmio_base + REG_RDBAL, 0x2000);
    bus.write_physical_u32(mmio_base + REG_RDBAH, 0);
    bus.write_physical_u32(mmio_base + REG_RDLEN, 2 * 16);
    bus.write_physical_u32(mmio_base + REG_RDH, 0);
    bus.write_physical_u32(mmio_base + REG_RDT, 1);
    bus.write_physical_u32(mmio_base + REG_RCTL, RCTL_EN);

    // Populate RX descriptors with guest buffers.
    write_rx_desc(&mut bus, 0x2000, 0x3000, 0);
    write_rx_desc(&mut bus, 0x2010, 0x3400, 0);

    // Guest TX: descriptor 0 points at packet buffer 0x4000.
    let pkt_out = build_test_frame(b"guest->host");
    bus.write_physical(0x4000, &pkt_out);
    write_tx_desc(
        &mut bus,
        0x1000,
        0x4000,
        pkt_out.len() as u16,
        TXD_CMD_EOP | TXD_CMD_RS,
        0,
    );

    // Tail update via MMIO happens through the bus -> MmioHandler path, which does not provide a
    // memory reference. The device must not perform DMA until explicitly polled.
    bus.write_physical_u32(mmio_base + REG_TDT, 1);

    assert!(dev.borrow_mut().pop_tx_frame().is_none());
    let mut desc_bytes = [0u8; 16];
    bus.read_physical(0x1000, &mut desc_bytes);
    assert_eq!(desc_bytes[12] & 0x01, 0, "DD should not be set before poll()");

    dev.borrow_mut().poll(&mut bus);

    assert_eq!(dev.borrow_mut().pop_tx_frame().as_deref(), Some(pkt_out.as_slice()));
    bus.read_physical(0x1000, &mut desc_bytes);
    assert_ne!(desc_bytes[12] & 0x01, 0, "DD should be set after poll()");

    assert!(dev.borrow().irq_level());
    let causes = bus.read_physical_u32(mmio_base + REG_ICR);
    assert_eq!(causes & ICR_TXDW, ICR_TXDW);
    assert!(!dev.borrow().irq_level());

    // Host RX: queue without DMA, then poll to flush into guest memory.
    let pkt_in = build_test_frame(b"host->guest");
    dev.borrow_mut().enqueue_rx_frame(pkt_in.clone());

    let mut out = vec![0u8; pkt_in.len()];
    bus.read_physical(0x3000, &mut out);
    assert_ne!(out, pkt_in, "RX buffer should not be written before poll()");

    dev.borrow_mut().poll(&mut bus);

    bus.read_physical(0x3000, &mut out);
    assert_eq!(out, pkt_in);

    assert!(dev.borrow().irq_level());
    let causes = bus.read_physical_u32(mmio_base + REG_ICR);
    assert_eq!(causes & ICR_RXT0, ICR_RXT0);
    assert!(!dev.borrow().irq_level());
}
