use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use crate::io::PortIO;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartConfig {
    pub base_port: u16,
    pub irq: u8,
}

impl UartConfig {
    pub const COM1: Self = Self {
        base_port: 0x3F8,
        irq: 4,
    };
    pub const COM2: Self = Self {
        base_port: 0x2F8,
        irq: 3,
    };
    pub const COM3: Self = Self {
        base_port: 0x3E8,
        irq: 4,
    };
    pub const COM4: Self = Self {
        base_port: 0x2E8,
        irq: 3,
    };
}

pub struct Uart16550 {
    config: UartConfig,
    dll: Cell<u8>,
    dlm: Cell<u8>,
    ier: Cell<u8>,
    fcr: Cell<u8>,
    lcr: Cell<u8>,
    mcr: Cell<u8>,
    scr: Cell<u8>,
    rx_fifo: RefCell<VecDeque<u8>>,
    tx_callback: RefCell<Option<Box<dyn FnMut(u16, &[u8])>>>,
}

impl std::fmt::Debug for Uart16550 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Uart16550")
            .field("config", &self.config)
            .field("dll", &self.dll.get())
            .field("dlm", &self.dlm.get())
            .field("ier", &self.ier.get())
            .field("fcr", &self.fcr.get())
            .field("lcr", &self.lcr.get())
            .field("mcr", &self.mcr.get())
            .field("scr", &self.scr.get())
            .field("rx_fifo_len", &self.rx_fifo.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Uart16550 {
    pub fn new(config: UartConfig) -> Self {
        Self {
            config,
            dll: Cell::new(0),
            dlm: Cell::new(0),
            ier: Cell::new(0),
            fcr: Cell::new(0),
            lcr: Cell::new(0),
            mcr: Cell::new(0),
            scr: Cell::new(0),
            rx_fifo: RefCell::new(VecDeque::new()),
            tx_callback: RefCell::new(None),
        }
    }

    pub fn config(&self) -> UartConfig {
        self.config
    }

    pub fn set_tx_callback(&self, cb: impl FnMut(u16, &[u8]) + 'static) {
        *self.tx_callback.borrow_mut() = Some(Box::new(cb));
    }

    pub fn inject_rx(&self, byte: u8) {
        self.rx_fifo.borrow_mut().push_back(byte);
    }

    fn dlab(&self) -> bool {
        (self.lcr.get() & 0x80) != 0
    }

    fn read_u8(&self, port: u16) -> u8 {
        let offset = port.wrapping_sub(self.config.base_port);
        match offset {
            0 => {
                if self.dlab() {
                    self.dll.get()
                } else {
                    self.rx_fifo.borrow_mut().pop_front().unwrap_or(0)
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm.get()
                } else {
                    self.ier.get()
                }
            }
            2 => {
                let fifo_enabled = (self.fcr.get() & 0x01) != 0;
                let fifo_bits = if fifo_enabled { 0xC0 } else { 0x00 };
                fifo_bits | 0x01
            }
            3 => self.lcr.get(),
            4 => self.mcr.get(),
            5 => {
                let mut lsr = 0x60;
                if !self.rx_fifo.borrow().is_empty() {
                    lsr |= 0x01;
                }
                lsr
            }
            6 => 0,
            7 => self.scr.get(),
            _ => 0,
        }
    }

    fn write_u8(&mut self, port: u16, value: u8) {
        let offset = port.wrapping_sub(self.config.base_port);
        match offset {
            0 => {
                if self.dlab() {
                    self.dll.set(value);
                } else if let Some(cb) = self.tx_callback.borrow_mut().as_mut() {
                    cb(self.config.base_port, &[value]);
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm.set(value);
                } else {
                    self.ier.set(value);
                }
            }
            2 => {
                self.fcr.set(value);
                if (value & 0x02) != 0 {
                    self.rx_fifo.borrow_mut().clear();
                }
            }
            3 => self.lcr.set(value),
            4 => self.mcr.set(value),
            7 => self.scr.set(value),
            _ => {}
        }
    }
}

impl PortIO for Uart16550 {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        match size {
            1 => self.read_u8(port) as u32,
            2 => {
                let lo = self.read_u8(port) as u32;
                let hi = self.read_u8(port.wrapping_add(1)) as u32;
                lo | (hi << 8)
            }
            4 => {
                let b0 = self.read_u8(port) as u32;
                let b1 = self.read_u8(port.wrapping_add(1)) as u32;
                let b2 = self.read_u8(port.wrapping_add(2)) as u32;
                let b3 = self.read_u8(port.wrapping_add(3)) as u32;
                b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
            }
            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match size {
            1 => self.write_u8(port, val as u8),
            2 => {
                self.write_u8(port, val as u8);
                self.write_u8(port.wrapping_add(1), (val >> 8) as u8);
            }
            4 => {
                self.write_u8(port, val as u8);
                self.write_u8(port.wrapping_add(1), (val >> 8) as u8);
                self.write_u8(port.wrapping_add(2), (val >> 16) as u8);
                self.write_u8(port.wrapping_add(3), (val >> 24) as u8);
            }
            _ => {}
        }
    }
}
