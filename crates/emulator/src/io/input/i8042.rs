use core::cell::{Cell, RefCell};
use std::collections::VecDeque;

use crate::io::PortIO;

use super::ps2_keyboard::Ps2Keyboard;
use super::ps2_mouse::Ps2Mouse;

const STATUS_OBF: u8 = 0x01; // Output buffer full
const STATUS_IBF: u8 = 0x02; // Input buffer full
const STATUS_SYS: u8 = 0x04; // System flag
const STATUS_MOBF: u8 = 0x20; // Mouse output buffer full

const CMD_BYTE_KBD_INT: u8 = 0x01;
const CMD_BYTE_MOUSE_INT: u8 = 0x02;
const CMD_BYTE_KBD_DISABLE: u8 = 0x10;
const CMD_BYTE_MOUSE_DISABLE: u8 = 0x20;

const OUTPUT_PORT_RESET: u8 = 0x01; // Bit 0 (active-low reset line)
const OUTPUT_PORT_A20: u8 = 0x02; // Bit 1

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSource {
    Controller,
    Keyboard,
    Mouse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCommand {
    WriteCommandByte,
    WriteOutputPort,
    WriteToMouse,
}

/// Hooks for wiring the i8042 controller into the rest of the system.
pub trait I8042Callbacks {
    /// (De)assert the A20 gate.
    fn set_a20(&mut self, enabled: bool);

    /// Request a CPU/system reset (usually via i8042 output port bit 0).
    fn request_reset(&mut self);
}

impl I8042Callbacks for () {
    fn set_a20(&mut self, _enabled: bool) {}

    fn request_reset(&mut self) {}
}

/// i8042 PS/2 controller (ports 0x60/0x64).
///
/// This implements the legacy PS/2 controller used by BIOS and Windows
/// (`i8042prt`) drivers, including:
/// - command byte read/write and port enable/disable
/// - controller self-test and port tests
/// - output buffer source tracking (keyboard vs mouse)
/// - output port read/write (A20 + reset)
#[derive(Debug)]
pub struct I8042Controller<Cb: I8042Callbacks> {
    status: Cell<u8>,
    command_byte: Cell<u8>,

    output_buffer: Cell<u8>,
    output_source: Cell<OutputSource>,
    output_queue: RefCell<VecDeque<(u8, OutputSource)>>,

    pending_command: Cell<Option<PendingCommand>>,

    /// Controller output port.
    ///
    /// Important bits:
    /// - bit 0: system reset line (active low)
    /// - bit 1: A20 gate
    output_port: Cell<u8>,

    callbacks: Cb,

    keyboard: RefCell<Ps2Keyboard>,
    mouse: RefCell<Ps2Mouse>,

    keyboard_irq_pending: Cell<bool>,
    mouse_irq_pending: Cell<bool>,
}

impl<Cb: I8042Callbacks> I8042Controller<Cb> {
    pub fn new(callbacks: Cb) -> Self {
        Self {
            status: Cell::new(0),
            command_byte: Cell::new(0),
            output_buffer: Cell::new(0),
            output_source: Cell::new(OutputSource::Controller),
            output_queue: RefCell::new(VecDeque::new()),
            pending_command: Cell::new(None),
            // Platform dependent; bit 0 typically deasserted, A20 typically off.
            output_port: Cell::new(OUTPUT_PORT_RESET),
            callbacks,
            keyboard: RefCell::new(Ps2Keyboard::new()),
            mouse: RefCell::new(Ps2Mouse::new()),
            keyboard_irq_pending: Cell::new(false),
            mouse_irq_pending: Cell::new(false),
        }
    }

    pub fn callbacks(&self) -> &Cb {
        &self.callbacks
    }

    pub fn callbacks_mut(&mut self) -> &mut Cb {
        &mut self.callbacks
    }

    pub fn keyboard_irq_pending(&self) -> bool {
        self.keyboard_irq_pending.get()
    }

    pub fn mouse_irq_pending(&self) -> bool {
        self.mouse_irq_pending.get()
    }

    pub fn key_event(&self, scancode: u8, pressed: bool, extended: bool) {
        if (self.command_byte.get() & CMD_BYTE_KBD_DISABLE) != 0 {
            return;
        }

        let mut kbd = self.keyboard.borrow_mut();
        kbd.key_event(scancode, pressed, extended);
        while let Some(byte) = kbd.pop_output_byte() {
            self.enqueue_output(byte, OutputSource::Keyboard);
        }
    }

    pub fn mouse_movement(&self, dx: i32, dy: i32, dz: i32) {
        if (self.command_byte.get() & CMD_BYTE_MOUSE_DISABLE) != 0 {
            return;
        }

        let mut mouse = self.mouse.borrow_mut();
        mouse.movement(dx, dy, dz);
        while let Some(byte) = mouse.pop_output_byte() {
            self.enqueue_output(byte, OutputSource::Mouse);
        }
    }

    pub fn mouse_button_event(&self, button_mask: u8, pressed: bool) {
        if (self.command_byte.get() & CMD_BYTE_MOUSE_DISABLE) != 0 {
            return;
        }

        let mut mouse = self.mouse.borrow_mut();
        mouse.button_event(button_mask, pressed);
        while let Some(byte) = mouse.pop_output_byte() {
            self.enqueue_output(byte, OutputSource::Mouse);
        }
    }

    fn read_u8(&self, port: u16) -> u8 {
        match port {
            0x60 => self.read_data_port(),
            0x64 => self.status.get(),
            _ => 0xFF,
        }
    }

    fn write_u8(&mut self, port: u16, value: u8) {
        match port {
            0x60 => self.write_data_port(value),
            0x64 => self.execute_controller_command(value),
            _ => {}
        }
    }

    fn read_data_port(&self) -> u8 {
        let status = self.status.get();
        if status & STATUS_OBF == 0 {
            return 0xFF;
        }

        let value = self.output_buffer.get();

        match self.output_source.get() {
            OutputSource::Keyboard => self.keyboard_irq_pending.set(false),
            OutputSource::Mouse => self.mouse_irq_pending.set(false),
            OutputSource::Controller => {}
        }

        let new_status = status & !STATUS_OBF & !STATUS_MOBF;
        self.status.set(new_status);
        self.output_source.set(OutputSource::Controller);

        self.pump_output_queue();
        value
    }

    fn write_data_port(&mut self, value: u8) {
        // Operations complete synchronously; IBF is asserted for the duration of
        // this function and cleared at the end.
        self.status.set(self.status.get() | STATUS_IBF);

        if let Some(pending) = self.pending_command.take() {
            self.execute_controller_command_data(pending, value);
        } else {
            self.send_to_keyboard(value);
        }

        self.status.set(self.status.get() & !STATUS_IBF);
    }

    fn execute_controller_command(&mut self, cmd: u8) {
        match cmd {
            0x20 => {
                // Read command byte.
                self.enqueue_output(self.command_byte.get(), OutputSource::Controller);
            }
            0x60 => {
                // Write command byte (next byte on data port).
                self.pending_command
                    .set(Some(PendingCommand::WriteCommandByte));
            }
            0xA7 => {
                // Disable mouse port.
                self.command_byte
                    .set(self.command_byte.get() | CMD_BYTE_MOUSE_DISABLE);
            }
            0xA8 => {
                // Enable mouse port.
                self.command_byte
                    .set(self.command_byte.get() & !CMD_BYTE_MOUSE_DISABLE);
            }
            0xA9 => {
                // Test mouse port (0 = pass).
                self.enqueue_output(0x00, OutputSource::Controller);
            }
            0xAA => {
                // Controller self-test (0x55 = pass).
                self.status.set(self.status.get() | STATUS_SYS);
                self.enqueue_output(0x55, OutputSource::Controller);
            }
            0xAB => {
                // Test keyboard port (0 = pass).
                self.enqueue_output(0x00, OutputSource::Controller);
            }
            0xAD => {
                // Disable keyboard.
                self.command_byte
                    .set(self.command_byte.get() | CMD_BYTE_KBD_DISABLE);
            }
            0xAE => {
                // Enable keyboard.
                self.command_byte
                    .set(self.command_byte.get() & !CMD_BYTE_KBD_DISABLE);
            }
            0xD0 => {
                // Read output port.
                self.enqueue_output(self.output_port.get(), OutputSource::Controller);
            }
            0xD1 => {
                // Write output port (next byte on data port).
                self.pending_command
                    .set(Some(PendingCommand::WriteOutputPort));
            }
            0xD4 => {
                // Write to mouse (next byte on data port).
                self.pending_command.set(Some(PendingCommand::WriteToMouse));
            }
            0xDD => {
                // Non-standard but seen in some firmware: disable A20 gate.
                let new = self.output_port.get() & !OUTPUT_PORT_A20;
                self.write_output_port(new);
            }
            0xDF => {
                // Non-standard but seen in some firmware: enable A20 gate.
                let new = self.output_port.get() | OUTPUT_PORT_A20;
                self.write_output_port(new);
            }
            0xFE => {
                // Pulse output port bit 0 low (system reset).
                self.callbacks.request_reset();
            }
            _ => {}
        }
    }

    fn execute_controller_command_data(&mut self, cmd: PendingCommand, value: u8) {
        match cmd {
            PendingCommand::WriteCommandByte => self.command_byte.set(value),
            PendingCommand::WriteOutputPort => self.write_output_port(value),
            PendingCommand::WriteToMouse => self.send_to_mouse(value),
        }
    }

    fn write_output_port(&mut self, value: u8) {
        let prev = self.output_port.get();
        self.output_port.set(value);

        let prev_a20 = (prev & OUTPUT_PORT_A20) != 0;
        let new_a20 = (value & OUTPUT_PORT_A20) != 0;
        if prev_a20 != new_a20 {
            self.callbacks.set_a20(new_a20);
        }

        // Bit 0 is active low: transitioning from 1 -> 0 asserts reset.
        let prev_reset_deasserted = (prev & OUTPUT_PORT_RESET) != 0;
        let new_reset_deasserted = (value & OUTPUT_PORT_RESET) != 0;
        if prev_reset_deasserted && !new_reset_deasserted {
            self.callbacks.request_reset();
        }
    }

    fn enqueue_output(&self, value: u8, source: OutputSource) {
        if self.status.get() & STATUS_OBF == 0 {
            self.set_output_now(value, source);
        } else {
            self.output_queue.borrow_mut().push_back((value, source));
        }
    }

    fn set_output_now(&self, value: u8, source: OutputSource) {
        self.output_buffer.set(value);
        self.output_source.set(source);

        let mut status = self.status.get() | STATUS_OBF;

        match source {
            OutputSource::Mouse => {
                status |= STATUS_MOBF;
                if (self.command_byte.get() & (CMD_BYTE_MOUSE_INT | CMD_BYTE_MOUSE_DISABLE))
                    == CMD_BYTE_MOUSE_INT
                {
                    self.mouse_irq_pending.set(true);
                }
            }
            OutputSource::Keyboard => {
                status &= !STATUS_MOBF;
                if (self.command_byte.get() & (CMD_BYTE_KBD_INT | CMD_BYTE_KBD_DISABLE))
                    == CMD_BYTE_KBD_INT
                {
                    self.keyboard_irq_pending.set(true);
                }
            }
            OutputSource::Controller => {
                status &= !STATUS_MOBF;
            }
        }

        self.status.set(status);
    }

    fn pump_output_queue(&self) {
        if self.status.get() & STATUS_OBF != 0 {
            return;
        }

        if let Some((value, source)) = self.output_queue.borrow_mut().pop_front() {
            self.set_output_now(value, source);
        }
    }

    fn send_to_keyboard(&self, value: u8) {
        let mut kbd = self.keyboard.borrow_mut();
        kbd.receive_byte(value);
        while let Some(byte) = kbd.pop_output_byte() {
            self.enqueue_output(byte, OutputSource::Keyboard);
        }
    }

    fn send_to_mouse(&self, value: u8) {
        let mut mouse = self.mouse.borrow_mut();
        mouse.receive_byte(value);
        while let Some(byte) = mouse.pop_output_byte() {
            self.enqueue_output(byte, OutputSource::Mouse);
        }
    }
}

impl<Cb: I8042Callbacks> PortIO for I8042Controller<Cb> {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        match size {
            1 => u32::from(self.read_u8(port)),
            2 => {
                let lo = self.read_u8(port);
                let hi = self.read_u8(port.wrapping_add(1));
                u32::from(u16::from_le_bytes([lo, hi]))
            }
            4 => {
                let b0 = self.read_u8(port);
                let b1 = self.read_u8(port.wrapping_add(1));
                let b2 = self.read_u8(port.wrapping_add(2));
                let b3 = self.read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match size {
            1 => self.write_u8(port, val as u8),
            2 => {
                let [b0, b1] = (val as u16).to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = val.to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
                self.write_u8(port.wrapping_add(2), b2);
                self.write_u8(port.wrapping_add(3), b3);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestCallbacks {
        a20: Vec<bool>,
        resets: usize,
    }

    impl I8042Callbacks for TestCallbacks {
        fn set_a20(&mut self, enabled: bool) {
            self.a20.push(enabled);
        }

        fn request_reset(&mut self) {
            self.resets += 1;
        }
    }

    #[test]
    fn write_output_port_toggles_a20() {
        let mut ctrl = I8042Controller::new(TestCallbacks::default());

        ctrl.port_write(0x64, 1, 0xD1);
        ctrl.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));
        assert_eq!(ctrl.callbacks().a20, vec![true]);

        ctrl.port_write(0x64, 1, 0xD1);
        ctrl.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET));
        assert_eq!(ctrl.callbacks().a20, vec![true, false]);
    }

    #[test]
    fn write_output_port_reset_bit_requests_reset() {
        let mut ctrl = I8042Controller::new(TestCallbacks::default());

        ctrl.port_write(0x64, 1, 0xD1);
        ctrl.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));

        // Assert reset without changing A20.
        ctrl.port_write(0x64, 1, 0xD1);
        ctrl.port_write(0x60, 1, u32::from(OUTPUT_PORT_A20));

        assert_eq!(ctrl.callbacks().resets, 1);
        assert_eq!(ctrl.callbacks().a20, vec![true]);
    }

    #[test]
    fn read_output_port_returns_last_written_value() {
        let mut ctrl = I8042Controller::new(TestCallbacks::default());

        ctrl.port_write(0x64, 1, 0xD1);
        ctrl.port_write(0x60, 1, 0xAB);

        ctrl.port_write(0x64, 1, 0xD0);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0xAB);
    }

    #[test]
    fn controller_self_test_and_command_byte_rw() {
        let mut ctrl = I8042Controller::new(());

        ctrl.port_write(0x64, 1, 0xAA);
        assert_ne!(ctrl.port_read(0x64, 1) as u8 & STATUS_OBF, 0);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0x55);
        assert_ne!(ctrl.port_read(0x64, 1) as u8 & STATUS_SYS, 0);

        ctrl.port_write(0x64, 1, 0x20);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0x00);

        ctrl.port_write(0x64, 1, 0x60);
        ctrl.port_write(0x60, 1, 0x03);

        ctrl.port_write(0x64, 1, 0x20);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0x03);
    }

    #[test]
    fn keyboard_reset_returns_ack_and_self_test_ok() {
        let mut ctrl = I8042Controller::new(());

        ctrl.port_write(0x60, 1, 0xFF);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0xFA);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0xAA);
    }

    #[test]
    fn irq_gating_by_command_byte() {
        let mut ctrl = I8042Controller::new(());

        // Enable keyboard IRQ1 and inject a key.
        ctrl.port_write(0x64, 1, 0x60);
        ctrl.port_write(0x60, 1, u32::from(CMD_BYTE_KBD_INT));

        ctrl.key_event(0x1C, true, false);
        assert!(ctrl.keyboard_irq_pending());
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0x1C);
        assert!(!ctrl.keyboard_irq_pending());

        // Enable mouse reporting via 0xD4 prefix; IRQ12 is not yet enabled.
        ctrl.port_write(0x64, 1, 0xD4);
        ctrl.port_write(0x60, 1, 0xF4);
        assert_eq!(ctrl.port_read(0x60, 1) as u8, 0xFA);
        assert!(!ctrl.mouse_irq_pending());

        ctrl.mouse_movement(1, 0, 0);
        assert!(!ctrl.mouse_irq_pending());
        assert_ne!(ctrl.port_read(0x64, 1) as u8 & STATUS_MOBF, 0);
        // Drain packet.
        ctrl.port_read(0x60, 1);
        ctrl.port_read(0x60, 1);
        ctrl.port_read(0x60, 1);

        // Enable mouse IRQ12 and inject again.
        ctrl.port_write(0x64, 1, 0x60);
        ctrl.port_write(0x60, 1, u32::from(CMD_BYTE_KBD_INT | CMD_BYTE_MOUSE_INT));

        ctrl.mouse_movement(1, 0, 0);
        assert!(ctrl.mouse_irq_pending());
        // Drain packet; IRQ should be cleared after the final byte.
        ctrl.port_read(0x60, 1);
        ctrl.port_read(0x60, 1);
        ctrl.port_read(0x60, 1);
        assert!(!ctrl.mouse_irq_pending());
    }
}
