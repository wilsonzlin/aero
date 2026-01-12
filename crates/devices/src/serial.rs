use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Minimal 16550 UART.
///
/// This is primarily for early boot debugging (BIOS logging / kernel serial
/// console). Interrupt-driven operation is not implemented yet.
#[derive(Debug)]
pub struct Serial16550 {
    base: u16,
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    lsr: u8,
    msr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    rx: VecDeque<u8>,
    tx: Vec<u8>,
}

impl Serial16550 {
    pub fn new(base: u16) -> Self {
        Self {
            base,
            ier: 0,
            fcr: 0,
            lcr: 0x03,
            mcr: 0,
            // THR empty + transmitter empty.
            lsr: 0x60,
            msr: 0,
            scr: 0,
            dll: 1,
            dlm: 0,
            rx: VecDeque::new(),
            tx: Vec::new(),
        }
    }

    pub fn push_rx(&mut self, byte: u8) {
        self.rx.push_back(byte);
    }

    /// Whether the UART is currently requesting an interrupt.
    ///
    /// This models the UART's `INTR` output as wired on PC hardware: the line is gated by the
    /// `OUT2` bit in the Modem Control Register (MCR). Software typically sets `OUT2` during UART
    /// initialization to enable interrupt delivery.
    pub fn irq_level(&self) -> bool {
        self.interrupt_pending() && (self.mcr & 0x08) != 0
    }

    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    fn fifo_enabled(&self) -> bool {
        (self.fcr & 0x01) != 0
    }

    fn interrupt_pending(&self) -> bool {
        // Receive Data Available.
        (self.ier & 0x01) != 0 && !self.rx.is_empty()
    }

    fn read_iir(&self) -> u8 {
        // Bit 0: 1 = no interrupt pending.
        // Bits 3:1: interrupt ID.
        // Bits 7:6: FIFO enabled status (16550).
        let fifo_bits = if self.fifo_enabled() { 0xC0 } else { 0x00 };
        if self.interrupt_pending() {
            fifo_bits | 0x04
        } else {
            fifo_bits | 0x01
        }
    }

    fn offset(&self, port: u16) -> Option<u16> {
        port.checked_sub(self.base).filter(|o| *o < 8)
    }

    pub fn read_u8(&mut self, port: u16) -> u8 {
        let Some(off) = self.offset(port) else {
            return 0xFF;
        };

        match off {
            0 => {
                if self.dlab() {
                    self.dll
                } else {
                    self.rx.pop_front().unwrap_or(0)
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm
                } else {
                    self.ier
                }
            }
            2 => self.read_iir(),
            3 => self.lcr,
            4 => self.mcr,
            5 => {
                let mut lsr = self.lsr | 0x60;
                if !self.rx.is_empty() {
                    lsr |= 0x01;
                }
                lsr
            }
            6 => self.msr,
            7 => self.scr,
            _ => 0xFF,
        }
    }

    pub fn write_u8(&mut self, port: u16, value: u8) {
        let Some(off) = self.offset(port) else {
            return;
        };

        match off {
            0 => {
                if self.dlab() {
                    self.dll = value;
                } else {
                    self.tx.push(value);
                }
            }
            1 => {
                if self.dlab() {
                    self.dlm = value;
                } else {
                    self.ier = value;
                }
            }
            2 => {
                // FCR write.
                self.fcr = value;
                // Clear receive FIFO.
                if (value & 0x02) != 0 {
                    self.rx.clear();
                }
            }
            3 => self.lcr = value,
            4 => self.mcr = value,
            7 => self.scr = value,
            _ => {}
        }
    }
}

pub type SharedSerial16550 = Rc<RefCell<Serial16550>>;

pub struct Serial16550Port {
    uart: SharedSerial16550,
    port: u16,
}

impl Serial16550Port {
    pub fn new(uart: SharedSerial16550, port: u16) -> Self {
        Self { uart, port }
    }
}

impl PortIoDevice for Serial16550Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        debug_assert_eq!(port, self.port);
        let mut uart = self.uart.borrow_mut();
        match size {
            1 => u32::from(uart.read_u8(port)),
            2 => {
                let lo = uart.read_u8(port) as u16;
                let hi = uart.read_u8(port.wrapping_add(1)) as u16;
                u32::from(lo | (hi << 8))
            }
            4 => {
                let b0 = uart.read_u8(port);
                let b1 = uart.read_u8(port.wrapping_add(1));
                let b2 = uart.read_u8(port.wrapping_add(2));
                let b3 = uart.read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => u32::from(uart.read_u8(port)),
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }
        debug_assert_eq!(port, self.port);
        let mut uart = self.uart.borrow_mut();
        match size {
            1 => uart.write_u8(port, value as u8),
            2 => {
                let [b0, b1] = (value as u16).to_le_bytes();
                uart.write_u8(port, b0);
                uart.write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = value.to_le_bytes();
                uart.write_u8(port, b0);
                uart.write_u8(port.wrapping_add(1), b1);
                uart.write_u8(port.wrapping_add(2), b2);
                uart.write_u8(port.wrapping_add(3), b3);
            }
            _ => uart.write_u8(port, value as u8),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_io_size0_is_noop() {
        let uart = Rc::new(RefCell::new(Serial16550::new(0x3F8)));
        uart.borrow_mut().push_rx(0xAB);

        let mut port = Serial16550Port::new(uart.clone(), 0x3F8);

        // Size-0 reads must return 0 and must not consume RX bytes.
        assert_eq!(port.read(0x3F8, 0), 0);
        assert_eq!(port.read(0x3F8, 1) as u8, 0xAB);

        // Size-0 writes must not enqueue TX bytes.
        port.write(0x3F8, 0, 0xCD);
        assert!(uart.borrow_mut().take_tx().is_empty());

        // Sanity: size-1 writes should still work.
        port.write(0x3F8, 1, 0xEF);
        assert_eq!(uart.borrow_mut().take_tx(), vec![0xEF]);
    }
}

pub fn register_serial16550(bus: &mut IoPortBus, uart: SharedSerial16550) {
    let base = uart.borrow().base;
    for offset in 0..8u16 {
        let port = base + offset;
        bus.register(port, Box::new(Serial16550Port::new(uart.clone(), port)));
    }
}
