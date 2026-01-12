use std::cell::RefCell;
use std::rc::Rc;

use emulator::io::serial::{Uart16550, UartConfig};
use emulator::io::PortIO;

#[test]
fn lsr_reports_thr_empty_and_data_ready() {
    let uart = Uart16550::new(UartConfig::COM1);
    let base = uart.config().base_port;

    let lsr = uart.port_read(base + 5, 1) as u8;
    assert_eq!(lsr & 0x60, 0x60);
    assert_eq!(lsr & 0x01, 0);

    uart.inject_rx(0x41);
    let lsr = uart.port_read(base + 5, 1) as u8;
    assert_eq!(lsr & 0x01, 0x01);

    let byte = uart.port_read(base, 1) as u8;
    assert_eq!(byte, 0x41);
    let lsr = uart.port_read(base + 5, 1) as u8;
    assert_eq!(lsr & 0x01, 0);
}

#[test]
fn dlab_switches_data_registers() {
    let mut uart = Uart16550::new(UartConfig::COM1);
    let base = uart.config().base_port;

    uart.port_write(base + 3, 1, 0x80);
    uart.port_write(base, 1, 0x34);
    uart.port_write(base + 1, 1, 0x12);
    assert_eq!(uart.port_read(base, 1) as u8, 0x34);
    assert_eq!(uart.port_read(base + 1, 1) as u8, 0x12);

    uart.port_write(base + 3, 1, 0x00);
    uart.port_write(base + 1, 1, 0xAA);
    assert_eq!(uart.port_read(base + 1, 1) as u8, 0xAA);
}

#[test]
fn thr_write_invokes_tx_callback() {
    let mut uart = Uart16550::new(UartConfig::COM1);
    let base = uart.config().base_port;

    let bytes = Rc::new(RefCell::new(Vec::new()));
    uart.set_tx_callback({
        let bytes = Rc::clone(&bytes);
        move |port, data| {
            assert_eq!(port, base);
            bytes.borrow_mut().extend_from_slice(data);
        }
    });

    uart.port_write(base, 1, b'H' as u32);
    uart.port_write(base, 1, b'i' as u32);

    assert_eq!(&*bytes.borrow(), b"Hi");
}

#[test]
fn iir_fifo_bits_reflect_fcr_enable() {
    let mut uart = Uart16550::new(UartConfig::COM1);
    let base = uart.config().base_port;

    let iir = uart.port_read(base + 2, 1) as u8;
    assert_eq!(iir & 0xC0, 0x00);

    uart.port_write(base + 2, 1, 0x01);
    let iir = uart.port_read(base + 2, 1) as u8;
    assert_eq!(iir & 0xC0, 0xC0);
}
