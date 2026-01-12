use std::cell::RefCell;
use std::rc::Rc;

use aero_devices_input::I8042Controller;

use crate::io::PortIO;

/// Shared i8042 controller handle, suitable for both port I/O emulation and
/// host-side input injection.
pub type SharedI8042Controller = Rc<RefCell<I8042Controller>>;

/// Create a new shared i8042 controller with the canonical `aero-devices-input`
/// model.
pub fn new_shared_controller() -> SharedI8042Controller {
    Rc::new(RefCell::new(I8042Controller::new()))
}

impl PortIO for SharedI8042Controller {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        let mut ctrl = self.borrow_mut();
        match size {
            1 => ctrl.read_port(port) as u32,
            2 => {
                let lo = ctrl.read_port(port) as u32;
                let hi = ctrl.read_port(port.wrapping_add(1)) as u32;
                lo | (hi << 8)
            }
            4 => {
                let b0 = ctrl.read_port(port) as u32;
                let b1 = ctrl.read_port(port.wrapping_add(1)) as u32;
                let b2 = ctrl.read_port(port.wrapping_add(2)) as u32;
                let b3 = ctrl.read_port(port.wrapping_add(3)) as u32;
                b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
            }
            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        let mut ctrl = self.borrow_mut();
        match size {
            1 => ctrl.write_port(port, val as u8),
            2 => {
                let [b0, b1] = (val as u16).to_le_bytes();
                ctrl.write_port(port, b0);
                ctrl.write_port(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = val.to_le_bytes();
                ctrl.write_port(port, b0);
                ctrl.write_port(port.wrapping_add(1), b1);
                ctrl.write_port(port.wrapping_add(2), b2);
                ctrl.write_port(port.wrapping_add(3), b3);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_devices_input::{IrqSink, SystemControlSink};
    use std::cell::RefCell;

    const STATUS_OBF: u8 = 0x01;
    const STATUS_SYS: u8 = 0x04;
    const STATUS_AUX_OBF: u8 = 0x20;

    const OUTPUT_PORT_RESET: u8 = 0x01;
    const OUTPUT_PORT_A20: u8 = 0x02;

    #[derive(Debug, Default)]
    struct TestCtrlState {
        a20: Vec<bool>,
        resets: usize,
        a20_enabled: bool,
    }

    #[derive(Debug, Clone, Default)]
    struct TestSystemControl {
        state: Rc<RefCell<TestCtrlState>>,
    }

    impl TestSystemControl {
        fn a20_events(&self) -> Vec<bool> {
            self.state.borrow().a20.clone()
        }

        fn resets(&self) -> usize {
            self.state.borrow().resets
        }
    }

    impl SystemControlSink for TestSystemControl {
        fn set_a20(&mut self, enabled: bool) {
            let mut state = self.state.borrow_mut();
            state.a20_enabled = enabled;
            state.a20.push(enabled);
        }

        fn request_reset(&mut self) {
            self.state.borrow_mut().resets += 1;
        }

        fn a20_enabled(&self) -> Option<bool> {
            Some(self.state.borrow().a20_enabled)
        }
    }

    #[derive(Debug, Clone, Default)]
    struct TestIrqSink {
        raised: Rc<RefCell<Vec<u8>>>,
    }

    impl TestIrqSink {
        fn take(&self) -> Vec<u8> {
            std::mem::take(&mut self.raised.borrow_mut())
        }
    }

    impl IrqSink for TestIrqSink {
        fn raise_irq(&mut self, irq: u8) {
            self.raised.borrow_mut().push(irq);
        }
    }

    #[test]
    fn write_output_port_toggles_a20() {
        let ctrl = new_shared_controller();
        let sys = TestSystemControl::default();
        ctrl.borrow_mut()
            .set_system_control_sink(Box::new(sys.clone()));

        let mut dev = ctrl.clone();
        dev.port_write(0x64, 1, 0xD1);
        dev.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));
        assert_eq!(sys.a20_events(), vec![true]);

        dev.port_write(0x64, 1, 0xD1);
        dev.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET));
        assert_eq!(sys.a20_events(), vec![true, false]);
    }

    #[test]
    fn write_output_port_reset_bit_requests_reset() {
        let ctrl = new_shared_controller();
        let sys = TestSystemControl::default();
        ctrl.borrow_mut()
            .set_system_control_sink(Box::new(sys.clone()));

        let mut dev = ctrl.clone();
        dev.port_write(0x64, 1, 0xD1);
        dev.port_write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));

        // Assert reset without changing A20.
        dev.port_write(0x64, 1, 0xD1);
        dev.port_write(0x60, 1, u32::from(OUTPUT_PORT_A20));

        assert_eq!(sys.resets(), 1);
        assert_eq!(sys.a20_events(), vec![true]);
    }

    #[test]
    fn read_output_port_returns_last_written_value() {
        let ctrl = new_shared_controller();
        let mut dev = ctrl.clone();

        dev.port_write(0x64, 1, 0xD1);
        dev.port_write(0x60, 1, u32::from(0xABu8));

        dev.port_write(0x64, 1, 0xD0);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0xAB);
    }

    #[test]
    fn controller_self_test_and_command_byte_rw() {
        let ctrl = new_shared_controller();
        let mut dev = ctrl.clone();

        dev.port_write(0x64, 1, 0xAA);
        assert_ne!(dev.port_read(0x64, 1) as u8 & STATUS_OBF, 0);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0x55);
        assert_ne!(dev.port_read(0x64, 1) as u8 & STATUS_SYS, 0);

        dev.port_write(0x64, 1, 0x20);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0x45);

        dev.port_write(0x64, 1, 0x60);
        dev.port_write(0x60, 1, 0x03);

        dev.port_write(0x64, 1, 0x20);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0x03);
    }

    #[test]
    fn keyboard_reset_returns_ack_and_self_test_ok() {
        let ctrl = new_shared_controller();
        let mut dev = ctrl.clone();

        dev.port_write(0x60, 1, 0xFF);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0xFA);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0xAA);
    }

    #[test]
    fn irq_gating_by_command_byte() {
        let ctrl = new_shared_controller();
        let irq = TestIrqSink::default();
        ctrl.borrow_mut().set_irq_sink(Box::new(irq.clone()));

        let mut dev = ctrl.clone();

        // Disable all interrupts and inject a key; no IRQ should be raised.
        dev.port_write(0x64, 1, 0x60);
        dev.port_write(0x60, 1, 0x00);

        ctrl.borrow_mut().inject_browser_key("KeyA", true);
        assert_eq!(irq.take(), Vec::<u8>::new());
        // Drain the scancode.
        dev.port_read(0x60, 1);

        // Enable keyboard IRQ1 and inject a key.
        dev.port_write(0x64, 1, 0x60);
        dev.port_write(0x60, 1, 0x01);

        ctrl.borrow_mut().inject_browser_key("KeyA", true);
        assert_eq!(irq.take(), vec![1]);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0x1C);

        // Enable mouse reporting via 0xD4 prefix; IRQ12 is not yet enabled.
        dev.port_write(0x64, 1, 0xD4);
        dev.port_write(0x60, 1, 0xF4);
        assert_eq!(dev.port_read(0x60, 1) as u8, 0xFA);
        assert_eq!(irq.take(), Vec::<u8>::new());

        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);
        assert_eq!(irq.take(), Vec::<u8>::new());
        assert_ne!(dev.port_read(0x64, 1) as u8 & STATUS_AUX_OBF, 0);
        // Drain packet.
        dev.port_read(0x60, 1);
        dev.port_read(0x60, 1);
        dev.port_read(0x60, 1);

        // Enable mouse IRQ12 and inject again.
        dev.port_write(0x64, 1, 0x60);
        dev.port_write(0x60, 1, 0x03);

        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);
        // One IRQ per byte in the packet.
        dev.port_read(0x60, 1);
        dev.port_read(0x60, 1);
        dev.port_read(0x60, 1);
        assert_eq!(irq.take(), vec![12, 12, 12]);
    }

    #[test]
    fn key_a_is_translated_to_set1_by_default() {
        let ctrl = new_shared_controller();
        let dev = ctrl.clone();

        ctrl.borrow_mut().inject_browser_key("KeyA", true);

        // Set-2 make code for KeyA is 0x1C, which translates to Set-1 0x1E when command-byte bit 6
        // (translation) is enabled (default).
        assert_eq!(dev.port_read(0x60, 1) as u8, 0x1E);
    }

    #[test]
    fn ps2_mouse_stream_packets_are_generated() {
        let ctrl = new_shared_controller();
        let mut dev = ctrl.clone();

        // Enable mouse reporting (write-to-mouse prefix + enable data reporting).
        dev.port_write(0x64, 1, 0xD4);
        dev.port_write(0x60, 1, 0xF4);

        // Mouse ACK.
        assert_eq!(dev.port_read(0x60, 1) as u8, 0xFA);

        // Inject a small movement and verify a 3-byte packet is emitted.
        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);

        let b0 = dev.port_read(0x60, 1) as u8;
        let b1 = dev.port_read(0x60, 1) as u8;
        let b2 = dev.port_read(0x60, 1) as u8;

        assert_eq!([b0, b1, b2], [0x08, 0x01, 0x00]);
    }
}
