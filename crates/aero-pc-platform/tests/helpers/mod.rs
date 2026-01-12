#![allow(dead_code)]

use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::PcPlatform;
use aero_platform::io::{IoPortBus, PortIoDevice};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    Io,
    Mem32,
    Mem64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarInfo {
    pub kind: BarKind,
    pub base: u64,
}

pub fn cfg_addr(bus: u8, device: u8, function: u8, offset: u16) -> u32 {
    assert!(device < 32);
    assert!(function < 8);
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

pub fn cfg_addr_bdf(bdf: PciBdf, offset: u16) -> u32 {
    cfg_addr(bdf.bus, bdf.device, bdf.function, offset)
}

pub fn pci_cfg_read(pc: &mut PcPlatform, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr_bdf(bdf, offset));
    pc.io.read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

pub fn pci_cfg_write(pc: &mut PcPlatform, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr_bdf(bdf, offset));
    pc.io.write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

pub fn pci_cfg_read_u8(pc: &mut PcPlatform, bdf: PciBdf, offset: u16) -> u8 {
    pci_cfg_read(pc, bdf, offset, 1) as u8
}

pub fn pci_cfg_read_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u16) -> u16 {
    pci_cfg_read(pc, bdf, offset, 2) as u16
}

pub fn pci_cfg_read_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u16) -> u32 {
    pci_cfg_read(pc, bdf, offset, 4)
}

pub fn pci_cfg_write_u8(pc: &mut PcPlatform, bdf: PciBdf, offset: u16, value: u8) {
    pci_cfg_write(pc, bdf, offset, 1, u32::from(value));
}

pub fn pci_cfg_write_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u16, value: u16) {
    pci_cfg_write(pc, bdf, offset, 2, u32::from(value));
}

pub fn pci_cfg_write_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u16, value: u32) {
    pci_cfg_write(pc, bdf, offset, 4, value);
}

pub fn pci_read_bar(pc: &mut PcPlatform, bdf: PciBdf, index: u8) -> BarInfo {
    assert!(index < 6);
    let off = 0x10u16 + u16::from(index) * 4;
    let lo = pci_cfg_read_u32(pc, bdf, off);
    if (lo & 0x1) != 0 {
        // I/O BAR.
        return BarInfo {
            kind: BarKind::Io,
            base: u64::from(lo & 0xFFFF_FFFC),
        };
    }

    let typ = (lo >> 1) & 0x3;
    match typ {
        0x0 => BarInfo {
            kind: BarKind::Mem32,
            base: u64::from(lo & 0xFFFF_FFF0),
        },
        0x2 => {
            let hi = pci_cfg_read_u32(pc, bdf, off + 4);
            let base = (u64::from(hi) << 32) | u64::from(lo & 0xFFFF_FFF0);
            BarInfo {
                kind: BarKind::Mem64,
                base,
            }
        }
        _ => BarInfo {
            kind: BarKind::Mem32,
            base: u64::from(lo & 0xFFFF_FFF0),
        },
    }
}

pub fn pci_enable_bus_mastering(pc: &mut PcPlatform, bdf: PciBdf) {
    let mut cmd = pci_cfg_read_u16(pc, bdf, 0x04);
    cmd |= 1 << 2;
    pci_cfg_write_u16(pc, bdf, 0x04, cmd);
}

pub fn pci_enable_mmio(pc: &mut PcPlatform, bdf: PciBdf) {
    let mut cmd = pci_cfg_read_u16(pc, bdf, 0x04);
    cmd |= 1 << 1;
    pci_cfg_write_u16(pc, bdf, 0x04, cmd);
}

pub fn pci_enable_io(pc: &mut PcPlatform, bdf: PciBdf) {
    let mut cmd = pci_cfg_read_u16(pc, bdf, 0x04);
    cmd |= 1 << 0;
    pci_cfg_write_u16(pc, bdf, 0x04, cmd);
}

#[derive(Debug, Clone)]
pub struct GuestAllocator {
    next: u64,
    end: u64,
}

impl GuestAllocator {
    pub fn new(ram_size: u64, start: u64) -> Self {
        assert!(start <= ram_size);
        Self {
            next: start,
            end: ram_size,
        }
    }

    pub fn alloc(&mut self, size: u64, align: u64) -> u64 {
        assert!(align.is_power_of_two());
        let base = (self.next + (align - 1)) & !(align - 1);
        let end = base.checked_add(size).expect("guest alloc overflow");
        assert!(end <= self.end, "out of guest memory for allocator");
        self.next = end;
        base
    }

    pub fn alloc_bytes(&mut self, size: usize, align: usize) -> u64 {
        self.alloc(size as u64, align as u64)
    }
}

pub fn mem_write(pc: &mut PcPlatform, paddr: u64, data: &[u8]) {
    pc.memory.write_physical(paddr, data);
}

pub fn mem_read(pc: &mut PcPlatform, paddr: u64, data: &mut [u8]) {
    pc.memory.read_physical(paddr, data);
}

pub fn unmask_pic_irq(pc: &mut PcPlatform, irq: u8) {
    let mut interrupts = pc.interrupts.borrow_mut();
    interrupts.pic_mut().set_offsets(0x20, 0x28);
    if irq >= 8 {
        interrupts.pic_mut().set_masked(2, false);
    }
    interrupts.pic_mut().set_masked(irq, false);
}

pub fn pic_pending_vector(pc: &PcPlatform) -> Option<u8> {
    pc.interrupts.borrow().pic().get_pending_vector()
}

pub fn pic_pending_irq(pc: &PcPlatform) -> Option<u8> {
    let interrupts = pc.interrupts.borrow();
    let vec = interrupts.pic().get_pending_vector()?;
    interrupts.pic().vector_to_irq(vec)
}

pub fn pic_acknowledge_and_eoi(pc: &mut PcPlatform, vector: u8) {
    let mut interrupts = pc.interrupts.borrow_mut();
    interrupts.pic_mut().acknowledge(vector);
    interrupts.pic_mut().eoi(vector);
}

/// Register a port-I/O device for a contiguous range of ports.
///
/// `make_dev` is called for every port and must produce a fresh boxed device.
pub fn register_port_range<D, F>(bus: &mut IoPortBus, start: u16, len: u16, mut make_dev: F)
where
    D: PortIoDevice + 'static,
    F: FnMut(u16) -> D,
{
    for port in start..start.saturating_add(len) {
        bus.register(port, Box::new(make_dev(port)));
    }
}
