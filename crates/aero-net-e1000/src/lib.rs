//! E1000 (Intel 82540EM-ish) virtual NIC model.
//!
//! This crate intentionally models only the subset of the device required for
//! basic Windows 7 networking driver bring-up:
//! - Basic PCI config space (vendor/device IDs + BAR0 probing/programming)
//! - MMIO register interface for init, RX/TX rings, and interrupts
//! - Legacy RX/TX descriptor rings with DMA read/write via [`memory::MemoryBus`]
//! - Simple host-facing frame queues
//!
//! The implementation aims to be "good enough" for driver compatibility
//! without chasing every obscure corner case of real silicon.

use std::collections::{HashMap, VecDeque};

use memory::MemoryBus;

mod offload;

use offload::{apply_checksum_offload, tso_segment, TxChecksumFlags, TxOffloadContext};

/// Size of the E1000 MMIO BAR.
pub const E1000_MMIO_SIZE: u32 = 0x20_000;

// MMIO register offsets (subset).
const REG_CTRL: u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EECD: u32 = 0x0010;
const REG_EERD: u32 = 0x0014;
const REG_CTRL_EXT: u32 = 0x0018;
const REG_MDIC: u32 = 0x0020;

const REG_ICR: u32 = 0x00C0;
const REG_ICS: u32 = 0x00C8;
const REG_IMS: u32 = 0x00D0;
const REG_IMC: u32 = 0x00D8;

const REG_RCTL: u32 = 0x0100;
const REG_TCTL: u32 = 0x0400;

const REG_RDBAL: u32 = 0x2800;
const REG_RDBAH: u32 = 0x2804;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;

const REG_TDBAL: u32 = 0x3800;
const REG_TDBAH: u32 = 0x3804;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH: u32 = 0x3810;
const REG_TDT: u32 = 0x3818;

const REG_RAL0: u32 = 0x5400;
const REG_RAH0: u32 = 0x5404;

// CTRL bits.
const CTRL_RST: u32 = 1 << 26;

// STATUS bits.
const STATUS_FD: u32 = 1 << 0;
const STATUS_LU: u32 = 1 << 1;
const STATUS_SPEED_1000: u32 = 1 << 7;

// EERD bits/fields.
const EERD_START: u32 = 1 << 0;
const EERD_DONE: u32 = 1 << 4;
const EERD_ADDR_SHIFT: u32 = 8;
const EERD_DATA_SHIFT: u32 = 16;

// EECD bits (subset).
const EECD_EE_PRES: u32 = 1 << 8;

// MDIC bits/fields (subset).
const MDIC_DATA_MASK: u32 = 0x0000_FFFF;
const MDIC_REG_SHIFT: u32 = 16;
const MDIC_REG_MASK: u32 = 0x001F_0000;
const MDIC_PHY_SHIFT: u32 = 21;
const MDIC_PHY_MASK: u32 = 0x03E0_0000;
const MDIC_OP_WRITE: u32 = 0x0400_0000;
const MDIC_OP_READ: u32 = 0x0800_0000;
const MDIC_READY: u32 = 0x1000_0000;

// Interrupt Cause bits (subset).
pub const ICR_TXDW: u32 = 1 << 0;
pub const ICR_RXT0: u32 = 1 << 7;

// RCTL bits (subset).
const RCTL_EN: u32 = 1 << 1;
const RCTL_BSIZE_SHIFT: u32 = 16;
const RCTL_BSIZE_MASK: u32 = 0b11 << RCTL_BSIZE_SHIFT;
const RCTL_BSEX: u32 = 1 << 25;

// TCTL bits (subset).
const TCTL_EN: u32 = 1 << 1;

// TX descriptor bits (legacy).
const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IC: u8 = 1 << 2;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_CMD_DEXT: u8 = 1 << 5;
const TXD_CMD_TSE: u8 = 1 << 7;
const TXD_STAT_DD: u8 = 1 << 0;

// TX descriptor "DTYP" field (advanced).
const TXD_DTYP_CTXT: u8 = 0x2;
const TXD_DTYP_DATA: u8 = 0x3;

// RX descriptor bits (legacy).
const RXD_STAT_DD: u8 = 1 << 0;
const RXD_STAT_EOP: u8 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RxDesc {
    buffer_addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

impl RxDesc {
    const LEN: usize = 16;

    fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self {
            buffer_addr: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            checksum: u16::from_le_bytes(bytes[10..12].try_into().unwrap()),
            status: bytes[12],
            errors: bytes[13],
            special: u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
        }
    }

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.buffer_addr.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.length.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.checksum.to_le_bytes());
        bytes[12] = self.status;
        bytes[13] = self.errors;
        bytes[14..16].copy_from_slice(&self.special.to_le_bytes());
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self {
            buffer_addr: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            cso: bytes[10],
            cmd: bytes[11],
            status: bytes[12],
            css: bytes[13],
            special: u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TxContextDesc {
    ipcss: u8,
    ipcso: u8,
    ipcse: u16,
    tucss: u8,
    tucso: u8,
    tucse: u16,
    mss: u16,
    hdr_len: u8,
    cmd: u8,
    tcp_hdr_len: u8,
}

impl TxContextDesc {
    fn from_bytes(bytes: [u8; TxDesc::LEN]) -> Self {
        Self {
            ipcss: bytes[0],
            ipcso: bytes[1],
            ipcse: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            tucss: bytes[4],
            tucso: bytes[5],
            tucse: u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            mss: u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
            hdr_len: bytes[14],
            cmd: bytes[11],
            tcp_hdr_len: bytes[15],
        }
    }
}

impl From<TxContextDesc> for TxOffloadContext {
    fn from(value: TxContextDesc) -> Self {
        Self {
            ipcss: value.ipcss as usize,
            ipcso: value.ipcso as usize,
            ipcse: value.ipcse as usize,
            tucss: value.tucss as usize,
            tucso: value.tucso as usize,
            tucse: value.tucse as usize,
            mss: value.mss as usize,
            hdr_len: value.hdr_len as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TxDataDesc {
    buffer_addr: u64,
    length: u16,
    cmd: u8,
    status: u8,
    popts: u8,
    special: u16,
}

impl TxDataDesc {
    fn from_bytes(bytes: [u8; TxDesc::LEN]) -> Self {
        Self {
            buffer_addr: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            cmd: bytes[11],
            status: bytes[12],
            popts: bytes[13],
            special: u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxDescriptor {
    Legacy(TxDesc),
    Context(TxContextDesc),
    Data(TxDataDesc),
}

impl TxDescriptor {
    fn parse(bytes: [u8; TxDesc::LEN]) -> Option<Self> {
        let cmd = bytes[11];
        if (cmd & TXD_CMD_DEXT) == 0 {
            return Some(Self::Legacy(TxDesc::from_bytes(bytes)));
        }

        let dtyp = bytes[10] >> 4;
        match dtyp {
            TXD_DTYP_CTXT => Some(Self::Context(TxContextDesc::from_bytes(bytes))),
            TXD_DTYP_DATA => Some(Self::Data(TxDataDesc::from_bytes(bytes))),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxPacketState {
    Legacy { cmd: u8, css: usize, cso: usize },
    Advanced { cmd: u8, popts: u8 },
}

fn read_desc<const N: usize>(mem: &mut dyn MemoryBus, addr: u64) -> [u8; N] {
    let mut buf = [0u8; N];
    mem.read_physical(addr, &mut buf);
    buf
}

fn write_desc(mem: &mut dyn MemoryBus, addr: u64, bytes: &[u8]) {
    mem.write_physical(addr, bytes);
}

#[derive(Clone, Debug)]
pub struct PciConfig {
    regs: [u8; 256],
    bar0: u32,
    bar0_probe: bool,
}

impl PciConfig {
    pub const VENDOR_ID: u16 = 0x8086;
    pub const DEVICE_ID: u16 = 0x100E; // 82540EM (QEMU default)

    pub fn new() -> Self {
        let mut regs = [0u8; 256];
        regs[0x00..0x02].copy_from_slice(&Self::VENDOR_ID.to_le_bytes());
        regs[0x02..0x04].copy_from_slice(&Self::DEVICE_ID.to_le_bytes());

        // Class code: Network controller / Ethernet controller.
        regs[0x0a] = 0x00; // subclass
        regs[0x0b] = 0x02; // class
        regs[0x0e] = 0x00; // header type

        // Subsystem IDs (keep Intel for familiarity).
        regs[0x2c..0x2e].copy_from_slice(&Self::VENDOR_ID.to_le_bytes());
        regs[0x2e..0x30].copy_from_slice(&Self::DEVICE_ID.to_le_bytes());

        // INTA#
        regs[0x3d] = 0x01;

        Self {
            regs,
            bar0: 0,
            bar0_probe: false,
        }
    }

    pub fn bar0(&self) -> u32 {
        self.bar0
    }

    fn read_u32_raw(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.regs[offset..offset + 4].try_into().unwrap())
    }

    fn write_u32_raw(&mut self, offset: usize, value: u32) {
        self.regs[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read(&self, offset: u16, size: usize) -> u32 {
        let offset = offset as usize;
        match size {
            1 => self.regs[offset] as u32,
            2 => u16::from_le_bytes(self.regs[offset..offset + 2].try_into().unwrap()) as u32,
            4 => {
                if offset == 0x10 {
                    return if self.bar0_probe {
                        (!(E1000_MMIO_SIZE - 1)) & 0xffff_fff0
                    } else {
                        self.bar0
                    };
                }
                self.read_u32_raw(offset)
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
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
                self.write_u32_raw(offset, value);
            }
            _ => {}
        }
    }

    pub fn read_u32(&self, offset: u16) -> u32 {
        self.read(offset, 4)
    }

    pub fn write_u32(&mut self, offset: u16, value: u32) {
        self.write(offset, 4, value)
    }
}

/// E1000 PCI device model.
///
/// The device exposes:
/// - PCI config space via [`pci_read_u32`] / [`pci_write_u32`]
/// - BAR0 MMIO via [`mmio_read_u32`] / [`mmio_write_u32`]
/// - host networking queues: RX in (`receive_frame`), TX out (`pop_tx_frame`)
#[derive(Debug)]
pub struct E1000Device {
    pub pci: PciConfig,

    // Registers (subset).
    ctrl: u32,
    status: u32,
    eecd: u32,
    eerd: u32,
    ctrl_ext: u32,
    mdic: u32,

    icr: u32,
    ims: u32,
    irq_level: bool,

    rctl: u32,
    tctl: u32,

    rdbal: u32,
    rdbah: u32,
    rdlen: u32,
    rdh: u32,
    rdt: u32,

    tdbal: u32,
    tdbah: u32,
    tdlen: u32,
    tdh: u32,
    tdt: u32,
    tx_partial: Vec<u8>,
    tx_ctx: TxOffloadContext,
    tx_state: Option<TxPacketState>,

    mac_addr: [u8; 6],
    eeprom: [u16; 64],
    phy: [u16; 32],
    other_regs: HashMap<u32, u32>,

    rx_pending: VecDeque<Vec<u8>>,
    tx_out: VecDeque<Vec<u8>>,
}

impl E1000Device {
    pub fn new(mac_addr: [u8; 6]) -> Self {
        let mut dev = Self {
            pci: PciConfig::new(),
            ctrl: 0,
            status: STATUS_LU | STATUS_FD | STATUS_SPEED_1000,
            eecd: EECD_EE_PRES,
            eerd: 0,
            ctrl_ext: 0,
            mdic: 0,
            icr: 0,
            ims: 0,
            irq_level: false,
            rctl: 0,
            tctl: 0,
            rdbal: 0,
            rdbah: 0,
            rdlen: 0,
            rdh: 0,
            rdt: 0,
            tdbal: 0,
            tdbah: 0,
            tdlen: 0,
            tdh: 0,
            tdt: 0,
            tx_partial: Vec::new(),
            tx_ctx: TxOffloadContext::default(),
            tx_state: None,
            mac_addr,
            eeprom: [0xFFFF; 64],
            phy: [0; 32],
            other_regs: HashMap::new(),
            rx_pending: VecDeque::new(),
            tx_out: VecDeque::new(),
        };
        dev.init_eeprom_from_mac();
        dev.init_phy();
        dev
    }

    fn init_eeprom_from_mac(&mut self) {
        self.eeprom[0] = u16::from_le_bytes([self.mac_addr[0], self.mac_addr[1]]);
        self.eeprom[1] = u16::from_le_bytes([self.mac_addr[2], self.mac_addr[3]]);
        self.eeprom[2] = u16::from_le_bytes([self.mac_addr[4], self.mac_addr[5]]);
    }

    fn init_phy(&mut self) {
        // Minimal PHY register set to keep common drivers happy.
        //
        // Registers are standard MII:
        //  - 0: BMCR
        //  - 1: BMSR
        //  - 2/3: PHY ID
        const MII_BMSR: usize = 1;
        const MII_PHYSID1: usize = 2;
        const MII_PHYSID2: usize = 3;

        // BMSR: link up + auto-negotiation complete.
        self.phy[MII_BMSR] = 0x0004 | 0x0020;

        // A plausible Intel-ish PHY ID (not intended to match real silicon).
        self.phy[MII_PHYSID1] = 0x0141;
        self.phy[MII_PHYSID2] = 0x0CC2;
    }

    pub fn mac_addr(&self) -> [u8; 6] {
        self.mac_addr
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn pci_config_read(&self, offset: u16, size: usize) -> u32 {
        self.pci.read(offset, size)
    }

    pub fn pci_config_write(&mut self, offset: u16, size: usize, value: u32) {
        self.pci.write(offset, size, value)
    }

    pub fn pci_read_u32(&self, offset: u16) -> u32 {
        self.pci.read_u32(offset)
    }

    pub fn pci_write_u32(&mut self, offset: u16, value: u32) {
        self.pci.write_u32(offset, value)
    }

    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u32 {
        let aligned = (offset & !3) as u32;
        let shift = ((offset & 3) * 8) as u32;
        let value = self.mmio_read_u32_aligned(aligned);
        match size {
            4 => value,
            2 => (value >> shift) & 0xffff,
            1 => (value >> shift) & 0xff,
            _ => 0,
        }
    }

    pub fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let aligned = (offset & !3) as u32;
        let shift = ((offset & 3) * 8) as u32;

        let value32 = match size {
            4 => value,
            2 => (value & 0xffff) << shift,
            1 => (value & 0xff) << shift,
            _ => return,
        };

        let merged = if size == 4 {
            value32
        } else {
            let cur = self.mmio_peek_u32(aligned);
            let mask = match size {
                2 => 0xffffu32 << shift,
                1 => 0xffu32 << shift,
                _ => 0,
            };
            (cur & !mask) | value32
        };

        self.mmio_write_u32_aligned(mem, aligned, merged);
    }

    pub fn mmio_read_u32(&mut self, offset: u32) -> u32 {
        self.mmio_read(offset as u64, 4)
    }

    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u32, value: u32) {
        self.mmio_write(mem, offset as u64, 4, value);
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        self.process_tx(mem);
        self.flush_rx_pending(mem);
    }

    /// Queue a host→guest Ethernet frame for later delivery.
    ///
    /// The caller is expected to invoke [`poll`] (or [`receive_frame`]) to flush
    /// pending frames into the RX descriptor ring when buffers are available.
    pub fn enqueue_rx_frame(&mut self, frame: Vec<u8>) {
        // Keep memory bounded even if the guest never enables RX.
        const MAX_PENDING: usize = 256;
        if self.rx_pending.len() >= MAX_PENDING {
            self.rx_pending.pop_front();
        }
        self.rx_pending.push_back(frame);
    }

    /// Host → guest path.
    ///
    /// Frames are queued and then copied into guest RX buffers when the guest
    /// has enabled reception and made descriptors available.
    pub fn receive_frame(&mut self, mem: &mut dyn MemoryBus, frame: &[u8]) {
        self.enqueue_rx_frame(frame.to_vec());
        self.flush_rx_pending(mem);
    }

    /// Guest → host path. Returns the next frame transmitted by the guest.
    pub fn pop_tx_frame(&mut self) -> Option<Vec<u8>> {
        self.tx_out.pop_front()
    }

    fn reset(&mut self) {
        self.ctrl = 0;
        self.eecd = EECD_EE_PRES;
        self.eerd = 0;
        self.ctrl_ext = 0;
        self.mdic = 0;

        self.icr = 0;
        self.ims = 0;
        self.irq_level = false;

        self.rctl = 0;
        self.tctl = 0;

        self.rdbal = 0;
        self.rdbah = 0;
        self.rdlen = 0;
        self.rdh = 0;
        self.rdt = 0;

        self.tdbal = 0;
        self.tdbah = 0;
        self.tdlen = 0;
        self.tdh = 0;
        self.tdt = 0;
        self.tx_partial.clear();
        self.tx_ctx = TxOffloadContext::default();
        self.tx_state = None;

        self.other_regs.clear();
        self.rx_pending.clear();
        self.tx_out.clear();

        self.init_eeprom_from_mac();
        self.init_phy();
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.icr & self.ims) != 0;
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
            REG_TCTL => self.tctl,

            REG_RDBAL => self.rdbal,
            REG_RDBAH => self.rdbah,
            REG_RDLEN => self.rdlen,
            REG_RDH => self.rdh,
            REG_RDT => self.rdt,

            REG_TDBAL => self.tdbal,
            REG_TDBAH => self.tdbah,
            REG_TDLEN => self.tdlen,
            REG_TDH => self.tdh,
            REG_TDT => self.tdt,

            REG_RAL0 => u32::from_le_bytes([
                self.mac_addr[0],
                self.mac_addr[1],
                self.mac_addr[2],
                self.mac_addr[3],
            ]),
            REG_RAH0 => {
                let mut v = u32::from_le_bytes([self.mac_addr[4], self.mac_addr[5], 0, 0]);
                v |= 1u32 << 31; // AV bit
                v
            }
            _ => *self.other_regs.get(&offset).unwrap_or(&0),
        }
    }

    fn mmio_read_u32_aligned(&mut self, offset: u32) -> u32 {
        match offset {
            REG_ICR => {
                let v = self.icr;
                self.icr = 0;
                self.update_irq_level();
                v
            }
            _ => self.mmio_peek_u32(offset),
        }
    }

    fn mmio_write_u32_aligned(&mut self, mem: &mut dyn MemoryBus, offset: u32, value: u32) {
        match offset {
            REG_CTRL => {
                if (value & CTRL_RST) != 0 {
                    self.reset();
                } else {
                    self.ctrl = value;
                }
            }
            REG_EECD => self.eecd = value | EECD_EE_PRES,
            REG_EERD => {
                self.eerd = value;
                if (value & EERD_START) != 0 {
                    let addr = ((value >> EERD_ADDR_SHIFT) & 0xFF) as usize;
                    let data = self.eeprom.get(addr).copied().unwrap_or(0xFFFF) as u32;
                    self.eerd =
                        (addr as u32) << EERD_ADDR_SHIFT | EERD_DONE | (data << EERD_DATA_SHIFT);
                }
            }
            REG_CTRL_EXT => self.ctrl_ext = value,
            REG_MDIC => {
                let reg = ((value & MDIC_REG_MASK) >> MDIC_REG_SHIFT) as usize;
                let data = (value & MDIC_DATA_MASK) as u16;

                // Only a single PHY at address 1 is modeled. If another address is used, we
                // still return READY with 0 data.
                let _phy = ((value & MDIC_PHY_MASK) >> MDIC_PHY_SHIFT) as u8;

                if (value & MDIC_OP_READ) != 0 {
                    let v = self.phy.get(reg).copied().unwrap_or(0) as u32;
                    self.mdic = (value & (MDIC_REG_MASK | MDIC_PHY_MASK)) | MDIC_READY | v;
                } else if (value & MDIC_OP_WRITE) != 0 {
                    if let Some(slot) = self.phy.get_mut(reg) {
                        *slot = data;
                    }
                    self.mdic =
                        (value & (MDIC_REG_MASK | MDIC_PHY_MASK)) | MDIC_READY | data as u32;
                } else {
                    self.mdic = value | MDIC_READY;
                }
            }

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

            REG_RCTL => {
                self.rctl = value;
                self.flush_rx_pending(mem);
            }
            REG_TCTL => self.tctl = value,

            REG_RDBAL => self.rdbal = value,
            REG_RDBAH => self.rdbah = value,
            REG_RDLEN => self.rdlen = value,
            REG_RDH => self.rdh = value,
            REG_RDT => {
                self.rdt = value;
                self.flush_rx_pending(mem);
            }

            REG_TDBAL => self.tdbal = value,
            REG_TDBAH => self.tdbah = value,
            REG_TDLEN => self.tdlen = value,
            REG_TDH => self.tdh = value,
            REG_TDT => {
                self.tdt = value;
                self.process_tx(mem);
            }

            REG_RAL0 => {
                let bytes = value.to_le_bytes();
                self.mac_addr[0..4].copy_from_slice(&bytes);
                self.init_eeprom_from_mac();
            }
            REG_RAH0 => {
                let bytes = value.to_le_bytes();
                self.mac_addr[4] = bytes[0];
                self.mac_addr[5] = bytes[1];
                self.init_eeprom_from_mac();
            }
            _ => {
                self.other_regs.insert(offset, value);
            }
        }
    }

    fn rx_ring_desc_count(&self) -> Option<u32> {
        if self.rdlen < RxDesc::LEN as u32 || (self.rdlen % RxDesc::LEN as u32) != 0 {
            return None;
        }
        Some(self.rdlen / RxDesc::LEN as u32)
    }

    fn tx_ring_desc_count(&self) -> Option<u32> {
        if self.tdlen < TxDesc::LEN as u32 || (self.tdlen % TxDesc::LEN as u32) != 0 {
            return None;
        }
        Some(self.tdlen / TxDesc::LEN as u32)
    }

    fn rx_desc_base(&self) -> u64 {
        ((self.rdbah as u64) << 32) | (self.rdbal as u64)
    }

    fn tx_desc_base(&self) -> u64 {
        ((self.tdbah as u64) << 32) | (self.tdbal as u64)
    }

    fn rx_buf_len(&self) -> usize {
        let bsize = (self.rctl & RCTL_BSIZE_MASK) >> RCTL_BSIZE_SHIFT;
        let bsex = (self.rctl & RCTL_BSEX) != 0;
        match (bsex, bsize) {
            (false, 0b00) => 2048,
            (false, 0b01) => 1024,
            (false, 0b10) => 512,
            (false, 0b11) => 256,
            (true, 0b00) => 16 * 1024,
            (true, 0b01) => 8 * 1024,
            (true, 0b10) => 4 * 1024,
            // Hardware reserves 0b11 when BSEX=1.
            (true, 0b11) => 2048,
            _ => 2048,
        }
    }

    fn flush_rx_pending(&mut self, mem: &mut dyn MemoryBus) {
        if (self.rctl & RCTL_EN) == 0 {
            return;
        }
        let Some(desc_count) = self.rx_ring_desc_count() else {
            return;
        };
        if desc_count == 0 {
            return;
        }
        let base = self.rx_desc_base();
        let buf_len = self.rx_buf_len();

        while let Some(frame) = self.rx_pending.front() {
            // The hardware head (RDH) must not catch up to the software tail (RDT).
            // Keep one descriptor unused to avoid ambiguity in full/empty conditions.
            if self.rdh == self.rdt {
                break;
            }
            let idx = (self.rdh % desc_count) as u64;
            let desc_addr = base + idx * RxDesc::LEN as u64;
            let desc_bytes = read_desc::<{ RxDesc::LEN }>(mem, desc_addr);
            let mut desc = RxDesc::from_bytes(desc_bytes);

            if desc.buffer_addr == 0 {
                // Driver hasn't set up this descriptor; stop.
                break;
            }

            let copy_len = frame.len().min(buf_len);
            mem.write_physical(desc.buffer_addr, &frame[..copy_len]);

            desc.length = copy_len as u16;
            desc.checksum = 0;
            desc.errors = 0;
            desc.status = RXD_STAT_DD | RXD_STAT_EOP;
            write_desc(mem, desc_addr, &desc.to_bytes());

            self.rx_pending.pop_front();

            self.rdh = (self.rdh + 1) % desc_count;

            self.icr |= ICR_RXT0;
            self.update_irq_level();
        }
    }

    fn process_tx(&mut self, mem: &mut dyn MemoryBus) {
        if (self.tctl & TCTL_EN) == 0 {
            return;
        }
        let Some(desc_count) = self.tx_ring_desc_count() else {
            return;
        };
        if desc_count == 0 {
            return;
        }
        let base = self.tx_desc_base();

        let mut should_raise_txdw = false;

        while self.tdh != self.tdt {
            let idx = (self.tdh % desc_count) as u64;
            let desc_addr = base + idx * TxDesc::LEN as u64;
            let mut desc_bytes = read_desc::<{ TxDesc::LEN }>(mem, desc_addr);

            let Some(desc) = TxDescriptor::parse(desc_bytes) else {
                // Unknown descriptor type; best-effort mark completion and move on.
                desc_bytes[12] |= TXD_STAT_DD;
                write_desc(mem, desc_addr, &desc_bytes);
                self.tdh = (self.tdh + 1) % desc_count;
                continue;
            };

            match desc {
                TxDescriptor::Context(ctx_desc) => {
                    self.tx_ctx = ctx_desc.into();

                    if (ctx_desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    // Context descriptors have a different upper dword layout. We
                    // best-effort mark completion in the last byte without
                    // clobbering MSS/hdr_len.
                    desc_bytes[15] |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc_bytes);
                }
                TxDescriptor::Legacy(mut desc) => {
                    match self.tx_state {
                        None => {
                            self.tx_state = Some(TxPacketState::Legacy {
                                cmd: desc.cmd,
                                css: desc.css as usize,
                                cso: desc.cso as usize,
                            });
                        }
                        Some(TxPacketState::Legacy {
                            ref mut cmd,
                            ref mut css,
                            ref mut cso,
                        }) => {
                            *cmd |= desc.cmd;
                            *css = desc.css as usize;
                            *cso = desc.cso as usize;
                        }
                        Some(TxPacketState::Advanced { .. }) => {
                            self.tx_partial.clear();
                            self.tx_state = Some(TxPacketState::Legacy {
                                cmd: desc.cmd,
                                css: desc.css as usize,
                                cso: desc.cso as usize,
                            });
                        }
                    }

                    if desc.buffer_addr != 0 && desc.length != 0 {
                        let mut buf = vec![0u8; desc.length as usize];
                        mem.read_physical(desc.buffer_addr, &mut buf);
                        self.tx_partial.extend_from_slice(&buf);
                    }

                    desc.status |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc.to_bytes());

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        let Some(TxPacketState::Legacy { cmd, css, cso }) = self.tx_state.take() else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % desc_count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            use nt_packetlib::io::net::packet::checksum::internet_checksum;

                            let mut frame = std::mem::take(&mut self.tx_partial);
                            if (cmd & TXD_CMD_IC) != 0 && css < frame.len() && cso + 2 <= frame.len() {
                                frame[cso..cso + 2].fill(0);
                                let csum = internet_checksum(&frame[css..]);
                                frame[cso..cso + 2].copy_from_slice(&csum.to_be_bytes());
                            }

                            self.tx_out.push_back(frame);
                        }
                    }
                }
                TxDescriptor::Data(desc) => {
                    match self.tx_state {
                        None => {
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                        }
                        Some(TxPacketState::Advanced {
                            ref mut cmd,
                            ref mut popts,
                        }) => {
                            *cmd |= desc.cmd;
                            *popts |= desc.popts;
                        }
                        Some(TxPacketState::Legacy { .. }) => {
                            self.tx_partial.clear();
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                        }
                    }

                    if desc.buffer_addr != 0 && desc.length != 0 {
                        let mut buf = vec![0u8; desc.length as usize];
                        mem.read_physical(desc.buffer_addr, &mut buf);
                        self.tx_partial.extend_from_slice(&buf);
                    }

                    desc_bytes[12] |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc_bytes);

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        let Some(TxPacketState::Advanced { cmd, popts }) = self.tx_state.take() else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            self.tdh = (self.tdh + 1) % desc_count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            let flags = TxChecksumFlags::from_popts(popts);
                            let mut frame = std::mem::take(&mut self.tx_partial);

                            if (cmd & TXD_CMD_TSE) != 0 {
                                match tso_segment(&frame, self.tx_ctx, flags) {
                                    Ok(frames) => {
                                        for frame in frames {
                                            self.tx_out.push_back(frame);
                                        }
                                    }
                                    Err(_) => {
                                        let _ = apply_checksum_offload(&mut frame, self.tx_ctx, flags);
                                        self.tx_out.push_back(frame);
                                    }
                                }
                            } else {
                                let _ = apply_checksum_offload(&mut frame, self.tx_ctx, flags);
                                self.tx_out.push_back(frame);
                            }
                        }
                    }
                }
            }

            self.tdh = (self.tdh + 1) % desc_count;
        }

        if should_raise_txdw {
            self.icr |= ICR_TXDW;
            self.update_irq_level();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMem {
        mem: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self {
                mem: vec![0u8; size],
            }
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

    #[test]
    fn tx_desc_roundtrip() {
        let desc = TxDesc {
            buffer_addr: 0x1122_3344_5566_7788,
            length: 1514,
            cso: 3,
            cmd: 0xA5,
            status: 0x5A,
            css: 7,
            special: 0xBEEF,
        };
        let bytes = desc.to_bytes();
        assert_eq!(TxDesc::from_bytes(bytes), desc);
    }

    #[test]
    fn rx_desc_roundtrip() {
        let desc = RxDesc {
            buffer_addr: 0xDEAD_BEEF_CAFE_BABE,
            length: 512,
            checksum: 0x1234,
            status: 0x7F,
            errors: 0x01,
            special: 0x2222,
        };
        let bytes = desc.to_bytes();
        assert_eq!(RxDesc::from_bytes(bytes), desc);
    }

    #[test]
    fn tx_processing_emits_frame_and_sets_dd() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Set up TX ring at 0x1000 with 4 descriptors.
        dev.tdbal = 0x1000;
        dev.tdlen = (TxDesc::LEN as u32) * 4;
        dev.tdh = 0;
        dev.tdt = 0;
        dev.tctl = TCTL_EN;
        dev.ims = ICR_TXDW;

        // Packet buffer at 0x2000.
        let pkt = b"hello";
        mem.write_bytes(0x2000, pkt);

        let desc0 = TxDesc {
            buffer_addr: 0x2000,
            length: pkt.len() as u16,
            cso: 0,
            cmd: TXD_CMD_EOP | TXD_CMD_RS,
            status: 0,
            css: 0,
            special: 0,
        };
        mem.write_bytes(0x1000, &desc0.to_bytes());

        // Guest updates tail to 1.
        dev.mmio_write_u32(&mut mem, REG_TDT, 1);

        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));

        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_ne!(updated.status & TXD_STAT_DD, 0);

        assert!(dev.irq_level());
        let icr = dev.mmio_read_u32(REG_ICR);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
        assert!(!dev.irq_level());
    }

    #[test]
    fn rx_processing_writes_frame_and_sets_dd() {
        let mut mem = TestMem::new(0x20_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // RX ring at 0x3000 with 2 descriptors.
        dev.rdbal = 0x3000;
        dev.rdlen = (RxDesc::LEN as u32) * 2;
        dev.rdh = 0;
        dev.rdt = 1;
        dev.rctl = RCTL_EN;
        dev.ims = ICR_RXT0;

        // Two receive buffers at 0x4000, 0x5000.
        let desc0 = RxDesc {
            buffer_addr: 0x4000,
            length: 0,
            checksum: 0,
            status: 0,
            errors: 0,
            special: 0,
        };
        let desc1 = RxDesc {
            buffer_addr: 0x5000,
            ..desc0
        };
        mem.write_bytes(0x3000, &desc0.to_bytes());
        mem.write_bytes(0x3010, &desc1.to_bytes());

        let frame = b"frame-data";
        dev.receive_frame(&mut mem, frame);

        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);
        let updated = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(updated.length as usize, frame.len());
        assert_eq!(
            updated.status & (RXD_STAT_DD | RXD_STAT_EOP),
            RXD_STAT_DD | RXD_STAT_EOP
        );

        assert!(dev.irq_level());
        let icr = dev.mmio_read_u32(REG_ICR);
        assert_eq!(icr & ICR_RXT0, ICR_RXT0);
        assert!(!dev.irq_level());
    }
}
