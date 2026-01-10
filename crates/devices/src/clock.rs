use std::cell::Cell;
use std::rc::Rc;

pub trait Clock {
    fn now_ns(&self) -> u64;
}

#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now_ns: Rc<Cell<u64>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ns(&self, now_ns: u64) {
        self.now_ns.set(now_ns);
    }

    pub fn advance_ns(&self, delta_ns: u64) {
        self.now_ns
            .set(self.now_ns.get().wrapping_add(delta_ns));
    }
}

impl Clock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.get()
    }
}

