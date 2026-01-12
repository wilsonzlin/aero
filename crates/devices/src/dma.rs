use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Minimal 8237 DMA controller model.
///
/// Windows 7 doesn't rely on legacy DMA for most devices, but probes the
/// controller for compatibility. This implementation is a "register file"
/// stub: it stores written values and returns them on reads, without actually
/// performing DMA transfers.
#[derive(Default, Clone, Debug)]
pub struct Dma8237 {
    regs: HashMap<u16, u8>,
}

impl Dma8237 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.regs.clear();
    }

    pub fn read_u8(&self, port: u16) -> u8 {
        self.regs.get(&port).copied().unwrap_or(0)
    }

    pub fn write_u8(&mut self, port: u16, value: u8) {
        self.regs.insert(port, value);
    }
}

pub type SharedDma8237 = Rc<RefCell<Dma8237>>;

pub struct Dma8237Port {
    dma: SharedDma8237,
    port: u16,
}

impl Dma8237Port {
    pub fn new(dma: SharedDma8237, port: u16) -> Self {
        Self { dma, port }
    }
}

impl PortIoDevice for Dma8237Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        let dma = self.dma.borrow();
        match size {
            1 => u32::from(dma.read_u8(port)),
            2 => {
                let lo = dma.read_u8(port) as u16;
                let hi = dma.read_u8(port.wrapping_add(1)) as u16;
                u32::from(lo | (hi << 8))
            }
            4 => {
                let b0 = dma.read_u8(port);
                let b1 = dma.read_u8(port.wrapping_add(1));
                let b2 = dma.read_u8(port.wrapping_add(2));
                let b3 = dma.read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => u32::from(dma.read_u8(port)),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        let mut dma = self.dma.borrow_mut();
        match size {
            1 => dma.write_u8(port, value as u8),
            2 => {
                let [b0, b1] = (value as u16).to_le_bytes();
                dma.write_u8(port, b0);
                dma.write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = value.to_le_bytes();
                dma.write_u8(port, b0);
                dma.write_u8(port.wrapping_add(1), b1);
                dma.write_u8(port.wrapping_add(2), b2);
                dma.write_u8(port.wrapping_add(3), b3);
            }
            _ => dma.write_u8(port, value as u8),
        }
    }

    fn reset(&mut self) {
        self.dma.borrow_mut().reset();
    }
}

pub fn register_dma8237(bus: &mut IoPortBus, dma: SharedDma8237) {
    // Primary DMA controller ports.
    for port in 0x00u16..=0x0F {
        bus.register(port, Box::new(Dma8237Port::new(dma.clone(), port)));
    }

    // DMA page registers and miscellaneous ports.
    for port in 0x80u16..=0x8F {
        bus.register(port, Box::new(Dma8237Port::new(dma.clone(), port)));
    }

    // Secondary DMA controller ports.
    for port in 0xC0u16..=0xDF {
        bus.register(port, Box::new(Dma8237Port::new(dma.clone(), port)));
    }
}
