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

use memory::{MemoryBus, MmioHandler};

mod offload;

use offload::{apply_checksum_offload, tso_segment, TxChecksumFlags, TxOffloadContext};

#[cfg(feature = "io-snapshot")]
use aero_io_snapshot::io::net::state::{E1000DeviceState, E1000TxContextState, E1000TxPacketState};

/// Size of the E1000 MMIO BAR.
pub const E1000_MMIO_SIZE: u32 = 0x20_000;
/// Size of the E1000 I/O BAR (IOADDR/IODATA window).
pub const E1000_IO_SIZE: u32 = 0x40;

const E1000_BAR0_ADDR_MASK: u32 = (!(E1000_MMIO_SIZE - 1)) & 0xffff_fff0;
const E1000_BAR1_ADDR_MASK: u32 = (!(E1000_IO_SIZE - 1)) & 0xffff_fffc;

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
/// Upper bound on the number of TX descriptors that may be processed in a single [`E1000Device::poll`]
/// call.
///
/// The E1000 device model executes DMA in host-driven polling loops. Without an explicit bound here,
/// a malicious guest could program an extremely large descriptor ring and force the host to spend an
/// unbounded amount of time walking descriptors in a single poll call.
///
/// We pick a large value so normal guests/drivers still make rapid progress, while ensuring
/// deterministic upper bounds for callers that invoke [`E1000Device::poll`] from latency-sensitive
/// host runtimes (e.g. browser workers).
pub const MAX_TX_DESCS_PER_POLL: u32 = 4096;

/// Upper bound on the number of RX/TX descriptors the device model will accept.
///
/// This is primarily a robustness limit: in real hardware the ring lengths are bounded, but a
/// malicious guest (or a corrupted snapshot) could otherwise configure an absurdly large ring and
/// cause `poll()` to iterate an unbounded number of descriptors.
///
/// Keep this value in sync with the snapshot decoder limits in
/// `aero_io_snapshot::io::net::state::E1000DeviceState`.
const MAX_RING_DESC_COUNT: u32 = 65_536;

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

        // BAR1 is an I/O BAR: bit0 is always set.
        regs[0x14..0x18].copy_from_slice(&0x1u32.to_le_bytes());

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

    fn is_read_only_byte(addr: usize) -> bool {
        // Identity / header bytes are read-only on real PCI hardware.
        if addr < 0x04 {
            return true;
        }
        // Revision ID + Class Code bytes.
        if (0x08..=0x0B).contains(&addr) {
            return true;
        }
        // Header Type.
        if addr == 0x0E {
            return true;
        }
        // Subsystem IDs.
        if (0x2C..0x30).contains(&addr) {
            return true;
        }
        // Interrupt Pin.
        if addr == 0x3D {
            return true;
        }
        false
    }

    pub fn read(&self, offset: u16, size: usize) -> u32 {
        let offset = offset as usize;
        if offset
            .checked_add(size)
            .is_none_or(|end| end > self.regs.len())
        {
            return 0;
        }

        // BAR0/BAR1 are defined as full 32-bit registers in PCI config space, but some access
        // mechanisms perform byte/word reads (including unaligned reads that may straddle the
        // BAR0/BAR1 boundary). The device model stores BAR values in both decoded fields
        // (`bar0`/`bar1` + probe flags) and the raw `regs` array; ensure probe behavior and decoded
        // BAR values are observable regardless of access width or alignment.
        let read_byte = |off: usize| -> u8 {
            if (0x10..0x14).contains(&off) {
                let full = if self.bar0_probe {
                    E1000_BAR0_ADDR_MASK
                } else {
                    self.bar0
                };
                let shift = ((off - 0x10) * 8) as u32;
                ((full >> shift) & 0xff) as u8
            } else if (0x14..0x18).contains(&off) {
                let full = if self.bar1_probe {
                    // I/O BAR: bit0 must remain set.
                    E1000_BAR1_ADDR_MASK | 0x1
                } else {
                    self.bar1
                };
                let shift = ((off - 0x14) * 8) as u32;
                ((full >> shift) & 0xff) as u8
            } else {
                self.regs[off]
            }
        };

        match size {
            1 => read_byte(offset) as u32,
            2 => u16::from_le_bytes([read_byte(offset), read_byte(offset + 1)]) as u32,
            4 => u32::from_le_bytes([
                read_byte(offset),
                read_byte(offset + 1),
                read_byte(offset + 2),
                read_byte(offset + 3),
            ]),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
        let offset = offset as usize;
        if offset
            .checked_add(size)
            .is_none_or(|end| end > self.regs.len())
        {
            return;
        }

        // If a multi-byte write overlaps the BAR0/BAR1 dwords but isn't fully contained within a
        // single BAR, split into byte writes so both decoded BAR fields remain coherent.
        //
        // This matters for edge-case unaligned config space writes (e.g. 16-bit at 0x13, which
        // touches BAR0 high byte + BAR1 low byte).
        if (size == 2 || size == 4) && offset < 0x18 && offset + size > 0x10 {
            let within_bar0 = offset >= 0x10 && offset + size <= 0x14;
            let within_bar1 = offset >= 0x14 && offset + size <= 0x18;
            if !within_bar0 && !within_bar1 {
                for i in 0..size {
                    let byte = (value >> (i * 8)) & 0xff;
                    self.write((offset + i) as u16, 1, byte);
                }
                return;
            }
        }

        match size {
            1 | 2 => {
                // BARs are conceptually 32-bit registers, but some guests may perform byte/word
                // writes. Keep the decoded BAR fields coherent by reassembling a 32-bit value and
                // updating the decoded BAR fields + raw bytes.
                //
                // Note: BAR size probing is only triggered by a full 32-bit aligned write of
                // `0xFFFF_FFFF`. Real hardware does *not* treat sub-dword writes that happen to
                // synthesize `0xFFFF_FFFF` via byte-enables as a probe.
                if offset >= 0x10 && offset + size <= 0x14 {
                    let shift = ((offset - 0x10) * 8) as u32;
                    let mask = match size {
                        2 => 0xffffu32 << shift,
                        1 => 0xffu32 << shift,
                        _ => 0,
                    };
                    let cur = self.read_u32_raw(0x10);
                    let val = match size {
                        2 => (value & 0xffff) << shift,
                        1 => (value & 0xff) << shift,
                        _ => 0,
                    };
                    let new_raw = (cur & !mask) | (val & mask);

                    self.bar0_probe = false;
                    self.bar0 = new_raw & E1000_BAR0_ADDR_MASK;
                    self.write_u32_raw(0x10, self.bar0);
                    return;
                }
                if offset >= 0x14 && offset + size <= 0x18 {
                    let shift = ((offset - 0x14) * 8) as u32;
                    let mask = match size {
                        2 => 0xffffu32 << shift,
                        1 => 0xffu32 << shift,
                        _ => 0,
                    };
                    let cur = self.read_u32_raw(0x14);
                    let val = match size {
                        2 => (value & 0xffff) << shift,
                        1 => (value & 0xff) << shift,
                        _ => 0,
                    };
                    let new_raw = (cur & !mask) | (val & mask);

                    self.bar1_probe = false;
                    // I/O BAR: bit0 must remain set.
                    self.bar1 = (new_raw & E1000_BAR1_ADDR_MASK) | 0x1;
                    self.write_u32_raw(0x14, self.bar1);
                    return;
                }

                for i in 0..size {
                    let addr = offset + i;
                    if Self::is_read_only_byte(addr) {
                        continue;
                    }
                    self.regs[addr] = ((value >> (8 * i)) & 0xff) as u8;
                }
            }
            4 => {
                // PCI command register writes are commonly performed as a full dword store, but the
                // upper 16 bits are the Status register which is largely read-only / RW1C on real
                // hardware. Guests frequently write `0` in the upper half while intending to update
                // Command only; such writes must not clobber Status.
                if offset == 0x04 {
                    let status = u16::from_le_bytes(self.regs[0x06..0x08].try_into().unwrap());
                    let command = (value & 0xffff) as u16;
                    self.regs[0x04..0x06].copy_from_slice(&command.to_le_bytes());
                    self.regs[0x06..0x08].copy_from_slice(&status.to_le_bytes());
                    return;
                }
                if offset == 0x10 {
                    if value == 0xffff_ffff {
                        self.bar0_probe = true;
                        self.bar0 = 0;
                    } else {
                        self.bar0_probe = false;
                        self.bar0 = value & E1000_BAR0_ADDR_MASK;
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
                        self.bar1 = (value & E1000_BAR1_ADDR_MASK) | 0x1;
                    }
                    self.write_u32_raw(offset, self.bar1);
                    return;
                }
                for i in 0..4 {
                    let addr = offset + i;
                    if Self::is_read_only_byte(addr) {
                        continue;
                    }
                    self.regs[addr] = ((value >> (8 * i)) & 0xff) as u8;
                }
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
        // PCI command register bit 10 disables legacy INTx assertion.
        //
        // Some integration layers additionally gate INTx externally (e.g. a platform router), but
        // keeping this behavior in the device model makes standalone/unit-test usage consistent
        // with real hardware.
        let intx_disabled = (self.pci.read(0x04, 2) & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
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
        if offset > u32::MAX as u64 {
            return 0;
        }
        let aligned = (offset & !3) as u32;
        if aligned >= E1000_MMIO_SIZE {
            return 0;
        }
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
        if offset > u32::MAX as u64 {
            return;
        }
        let aligned = (offset & !3) as u32;
        if aligned >= E1000_MMIO_SIZE {
            return;
        }
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

    /// Write to the device MMIO BAR (legacy DMA-capable API).
    ///
    /// This preserves the older `mmio_write(&mut dyn MemoryBus, ..)` behavior where
    /// doorbell-like writes immediately kick TX/RX DMA. The write itself remains
    /// register-only; DMA occurs via an implicit [`poll`] call.
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

    /// Write to the device's I/O BAR (IOADDR/IODATA window) (legacy DMA-capable API).
    ///
    /// This preserves the older `io_write(&mut dyn MemoryBus, ..)` behavior where
    /// register writes immediately kick DMA. The write itself remains register-only;
    /// DMA occurs via an implicit [`poll`] call.
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
        let count = self.rdlen / desc_len;
        if count > MAX_RING_DESC_COUNT {
            return None;
        }
        Some(count)
    }

    fn tx_ring_desc_count(&self) -> Option<u32> {
        let desc_len = TxDesc::LEN as u32;
        if self.tdlen < desc_len || !self.tdlen.is_multiple_of(desc_len) {
            return None;
        }
        let count = self.tdlen / desc_len;
        if count > MAX_RING_DESC_COUNT {
            return None;
        }
        Some(count)
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

    fn bus_master_enabled(&self) -> bool {
        // PCI command register bit 2 (Bus Master Enable) gates all DMA on real hardware.
        // The device model must not touch guest memory (descriptor reads/writes) until the guest
        // explicitly enables bus mastering during enumeration.
        (self.pci.read(0x04, 2) & (1 << 2)) != 0
    }

    fn flush_rx_pending(&mut self, mem: &mut dyn MemoryBus) {
        if !self.bus_master_enabled() {
            return;
        }
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
            // This means a ring programmed with N descriptors provides N-1 usable RX buffers.
            if head == tail {
                break;
            }
            let idx = head as u64;
            let desc_addr = match idx
                .checked_mul(RxDesc::LEN as u64)
                .and_then(|off| base.checked_add(off))
            {
                Some(addr) => addr,
                None => break,
            };
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
        if !self.bus_master_enabled() {
            return;
        }
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

        let mut desc_budget = MAX_TX_DESCS_PER_POLL;
        while head != tail && desc_budget != 0 {
            desc_budget -= 1;
            let idx = head as u64;
            let desc_addr = match idx
                .checked_mul(TxDesc::LEN as u64)
                .and_then(|off| base.checked_add(off))
            {
                Some(addr) => addr,
                None => break,
            };
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

#[cfg(feature = "io-snapshot")]
impl E1000Device {
    pub fn snapshot_state(&self) -> E1000DeviceState {
        let mut other_regs: Vec<(u32, u32)> =
            self.other_regs.iter().map(|(k, v)| (*k, *v)).collect();
        other_regs.sort_by_key(|(k, _)| *k);

        E1000DeviceState {
            pci_regs: self.pci.regs,
            pci_bar0: self.pci.bar0,
            pci_bar0_probe: self.pci.bar0_probe,
            pci_bar1: self.pci.bar1,
            pci_bar1_probe: self.pci.bar1_probe,

            ctrl: self.ctrl,
            status: self.status,
            eecd: self.eecd,
            eerd: self.eerd,
            ctrl_ext: self.ctrl_ext,
            mdic: self.mdic,
            io_reg: self.io_reg,

            icr: self.icr,
            ims: self.ims,

            rctl: self.rctl,
            tctl: self.tctl,

            rdbal: self.rdbal,
            rdbah: self.rdbah,
            rdlen: self.rdlen,
            rdh: self.rdh,
            rdt: self.rdt,

            tdbal: self.tdbal,
            tdbah: self.tdbah,
            tdlen: self.tdlen,
            tdh: self.tdh,
            tdt: self.tdt,

            tx_partial: self.tx_partial.clone(),
            tx_drop: self.tx_drop,
            tx_ctx: E1000TxContextState {
                ipcss: self.tx_ctx.ipcss as u32,
                ipcso: self.tx_ctx.ipcso as u32,
                ipcse: self.tx_ctx.ipcse as u32,
                tucss: self.tx_ctx.tucss as u32,
                tucso: self.tx_ctx.tucso as u32,
                tucse: self.tx_ctx.tucse as u32,
                mss: self.tx_ctx.mss as u32,
                hdr_len: self.tx_ctx.hdr_len as u32,
            },
            tx_state: match self.tx_state {
                None => E1000TxPacketState::None,
                Some(TxPacketState::Legacy { cmd, css, cso }) => E1000TxPacketState::Legacy {
                    cmd,
                    css: css as u32,
                    cso: cso as u32,
                },
                Some(TxPacketState::Advanced { cmd, popts }) => {
                    E1000TxPacketState::Advanced { cmd, popts }
                }
            },

            mac_addr: self.mac_addr,
            ra_valid: self.ra_valid,
            eeprom: self.eeprom,
            phy: self.phy,

            other_regs,
            // Snapshot the internal host-facing frame queues.
            //
            // These queues are part of the *device model* (frames that have entered the emulator
            // from the host network stack but have not yet been DMA'd into guest memory, and
            // frames that the guest has produced but the host has not yet drained).
            //
            // This intentionally does **not** attempt to snapshot/restore external host networking
            // (socket connections, NAT state, etc); it only preserves the deterministic byte
            // buffers currently queued at the device boundary.
            rx_pending: self.rx_pending.iter().cloned().collect(),
            tx_out: self.tx_out.iter().cloned().collect(),
        }
    }

    pub fn restore_state(&mut self, state: &E1000DeviceState) {
        // Restore PCI config space.
        self.pci.regs = state.pci_regs;
        self.pci.bar0_probe = state.pci_bar0_probe;
        self.pci.bar1_probe = state.pci_bar1_probe;
        // PCI BARs are aligned/sanitized by the live device model when written via config space.
        // Keep restore resilient to corrupted snapshots by re-applying the same invariants here.
        //
        // In probe mode, the device model always resets BAR0 to 0 and BAR1 to 0x1 (I/O indicator
        // bit set), matching the behavior of `PciConfig::write`.
        self.pci.bar0 = if self.pci.bar0_probe {
            0
        } else {
            state.pci_bar0 & E1000_BAR0_ADDR_MASK
        };
        self.pci.bar1 = if self.pci.bar1_probe {
            0x1
        } else {
            // I/O BAR: bit0 must remain set.
            (state.pci_bar1 & E1000_BAR1_ADDR_MASK) | 0x1
        };

        // Keep the raw config-space bytes coherent with the BAR fields so that 8/16-bit config
        // reads observe the same values as 32-bit reads.
        //
        // This also fixes up older snapshots that may have stored inconsistent BAR dwords in
        // `pci_regs`.
        self.pci.write_u32_raw(0x10, self.pci.bar0);
        self.pci.write_u32_raw(0x14, self.pci.bar1);

        // Restore MMIO-visible register state + internal runtime state.
        self.ctrl = state.ctrl;
        self.status = state.status;
        self.eecd = state.eecd | EECD_EE_PRES;
        self.eerd = state.eerd;
        self.ctrl_ext = state.ctrl_ext;
        self.mdic = state.mdic | MDIC_READY;
        self.io_reg = state.io_reg & !3;

        self.icr = state.icr;
        self.ims = state.ims;

        self.rctl = state.rctl;
        self.tctl = state.tctl;

        self.rdbal = state.rdbal;
        self.rdbah = state.rdbah;
        self.rdlen = state.rdlen;
        self.rdh = state.rdh;
        self.rdt = state.rdt;

        self.tdbal = state.tdbal;
        self.tdbah = state.tdbah;
        self.tdlen = state.tdlen;
        self.tdh = state.tdh;
        self.tdt = state.tdt;

        self.tx_partial = state.tx_partial.clone();
        self.tx_drop = state.tx_drop;
        self.tx_ctx = TxOffloadContext {
            ipcss: state.tx_ctx.ipcss as usize,
            ipcso: state.tx_ctx.ipcso as usize,
            ipcse: state.tx_ctx.ipcse as usize,
            tucss: state.tx_ctx.tucss as usize,
            tucso: state.tx_ctx.tucso as usize,
            tucse: state.tx_ctx.tucse as usize,
            mss: state.tx_ctx.mss as usize,
            hdr_len: state.tx_ctx.hdr_len as usize,
        };
        self.tx_state = match state.tx_state {
            E1000TxPacketState::None => None,
            E1000TxPacketState::Legacy { cmd, css, cso } => Some(TxPacketState::Legacy {
                cmd,
                css: css as usize,
                cso: cso as usize,
            }),
            E1000TxPacketState::Advanced { cmd, popts } => {
                Some(TxPacketState::Advanced { cmd, popts })
            }
        };

        // Normalize potentially-corrupted snapshot state.
        if self.tx_drop {
            self.tx_partial.clear();
            self.tx_state = None;
        } else if self.tx_state.is_none() {
            self.tx_partial.clear();
        }

        self.mac_addr = state.mac_addr;
        self.ra_valid = state.ra_valid;
        self.eeprom = state.eeprom;
        // Keep EEPROM words 0..2 coherent with `mac_addr` as enforced by the live device model.
        self.init_eeprom_from_mac();
        self.phy = state.phy;

        self.other_regs.clear();
        for (k, v) in &state.other_regs {
            // Unknown/unmodeled registers are always within the MMIO BAR window.
            //
            // Snapshots may be loaded from untrusted sources; ignore out-of-range keys to keep the
            // in-memory map bounded and consistent with the live device model.
            if *k < E1000_MMIO_SIZE {
                self.other_regs.insert(*k, *v);
            }
        }

        const MAX_RX_PENDING: usize = 256;
        let mut rx_pending = state.rx_pending.clone();
        rx_pending.retain(|frame| (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&frame.len()));
        if rx_pending.len() > MAX_RX_PENDING {
            rx_pending.drain(0..rx_pending.len() - MAX_RX_PENDING);
        }
        self.rx_pending = VecDeque::from(rx_pending);

        let mut tx_out = state.tx_out.clone();
        tx_out.retain(|frame| (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&frame.len()));
        if tx_out.len() > MAX_TX_OUT_QUEUE {
            tx_out.drain(0..tx_out.len() - MAX_TX_OUT_QUEUE);
        }
        self.tx_out = VecDeque::from(tx_out);

        // Clamp ring pointers to keep `poll()` bounded even for corrupted snapshots.
        let rx_desc_count = self.rx_ring_desc_count();
        if let Some(count) = rx_desc_count.filter(|&c| c != 0) {
            if self.rdh >= count {
                self.rdh = 0;
            }
            if self.rdt >= count {
                self.rdt = 0;
            }
        } else {
            self.rdh = 0;
            self.rdt = 0;
        }

        let tx_desc_count = self.tx_ring_desc_count();
        if let Some(count) = tx_desc_count.filter(|&c| c != 0) {
            if self.tdh >= count {
                self.tdh = 0;
            }
            if self.tdt >= count {
                self.tdt = 0;
            }
        } else {
            self.tdh = 0;
            self.tdt = 0;
        }

        self.update_irq_level();

        // Restore derived poll work flags from ring pointers and pending queues.
        //
        // These flags are not part of the snapshot format; they are runtime hints that allow
        // `poll()` to skip work when no DMA-relevant doorbell writes have occurred.
        self.tx_needs_poll = self.tx_work_pending();
        self.rx_needs_flush = !self.rx_pending.is_empty();
    }
}

#[cfg(feature = "io-snapshot")]
impl aero_io_snapshot::io::state::IoSnapshot for E1000Device {
    const DEVICE_ID: [u8; 4] =
        <E1000DeviceState as aero_io_snapshot::io::state::IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: aero_io_snapshot::io::state::SnapshotVersion =
        <E1000DeviceState as aero_io_snapshot::io::state::IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        aero_io_snapshot::io::state::IoSnapshot::save_state(&self.snapshot_state())
    }

    fn load_state(&mut self, bytes: &[u8]) -> aero_io_snapshot::io::state::SnapshotResult<()> {
        let mut state = self.snapshot_state();
        aero_io_snapshot::io::state::IoSnapshot::load_state(&mut state, bytes)?;
        self.restore_state(&state);
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

    #[cfg(feature = "io-snapshot")]
    use aero_io_snapshot::io::state::codec::Encoder;
    #[cfg(feature = "io-snapshot")]
    use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

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
    fn ioaddr_out_of_range_does_not_touch_other_regs() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        assert!(dev.other_regs.is_empty());

        // Program an out-of-range IOADDR and attempt to access IODATA.
        dev.io_write_reg(0x0, 4, E1000_MMIO_SIZE); // out of range (valid: 0..E1000_MMIO_SIZE-1)
        dev.io_write_reg(0x4, 4, 0xDEAD_BEEF);
        assert_eq!(dev.io_read(0x4, 4), 0);
        assert!(
            dev.other_regs.is_empty(),
            "out-of-range IOADDR should not create other_regs entries"
        );

        // Direct MMIO out-of-range accesses should also be ignored.
        dev.mmio_write_u32_reg(E1000_MMIO_SIZE + 0x100, 0xC0FF_EE00);
        assert_eq!(dev.mmio_read((E1000_MMIO_SIZE + 0x100) as u64, 4), 0);
        assert!(dev.other_regs.is_empty());

        // In-range IOADDR/IODATA accesses should still work.
        dev.mmio_write_u32_reg(REG_IMS, 0x1234_5678);
        dev.io_write_reg(0x0, 4, REG_IMS);
        assert_eq!(dev.io_read(0x4, 4), 0x1234_5678);
    }

    #[test]
    fn tx_processing_emits_frame_and_sets_dd() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
    fn pci_intx_disable_bit_gates_irq_level() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Enable a cause + interrupt mask so the device would normally assert INTx.
        dev.mmio_write_u32_reg(REG_IMS, ICR_TXDW);
        dev.mmio_write_u32_reg(REG_ICS, ICR_TXDW);
        assert!(dev.irq_level());

        // Disable INTx via PCI command bit 10; the line must be deasserted.
        dev.pci_config_write(0x04, 2, 1 << 10);
        assert!(!dev.irq_level());

        // Re-enable INTx: since the interrupt cause is still pending, the line should reassert.
        dev.pci_config_write(0x04, 2, 0);
        assert!(dev.irq_level());
    }

    #[test]
    fn pci_command_dword_write_does_not_clobber_status() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Seed a non-zero Status register value.
        dev.pci_config_write(0x06, 2, 0x1234);

        // Guest writes the full dword at 0x04 with Status=0 while intending to update Command.
        dev.pci_config_write(0x04, 4, 0x0004); // COMMAND.BME

        assert_eq!(dev.pci_config_read(0x06, 2) as u16, 0x1234);
        assert_eq!(dev.pci_config_read(0x04, 2) as u16, 0x0004);
        assert_eq!(dev.pci_config_read(0x04, 4), 0x1234_0004);
    }

    #[test]
    fn pci_interrupt_line_dword_write_does_not_clobber_interrupt_pin() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        let pin_before = dev.pci_config_read(0x3d, 1) as u8;
        assert_eq!(pin_before, 1);

        // Common guest pattern: 32-bit write at 0x3C with upper bytes (Interrupt Pin + reserved)
        // set to 0. This must not clear the device-reported Interrupt Pin.
        dev.pci_config_write(0x3c, 4, 0x0000_000a);

        assert_eq!(dev.pci_config_read(0x3d, 1) as u8, pin_before);
        assert_eq!(dev.pci_config_read(0x3c, 1) as u8, 0x0a);
    }

    #[test]
    fn pci_cache_line_dword_write_does_not_clobber_header_type() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        let header_before = dev.pci_config_read(0x0e, 1) as u8;
        assert_eq!(header_before, 0);

        // Dword write at 0x0C spans:
        // - Cache Line Size (0x0C)
        // - Latency Timer (0x0D)
        // - Header Type (0x0E, read-only)
        // - BIST (0x0F)
        dev.pci_config_write(0x0c, 4, 0x12_34_56_78);

        assert_eq!(dev.pci_config_read(0x0e, 1) as u8, header_before);
        assert_eq!(dev.pci_config_read(0x0c, 1) as u8, 0x78);
        assert_eq!(dev.pci_config_read(0x0d, 1) as u8, 0x56);
        assert_eq!(dev.pci_config_read(0x0f, 1) as u8, 0x12);
    }

    #[test]
    fn tx_mmio_write_reg_defers_dma_until_poll() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut panic_mem = PanicMem;
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
    fn tx_desc_address_overflow_does_not_panic_or_dma() {
        // Regression test: descriptor address calculations must not panic on u64 overflow, and the
        // device model must not wrap around and DMA at an unintended low address.
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut panic_mem = PanicMem;

        // 2 descriptors, so RDH=1 is valid. Choose a base such that base + 1*16 overflows.
        dev.tdbah = 0xFFFF_FFFF;
        dev.tdbal = 0xFFFF_FFF8;
        dev.tdlen = (TxDesc::LEN as u32) * 2;
        dev.tdh = 1;
        dev.tdt = 0;
        dev.tctl = TCTL_EN;

        dev.poll(&mut panic_mem);
    }

    #[test]
    fn rx_desc_address_overflow_does_not_panic_or_dma() {
        // Same as `tx_desc_address_overflow_does_not_panic_or_dma`, but for RX descriptor reads.
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut panic_mem = PanicMem;

        dev.rdbah = 0xFFFF_FFFF;
        dev.rdbal = 0xFFFF_FFF8;
        dev.rdlen = (RxDesc::LEN as u32) * 2;
        dev.rdh = 1;
        dev.rdt = 0;
        dev.rctl = RCTL_EN;

        dev.enqueue_rx_frame(vec![0x22u8; MIN_L2_FRAME_LEN]);

        dev.poll(&mut panic_mem);
    }

    #[test]
    fn tx_rejects_excessive_ring_len_without_dma() {
        // If the guest configures an absurdly large descriptor ring, the model should reject it
        // and avoid attempting a pathological amount of DMA/descriptor iteration.
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut panic_mem = PanicMem;

        dev.mmio_write_u32_reg(REG_TDLEN, (MAX_RING_DESC_COUNT + 1) * (TxDesc::LEN as u32));
        dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);
        dev.mmio_write_u32_reg(REG_TDT, 1); // doorbell

        dev.poll(&mut panic_mem);
    }

    #[test]
    fn rx_rejects_excessive_ring_len_without_dma() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut panic_mem = PanicMem;

        dev.enqueue_rx_frame(vec![0x22u8; MIN_L2_FRAME_LEN]);
        dev.mmio_write_u32_reg(REG_RDLEN, (MAX_RING_DESC_COUNT + 1) * (RxDesc::LEN as u32));
        dev.mmio_write_u32_reg(REG_RDT, 1);
        dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

        dev.poll(&mut panic_mem);
    }

    #[test]
    fn tx_io_write_reg_defers_dma_until_poll() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

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

    #[test]
    fn tx_dma_is_gated_on_pci_bus_master_enable() {
        let mut mem = TestMem::new(0x10_000);
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Set up TX ring at 0x1000 with 4 descriptors.
        dev.mmio_write_u32_reg(REG_TDBAL, 0x1000);
        dev.mmio_write_u32_reg(REG_TDLEN, (TxDesc::LEN as u32) * 4);
        dev.mmio_write_u32_reg(REG_TDH, 0);
        dev.mmio_write_u32_reg(REG_TDT, 0);
        dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

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

        // With bus mastering disabled, advancing the tail must not trigger any DMA.
        dev.mmio_write_u32_reg(REG_TDT, 1);
        dev.poll(&mut mem);

        assert!(dev.pop_tx_frame().is_none());
        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_eq!(updated.status & TXD_STAT_DD, 0);

        // Enable Bus Mastering and poll again: now TX should complete.
        dev.pci_config_write(0x04, 2, 0x4);
        dev.poll(&mut mem);

        assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt.as_slice()));
        let updated = TxDesc::from_bytes(read_desc::<{ TxDesc::LEN }>(&mut mem, 0x1000));
        assert_ne!(updated.status & TXD_STAT_DD, 0);
    }

    #[test]
    fn rx_dma_is_gated_on_pci_bus_master_enable() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Configure RX ring: 2 descriptors at 0x3000.
        // With the device model's \"keep one unused\" rule, this provides 1 usable RX buffer.
        dev.mmio_write_u32_reg(REG_RDBAL, 0x3000);
        dev.mmio_write_u32_reg(REG_RDLEN, (RxDesc::LEN as u32) * 2);
        dev.mmio_write_u32_reg(REG_RDH, 0);
        dev.mmio_write_u32_reg(REG_RDT, 1);
        dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

        // Enable RX interrupts so we can observe IRQ assertion after delivery.
        dev.mmio_write_u32_reg(REG_IMS, ICR_RXT0);

        // Queue a frame for delivery.
        let frame = vec![0x11u8; MIN_L2_FRAME_LEN];
        dev.enqueue_rx_frame(frame.clone());

        // With bus mastering disabled, poll must not touch guest memory.
        let mut panic_mem = PanicMem;
        dev.poll(&mut panic_mem);
        assert_eq!(dev.rx_pending.len(), 1);

        // Set up guest memory for the RX descriptor + buffer.
        let mut mem = TestMem::new(0x10_000);
        mem.write_bytes(
            0x3000,
            &RxDesc {
                buffer_addr: 0x4000,
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            }
            .to_bytes(),
        );
        mem.write_bytes(0x4000, &[0xAA; MIN_L2_FRAME_LEN]);

        // Enable Bus Mastering and poll again: now RX should flush into guest memory.
        dev.pci_config_write(0x04, 2, 0x4);
        dev.poll(&mut mem);

        assert!(dev.rx_pending.is_empty());
        assert_eq!(mem.read_bytes(0x4000, frame.len()), frame);

        let updated = RxDesc::from_bytes(read_desc::<{ RxDesc::LEN }>(&mut mem, 0x3000));
        assert_ne!(updated.status & RXD_STAT_DD, 0);
        assert_ne!(updated.status & RXD_STAT_EOP, 0);
        assert_eq!(updated.length as usize, frame.len());

        assert!(dev.irq_level());
        let icr = dev.mmio_read_u32(REG_ICR);
        assert_eq!(icr & ICR_RXT0, ICR_RXT0);
        assert!(!dev.irq_level());
    }

    #[cfg(feature = "io-snapshot")]
    #[test]
    fn snapshot_roundtrip_is_lossless() {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Touch some state (including collections) so the snapshot isn't trivial.
        dev.pci_write_u32(0x10, 0xffff_ffff);
        dev.pci_write_u32(0x14, 0xffff_ffff);
        dev.mmio_write_u32_reg(REG_IMS, ICR_RXT0 | ICR_TXDW);
        dev.mmio_write_u32_reg(0x9000, 0xDEAD_BEEF);
        dev.mmio_write_u32_reg(0x9004, 0xC0FF_EE00);

        dev.eeprom[10] = 0x1234;
        dev.phy[1] = 0x5678;

        dev.tx_partial = vec![0x33; 128];
        dev.tx_ctx = TxOffloadContext {
            ipcss: 14,
            ipcso: 24,
            ipcse: 33,
            tucss: 34,
            tucso: 50,
            tucse: 100,
            mss: 1460,
            hdr_len: 54,
        };
        dev.tx_state = Some(TxPacketState::Advanced {
            cmd: 0xaa,
            popts: 0xbb,
        });

        dev.enqueue_rx_frame(vec![0x11; MIN_L2_FRAME_LEN]);
        dev.tx_out.push_back(vec![0x22; MIN_L2_FRAME_LEN]);

        let bytes = dev.save_state();

        let mut restored = E1000Device::new([0, 0, 0, 0, 0, 0]);
        restored.load_state(&bytes).unwrap();

        assert_eq!(bytes, restored.save_state());
    }

    #[cfg(feature = "io-snapshot")]
    #[test]
    fn snapshot_encoding_is_deterministic_for_other_regs_hashmap() {
        let mut a = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut b = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // Same keys/values but inserted in different order.
        a.mmio_write_u32_reg(0x9000, 1);
        a.mmio_write_u32_reg(0x9004, 2);
        a.mmio_write_u32_reg(0x9008, 3);

        b.mmio_write_u32_reg(0x9008, 3);
        b.mmio_write_u32_reg(0x9004, 2);
        b.mmio_write_u32_reg(0x9000, 1);

        assert_eq!(a.save_state(), b.save_state());
    }

    #[cfg(feature = "io-snapshot")]
    #[test]
    fn snapshot_load_rejects_oversized_other_regs_and_truncated_tlv() {
        // Oversized other_regs count.
        let oversized = Encoder::new().u32(u32::MAX).finish();
        let mut w = SnapshotWriter::new(
            <E1000Device as IoSnapshot>::DEVICE_ID,
            <E1000Device as IoSnapshot>::DEVICE_VERSION,
        );
        w.field_bytes(90, oversized); // TAG_OTHER_REGS
        let bytes = w.finish();

        let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        let err = dev.load_state(&bytes).unwrap_err();
        assert_eq!(
            err,
            SnapshotError::InvalidFieldEncoding("e1000 other_regs count")
        );

        // Malformed/truncated TLV should produce a decode error (not panic).
        let mut good = dev.save_state();
        good.pop();
        let err = dev.load_state(&good).unwrap_err();
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[cfg(feature = "io-snapshot")]
    #[test]
    fn snapshot_load_ignores_unknown_tags_for_forward_compat() {
        let dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        let canonical = dev.save_state();

        // Append a synthetic unknown TLV field. SnapshotReader does not require tags to be ordered.
        let mut with_unknown = canonical.clone();
        let tag: u16 = 0xF00D;
        let payload = [1u8, 2, 3, 4];
        with_unknown.extend_from_slice(&tag.to_le_bytes());
        with_unknown.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        with_unknown.extend_from_slice(&payload);

        let mut restored = E1000Device::new([0; 6]);
        restored
            .load_state(&with_unknown)
            .expect("unknown tags should be ignored");

        // Unknown tags are not preserved; the re-saved snapshot should match the canonical bytes.
        assert_eq!(restored.save_state(), canonical);
    }
}
