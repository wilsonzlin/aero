//! E1000 (Intel 82540EM-ish) virtual NIC model.
//!
//! This crate intentionally models only the subset of the device required for
//! basic Windows 7 networking driver bring-up:
//! - Basic PCI config space (vendor/device IDs + BAR0/BAR1 probing/programming)
//! - MMIO register interface for init, RX/TX rings, and interrupts
//! - Legacy RX/TX descriptor rings with DMA read/write via [`memory::MemoryBus`]
//! - Simple host-facing frame queues
//! - `memory::MmioHandler`-style integration: MMIO/PIO register access is split from DMA; DMA
//!   happens only in [`E1000Device::poll`]
//!
//! The implementation aims to be "good enough" for driver compatibility
//! without chasing every obscure corner case of real silicon.

use std::collections::{HashMap, VecDeque};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use memory::{MemoryBus, MmioHandler};

mod offload;

use offload::{apply_checksum_offload, tso_segment, TxChecksumFlags, TxOffloadContext};

/// Size of the E1000 MMIO BAR.
pub const E1000_MMIO_SIZE: u32 = 0x20_000;
/// Size of the E1000 I/O BAR (IOADDR/IODATA window).
pub const E1000_IO_SIZE: u32 = 0x40;

/// Minimum Ethernet frame length: destination MAC (6) + source MAC (6) + ethertype (2).
pub const MIN_L2_FRAME_LEN: usize = 14;
/// Maximum Ethernet frame length accepted by the device model (no FCS).
///
/// We allow a small amount above the usual 1514-byte "Ethernet header + 1500 MTU"
/// to tolerate VLAN-tagged frames and occasional capture sources that include
/// extra bytes. Jumbo frames are intentionally not supported.
pub const MAX_L2_FRAME_LEN: usize = 1522;
/// Upper bound for a single in-progress guest TX packet being assembled from descriptors.
///
/// This must be large enough for common Windows offloads (TSO can exceed 64KiB),
/// but finite so a malicious guest cannot cause unbounded allocations.
pub const MAX_TX_AGGREGATE_LEN: usize = 256 * 1024;
/// Upper bound on the number of TSO segments that may be produced from a single packet.
pub const MAX_TSO_SEGMENTS: usize = 256;
/// Upper bound for the host-facing TX output queue.
pub const MAX_TX_OUT_QUEUE: usize = 256;

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
const RXD_ERR_RXE: u8 = 1 << 7;

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
    bar1: u32,
    bar1_probe: bool,
}

impl Default for PciConfig {
    fn default() -> Self {
        Self::new()
    }
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
            bar1: 0x1, // I/O BAR indicator bit set.
            bar1_probe: false,
        }
    }

    pub fn bar0(&self) -> u32 {
        self.bar0
    }

    pub fn bar1(&self) -> u32 {
        self.bar1
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
                if offset == 0x14 {
                    return if self.bar1_probe {
                        // I/O BAR: bit0 must remain set.
                        (!(E1000_IO_SIZE - 1) & 0xffff_fffc) | 0x1
                    } else {
                        self.bar1
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
/// - BAR1 I/O via [`io_read`] / [`io_write`]
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
    io_reg: u32,

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
    tx_drop: bool,
    tx_ctx: TxOffloadContext,
    tx_state: Option<TxPacketState>,

    mac_addr: [u8; 6],
    ra_valid: bool,
    eeprom: [u16; 64],
    phy: [u16; 32],
    other_regs: HashMap<u32, u32>,

    rx_pending: VecDeque<Vec<u8>>,
    tx_out: VecDeque<Vec<u8>>,

    // Work flags set by register-only MMIO/PIO writes.
    //
    // These exist so that `poll()` (the only DMA-capable entrypoint) can cheaply
    // skip work when no doorbell-like register writes have occurred.
    tx_needs_poll: bool,
    rx_needs_flush: bool,
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
            mdic: MDIC_READY,
            io_reg: 0,
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
            tx_drop: false,
            tx_ctx: TxOffloadContext::default(),
            tx_state: None,
            mac_addr,
            ra_valid: true,
            eeprom: [0xFFFF; 64],
            phy: [0; 32],
            other_regs: HashMap::new(),
            rx_pending: VecDeque::new(),
            tx_out: VecDeque::new(),
            tx_needs_poll: false,
            rx_needs_flush: false,
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

    /// Register-only MMIO write path.
    ///
    /// This updates internal device state but **must not** touch guest memory.
    /// DMA side effects (descriptor reads/writes, RX buffer writes, etc.) are
    /// deferred to [`poll`].
    pub fn mmio_write_reg(&mut self, offset: u64, size: usize, value: u32) {
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

        self.mmio_write_u32_aligned_reg(aligned, merged);
    }

    /// MMIO write path that preserves legacy semantics: register write + immediate DMA.
    ///
    /// This is a thin compatibility wrapper around [`mmio_write_reg`] + [`poll`].
    pub fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        self.mmio_write_reg(offset, size, value);
        self.poll(mem);
    }

    pub fn mmio_read_u32(&mut self, offset: u32) -> u32 {
        self.mmio_read(offset as u64, 4)
    }

    pub fn mmio_write_u32_reg(&mut self, offset: u32, value: u32) {
        self.mmio_write_reg(offset as u64, 4, value);
    }

    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u32, value: u32) {
        self.mmio_write(mem, offset as u64, 4, value);
    }

    /// Read from the device's I/O BAR (IOADDR/IODATA window).
    pub fn io_read(&mut self, offset: u32, size: usize) -> u32 {
        match offset {
            // IOADDR (selected MMIO register offset).
            0x0..=0x3 => {
                let shift = (offset & 3) * 8;
                match size {
                    4 => self.io_reg,
                    2 => (self.io_reg >> shift) & 0xffff,
                    1 => (self.io_reg >> shift) & 0xff,
                    _ => 0,
                }
            }
            // IODATA (MMIO window to the selected register).
            0x4..=0x7 => self.mmio_read((self.io_reg + (offset - 0x4)) as u64, size),
            _ => 0,
        }
    }

    /// Write to the device's I/O BAR (IOADDR/IODATA window).
    ///
    /// This register-only variant updates the IOADDR latch and the targeted MMIO register but
    /// does **not** touch guest memory. DMA side effects are deferred to [`poll`].
    pub fn io_write_reg(&mut self, offset: u32, size: usize, value: u32) {
        match offset {
            0x0..=0x3 => {
                let shift = (offset & 3) * 8;
                if size == 4 {
                    self.io_reg = value & !3;
                    return;
                }

                let mask = match size {
                    2 => 0xffffu32 << shift,
                    1 => 0xffu32 << shift,
                    _ => 0,
                };
                let cur = self.io_reg;
                self.io_reg = ((cur & !mask) | ((value << shift) & mask)) & !3;
            }
            0x4..=0x7 => {
                self.mmio_write_reg((self.io_reg + (offset - 0x4)) as u64, size, value);
            }
            _ => {}
        }
    }

    /// Write to the device's I/O BAR (IOADDR/IODATA window) with immediate DMA.
    ///
    /// This is a compatibility wrapper around [`io_write_reg`] + [`poll`].
    pub fn io_write(&mut self, mem: &mut dyn MemoryBus, offset: u32, size: usize, value: u32) {
        self.io_write_reg(offset, size, value);
        self.poll(mem);
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        if self.tx_needs_poll || self.tx_work_pending() {
            self.process_tx(mem);
            self.tx_needs_poll = self.tx_work_pending();
        }

        if self.rx_needs_flush || !self.rx_pending.is_empty() {
            self.flush_rx_pending(mem);
            self.rx_needs_flush = !self.rx_pending.is_empty();
        }
    }

    /// Queue a host→guest Ethernet frame for later delivery.
    ///
    /// The caller is expected to invoke [`poll`] (or [`receive_frame`]) to flush
    /// pending frames into the RX descriptor ring when buffers are available.
    pub fn enqueue_rx_frame(&mut self, frame: Vec<u8>) {
        if frame.len() < MIN_L2_FRAME_LEN || frame.len() > MAX_L2_FRAME_LEN {
            return;
        }
        // Keep memory bounded even if the guest never enables RX.
        const MAX_PENDING: usize = 256;
        if self.rx_pending.len() >= MAX_PENDING {
            self.rx_pending.pop_front();
        }
        self.rx_pending.push_back(frame);
        self.rx_needs_flush = true;
    }

    /// Host → guest path.
    ///
    /// Frames are queued and then copied into guest RX buffers when the guest
    /// has enabled reception and made descriptors available.
    pub fn receive_frame(&mut self, mem: &mut dyn MemoryBus, frame: &[u8]) {
        self.enqueue_rx_frame(frame.to_vec());
        self.poll(mem);
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
        self.mdic = MDIC_READY;
        self.io_reg = 0;

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
        self.tx_drop = false;
        self.tx_ctx = TxOffloadContext::default();
        self.tx_state = None;

        self.other_regs.clear();
        self.rx_pending.clear();
        self.tx_out.clear();

        self.ra_valid = true;
        self.init_eeprom_from_mac();
        self.init_phy();

        self.tx_needs_poll = false;
        self.rx_needs_flush = false;
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.icr & self.ims) != 0;
    }

    fn tx_work_pending(&self) -> bool {
        match self.tx_ring_desc_count() {
            Some(desc_count) if desc_count != 0 => self.tdh % desc_count != self.tdt % desc_count,
            _ => self.tdh != self.tdt,
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
                if self.ra_valid {
                    v |= 1u32 << 31; // AV bit
                }
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

    fn mmio_write_u32_aligned_reg(&mut self, offset: u32, value: u32) {
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
                self.rx_needs_flush = true;
            }
            REG_TCTL => self.tctl = value,

            REG_RDBAL => self.rdbal = value,
            REG_RDBAH => self.rdbah = value,
            REG_RDLEN => self.rdlen = value,
            REG_RDH => self.rdh = value,
            REG_RDT => {
                self.rdt = value;
                self.rx_needs_flush = true;
            }

            REG_TDBAL => self.tdbal = value,
            REG_TDBAH => self.tdbah = value,
            REG_TDLEN => self.tdlen = value,
            REG_TDH => self.tdh = value,
            REG_TDT => {
                self.tdt = value;
                self.tx_needs_poll = true;
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
                self.ra_valid = (value & (1 << 31)) != 0;
                self.init_eeprom_from_mac();
            }
            _ => {
                self.other_regs.insert(offset, value);
            }
        }
    }

    fn rx_ring_desc_count(&self) -> Option<u32> {
        let desc_len = RxDesc::LEN as u32;
        if self.rdlen < desc_len || !self.rdlen.is_multiple_of(desc_len) {
            return None;
        }
        Some(self.rdlen / desc_len)
    }

    fn tx_ring_desc_count(&self) -> Option<u32> {
        let desc_len = TxDesc::LEN as u32;
        if self.tdlen < desc_len || !self.tdlen.is_multiple_of(desc_len) {
            return None;
        }
        Some(self.tdlen / desc_len)
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
        let mut head = self.rdh % desc_count;
        let tail = self.rdt % desc_count;

        while let Some(frame) = self.rx_pending.front() {
            if frame.len() < MIN_L2_FRAME_LEN || frame.len() > MAX_L2_FRAME_LEN {
                // Should be filtered at enqueue time, but keep this resilient to
                // callers bypassing `enqueue_rx_frame`.
                self.rx_pending.pop_front();
                continue;
            }
            // The hardware head (RDH) must not catch up to the software tail (RDT).
            // Keep one descriptor unused to avoid ambiguity in full/empty conditions.
            if head == tail {
                break;
            }
            let idx = head as u64;
            let desc_addr = base + idx * RxDesc::LEN as u64;
            let desc_bytes = read_desc::<{ RxDesc::LEN }>(mem, desc_addr);
            let mut desc = RxDesc::from_bytes(desc_bytes);

            if desc.buffer_addr == 0 {
                // Driver hasn't set up this descriptor; stop.
                break;
            }

            if buf_len < frame.len() {
                // Avoid delivering truncated frames; drop and surface an error.
                desc.length = 0;
                desc.checksum = 0;
                desc.errors = RXD_ERR_RXE;
                desc.status = RXD_STAT_DD | RXD_STAT_EOP;
                write_desc(mem, desc_addr, &desc.to_bytes());
            } else {
                mem.write_physical(desc.buffer_addr, frame);

                desc.length = frame.len() as u16;
                desc.checksum = 0;
                desc.errors = 0;
                desc.status = RXD_STAT_DD | RXD_STAT_EOP;
                write_desc(mem, desc_addr, &desc.to_bytes());
            }

            self.rx_pending.pop_front();

            head = (head + 1) % desc_count;

            self.icr |= ICR_RXT0;
            self.update_irq_level();
        }

        self.rdh = head;
    }

    fn queue_tx_frame(&mut self, frame: Vec<u8>) {
        if frame.len() < MIN_L2_FRAME_LEN || frame.len() > MAX_L2_FRAME_LEN {
            return;
        }
        if self.tx_out.len() >= MAX_TX_OUT_QUEUE {
            // Bound memory even if the host never drains the TX queue.
            self.tx_out.pop_front();
        }
        self.tx_out.push_back(frame);
    }

    fn enter_tx_drop_mode(&mut self) {
        self.tx_drop = true;
        self.tx_partial.clear();
        self.tx_state = None;
    }

    fn append_tx_data(&mut self, mem: &mut dyn MemoryBus, buffer_addr: u64, len: usize, tso: bool) {
        if self.tx_drop || buffer_addr == 0 || len == 0 {
            return;
        }

        let new_len = self
            .tx_partial
            .len()
            .checked_add(len)
            .unwrap_or(MAX_TX_AGGREGATE_LEN + 1);

        if new_len > MAX_TX_AGGREGATE_LEN || (!tso && new_len > MAX_L2_FRAME_LEN) {
            self.enter_tx_drop_mode();
            return;
        }

        let old_len = self.tx_partial.len();
        self.tx_partial.resize(new_len, 0);
        mem.read_physical(buffer_addr, &mut self.tx_partial[old_len..new_len]);
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
        let mut head = self.tdh % desc_count;
        let tail = self.tdt % desc_count;

        let mut should_raise_txdw = false;

        while head != tail {
            let idx = head as u64;
            let desc_addr = base + idx * TxDesc::LEN as u64;
            let mut desc_bytes = read_desc::<{ TxDesc::LEN }>(mem, desc_addr);

            let Some(desc) = TxDescriptor::parse(desc_bytes) else {
                // Unknown descriptor type; best-effort mark completion and move on.
                desc_bytes[12] |= TXD_STAT_DD;
                write_desc(mem, desc_addr, &desc_bytes);
                head = (head + 1) % desc_count;
                continue;
            };

            match desc {
                TxDescriptor::Context(ctx_desc) => {
                    self.tx_ctx = ctx_desc.into();

                    if (ctx_desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    // For advanced context descriptors, the DD bit lives in the
                    // same status location as advanced data descriptors (the
                    // low bits of dword 3). This overlaps with MSS; real
                    // hardware overwrites the context descriptor on completion
                    // and drivers only care about the DD bit.
                    desc_bytes[12] |= TXD_STAT_DD;
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
                        self.append_tx_data(mem, desc.buffer_addr, desc.length as usize, false);
                    }

                    desc.status |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc.to_bytes());

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        if self.tx_drop {
                            self.tx_drop = false;
                            self.tx_partial.clear();
                            self.tx_state = None;
                            head = (head + 1) % desc_count;
                            continue;
                        }

                        let Some(TxPacketState::Legacy { cmd, css, cso }) = self.tx_state.take()
                        else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            head = (head + 1) % desc_count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            use nt_packetlib::io::net::packet::checksum::internet_checksum;

                            let mut frame = std::mem::take(&mut self.tx_partial);
                            if (cmd & TXD_CMD_IC) != 0
                                && css < frame.len()
                                && cso + 2 <= frame.len()
                            {
                                frame[cso..cso + 2].fill(0);
                                let csum = internet_checksum(&frame[css..]);
                                frame[cso..cso + 2].copy_from_slice(&csum.to_be_bytes());
                            }

                            self.queue_tx_frame(frame);
                        }
                    }
                }
                TxDescriptor::Data(desc) => {
                    let cmd = match self.tx_state {
                        None => {
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                            desc.cmd
                        }
                        Some(TxPacketState::Advanced {
                            ref mut cmd,
                            ref mut popts,
                        }) => {
                            *cmd |= desc.cmd;
                            *popts |= desc.popts;
                            *cmd
                        }
                        Some(TxPacketState::Legacy { .. }) => {
                            self.tx_partial.clear();
                            self.tx_state = Some(TxPacketState::Advanced {
                                cmd: desc.cmd,
                                popts: desc.popts,
                            });
                            desc.cmd
                        }
                    };

                    let tso = (cmd & TXD_CMD_TSE) != 0;
                    if desc.buffer_addr != 0 && desc.length != 0 {
                        self.append_tx_data(mem, desc.buffer_addr, desc.length as usize, tso);
                    }

                    desc_bytes[12] |= TXD_STAT_DD;
                    write_desc(mem, desc_addr, &desc_bytes);

                    if (desc.cmd & TXD_CMD_RS) != 0 {
                        should_raise_txdw = true;
                    }

                    if (desc.cmd & TXD_CMD_EOP) != 0 {
                        if self.tx_drop {
                            self.tx_drop = false;
                            self.tx_partial.clear();
                            self.tx_state = None;
                            head = (head + 1) % desc_count;
                            continue;
                        }

                        let Some(TxPacketState::Advanced { cmd, popts }) = self.tx_state.take()
                        else {
                            self.tx_partial.clear();
                            self.tx_state = None;
                            head = (head + 1) % desc_count;
                            continue;
                        };

                        if !self.tx_partial.is_empty() {
                            let flags = TxChecksumFlags::from_popts(popts);
                            let mut frame = std::mem::take(&mut self.tx_partial);

                            if (cmd & TXD_CMD_TSE) != 0 {
                                match tso_segment(&frame, self.tx_ctx, flags) {
                                    Ok(frames) => {
                                        for frame in frames {
                                            self.queue_tx_frame(frame);
                                        }
                                    }
                                    Err(_) => {
                                        if (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN)
                                            .contains(&frame.len())
                                        {
                                            let _ = apply_checksum_offload(
                                                &mut frame,
                                                self.tx_ctx,
                                                flags,
                                            );
                                            self.queue_tx_frame(frame);
                                        }
                                    }
                                }
                            } else if (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&frame.len()) {
                                let _ = apply_checksum_offload(&mut frame, self.tx_ctx, flags);
                                self.queue_tx_frame(frame);
                            }
                        }
                    }
                }
            }

            head = (head + 1) % desc_count;
        }

        self.tdh = head;

        if should_raise_txdw {
            self.icr |= ICR_TXDW;
            self.update_irq_level();
        }
    }
}

impl IoSnapshot for E1000Device {
    const DEVICE_ID: [u8; 4] = *b"E1K0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI_REGS: u16 = 1;
        const TAG_PCI_BAR0: u16 = 2;
        const TAG_PCI_BAR0_PROBE: u16 = 3;
        const TAG_PCI_BAR1: u16 = 4;
        const TAG_PCI_BAR1_PROBE: u16 = 5;

        const TAG_CTRL: u16 = 10;
        const TAG_STATUS: u16 = 11;
        const TAG_EECD: u16 = 12;
        const TAG_EERD: u16 = 13;
        const TAG_CTRL_EXT: u16 = 14;
        const TAG_MDIC: u16 = 15;
        const TAG_IO_REG: u16 = 16;

        const TAG_ICR: u16 = 20;
        const TAG_IMS: u16 = 21;
        const TAG_IRQ_LEVEL: u16 = 22;

        const TAG_RCTL: u16 = 30;
        const TAG_TCTL: u16 = 31;

        const TAG_RDBAL: u16 = 40;
        const TAG_RDBAH: u16 = 41;
        const TAG_RDLEN: u16 = 42;
        const TAG_RDH: u16 = 43;
        const TAG_RDT: u16 = 44;

        const TAG_TDBAL: u16 = 50;
        const TAG_TDBAH: u16 = 51;
        const TAG_TDLEN: u16 = 52;
        const TAG_TDH: u16 = 53;
        const TAG_TDT: u16 = 54;

        const TAG_TX_PARTIAL: u16 = 60;
        const TAG_TX_DROP: u16 = 61;
        const TAG_TX_CTX_IPCSS: u16 = 62;
        const TAG_TX_CTX_IPCSO: u16 = 63;
        const TAG_TX_CTX_IPCSE: u16 = 64;
        const TAG_TX_CTX_TUCSS: u16 = 65;
        const TAG_TX_CTX_TUCSO: u16 = 66;
        const TAG_TX_CTX_TUCSE: u16 = 67;
        const TAG_TX_CTX_MSS: u16 = 68;
        const TAG_TX_CTX_HDR_LEN: u16 = 69;
        const TAG_TX_STATE_KIND: u16 = 70;
        const TAG_TX_STATE_CMD: u16 = 71;
        const TAG_TX_STATE_CSS: u16 = 72;
        const TAG_TX_STATE_CSO: u16 = 73;
        const TAG_TX_STATE_POPTS: u16 = 74;

        const TAG_MAC_ADDR: u16 = 80;
        const TAG_RA_VALID: u16 = 81;
        const TAG_EEPROM: u16 = 82;
        const TAG_PHY: u16 = 83;

        const TAG_OTHER_REGS: u16 = 90;
        const TAG_RX_PENDING: u16 = 91;
        const TAG_TX_OUT: u16 = 92;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_bytes(TAG_PCI_REGS, self.pci.regs.to_vec());
        w.field_u32(TAG_PCI_BAR0, self.pci.bar0);
        w.field_bool(TAG_PCI_BAR0_PROBE, self.pci.bar0_probe);
        w.field_u32(TAG_PCI_BAR1, self.pci.bar1);
        w.field_bool(TAG_PCI_BAR1_PROBE, self.pci.bar1_probe);

        w.field_u32(TAG_CTRL, self.ctrl);
        w.field_u32(TAG_STATUS, self.status);
        w.field_u32(TAG_EECD, self.eecd);
        w.field_u32(TAG_EERD, self.eerd);
        w.field_u32(TAG_CTRL_EXT, self.ctrl_ext);
        w.field_u32(TAG_MDIC, self.mdic);
        w.field_u32(TAG_IO_REG, self.io_reg);

        w.field_u32(TAG_ICR, self.icr);
        w.field_u32(TAG_IMS, self.ims);
        w.field_bool(TAG_IRQ_LEVEL, self.irq_level);

        w.field_u32(TAG_RCTL, self.rctl);
        w.field_u32(TAG_TCTL, self.tctl);

        w.field_u32(TAG_RDBAL, self.rdbal);
        w.field_u32(TAG_RDBAH, self.rdbah);
        w.field_u32(TAG_RDLEN, self.rdlen);
        w.field_u32(TAG_RDH, self.rdh);
        w.field_u32(TAG_RDT, self.rdt);

        w.field_u32(TAG_TDBAL, self.tdbal);
        w.field_u32(TAG_TDBAH, self.tdbah);
        w.field_u32(TAG_TDLEN, self.tdlen);
        w.field_u32(TAG_TDH, self.tdh);
        w.field_u32(TAG_TDT, self.tdt);

        w.field_bytes(TAG_TX_PARTIAL, self.tx_partial.clone());
        w.field_bool(TAG_TX_DROP, self.tx_drop);
        w.field_u32(TAG_TX_CTX_IPCSS, self.tx_ctx.ipcss as u32);
        w.field_u32(TAG_TX_CTX_IPCSO, self.tx_ctx.ipcso as u32);
        w.field_u32(TAG_TX_CTX_IPCSE, self.tx_ctx.ipcse as u32);
        w.field_u32(TAG_TX_CTX_TUCSS, self.tx_ctx.tucss as u32);
        w.field_u32(TAG_TX_CTX_TUCSO, self.tx_ctx.tucso as u32);
        w.field_u32(TAG_TX_CTX_TUCSE, self.tx_ctx.tucse as u32);
        w.field_u32(TAG_TX_CTX_MSS, self.tx_ctx.mss as u32);
        w.field_u32(TAG_TX_CTX_HDR_LEN, self.tx_ctx.hdr_len as u32);

        match self.tx_state {
            None => {
                w.field_u8(TAG_TX_STATE_KIND, 0);
            }
            Some(TxPacketState::Legacy { cmd, css, cso }) => {
                w.field_u8(TAG_TX_STATE_KIND, 1);
                w.field_u8(TAG_TX_STATE_CMD, cmd);
                w.field_u32(TAG_TX_STATE_CSS, css as u32);
                w.field_u32(TAG_TX_STATE_CSO, cso as u32);
            }
            Some(TxPacketState::Advanced { cmd, popts }) => {
                w.field_u8(TAG_TX_STATE_KIND, 2);
                w.field_u8(TAG_TX_STATE_CMD, cmd);
                w.field_u8(TAG_TX_STATE_POPTS, popts);
            }
        }

        w.field_bytes(TAG_MAC_ADDR, self.mac_addr.to_vec());
        w.field_bool(TAG_RA_VALID, self.ra_valid);

        let mut eeprom = Vec::with_capacity(self.eeprom.len() * 2);
        for v in self.eeprom {
            eeprom.extend_from_slice(&v.to_le_bytes());
        }
        w.field_bytes(TAG_EEPROM, eeprom);

        let mut phy = Vec::with_capacity(self.phy.len() * 2);
        for v in self.phy {
            phy.extend_from_slice(&v.to_le_bytes());
        }
        w.field_bytes(TAG_PHY, phy);

        let mut other: Vec<(u32, u32)> = self.other_regs.iter().map(|(&k, &v)| (k, v)).collect();
        other.sort_by_key(|(k, _)| *k);
        let mut other_enc = Encoder::new().u32(other.len() as u32);
        for (k, v) in other {
            other_enc = other_enc.u32(k).u32(v);
        }
        w.field_bytes(TAG_OTHER_REGS, other_enc.finish());

        let mut rx_enc = Encoder::new().u32(self.rx_pending.len() as u32);
        for frame in &self.rx_pending {
            rx_enc = rx_enc.u32(frame.len() as u32).bytes(frame);
        }
        w.field_bytes(TAG_RX_PENDING, rx_enc.finish());

        let mut tx_enc = Encoder::new().u32(self.tx_out.len() as u32);
        for frame in &self.tx_out {
            tx_enc = tx_enc.u32(frame.len() as u32).bytes(frame);
        }
        w.field_bytes(TAG_TX_OUT, tx_enc.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI_REGS: u16 = 1;
        const TAG_PCI_BAR0: u16 = 2;
        const TAG_PCI_BAR0_PROBE: u16 = 3;
        const TAG_PCI_BAR1: u16 = 4;
        const TAG_PCI_BAR1_PROBE: u16 = 5;

        const TAG_CTRL: u16 = 10;
        const TAG_STATUS: u16 = 11;
        const TAG_EECD: u16 = 12;
        const TAG_EERD: u16 = 13;
        const TAG_CTRL_EXT: u16 = 14;
        const TAG_MDIC: u16 = 15;
        const TAG_IO_REG: u16 = 16;

        const TAG_ICR: u16 = 20;
        const TAG_IMS: u16 = 21;
        const TAG_IRQ_LEVEL: u16 = 22;

        const TAG_RCTL: u16 = 30;
        const TAG_TCTL: u16 = 31;

        const TAG_RDBAL: u16 = 40;
        const TAG_RDBAH: u16 = 41;
        const TAG_RDLEN: u16 = 42;
        const TAG_RDH: u16 = 43;
        const TAG_RDT: u16 = 44;

        const TAG_TDBAL: u16 = 50;
        const TAG_TDBAH: u16 = 51;
        const TAG_TDLEN: u16 = 52;
        const TAG_TDH: u16 = 53;
        const TAG_TDT: u16 = 54;

        const TAG_TX_PARTIAL: u16 = 60;
        const TAG_TX_DROP: u16 = 61;
        const TAG_TX_CTX_IPCSS: u16 = 62;
        const TAG_TX_CTX_IPCSO: u16 = 63;
        const TAG_TX_CTX_IPCSE: u16 = 64;
        const TAG_TX_CTX_TUCSS: u16 = 65;
        const TAG_TX_CTX_TUCSO: u16 = 66;
        const TAG_TX_CTX_TUCSE: u16 = 67;
        const TAG_TX_CTX_MSS: u16 = 68;
        const TAG_TX_CTX_HDR_LEN: u16 = 69;
        const TAG_TX_STATE_KIND: u16 = 70;
        const TAG_TX_STATE_CMD: u16 = 71;
        const TAG_TX_STATE_CSS: u16 = 72;
        const TAG_TX_STATE_CSO: u16 = 73;
        const TAG_TX_STATE_POPTS: u16 = 74;

        const TAG_MAC_ADDR: u16 = 80;
        const TAG_RA_VALID: u16 = 81;
        const TAG_EEPROM: u16 = 82;
        const TAG_PHY: u16 = 83;

        const TAG_OTHER_REGS: u16 = 90;
        const TAG_RX_PENDING: u16 = 91;
        const TAG_TX_OUT: u16 = 92;

        const MAX_OTHER_REGS: usize = 65_536;
        const MAX_RX_PENDING: usize = 256;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let mut mac_addr = self.mac_addr();
        if let Some(mac) = r.bytes(TAG_MAC_ADDR) {
            if mac.len() != mac_addr.len() {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 mac"));
            }
            mac_addr.copy_from_slice(mac);
        }

        let mut dev = E1000Device::new(mac_addr);

        if let Some(pci_regs) = r.bytes(TAG_PCI_REGS) {
            if pci_regs.len() != dev.pci.regs.len() {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 pci regs"));
            }
            dev.pci.regs.copy_from_slice(pci_regs);
        }
        dev.pci.bar0 = r.u32(TAG_PCI_BAR0)?.unwrap_or(dev.pci.bar0);
        dev.pci.bar0_probe = r.bool(TAG_PCI_BAR0_PROBE)?.unwrap_or(dev.pci.bar0_probe);
        dev.pci.bar1 = r.u32(TAG_PCI_BAR1)?.unwrap_or(dev.pci.bar1);
        dev.pci.bar1_probe = r.bool(TAG_PCI_BAR1_PROBE)?.unwrap_or(dev.pci.bar1_probe);

        dev.ctrl = r.u32(TAG_CTRL)?.unwrap_or(dev.ctrl);
        dev.status = r.u32(TAG_STATUS)?.unwrap_or(dev.status);
        dev.eecd = r.u32(TAG_EECD)?.unwrap_or(dev.eecd);
        dev.eerd = r.u32(TAG_EERD)?.unwrap_or(dev.eerd);
        dev.ctrl_ext = r.u32(TAG_CTRL_EXT)?.unwrap_or(dev.ctrl_ext);
        dev.mdic = r.u32(TAG_MDIC)?.unwrap_or(dev.mdic);
        dev.io_reg = r.u32(TAG_IO_REG)?.unwrap_or(dev.io_reg);

        dev.icr = r.u32(TAG_ICR)?.unwrap_or(dev.icr);
        dev.ims = r.u32(TAG_IMS)?.unwrap_or(dev.ims);

        dev.rctl = r.u32(TAG_RCTL)?.unwrap_or(dev.rctl);
        dev.tctl = r.u32(TAG_TCTL)?.unwrap_or(dev.tctl);

        dev.rdbal = r.u32(TAG_RDBAL)?.unwrap_or(dev.rdbal);
        dev.rdbah = r.u32(TAG_RDBAH)?.unwrap_or(dev.rdbah);
        dev.rdlen = r.u32(TAG_RDLEN)?.unwrap_or(dev.rdlen);
        dev.rdh = r.u32(TAG_RDH)?.unwrap_or(dev.rdh);
        dev.rdt = r.u32(TAG_RDT)?.unwrap_or(dev.rdt);

        dev.tdbal = r.u32(TAG_TDBAL)?.unwrap_or(dev.tdbal);
        dev.tdbah = r.u32(TAG_TDBAH)?.unwrap_or(dev.tdbah);
        dev.tdlen = r.u32(TAG_TDLEN)?.unwrap_or(dev.tdlen);
        dev.tdh = r.u32(TAG_TDH)?.unwrap_or(dev.tdh);
        dev.tdt = r.u32(TAG_TDT)?.unwrap_or(dev.tdt);

        if let Some(buf) = r.bytes(TAG_TX_PARTIAL) {
            if buf.len() > MAX_TX_AGGREGATE_LEN {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_partial"));
            }
            dev.tx_partial = buf.to_vec();
        }
        dev.tx_drop = r.bool(TAG_TX_DROP)?.unwrap_or(dev.tx_drop);

        let ipcss = r.u32(TAG_TX_CTX_IPCSS)?.unwrap_or(dev.tx_ctx.ipcss as u32) as usize;
        let ipcso = r.u32(TAG_TX_CTX_IPCSO)?.unwrap_or(dev.tx_ctx.ipcso as u32) as usize;
        let ipcse = r.u32(TAG_TX_CTX_IPCSE)?.unwrap_or(dev.tx_ctx.ipcse as u32) as usize;
        let tucss = r.u32(TAG_TX_CTX_TUCSS)?.unwrap_or(dev.tx_ctx.tucss as u32) as usize;
        let tucso = r.u32(TAG_TX_CTX_TUCSO)?.unwrap_or(dev.tx_ctx.tucso as u32) as usize;
        let tucse = r.u32(TAG_TX_CTX_TUCSE)?.unwrap_or(dev.tx_ctx.tucse as u32) as usize;
        let mss = r.u32(TAG_TX_CTX_MSS)?.unwrap_or(dev.tx_ctx.mss as u32) as usize;
        let hdr_len = r
            .u32(TAG_TX_CTX_HDR_LEN)?
            .unwrap_or(dev.tx_ctx.hdr_len as u32) as usize;

        for v in [ipcss, ipcso, ipcse, tucss, tucso, tucse, mss, hdr_len] {
            if v > MAX_TX_AGGREGATE_LEN {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_ctx"));
            }
        }

        dev.tx_ctx = TxOffloadContext {
            ipcss,
            ipcso,
            ipcse,
            tucss,
            tucso,
            tucse,
            mss,
            hdr_len,
        };

        dev.tx_state = match r.u8(TAG_TX_STATE_KIND)?.unwrap_or(0) {
            0 => None,
            1 => {
                let cmd = r.u8(TAG_TX_STATE_CMD)?.unwrap_or(0);
                let css = r.u32(TAG_TX_STATE_CSS)?.unwrap_or(0) as usize;
                let cso = r.u32(TAG_TX_STATE_CSO)?.unwrap_or(0) as usize;
                if css > MAX_TX_AGGREGATE_LEN || cso > MAX_TX_AGGREGATE_LEN {
                    return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_state"));
                }
                Some(TxPacketState::Legacy { cmd, css, cso })
            }
            2 => {
                let cmd = r.u8(TAG_TX_STATE_CMD)?.unwrap_or(0);
                let popts = r.u8(TAG_TX_STATE_POPTS)?.unwrap_or(0);
                Some(TxPacketState::Advanced { cmd, popts })
            }
            _ => return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_state kind")),
        };

        dev.ra_valid = r.bool(TAG_RA_VALID)?.unwrap_or(dev.ra_valid);

        if let Some(buf) = r.bytes(TAG_EEPROM) {
            if buf.len() != dev.eeprom.len() * 2 {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 eeprom"));
            }
            for (slot, chunk) in dev.eeprom.iter_mut().zip(buf.chunks_exact(2)) {
                *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }

        if let Some(buf) = r.bytes(TAG_PHY) {
            if buf.len() != dev.phy.len() * 2 {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 phy"));
            }
            for (slot, chunk) in dev.phy.iter_mut().zip(buf.chunks_exact(2)) {
                *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }

        dev.other_regs.clear();
        if let Some(buf) = r.bytes(TAG_OTHER_REGS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_OTHER_REGS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "e1000 other_regs count",
                ));
            }
            for _ in 0..count {
                let key = d.u32()?;
                let value = d.u32()?;
                if dev.other_regs.insert(key, value).is_some() {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "e1000 other_regs duplicate key",
                    ));
                }
            }
            d.finish()?;
        }

        dev.rx_pending.clear();
        if let Some(buf) = r.bytes(TAG_RX_PENDING) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_RX_PENDING {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "e1000 rx_pending count",
                ));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if !(MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&len) {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "e1000 rx_pending frame",
                    ));
                }
                dev.rx_pending.push_back(d.bytes(len)?.to_vec());
            }
            d.finish()?;
        }

        dev.tx_out.clear();
        if let Some(buf) = r.bytes(TAG_TX_OUT) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_TX_OUT_QUEUE {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_out count"));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if !(MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&len) {
                    return Err(SnapshotError::InvalidFieldEncoding("e1000 tx_out frame"));
                }
                dev.tx_out.push_back(d.bytes(len)?.to_vec());
            }
            d.finish()?;
        }

        // Validate ring indices to avoid getting stuck in `process_tx`/`flush_rx_pending` after
        // restoring a corrupted snapshot.
        if let Some(desc_count) = dev.tx_ring_desc_count() {
            if desc_count == 0 || dev.tdh >= desc_count || dev.tdt >= desc_count {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 tx ring indices"));
            }
        }
        if let Some(desc_count) = dev.rx_ring_desc_count() {
            if desc_count == 0 || dev.rdh >= desc_count || dev.rdt >= desc_count {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 rx ring indices"));
            }
        }

        dev.update_irq_level();
        if let Some(saved_irq) = r.bool(TAG_IRQ_LEVEL)? {
            if saved_irq != dev.irq_level {
                return Err(SnapshotError::InvalidFieldEncoding("e1000 irq_level"));
            }
        }

        // Derive DMA work flags from restored state.
        dev.tx_needs_poll = dev.tx_work_pending();
        dev.rx_needs_flush = !dev.rx_pending.is_empty();

        *self = dev;
        Ok(())
    }
}

impl MmioHandler for E1000Device {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match size {
            1 | 2 | 4 => self.mmio_read(offset, size) as u64,
            8 => {
                let lo = self.mmio_read(offset, 4) as u64;
                let hi = self.mmio_read(offset + 4, 4) as u64;
                lo | (hi << 32)
            }
            _ => {
                let mut out = 0u64;
                for i in 0..size.min(8) {
                    let byte = self.mmio_read(offset + i as u64, 1) as u64 & 0xff;
                    out |= byte << (i * 8);
                }
                out
            }
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        match size {
            1 | 2 | 4 => self.mmio_write_reg(offset, size, value as u32),
            8 => {
                self.mmio_write_reg(offset, 4, value as u32);
                self.mmio_write_reg(offset + 4, 4, (value >> 32) as u32);
            }
            _ => {
                let bytes = value.to_le_bytes();
                for (i, byte) in bytes.iter().copied().enumerate().take(size.min(8)) {
                    self.mmio_write_reg(offset + i as u64, 1, u32::from(byte));
                }
            }
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

    struct LimitedReadMem {
        mem: Vec<u8>,
        max_read_len: usize,
    }

    impl LimitedReadMem {
        fn new(size: usize, max_read_len: usize) -> Self {
            Self {
                mem: vec![0u8; size],
                max_read_len,
            }
        }

        fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
            let addr = addr as usize;
            self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl MemoryBus for LimitedReadMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            assert!(
                buf.len() <= self.max_read_len,
                "unexpected large DMA read: {} bytes",
                buf.len()
            );
            let addr = paddr as usize;
            buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let addr = paddr as usize;
            self.mem[addr..addr + buf.len()].copy_from_slice(buf);
        }
    }

    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected DMA read");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected DMA write");
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
    fn mdic_starts_ready() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        assert_ne!(dev.mmio_read_u32(REG_MDIC) & MDIC_READY, 0);
    }

    #[test]
    fn ioaddr_iodata_interface_maps_to_mmio_registers() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        dev.mmio_write_u32_reg(REG_IMS, 0x1234_5678);

        dev.io_write_reg(0x0, 4, REG_IMS);
        assert_eq!(dev.io_read(0x4, 4), 0x1234_5678);

        dev.io_write_reg(0x0, 4, REG_IMC);
        dev.io_write_reg(0x4, 4, 0x1234_0000);
        assert_eq!(dev.mmio_read_u32(REG_IMS), 0x0000_5678);
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
        let pkt = [0x11u8; MIN_L2_FRAME_LEN];
        mem.write_bytes(0x2000, &pkt);

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
        dev.mmio_write_u32_reg(REG_TDT, 1);
        dev.poll(&mut mem);

        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));

        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_ne!(updated.status & TXD_STAT_DD, 0);

        assert!(dev.irq_level());
        let icr = dev.mmio_read_u32(REG_ICR);
        assert_eq!(icr & ICR_TXDW, ICR_TXDW);
        assert!(!dev.irq_level());
    }

    #[test]
    fn tx_mmio_write_reg_defers_dma_until_poll() {
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
        let pkt = [0x11u8; MIN_L2_FRAME_LEN];
        mem.write_bytes(0x2000, &pkt);

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

        // Guest updates tail to 1 (register-only write, no DMA yet).
        dev.mmio_write_u32_reg(REG_TDT, 1);

        assert!(dev.pop_tx_frame().is_none());
        let unchanged = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_eq!(unchanged.status & TXD_STAT_DD, 0);

        // DMA happens once the device is polled.
        dev.poll(&mut mem);

        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));
        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_ne!(updated.status & TXD_STAT_DD, 0);
        assert!(dev.irq_level());
    }

    #[test]
    fn register_only_doorbells_do_not_touch_guest_memory_until_poll() {
        let mut mem = TestMem::new(0x40_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut panic_mem = PanicMem;

        // --- TX: REG_TDT doorbell sets pending work, but must not DMA until poll.
        dev.mmio_write_u32_reg(REG_TDBAL, 0x1000);
        dev.mmio_write_u32_reg(REG_TDLEN, (TxDesc::LEN as u32) * 4);
        dev.mmio_write_u32_reg(REG_TDH, 0);
        dev.mmio_write_u32_reg(REG_TDT, 0);
        dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

        let pkt = [0x11u8; MIN_L2_FRAME_LEN];
        mem.write_bytes(0x2000, &pkt);
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

        // Register-only doorbell.
        dev.mmio_write_u32_reg(REG_TDT, 1);

        // No DMA yet.
        assert!(dev.pop_tx_frame().is_none());
        let unchanged = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_eq!(unchanged.status & TXD_STAT_DD, 0);

        // Polling is the only place DMA should occur.
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.poll(&mut panic_mem);
        }));
        assert!(err.is_err());

        dev.poll(&mut mem);
        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));
        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_ne!(updated.status & TXD_STAT_DD, 0);

        // --- RX: REG_RCTL/REG_RDT must not DMA until poll.
        dev.mmio_write_u32_reg(REG_RDBAL, 0x3000);
        dev.mmio_write_u32_reg(REG_RDLEN, (RxDesc::LEN as u32) * 2);
        dev.mmio_write_u32_reg(REG_RDH, 0);
        dev.mmio_write_u32_reg(REG_RDT, 1);

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

        // Sentinel to detect unexpected writes.
        mem.write_bytes(0x4000, &[0x5a; 32]);

        let frame = vec![0x22u8; MIN_L2_FRAME_LEN];
        dev.enqueue_rx_frame(frame.clone());

        dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

        // No DMA yet.
        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);
        let unchanged = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(unchanged.status, 0);

        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.poll(&mut panic_mem);
        }));
        assert!(err.is_err());

        dev.poll(&mut mem);
        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);
        let updated = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(updated.length as usize, frame.len());
        assert_eq!(
            updated.status & (RXD_STAT_DD | RXD_STAT_EOP),
            RXD_STAT_DD | RXD_STAT_EOP
        );
    }

    #[test]
    fn tx_poll_does_not_spin_on_out_of_range_tdt() {
        // Regression test: if the guest writes an out-of-range tail pointer, the model must not
        // spin forever in `poll()`. Real drivers should never do this, but we still want bounded
        // behavior for robustness.
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        dev.tdbal = 0x1000;
        dev.tdlen = (TxDesc::LEN as u32) * 4;
        dev.tdh = 1;
        dev.tdt = 0x1_0000; // far beyond desc_count; tail wraps via modulo
        dev.tctl = TCTL_EN;

        // Empty descriptors are fine; DMA reads should be small.
        dev.poll(&mut mem);

        assert_eq!(dev.mmio_read_u32(REG_TDH), dev.tdt % 4);
    }

    #[test]
    fn tx_io_write_reg_defers_dma_until_poll() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Set up TX ring at 0x1000 with 4 descriptors.
        dev.tdbal = 0x1000;
        dev.tdlen = (TxDesc::LEN as u32) * 4;
        dev.tdh = 0;
        dev.tdt = 0;
        dev.tctl = TCTL_EN;

        // Packet buffer at 0x2000.
        let pkt = [0x11u8; MIN_L2_FRAME_LEN];
        mem.write_bytes(0x2000, &pkt);

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

        // Select TDT via IOADDR, then write tail via IODATA (register-only path).
        dev.io_write_reg(0x0, 4, REG_TDT);
        dev.io_write_reg(0x4, 4, 1);

        assert!(dev.pop_tx_frame().is_none());

        dev.poll(&mut mem);
        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));
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

        let frame = vec![0x22u8; MIN_L2_FRAME_LEN];
        dev.receive_frame(&mut mem, &frame);

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

    #[test]
    fn rx_enqueue_defers_dma_until_poll() {
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

        // Sentinel to detect unexpected writes.
        mem.write_bytes(0x4000, &[0x5a; 32]);

        let frame = vec![0x22u8; MIN_L2_FRAME_LEN];
        dev.enqueue_rx_frame(frame.clone());

        // No DMA occurs until poll.
        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);
        assert!(!dev.irq_level());

        dev.poll(&mut mem);

        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);
        let updated = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(
            updated.status & (RXD_STAT_DD | RXD_STAT_EOP),
            RXD_STAT_DD | RXD_STAT_EOP
        );
        assert!(dev.irq_level());
    }

    #[test]
    fn rx_mmio_write_reg_does_not_dma_on_rctl_enable_until_poll() {
        let mut mem = TestMem::new(0x20_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // RX ring at 0x3000 with 2 descriptors. Keep RX disabled initially.
        dev.mmio_write_u32_reg(REG_RDBAL, 0x3000);
        dev.mmio_write_u32_reg(REG_RDLEN, (RxDesc::LEN as u32) * 2);
        dev.mmio_write_u32_reg(REG_RDH, 0);
        dev.mmio_write_u32_reg(REG_RDT, 1);
        dev.mmio_write_u32_reg(REG_IMS, ICR_RXT0);

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

        // Sentinel to detect unexpected writes.
        mem.write_bytes(0x4000, &[0x5a; 32]);

        let frame = vec![0x22u8; MIN_L2_FRAME_LEN];
        dev.enqueue_rx_frame(frame.clone());

        // Enabling RCTL via register-only write must not DMA until poll().
        dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);
        let unchanged = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(unchanged.status, 0);
        assert!(!dev.irq_level());

        dev.poll(&mut mem);

        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);
        let updated = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(
            updated.status & (RXD_STAT_DD | RXD_STAT_EOP),
            RXD_STAT_DD | RXD_STAT_EOP
        );
        assert!(dev.irq_level());
    }

    #[test]
    fn rx_mmio_write_reg_does_not_dma_on_rdt_update_until_poll() {
        let mut mem = TestMem::new(0x20_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // RX ring at 0x3000 with 2 descriptors, but start with no available buffers (RDH == RDT).
        dev.mmio_write_u32_reg(REG_RDBAL, 0x3000);
        dev.mmio_write_u32_reg(REG_RDLEN, (RxDesc::LEN as u32) * 2);
        dev.mmio_write_u32_reg(REG_RDH, 0);
        dev.mmio_write_u32_reg(REG_RDT, 0);
        dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);
        dev.mmio_write_u32_reg(REG_IMS, ICR_RXT0);

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

        // Sentinel to detect unexpected writes.
        mem.write_bytes(0x4000, &[0x5a; 32]);

        let frame = vec![0x22u8; MIN_L2_FRAME_LEN];
        dev.enqueue_rx_frame(frame.clone());

        // Even with a pending frame, no DMA occurs because RDH == RDT.
        dev.poll(&mut mem);
        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);

        // Open up a buffer via register-only tail update; still should not DMA until poll().
        dev.mmio_write_u32_reg(REG_RDT, 1);
        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);
        assert!(!dev.irq_level());

        dev.poll(&mut mem);
        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);
        assert!(dev.irq_level());
    }

    #[test]
    fn tx_oversized_descriptor_drops_packet_without_large_dma_reads() {
        let mut mem = LimitedReadMem::new(0x20_000, 2048);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // TX ring at 0x1000 with 4 descriptors.
        dev.tdbal = 0x1000;
        dev.tdlen = (TxDesc::LEN as u32) * 4;
        dev.tdh = 0;
        dev.tdt = 0;
        dev.tctl = TCTL_EN;

        // Two-descriptor packet where the first descriptor claims an absurd length.
        // The device should enter drop mode and never attempt a huge DMA read.
        let desc0 = TxDesc {
            buffer_addr: 0x2000,
            length: u16::MAX,
            cso: 0,
            cmd: TXD_CMD_RS, // no EOP
            status: 0,
            css: 0,
            special: 0,
        };
        let desc1 = TxDesc {
            buffer_addr: 0x3000,
            length: 64,
            cso: 0,
            cmd: TXD_CMD_EOP | TXD_CMD_RS,
            status: 0,
            css: 0,
            special: 0,
        };
        mem.write_bytes(0x1000, &desc0.to_bytes());
        mem.write_bytes(0x1010, &desc1.to_bytes());

        // Guest updates tail to 2.
        dev.mmio_write_u32_reg(REG_TDT, 2);
        dev.poll(&mut mem);

        assert!(dev.pop_tx_frame().is_none());

        let updated0 = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        let updated1 = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1010));
        assert_ne!(updated0.status & TXD_STAT_DD, 0);
        assert_ne!(updated1.status & TXD_STAT_DD, 0);
        assert_eq!(dev.mmio_read_u32(REG_TDH), 2);

        assert!(!dev.tx_drop);
        assert!(dev.tx_partial.is_empty());
        assert!(dev.tx_state.is_none());
    }

    #[test]
    fn rx_drops_oversized_frame_without_touching_guest_memory() {
        let mut mem = TestMem::new(0x20_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // RX ring at 0x3000 with 2 descriptors.
        dev.rdbal = 0x3000;
        dev.rdlen = (RxDesc::LEN as u32) * 2;
        dev.rdh = 0;
        dev.rdt = 1;
        dev.rctl = RCTL_EN;
        dev.ims = ICR_RXT0;

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

        // Sentinel to detect unexpected writes.
        mem.write_bytes(0x4000, &[0x5a; 32]);

        let frame = vec![0u8; MAX_L2_FRAME_LEN + 1];
        dev.receive_frame(&mut mem, &frame);

        assert_eq!(mem.read_bytes(0x4000, 32), vec![0x5a; 32]);
        let updated0 = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_eq!(updated0.status, 0);
        assert_eq!(dev.mmio_read_u32(REG_RDH), 0);
        assert!(!dev.irq_level());
        assert_eq!(dev.mmio_read_u32(REG_ICR), 0);
    }
}
