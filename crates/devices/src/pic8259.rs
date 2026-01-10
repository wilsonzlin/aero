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

use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

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
        }
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

            // Reset internal state. Real hardware clears ISR/IRR and resets
            // various modes; for bringup purposes this is sufficient.
            self.isr = 0;
            self.irr = 0;
            self.line_level = 0;
            self.read_isr = false;
            self.auto_eoi = false;

            // IMR is cleared on initialization (all interrupts enabled) on
            // real hardware. Guests typically follow up with an OCW1 write.
            self.imr = 0x00;
            return;
        }

        if (val & 0x08) != 0 {
            // OCW3
            // RR (bit 1) enables setting the read register, RIS (bit 0) selects
            // between IRR/ISR. Common values: 0x0A (IRR), 0x0B (ISR).
            if (val & 0x02) != 0 {
                self.read_isr = (val & 0x01) != 0;
            }
            return;
        }

        // OCW2
        if (val & 0x20) != 0 {
            let specific = (val & 0x40) != 0;
            if specific {
                let level = val & 0x07;
                self.isr &= !(1u8 << level);
            } else if let Some(level) = lowest_set_bit(self.isr) {
                self.isr &= !(1u8 << level);
            }

            if self.level_triggered {
                self.refresh_level_triggered_irr();
            }
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
        // For level-triggered mode, IRR reflects active input levels which are
        // not currently in-service. While a line is in-service, it can't
        // re-enter IRR until an EOI clears the ISR bit.
        self.irr = self.irr | (self.line_level & !self.isr);
        self.irr &= self.line_level | self.isr;
    }

    fn pending_irq_with_extra_irr(&self, extra_irr: u8) -> Option<u8> {
        let pending = (self.irr | extra_irr) & !self.imr;
        if pending == 0 {
            return None;
        }

        let threshold = match lowest_set_bit(self.isr) {
            Some(level) => level,
            None => 8,
        };

        for irq in 0..threshold {
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
        // Prefer decoding as slave first (as its vectors typically do not overlap
        // with the master's; when they do, a real system is misconfigured).
        if (vector & 0xF8) == self.slave.vector_base {
            let irq = vector & 0x07;
            let cascade_line = self.cascade_line().unwrap_or(2);

            if self.slave.acknowledge_irq(irq) {
                self.master.force_in_service(cascade_line);
                self.master.irr &= !(1u8 << cascade_line);
                return Some(8 + irq);
            }

            // Spurious IRQ15: slave vector base + 7 but no request in IRR.
            if irq == 7 {
                self.master.force_in_service(cascade_line);
            }
            return None;
        }

        if (vector & 0xF8) == self.master.vector_base {
            let irq = vector & 0x07;

            if irq == 7 && !self.master.is_irq_requested(7) {
                // Spurious IRQ7: do not set an ISR bit.
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

pub type SharedPic8259 = Rc<RefCell<DualPic8259>>;

/// I/O-port view of a shared [`DualPic8259`].
///
/// `IoPortBus` maps one port to one device instance. A real PIC responds to
/// four ports, so the common pattern is to share the PIC behind `Rc<RefCell<_>>`
/// and register four `Pic8259Port` instances.
pub struct Pic8259Port {
    pic: SharedPic8259,
    port: u16,
}

impl Pic8259Port {
    pub fn new(pic: SharedPic8259, port: u16) -> Self {
        Self { pic, port }
    }
}

impl PortIoDevice for Pic8259Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        let pic = self.pic.borrow();
        match size {
            1 => u32::from(pic.port_read_u8(port)),
            2 => {
                let lo = pic.port_read_u8(port) as u16;
                let hi = pic.port_read_u8(port.wrapping_add(1)) as u16;
                u32::from(lo | (hi << 8))
            }
            4 => {
                let b0 = pic.port_read_u8(port);
                let b1 = pic.port_read_u8(port.wrapping_add(1));
                let b2 = pic.port_read_u8(port.wrapping_add(2));
                let b3 = pic.port_read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => u32::from(pic.port_read_u8(port)),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        let mut pic = self.pic.borrow_mut();
        match size {
            1 => pic.port_write_u8(port, value as u8),
            2 => {
                let [b0, b1] = (value as u16).to_le_bytes();
                pic.port_write_u8(port, b0);
                pic.port_write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = value.to_le_bytes();
                pic.port_write_u8(port, b0);
                pic.port_write_u8(port.wrapping_add(1), b1);
                pic.port_write_u8(port.wrapping_add(2), b2);
                pic.port_write_u8(port.wrapping_add(3), b3);
            }
            _ => pic.port_write_u8(port, value as u8),
        }
    }
}

/// Convenience helper to register a dual PIC on an [`IoPortBus`].
pub fn register_pic8259(bus: &mut IoPortBus, pic: SharedPic8259) {
    bus.register(
        MASTER_CMD,
        Box::new(Pic8259Port::new(pic.clone(), MASTER_CMD)),
    );
    bus.register(
        MASTER_DATA,
        Box::new(Pic8259Port::new(pic.clone(), MASTER_DATA)),
    );
    bus.register(
        SLAVE_CMD,
        Box::new(Pic8259Port::new(pic.clone(), SLAVE_CMD)),
    );
    bus.register(SLAVE_DATA, Box::new(Pic8259Port::new(pic, SLAVE_DATA)));
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

    fn init_legacy_pc_level_triggered(pic: &mut DualPic8259) {
        // Same as `init_legacy_pc`, but with ICW1.LTIM set (level triggered).
        pic.port_write_u8(MASTER_CMD, 0x19);
        pic.port_write_u8(MASTER_DATA, 0x20);
        pic.port_write_u8(MASTER_DATA, 0x04);
        pic.port_write_u8(MASTER_DATA, 0x01);

        pic.port_write_u8(SLAVE_CMD, 0x19);
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

    #[test]
    fn fixed_priority_and_eoi() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        pic.raise_irq(3);
        pic.raise_irq(1);

        let vec1 = pic.get_pending_vector();
        assert_eq!(vec1, Some(0x21));
        pic.acknowledge(vec1.unwrap());

        // IRQ3 is lower priority than IRQ1; it should be blocked until EOI.
        assert_eq!(pic.get_pending_vector(), None);
        pic.port_write_u8(MASTER_CMD, 0x20);

        let vec2 = pic.get_pending_vector();
        assert_eq!(vec2, Some(0x23));
        pic.acknowledge(vec2.unwrap());
    }

    #[test]
    fn masked_irqs_are_not_delivered() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Mask IRQ1.
        pic.port_write_u8(MASTER_DATA, 0x02);
        pic.raise_irq(1);
        assert_eq!(pic.get_pending_vector(), None);

        // Unmask and ensure the latched request becomes pending.
        pic.port_write_u8(MASTER_DATA, 0x00);
        assert_eq!(pic.get_pending_vector(), Some(0x21));
    }

    #[test]
    fn slave_irqs_route_via_master_irq2() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Mask the cascade line on the master.
        pic.port_write_u8(MASTER_DATA, 0x04);

        pic.raise_irq(8);
        assert_eq!(pic.get_pending_vector(), None);

        // Unmask and ensure IRQ8 becomes deliverable.
        pic.port_write_u8(MASTER_DATA, 0x00);
        assert_eq!(pic.get_pending_vector(), Some(0x28));
    }

    #[test]
    fn eoi_requires_slave_then_master_for_cascaded_interrupts() {
        let mut pic = DualPic8259::new();
        init_legacy_pc(&mut pic);

        // Queue two slave interrupts.
        pic.raise_irq(8);
        pic.raise_irq(9);

        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x28);
        pic.acknowledge(vec);

        // EOI only to the slave: IRQ9 becomes eligible on the slave, but the
        // master still has the cascade IRQ in service, so nothing should be
        // deliverable yet.
        pic.port_write_u8(SLAVE_CMD, 0x20);
        assert_eq!(pic.get_pending_vector(), None);

        // EOI to the master clears the cascade in-service bit, allowing IRQ9
        // through.
        pic.port_write_u8(MASTER_CMD, 0x20);
        assert_eq!(pic.get_pending_vector(), Some(0x29));
    }

    #[test]
    fn spurious_irq7_does_not_set_isr() {
        let mut pic = DualPic8259::new();
        init_legacy_pc_level_triggered(&mut pic);

        pic.raise_irq(7);
        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x27);

        // Withdraw the request before the CPU acknowledges (level-triggered).
        pic.lower_irq(7);
        assert_eq!(pic.acknowledge(vec), None);

        // Master ISR must remain clear.
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 0x00);
    }

    #[test]
    fn spurious_irq15_sets_only_master_cascade_in_service() {
        let mut pic = DualPic8259::new();
        init_legacy_pc_level_triggered(&mut pic);

        pic.raise_irq(15);
        let vec = pic.get_pending_vector().unwrap();
        assert_eq!(vec, 0x2F);

        // Withdraw request before INTA (level-triggered), causing a spurious IRQ15.
        pic.lower_irq(15);
        assert_eq!(pic.acknowledge(vec), None);

        // Slave ISR must remain clear.
        pic.port_write_u8(SLAVE_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(SLAVE_CMD), 0x00);

        // Master should have the cascade IRQ in service.
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 1u8 << 2);

        // EOI to the master clears it.
        pic.port_write_u8(MASTER_CMD, 0x20);
        pic.port_write_u8(MASTER_CMD, 0x0B);
        assert_eq!(pic.port_read_u8(MASTER_CMD), 0x00);
    }
}
