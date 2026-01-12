use aero_devices::serial::Serial16550;

const COM1: u16 = 0x3F8;

#[test]
fn iir_reports_no_interrupt_pending_by_default() {
    let mut uart = Serial16550::new(COM1);
    assert_eq!(uart.read_u8(COM1 + 2), 0x01);
    assert!(!uart.irq_level());
}

#[test]
fn rx_interrupt_reports_in_iir_and_irq_level_is_gated_by_out2() {
    let mut uart = Serial16550::new(COM1);

    // Enable "received data available" interrupt.
    uart.write_u8(COM1 + 1, 0x01);

    // Inject a byte into the receive FIFO.
    uart.push_rx(0x41);

    // Interrupt reason should be visible in IIR (independent of OUT2 gating).
    assert_eq!(uart.read_u8(COM1 + 2) & 0x0F, 0x04);

    // But the external IRQ line is gated by MCR.OUT2 on PC hardware.
    assert!(!uart.irq_level());

    // Enable OUT2 to allow interrupt delivery.
    uart.write_u8(COM1 + 4, 0x08);
    assert!(uart.irq_level());

    // Reading the receive buffer drains the FIFO and clears the pending condition.
    assert_eq!(uart.read_u8(COM1 + 0), 0x41);
    assert_eq!(uart.read_u8(COM1 + 2) & 0x0F, 0x01);
    assert!(!uart.irq_level());
}

#[test]
fn fifo_enable_sets_iir_fifo_status_bits_and_clear_rx_works() {
    let mut uart = Serial16550::new(COM1);

    // Enable FIFO (FCR bit0).
    uart.write_u8(COM1 + 2, 0x01);
    assert_eq!(uart.read_u8(COM1 + 2), 0xC1);

    // Populate RX, then clear it via FCR bit1.
    uart.push_rx(0xAA);
    uart.write_u8(COM1 + 2, 0x03); // FIFO enable + clear RX
    assert_eq!(uart.read_u8(COM1 + 0), 0x00);
}

