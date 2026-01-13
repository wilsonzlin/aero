use aero_devices::serial::Serial16550;

#[test]
fn lsr_reports_thr_empty_and_data_ready() {
    let mut uart = Serial16550::new(0x3F8);
    let base = 0x3F8;

    let lsr = uart.read_u8(base + 5);
    assert_eq!(lsr & 0x60, 0x60);
    assert_eq!(lsr & 0x01, 0);

    uart.push_rx(0x41);
    let lsr = uart.read_u8(base + 5);
    assert_eq!(lsr & 0x01, 0x01);

    let byte = uart.read_u8(base);
    assert_eq!(byte, 0x41);
    let lsr = uart.read_u8(base + 5);
    assert_eq!(lsr & 0x01, 0);
}

#[test]
fn dlab_switches_data_registers() {
    let mut uart = Serial16550::new(0x3F8);
    let base = 0x3F8;

    uart.write_u8(base + 3, 0x80);
    uart.write_u8(base, 0x34);
    uart.write_u8(base + 1, 0x12);
    assert_eq!(uart.read_u8(base), 0x34);
    assert_eq!(uart.read_u8(base + 1), 0x12);

    uart.write_u8(base + 3, 0x00);
    uart.write_u8(base + 1, 0xAA);
    assert_eq!(uart.read_u8(base + 1), 0xAA);
}

#[test]
fn thr_write_enqueues_tx_bytes() {
    let mut uart = Serial16550::new(0x3F8);
    let base = 0x3F8;

    uart.write_u8(base, b'H');
    uart.write_u8(base, b'i');

    assert_eq!(uart.take_tx(), b"Hi".to_vec());
}

#[test]
fn iir_fifo_bits_reflect_fcr_enable() {
    let mut uart = Serial16550::new(0x3F8);
    let base = 0x3F8;

    let iir = uart.read_u8(base + 2);
    assert_eq!(iir & 0xC0, 0x00);

    uart.write_u8(base + 2, 0x01);
    let iir = uart.read_u8(base + 2);
    assert_eq!(iir & 0xC0, 0xC0);
}
