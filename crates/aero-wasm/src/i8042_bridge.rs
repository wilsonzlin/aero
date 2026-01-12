//! WASM-side bridge for exposing the canonical i8042 (PS/2 controller) model to JS.
//!
//! The device model lives in `crates/aero-devices-input` (`aero_devices_input::I8042Controller`).
//! This bridge provides a small, JS-friendly ABI so the browser I/O worker can:
//! - forward port I/O for ports 0x60/0x64,
//! - inject host keyboard/mouse events,
//! - snapshot/restore device state via `aero-io-snapshot` deterministic bytes, and
//! - observe IRQ/A20/reset side effects.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use aero_devices_input::{I8042Controller, Ps2MouseButton, SystemControlSink};
use aero_io_snapshot::io::state::IoSnapshot;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

#[derive(Debug, Default)]
struct SystemState {
    a20_enabled: bool,
    reset_requests: u32,
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
/// The bridge exposes an IRQ "level mask" via [`I8042Bridge::irq_mask`]:
/// - bit 0 (`0x01`): IRQ1 asserted (keyboard output byte pending)
/// - bit 1 (`0x02`): IRQ12 asserted (mouse output byte pending)
///
/// The browser I/O worker should translate this into the host `irqRaise`/`irqLower` event stream
/// by comparing the current mask to the previous mask and emitting level transitions for each bit.
/// (See `web/src/workers/io.worker.ts`.)
#[wasm_bindgen]
pub struct I8042Bridge {
    ctrl: I8042Controller,
    sys: Rc<RefCell<SystemState>>,
    mouse_buttons: u8,
}

#[wasm_bindgen]
impl I8042Bridge {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let sys = Rc::new(RefCell::new(SystemState::default()));
        let mut ctrl = I8042Controller::new();
        ctrl.set_system_control_sink(Box::new(BridgeSystemControlSink { state: sys.clone() }));

        Self {
            ctrl,
            sys,
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
        self.mouse_buttons = self.ctrl.mouse_buttons_mask() & 0x07;
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

        let mut bytes = [0u8; 4];
        for (i, slot) in bytes.iter_mut().enumerate().take(len) {
            *slot = ((packed >> (i * 8)) & 0xff) as u8;
        }

        self.ctrl.inject_key_scancode_bytes(&bytes[..len]);
    }

    /// Inject a relative PS/2 mouse movement event.
    ///
    /// Coordinates use PS/2 convention: `dx` is positive right, `dy` is positive up.
    pub fn inject_mouse_move(&mut self, dx: i32, dy: i32) {
        // `aero_devices_input::Ps2Mouse` stores browser-style motion (positive Y down) and flips
        // the sign when encoding PS/2 packets. Convert here so the JS side can stay in PS/2
        // coordinates (matching `InputEventType.MouseMove`).
        self.ctrl.inject_mouse_motion(dx, -dy, 0);
    }

    /// Inject a PS/2 mouse wheel movement (positive = wheel up).
    pub fn inject_mouse_wheel(&mut self, delta: i32) {
        self.ctrl.inject_mouse_motion(0, 0, delta);
    }

    /// Set PS/2 mouse button state as a bitmask (bit0=left, bit1=right, bit2=middle).
    pub fn inject_mouse_buttons(&mut self, buttons: u8) {
        let next = buttons & 0x07;
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
            self.ctrl.inject_mouse_button(
                Ps2MouseButton::Middle,
                (next & 0x04) != 0,
            );
        }

        self.mouse_buttons = next;
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
        self.ctrl
            .load_state(bytes)
            .map_err(|e| js_error(format!("Invalid i8042 snapshot: {e}")))?;
        // The snapshot contains the mouse button image; keep our host-side injection tracker in
        // sync so subsequent absolute button-mask injections compute correct deltas.
        self.mouse_buttons = self.ctrl.mouse_buttons_mask() & 0x07;
        Ok(())
    }
}
