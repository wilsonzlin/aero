//! Minimal UHCI (USB 1.1) host controller.
//!
//! This implementation is focused on:
//! - Frame list / QH / TD walking
//! - Endpoint 0 control transfers
//! - Interrupt IN polling for HID reports
//! - Root hub with two ports exposed via PORTSC registers

mod schedule;

pub mod regs;

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::hub::RootHub;
use crate::memory::MemoryBus;

use regs::*;
use schedule::{process_frame, ScheduleContext};

pub struct UhciController {
    regs: UhciRegs,
    hub: RootHub,
    irq_level: bool,
    prev_port_resume_detect: bool,
}

impl UhciController {
    pub fn new() -> Self {
        Self {
            regs: UhciRegs::new(),
            hub: RootHub::new(),
            irq_level: false,
            prev_port_resume_detect: false,
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn hub_mut(&mut self) -> &mut RootHub {
        &mut self.hub
    }

    pub fn hub(&self) -> &RootHub {
        &self.hub
    }

    /// Traverse attached USB topology and clear any host-side asynchronous state that cannot be
    /// resumed after restoring a snapshot (e.g. WebUSB passthrough actions backed by JS Promises).
    ///
    /// This does not alter guest-visible USB state.
    pub fn reset_host_state_for_restore(&mut self) {
        for port in 0..2 {
            if let Some(mut dev) = self.hub.port_device_mut(port) {
                dev.reset_host_state_for_restore();
            }
        }
    }

    pub fn regs(&self) -> &UhciRegs {
        &self.regs
    }

    /// Forces status bits in USBSTS for tests and diagnostics.
    ///
    /// Reserved bits are masked out; the HCHALTED bit is driven by `USBCMD.RS` and should not be
    /// set manually (it is derived from the controller run/suspend state).
    pub fn set_usbsts_bits(&mut self, bits: u16) {
        let bits = bits & (USBSTS_READ_MASK & !USBSTS_HCHALTED);
        if bits & USBSTS_USBINT != 0 {
            self.regs.usbint_causes |= USBINT_CAUSE_IOC | USBINT_CAUSE_SHORT_PACKET;
        }
        self.regs.usbsts |= bits;
        self.update_irq();
    }

    fn reset(&mut self) {
        self.regs = UhciRegs::new();
        self.irq_level = false;
        self.prev_port_resume_detect = false;
    }

    fn update_irq(&mut self) {
        let mut pending = false;
        if self.regs.usbsts & USBSTS_USBINT != 0
            && ((self.regs.usbint_causes & USBINT_CAUSE_IOC != 0
                && self.regs.usbintr & USBINTR_IOC != 0)
                || (self.regs.usbint_causes & USBINT_CAUSE_SHORT_PACKET != 0
                    && self.regs.usbintr & USBINTR_SHORT_PACKET != 0))
        {
            pending = true;
        }
        if self.regs.usbsts & USBSTS_USBERRINT != 0 && self.regs.usbintr & USBINTR_TIMEOUT_CRC != 0
        {
            pending = true;
        }
        if self.regs.usbsts & USBSTS_RESUMEDETECT != 0 && self.regs.usbintr & USBINTR_RESUME != 0 {
            pending = true;
        }
        self.irq_level = pending;
    }

    fn write_usbcmd(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.1 "USB Command (USBCMD)".
        if value & USBCMD_HCRESET != 0 {
            self.reset();
            return;
        }

        let prev = self.regs.usbcmd;
        let mut cmd = value & USBCMD_WRITE_MASK;

        // Global reset is latched in USBCMD (software clears it), but the act of *setting*
        // the bit resets controller state.
        if cmd & USBCMD_GRESET != 0 && prev & USBCMD_GRESET == 0 {
            self.reset();
            self.hub.bus_reset();
        }

        // Force Global Resume latches in USBCMD; raising it latches RESUMEDETECT in USBSTS.
        if cmd & USBCMD_FGR != 0 && prev & USBCMD_FGR == 0 {
            self.regs.usbsts |= USBSTS_RESUMEDETECT;
        }

        // While global reset is asserted the controller shouldn't be running.
        if cmd & USBCMD_GRESET != 0 {
            cmd &= !USBCMD_RS;
        }

        self.regs.usbcmd = cmd;
        self.regs.update_halted();
    }

    fn write_usbsts(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.2 "USB Status (USBSTS)".
        // Write-1-to-clear status bits.
        let w1c = value & USBSTS_W1C_MASK;
        self.regs.usbsts &= !w1c;
        self.regs.usbsts &= USBSTS_READ_MASK;
        if w1c & USBSTS_USBINT != 0 {
            self.regs.usbint_causes = 0;
        }
    }

    fn write_usbintr(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.3 "USB Interrupt Enable (USBINTR)".
        self.regs.usbintr = value & USBINTR_MASK;
    }

    fn write_frnum(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.4 "Frame Number (FRNUM)".
        self.regs.frnum = value & 0x07ff;
    }

    fn write_flbaseadd(&mut self, value: u32) {
        // UHCI 1.1 spec, section 2.1.5 "Frame List Base Address (FLBASEADD)".
        self.regs.flbaseadd = value & 0xffff_f000;
    }

    fn io_read_u8(&self, offset: u16) -> u8 {
        const REG_USBCMD_HI: u16 = REG_USBCMD + 1;
        const REG_USBSTS_HI: u16 = REG_USBSTS + 1;
        const REG_USBINTR_HI: u16 = REG_USBINTR + 1;
        const REG_FRNUM_HI: u16 = REG_FRNUM + 1;
        const REG_FLBASEADD_END: u16 = REG_FLBASEADD + 3;
        const REG_PORTSC1_HI: u16 = REG_PORTSC1 + 1;
        const REG_PORTSC2_HI: u16 = REG_PORTSC2 + 1;

        let usbsts = self.regs.usbsts & USBSTS_READ_MASK;
        let usbintr = self.regs.usbintr & USBINTR_MASK;

        match offset {
            REG_USBCMD => (self.regs.usbcmd & 0x00ff) as u8,
            REG_USBCMD_HI => (self.regs.usbcmd >> 8) as u8,
            REG_USBSTS => (usbsts & 0x00ff) as u8,
            REG_USBSTS_HI => (usbsts >> 8) as u8,
            REG_USBINTR => (usbintr & 0x00ff) as u8,
            REG_USBINTR_HI => (usbintr >> 8) as u8,
            REG_FRNUM => (self.regs.frnum & 0x00ff) as u8,
            REG_FRNUM_HI => (self.regs.frnum >> 8) as u8,
            REG_FLBASEADD..=REG_FLBASEADD_END => {
                let shift = (offset - REG_FLBASEADD) * 8;
                (self.regs.flbaseadd >> shift) as u8
            }
            REG_SOFMOD => self.regs.sofmod,
            REG_PORTSC1 => (self.hub.read_portsc(0) & 0x00ff) as u8,
            REG_PORTSC1_HI => (self.hub.read_portsc(0) >> 8) as u8,
            REG_PORTSC2 => (self.hub.read_portsc(1) & 0x00ff) as u8,
            REG_PORTSC2_HI => (self.hub.read_portsc(1) >> 8) as u8,
            // The UHCI register block is 0x20 bytes wide; bytes that are within that range but
            // not implemented are reserved and should read back as 0. Out-of-range accesses are
            // treated as open bus.
            _ => {
                if offset < 0x20 {
                    0
                } else {
                    0xff
                }
            }
        }
    }

    fn io_write_u8(&mut self, offset: u16, value: u8) {
        const REG_USBCMD_HI: u16 = REG_USBCMD + 1;
        const REG_USBSTS_HI: u16 = REG_USBSTS + 1;
        const REG_USBINTR_HI: u16 = REG_USBINTR + 1;
        const REG_FRNUM_HI: u16 = REG_FRNUM + 1;
        const REG_FLBASEADD_END: u16 = REG_FLBASEADD + 3;
        const REG_PORTSC1_HI: u16 = REG_PORTSC1 + 1;
        const REG_PORTSC2_HI: u16 = REG_PORTSC2 + 1;

        match offset {
            REG_USBCMD => {
                let v = (self.regs.usbcmd & 0xff00) | (value as u16);
                self.write_usbcmd(v);
            }
            REG_USBCMD_HI => {
                let v = (self.regs.usbcmd & 0x00ff) | ((value as u16) << 8);
                self.write_usbcmd(v);
            }
            REG_USBSTS => {
                self.write_usbsts(value as u16);
            }
            REG_USBSTS_HI => {
                self.write_usbsts((value as u16) << 8);
            }
            REG_USBINTR => {
                let v = (self.regs.usbintr & 0xff00) | (value as u16);
                self.write_usbintr(v);
            }
            REG_USBINTR_HI => {
                let v = (self.regs.usbintr & 0x00ff) | ((value as u16) << 8);
                self.write_usbintr(v);
            }
            REG_FRNUM => {
                let v = (self.regs.frnum & 0xff00) | (value as u16);
                self.write_frnum(v);
            }
            REG_FRNUM_HI => {
                let v = (self.regs.frnum & 0x00ff) | ((value as u16) << 8);
                self.write_frnum(v);
            }
            REG_FLBASEADD..=REG_FLBASEADD_END => {
                let shift = (offset - REG_FLBASEADD) * 8;
                let mask = !(0xffu32 << shift);
                let v = (self.regs.flbaseadd & mask) | ((value as u32) << shift);
                self.write_flbaseadd(v);
            }
            REG_SOFMOD => self.regs.sofmod = value,
            REG_PORTSC1 => {
                self.hub.write_portsc_masked(0, value as u16, 0x00ff);
            }
            REG_PORTSC1_HI => {
                // Use a masked write so high-byte stores don't inadvertently clear W1C bits in
                // the low byte (CSC/PEDC).
                self.hub.write_portsc_masked(0, (value as u16) << 8, 0xff00);
            }
            REG_PORTSC2 => {
                self.hub.write_portsc_masked(1, value as u16, 0x00ff);
            }
            REG_PORTSC2_HI => {
                self.hub.write_portsc_masked(1, (value as u16) << 8, 0xff00);
            }
            _ => {}
        }
    }

    pub fn io_read(&self, offset: u16, size: usize) -> u32 {
        let size = size.min(4);
        if size == 0 {
            return 0;
        }

        // Treat invalid/out-of-range reads as open bus. Do not allow `offset + i` to wrap around
        // (e.g. `0xffff + 1 == 0`) and alias valid registers.
        let open_bus = if size >= 4 {
            u32::MAX
        } else {
            (1u32 << (size * 8)) - 1
        };
        let last = (size - 1) as u16;
        if offset.checked_add(last).is_none() {
            return open_bus;
        }

        let mut out = 0u32;
        for i in 0..size {
            let Some(off) = offset.checked_add(i as u16) else {
                return open_bus;
            };
            out |= (self.io_read_u8(off) as u32) << (i * 8);
        }
        out & open_bus
    }

    pub fn io_write(&mut self, offset: u16, size: usize, value: u32) {
        let size_bytes = size.min(4);
        if size_bytes == 0 {
            return;
        }
        let last = (size_bytes - 1) as u16;
        if offset.checked_add(last).is_none() {
            // Overflowing offsets are treated as out-of-range.
            return;
        }

        match (offset, size) {
            (REG_USBCMD, 2) => self.write_usbcmd(value as u16),
            (REG_USBSTS, 2) => self.write_usbsts(value as u16),
            (REG_USBINTR, 2) => self.write_usbintr(value as u16),
            (REG_FRNUM, 2) => self.write_frnum(value as u16),
            (REG_FLBASEADD, 4) => self.write_flbaseadd(value),
            (REG_SOFMOD, 1) => self.regs.sofmod = value as u8,
            (REG_PORTSC1, 2) => self.hub.write_portsc(0, value as u16),
            (REG_PORTSC2, 2) => self.hub.write_portsc(1, value as u16),
            _ => {
                for i in 0..size_bytes {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    let Some(off) = offset.checked_add(i as u16) else {
                        break;
                    };
                    self.io_write_u8(off, byte);
                }
            }
        }
        self.update_irq();
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        self.hub.tick_1ms();
        // Latch global resume-detect status if a port asserts its Resume Detect (RD) bit.
        //
        // This provides a realistic interrupt path for remote-wake style flows where software
        // enables USBINTR.RESUME and expects USBSTS.RESUMEDETECT to latch from port events.
        const PORTSC_RD: u16 = 1 << 6;
        let rd = (self.hub.read_portsc(0) | self.hub.read_portsc(1)) & PORTSC_RD != 0;
        if rd && !self.prev_port_resume_detect {
            self.regs.usbsts |= USBSTS_RESUMEDETECT;
        }
        self.prev_port_resume_detect = rd;

        if self.regs.usbcmd & (USBCMD_RS | USBCMD_EGSM) != USBCMD_RS {
            self.regs.update_halted();
            self.update_irq();
            return;
        }

        self.regs.update_halted();

        let frame_index = self.regs.frnum & 0x03ff;
        // Like other PCI devices, UHCI schedule DMA is gated by PCI Bus Master Enable. When DMA is
        // disabled integrations typically return open-bus reads (`0xFF`); avoid interpreting that
        // data as real schedule structures.
        if self.regs.flbaseadd != 0 && mem.dma_enabled() {
            let mut ctx = ScheduleContext {
                mem,
                hub: &mut self.hub,
                usbsts: &mut self.regs.usbsts,
                usbint_causes: &mut self.regs.usbint_causes,
            };
            process_frame(&mut ctx, self.regs.flbaseadd, frame_index);
        }

        self.regs.frnum = (self.regs.frnum + 1) & 0x07ff;
        self.update_irq();
    }
}

impl Default for UhciController {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UhciController {
    const DEVICE_ID: [u8; 4] = *b"UHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_USBINTR: u16 = 3;
        const TAG_USBINT_CAUSES: u16 = 4;
        const TAG_FRNUM: u16 = 5;
        const TAG_FLBASEADD: u16 = 6;
        const TAG_SOFMOD: u16 = 7;
        const TAG_ROOT_HUB_PORTS: u16 = 8;
        const TAG_PREV_PORT_RD: u16 = 9;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u16(TAG_USBCMD, self.regs.usbcmd);
        w.field_u16(TAG_USBSTS, self.regs.usbsts);
        w.field_u16(TAG_USBINTR, self.regs.usbintr);
        w.field_u16(TAG_USBINT_CAUSES, self.regs.usbint_causes);
        w.field_u16(TAG_FRNUM, self.regs.frnum);
        w.field_u32(TAG_FLBASEADD, self.regs.flbaseadd);
        w.field_u8(TAG_SOFMOD, self.regs.sofmod);
        w.field_bytes(TAG_ROOT_HUB_PORTS, self.hub.save_snapshot_ports());
        w.field_bool(TAG_PREV_PORT_RD, self.prev_port_resume_detect);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_USBINTR: u16 = 3;
        const TAG_USBINT_CAUSES: u16 = 4;
        const TAG_FRNUM: u16 = 5;
        const TAG_FLBASEADD: u16 = 6;
        const TAG_SOFMOD: u16 = 7;
        const TAG_ROOT_HUB_PORTS: u16 = 8;
        const TAG_PREV_PORT_RD: u16 = 9;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset controller-local state without disturbing attached device models.
        self.regs = UhciRegs::new();
        self.irq_level = false;
        self.prev_port_resume_detect = false;

        if let Some(usbcmd) = r.u16(TAG_USBCMD)? {
            self.regs.usbcmd = usbcmd & USBCMD_WRITE_MASK;
        }
        if let Some(usbsts) = r.u16(TAG_USBSTS)? {
            self.regs.usbsts = usbsts & USBSTS_READ_MASK;
        }
        if let Some(usbintr) = r.u16(TAG_USBINTR)? {
            self.regs.usbintr = usbintr & USBINTR_MASK;
        }
        if let Some(causes) = r.u16(TAG_USBINT_CAUSES)? {
            self.regs.usbint_causes = causes & (USBINT_CAUSE_IOC | USBINT_CAUSE_SHORT_PACKET);
        }
        if let Some(frnum) = r.u16(TAG_FRNUM)? {
            self.regs.frnum = frnum & 0x07ff;
        }
        if let Some(flbaseadd) = r.u32(TAG_FLBASEADD)? {
            self.regs.flbaseadd = flbaseadd & 0xffff_f000;
        }
        if let Some(sofmod) = r.u8(TAG_SOFMOD)? {
            self.regs.sofmod = sofmod;
        }

        if let Some(buf) = r.bytes(TAG_ROOT_HUB_PORTS) {
            self.hub.load_snapshot_ports(buf)?;
        }

        if let Some(prev) = r.bool(TAG_PREV_PORT_RD)? {
            self.prev_port_resume_detect = prev;
        }

        self.regs.update_halted();
        self.update_irq();

        Ok(())
    }
}
