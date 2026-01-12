//! Glue helpers for the legacy i8042 PS/2 controller.
//!
//! The core device model lives in [`aero_devices_input`] so it can be reused by
//! the WASM build and native tests. The [`aero_platform::io::IoPortBus`] maps a
//! single port to a single device instance, so this module provides small
//! per-port adapters and convenience registration helpers.

use std::cell::RefCell;
use std::rc::Rc;

use aero_devices_input::{I8042Controller, IrqSink, SystemControlSink};
use aero_platform::chipset::A20GateHandle;
use aero_platform::interrupts::{InterruptInput, PlatformInterrupts};
use aero_platform::io::{IoPortBus, PortIoDevice};
use aero_platform::reset::{PlatformResetSink, ResetKind};

pub const I8042_DATA_PORT: u16 = 0x60;
pub const I8042_STATUS_PORT: u16 = 0x64;

pub type SharedI8042Controller = Rc<RefCell<I8042Controller>>;

/// PS/2 i8042 controller exposed as two port-mapped [`PortIoDevice`] handles.
///
/// The controller uses two I/O ports:
/// - `0x60`: data port
/// - `0x64`: status/command port
///
/// [`aero_platform::io::IoPortBus`] currently routes by exact port number, so we
/// expose one device instance per port that shares the same underlying
/// controller.
#[derive(Clone)]
pub struct I8042Ports {
    inner: SharedI8042Controller,
}

impl I8042Ports {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(I8042Controller::new())),
        }
    }

    /// Returns a cloneable handle to the shared controller for host-side input injection.
    pub fn controller(&self) -> SharedI8042Controller {
        self.inner.clone()
    }

    /// Convenience helper to route i8042 IRQ1/IRQ12 pulses to the platform interrupt router.
    ///
    /// The i8042 controller calls [`IrqSink::raise_irq`] when loading a byte into the output
    /// buffer, which matches an edge-triggered interrupt source. The platform router models
    /// edge detection by tracking line level, so we explicitly pulse the line high then low.
    pub fn connect_irqs_to_platform_interrupts(&self, interrupts: Rc<RefCell<PlatformInterrupts>>) {
        self.inner
            .borrow_mut()
            .set_irq_sink(Box::new(PlatformIrqSink::new(interrupts)));
    }

    pub fn port60(&self) -> I8042Port {
        I8042Port::new(self.inner.clone(), I8042_DATA_PORT)
    }

    pub fn port64(&self) -> I8042Port {
        I8042Port::new(self.inner.clone(), I8042_STATUS_PORT)
    }
}

impl Default for I8042Ports {
    fn default() -> Self {
        Self::new()
    }
}

/// I/O-port view of a shared i8042 controller.
///
/// `IoPortBus` maps one port to one device instance. The i8042 controller
/// responds to ports `0x60` and `0x64`, so the common pattern is to share the
/// controller behind `Rc<RefCell<_>>` and register two `I8042Port` instances.
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
        // Reset the shared controller back to its power-on state. This is safe to call multiple
        // times (once per port mapping) as the operation is idempotent.
        self.inner.borrow_mut().reset();
    }
}

/// Convenience helper to register the i8042 controller ports on an [`IoPortBus`].
pub fn register_i8042(bus: &mut IoPortBus, ctrl: SharedI8042Controller) {
    bus.register(
        I8042_DATA_PORT,
        Box::new(I8042Port::new(ctrl.clone(), I8042_DATA_PORT)),
    );
    bus.register(
        I8042_STATUS_PORT,
        Box::new(I8042Port::new(ctrl, I8042_STATUS_PORT)),
    );
}

/// Routes i8042 IRQ pulses to the platform interrupt router.
///
/// The i8042 model emits IRQs as pulses (edge-triggered). `PlatformInterrupts`
/// models ISA lines as level state with edge detection, so we raise and
/// immediately lower the line to generate a single edge.
pub struct PlatformIrqSink {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl PlatformIrqSink {
    pub fn new(interrupts: Rc<RefCell<PlatformInterrupts>>) -> Self {
        Self { interrupts }
    }
}

impl IrqSink for PlatformIrqSink {
    fn raise_irq(&mut self, irq: u8) {
        let mut interrupts = self.interrupts.borrow_mut();
        interrupts.raise_irq(InterruptInput::IsaIrq(irq));
        interrupts.lower_irq(InterruptInput::IsaIrq(irq));
    }
}

/// Bridges i8042 output-port side effects (A20 + reset) into the platform chipset.
pub struct PlatformSystemControlSink {
    a20: A20GateHandle,
    reset: Option<Box<dyn PlatformResetSink>>,
}

impl PlatformSystemControlSink {
    pub fn new(a20: A20GateHandle) -> Self {
        Self { a20, reset: None }
    }

    pub fn with_reset_sink(a20: A20GateHandle, reset: impl PlatformResetSink + 'static) -> Self {
        Self {
            a20,
            reset: Some(Box::new(reset)),
        }
    }
}

impl SystemControlSink for PlatformSystemControlSink {
    fn set_a20(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn request_reset(&mut self) {
        if let Some(reset) = self.reset.as_mut() {
            reset.request_reset(ResetKind::System);
        }
    }

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20.enabled())
    }
}
