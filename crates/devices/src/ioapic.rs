use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

pub trait GsiSink {
    fn raise_gsi(&mut self, gsi: u32);
    fn lower_gsi(&mut self, gsi: u32);

    fn pulse_gsi(&mut self, gsi: u32) {
        self.raise_gsi(gsi);
        self.lower_gsi(gsi);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GsiEvent {
    Raise(u32),
    Lower(u32),
}

#[derive(Debug, Default)]
pub struct IoApic {
    asserted: BTreeSet<u32>,
    events: Vec<GsiEvent>,
}

impl IoApic {
    pub fn is_asserted(&self, gsi: u32) -> bool {
        self.asserted.contains(&gsi)
    }

    pub fn take_events(&mut self) -> Vec<GsiEvent> {
        std::mem::take(&mut self.events)
    }
}

impl GsiSink for IoApic {
    fn raise_gsi(&mut self, gsi: u32) {
        if self.asserted.insert(gsi) {
            self.events.push(GsiEvent::Raise(gsi));
        }
    }

    fn lower_gsi(&mut self, gsi: u32) {
        if self.asserted.remove(&gsi) {
            self.events.push(GsiEvent::Lower(gsi));
        }
    }
}

impl GsiSink for crate::apic::IoApic {
    fn raise_gsi(&mut self, gsi: u32) {
        self.set_irq_level(gsi, true);
    }

    fn lower_gsi(&mut self, gsi: u32) {
        self.set_irq_level(gsi, false);
    }
}

impl GsiSink for aero_platform::interrupts::PlatformInterrupts {
    fn raise_gsi(&mut self, gsi: u32) {
        use aero_platform::interrupts::{InterruptInput, PlatformInterruptMode};

        // Always keep the IOAPIC line state up-to-date so switching to APIC mode
        // via the IMCR can deliver any pending level-triggered interrupts.
        self.raise_irq(InterruptInput::Gsi(gsi));

        // In legacy PIC mode, HPET legacy replacement routes are expected to show
        // up on the same IRQ lines as the PIT (IRQ0) and RTC (IRQ8).
        //
        // ACPI MADT publishes IRQ0 -> GSI2, so we special-case that mapping here.
        if self.mode() == PlatformInterruptMode::LegacyPic {
            match gsi {
                2 => self.pic_mut().raise_irq(0),
                8 => self.pic_mut().raise_irq(8),
                _ => {}
            }
        }
    }

    fn lower_gsi(&mut self, gsi: u32) {
        use aero_platform::interrupts::{InterruptInput, PlatformInterruptMode};

        self.lower_irq(InterruptInput::Gsi(gsi));

        if self.mode() == PlatformInterruptMode::LegacyPic {
            match gsi {
                2 => self.pic_mut().lower_irq(0),
                8 => self.pic_mut().lower_irq(8),
                _ => {}
            }
        }
    }
}

impl GsiSink for Rc<RefCell<aero_platform::interrupts::PlatformInterrupts>> {
    fn raise_gsi(&mut self, gsi: u32) {
        <aero_platform::interrupts::PlatformInterrupts as GsiSink>::raise_gsi(
            &mut *self.borrow_mut(),
            gsi,
        );
    }

    fn lower_gsi(&mut self, gsi: u32) {
        <aero_platform::interrupts::PlatformInterrupts as GsiSink>::lower_gsi(
            &mut *self.borrow_mut(),
            gsi,
        );
    }
}
