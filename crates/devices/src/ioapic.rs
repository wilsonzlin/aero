use std::collections::BTreeSet;
use std::cell::RefCell;
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
        self.asserted.insert(gsi);
        self.events.push(GsiEvent::Raise(gsi));
    }

    fn lower_gsi(&mut self, gsi: u32) {
        self.asserted.remove(&gsi);
        self.events.push(GsiEvent::Lower(gsi));
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
        self.raise_irq(aero_platform::interrupts::InterruptInput::Gsi(gsi));
    }

    fn lower_gsi(&mut self, gsi: u32) {
        self.lower_irq(aero_platform::interrupts::InterruptInput::Gsi(gsi));
    }
}

impl GsiSink for Rc<RefCell<aero_platform::interrupts::PlatformInterrupts>> {
    fn raise_gsi(&mut self, gsi: u32) {
        self.borrow_mut()
            .raise_irq(aero_platform::interrupts::InterruptInput::Gsi(gsi));
    }

    fn lower_gsi(&mut self, gsi: u32) {
        self.borrow_mut()
            .lower_irq(aero_platform::interrupts::InterruptInput::Gsi(gsi));
    }
}
