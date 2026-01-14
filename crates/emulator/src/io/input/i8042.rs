use std::cell::RefCell;
use std::rc::Rc;

use aero_devices_input::I8042Controller;

use aero_platform::io::PortIoDevice;

/// Shared i8042 controller handle, suitable for both port I/O emulation and
/// host-side input injection.
pub type SharedI8042Controller = Rc<RefCell<I8042Controller>>;

/// Create a new shared i8042 controller with the canonical `aero-devices-input`
/// model.
pub fn new_shared_controller() -> SharedI8042Controller {
    Rc::new(RefCell::new(I8042Controller::new()))
}

/// Single-port view of an i8042 controller.
///
/// The i8042 responds to ports `0x60` (data) and `0x64` (status/command). The canonical
/// [`aero_platform::io::IoPortBus`] routes by exact port number, so multi-port devices are
/// typically registered as one `PortIoDevice` instance per port that shares a common controller
/// state.
#[derive(Clone)]
pub struct I8042Port {
    inner: SharedI8042Controller,
    port: u16,
}

impl I8042Port {
    pub fn new(inner: SharedI8042Controller, port: u16) -> Self {
        Self { inner, port }
    }
}

impl PortIoDevice for I8042Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        debug_assert_eq!(port, self.port);
        let byte = self.inner.borrow_mut().read_port(self.port);
        match size {
            1 => byte as u32,
            2 => u16::from_le_bytes([byte, byte]) as u32,
            4 => u32::from_le_bytes([byte, byte, byte, byte]),
            _ => byte as u32,
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }
        debug_assert_eq!(port, self.port);
        self.inner
            .borrow_mut()
            .write_port(self.port, (value & 0xFF) as u8);
    }

    fn reset(&mut self) {
        self.inner.borrow_mut().reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_devices_input::{IrqSink, SystemControlSink};
    use aero_platform::io::PortIoDevice;
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

        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);
        port64.write(0x64, 1, 0xD1);
        port60.write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));
        assert_eq!(sys.a20_events(), vec![true]);

        port64.write(0x64, 1, 0xD1);
        port60.write(0x60, 1, u32::from(OUTPUT_PORT_RESET));
        assert_eq!(sys.a20_events(), vec![true, false]);
    }

    #[test]
    fn write_output_port_reset_bit_requests_reset() {
        let ctrl = new_shared_controller();
        let sys = TestSystemControl::default();
        ctrl.borrow_mut()
            .set_system_control_sink(Box::new(sys.clone()));

        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);
        port64.write(0x64, 1, 0xD1);
        port60.write(0x60, 1, u32::from(OUTPUT_PORT_RESET | OUTPUT_PORT_A20));

        // Assert reset without changing A20.
        port64.write(0x64, 1, 0xD1);
        port60.write(0x60, 1, u32::from(OUTPUT_PORT_A20));

        assert_eq!(sys.resets(), 1);
        assert_eq!(sys.a20_events(), vec![true]);
    }

    #[test]
    fn read_output_port_returns_last_written_value() {
        let ctrl = new_shared_controller();
        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        port64.write(0x64, 1, 0xD1);
        port60.write(0x60, 1, u32::from(0xABu8));

        port64.write(0x64, 1, 0xD0);
        assert_eq!(port60.read(0x60, 1) as u8, 0xAB);
    }

    #[test]
    fn controller_self_test_and_command_byte_rw() {
        let ctrl = new_shared_controller();
        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        port64.write(0x64, 1, 0xAA);
        assert_ne!(port64.read(0x64, 1) as u8 & STATUS_OBF, 0);
        assert_eq!(port60.read(0x60, 1) as u8, 0x55);
        assert_ne!(port64.read(0x64, 1) as u8 & STATUS_SYS, 0);

        port64.write(0x64, 1, 0x20);
        assert_eq!(port60.read(0x60, 1) as u8, 0x45);

        port64.write(0x64, 1, 0x60);
        port60.write(0x60, 1, 0x03);

        port64.write(0x64, 1, 0x20);
        assert_eq!(port60.read(0x60, 1) as u8, 0x03);
    }

    #[test]
    fn keyboard_reset_returns_ack_and_self_test_ok() {
        let ctrl = new_shared_controller();
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        port60.write(0x60, 1, 0xFF);
        assert_eq!(port60.read(0x60, 1) as u8, 0xFA);
        assert_eq!(port60.read(0x60, 1) as u8, 0xAA);
    }

    #[test]
    fn irq_gating_by_command_byte() {
        let ctrl = new_shared_controller();
        let irq = TestIrqSink::default();
        ctrl.borrow_mut().set_irq_sink(Box::new(irq.clone()));

        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        // Disable all interrupts and inject a key; no IRQ should be raised.
        port64.write(0x64, 1, 0x60);
        port60.write(0x60, 1, 0x00);

        ctrl.borrow_mut().inject_browser_key("KeyA", true);
        assert_eq!(irq.take(), Vec::<u8>::new());
        // Drain the scancode.
        port60.read(0x60, 1);

        // Enable keyboard IRQ1 and inject a key.
        port64.write(0x64, 1, 0x60);
        port60.write(0x60, 1, 0x01);

        ctrl.borrow_mut().inject_browser_key("KeyA", true);
        assert_eq!(irq.take(), vec![1]);
        assert_eq!(port60.read(0x60, 1) as u8, 0x1C);

        // Enable mouse reporting via 0xD4 prefix; IRQ12 is not yet enabled.
        port64.write(0x64, 1, 0xD4);
        port60.write(0x60, 1, 0xF4);
        assert_eq!(port60.read(0x60, 1) as u8, 0xFA);
        assert_eq!(irq.take(), Vec::<u8>::new());

        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);
        assert_eq!(irq.take(), Vec::<u8>::new());
        assert_ne!(port64.read(0x64, 1) as u8 & STATUS_AUX_OBF, 0);
        // Drain packet.
        port60.read(0x60, 1);
        port60.read(0x60, 1);
        port60.read(0x60, 1);

        // Enable mouse IRQ12 and inject again.
        port64.write(0x64, 1, 0x60);
        port60.write(0x60, 1, 0x03);

        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);
        // One IRQ per byte in the packet.
        port60.read(0x60, 1);
        port60.read(0x60, 1);
        port60.read(0x60, 1);
        assert_eq!(irq.take(), vec![12, 12, 12]);
    }

    #[test]
    fn key_a_is_translated_to_set1_by_default() {
        let ctrl = new_shared_controller();
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        ctrl.borrow_mut().inject_browser_key("KeyA", true);

        // Set-2 make code for KeyA is 0x1C, which translates to Set-1 0x1E when command-byte bit 6
        // (translation) is enabled (default).
        assert_eq!(port60.read(0x60, 1) as u8, 0x1E);
    }

    #[test]
    fn ps2_mouse_stream_packets_are_generated() {
        let ctrl = new_shared_controller();
        let mut port64 = I8042Port::new(ctrl.clone(), 0x64);
        let mut port60 = I8042Port::new(ctrl.clone(), 0x60);

        // Enable mouse reporting (write-to-mouse prefix + enable data reporting).
        port64.write(0x64, 1, 0xD4);
        port60.write(0x60, 1, 0xF4);

        // Mouse ACK.
        assert_eq!(port60.read(0x60, 1) as u8, 0xFA);

        // Inject a small movement and verify a 3-byte packet is emitted.
        ctrl.borrow_mut().inject_mouse_motion(1, 0, 0);

        let b0 = port60.read(0x60, 1) as u8;
        let b1 = port60.read(0x60, 1) as u8;
        let b2 = port60.read(0x60, 1) as u8;

        assert_eq!([b0, b1, b2], [0x08, 0x01, 0x00]);
    }
}
