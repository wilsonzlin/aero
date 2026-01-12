//! PCI IDE controller with legacy primary/secondary channels (ATA + ATAPI).
//!
//! This is a compatibility-first model intended to satisfy:
//! - BIOS INT13 (PIO via legacy I/O ports).
//! - Windows 7 "IDE mode" (`pciide.sys`/`atapi.sys`) including Bus Master IDE DMA.

mod ata;
mod atapi;
mod busmaster;

pub use ata::AtaDevice;
pub use atapi::{AtapiCdrom, IsoBackend};
pub use busmaster::{BusMasterChannel, PrdEntry};

use crate::io::pci::PciDevice;
use crate::io::storage::SECTOR_SIZE;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use aero_io_snapshot::io::storage::state::{
    IdeChannelState, IdeControllerState, IdeDataMode, IdeDmaCommitState, IdeDmaDirection,
    IdeDmaRequestState, IdeDriveState, IdePioWriteState, IdePortMapState, IdeTaskFileState,
    IdeTransferKind, PciConfigSpaceState, MAX_IDE_DATA_BUFFER_BYTES,
};
use memory::MemoryBus;

const IDE_STATUS_BSY: u8 = 0x80;
const IDE_STATUS_DRDY: u8 = 0x40;
const IDE_STATUS_DRQ: u8 = 0x08;
const IDE_STATUS_ERR: u8 = 0x01;

const IDE_CTRL_NIEN: u8 = 0x02;
const IDE_CTRL_SRST: u8 = 0x04;
const IDE_CTRL_HOB: u8 = 0x80;

const ATA_REG_DATA: u16 = 0;
const ATA_REG_ERROR_FEATURES: u16 = 1;
const ATA_REG_SECTOR_COUNT: u16 = 2;
const ATA_REG_LBA0: u16 = 3;
const ATA_REG_LBA1: u16 = 4;
const ATA_REG_LBA2: u16 = 5;
const ATA_REG_DEVICE: u16 = 6;
const ATA_REG_STATUS_COMMAND: u16 = 7;

const ATA_CTRL_ALT_STATUS_DEVICE_CTRL: u16 = 0;
const ATA_CTRL_DRIVE_ADDRESS: u16 = 1;

fn try_alloc_zeroed(len: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).ok()?;
    buf.resize(len, 0);
    Some(buf)
}

/// Legacy primary/secondary I/O port assignments.
#[derive(Debug, Clone, Copy)]
pub struct IdePortMap {
    pub cmd_base: u16,
    pub ctrl_base: u16,
    pub irq: u8,
}

pub const PRIMARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x1F0,
    ctrl_base: 0x3F6,
    irq: 14,
};

pub const SECONDARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x170,
    ctrl_base: 0x376,
    irq: 15,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveSelect {
    Master = 0,
    Slave = 1,
}

impl DriveSelect {
    fn from_device_reg(val: u8) -> Self {
        if (val & 0x10) != 0 {
            DriveSelect::Slave
        } else {
            DriveSelect::Master
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataMode {
    None,
    /// Device -> host (PIO IN) via data register.
    PioIn,
    /// Host -> device (PIO OUT) via data register.
    PioOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferKind {
    AtaPioRead,
    AtaPioWrite,
    Identify,
    AtapiPacket,
    AtapiPioIn,
}

#[derive(Debug, Clone, Default)]
struct TaskFile {
    features: u8,
    sector_count: u8,
    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8,

    hob_features: u8,
    hob_sector_count: u8,
    hob_lba0: u8,
    hob_lba1: u8,
    hob_lba2: u8,

    pending_features_high: bool,
    pending_sector_count_high: bool,
    pending_lba0_high: bool,
    pending_lba1_high: bool,
    pending_lba2_high: bool,
}

impl TaskFile {
    fn read_reg(&self, reg: u16, hob: bool) -> u8 {
        match reg {
            ATA_REG_ERROR_FEATURES => {
                if hob {
                    self.hob_features
                } else {
                    self.features
                }
            }
            ATA_REG_SECTOR_COUNT => {
                if hob {
                    self.hob_sector_count
                } else {
                    self.sector_count
                }
            }
            ATA_REG_LBA0 => {
                if hob {
                    self.hob_lba0
                } else {
                    self.lba0
                }
            }
            ATA_REG_LBA1 => {
                if hob {
                    self.hob_lba1
                } else {
                    self.lba1
                }
            }
            ATA_REG_LBA2 => {
                if hob {
                    self.hob_lba2
                } else {
                    self.lba2
                }
            }
            ATA_REG_DEVICE => self.device,
            _ => 0,
        }
    }

    fn write_reg(&mut self, reg: u16, val: u8) {
        match reg {
            ATA_REG_ERROR_FEATURES => {
                if !self.pending_features_high {
                    self.hob_features = val;
                    self.pending_features_high = true;
                } else {
                    self.features = val;
                    self.pending_features_high = false;
                }
            }
            ATA_REG_SECTOR_COUNT => {
                if !self.pending_sector_count_high {
                    self.hob_sector_count = val;
                    self.pending_sector_count_high = true;
                } else {
                    self.sector_count = val;
                    self.pending_sector_count_high = false;
                }
            }
            ATA_REG_LBA0 => {
                if !self.pending_lba0_high {
                    self.hob_lba0 = val;
                    self.pending_lba0_high = true;
                } else {
                    self.lba0 = val;
                    self.pending_lba0_high = false;
                }
            }
            ATA_REG_LBA1 => {
                if !self.pending_lba1_high {
                    self.hob_lba1 = val;
                    self.pending_lba1_high = true;
                } else {
                    self.lba1 = val;
                    self.pending_lba1_high = false;
                }
            }
            ATA_REG_LBA2 => {
                if !self.pending_lba2_high {
                    self.hob_lba2 = val;
                    self.pending_lba2_high = true;
                } else {
                    self.lba2 = val;
                    self.pending_lba2_high = false;
                }
            }
            ATA_REG_DEVICE => {
                self.device = val;
            }
            _ => {}
        }
    }

    fn normalize_for_command(&mut self, is_lba48: bool) {
        // For non-LBA48 commands the driver writes each register once. We
        // capture the first write in the HOB shadow, so commit those into the
        // visible "low" registers here.
        if self.pending_features_high {
            self.features = self.hob_features;
            self.pending_features_high = false;
        }
        if self.pending_sector_count_high {
            self.sector_count = self.hob_sector_count;
            self.pending_sector_count_high = false;
        }
        if self.pending_lba0_high {
            self.lba0 = self.hob_lba0;
            self.pending_lba0_high = false;
        }
        if self.pending_lba1_high {
            self.lba1 = self.hob_lba1;
            self.pending_lba1_high = false;
        }
        if self.pending_lba2_high {
            self.lba2 = self.hob_lba2;
            self.pending_lba2_high = false;
        }

        if !is_lba48 {
            self.hob_features = 0;
            self.hob_sector_count = 0;
            self.hob_lba0 = 0;
            self.hob_lba1 = 0;
            self.hob_lba2 = 0;
        }
    }

    fn lba28(&self) -> u64 {
        let high4 = (self.device & 0x0F) as u64;
        (high4 << 24) | (self.lba2 as u64) << 16 | (self.lba1 as u64) << 8 | self.lba0 as u64
    }

    fn lba48(&self) -> u64 {
        (self.hob_lba2 as u64) << 40
            | (self.hob_lba1 as u64) << 32
            | (self.hob_lba0 as u64) << 24
            | (self.lba2 as u64) << 16
            | (self.lba1 as u64) << 8
            | self.lba0 as u64
    }

    fn sector_count28(&self) -> u16 {
        let c = self.sector_count as u16;
        if c == 0 {
            256
        } else {
            c
        }
    }

    fn sector_count48(&self) -> u32 {
        let c = ((self.hob_sector_count as u32) << 8) | self.sector_count as u32;
        if c == 0 {
            65536
        } else {
            c
        }
    }
}

enum IdeDevice {
    Ata(AtaDevice),
    Atapi(AtapiCdrom),
}

struct Channel {
    ports: IdePortMap,
    devices: [Option<IdeDevice>; 2],

    tf: TaskFile,
    status: u8,
    error: u8,
    control: u8,

    data_mode: DataMode,
    transfer_kind: Option<TransferKind>,
    data: Vec<u8>,
    data_index: usize,

    irq_pending: bool,

    // DMA requested by the currently-selected device and waiting for the Bus
    // Master engine to be started.
    pending_dma: Option<busmaster::DmaRequest>,

    // Latched parameters for an in-flight ATA PIO write command.
    pio_write: Option<(u64, u64)>,
}

impl Channel {
    fn new(ports: IdePortMap) -> Self {
        Self {
            ports,
            devices: [None, None],
            tf: TaskFile::default(),
            status: IDE_STATUS_DRDY,
            error: 0,
            control: 0,
            data_mode: DataMode::None,
            transfer_kind: None,
            data: Vec::new(),
            data_index: 0,
            irq_pending: false,
            pending_dma: None,
            pio_write: None,
        }
    }

    fn selected_drive(&self) -> DriveSelect {
        DriveSelect::from_device_reg(self.tf.device)
    }

    fn set_irq(&mut self) {
        if (self.control & IDE_CTRL_NIEN) == 0 {
            self.irq_pending = true;
        }
    }

    fn clear_irq(&mut self) {
        self.irq_pending = false;
    }

    fn reset(&mut self) {
        self.tf = TaskFile::default();
        self.status = IDE_STATUS_DRDY;
        self.error = 0;
        self.data_mode = DataMode::None;
        self.transfer_kind = None;
        self.data.clear();
        self.data_index = 0;
        self.pending_dma = None;
        self.pio_write = None;
        self.clear_irq();
    }

    fn set_error(&mut self, err: u8) {
        self.error = err;
        self.status |= IDE_STATUS_ERR;
    }

    fn clear_error(&mut self) {
        self.error = 0;
        self.status &= !IDE_STATUS_ERR;
    }

    fn abort_command(&mut self, err: u8) {
        self.data_mode = DataMode::None;
        self.transfer_kind = None;
        self.data.clear();
        self.data_index = 0;
        self.pending_dma = None;
        self.pio_write = None;

        self.set_error(err);
        self.status &= !(IDE_STATUS_BSY | IDE_STATUS_DRQ);
        self.status |= IDE_STATUS_DRDY;
        self.set_irq();
    }

    fn begin_pio_in(&mut self, kind: TransferKind, data: Vec<u8>) {
        self.data = data;
        self.data_index = 0;
        self.data_mode = DataMode::PioIn;
        self.transfer_kind = Some(kind);
        self.status &= !IDE_STATUS_BSY;
        self.status |= IDE_STATUS_DRQ | IDE_STATUS_DRDY;
        self.clear_error();
        self.set_irq();
    }

    fn begin_pio_out(&mut self, kind: TransferKind, len: usize) {
        let Some(data) = try_alloc_zeroed(len) else {
            self.abort_command(0x04);
            return;
        };
        self.data = data;
        self.data_index = 0;
        self.data_mode = DataMode::PioOut;
        self.transfer_kind = Some(kind);
        self.status &= !IDE_STATUS_BSY;
        self.status |= IDE_STATUS_DRQ | IDE_STATUS_DRDY;
        self.clear_error();
        self.set_irq();
    }

    fn complete_non_data_command(&mut self) {
        self.data_mode = DataMode::None;
        self.transfer_kind = None;
        self.data.clear();
        self.data_index = 0;
        self.status &= !(IDE_STATUS_BSY | IDE_STATUS_DRQ);
        self.status |= IDE_STATUS_DRDY;
        self.clear_error();
        self.set_irq();
    }

    fn data_in_u16(&mut self) -> u16 {
        if self.data_index >= self.data.len() {
            self.finish_data_phase();
            return 0;
        }
        let lo = self.data[self.data_index];
        let hi = self.data.get(self.data_index + 1).copied().unwrap_or(0);
        self.data_index += 2;
        if self.data_index >= self.data.len() {
            self.finish_data_phase();
        }
        u16::from_le_bytes([lo, hi])
    }

    fn data_in_u32(&mut self) -> u32 {
        let lo = self.data_in_u16() as u32;
        let hi = self.data_in_u16() as u32;
        lo | (hi << 16)
    }

    fn data_out_u16(&mut self, val: u16) {
        if self.data_index + 2 > self.data.len() {
            // Ignore overflow.
            return;
        }
        let b = val.to_le_bytes();
        self.data[self.data_index] = b[0];
        self.data[self.data_index + 1] = b[1];
        self.data_index += 2;
        if self.data_index >= self.data.len() {
            self.finish_data_phase();
        }
    }

    fn data_out_u32(&mut self, val: u32) {
        self.data_out_u16((val & 0xFFFF) as u16);
        self.data_out_u16((val >> 16) as u16);
    }

    fn finish_data_phase(&mut self) {
        match self.transfer_kind {
            Some(TransferKind::AtaPioWrite) => {
                let (lba, sectors) = self
                    .pio_write
                    .take()
                    .unwrap_or_else(|| (self.tf.lba28(), self.tf.sector_count28() as u64));
                let data = std::mem::take(&mut self.data);
                let idx = self.selected_drive() as usize;
                if let Some(IdeDevice::Ata(dev)) = self.devices[idx].as_mut() {
                    let _ = dev.pio_write(lba, sectors, &data);
                }
                self.complete_non_data_command();
            }
            Some(TransferKind::AtapiPioIn) => {
                // Data phase complete; transition to status phase.
                self.tf.sector_count = 0x03; // IO=1, CoD=1
                self.complete_non_data_command();
            }
            Some(TransferKind::AtapiPacket) => {
                let mut packet = [0u8; 12];
                packet.copy_from_slice(&self.data[..12]);
                let dma_requested = (self.tf.features & 0x01) != 0;
                let idx = self.selected_drive() as usize;
                let result = match self.devices[idx].as_mut() {
                    Some(IdeDevice::Atapi(dev)) => dev.handle_packet(&packet, dma_requested),
                    _ => {
                        self.set_error(0x04);
                        self.status &= !IDE_STATUS_DRQ;
                        self.set_irq();
                        return;
                    }
                };

                match result {
                    atapi::PacketResult::DataIn(buf) => {
                        // ATAPI uses sector_count as interrupt reason; IO=1, CoD=0.
                        self.tf.sector_count = 0x02;
                        let byte_count = buf.len().min(u16::MAX as usize) as u16;
                        self.tf.lba1 = (byte_count & 0xFF) as u8;
                        self.tf.lba2 = (byte_count >> 8) as u8;
                        self.begin_pio_in(TransferKind::AtapiPioIn, buf);
                    }
                    atapi::PacketResult::NoDataSuccess => {
                        self.tf.sector_count = 0x03; // IO=1, CoD=1 (status)
                        self.complete_non_data_command();
                    }
                    atapi::PacketResult::Error { .. } => {
                        self.tf.sector_count = 0x03;
                        self.set_error(0x04); // ABRT
                        self.status &= !IDE_STATUS_DRQ;
                        self.status |= IDE_STATUS_DRDY;
                        self.set_irq();
                        // Command completed with error; exit the packet phase.
                        self.data_mode = DataMode::None;
                        self.transfer_kind = None;
                        self.data.clear();
                        self.data_index = 0;
                    }
                    atapi::PacketResult::DmaIn(buf) => {
                        // Queue a DMA transfer; completion will raise IRQ.
                        self.tf.sector_count = 0x02;
                        self.pending_dma = Some(busmaster::DmaRequest::atapi_data_in(buf));
                        self.status &= !(IDE_STATUS_DRQ | IDE_STATUS_BSY);
                        self.status |= IDE_STATUS_DRDY;
                        // Packet phase completed; DMA engine will move data.
                        self.data_mode = DataMode::None;
                        self.transfer_kind = None;
                        self.data.clear();
                        self.data_index = 0;
                    }
                }
            }
            Some(TransferKind::Identify) | Some(TransferKind::AtaPioRead) => {
                self.complete_non_data_command();
            }
            None => {
                self.complete_non_data_command();
            }
        }
    }
}

/// A compatibility-first PCI IDE controller exposing legacy ports and Bus Master DMA.
pub struct IdeController {
    primary: Channel,
    secondary: Channel,
    bus_master_base: u16,
    bus_master: [BusMasterChannel; 2],
    pci_regs: [u8; 256],
    bar0: u32,
    bar0_probe: bool,
    bar1: u32,
    bar1_probe: bool,
    bar2: u32,
    bar2_probe: bool,
    bar3: u32,
    bar3_probe: bool,
    bar4: u32,
    bar4_probe: bool,
}

impl IdeController {
    pub fn new(bus_master_base: u16) -> Self {
        let primary = Channel::new(PRIMARY_PORTS);
        let secondary = Channel::new(SECONDARY_PORTS);
        let mut pci_regs = [0u8; 256];
        // PIIX3-ish identifiers: vendor/device/class are enough for Windows IDE mode.
        pci_regs[0x00..0x02].copy_from_slice(&0x8086u16.to_le_bytes()); // Intel
        pci_regs[0x02..0x04].copy_from_slice(&0x7010u16.to_le_bytes()); // PIIX3 IDE
        pci_regs[0x04..0x06].copy_from_slice(&0x0005u16.to_le_bytes()); // I/O space + bus master
        pci_regs[0x08] = 0x00; // revision
        pci_regs[0x09] = 0x8A; // prog IF: legacy primary/secondary + bus master
        pci_regs[0x0A] = 0x01; // subclass: IDE
        pci_regs[0x0B] = 0x01; // class: mass storage
        pci_regs[0x0E] = 0x00; // header type

        // BAR0-3: legacy I/O windows (command/control blocks). We keep these fixed.
        let bar0 = 0x1F0u32 | 0x01;
        let bar1 = 0x3F4u32 | 0x01;
        let bar2 = 0x170u32 | 0x01;
        let bar3 = 0x374u32 | 0x01;
        pci_regs[0x10..0x14].copy_from_slice(&bar0.to_le_bytes());
        pci_regs[0x14..0x18].copy_from_slice(&bar1.to_le_bytes());
        pci_regs[0x18..0x1C].copy_from_slice(&bar2.to_le_bytes());
        pci_regs[0x1C..0x20].copy_from_slice(&bar3.to_le_bytes());
        // BAR4: Bus Master IDE.
        let bar4 = (bus_master_base as u32) | 0x01;
        pci_regs[0x20..0x24].copy_from_slice(&bar4.to_le_bytes());

        pci_regs[0x3C] = PRIMARY_PORTS.irq; // interrupt line (best-effort)
        pci_regs[0x3D] = 0x01; // INTA#

        Self {
            primary,
            secondary,
            bus_master_base,
            bus_master: [BusMasterChannel::new(), BusMasterChannel::new()],
            pci_regs,
            bar0,
            bar0_probe: false,
            bar1,
            bar1_probe: false,
            bar2,
            bar2_probe: false,
            bar3,
            bar3_probe: false,
            bar4,
            bar4_probe: false,
        }
    }

    fn command(&self) -> u16 {
        u16::from_le_bytes(self.pci_regs[0x04..0x06].try_into().unwrap())
    }

    fn io_space_enabled(&self) -> bool {
        (self.command() & (1 << 0)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    pub fn attach_primary_master_ata(&mut self, dev: AtaDevice) {
        let capable = dev.supports_dma();
        self.primary.devices[0] = Some(IdeDevice::Ata(dev));
        self.bus_master[0].set_drive_dma_capable(0, capable);
    }

    pub fn attach_primary_slave_ata(&mut self, dev: AtaDevice) {
        let capable = dev.supports_dma();
        self.primary.devices[1] = Some(IdeDevice::Ata(dev));
        self.bus_master[0].set_drive_dma_capable(1, capable);
    }

    pub fn attach_secondary_master_ata(&mut self, dev: AtaDevice) {
        let capable = dev.supports_dma();
        self.secondary.devices[0] = Some(IdeDevice::Ata(dev));
        self.bus_master[1].set_drive_dma_capable(0, capable);
    }

    pub fn attach_secondary_slave_ata(&mut self, dev: AtaDevice) {
        let capable = dev.supports_dma();
        self.secondary.devices[1] = Some(IdeDevice::Ata(dev));
        self.bus_master[1].set_drive_dma_capable(1, capable);
    }

    pub fn attach_primary_master_atapi(&mut self, dev: AtapiCdrom) {
        let capable = dev.supports_dma();
        self.primary.devices[0] = Some(IdeDevice::Atapi(dev));
        self.bus_master[0].set_drive_dma_capable(0, capable);
    }

    pub fn attach_primary_slave_atapi(&mut self, dev: AtapiCdrom) {
        let capable = dev.supports_dma();
        self.primary.devices[1] = Some(IdeDevice::Atapi(dev));
        self.bus_master[0].set_drive_dma_capable(1, capable);
    }

    pub fn attach_secondary_master_atapi(&mut self, dev: AtapiCdrom) {
        let capable = dev.supports_dma();
        self.secondary.devices[0] = Some(IdeDevice::Atapi(dev));
        self.bus_master[1].set_drive_dma_capable(0, capable);
    }

    pub fn attach_secondary_slave_atapi(&mut self, dev: AtapiCdrom) {
        let capable = dev.supports_dma();
        self.secondary.devices[1] = Some(IdeDevice::Atapi(dev));
        self.bus_master[1].set_drive_dma_capable(1, capable);
    }

    /// Read an I/O port (8/16/32-bit).
    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        // Gate port I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => 0xffff_ffff,
                _ => 0xffff_ffff,
            };
        }
        if let Some((chan_idx, off)) = self.decode_bus_master(port) {
            return self.bus_master[chan_idx].read(off, size);
        }

        if let Some(off) = port.checked_sub(self.primary.ports.cmd_base) {
            if off < 8 {
                return Self::read_cmd_reg(&mut self.primary, off, size);
            }
        }
        if let Some(off) = port.checked_sub(self.secondary.ports.cmd_base) {
            if off < 8 {
                return Self::read_cmd_reg(&mut self.secondary, off, size);
            }
        }
        if let Some(off) = port.checked_sub(self.primary.ports.ctrl_base) {
            if off < 2 {
                return Self::read_ctrl_reg(&mut self.primary, off);
            }
        }
        if let Some(off) = port.checked_sub(self.secondary.ports.ctrl_base) {
            if off < 2 {
                return Self::read_ctrl_reg(&mut self.secondary, off);
            }
        }

        0xFFFF_FFFF
    }

    /// Write an I/O port (8/16/32-bit).
    pub fn io_write(&mut self, port: u16, size: u8, val: u32) {
        // Gate port I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return;
        }
        if let Some((chan_idx, off)) = self.decode_bus_master(port) {
            self.bus_master[chan_idx].write(off, size, val);
            return;
        }

        if let Some(off) = port.checked_sub(self.primary.ports.cmd_base) {
            if off < 8 {
                Self::write_cmd_reg(&mut self.primary, off, size, val);
                return;
            }
        }
        if let Some(off) = port.checked_sub(self.secondary.ports.cmd_base) {
            if off < 8 {
                Self::write_cmd_reg(&mut self.secondary, off, size, val);
                return;
            }
        }
        if let Some(off) = port.checked_sub(self.primary.ports.ctrl_base) {
            if off < 2 {
                Self::write_ctrl_reg(&mut self.primary, off, val as u8);
                return;
            }
        }
        if let Some(off) = port.checked_sub(self.secondary.ports.ctrl_base) {
            if off < 2 {
                Self::write_ctrl_reg(&mut self.secondary, off, val as u8);
            }
        }
    }

    pub fn bus_master_base(&self) -> u16 {
        self.bus_master_base
    }

    /// Read from the PCI configuration space (little-endian).
    pub fn pci_config_read(&self, offset: u16, size: u8) -> u32 {
        let offset = offset as usize;
        if offset >= self.pci_regs.len() || offset + size as usize > self.pci_regs.len() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => 0xffff_ffff,
                _ => 0xffff_ffff,
            };
        }

        // BAR reads must respect size probing state and may be performed with byte/word accesses
        // (e.g. via 0xCFC+1). Model them by reading the aligned dword value and applying
        // shift/mask based on the access size.
        if (0x10..=0x23).contains(&offset) {
            let aligned = offset & !0x3;
            let bar = match aligned {
                0x10 => {
                    if self.bar0_probe {
                        // 8-byte I/O BAR.
                        !(0x08u32 - 1) | 0x01
                    } else {
                        self.bar0
                    }
                }
                0x14 => {
                    if self.bar1_probe {
                        // 4-byte I/O BAR.
                        !(0x04u32 - 1) | 0x01
                    } else {
                        self.bar1
                    }
                }
                0x18 => {
                    if self.bar2_probe {
                        // 8-byte I/O BAR.
                        !(0x08u32 - 1) | 0x01
                    } else {
                        self.bar2
                    }
                }
                0x1C => {
                    if self.bar3_probe {
                        // 4-byte I/O BAR.
                        !(0x04u32 - 1) | 0x01
                    } else {
                        self.bar3
                    }
                }
                0x20 => {
                    // BAR4: Bus Master IDE (16 bytes).
                    if self.bar4_probe {
                        !(0x10u32 - 1) | 0x01
                    } else {
                        self.bar4
                    }
                }
                _ => u32::from_le_bytes(self.pci_regs[aligned..aligned + 4].try_into().unwrap()),
            };

            let shift = (offset - aligned) * 8;
            let mask = match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0xFFFF_FFFF,
            };
            return (bar >> shift) & mask;
        }

        match size {
            1 => self.pci_regs[offset] as u32,
            2 => u16::from_le_bytes(self.pci_regs[offset..offset + 2].try_into().unwrap()) as u32,
            4 => {
                match offset {
                    0x10 => {
                        if self.bar0_probe {
                            // 8-byte I/O BAR.
                            !(0x08u32 - 1) | 0x01
                        } else {
                            self.bar0
                        }
                    }
                    0x14 => {
                        if self.bar1_probe {
                            // 4-byte I/O BAR.
                            !(0x04u32 - 1) | 0x01
                        } else {
                            self.bar1
                        }
                    }
                    0x18 => {
                        if self.bar2_probe {
                            // 8-byte I/O BAR.
                            !(0x08u32 - 1) | 0x01
                        } else {
                            self.bar2
                        }
                    }
                    0x1C => {
                        if self.bar3_probe {
                            // 4-byte I/O BAR.
                            !(0x04u32 - 1) | 0x01
                        } else {
                            self.bar3
                        }
                    }
                    0x20 => {
                        // BAR4: Bus Master IDE (16 bytes).
                        if self.bar4_probe {
                            !(0x10u32 - 1) | 0x01
                        } else {
                            self.bar4
                        }
                    }
                    _ => u32::from_le_bytes(self.pci_regs[offset..offset + 4].try_into().unwrap()),
                }
            }
            _ => 0,
        }
    }

    /// Write to the PCI configuration space (little-endian).
    pub fn pci_config_write(&mut self, offset: u16, size: u8, val: u32) {
        let offset = offset as usize;
        if offset >= self.pci_regs.len() || offset + size as usize > self.pci_regs.len() {
            return;
        }
        match size {
            1 => self.pci_regs[offset] = val as u8,
            2 => self.pci_regs[offset..offset + 2].copy_from_slice(&(val as u16).to_le_bytes()),
            4 => {
                match offset {
                    0x10 => {
                        if val == 0xffff_ffff {
                            self.bar0_probe = true;
                            self.bar0 = 0;
                        } else {
                            self.bar0_probe = false;
                            self.bar0 = (val & !(0x08u32 - 1) & 0xffff_fffc) | 0x01;
                            self.primary.ports.cmd_base = (self.bar0 & 0xffff_fffc) as u16;
                        }
                        self.pci_regs[offset..offset + 4].copy_from_slice(&self.bar0.to_le_bytes());
                        return;
                    }
                    0x14 => {
                        if val == 0xffff_ffff {
                            self.bar1_probe = true;
                            self.bar1 = 0;
                        } else {
                            self.bar1_probe = false;
                            self.bar1 = (val & !(0x04u32 - 1) & 0xffff_fffc) | 0x01;
                            let base = (self.bar1 & 0xffff_fffc) as u16;
                            self.primary.ports.ctrl_base = base.wrapping_add(2);
                        }
                        self.pci_regs[offset..offset + 4].copy_from_slice(&self.bar1.to_le_bytes());
                        return;
                    }
                    0x18 => {
                        if val == 0xffff_ffff {
                            self.bar2_probe = true;
                            self.bar2 = 0;
                        } else {
                            self.bar2_probe = false;
                            self.bar2 = (val & !(0x08u32 - 1) & 0xffff_fffc) | 0x01;
                            self.secondary.ports.cmd_base = (self.bar2 & 0xffff_fffc) as u16;
                        }
                        self.pci_regs[offset..offset + 4].copy_from_slice(&self.bar2.to_le_bytes());
                        return;
                    }
                    0x1C => {
                        if val == 0xffff_ffff {
                            self.bar3_probe = true;
                            self.bar3 = 0;
                        } else {
                            self.bar3_probe = false;
                            self.bar3 = (val & !(0x04u32 - 1) & 0xffff_fffc) | 0x01;
                            let base = (self.bar3 & 0xffff_fffc) as u16;
                            self.secondary.ports.ctrl_base = base.wrapping_add(2);
                        }
                        self.pci_regs[offset..offset + 4].copy_from_slice(&self.bar3.to_le_bytes());
                        return;
                    }
                    0x20 => {
                        // BAR4: Bus Master IDE base.
                        if val == 0xffff_ffff {
                            self.bar4_probe = true;
                            self.bar4 = 0;
                        } else {
                            self.bar4_probe = false;
                            self.bar4 = (val & !(0x10u32 - 1) & 0xffff_fffc) | 0x01;
                            self.bus_master_base = (self.bar4 & 0xffff_fffc) as u16;
                        }
                        self.pci_regs[offset..offset + 4].copy_from_slice(&self.bar4.to_le_bytes());
                        return;
                    }
                    _ => {}
                }

                self.pci_regs[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            }
            _ => {}
        }
    }

    fn decode_bus_master(&self, port: u16) -> Option<(usize, u16)> {
        let base = self.bus_master_base;
        let off = port.checked_sub(base)?;
        if off < 16 {
            let chan = (off / 8) as usize;
            let reg_off = off % 8;
            return Some((chan, reg_off));
        }
        None
    }

    fn read_cmd_reg(chan: &mut Channel, reg: u16, size: u8) -> u32 {
        match reg {
            ATA_REG_DATA => match size {
                2 => chan.data_in_u16() as u32,
                4 => chan.data_in_u32(),
                _ => 0,
            },
            ATA_REG_ERROR_FEATURES => chan.error as u32,
            ATA_REG_SECTOR_COUNT | ATA_REG_LBA0 | ATA_REG_LBA1 | ATA_REG_LBA2 | ATA_REG_DEVICE => {
                let hob = (chan.control & IDE_CTRL_HOB) != 0;
                chan.tf.read_reg(reg, hob) as u32
            }
            ATA_REG_STATUS_COMMAND => {
                // Reading STATUS clears pending IRQ.
                chan.clear_irq();
                chan.status as u32
            }
            _ => 0,
        }
    }

    fn write_cmd_reg(chan: &mut Channel, reg: u16, size: u8, val: u32) {
        match reg {
            ATA_REG_DATA => match size {
                2 => chan.data_out_u16(val as u16),
                4 => chan.data_out_u32(val),
                _ => {}
            },
            ATA_REG_ERROR_FEATURES
            | ATA_REG_SECTOR_COUNT
            | ATA_REG_LBA0
            | ATA_REG_LBA1
            | ATA_REG_LBA2
            | ATA_REG_DEVICE => {
                chan.tf.write_reg(reg, val as u8);
            }
            ATA_REG_STATUS_COMMAND => {
                Self::exec_command(chan, val as u8);
            }
            _ => {}
        }
    }

    fn read_ctrl_reg(chan: &mut Channel, reg: u16) -> u32 {
        match reg {
            ATA_CTRL_ALT_STATUS_DEVICE_CTRL => chan.status as u32,
            ATA_CTRL_DRIVE_ADDRESS => 0,
            _ => 0,
        }
    }

    fn write_ctrl_reg(chan: &mut Channel, reg: u16, val: u8) {
        if reg != ATA_CTRL_ALT_STATUS_DEVICE_CTRL {
            return;
        }
        // Handle software reset edge (SRST going 0->1 then 1->0).
        let prev = chan.control;
        chan.control = val;
        if (prev & IDE_CTRL_SRST) == 0 && (val & IDE_CTRL_SRST) != 0 {
            chan.reset();
        }
    }

    fn exec_command(chan: &mut Channel, cmd: u8) {
        chan.status |= IDE_STATUS_BSY;
        chan.status &= !IDE_STATUS_DRQ;
        chan.clear_irq();

        let is_lba48 = matches!(cmd, 0x24 | 0x34 | 0x25 | 0x35 | 0xEA);
        chan.tf.normalize_for_command(is_lba48);

        let dev_idx = chan.selected_drive() as usize;

        match cmd {
            0xEC => {
                let data = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => Some(dev.identify_data()),
                    _ => None,
                };
                if let Some(data) = data {
                    chan.begin_pio_in(TransferKind::Identify, data);
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0xA1 => {
                let data = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Atapi(dev)) => Some(dev.identify_packet_data()),
                    _ => None,
                };
                if let Some(data) = data {
                    chan.begin_pio_in(TransferKind::Identify, data);
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0x20 | 0x24 => {
                // READ SECTORS (PIO) / READ SECTORS EXT (PIO).
                let (lba, sectors) = if cmd == 0x24 {
                    (chan.tf.lba48(), chan.tf.sector_count48() as u64)
                } else {
                    (chan.tf.lba28(), chan.tf.sector_count28() as u64)
                };

                let res = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => dev.pio_read(lba, sectors).ok(),
                    _ => None,
                };

                if let Some(buf) = res {
                    chan.begin_pio_in(TransferKind::AtaPioRead, buf);
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0x30 | 0x34 => {
                // WRITE SECTORS (PIO) / WRITE SECTORS EXT (PIO).
                if matches!(chan.devices[dev_idx], Some(IdeDevice::Ata(_))) {
                    let (lba, sectors) = if cmd == 0x34 {
                        (chan.tf.lba48(), chan.tf.sector_count48() as u64)
                    } else {
                        (chan.tf.lba28(), chan.tf.sector_count28() as u64)
                    };

                    let byte_len = sectors
                        .checked_mul(SECTOR_SIZE as u64)
                        .and_then(|v| usize::try_from(v).ok())
                        .filter(|&v| v <= MAX_IDE_DATA_BUFFER_BYTES);

                    if let Some(byte_len) = byte_len {
                        chan.pio_write = Some((lba, sectors));
                        chan.begin_pio_out(TransferKind::AtaPioWrite, byte_len);
                    } else {
                        chan.set_error(0x04);
                        chan.status &= !IDE_STATUS_BSY;
                    }
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0xC8 | 0xCA | 0x25 | 0x35 => {
                // DMA commands.
                let is_write = matches!(cmd, 0xCA | 0x35);
                let (lba, sectors) = if matches!(cmd, 0x25 | 0x35) {
                    (chan.tf.lba48(), chan.tf.sector_count48() as u64)
                } else {
                    (chan.tf.lba28(), chan.tf.sector_count28() as u64)
                };

                let req = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => {
                        if is_write {
                            dev.sector_bytes(sectors)
                                .ok()
                                .map(|buf| busmaster::DmaRequest::ata_write(buf, lba, sectors))
                        } else {
                            dev.pio_read(lba, sectors)
                                .ok()
                                .map(|buf| busmaster::DmaRequest::ata_read(buf, lba, sectors))
                        }
                    }
                    _ => None,
                };

                if let Some(req) = req {
                    chan.pending_dma = Some(req);
                    chan.status &= !IDE_STATUS_BSY;
                    chan.status |= IDE_STATUS_DRDY;
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0xE7 | 0xEA => {
                let ok = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => dev.flush().is_ok(),
                    _ => false,
                };
                if ok {
                    chan.complete_non_data_command();
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0xEF => {
                let features = chan.tf.features;
                let sector_count = chan.tf.sector_count;
                let ok = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => {
                        dev.set_features(features, sector_count);
                        true
                    }
                    _ => false,
                };
                if ok {
                    chan.complete_non_data_command();
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            0xA0 => {
                // ATAPI PACKET
                if matches!(chan.devices[dev_idx], Some(IdeDevice::Atapi(_))) {
                    chan.tf.sector_count = 0x01; // IO=0, CoD=1 (packet)
                    chan.begin_pio_out(TransferKind::AtapiPacket, 12);
                } else {
                    chan.set_error(0x04);
                    chan.status &= !IDE_STATUS_BSY;
                }
            }
            _ => {
                chan.set_error(0x04); // ABRT
                chan.status &= !IDE_STATUS_BSY;
                chan.set_irq();
            }
        }
    }

    /// Execute any pending Bus Master DMA transfers.
    ///
    /// A real device would DMA asynchronously. For emulator simplicity we
    /// complete transfers synchronously when the Bus Master start bit is set.
    pub fn tick(&mut self, mem: &mut dyn MemoryBus) {
        // Gate DMA on PCI command Bus Master Enable (bit 2).
        //
        // This matches the platform-backed `Piix3IdePciDevice` behavior and real PCI semantics:
        // when the guest clears COMMAND.BME, the device must not perform bus-master DMA even if
        // the Bus Master IDE engine is started.
        if !self.bus_master_enabled() {
            return;
        }
        Self::tick_channel(&mut self.bus_master[0], &mut self.primary, mem);
        Self::tick_channel(&mut self.bus_master[1], &mut self.secondary, mem);
    }

    fn tick_channel(bm: &mut BusMasterChannel, chan: &mut Channel, mem: &mut dyn MemoryBus) {
        if !bm.is_started() {
            return;
        }
        let Some(mut req) = chan.pending_dma.take() else {
            return;
        };

        match bm.execute_dma(mem, &mut req) {
            Ok(()) => {
                // Commit writes after the DMA engine has pulled data from guest memory.
                if let Some(busmaster::DmaCommit::AtaWrite { lba, sectors }) = req.commit {
                    let dev_idx = chan.selected_drive() as usize;
                    if let Some(IdeDevice::Ata(dev)) = chan.devices[dev_idx].as_mut() {
                        let _ = dev.pio_write(lba, sectors, &req.buffer);
                    }
                }
                bm.finish_success();
                // For ATAPI DMA commands, transition to status phase (interrupt reason).
                let dev_idx = chan.selected_drive() as usize;
                if matches!(chan.devices[dev_idx], Some(IdeDevice::Atapi(_))) {
                    chan.tf.sector_count = 0x03; // IO=1, CoD=1
                }
                chan.complete_non_data_command();
            }
            Err(_) => {
                bm.finish_error();
                chan.set_error(0x04);
                chan.status &= !IDE_STATUS_BSY;
                chan.set_irq();
            }
        }
    }

    pub fn primary_irq_pending(&self) -> bool {
        self.primary.irq_pending && (self.primary.control & IDE_CTRL_NIEN) == 0
    }

    pub fn secondary_irq_pending(&self) -> bool {
        self.secondary.irq_pending && (self.secondary.control & IDE_CTRL_NIEN) == 0
    }

    pub fn clear_primary_irq(&mut self) {
        self.primary.clear_irq();
    }

    pub fn clear_secondary_irq(&mut self) {
        self.secondary.clear_irq();
    }

    pub fn snapshot_state(&self) -> IdeControllerState {
        fn snapshot_tf(tf: &TaskFile) -> IdeTaskFileState {
            IdeTaskFileState {
                features: tf.features,
                sector_count: tf.sector_count,
                lba0: tf.lba0,
                lba1: tf.lba1,
                lba2: tf.lba2,
                device: tf.device,
                hob_features: tf.hob_features,
                hob_sector_count: tf.hob_sector_count,
                hob_lba0: tf.hob_lba0,
                hob_lba1: tf.hob_lba1,
                hob_lba2: tf.hob_lba2,
                pending_features_high: tf.pending_features_high,
                pending_sector_count_high: tf.pending_sector_count_high,
                pending_lba0_high: tf.pending_lba0_high,
                pending_lba1_high: tf.pending_lba1_high,
                pending_lba2_high: tf.pending_lba2_high,
            }
        }

        fn snapshot_channel(chan: &Channel, bm: &BusMasterChannel) -> IdeChannelState {
            let data_mode = match chan.data_mode {
                DataMode::None => IdeDataMode::None,
                DataMode::PioIn => IdeDataMode::PioIn,
                DataMode::PioOut => IdeDataMode::PioOut,
            };
            let transfer_kind = chan.transfer_kind.map(|k| match k {
                TransferKind::AtaPioRead => IdeTransferKind::AtaPioRead,
                TransferKind::AtaPioWrite => IdeTransferKind::AtaPioWrite,
                TransferKind::Identify => IdeTransferKind::Identify,
                TransferKind::AtapiPacket => IdeTransferKind::AtapiPacket,
                TransferKind::AtapiPioIn => IdeTransferKind::AtapiPioIn,
            });

            let pending_dma = chan.pending_dma.as_ref().map(|req| IdeDmaRequestState {
                direction: match req.direction {
                    busmaster::DmaDirection::ToMemory => IdeDmaDirection::ToMemory,
                    busmaster::DmaDirection::FromMemory => IdeDmaDirection::FromMemory,
                },
                buffer: req.buffer.clone(),
                commit: req.commit.as_ref().map(|c| match c {
                    busmaster::DmaCommit::AtaWrite { lba, sectors } => {
                        IdeDmaCommitState::AtaWrite {
                            lba: *lba,
                            sectors: *sectors,
                        }
                    }
                }),
            });

            let pio_write = chan
                .pio_write
                .map(|(lba, sectors)| IdePioWriteState { lba, sectors });

            let drives = core::array::from_fn(|idx| match chan.devices[idx].as_ref() {
                None => IdeDriveState::None,
                Some(IdeDevice::Ata(dev)) => IdeDriveState::Ata(dev.snapshot_state()),
                Some(IdeDevice::Atapi(dev)) => IdeDriveState::Atapi(dev.snapshot_state()),
            });

            IdeChannelState {
                ports: IdePortMapState {
                    cmd_base: chan.ports.cmd_base,
                    ctrl_base: chan.ports.ctrl_base,
                    irq: chan.ports.irq,
                },
                tf: snapshot_tf(&chan.tf),
                status: chan.status,
                error: chan.error,
                control: chan.control,
                irq_pending: chan.irq_pending,
                data_mode,
                transfer_kind,
                data: chan.data.clone(),
                data_index: chan.data_index.min(u32::MAX as usize) as u32,
                pending_dma,
                pio_write,
                bus_master: bm.snapshot_state(),
                drives,
            }
        }

        IdeControllerState {
            pci: PciConfigSpaceState {
                regs: self.pci_regs,
                bar0: self.bar0,
                bar1: self.bar1,
                bar2: self.bar2,
                bar3: self.bar3,
                bar4: self.bar4,
                bar0_probe: self.bar0_probe,
                bar1_probe: self.bar1_probe,
                bar2_probe: self.bar2_probe,
                bar3_probe: self.bar3_probe,
                bar4_probe: self.bar4_probe,
                bus_master_base: self.bus_master_base,
            },
            primary: snapshot_channel(&self.primary, &self.bus_master[0]),
            secondary: snapshot_channel(&self.secondary, &self.bus_master[1]),
        }
    }

    pub fn restore_state(&mut self, state: &IdeControllerState) {
        fn restore_tf(tf: &mut TaskFile, state: &IdeTaskFileState) {
            tf.features = state.features;
            tf.sector_count = state.sector_count;
            tf.lba0 = state.lba0;
            tf.lba1 = state.lba1;
            tf.lba2 = state.lba2;
            tf.device = state.device;

            tf.hob_features = state.hob_features;
            tf.hob_sector_count = state.hob_sector_count;
            tf.hob_lba0 = state.hob_lba0;
            tf.hob_lba1 = state.hob_lba1;
            tf.hob_lba2 = state.hob_lba2;

            tf.pending_features_high = state.pending_features_high;
            tf.pending_sector_count_high = state.pending_sector_count_high;
            tf.pending_lba0_high = state.pending_lba0_high;
            tf.pending_lba1_high = state.pending_lba1_high;
            tf.pending_lba2_high = state.pending_lba2_high;
        }

        fn restore_channel(chan: &mut Channel, bm: &mut BusMasterChannel, state: &IdeChannelState) {
            chan.ports.cmd_base = state.ports.cmd_base;
            chan.ports.ctrl_base = state.ports.ctrl_base;
            chan.ports.irq = state.ports.irq;

            restore_tf(&mut chan.tf, &state.tf);

            chan.status = state.status;
            chan.error = state.error;
            chan.control = state.control;
            chan.irq_pending = state.irq_pending;

            chan.data_mode = match state.data_mode {
                IdeDataMode::None => DataMode::None,
                IdeDataMode::PioIn => DataMode::PioIn,
                IdeDataMode::PioOut => DataMode::PioOut,
            };

            chan.transfer_kind = state.transfer_kind.map(|k| match k {
                IdeTransferKind::AtaPioRead => TransferKind::AtaPioRead,
                IdeTransferKind::AtaPioWrite => TransferKind::AtaPioWrite,
                IdeTransferKind::Identify => TransferKind::Identify,
                IdeTransferKind::AtapiPacket => TransferKind::AtapiPacket,
                IdeTransferKind::AtapiPioIn => TransferKind::AtapiPioIn,
            });

            chan.data = state.data.clone();
            chan.data_index = (state.data_index as usize).min(chan.data.len());

            chan.pio_write = state.pio_write.as_ref().map(|pw| (pw.lba, pw.sectors));

            chan.pending_dma = state.pending_dma.as_ref().map(|req| busmaster::DmaRequest {
                direction: match req.direction {
                    IdeDmaDirection::ToMemory => busmaster::DmaDirection::ToMemory,
                    IdeDmaDirection::FromMemory => busmaster::DmaDirection::FromMemory,
                },
                buffer: req.buffer.clone(),
                commit: req.commit.as_ref().map(|c| match c {
                    IdeDmaCommitState::AtaWrite { lba, sectors } => {
                        busmaster::DmaCommit::AtaWrite {
                            lba: *lba,
                            sectors: *sectors,
                        }
                    }
                }),
            });

            bm.restore_state(&state.bus_master);

            // Restore per-drive state (where compatible with the currently-attached device).
            for slot in 0..2 {
                match (chan.devices[slot].as_mut(), &state.drives[slot]) {
                    (Some(IdeDevice::Ata(dev)), IdeDriveState::Ata(s)) => dev.restore_state(s),
                    (Some(IdeDevice::Atapi(dev)), IdeDriveState::Atapi(s)) => dev.restore_state(s),
                    _ => {}
                }
            }
        }

        self.pci_regs = state.pci.regs;
        self.bar0 = state.pci.bar0;
        self.bar1 = state.pci.bar1;
        self.bar2 = state.pci.bar2;
        self.bar3 = state.pci.bar3;
        self.bar4 = state.pci.bar4;
        self.bar0_probe = state.pci.bar0_probe;
        self.bar1_probe = state.pci.bar1_probe;
        self.bar2_probe = state.pci.bar2_probe;
        self.bar3_probe = state.pci.bar3_probe;
        self.bar4_probe = state.pci.bar4_probe;
        self.bus_master_base = state.pci.bus_master_base;

        restore_channel(&mut self.primary, &mut self.bus_master[0], &state.primary);
        restore_channel(
            &mut self.secondary,
            &mut self.bus_master[1],
            &state.secondary,
        );
    }
}

impl IoSnapshot for IdeController {
    const DEVICE_ID: [u8; 4] = IdeControllerState::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = IdeControllerState::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.snapshot_state().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut state = IdeControllerState::default();
        state.load_state(bytes)?;
        self.restore_state(&state);
        Ok(())
    }
}

impl PciDevice for IdeController {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        match size {
            1 | 2 | 4 => self.pci_config_read(offset, size as u8),
            _ => 0,
        }
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        match size {
            1 | 2 | 4 => self.pci_config_write(offset, size as u8, value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
