//! WASM-side bridge for exposing the canonical i8042 (PS/2 controller) model to JS.
//!
//! The device model lives in `crates/aero-devices-input` (`aero_devices_input::I8042Controller`).
//! This bridge provides a small, JS-friendly ABI so the browser I/O worker can:
//! - forward port I/O for ports 0x60/0x64,
//! - inject host keyboard/mouse events,
//! - snapshot/restore device state via `aero-io-snapshot` deterministic bytes, and
//! - observe IRQ/A20/reset side effects.
use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use aero_devices_input::{I8042Controller, IrqSink, Ps2MouseButton, SystemControlSink};
use aero_io_snapshot::io::state::IoSnapshot;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

#[derive(Debug, Default)]
struct SystemState {
    a20_enabled: bool,
    reset_requests: u32,
}

#[derive(Debug, Default)]
struct IrqState {
    /// Pending IRQ pulses since the last drain (bit0=IRQ1, bit1=IRQ12).
    pending_mask: u8,
}

#[derive(Clone)]
struct BridgeIrqSink {
    state: Rc<RefCell<IrqState>>,
}

impl IrqSink for BridgeIrqSink {
    fn raise_irq(&mut self, irq: u8) {
        let mut state = self.state.borrow_mut();
        match irq {
            1 => state.pending_mask |= 0x01,
            12 => state.pending_mask |= 0x02,
            _ => {}
        }
    }
}

#[derive(Clone)]
struct BridgeSystemControlSink {
    state: Rc<RefCell<SystemState>>,
}

impl SystemControlSink for BridgeSystemControlSink {
    fn set_a20(&mut self, enabled: bool) {
        self.state.borrow_mut().a20_enabled = enabled;
    }

    fn request_reset(&mut self) {
        let mut state = self.state.borrow_mut();
        state.reset_requests = state.reset_requests.saturating_add(1);
    }

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.state.borrow().a20_enabled)
    }
}

/// WASM export: canonical i8042 controller bridge.
///
/// ## IRQ contract
///
/// The bridge exposes a queued IRQ pulse stream via [`I8042Bridge::drain_irqs`]:
/// - bit 0 (`0x01`): IRQ1 pulse (keyboard byte became available)
/// - bit 1 (`0x02`): IRQ12 pulse (mouse byte became available)
///
/// The browser I/O worker should translate each asserted bit into an explicit pulse in the
/// host `IrqSink` (i.e. `raiseIrq(irq)` followed by `lowerIrq(irq)`).
///
/// For compatibility with older consumers, the bridge also exposes an IRQ "level mask" via
/// [`I8042Bridge::irq_mask`]:
/// - bit 0 (`0x01`): IRQ1 asserted (keyboard output byte pending)
/// - bit 1 (`0x02`): IRQ12 asserted (mouse output byte pending)
///
/// The level mask is useful for debug/observability and legacy fallbacks, but it does **not**
/// faithfully represent edge-triggered i8042 IRQ pulses when the output buffer is refilled
/// immediately after a port `0x60` read (multiple bytes pending). Prefer `drain_irqs`.
#[wasm_bindgen]
pub struct I8042Bridge {
    ctrl: I8042Controller,
    sys: Rc<RefCell<SystemState>>,
    irq: Rc<RefCell<IrqState>>,
    mouse_buttons: u8,
}

impl Default for I8042Bridge {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl I8042Bridge {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let sys = Rc::new(RefCell::new(SystemState::default()));
        let irq = Rc::new(RefCell::new(IrqState::default()));
        let mut ctrl = I8042Controller::new();
        ctrl.set_system_control_sink(Box::new(BridgeSystemControlSink { state: sys.clone() }));
        ctrl.set_irq_sink(Box::new(BridgeIrqSink { state: irq.clone() }));

        Self {
            ctrl,
            sys,
            irq,
            mouse_buttons: 0,
        }
    }

    /// Read a single byte from the guest I/O port space.
    ///
    /// Ports:
    /// - `0x60`: data
    /// - `0x64`: status/command
    pub fn port_read(&mut self, port: u16) -> u8 {
        self.ctrl.read_port(port)
    }

    /// Write a single byte to the guest I/O port space.
    pub fn port_write(&mut self, port: u16, value: u8) {
        self.ctrl.write_port(port, value);
        self.mouse_buttons = self.ctrl.mouse_buttons_mask() & 0x1f;
    }

    /// Return the current guest-set keyboard LED state as a HID-style bitmask.
    ///
    /// Bit layout:
    /// - bit0: Num Lock
    /// - bit1: Caps Lock
    /// - bit2: Scroll Lock
    /// - bit3: Compose
    /// - bit4: Kana
    ///
    /// Note: PS/2 `Set LEDs` uses a different bit order; this helper normalizes it.
    pub fn keyboard_leds(&self) -> u8 {
        // PS/2 raw bit layout (Set LEDs payload): bit0=Scroll, bit1=Num, bit2=Caps.
        let raw = self.ctrl.keyboard().leds() & 0x07;
        let scroll = raw & 0x01;
        let num = (raw >> 1) & 0x01;
        let caps = (raw >> 2) & 0x01;
        num | (caps << 1) | (scroll << 2)
    }

    /// Inject up to 4 Set-2 keyboard scancode bytes.
    ///
    /// The format matches `web/src/input/event_queue.ts`:
    /// - `packed`: little-endian packed bytes (b0 in bits 0..7)
    /// - `len`: number of valid bytes in `packed` (1..=4)
    pub fn inject_key_scancode_bytes(&mut self, packed: u32, len: u8) {
        let len = len.min(4) as usize;
        if len == 0 {
            return;
        }

        let bytes = packed.to_le_bytes();
        self.ctrl.inject_key_scancode_bytes(&bytes[..len]);
    }

    /// Inject raw PS/2 Set-2 scancode bytes into the keyboard device queue.
    ///
    /// This is a convenience API for host injection paths that already have an arbitrary-length
    /// scancode byte sequence and do not want to pack them into 32-bit chunks.
    pub fn inject_keyboard_bytes(&mut self, bytes: &[u8]) {
        self.ctrl.inject_keyboard_bytes(bytes);
    }

    /// Inject a relative PS/2 mouse movement event.
    ///
    /// Coordinates use PS/2 convention: `dx` is positive right, `dy` is positive up.
    pub fn inject_mouse_move(&mut self, dx: i32, dy: i32) {
        // `aero_devices_input::Ps2Mouse` stores browser-style motion (positive Y down) and flips
        // the sign when encoding PS/2 packets. Convert here so the JS side can stay in PS/2
        // coordinates (matching `InputEventType.MouseMove`).
        // Host input values are untrusted; avoid overflow when negating `i32::MIN`.
        self.ctrl
            .inject_mouse_motion(dx, 0i32.saturating_sub(dy), 0);
    }

    /// Inject a PS/2 mouse wheel movement (positive = wheel up).
    pub fn inject_mouse_wheel(&mut self, delta: i32) {
        self.ctrl.inject_mouse_motion(0, 0, delta);
    }

    /// Inject PS/2 mouse motion + wheel in one call.
    ///
    /// `dy` uses PS/2 convention: positive is up.
    pub fn inject_ps2_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        // Host input values are untrusted; avoid overflow when negating `i32::MIN`.
        self.ctrl
            .inject_mouse_motion(dx, 0i32.saturating_sub(dy), wheel);
    }

    /// Set PS/2 mouse button state as a bitmask matching DOM `MouseEvent.buttons` (low 5 bits).
    ///
    /// - bit0 (`0x01`): left
    /// - bit1 (`0x02`): right
    /// - bit2 (`0x04`): middle
    /// - bit3 (`0x08`): back / side (only emitted if the guest enabled the IntelliMouse Explorer
    ///   extension, i.e. device ID 0x04)
    /// - bit4 (`0x10`): forward / extra (same note as bit3)
    pub fn inject_mouse_buttons(&mut self, buttons: u8) {
        let next = buttons & 0x1f;
        let prev = self.mouse_buttons;
        let delta = prev ^ next;

        if (delta & 0x01) != 0 {
            self.ctrl
                .inject_mouse_button(Ps2MouseButton::Left, (next & 0x01) != 0);
        }
        if (delta & 0x02) != 0 {
            self.ctrl
                .inject_mouse_button(Ps2MouseButton::Right, (next & 0x02) != 0);
        }
        if (delta & 0x04) != 0 {
            self.ctrl
                .inject_mouse_button(Ps2MouseButton::Middle, (next & 0x04) != 0);
        }
        if (delta & 0x08) != 0 {
            self.ctrl
                .inject_mouse_button(Ps2MouseButton::Side, (next & 0x08) != 0);
        }
        if (delta & 0x10) != 0 {
            self.ctrl
                .inject_mouse_button(Ps2MouseButton::Extra, (next & 0x10) != 0);
        }

        self.mouse_buttons = next;
    }

    /// Alias for [`I8042Bridge::inject_mouse_buttons`].
    pub fn inject_ps2_mouse_buttons(&mut self, buttons: u8) {
        self.inject_mouse_buttons(buttons);
    }

    /// Drain pending IRQ pulses since the last call.
    ///
    /// Bits:
    /// - bit0: IRQ1 pulse
    /// - bit1: IRQ12 pulse
    ///
    /// The returned value is cleared after the call.
    pub fn drain_irqs(&mut self) -> u8 {
        let mut irq = self.irq.borrow_mut();
        let mask = irq.pending_mask;
        irq.pending_mask = 0;
        mask
    }

    /// IRQ level mask (see module docs).
    pub fn irq_mask(&self) -> u8 {
        let mut mask = 0u8;
        if self.ctrl.irq1_level() {
            mask |= 0x01;
        }
        if self.ctrl.irq12_level() {
            mask |= 0x02;
        }
        mask
    }

    /// Current A20 gate state as last requested by the guest via i8042 commands.
    #[wasm_bindgen(getter)]
    pub fn a20_enabled(&self) -> bool {
        self.sys.borrow().a20_enabled
    }

    /// Drain and clear the number of pending reset requests.
    pub fn take_reset_requests(&mut self) -> u32 {
        let mut sys = self.sys.borrow_mut();
        let count = sys.reset_requests;
        sys.reset_requests = 0;
        count
    }

    /// Serialize the current i8042 controller state into a deterministic `aero-io-snapshot` blob.
    pub fn save_state(&self) -> Vec<u8> {
        self.ctrl.save_state()
    }

    /// Restore controller state from snapshot bytes produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        // Reset pulses are a host-side side effect (not part of the deterministic snapshot blob).
        // Drop any queued reset requests from pre-restore execution.
        self.sys.borrow_mut().reset_requests = 0;

        self.ctrl
            .load_state(bytes)
            .map_err(|e| js_error(format!("Invalid i8042 snapshot: {e}")))?;
        // The snapshot contains the mouse button image; keep our host-side injection tracker in
        // sync so subsequent absolute button-mask injections compute correct deltas.
        self.mouse_buttons = self.ctrl.mouse_buttons_mask() & 0x1f;
        // Snapshot restore should not deliver IRQs for already-buffered bytes; clear any pending
        // pulses that might have been queued by prior activity.
        self.irq.borrow_mut().pending_mask = 0;
        // Likewise, snapshot restore should not deliver any previously queued reset requests.
        // If the guest requested a reset before the snapshot was taken, that event should have
        // been reflected in the coordinator/CPU state; the i8042 device snapshot itself only
        // captures controller/device state.
        self.sys.borrow_mut().reset_requests = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::I8042Bridge;

    #[test]
    fn keyboard_leds_are_reported_as_hid_style_bitmask() {
        let mut bridge = I8042Bridge::new();
        assert_eq!(bridge.keyboard_leds(), 0);

        // PS/2 raw layout: bit0=Scroll, bit1=Num, bit2=Caps.
        // HID layout: bit0=Num, bit1=Caps, bit2=Scroll.

        // Scroll -> HID bit2.
        bridge.port_write(0x0060, 0xED);
        bridge.port_write(0x0060, 0x01);
        assert_eq!(bridge.keyboard_leds(), 0x04);

        // Num -> HID bit0.
        bridge.port_write(0x0060, 0xED);
        bridge.port_write(0x0060, 0x02);
        assert_eq!(bridge.keyboard_leds(), 0x01);

        // Caps -> HID bit1.
        bridge.port_write(0x0060, 0xED);
        bridge.port_write(0x0060, 0x04);
        assert_eq!(bridge.keyboard_leds(), 0x02);
    }
}
