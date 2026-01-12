//! Legacy IDE (ATA) controller emulation.
//!
//! This is the classic I/O-port based interface used by BIOS bootloaders and older OSes.
//! For Phase 1 bring-up we focus on PIO transfers (sufficient to read the boot sector),
//! with enough register semantics for real-mode / early protected-mode code.

use std::io;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_storage::SECTOR_SIZE;

use crate::ata::{
    AtaDrive, ATA_CMD_FLUSH_CACHE, ATA_CMD_FLUSH_CACHE_EXT, ATA_CMD_IDENTIFY, ATA_CMD_READ_SECTORS,
    ATA_CMD_READ_SECTORS_EXT, ATA_CMD_SET_FEATURES, ATA_CMD_WRITE_SECTORS,
    ATA_CMD_WRITE_SECTORS_EXT, ATA_ERROR_ABRT, ATA_STATUS_BSY, ATA_STATUS_DRDY, ATA_STATUS_DRQ,
    ATA_STATUS_ERR,
};
use aero_devices::irq::IrqLine;

const PRIMARY_BASE: u16 = 0x1F0;
const PRIMARY_CTRL: u16 = 0x3F6;
const SECONDARY_BASE: u16 = 0x170;
const SECONDARY_CTRL: u16 = 0x376;

#[derive(Clone, Copy, Debug)]
pub enum IdeChannelId {
    Primary,
    Secondary,
}

#[derive(Default, Clone, Copy, Debug)]
struct Reg48 {
    low: u8,
    high: u8,
}

impl Reg48 {
    fn write(&mut self, val: u8) {
        // ATA 48-bit addressing uses "double writes" to the LBA and sector count registers.
        // The host writes the high byte first, then the low byte. Shifting the previous low into
        // high lets us capture this pattern without explicitly tracking phases.
        self.high = self.low;
        self.low = val;
    }

    fn read(&self, hob: bool) -> u8 {
        if hob {
            self.high
        } else {
            self.low
        }
    }

    fn full_u16(&self) -> u16 {
        (self.high as u16) << 8 | self.low as u16
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PioDirection {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PioKind {
    Identify,
    Data,
}

#[derive(Debug)]
struct PioState {
    dir: PioDirection,
    kind: PioKind,
    lba: u64,
    remaining_sectors: u32,
    buf: [u8; SECTOR_SIZE],
    index: usize,
}

impl PioState {
    fn new(dir: PioDirection, kind: PioKind, lba: u64, remaining_sectors: u32) -> Self {
        Self {
            dir,
            kind,
            lba,
            remaining_sectors,
            buf: [0u8; SECTOR_SIZE],
            index: 0,
        }
    }
}

#[derive(Debug)]
struct IdeChannel {
    // Task file registers.
    features: Reg48,
    sector_count: Reg48,
    lba_low: Reg48,
    lba_mid: Reg48,
    lba_high: Reg48,
    drive_head: u8,
    status: u8,
    error: u8,

    // Device control (alternate status port write).
    dev_ctl: u8,

    // Attached drives (0=master, 1=slave).
    drives: [Option<AtaDrive>; 2],
    selected: usize,

    // Active PIO transfer, if any.
    pio: Option<PioState>,

    irq_asserted: bool,
}

impl IdeChannel {
    fn new() -> Self {
        Self {
            features: Reg48::default(),
            sector_count: Reg48::default(),
            lba_low: Reg48::default(),
            lba_mid: Reg48::default(),
            lba_high: Reg48::default(),
            drive_head: 0xA0, // 0b1010_0000: master selected, CHS mode default.
            status: ATA_STATUS_DRDY,
            error: 0,
            dev_ctl: 0,
            drives: [None, None],
            selected: 0,
            pio: None,
            irq_asserted: false,
        }
    }

    fn interrupts_enabled(&self) -> bool {
        // Device control bit 1: nIEN (0 = enabled).
        self.dev_ctl & 0x02 == 0
    }

    fn hob(&self) -> bool {
        // Device control bit 7: HOB.
        self.dev_ctl & 0x80 != 0
    }

    fn set_irq(&mut self, irq: &dyn IrqLine, high: bool) {
        if self.irq_asserted == high {
            return;
        }
        self.irq_asserted = high;
        if self.interrupts_enabled() {
            irq.set_level(high);
        } else {
            // Keep it deasserted when interrupts are disabled.
            irq.set_level(false);
        }
    }

    fn clear_irq_on_status_read(&mut self, irq: &dyn IrqLine) {
        self.set_irq(irq, false);
    }

    fn attach_drive(&mut self, slot: usize, drive: AtaDrive) {
        self.drives[slot] = Some(drive);
    }

    fn drive_mut(&mut self) -> Option<&mut AtaDrive> {
        self.drives[self.selected].as_mut()
    }

    fn set_error(&mut self, error: u8) {
        self.error = error;
        self.status &= !ATA_STATUS_DRQ;
        self.status |= ATA_STATUS_ERR | ATA_STATUS_DRDY;
    }

    fn reset(&mut self, irq: &dyn IrqLine) {
        self.features = Reg48::default();
        self.sector_count = Reg48::default();
        self.lba_low = Reg48::default();
        self.lba_mid = Reg48::default();
        self.lba_high = Reg48::default();
        self.error = 0;
        self.status = ATA_STATUS_DRDY;
        self.pio = None;
        self.set_irq(irq, false);
    }

    fn write_reg(&mut self, offset: u16, val: u8, irq: &dyn IrqLine) {
        match offset {
            1 => self.features.write(val),
            2 => self.sector_count.write(val),
            3 => self.lba_low.write(val),
            4 => self.lba_mid.write(val),
            5 => self.lba_high.write(val),
            6 => {
                self.drive_head = val;
                self.selected = ((val >> 4) & 1) as usize;
            }
            7 => self.handle_command(val, irq),
            _ => {}
        }
    }

    fn read_reg(&mut self, offset: u16, irq: &dyn IrqLine) -> u8 {
        match offset {
            1 => self.error,
            2 => self.sector_count.read(self.hob()),
            3 => self.lba_low.read(self.hob()),
            4 => self.lba_mid.read(self.hob()),
            5 => self.lba_high.read(self.hob()),
            6 => self.drive_head,
            7 => {
                // Reading status clears the IRQ per ATA spec.
                self.clear_irq_on_status_read(irq);
                self.status
            }
            _ => 0,
        }
    }

    fn read_alt_status(&self) -> u8 {
        self.status
    }

    fn write_device_control(&mut self, val: u8, irq: &dyn IrqLine) {
        let prev = self.dev_ctl;
        self.dev_ctl = val;

        // SRST is bit 2. A 1->0 transition triggers a reset.
        let prev_srst = prev & 0x04 != 0;
        let srst = val & 0x04 != 0;
        if prev_srst && !srst {
            self.reset(irq);
        }

        // If interrupts were disabled, ensure the line is low.
        if !self.interrupts_enabled() {
            irq.set_level(false);
        } else if self.irq_asserted {
            irq.set_level(true);
        }
    }

    fn pio_read(&mut self, width: usize, irq: &dyn IrqLine) -> u32 {
        if self.status & ATA_STATUS_DRQ == 0 {
            return 0;
        }

        let Some(pio) = self.pio.as_mut() else {
            return 0;
        };
        if pio.dir != PioDirection::Read {
            return 0;
        }

        let mut out = 0u32;
        for i in 0..width {
            if pio.index >= pio.buf.len() {
                break;
            }
            out |= (pio.buf[pio.index] as u32) << (i * 8);
            pio.index += 1;
        }

        if pio.index >= pio.buf.len() {
            self.advance_after_sector_transfer(irq);
        }

        out
    }

    fn pio_write(&mut self, width: usize, val: u32, irq: &dyn IrqLine) {
        if self.status & ATA_STATUS_DRQ == 0 {
            return;
        }

        let Some(pio) = self.pio.as_mut() else {
            return;
        };
        if pio.dir != PioDirection::Write {
            return;
        }

        for i in 0..width {
            if pio.index >= pio.buf.len() {
                break;
            }
            pio.buf[pio.index] = ((val >> (i * 8)) & 0xFF) as u8;
            pio.index += 1;
        }

        if pio.index >= pio.buf.len() {
            self.advance_after_sector_transfer(irq);
        }
    }

    fn advance_after_sector_transfer(&mut self, irq: &dyn IrqLine) {
        let Some(mut pio) = self.pio.take() else {
            return;
        };

        match pio.dir {
            PioDirection::Read => {
                if pio.remaining_sectors > 1 && pio.kind == PioKind::Data {
                    pio.remaining_sectors -= 1;
                    pio.lba += 1;
                    pio.index = 0;
                    if self.load_sector_for_read(&mut pio).is_err() {
                        self.set_error(ATA_ERROR_ABRT);
                        self.set_irq(irq, true);
                        return;
                    }
                    self.pio = Some(pio);
                    // Next sector ready.
                    self.set_irq(irq, true);
                } else {
                    // Completed.
                    self.status &= !ATA_STATUS_DRQ;
                    self.status |= ATA_STATUS_DRDY;
                    self.set_irq(irq, true);
                }
            }
            PioDirection::Write => {
                if pio.kind != PioKind::Data {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }

                if self.store_sector_from_write(&pio).is_err() {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }

                if pio.remaining_sectors > 1 {
                    pio.remaining_sectors -= 1;
                    pio.lba += 1;
                    pio.index = 0;
                    pio.buf.fill(0);
                    self.pio = Some(pio);
                    // Ready for next sector.
                    self.set_irq(irq, true);
                } else {
                    self.status &= !ATA_STATUS_DRQ;
                    self.status |= ATA_STATUS_DRDY;
                    self.set_irq(irq, true);
                }
            }
        }
    }

    fn load_sector_for_read(&mut self, pio: &mut PioState) -> io::Result<()> {
        if pio.kind == PioKind::Identify {
            let Some(drive) = self.drive_mut() else {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no drive attached"));
            };
            pio.buf.copy_from_slice(drive.identify_sector());
            return Ok(());
        }

        let Some(drive) = self.drive_mut() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no drive attached"));
        };
        drive.read_sectors(pio.lba, &mut pio.buf)?;
        Ok(())
    }

    fn store_sector_from_write(&mut self, pio: &PioState) -> io::Result<()> {
        let Some(drive) = self.drive_mut() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no drive attached"));
        };
        drive.write_sectors(pio.lba, &pio.buf)?;
        Ok(())
    }

    fn handle_command(&mut self, cmd: u8, irq: &dyn IrqLine) {
        self.status |= ATA_STATUS_BSY;
        self.status &= !ATA_STATUS_ERR;
        self.error = 0;
        self.pio = None;

        let lba_mode = self.drive_head & 0x40 != 0;

        match cmd {
            ATA_CMD_IDENTIFY => {
                if self.drive_mut().is_none() {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }

                let mut pio = PioState::new(PioDirection::Read, PioKind::Identify, 0, 1);
                if self.load_sector_for_read(&mut pio).is_err() {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }
                self.pio = Some(pio);
                self.status = ATA_STATUS_DRDY | ATA_STATUS_DRQ;
                self.set_irq(irq, true);
            }
            ATA_CMD_READ_SECTORS | ATA_CMD_WRITE_SECTORS => {
                if !lba_mode {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }

                let count = match self.sector_count.low {
                    0 => 256,
                    v => v as u32,
                };
                let lba = (self.lba_low.low as u32) as u64
                    | ((self.lba_mid.low as u32 as u64) << 8)
                    | ((self.lba_high.low as u32 as u64) << 16)
                    | (((self.drive_head & 0x0F) as u64) << 24);

                self.start_pio_data(cmd, lba, count, irq);
            }
            ATA_CMD_READ_SECTORS_EXT | ATA_CMD_WRITE_SECTORS_EXT => {
                if !lba_mode {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                }

                let count = match self.sector_count.full_u16() {
                    0 => 65536,
                    v => v as u32,
                };
                let lba = (self.lba_low.low as u64)
                    | ((self.lba_mid.low as u64) << 8)
                    | ((self.lba_high.low as u64) << 16)
                    | ((self.lba_low.high as u64) << 24)
                    | ((self.lba_mid.high as u64) << 32)
                    | ((self.lba_high.high as u64) << 40);

                self.start_pio_data(cmd, lba, count, irq);
            }
            ATA_CMD_FLUSH_CACHE | ATA_CMD_FLUSH_CACHE_EXT => {
                let Some(drive) = self.drive_mut() else {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                };

                if drive.flush().is_err() {
                    self.set_error(ATA_ERROR_ABRT);
                } else {
                    self.status = ATA_STATUS_DRDY;
                }
                self.set_irq(irq, true);
            }
            ATA_CMD_SET_FEATURES => {
                let feature = self.features.low;
                let Some(drive) = self.drive_mut() else {
                    self.set_error(ATA_ERROR_ABRT);
                    self.set_irq(irq, true);
                    return;
                };

                match feature {
                    0x02 => drive.set_write_cache_enabled(true),
                    0x82 => drive.set_write_cache_enabled(false),
                    _ => {}
                }

                self.status = ATA_STATUS_DRDY;
                self.set_irq(irq, true);
            }
            _ => {
                self.set_error(ATA_ERROR_ABRT);
                self.set_irq(irq, true);
            }
        }
    }

    fn start_pio_data(&mut self, cmd: u8, lba: u64, count: u32, irq: &dyn IrqLine) {
        if self.drive_mut().is_none() {
            self.set_error(ATA_ERROR_ABRT);
            self.set_irq(irq, true);
            return;
        }

        let dir = match cmd {
            ATA_CMD_READ_SECTORS | ATA_CMD_READ_SECTORS_EXT => PioDirection::Read,
            ATA_CMD_WRITE_SECTORS | ATA_CMD_WRITE_SECTORS_EXT => PioDirection::Write,
            _ => {
                self.set_error(ATA_ERROR_ABRT);
                self.set_irq(irq, true);
                return;
            }
        };

        let mut pio = PioState::new(dir, PioKind::Data, lba, count);
        if dir == PioDirection::Read {
            if self.load_sector_for_read(&mut pio).is_err() {
                self.set_error(ATA_ERROR_ABRT);
                self.set_irq(irq, true);
                return;
            }
        } else {
            pio.buf.fill(0);
        }
        self.pio = Some(pio);
        self.status = ATA_STATUS_DRDY | ATA_STATUS_DRQ;
        self.set_irq(irq, true);
    }
}

pub struct IdeController {
    primary: IdeChannel,
    secondary: IdeChannel,
    irq14: Box<dyn IrqLine>,
    irq15: Box<dyn IrqLine>,
}

impl IdeController {
    pub fn new(irq14: Box<dyn IrqLine>, irq15: Box<dyn IrqLine>) -> Self {
        Self {
            primary: IdeChannel::new(),
            secondary: IdeChannel::new(),
            irq14,
            irq15,
        }
    }

    pub fn attach_drive(&mut self, channel: IdeChannelId, slot: usize, drive: AtaDrive) {
        match channel {
            IdeChannelId::Primary => self.primary.attach_drive(slot, drive),
            IdeChannelId::Secondary => self.secondary.attach_drive(slot, drive),
        }
    }

    fn decode_port(port: u16) -> Option<(IdeChannelId, u16, bool)> {
        // Returns (channel, offset, is_alt_status/dev_ctl).
        if (PRIMARY_BASE..=PRIMARY_BASE + 7).contains(&port) {
            return Some((IdeChannelId::Primary, port - PRIMARY_BASE, false));
        }
        if (SECONDARY_BASE..=SECONDARY_BASE + 7).contains(&port) {
            return Some((IdeChannelId::Secondary, port - SECONDARY_BASE, false));
        }
        if port == PRIMARY_CTRL || port == PRIMARY_CTRL + 1 {
            return Some((IdeChannelId::Primary, port - PRIMARY_CTRL, true));
        }
        if port == SECONDARY_CTRL || port == SECONDARY_CTRL + 1 {
            return Some((IdeChannelId::Secondary, port - SECONDARY_CTRL, true));
        }
        None
    }

    pub fn read_u8(&mut self, port: u16) -> u8 {
        let Some((chan_id, offset, ctrl)) = Self::decode_port(port) else {
            return 0;
        };
        let (channel, irq): (&mut IdeChannel, &dyn IrqLine) = match chan_id {
            IdeChannelId::Primary => (&mut self.primary, &*self.irq14),
            IdeChannelId::Secondary => (&mut self.secondary, &*self.irq15),
        };

        if !ctrl {
            if offset == 0 {
                return channel.pio_read(1, irq) as u8;
            }
            return channel.read_reg(offset, irq);
        }

        match offset {
            0 => channel.read_alt_status(),
            1 => 0, // drive address, not implemented
            _ => 0,
        }
    }

    pub fn read_u16(&mut self, port: u16) -> u16 {
        let Some((chan_id, offset, ctrl)) = Self::decode_port(port) else {
            return 0;
        };
        let (channel, irq): (&mut IdeChannel, &dyn IrqLine) = match chan_id {
            IdeChannelId::Primary => (&mut self.primary, &*self.irq14),
            IdeChannelId::Secondary => (&mut self.secondary, &*self.irq15),
        };

        if ctrl || offset != 0 {
            return (self.read_u8(port) as u16) | ((self.read_u8(port) as u16) << 8);
        }

        channel.pio_read(2, irq) as u16
    }

    pub fn write_u8(&mut self, port: u16, val: u8) {
        let Some((chan_id, offset, ctrl)) = Self::decode_port(port) else {
            return;
        };
        let (channel, irq): (&mut IdeChannel, &dyn IrqLine) = match chan_id {
            IdeChannelId::Primary => (&mut self.primary, &*self.irq14),
            IdeChannelId::Secondary => (&mut self.secondary, &*self.irq15),
        };

        if !ctrl {
            if offset == 0 {
                channel.pio_write(1, val as u32, irq);
            } else {
                channel.write_reg(offset, val, irq);
            }
            return;
        }

        if offset == 0 {
            channel.write_device_control(val, irq);
        }
    }

    pub fn write_u16(&mut self, port: u16, val: u16) {
        let Some((chan_id, offset, ctrl)) = Self::decode_port(port) else {
            return;
        };
        let (channel, irq): (&mut IdeChannel, &dyn IrqLine) = match chan_id {
            IdeChannelId::Primary => (&mut self.primary, &*self.irq14),
            IdeChannelId::Secondary => (&mut self.secondary, &*self.irq15),
        };

        if ctrl || offset != 0 {
            self.write_u8(port, (val & 0xFF) as u8);
            self.write_u8(port, (val >> 8) as u8);
            return;
        }

        channel.pio_write(2, val as u32, irq);
    }
}

impl IdeChannel {
    fn sync_irq_line(&self, irq: &dyn IrqLine) {
        let should_high = self.irq_asserted && self.interrupts_enabled();
        irq.set_level(should_high);
    }
}

impl IdeController {
    fn encode_channel(chan: &IdeChannel) -> Vec<u8> {
        let mut e = Encoder::new()
            .u8(chan.features.low)
            .u8(chan.features.high)
            .u8(chan.sector_count.low)
            .u8(chan.sector_count.high)
            .u8(chan.lba_low.low)
            .u8(chan.lba_low.high)
            .u8(chan.lba_mid.low)
            .u8(chan.lba_mid.high)
            .u8(chan.lba_high.low)
            .u8(chan.lba_high.high)
            .u8(chan.drive_head)
            .u8(chan.status)
            .u8(chan.error)
            .u8(chan.dev_ctl)
            .bool(chan.irq_asserted);

        match &chan.pio {
            None => {
                e = e.u8(0);
            }
            Some(pio) => {
                let dir = match pio.dir {
                    PioDirection::Read => 0u8,
                    PioDirection::Write => 1u8,
                };
                let kind = match pio.kind {
                    PioKind::Identify => 0u8,
                    PioKind::Data => 1u8,
                };
                e = e
                    .u8(1)
                    .u8(dir)
                    .u8(kind)
                    .u64(pio.lba)
                    .u32(pio.remaining_sectors)
                    .u32(pio.index as u32)
                    .bytes(&pio.buf);
            }
        }

        e.finish()
    }

    fn decode_channel(bytes: &[u8]) -> SnapshotResult<IdeChannel> {
        let mut d = Decoder::new(bytes);
        let mut chan = IdeChannel::new();

        chan.features.low = d.u8()?;
        chan.features.high = d.u8()?;
        chan.sector_count.low = d.u8()?;
        chan.sector_count.high = d.u8()?;
        chan.lba_low.low = d.u8()?;
        chan.lba_low.high = d.u8()?;
        chan.lba_mid.low = d.u8()?;
        chan.lba_mid.high = d.u8()?;
        chan.lba_high.low = d.u8()?;
        chan.lba_high.high = d.u8()?;
        chan.drive_head = d.u8()?;
        chan.selected = ((chan.drive_head >> 4) & 1) as usize;
        chan.status = d.u8()?;
        chan.error = d.u8()?;
        chan.dev_ctl = d.u8()?;
        chan.irq_asserted = d.bool()?;

        match d.u8()? {
            0 => {
                chan.pio = None;
            }
            1 => {
                let dir = match d.u8()? {
                    0 => PioDirection::Read,
                    1 => PioDirection::Write,
                    _ => return Err(SnapshotError::InvalidFieldEncoding("idep pio dir")),
                };
                let kind = match d.u8()? {
                    0 => PioKind::Identify,
                    1 => PioKind::Data,
                    _ => return Err(SnapshotError::InvalidFieldEncoding("idep pio kind")),
                };
                let lba = d.u64()?;
                let remaining_sectors = d.u32()?;
                if remaining_sectors == 0 {
                    return Err(SnapshotError::InvalidFieldEncoding("idep pio sectors"));
                }
                let index = d.u32()? as usize;
                if index > SECTOR_SIZE {
                    return Err(SnapshotError::InvalidFieldEncoding("idep pio index"));
                }
                let buf_bytes = d.bytes(SECTOR_SIZE)?;
                let mut buf = [0u8; SECTOR_SIZE];
                buf.copy_from_slice(buf_bytes);

                chan.pio = Some(PioState {
                    dir,
                    kind,
                    lba,
                    remaining_sectors,
                    buf,
                    index,
                });
            }
            _ => return Err(SnapshotError::InvalidFieldEncoding("idep pio present")),
        }

        d.finish()?;
        Ok(chan)
    }
}

impl IoSnapshot for IdeController {
    const DEVICE_ID: [u8; 4] = *b"IDEP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PRIMARY: u16 = 1;
        const TAG_SECONDARY: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_PRIMARY, Self::encode_channel(&self.primary));
        w.field_bytes(TAG_SECONDARY, Self::encode_channel(&self.secondary));
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PRIMARY: u16 = 1;
        const TAG_SECONDARY: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset to a deterministic baseline and drop any host-side drive backends. The platform
        // should re-attach disks after restore.
        self.primary = IdeChannel::new();
        self.secondary = IdeChannel::new();

        if let Some(buf) = r.bytes(TAG_PRIMARY) {
            self.primary = Self::decode_channel(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_SECONDARY) {
            self.secondary = Self::decode_channel(buf)?;
        }

        // Ensure the external IRQ lines reflect the restored latch state.
        self.primary.sync_irq_line(&*self.irq14);
        self.secondary.sync_irq_line(&*self.irq15);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TestIrqLine;

    fn setup_controller() -> (IdeController, TestIrqLine, TestIrqLine) {
        let irq14 = TestIrqLine::default();
        let irq15 = TestIrqLine::default();
        let mut ctl = IdeController::new(Box::new(irq14.clone()), Box::new(irq15.clone()));

        use aero_storage::{MemBackend, RawDisk, VirtualDisk};

        let capacity = 16 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[1, 2, 3, 4]);
        disk.write_sectors(1, &sector).unwrap();

        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        ctl.attach_drive(IdeChannelId::Primary, 0, drive);

        (ctl, irq14, irq15)
    }

    #[test]
    fn identify_works() {
        let (mut ctl, irq14, _irq15) = setup_controller();

        // Select LBA mode + master.
        ctl.write_u8(PRIMARY_BASE + 6, 0xE0);
        ctl.write_u8(PRIMARY_BASE + 7, ATA_CMD_IDENTIFY);

        assert!(irq14.level());

        let mut buf = [0u8; SECTOR_SIZE];
        for i in 0..(SECTOR_SIZE / 2) {
            let w = ctl.read_u16(PRIMARY_BASE);
            buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
        }

        // Signature: word 0 low byte should be 0x40.
        assert_eq!(buf[0], 0x40);
        // Reading status clears IRQ.
        let _ = ctl.read_u8(PRIMARY_BASE + 7);
        assert!(!irq14.level());
    }

    #[test]
    fn read_sector_pio() {
        let (mut ctl, irq14, _irq15) = setup_controller();

        // Read sector 1 via READ SECTORS.
        ctl.write_u8(PRIMARY_BASE + 6, 0xE0); // LBA
        ctl.write_u8(PRIMARY_BASE + 2, 1); // count
        ctl.write_u8(PRIMARY_BASE + 3, 1); // lba low
        ctl.write_u8(PRIMARY_BASE + 4, 0);
        ctl.write_u8(PRIMARY_BASE + 5, 0);
        ctl.write_u8(PRIMARY_BASE + 7, ATA_CMD_READ_SECTORS);

        assert!(irq14.level());

        // First 4 bytes of sector 1 are [1,2,3,4].
        let b0 = ctl.read_u8(PRIMARY_BASE);
        let b1 = ctl.read_u8(PRIMARY_BASE);
        let b2 = ctl.read_u8(PRIMARY_BASE);
        let b3 = ctl.read_u8(PRIMARY_BASE);
        assert_eq!([b0, b1, b2, b3], [1, 2, 3, 4]);
    }

    #[test]
    fn snapshot_roundtrip_preserves_pio_progress_and_irq_state() {
        let (mut ctl, irq14, _irq15) = setup_controller();

        // Read sector 1 via READ SECTORS.
        ctl.write_u8(PRIMARY_BASE + 6, 0xE0); // LBA
        ctl.write_u8(PRIMARY_BASE + 2, 1); // count
        ctl.write_u8(PRIMARY_BASE + 3, 1); // lba low
        ctl.write_u8(PRIMARY_BASE + 4, 0);
        ctl.write_u8(PRIMARY_BASE + 5, 0);
        ctl.write_u8(PRIMARY_BASE + 7, ATA_CMD_READ_SECTORS);

        assert!(irq14.level());

        // Consume first 4 bytes.
        let b0 = ctl.read_u8(PRIMARY_BASE);
        let b1 = ctl.read_u8(PRIMARY_BASE);
        let b2 = ctl.read_u8(PRIMARY_BASE);
        let b3 = ctl.read_u8(PRIMARY_BASE);
        assert_eq!([b0, b1, b2, b3], [1, 2, 3, 4]);

        let snap = ctl.save_state();

        let irq14_2 = TestIrqLine::default();
        let irq15_2 = TestIrqLine::default();
        let mut restored = IdeController::new(Box::new(irq14_2.clone()), Box::new(irq15_2));
        restored.load_state(&snap).unwrap();

        // IRQ should still be asserted after restore.
        assert!(irq14_2.level());

        // Next byte is zero (rest of sector was zero-filled).
        let next = restored.read_u8(PRIMARY_BASE);
        assert_eq!(next, 0);

        // Drain the remainder of the sector.
        for _ in 0..(SECTOR_SIZE - 5) {
            let _ = restored.read_u8(PRIMARY_BASE);
        }

        // Reading STATUS clears IRQ.
        let _ = restored.read_u8(PRIMARY_BASE + 7);
        assert!(!irq14_2.level());
    }
}
