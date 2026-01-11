//! Intel 8259A Programmable Interrupt Controller (PIC) emulation.
//!
//! This module implements two cascaded 8259A controllers (master + slave) as
//! typically wired in PC/AT compatibles:
//!
//! - Master: command @ 0x20, data @ 0x21
//! - Slave:  command @ 0xA0, data @ 0xA1
//!
//! The emulation supports the subset of features needed for BIOS and early OS
//! bringup:
//!
//! - ICW1-ICW4 initialization (vector base, cascade wiring).
//! - OCW1 IMR reads/writes.
//! - OCW2 (specific + non-specific EOI).
//! - OCW3 selection of IRR/ISR reads.
//! - Fixed priority (IRQ0 highest ... IRQ7 lowest).
//!
//! ## Spurious interrupts
//!
//! Spurious IRQ7/IRQ15 are modeled in a simplified manner:
//! - If the CPU acknowledges a vector for IRQ7/IRQ15 but the corresponding IRR
//!   bit is not set, the interrupt is treated as spurious.
//! - For spurious IRQ7: no ISR bit is set.
//! - For spurious IRQ15: the slave does not set an ISR bit, but the master does
//!   set the cascade IRQ in-service bit (as real hardware would); software can
//!   clear it via an EOI to the master PIC.
//!
//! This is sufficient for common OS spurious-interrupt handlers.

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

pub const MASTER_CMD: u16 = 0x20;
pub const MASTER_DATA: u16 = 0x21;
pub const SLAVE_CMD: u16 = 0xA0;
pub const SLAVE_DATA: u16 = 0xA1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitState {
    None,
    Icw2,
    Icw3,
    Icw4,
}

#[derive(Debug, Clone)]
struct Pic8259 {
    /// Interrupt mask register (OCW1).
    imr: u8,
    /// Interrupt request register.
    irr: u8,
    /// In-service register.
    isr: u8,

    /// Tracks the current level of each IRQ input line (for edge detection).
    line_level: u8,

    /// Base interrupt vector (ICW2, aligned to 8).
    vector_base: u8,

    /// ICW3 value (master: bitmask of slave lines; slave: ID number).
    icw3: u8,

    /// If true, read from command port returns ISR; otherwise IRR (OCW3).
    read_isr: bool,

    /// ICW1: 0=edge triggered, 1=level triggered.
    level_triggered: bool,

    /// ICW4: automatic EOI.
    auto_eoi: bool,

    /// Whether this PIC is in single mode (ICW1.SNGL).
    single: bool,

    /// Initialization state machine.
    init_state: InitState,

    /// Whether ICW4 is expected (ICW1.IC4).
    expect_icw4: bool,

    /// Lowest priority IRQ (0-7) for rotating priority mode.
    ///
    /// The highest priority is the next IRQ in sequence. A value of 7 yields the
    /// classic fixed priority order (IRQ0 highest .. IRQ7 lowest).
    lowest_priority: u8,
}

impl Pic8259 {
    fn new_power_on() -> Self {
        Self {
            // Typical BIOS expects all IRQs masked until initialization programs
            // the PICs and explicitly unmasks.
            imr: 0xFF,
            irr: 0,
            isr: 0,
            line_level: 0,
            vector_base: 0,
            icw3: 0,
            read_isr: false,
            level_triggered: false,
            auto_eoi: false,
            single: true,
            init_state: InitState::None,
            expect_icw4: false,
            lowest_priority: 7,
        }
    }

    fn irq_priority_order(&self) -> impl Iterator<Item = u8> {
        let base = self.lowest_priority & 7;
        (1u8..=8).map(move |i| base.wrapping_add(i) & 7)
    }

    fn highest_in_service(&self) -> Option<u8> {
        self.irq_priority_order()
            .find(|irq| (self.isr & (1u8 << irq)) != 0)
    }

    fn command_read(&self) -> u8 {
        if self.read_isr {
            self.isr
        } else {
            self.irr
        }
    }

    fn data_read(&self) -> u8 {
        self.imr
    }

    fn write_command(&mut self, val: u8) {
        if (val & 0x10) != 0 {
            // ICW1: start initialization sequence.
            self.init_state = InitState::Icw2;
            self.expect_icw4 = (val & 0x01) != 0;
            self.single = (val & 0x02) != 0;
            self.level_triggered = (val & 0x08) != 0;

            // Reset internal state.
            self.isr = 0;
            self.irr = 0;
            self.line_level = 0;
            self.read_isr = false;
            self.auto_eoi = false;
            self.lowest_priority = 7;

            // IMR is cleared on initialization (all interrupts enabled) on
            // real hardware. Guests typically follow up with an OCW1 write.
            self.imr = 0x00;
            return;
        }

        if (val & 0x08) != 0 {
            // OCW3
            if (val & 0x02) != 0 {
                self.read_isr = (val & 0x01) != 0;
            }
            return;
        }

        // OCW2
        if (val & 0x20) != 0 {
            let rotate = (val & 0x80) != 0;
            let specific = (val & 0x40) != 0;
            let cleared = if specific {
                let level = val & 0x07;
                self.isr &= !(1u8 << level);
                Some(level)
            } else if let Some(level) = self.highest_in_service() {
                self.isr &= !(1u8 << level);
                Some(level)
            } else {
                None
            };

            if rotate {
                if let Some(level) = cleared {
                    self.lowest_priority = level & 7;
                }
            }

            if self.level_triggered {
                self.refresh_level_triggered_irr();
            }
            return;
        }

        // OCW2: Set priority (rotate only, no EOI).
        if (val & 0xC0) == 0xC0 {
            self.lowest_priority = val & 0x07;
        }
    }

    fn write_data(&mut self, val: u8) {
        match self.init_state {
            InitState::Icw2 => {
                self.vector_base = val & 0xF8;
                if self.single {
                    self.init_state = if self.expect_icw4 {
                        InitState::Icw4
                    } else {
                        InitState::None
                    };
                } else {
                    self.init_state = InitState::Icw3;
                }
            }
            InitState::Icw3 => {
                self.icw3 = val;
                self.init_state = if self.expect_icw4 {
                    InitState::Icw4
                } else {
                    InitState::None
                };
            }
            InitState::Icw4 => {
                self.auto_eoi = (val & 0x02) != 0;
                self.init_state = InitState::None;
            }
            InitState::None => {
                // OCW1: interrupt mask register
                self.imr = val;
            }
        }
    }

    fn set_irq_level(&mut self, irq: u8, level: bool) {
        let mask = 1u8 << irq;
        if level {
            if (self.line_level & mask) == 0 {
                // Rising edge
                if !self.level_triggered {
                    self.irr |= mask;
                }
            }
            self.line_level |= mask;
            if self.level_triggered {
                self.refresh_level_triggered_irr();
            }
        } else {
            self.line_level &= !mask;
            if self.level_triggered {
                self.refresh_level_triggered_irr();
            }
        }
    }

    fn refresh_level_triggered_irr(&mut self) {
        self.irr = self.irr | (self.line_level & !self.isr);
        self.irr &= self.line_level | self.isr;
    }

    fn pending_irq_with_extra_irr(&self, extra_irr: u8) -> Option<u8> {
        let pending = (self.irr | extra_irr) & !self.imr;
        if pending == 0 {
            return None;
        }

        let highest_in_service = self.highest_in_service();

        for irq in self.irq_priority_order() {
            if Some(irq) == highest_in_service {
                break;
            }
            if (pending & (1u8 << irq)) != 0 {
                return Some(irq);
            }
        }
        None
    }

    fn pending_irq(&self) -> Option<u8> {
        self.pending_irq_with_extra_irr(0)
    }

    fn is_irq_requested(&self, irq: u8) -> bool {
        (self.irr & (1u8 << irq)) != 0
    }

    fn acknowledge_irq(&mut self, irq: u8) -> bool {
        let mask = 1u8 << irq;
        if (self.irr & mask) == 0 {
            return false;
        }

        self.irr &= !mask;
        if !self.auto_eoi {
            self.isr |= mask;
        }

        if self.level_triggered {
            self.refresh_level_triggered_irr();
        }
        true
    }

    fn force_in_service(&mut self, irq: u8) {
        if self.auto_eoi {
            return;
        }
        self.isr |= 1u8 << irq;
        if self.level_triggered {
            self.refresh_level_triggered_irr();
        }
    }
}

fn lowest_set_bit(val: u8) -> Option<u8> {
    if val == 0 {
        None
    } else {
        Some(val.trailing_zeros() as u8)
    }
}

/// Dual 8259A (master + slave) as used by PC/AT compatibles.
#[derive(Debug, Clone)]
pub struct DualPic8259 {
    master: Pic8259,
    slave: Pic8259,
}

impl DualPic8259 {
    pub fn new() -> Self {
        Self {
            master: Pic8259::new_power_on(),
            slave: Pic8259::new_power_on(),
        }
    }

    /// Returns the current interrupt vector base for the master and slave PICs.
    pub fn vector_bases(&self) -> (u8, u8) {
        (self.master.vector_base, self.slave.vector_base)
    }

    fn cascade_line(&self) -> Option<u8> {
        if self.master.single || self.slave.single || self.master.icw3 == 0 {
            return None;
        }

        let slave_id = self.slave.icw3 & 0x07;
        if (self.master.icw3 & (1u8 << slave_id)) != 0 {
            return Some(slave_id);
        }

        // Fallback: pick the first configured slave line on the master.
        lowest_set_bit(self.master.icw3)
    }

    fn slave_interrupt_output(&self) -> bool {
        self.slave.pending_irq().is_some()
    }

    /// Returns the currently pending x86 interrupt vector, if any.
    pub fn get_pending_vector(&self) -> Option<u8> {
        let (cascade_line, cascade_bit, slave_pending) = match self.cascade_line() {
            Some(line) => {
                let pending = self.slave.pending_irq();
                let bit = if pending.is_some() { 1u8 << line } else { 0 };
                (line, bit, pending)
            }
            None => (0, 0, None),
        };

        let master_irq = self.master.pending_irq_with_extra_irr(cascade_bit)?;
        if cascade_bit != 0 && master_irq == cascade_line {
            // Interrupt from the slave PIC routed through the master.
            if let Some(slave_irq) = slave_pending {
                return Some(self.slave.vector_base | slave_irq);
            }
        }
        Some(self.master.vector_base | master_irq)
    }

    /// Acknowledge a vector previously returned by [`get_pending_vector`].
    ///
    /// Returns the acknowledged IRQ number (0-15) if a request was consumed.
    pub fn acknowledge(&mut self, vector: u8) -> Option<u8> {
        // Prefer decoding as slave first.
        if (vector & 0xF8) == self.slave.vector_base {
            let irq = vector & 0x07;
            let cascade_line = self.cascade_line().unwrap_or(2);

            if self.slave.acknowledge_irq(irq) {
                self.master.force_in_service(cascade_line);
                self.master.irr &= !(1u8 << cascade_line);
                return Some(8 + irq);
            }

            // Spurious IRQ15
            if irq == 7 {
                self.master.force_in_service(cascade_line);
            }
            return None;
        }

        if (vector & 0xF8) == self.master.vector_base {
            let irq = vector & 0x07;

            if irq == 7 && !self.master.is_irq_requested(7) {
                // Spurious IRQ7
                return None;
            }

            if self.master.acknowledge_irq(irq) {
                return Some(irq);
            }
        }

        None
    }

    pub fn raise_irq(&mut self, irq: u8) {
        match irq {
            0..=7 => self.master.set_irq_level(irq, true),
            8..=15 => self.slave.set_irq_level(irq - 8, true),
            _ => {}
        }
    }

    pub fn lower_irq(&mut self, irq: u8) {
        match irq {
            0..=7 => self.master.set_irq_level(irq, false),
            8..=15 => self.slave.set_irq_level(irq - 8, false),
            _ => {}
        }
    }

    pub fn port_read_u8(&self, port: u16) -> u8 {
        match port {
            MASTER_CMD => {
                if self.master.read_isr {
                    self.master.isr
                } else {
                    let mut irr = self.master.irr;
                    if let Some(cascade_line) = self.cascade_line() {
                        if self.slave_interrupt_output() {
                            irr |= 1u8 << cascade_line;
                        }
                    }
                    irr
                }
            }
            MASTER_DATA => self.master.data_read(),
            SLAVE_CMD => self.slave.command_read(),
            SLAVE_DATA => self.slave.data_read(),
            _ => 0xFF,
        }
    }

    pub fn port_write_u8(&mut self, port: u16, val: u8) {
        match port {
            MASTER_CMD => self.master.write_command(val),
            MASTER_DATA => self.master.write_data(val),
            SLAVE_CMD => self.slave.write_command(val),
            SLAVE_DATA => self.slave.write_data(val),
            _ => {}
        }
    }
}

impl Default for DualPic8259 {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for DualPic8259 {
    const DEVICE_ID: [u8; 4] = *b"PIC9";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_MASTER: u16 = 1;
        const TAG_SLAVE: u16 = 2;

        fn encode_unit(unit: &Pic8259) -> Vec<u8> {
            let init_state = match unit.init_state {
                InitState::None => 0u8,
                InitState::Icw2 => 1,
                InitState::Icw3 => 2,
                InitState::Icw4 => 3,
            };

            Encoder::new()
                .u8(unit.imr)
                .u8(unit.irr)
                .u8(unit.isr)
                .u8(unit.line_level)
                .u8(unit.vector_base)
                .u8(unit.icw3)
                .bool(unit.read_isr)
                .bool(unit.level_triggered)
                .bool(unit.auto_eoi)
                .bool(unit.single)
                .u8(init_state)
                .bool(unit.expect_icw4)
                .u8(unit.lowest_priority)
                .finish()
        }

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_MASTER, encode_unit(&self.master));
        w.field_bytes(TAG_SLAVE, encode_unit(&self.slave));
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_MASTER: u16 = 1;
        const TAG_SLAVE: u16 = 2;

        fn decode_unit(unit: &mut Pic8259, buf: &[u8]) -> SnapshotResult<()> {
            let mut d = Decoder::new(buf);

            unit.imr = d.u8()?;
            unit.irr = d.u8()?;
            unit.isr = d.u8()?;
            unit.line_level = d.u8()?;
            unit.vector_base = d.u8()? & 0xF8;
            unit.icw3 = d.u8()?;
            unit.read_isr = d.bool()?;
            unit.level_triggered = d.bool()?;
            unit.auto_eoi = d.bool()?;
            unit.single = d.bool()?;
            unit.init_state = match d.u8()? {
                0 => InitState::None,
                1 => InitState::Icw2,
                2 => InitState::Icw3,
                3 => InitState::Icw4,
                _ => return Err(SnapshotError::InvalidFieldEncoding("pic init_state")),
            };
            unit.expect_icw4 = d.bool()?;
            unit.lowest_priority = d.u8()? & 0x07;

            d.finish()?;
            Ok(())
        }

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_MASTER) {
            decode_unit(&mut self.master, buf)?;
        }
        if let Some(buf) = r.bytes(TAG_SLAVE) {
            decode_unit(&mut self.slave, buf)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_legacy_pc(pic: &mut DualPic8259) {
        // Initialize master PIC: base 0x20, slave on IRQ2, 8086 mode.
        pic.port_write_u8(MASTER_CMD, 0x11);
        pic.port_write_u8(MASTER_DATA, 0x20);
        pic.port_write_u8(MASTER_DATA, 0x04);
        pic.port_write_u8(MASTER_DATA, 0x01);

        // Initialize slave PIC: base 0x28, cascade identity 2, 8086 mode.
        pic.port_write_u8(SLAVE_CMD, 0x11);
        pic.port_write_u8(SLAVE_DATA, 0x28);
        pic.port_write_u8(SLAVE_DATA, 0x02);
        pic.port_write_u8(SLAVE_DATA, 0x01);
    }

    #[test]
    fn init_sets_vector_bases() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        pic.raise_irq(0);
        assert_eq!(pic.get_pending_vector(), Some(0x20));
        pic.acknowledge(0x20);
        pic.port_write_u8(MASTER_CMD, 0x20);

        pic.raise_irq(8);
        assert_eq!(pic.get_pending_vector(), Some(0x28));
    }
}
