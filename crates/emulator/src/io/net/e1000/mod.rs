use std::collections::{HashMap, VecDeque};

mod regs;
mod rx;
mod tx;

pub use regs::*;

/// Guest physical memory access for DMA operations.
///
/// This is intentionally small and can be implemented by the emulator's memory subsystem.
pub trait GuestMemory {
    fn read(&self, addr: u64, buf: &mut [u8]);
    fn write(&mut self, addr: u64, data: &[u8]);

    fn read_u8(&self, addr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read(addr, &mut buf);
        buf[0]
    }

    fn read_u16(&self, addr: u64) -> u16 {
        let mut buf = [0u8; 2];
        self.read(addr, &mut buf);
        u16::from_le_bytes(buf)
    }

    fn read_u32(&self, addr: u64) -> u32 {
        let mut buf = [0u8; 4];
        self.read(addr, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn read_u64(&self, addr: u64) -> u64 {
        let mut buf = [0u8; 8];
        self.read(addr, &mut buf);
        u64::from_le_bytes(buf)
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        self.write(addr, &[val]);
    }

    fn write_u16(&mut self, addr: u64, val: u16) {
        self.write(addr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, addr: u64, val: u32) {
        self.write(addr, &val.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, val: u64) {
        self.write(addr, &val.to_le_bytes());
    }
}

/// Network backend to bridge frames between the emulated NIC and the host network stack.
pub trait NetworkBackend {
    fn transmit(&mut self, frame: Vec<u8>);
}

#[derive(Clone, Debug)]
pub struct PciConfigSpace {
    regs: [u8; 256],
    bar0: u32,
    bar0_probe: bool,
    bar1: u32,
    bar1_probe: bool,
}

impl PciConfigSpace {
    pub const VENDOR_ID_INTEL: u16 = 0x8086;
    /// 82540EM (QEMU e1000) - Windows has in-box drivers.
    pub const DEVICE_ID_82540EM: u16 = 0x100e;

    pub const MMIO_BAR_SIZE: u32 = 0x20000; // 128 KiB
    pub const IO_BAR_SIZE: u32 = 0x40; // 64 bytes (IOADDR + IODATA + misc)

    pub fn new() -> Self {
        let mut regs = [0u8; 256];

        regs[0x00..0x02].copy_from_slice(&Self::VENDOR_ID_INTEL.to_le_bytes());
        regs[0x02..0x04].copy_from_slice(&Self::DEVICE_ID_82540EM.to_le_bytes());

        // Class code: Network controller / Ethernet controller.
        regs[0x0a] = 0x00; // subclass
        regs[0x0b] = 0x02; // class
        regs[0x0e] = 0x00; // header type

        // Subsystem vendor/device (keep Intel to match common virtual setups).
        regs[0x2c..0x2e].copy_from_slice(&Self::VENDOR_ID_INTEL.to_le_bytes());
        regs[0x2e..0x30].copy_from_slice(&Self::DEVICE_ID_82540EM.to_le_bytes());

        // INTA#
        regs[0x3d] = 0x01;

        Self {
            regs,
            bar0: 0,
            bar0_probe: false,
            bar1: 0x1, // I/O BAR indicator bit set.
            bar1_probe: false,
        }
    }

    fn read_u32_raw(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.regs[offset..offset + 4].try_into().unwrap())
    }

    fn write_u32_raw(&mut self, offset: usize, value: u32) {
        self.regs[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read(&self, offset: u16, size: u8) -> u32 {
        let offset = offset as usize;
        match size {
            1 => self.regs[offset] as u32,
            2 => u16::from_le_bytes(self.regs[offset..offset + 2].try_into().unwrap()) as u32,
            4 => {
                if offset == 0x10 {
                    return if self.bar0_probe {
                        // Size mask response: 128 KiB MMIO BAR.
                        !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0
                    } else {
                        self.bar0
                    };
                }
                if offset == 0x14 {
                    return if self.bar1_probe {
                        // I/O BAR: bit0 must remain set.
                        (!(Self::IO_BAR_SIZE - 1) & 0xffff_fffc) | 0x1
                    } else {
                        self.bar1
                    };
                }
                self.read_u32_raw(offset)
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u16, size: u8, value: u32) {
        let offset = offset as usize;
        match size {
            1 => self.regs[offset] = value as u8,
            2 => self.regs[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            4 => {
                if offset == 0x10 {
                    if value == 0xffff_ffff {
                        self.bar0_probe = true;
                        self.bar0 = 0;
                    } else {
                        self.bar0_probe = false;
                        self.bar0 = value & 0xffff_fff0;
                    }
                    self.write_u32_raw(offset, self.bar0);
                    return;
                }
                if offset == 0x14 {
                    if value == 0xffff_ffff {
                        self.bar1_probe = true;
                        self.bar1 = 0x1;
                    } else {
                        self.bar1_probe = false;
                        self.bar1 = (value & 0xffff_fffc) | 0x1;
                    }
                    self.write_u32_raw(offset, self.bar1);
                    return;
                }
                self.write_u32_raw(offset, value);
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct E1000Device {
    // Core registers
    ctrl: u32,
    status: u32,
    eecd: u32,
    eerd: u32,
    ctrl_ext: u32,
    mdic: u32,

    // Interrupts
    icr: u32,
    ims: u32,

    // Receive path
    rctl: u32,
    rdbal: u32,
    rdbah: u32,
    rdlen: u32,
    rdh: u32,
    rdt: u32,

    // Transmit path
    tctl: u32,
    tdbal: u32,
    tdbah: u32,
    tdlen: u32,
    tdh: u32,
    tdt: u32,
    tx_partial: Vec<u8>,

    // MAC / filter
    mac: [u8; 6],
    ra_valid: bool,
    mta: [u32; 128],

    // EEPROM
    eeprom: [u16; 64],
    phy: [u16; 32],

    // Internal queues
    rx_queue: VecDeque<Vec<u8>>,

    // IRQ line level
    irq_level: bool,

    // Unimplemented register storage to keep driver bring-up smooth.
    other_regs: HashMap<u32, u32>,

    // IOADDR/ IODATA port interface (BAR1).
    io_reg: u32,

    pci: PciConfigSpace,
}

impl E1000Device {
    pub fn new(mac: [u8; 6]) -> Self {
        let mut dev = Self {
            ctrl: 0,
            status: 0,
            eecd: 0,
            eerd: 0,
            ctrl_ext: 0,
            mdic: 0,
            icr: 0,
            ims: 0,
            rctl: 0,
            rdbal: 0,
            rdbah: 0,
            rdlen: 0,
            rdh: 0,
            rdt: 0,
            tctl: 0,
            tdbal: 0,
            tdbah: 0,
            tdlen: 0,
            tdh: 0,
            tdt: 0,
            tx_partial: Vec::new(),
            mac,
            ra_valid: true,
            mta: [0; 128],
            eeprom: [0; 64],
            phy: [0; 32],
            rx_queue: VecDeque::new(),
            irq_level: false,
            other_regs: HashMap::new(),
            io_reg: 0,
            pci: PciConfigSpace::new(),
        };
        dev.reset();
        dev
    }

    pub fn reset(&mut self) {
        self.ctrl = 0;
        // Link up by default (full duplex @ 1Gbps).
        self.status = STATUS_LU | STATUS_FD | STATUS_SPEED_1000;

        self.eecd = EECD_EE_PRES;
        self.eerd = 0;
        self.ctrl_ext = 0;
        self.mdic = MDIC_READY;

        self.icr = 0;
        self.ims = 0;

        self.rctl = 0;
        self.rdbal = 0;
        self.rdbah = 0;
        self.rdlen = 0;
        self.rdh = 0;
        self.rdt = 0;

        self.tctl = 0;
        self.tdbal = 0;
        self.tdbah = 0;
        self.tdlen = 0;
        self.tdh = 0;
        self.tdt = 0;
        self.tx_partial.clear();

        self.rx_queue.clear();
        self.other_regs.clear();
        self.io_reg = 0;

        // Update EEPROM contents (MAC in words 0..=2).
        self.eeprom[0] = u16::from_le_bytes([self.mac[0], self.mac[1]]);
        self.eeprom[1] = u16::from_le_bytes([self.mac[2], self.mac[3]]);
        self.eeprom[2] = u16::from_le_bytes([self.mac[4], self.mac[5]]);

        // Minimal PHY register surface for drivers that probe link state via MDIC.
        // - BMSR (reg 1): link up + auto-negotiation complete.
        // - PHY ID (regs 2/3): plausible Intel-ish values.
        self.phy = [0; 32];
        self.phy[1] = 0x0004 | 0x0020;
        self.phy[2] = 0x0141;
        self.phy[3] = 0x0cc2;

        self.update_irq_level();
    }

    pub fn pci_config_read(&self, offset: u16, size: u8) -> u32 {
        self.pci.read(offset, size)
    }

    pub fn pci_config_write(&mut self, offset: u16, size: u8, value: u32) {
        self.pci.write(offset, size, value);
    }

    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let aligned = offset & !3;
        let shift = ((offset & 3) * 8) as u32;
        let value = self.mmio_read_u32(aligned);
        match size {
            4 => value,
            2 => (value >> shift) & 0xffff,
            1 => (value >> shift) & 0xff,
            _ => 0,
        }
    }

    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let aligned = offset & !3;
        let shift = ((offset & 3) * 8) as u32;
        if size == 4 {
            self.mmio_write_u32(aligned, value);
            return;
        }

        let mask = match size {
            2 => 0xffff << shift,
            1 => 0xff << shift,
            _ => 0,
        };
        let cur = self.mmio_peek_u32(aligned);
        let new_val = (cur & !mask) | ((value << shift) & mask);
        self.mmio_write_u32(aligned, new_val);
    }

    pub fn io_read(&mut self, offset: u32, size: u8) -> u32 {
        match offset {
            // IOADDR (selected MMIO register offset).
            0x0..=0x3 => {
                let shift = ((offset & 3) * 8) as u32;
                match size {
                    4 => self.io_reg,
                    2 => (self.io_reg >> shift) & 0xffff,
                    1 => (self.io_reg >> shift) & 0xff,
                    _ => 0,
                }
            }
            // IODATA (MMIO window to the selected register).
            0x4..=0x7 => self.mmio_read(self.io_reg + (offset - 0x4), size),
            _ => 0,
        }
    }

    pub fn io_write(&mut self, offset: u32, size: u8, value: u32) {
        match offset {
            0x0..=0x3 => {
                let shift = ((offset & 3) * 8) as u32;
                if size == 4 {
                    self.io_reg = value & !3;
                    return;
                }

                let mask = match size {
                    2 => 0xffff << shift,
                    1 => 0xff << shift,
                    _ => 0,
                };
                let cur = self.io_reg;
                self.io_reg = ((cur & !mask) | ((value << shift) & mask)) & !3;
            }
            0x4..=0x7 => self.mmio_write(self.io_reg + (offset - 0x4), size, value),
            _ => {}
        }
    }

    fn mmio_peek_u32(&self, offset: u32) -> u32 {
        match offset {
            REG_CTRL => self.ctrl,
            REG_STATUS => self.status,
            REG_EECD => self.eecd,
            REG_EERD => self.eerd,
            REG_CTRL_EXT => self.ctrl_ext,
            REG_MDIC => self.mdic,

            REG_ICR => self.icr,
            REG_ICS => 0,
            REG_IMS => self.ims,
            REG_IMC => 0,

            REG_RCTL => self.rctl,
            REG_RDBAL => self.rdbal,
            REG_RDBAH => self.rdbah,
            REG_RDLEN => self.rdlen,
            REG_RDH => self.rdh,
            REG_RDT => self.rdt,

            REG_TCTL => self.tctl,
            REG_TDBAL => self.tdbal,
            REG_TDBAH => self.tdbah,
            REG_TDLEN => self.tdlen,
            REG_TDH => self.tdh,
            REG_TDT => self.tdt,

            REG_RAL0 => self.ral0(),
            REG_RAH0 => self.rah0(),

            off if (REG_MTA..REG_MTA + (self.mta.len() as u32 * 4)).contains(&off) => {
                let idx = ((off - REG_MTA) / 4) as usize;
                self.mta[idx]
            }

            _ => *self.other_regs.get(&offset).unwrap_or(&0),
        }
    }

    fn mmio_read_u32(&mut self, offset: u32) -> u32 {
        match offset {
            REG_CTRL => self.ctrl,
            REG_STATUS => self.status,
            REG_EECD => self.eecd,
            REG_EERD => self.eerd,
            REG_CTRL_EXT => self.ctrl_ext,
            REG_MDIC => self.mdic,

            REG_ICR => {
                let val = self.icr;
                self.icr = 0;
                self.update_irq_level();
                val
            }
            REG_ICS => 0,
            REG_IMS => self.ims,
            REG_IMC => 0,

            REG_RCTL => self.rctl,
            REG_RDBAL => self.rdbal,
            REG_RDBAH => self.rdbah,
            REG_RDLEN => self.rdlen,
            REG_RDH => self.rdh,
            REG_RDT => self.rdt,

            REG_TCTL => self.tctl,
            REG_TDBAL => self.tdbal,
            REG_TDBAH => self.tdbah,
            REG_TDLEN => self.tdlen,
            REG_TDH => self.tdh,
            REG_TDT => self.tdt,

            REG_RAL0 => self.ral0(),
            REG_RAH0 => self.rah0(),

            off if (REG_MTA..REG_MTA + (self.mta.len() as u32 * 4)).contains(&off) => {
                let idx = ((off - REG_MTA) / 4) as usize;
                self.mta[idx]
            }

            _ => *self.other_regs.get(&offset).unwrap_or(&0),
        }
    }

    fn mmio_write_u32(&mut self, offset: u32, value: u32) {
        match offset {
            REG_CTRL => {
                if value & CTRL_RST != 0 {
                    self.reset();
                } else {
                    self.ctrl = value;
                }
            }
            REG_EECD => self.eecd = value | EECD_EE_PRES,
            REG_EERD => self.handle_eerd_write(value),
            REG_CTRL_EXT => self.ctrl_ext = value,
            REG_MDIC => self.handle_mdic_write(value),

            REG_ICS => {
                self.icr |= value;
                self.update_irq_level();
            }
            REG_IMS => {
                self.ims |= value;
                self.update_irq_level();
            }
            REG_IMC => {
                self.ims &= !value;
                self.update_irq_level();
            }

            REG_RCTL => self.rctl = value,
            REG_RDBAL => self.rdbal = value,
            REG_RDBAH => self.rdbah = value,
            REG_RDLEN => self.rdlen = value,
            REG_RDH => self.rdh = value,
            REG_RDT => self.rdt = value,

            REG_TCTL => self.tctl = value,
            REG_TDBAL => self.tdbal = value,
            REG_TDBAH => self.tdbah = value,
            REG_TDLEN => self.tdlen = value,
            REG_TDH => self.tdh = value,
            REG_TDT => self.tdt = value,

            REG_RAL0 => self.set_ral0(value),
            REG_RAH0 => self.set_rah0(value),

            off if (REG_MTA..REG_MTA + (self.mta.len() as u32 * 4)).contains(&off) => {
                let idx = ((off - REG_MTA) / 4) as usize;
                self.mta[idx] = value;
            }

            _ => {
                self.other_regs.insert(offset, value);
            }
        }
    }

    fn handle_eerd_write(&mut self, value: u32) {
        if value & EERD_START == 0 {
            self.eerd = value;
            return;
        }

        let addr = ((value >> EERD_ADDR_SHIFT) & 0xff) as usize;
        let data = self.eeprom.get(addr).copied().unwrap_or(0xffff) as u32;
        self.eerd = (data << EERD_DATA_SHIFT) | ((addr as u32) << EERD_ADDR_SHIFT) | EERD_DONE;
    }

    fn handle_mdic_write(&mut self, value: u32) {
        let reg = ((value & MDIC_REG_MASK) >> MDIC_REG_SHIFT) as usize;
        let data = (value & MDIC_DATA_MASK) as u16;

        if (value & MDIC_OP_READ) != 0 {
            let v = self.phy.get(reg).copied().unwrap_or(0) as u32;
            self.mdic = (value & (MDIC_REG_MASK | MDIC_PHY_MASK)) | MDIC_READY | v;
        } else if (value & MDIC_OP_WRITE) != 0 {
            if let Some(slot) = self.phy.get_mut(reg) {
                *slot = data;
            }
            self.mdic = (value & (MDIC_REG_MASK | MDIC_PHY_MASK)) | MDIC_READY | data as u32;
        } else {
            self.mdic = value | MDIC_READY;
        }
    }

    fn ral0(&self) -> u32 {
        u32::from_le_bytes([self.mac[0], self.mac[1], self.mac[2], self.mac[3]])
    }

    fn rah0(&self) -> u32 {
        let mut val = u16::from_le_bytes([self.mac[4], self.mac[5]]) as u32;
        if self.ra_valid {
            val |= 1 << 31;
        }
        val
    }

    fn set_ral0(&mut self, value: u32) {
        let bytes = value.to_le_bytes();
        self.mac[0..4].copy_from_slice(&bytes);
        // Mirror into EEPROM words as well for driver expectations.
        self.eeprom[0] = u16::from_le_bytes([self.mac[0], self.mac[1]]);
        self.eeprom[1] = u16::from_le_bytes([self.mac[2], self.mac[3]]);
    }

    fn set_rah0(&mut self, value: u32) {
        let upper = (value & 0xffff) as u16;
        let bytes = upper.to_le_bytes();
        self.mac[4] = bytes[0];
        self.mac[5] = bytes[1];
        self.ra_valid = (value & (1 << 31)) != 0;
        self.eeprom[2] = u16::from_le_bytes([self.mac[4], self.mac[5]]);
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn enqueue_rx_frame(&mut self, frame: Vec<u8>) {
        self.rx_queue.push_back(frame);
    }

    pub fn poll<M: GuestMemory, B: NetworkBackend>(&mut self, mem: &mut M, backend: &mut B) {
        self.process_tx(mem, backend);
        self.process_rx(mem);
    }

    fn raise_interrupt(&mut self, cause: u32) {
        self.icr |= cause;
        self.update_irq_level();
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.icr & self.ims) != 0;
    }

    fn rx_ring_base(&self) -> u64 {
        (self.rdbal as u64) | ((self.rdbah as u64) << 32)
    }

    fn tx_ring_base(&self) -> u64 {
        (self.tdbal as u64) | ((self.tdbah as u64) << 32)
    }

    fn rx_buffer_size(&self) -> usize {
        let bsex = (self.rctl & RCTL_BSEX) != 0;
        let bsize = (self.rctl & RCTL_BSIZE_MASK) >> 16;
        match (bsex, bsize) {
            (false, 0b00) => 2048,
            (false, 0b01) => 1024,
            (false, 0b10) => 512,
            (false, 0b11) => 256,
            (true, 0b00) => 16384,
            (true, 0b01) => 8192,
            (true, 0b10) => 4096,
            (true, 0b11) => 2048, // reserved; default to 2K
            _ => 2048,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestMemory {
        buf: Vec<u8>,
    }

    impl TestMemory {
        fn new(size: usize) -> Self {
            Self { buf: vec![0; size] }
        }
    }

    impl GuestMemory for TestMemory {
        fn read(&self, addr: u64, buf: &mut [u8]) {
            let addr = addr as usize;
            buf.copy_from_slice(&self.buf[addr..addr + buf.len()]);
        }

        fn write(&mut self, addr: u64, data: &[u8]) {
            let addr = addr as usize;
            self.buf[addr..addr + data.len()].copy_from_slice(data);
        }
    }

    #[derive(Default)]
    struct TestBackend {
        frames: Vec<Vec<u8>>,
    }

    impl NetworkBackend for TestBackend {
        fn transmit(&mut self, frame: Vec<u8>) {
            self.frames.push(frame);
        }
    }

    #[test]
    fn tx_descriptor_emits_frame_and_sets_dd() {
        let mut mem = TestMemory::new(0x10000);
        let mut backend = TestBackend::default();
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        let ring_base = 0x1000u64;
        let buf_addr = 0x2000u64;
        let frame = vec![0xde, 0xad, 0xbe, 0xef];
        mem.write(buf_addr, &frame);

        let desc = TxDesc {
            buffer_addr: buf_addr,
            length: frame.len() as u16,
            cmd: TXD_CMD_EOP | TXD_CMD_RS,
            status: 0,
            ..TxDesc::default()
        };
        desc.write(&mut mem, ring_base);

        dev.mmio_write(REG_TDBAL, 4, ring_base as u32);
        dev.mmio_write(REG_TDBAH, 4, 0);
        dev.mmio_write(REG_TDLEN, 4, 16 * 8);
        dev.mmio_write(REG_TDH, 4, 0);
        dev.mmio_write(REG_TDT, 4, 1);
        dev.mmio_write(REG_TCTL, 4, TCTL_EN);

        dev.mmio_write(REG_IMS, 4, ICR_TXDW);

        dev.poll(&mut mem, &mut backend);

        assert_eq!(backend.frames, vec![frame]);

        let written = TxDesc::read(&mem, ring_base);
        assert_ne!(written.status & TXD_STAT_DD, 0);
        assert_eq!(dev.mmio_read(REG_TDH, 4), 1);

        assert!(dev.irq_level());
        let icr = dev.mmio_read(REG_ICR, 4);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
        assert!(!dev.irq_level());
    }

    #[test]
    fn rx_path_writes_descriptor_and_raises_interrupt() {
        let mut mem = TestMemory::new(0x20000);
        let mut backend = TestBackend::default();
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        let ring_base = 0x3000u64;
        let buf_addr = 0x4000u64;
        let frame = vec![1, 2, 3, 4, 5, 6];

        // Build 8 RX descriptors with separate buffers.
        for i in 0..8u64 {
            let desc = RxDesc {
                buffer_addr: buf_addr + i * 0x800,
                ..RxDesc::default()
            };
            desc.write(&mut mem, ring_base + i * 16);
        }

        dev.mmio_write(REG_RDBAL, 4, ring_base as u32);
        dev.mmio_write(REG_RDBAH, 4, 0);
        dev.mmio_write(REG_RDLEN, 4, 16 * 8);
        dev.mmio_write(REG_RDH, 4, 0);
        dev.mmio_write(REG_RDT, 4, 7);
        dev.mmio_write(REG_RCTL, 4, RCTL_EN);

        dev.mmio_write(REG_IMS, 4, ICR_RXT0);

        dev.enqueue_rx_frame(frame.clone());
        dev.poll(&mut mem, &mut backend);

        let desc0 = RxDesc::read(&mem, ring_base);
        assert_eq!(desc0.length as usize, frame.len());
        assert_eq!(
            desc0.status & (RXD_STAT_DD | RXD_STAT_EOP),
            RXD_STAT_DD | RXD_STAT_EOP
        );

        let mut written = vec![0u8; frame.len()];
        mem.read(buf_addr, &mut written);
        assert_eq!(written, frame);

        assert_eq!(dev.mmio_read(REG_RDH, 4), 1);
        assert!(dev.irq_level());
        let icr = dev.mmio_read(REG_ICR, 4);
        assert_eq!(icr & ICR_RXT0, ICR_RXT0);
        assert!(!dev.irq_level());
    }

    #[test]
    fn interrupt_mask_and_icr_read_to_clear() {
        let mut dev = E1000Device::new([0, 1, 2, 3, 4, 5]);

        // Cause without mask should not assert IRQ line.
        dev.mmio_write(REG_ICS, 4, ICR_TXDW);
        assert!(!dev.irq_level());

        // Enabling the mask should immediately assert due to pending cause.
        dev.mmio_write(REG_IMS, 4, ICR_TXDW);
        assert!(dev.irq_level());

        // Reading ICR clears and deasserts.
        let icr = dev.mmio_read(REG_ICR, 4);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
        assert!(!dev.irq_level());

        // Mask clear prevents assertion for future causes.
        dev.mmio_write(REG_IMC, 4, ICR_TXDW);
        dev.mmio_write(REG_ICS, 4, ICR_TXDW);
        assert!(!dev.irq_level());
        let icr = dev.mmio_read(REG_ICR, 4);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
    }

    #[test]
    fn pci_config_bar_probing_and_eeprom_read_work() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let mut dev = E1000Device::new(mac);

        assert_eq!(
            dev.pci_config_read(0x00, 2) as u16,
            PciConfigSpace::VENDOR_ID_INTEL
        );
        assert_eq!(
            dev.pci_config_read(0x02, 2) as u16,
            PciConfigSpace::DEVICE_ID_82540EM
        );

        dev.pci_config_write(0x10, 4, 0xffff_ffff);
        assert_eq!(
            dev.pci_config_read(0x10, 4),
            !(PciConfigSpace::MMIO_BAR_SIZE - 1) & 0xffff_fff0
        );

        dev.pci_config_write(0x10, 4, 0xf000_1234);
        assert_eq!(dev.pci_config_read(0x10, 4), 0xf000_1230);

        dev.pci_config_write(0x14, 4, 0xffff_ffff);
        assert_eq!(
            dev.pci_config_read(0x14, 4),
            (!(PciConfigSpace::IO_BAR_SIZE - 1) & 0xffff_fffc) | 0x1
        );
        dev.pci_config_write(0x14, 4, 0xc000);
        assert_eq!(dev.pci_config_read(0x14, 4), 0xc001);

        dev.mmio_write(REG_EERD, 4, EERD_START | (0 << EERD_ADDR_SHIFT));
        let eerd = dev.mmio_read(REG_EERD, 4);
        assert_ne!(eerd & EERD_DONE, 0);
        let word0 = ((eerd >> EERD_DATA_SHIFT) & 0xffff) as u16;
        assert_eq!(word0, u16::from_le_bytes([mac[0], mac[1]]));
    }

    #[test]
    fn ioaddr_iodata_interface_maps_to_mmio_registers() {
        let mut dev = E1000Device::new([0, 1, 2, 3, 4, 5]);

        dev.io_write(0x0, 4, REG_ICS);
        dev.io_write(0x4, 4, ICR_TXDW);
        assert_eq!(dev.mmio_read(REG_ICR, 4) & ICR_TXDW, ICR_TXDW);

        dev.mmio_write(REG_ICS, 4, ICR_TXDW);
        dev.mmio_write(REG_IMS, 4, ICR_TXDW);
        assert!(dev.irq_level());

        dev.io_write(0x0, 4, REG_ICR);
        let icr = dev.io_read(0x4, 4);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
        assert!(!dev.irq_level());
    }

    #[test]
    fn mdic_reads_phy_registers_and_sets_ready() {
        let mut dev = E1000Device::new([0, 1, 2, 3, 4, 5]);

        // Read BMSR (reg 1) via MDIC.
        let cmd = (1 << MDIC_REG_SHIFT) | (1 << MDIC_PHY_SHIFT) | MDIC_OP_READ;
        dev.mmio_write(REG_MDIC, 4, cmd);
        let mdic = dev.mmio_read(REG_MDIC, 4);
        assert_ne!(mdic & MDIC_READY, 0);
        assert_eq!(mdic & MDIC_DATA_MASK, 0x0004 | 0x0020);

        // Write BMCR (reg 0) and read back.
        let write_cmd =
            (0 << MDIC_REG_SHIFT) | (1 << MDIC_PHY_SHIFT) | MDIC_OP_WRITE | 0x1234;
        dev.mmio_write(REG_MDIC, 4, write_cmd);
        let mdic = dev.mmio_read(REG_MDIC, 4);
        assert_ne!(mdic & MDIC_READY, 0);
        assert_eq!(mdic & MDIC_DATA_MASK, 0x1234);

        dev.mmio_write(REG_MDIC, 4, (0 << MDIC_REG_SHIFT) | (1 << MDIC_PHY_SHIFT) | MDIC_OP_READ);
        let mdic = dev.mmio_read(REG_MDIC, 4);
        assert_eq!(mdic & MDIC_DATA_MASK, 0x1234);
    }
}
