#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// Legacy ISA IRQ line.
    Irq(u8),
    /// CPU interrupt vector.
    Vector(u8),
}

pub trait InterruptSink {
    fn raise(&mut self, interrupt: Interrupt, at_ns: u64);
}
