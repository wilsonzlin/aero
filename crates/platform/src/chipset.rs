use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone)]
pub struct A20GateHandle(Rc<Cell<bool>>);

impl A20GateHandle {
    pub fn enabled(&self) -> bool {
        self.0.get()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.0.set(enabled);
    }
}

pub struct ChipsetState {
    a20: A20GateHandle,
}

impl ChipsetState {
    pub fn new(a20_enabled: bool) -> Self {
        Self {
            a20: A20GateHandle(Rc::new(Cell::new(a20_enabled))),
        }
    }

    pub fn a20(&self) -> A20GateHandle {
        self.a20.clone()
    }
}
