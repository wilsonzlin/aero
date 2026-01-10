pub trait IrqLine {
    fn set_level(&self, level: bool);
}

#[derive(Clone, Copy, Default)]
pub struct NoIrq;

impl IrqLine for NoIrq {
    fn set_level(&self, _level: bool) {}
}
