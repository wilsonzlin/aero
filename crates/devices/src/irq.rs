pub trait IrqLine {
    fn set_level(&self, level: bool);
}

#[derive(Clone, Copy, Default)]
pub struct NoIrq;

impl IrqLine for NoIrq {
    fn set_level(&self, _level: bool) {}
}

use aero_platform::interrupts::{InterruptInput, PlatformInterrupts};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct PlatformIrqLine {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
    input: InterruptInput,
}

impl PlatformIrqLine {
    pub fn isa(interrupts: Rc<RefCell<PlatformInterrupts>>, irq: u8) -> Self {
        Self {
            interrupts,
            input: InterruptInput::IsaIrq(irq),
        }
    }

    pub fn gsi(interrupts: Rc<RefCell<PlatformInterrupts>>, gsi: u32) -> Self {
        Self {
            interrupts,
            input: InterruptInput::Gsi(gsi),
        }
    }
}

impl IrqLine for PlatformIrqLine {
    fn set_level(&self, level: bool) {
        let mut interrupts = self.interrupts.borrow_mut();
        if level {
            interrupts.raise_irq(self.input);
        } else {
            interrupts.lower_irq(self.input);
        }
    }
}
