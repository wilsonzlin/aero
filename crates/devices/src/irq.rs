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

#[derive(Debug, Default)]
struct PlatformIrqLineState {
    last_level: bool,
    last_generation: u64,
}

#[derive(Clone)]
pub struct PlatformIrqLine {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
    input: InterruptInput,
    state: Rc<RefCell<PlatformIrqLineState>>,
}

impl PlatformIrqLine {
    pub fn isa(interrupts: Rc<RefCell<PlatformInterrupts>>, irq: u8) -> Self {
        let generation = interrupts.borrow().irq_line_generation();
        Self {
            interrupts,
            input: InterruptInput::IsaIrq(irq),
            state: Rc::new(RefCell::new(PlatformIrqLineState {
                last_level: false,
                last_generation: generation,
            })),
        }
    }

    pub fn gsi(interrupts: Rc<RefCell<PlatformInterrupts>>, gsi: u32) -> Self {
        let generation = interrupts.borrow().irq_line_generation();
        Self {
            interrupts,
            input: InterruptInput::Gsi(gsi),
            state: Rc::new(RefCell::new(PlatformIrqLineState {
                last_level: false,
                last_generation: generation,
            })),
        }
    }
}

impl IrqLine for PlatformIrqLine {
    fn set_level(&self, level: bool) {
        let generation = self.interrupts.borrow().irq_line_generation();

        {
            let mut state = self.state.borrow_mut();
            if state.last_generation != generation {
                // The platform interrupt router was reset or restored from a snapshot. Discard our
                // cached last-driven level so we don't over/under-count edge transitions against
                // the ref-counted `PlatformInterrupts` implementation.
                state.last_generation = generation;
                state.last_level = false;
            }

            if state.last_level == level {
                return;
            }
            state.last_level = level;
        }

        let mut interrupts = self.interrupts.borrow_mut();
        if level {
            interrupts.raise_irq(self.input);
        } else {
            interrupts.lower_irq(self.input);
        }
    }
}
