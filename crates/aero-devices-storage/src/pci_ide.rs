//! PCI PIIX3-compatible IDE controller (legacy ports + Bus Master IDE DMA).
//!
//! This device model is designed for compatibility with:
//! - BIOS-era software using legacy IDE ports (0x1F0/0x3F6 and 0x170/0x376)
//! - Windows 7 `pciide.sys` / `atapi.sys` in IDE mode (including Bus Master DMA)

use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PciConfigSpace, PciDevice};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use aero_io_snapshot::io::storage::state::{
    IdeChannelState, IdeControllerState, IdeDataMode, IdeDmaCommitState, IdeDmaDirection,
    IdeDmaRequestState, IdeDriveState, IdePioWriteState, IdePortMapState, IdeTaskFileState,
    IdeTransferKind, PciConfigSpaceState, MAX_IDE_DATA_BUFFER_BYTES,
};
use aero_platform::io::{IoPortBus, PortIoDevice};
use aero_storage::SECTOR_SIZE;
use memory::MemoryBus;

use crate::ata::AtaDrive;
use crate::atapi::{AtapiCdrom, IsoBackend, PacketResult};
use crate::busmaster::{BusMasterChannel, DmaCommit, DmaRequest};

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

/// Legacy primary/secondary I/O port assignments.
#[derive(Debug, Clone, Copy)]
pub struct IdePortMap {
    pub cmd_base: u16,
    pub ctrl_base: u16,
}

pub const PRIMARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x1F0,
    ctrl_base: 0x3F6,
};

pub const SECONDARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x170,
    ctrl_base: 0x376,
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
        if c == 0 { 256 } else { c }
    }

    fn sector_count48(&self) -> u32 {
        let c = ((self.hob_sector_count as u32) << 8) | self.sector_count as u32;
        if c == 0 { 65536 } else { c }
    }
}

enum IdeDevice {
    Ata(AtaDrive),
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
    pending_dma: Option<DmaRequest>,

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
        self.data = vec![0u8; len];
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
                    let _ = ata_pio_write(dev, lba, sectors, &data);
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
                    PacketResult::DataIn(buf) => {
                        // ATAPI uses sector_count as interrupt reason; IO=1, CoD=0.
                        self.tf.sector_count = 0x02;
                        let byte_count = buf.len().min(u16::MAX as usize) as u16;
                        self.tf.lba1 = (byte_count & 0xFF) as u8;
                        self.tf.lba2 = (byte_count >> 8) as u8;
                        self.begin_pio_in(TransferKind::AtapiPioIn, buf);
                    }
                    PacketResult::NoDataSuccess => {
                        self.tf.sector_count = 0x03; // IO=1, CoD=1 (status)
                        self.complete_non_data_command();
                    }
                    PacketResult::Error { .. } => {
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
                    PacketResult::DmaIn(buf) => {
                        // Queue a DMA transfer; completion will raise IRQ.
                        self.tf.sector_count = 0x02;
                        self.pending_dma = Some(DmaRequest::atapi_data_in(buf));
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
            Some(TransferKind::Identify) | Some(TransferKind::AtaPioRead) | None => {
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
}

impl IdeController {
    pub fn new(bus_master_base: u16) -> Self {
        Self {
            primary: Channel::new(PRIMARY_PORTS),
            secondary: Channel::new(SECONDARY_PORTS),
            bus_master_base,
            bus_master: [BusMasterChannel::new(), BusMasterChannel::new()],
        }
    }

    pub fn bus_master_base(&self) -> u16 {
        self.bus_master_base
    }

    pub fn set_bus_master_base(&mut self, base: u16) {
        self.bus_master_base = base;
    }

    pub fn attach_primary_master_ata(&mut self, drive: AtaDrive) {
        self.primary.devices[0] = Some(IdeDevice::Ata(drive));
        self.bus_master[0].set_drive_dma_capable(0, true);
    }

    pub fn attach_secondary_master_ata(&mut self, drive: AtaDrive) {
        self.secondary.devices[0] = Some(IdeDevice::Ata(drive));
        self.bus_master[1].set_drive_dma_capable(0, true);
    }

    pub fn attach_primary_master_atapi(&mut self, dev: AtapiCdrom) {
        let dma = dev.supports_dma();
        self.primary.devices[0] = Some(IdeDevice::Atapi(dev));
        self.bus_master[0].set_drive_dma_capable(0, dma);
    }

    pub fn attach_secondary_master_atapi(&mut self, dev: AtapiCdrom) {
        let dma = dev.supports_dma();
        self.secondary.devices[0] = Some(IdeDevice::Atapi(dev));
        self.bus_master[1].set_drive_dma_capable(0, dma);
    }

    /// Re-attaches a host ISO backend to an existing ATAPI device without changing guest-visible
    /// media state.
    ///
    /// This is intended for snapshot restore: the controller snapshot restores the ATAPI device's
    /// internal state (tray/sense/media_changed) but drops the host backend reference.
    pub fn attach_primary_master_atapi_backend_for_restore(&mut self, backend: Box<dyn IsoBackend>) {
        match self.primary.devices[0].as_mut() {
            Some(IdeDevice::Atapi(dev)) => {
                dev.attach_backend_for_restore(backend);
                self.bus_master[0].set_drive_dma_capable(0, dev.supports_dma());
            }
            _ => {
                let dev = AtapiCdrom::new(Some(backend));
                let dma = dev.supports_dma();
                self.primary.devices[0] = Some(IdeDevice::Atapi(dev));
                self.bus_master[0].set_drive_dma_capable(0, dma);
            }
        }
    }

    pub fn attach_secondary_master_atapi_backend_for_restore(
        &mut self,
        backend: Box<dyn IsoBackend>,
    ) {
        match self.secondary.devices[0].as_mut() {
            Some(IdeDevice::Atapi(dev)) => {
                dev.attach_backend_for_restore(backend);
                self.bus_master[1].set_drive_dma_capable(0, dev.supports_dma());
            }
            _ => {
                let dev = AtapiCdrom::new(Some(backend));
                let dma = dev.supports_dma();
                self.secondary.devices[0] = Some(IdeDevice::Atapi(dev));
                self.bus_master[1].set_drive_dma_capable(0, dma);
            }
        }
    }

    fn decode_bus_master(&self, port: u16) -> Option<(usize, u16)> {
        let base = self.bus_master_base;
        if port >= base && port < base + 16 {
            let off = port - base;
            let chan = (off / 8) as usize;
            let reg_off = off % 8;
            return Some((chan, reg_off));
        }
        None
    }

    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        // Command blocks.
        if port >= self.primary.ports.cmd_base && port < self.primary.ports.cmd_base + 8 {
            let reg = port - self.primary.ports.cmd_base;
            return Self::read_cmd_reg(&mut self.primary, reg, size);
        }
        if port >= self.secondary.ports.cmd_base && port < self.secondary.ports.cmd_base + 8 {
            let reg = port - self.secondary.ports.cmd_base;
            return Self::read_cmd_reg(&mut self.secondary, reg, size);
        }

        // Control blocks.
        if port >= self.primary.ports.ctrl_base && port < self.primary.ports.ctrl_base + 2 {
            let reg = port - self.primary.ports.ctrl_base;
            return Self::read_ctrl_reg(&mut self.primary, reg);
        }
        if port >= self.secondary.ports.ctrl_base && port < self.secondary.ports.ctrl_base + 2 {
            let reg = port - self.secondary.ports.ctrl_base;
            return Self::read_ctrl_reg(&mut self.secondary, reg);
        }

        // Bus master.
        if let Some((chan, reg_off)) = self.decode_bus_master(port) {
            return self.bus_master[chan].read(reg_off, size);
        }

        // Open bus.
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn io_write(&mut self, port: u16, size: u8, val: u32) {
        // Command blocks.
        if port >= self.primary.ports.cmd_base && port < self.primary.ports.cmd_base + 8 {
            let reg = port - self.primary.ports.cmd_base;
            Self::write_cmd_reg(&mut self.primary, reg, size, val);
            return;
        }
        if port >= self.secondary.ports.cmd_base && port < self.secondary.ports.cmd_base + 8 {
            let reg = port - self.secondary.ports.cmd_base;
            Self::write_cmd_reg(&mut self.secondary, reg, size, val);
            return;
        }

        // Control blocks.
        if port >= self.primary.ports.ctrl_base && port < self.primary.ports.ctrl_base + 2 {
            let reg = port - self.primary.ports.ctrl_base;
            Self::write_ctrl_reg(&mut self.primary, reg, val as u8);
            return;
        }
        if port >= self.secondary.ports.ctrl_base && port < self.secondary.ports.ctrl_base + 2 {
            let reg = port - self.secondary.ports.ctrl_base;
            Self::write_ctrl_reg(&mut self.secondary, reg, val as u8);
            return;
        }

        // Bus master.
        if let Some((chan, reg_off)) = self.decode_bus_master(port) {
            self.bus_master[chan].write(reg_off, size, val);
            return;
        }
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
                // IDENTIFY DEVICE
                let data = match chan.devices[dev_idx].as_ref() {
                    Some(IdeDevice::Ata(_)) => Some(ata_identify_data(chan.devices[dev_idx].as_ref())),
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
                // IDENTIFY PACKET DEVICE
                let data = match chan.devices[dev_idx].as_ref() {
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
                    Some(IdeDevice::Ata(dev)) => ata_pio_read(dev, lba, sectors).ok(),
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
                            let byte_len = sectors
                                .checked_mul(SECTOR_SIZE as u64)
                                .and_then(|v| usize::try_from(v).ok())
                                .filter(|&v| v <= MAX_IDE_DATA_BUFFER_BYTES);

                            byte_len.map(|len| DmaRequest::ata_write(vec![0u8; len], lba, sectors))
                        } else {
                            ata_pio_read(dev, lba, sectors)
                                .ok()
                                .map(DmaRequest::ata_read)
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
                // FLUSH CACHE / FLUSH CACHE EXT
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
                // SET FEATURES
                let features = chan.tf.features;
                let sector_count = chan.tf.sector_count;
                let ok = match chan.devices[dev_idx].as_mut() {
                    Some(IdeDevice::Ata(dev)) => {
                        match features {
                            0x02 => dev.set_write_cache_enabled(true),
                            0x82 => dev.set_write_cache_enabled(false),
                            0x03 => {
                                // Set transfer mode - accept but ignore.
                                let _ = sector_count;
                            }
                            _ => {}
                        }
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
                if let Some(DmaCommit::AtaWrite { lba, sectors }) = req.commit {
                    let dev_idx = chan.selected_drive() as usize;
                    if let Some(IdeDevice::Ata(dev)) = chan.devices[dev_idx].as_mut() {
                        let _ = ata_pio_write(dev, lba, sectors, &req.buffer);
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
}

fn ata_identify_data(dev: Option<&IdeDevice>) -> Vec<u8> {
    match dev {
        Some(IdeDevice::Ata(d)) => d.identify_sector().to_vec(),
        _ => vec![0; SECTOR_SIZE],
    }
}

fn ata_pio_read(dev: &mut AtaDrive, lba: u64, sectors: u64) -> io::Result<Vec<u8>> {
    let byte_len = sectors
        .checked_mul(SECTOR_SIZE as u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "transfer too large"))?;
    if byte_len > MAX_IDE_DATA_BUFFER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transfer too large",
        ));
    }
    let mut buf = vec![0u8; byte_len];
    dev.read_sectors(lba, &mut buf)?;
    Ok(buf)
}

fn ata_pio_write(dev: &mut AtaDrive, lba: u64, sectors: u64, data: &[u8]) -> io::Result<()> {
    let byte_len = sectors
        .checked_mul(SECTOR_SIZE as u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "transfer too large"))?;
    if byte_len > MAX_IDE_DATA_BUFFER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transfer too large",
        ));
    }
    if data.len() < byte_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short write buffer",
        ));
    }
    dev.write_sectors(lba, &data[..byte_len])?;
    Ok(())
}

/// Canonical PCI function wrapper for a PIIX3-compatible IDE controller.
pub struct Piix3IdePciDevice {
    pub controller: IdeController,
    config: PciConfigSpace,
}

impl Piix3IdePciDevice {
    pub const DEFAULT_BUS_MASTER_BASE: u16 = 0xC000;

    pub fn new() -> Self {
        let mut config = IDE_PIIX3.build_config_space();

        // Provide sensible legacy defaults for firmware/BIOS-era software.
        config.set_bar_base(0, PRIMARY_PORTS.cmd_base as u64);
        config.set_bar_base(1, 0x3F4); // primary control block base; alt-status at +2 => 0x3F6
        config.set_bar_base(2, SECONDARY_PORTS.cmd_base as u64);
        config.set_bar_base(3, 0x374); // secondary control block base; alt-status at +2 => 0x376
        config.set_bar_base(4, Self::DEFAULT_BUS_MASTER_BASE as u64);

        Self {
            controller: IdeController::new(Self::DEFAULT_BUS_MASTER_BASE),
            config,
        }
    }

    pub fn bus_master_base(&self) -> u16 {
        self.config
            .bar_range(4)
            .and_then(|range| u16::try_from(range.base).ok())
            .unwrap_or_else(|| self.controller.bus_master_base())
    }

    fn sync_bus_master_base_from_config(&mut self) {
        let Some(range) = self.config.bar_range(4) else {
            return;
        };
        if range.base == 0 {
            return;
        }
        let Ok(base) = u16::try_from(range.base) else {
            return;
        };
        self.controller.set_bus_master_base(base);
    }

    pub fn tick(&mut self, mem: &mut dyn MemoryBus) {
        // Only allow the device to DMA when PCI Bus Mastering is enabled (PCI command bit 2).
        if (self.config.command() & (1 << 2)) == 0 {
            return;
        }
        self.controller.tick(mem);
    }

    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.sync_bus_master_base_from_config();
        // IO space decode is gated by PCI command bit 0.
        if (self.config.command() & 0x1) == 0 {
            return match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0xFFFF_FFFF,
            };
        }
        self.controller.io_read(port, size)
    }

    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.sync_bus_master_base_from_config();
        if (self.config.command() & 0x1) == 0 {
            return;
        }
        self.controller.io_write(port, size, value)
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

        fn snapshot_channel(chan: &Channel, bm: &BusMasterChannel, irq: u8) -> IdeChannelState {
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
                    crate::busmaster::DmaDirection::ToMemory => IdeDmaDirection::ToMemory,
                    crate::busmaster::DmaDirection::FromMemory => IdeDmaDirection::FromMemory,
                },
                buffer: req.buffer.clone(),
                commit: req.commit.as_ref().map(|c| match c {
                    DmaCommit::AtaWrite { lba, sectors } => IdeDmaCommitState::AtaWrite {
                        lba: *lba,
                        sectors: *sectors,
                    },
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
                    irq,
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

        // Snapshot the PCI config space and BAR state. `PciConfigSpaceState::bytes` stores
        // BAR dwords as raw base addresses (without the IO space indicator bit), so derive
        // guest-visible BAR register values from `bar_base` and keep the byte image in sync.
        let pci_snap = self.config.snapshot_state();
        let mut regs = pci_snap.bytes;
        let bar0 = (pci_snap.bar_base[0] as u32 & 0xFFFF_FFFC) | 0x01;
        let bar1 = (pci_snap.bar_base[1] as u32 & 0xFFFF_FFFC) | 0x01;
        let bar2 = (pci_snap.bar_base[2] as u32 & 0xFFFF_FFFC) | 0x01;
        let bar3 = (pci_snap.bar_base[3] as u32 & 0xFFFF_FFFC) | 0x01;
        let bar4 = (pci_snap.bar_base[4] as u32 & 0xFFFF_FFFC) | 0x01;

        regs[0x10..0x14].copy_from_slice(&bar0.to_le_bytes());
        regs[0x14..0x18].copy_from_slice(&bar1.to_le_bytes());
        regs[0x18..0x1C].copy_from_slice(&bar2.to_le_bytes());
        regs[0x1C..0x20].copy_from_slice(&bar3.to_le_bytes());
        regs[0x20..0x24].copy_from_slice(&bar4.to_le_bytes());

        IdeControllerState {
            pci: PciConfigSpaceState {
                regs,
                bar0,
                bar1,
                bar2,
                bar3,
                bar4,
                bar0_probe: pci_snap.bar_probe[0],
                bar1_probe: pci_snap.bar_probe[1],
                bar2_probe: pci_snap.bar_probe[2],
                bar3_probe: pci_snap.bar_probe[3],
                bar4_probe: pci_snap.bar_probe[4],
                bus_master_base: self.controller.bus_master_base,
            },
            primary: snapshot_channel(&self.controller.primary, &self.controller.bus_master[0], 14),
            secondary: snapshot_channel(
                &self.controller.secondary,
                &self.controller.bus_master[1],
                15,
            ),
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

            chan.pending_dma = state.pending_dma.as_ref().map(|req| DmaRequest {
                direction: match req.direction {
                    IdeDmaDirection::ToMemory => crate::busmaster::DmaDirection::ToMemory,
                    IdeDmaDirection::FromMemory => crate::busmaster::DmaDirection::FromMemory,
                },
                buffer: req.buffer.clone(),
                commit: req.commit.as_ref().map(|c| match c {
                    IdeDmaCommitState::AtaWrite { lba, sectors } => DmaCommit::AtaWrite {
                        lba: *lba,
                        sectors: *sectors,
                    },
                }),
            });

            bm.restore_state(&state.bus_master);

            // Restore drive-visible state but drop any host-side backends. The platform must
            // re-attach disks/ISOs after restore.
            for slot in 0..2 {
                chan.devices[slot] = None;
                bm.set_drive_dma_capable(slot, false);
            }

            for slot in 0..2 {
                match &state.drives[slot] {
                    IdeDriveState::None => {}
                    IdeDriveState::Ata(_s) => {
                        // ATA backends are host-managed (snapshotted separately). Guests may still
                        // observe DMA capability bits, so restore them conservatively.
                        bm.set_drive_dma_capable(slot, true);
                    }
                    IdeDriveState::Atapi(s) => {
                        let mut dev = AtapiCdrom::new(None);
                        dev.restore_state(s);
                        bm.set_drive_dma_capable(slot, dev.supports_dma());
                        chan.devices[slot] = Some(IdeDevice::Atapi(dev));
                    }
                }
            }
        }

        // Restore PCI config space state.
        let mut bar_base = [0u64; 6];
        let mut bar_probe = [false; 6];
        bar_base[0] = u64::from(state.pci.bar0 & 0xFFFF_FFFC);
        bar_base[1] = u64::from(state.pci.bar1 & 0xFFFF_FFFC);
        bar_base[2] = u64::from(state.pci.bar2 & 0xFFFF_FFFC);
        bar_base[3] = u64::from(state.pci.bar3 & 0xFFFF_FFFC);
        bar_base[4] = u64::from(state.pci.bar4 & 0xFFFF_FFFC);
        bar_probe[0] = state.pci.bar0_probe;
        bar_probe[1] = state.pci.bar1_probe;
        bar_probe[2] = state.pci.bar2_probe;
        bar_probe[3] = state.pci.bar3_probe;
        bar_probe[4] = state.pci.bar4_probe;

        self.config.restore_state(&aero_devices::pci::PciConfigSpaceState {
            bytes: state.pci.regs,
            bar_base,
            bar_probe,
        });

        // Keep controller and config BAR4 decode consistent.
        self.controller.bus_master_base = state.pci.bus_master_base;

        restore_channel(
            &mut self.controller.primary,
            &mut self.controller.bus_master[0],
            &state.primary,
        );
        restore_channel(
            &mut self.controller.secondary,
            &mut self.controller.bus_master[1],
            &state.secondary,
        );
    }
}

impl IoSnapshot for Piix3IdePciDevice {
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

impl Default for Piix3IdePciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for Piix3IdePciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

pub type SharedPiix3IdePciDevice = Rc<RefCell<Piix3IdePciDevice>>;

/// Per-port `PortIoDevice` view into a shared PIIX3 IDE controller.
pub struct Piix3IdePort {
    ide: SharedPiix3IdePciDevice,
    port: u16,
}

impl Piix3IdePort {
    pub fn new(ide: SharedPiix3IdePciDevice, port: u16) -> Self {
        Self { ide, port }
    }
}

impl PortIoDevice for Piix3IdePort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.ide.borrow_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.ide.borrow_mut().io_write(port, size, value);
    }
}

/// Register the PIIX3 IDE controller's legacy ports + Bus Master IDE BAR on an [`IoPortBus`].
pub fn register_piix3_ide_ports(bus: &mut IoPortBus, ide: SharedPiix3IdePciDevice) {
    // Primary command block (0x1F0..=0x1F7).
    for port in PRIMARY_PORTS.cmd_base..PRIMARY_PORTS.cmd_base + 8 {
        bus.register(port, Box::new(Piix3IdePort::new(ide.clone(), port)));
    }
    // Primary control block (alt-status/dev-ctl + drive address): 0x3F6..=0x3F7.
    for port in PRIMARY_PORTS.ctrl_base..PRIMARY_PORTS.ctrl_base + 2 {
        bus.register(port, Box::new(Piix3IdePort::new(ide.clone(), port)));
    }

    // Secondary command block (0x170..=0x177).
    for port in SECONDARY_PORTS.cmd_base..SECONDARY_PORTS.cmd_base + 8 {
        bus.register(port, Box::new(Piix3IdePort::new(ide.clone(), port)));
    }
    // Secondary control block: 0x376..=0x377.
    for port in SECONDARY_PORTS.ctrl_base..SECONDARY_PORTS.ctrl_base + 2 {
        bus.register(port, Box::new(Piix3IdePort::new(ide.clone(), port)));
    }

    // Bus Master IDE (BAR4): 16 bytes, both channels.
    let bm_base = ide.borrow().bus_master_base();
    for port in bm_base..bm_base + 16 {
        bus.register(port, Box::new(Piix3IdePort::new(ide.clone(), port)));
    }
}
