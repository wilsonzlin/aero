use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::UsbInResult;
use crate::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

use super::{
    build_string_descriptor_utf16le, clamp_response, gamepad::GamepadReport,
    keyboard::KeyboardReport, mouse::MouseReport, HidProtocol, HID_REQUEST_GET_IDLE,
    HID_REQUEST_GET_PROTOCOL, HID_REQUEST_GET_REPORT, HID_REQUEST_SET_IDLE,
    HID_REQUEST_SET_PROTOCOL, HID_REQUEST_SET_REPORT, USB_DESCRIPTOR_TYPE_CONFIGURATION,
    USB_DESCRIPTOR_TYPE_DEVICE, USB_DESCRIPTOR_TYPE_HID, USB_DESCRIPTOR_TYPE_HID_REPORT,
    USB_DESCRIPTOR_TYPE_STRING, USB_FEATURE_DEVICE_REMOTE_WAKEUP, USB_FEATURE_ENDPOINT_HALT,
    USB_REQUEST_CLEAR_FEATURE, USB_REQUEST_GET_CONFIGURATION, USB_REQUEST_GET_DESCRIPTOR,
    USB_REQUEST_GET_INTERFACE, USB_REQUEST_GET_STATUS, USB_REQUEST_SET_ADDRESS,
    USB_REQUEST_SET_CONFIGURATION, USB_REQUEST_SET_FEATURE, USB_REQUEST_SET_INTERFACE,
};

const KEYBOARD_INTERFACE: u8 = 0;
const MOUSE_INTERFACE: u8 = 1;
const GAMEPAD_INTERFACE: u8 = 2;

const KEYBOARD_INTERRUPT_IN_EP: u8 = 0x81;
const MOUSE_INTERRUPT_IN_EP: u8 = 0x82;
const GAMEPAD_INTERRUPT_IN_EP: u8 = 0x83;

const MAX_PENDING_KEYBOARD_REPORTS: usize = 64;
const MAX_PENDING_MOUSE_REPORTS: usize = 128;
const MAX_PENDING_GAMEPAD_REPORTS: usize = 128;
const MAX_PRESSED_KEYS: usize = 256;

#[derive(Debug, Clone)]
struct KeyboardInterface {
    idle_rate: u8,
    protocol: HidProtocol,
    leds: u8,

    modifiers: u8,
    pressed_keys: Vec<u8>,

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl KeyboardInterface {
    fn new() -> Self {
        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            leds: 0,
            modifiers: 0,
            pressed_keys: Vec::new(),
            last_report: [0; 8],
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn key_event(&mut self, usage: u8, pressed: bool, configured: bool) {
        if usage == 0 {
            return;
        }

        let mut changed = false;
        let modifier = modifier_bit(usage);
        if modifier.is_none() && usage > super::keyboard::KEY_USAGE_MAX {
            return;
        }
        if let Some(bit) = modifier {
            let before = self.modifiers;
            if pressed {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            changed = before != self.modifiers;
        } else if pressed {
            if !self.pressed_keys.contains(&usage) {
                self.pressed_keys.push(usage);
                changed = true;
            }
        } else {
            let before_len = self.pressed_keys.len();
            self.pressed_keys.retain(|&k| k != usage);
            changed = before_len != self.pressed_keys.len();
        }

        if changed && configured {
            self.enqueue_current_report();
        }
    }

    fn current_input_report(&self) -> KeyboardReport {
        let mut keys = [0u8; 6];
        if self.pressed_keys.len() > 6 {
            keys.fill(0x01); // ErrorRollOver
        } else {
            for (idx, &usage) in self.pressed_keys.iter().take(6).enumerate() {
                keys[idx] = usage;
            }
        }
        KeyboardReport {
            modifiers: self.modifiers,
            reserved: 0,
            keys,
        }
    }

    fn enqueue_current_report(&mut self) {
        let report = self.current_input_report().to_bytes();
        if report != self.last_report {
            self.last_report = report;
            if self.pending_reports.len() >= MAX_PENDING_KEYBOARD_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports.pop_front().map(|r| r.to_vec())
    }
}

#[derive(Debug, Clone)]
struct MouseInterface {
    idle_rate: u8,
    protocol: HidProtocol,

    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,
    hwheel: i32,

    pending_reports: VecDeque<MouseReport>,
}

impl MouseInterface {
    fn new() -> Self {
        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
            hwheel: 0,
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn push_report(&mut self, report: MouseReport, configured: bool) {
        if !configured {
            return;
        }
        if self.pending_reports.len() >= MAX_PENDING_MOUSE_REPORTS {
            self.pending_reports.pop_front();
        }
        self.pending_reports.push_back(report);
    }

    fn button_event(&mut self, button_bit: u8, pressed: bool, configured: bool) {
        let bit = button_bit & 0x1f;
        if bit == 0 {
            return;
        }
        self.flush_motion(configured);
        // Ignore any stale padding bits (e.g. from a corrupt snapshot) when determining whether the
        // guest-visible button state actually changed.
        let visible_mask = match self.protocol {
            HidProtocol::Boot => 0x07,
            HidProtocol::Report => 0x1f,
        };
        let before = self.buttons & visible_mask;
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.buttons &= 0x1f;
        if (self.buttons & visible_mask) != before {
            self.push_report(
                MouseReport {
                    buttons: self.buttons,
                    x: 0,
                    y: 0,
                    wheel: 0,
                    hwheel: 0,
                },
                configured,
            );
        }
    }

    fn movement(&mut self, dx: i32, dy: i32, configured: bool) {
        // Host input is untrusted; use saturating arithmetic so extreme values cannot overflow
        // before we clamp/split them into HID reports.
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        self.flush_motion(configured);
    }

    fn wheel(&mut self, delta: i32, configured: bool) {
        self.wheel2(delta, 0, configured);
    }

    fn hwheel(&mut self, delta: i32, configured: bool) {
        self.wheel2(0, delta, configured);
    }

    fn wheel2(&mut self, wheel: i32, hwheel: i32, configured: bool) {
        self.wheel = self.wheel.saturating_add(wheel);
        self.hwheel = self.hwheel.saturating_add(hwheel);
        self.flush_motion(configured);
    }

    fn flush_motion(&mut self, configured: bool) {
        // USB interrupt endpoints are inactive until the device is configured; align with
        // `push_report` semantics by dropping accumulated motion immediately.
        if !configured {
            self.dx = 0;
            self.dy = 0;
            self.wheel = 0;
            self.hwheel = 0;
            return;
        }

        // The HID boot mouse protocol does not include wheel/hwheel fields. Avoid generating
        // guest-visible no-op reports for scroll input when the guest has selected boot protocol.
        if self.protocol == HidProtocol::Boot {
            self.wheel = 0;
            self.hwheel = 0;
        }

        // Cap per-flush work so absurd/hostile deltas can't spin in an unbounded loop producing
        // reports that will be dropped by the bounded queue anyway.
        let mut emitted = 0usize;
        while self.dx != 0 || self.dy != 0 || self.wheel != 0 || self.hwheel != 0 {
            if emitted >= MAX_PENDING_MOUSE_REPORTS {
                self.dx = 0;
                self.dy = 0;
                self.wheel = 0;
                self.hwheel = 0;
                break;
            }
            let step_x = self.dx.clamp(-127, 127) as i8;
            let step_y = self.dy.clamp(-127, 127) as i8;
            let step_wheel = self.wheel.clamp(-127, 127) as i8;
            let step_hwheel = self.hwheel.clamp(-127, 127) as i8;

            self.dx -= step_x as i32;
            self.dy -= step_y as i32;
            self.wheel -= step_wheel as i32;
            self.hwheel -= step_hwheel as i32;

            self.push_report(
                MouseReport {
                    buttons: self.buttons,
                    x: step_x,
                    y: step_y,
                    wheel: step_wheel,
                    hwheel: step_hwheel,
                },
                configured,
            );
            emitted += 1;
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports
            .pop_front()
            .map(|r| r.to_bytes(self.protocol))
    }
}

#[derive(Debug, Clone)]
struct GamepadInterface {
    idle_rate: u8,
    protocol: HidProtocol,

    buttons: u16,
    hat: u8,
    x: i8,
    y: i8,
    rx: i8,
    ry: i8,

    last_report: [u8; 8],
    pending_reports: VecDeque<[u8; 8]>,
}

impl GamepadInterface {
    fn new() -> Self {
        let initial_report = GamepadReport {
            buttons: 0,
            hat: 8,
            x: 0,
            y: 0,
            rx: 0,
            ry: 0,
        }
        .to_bytes();

        Self {
            idle_rate: 0,
            protocol: HidProtocol::Report,
            buttons: 0,
            hat: 8,
            x: 0,
            y: 0,
            rx: 0,
            ry: 0,
            last_report: initial_report,
            pending_reports: VecDeque::new(),
        }
    }

    fn clear_reports(&mut self) {
        self.pending_reports.clear();
    }

    fn buttons_mask_event(&mut self, button_mask: u16, pressed: bool, configured: bool) {
        let before = self.buttons;
        if pressed {
            self.buttons |= button_mask;
        } else {
            self.buttons &= !button_mask;
        }
        if before != self.buttons {
            self.enqueue_current_report(configured);
        }
    }

    fn button_event(&mut self, button_idx: u8, pressed: bool, configured: bool) {
        if !(1..=16).contains(&button_idx) {
            return;
        }
        let mask = 1u16 << (button_idx - 1);
        self.buttons_mask_event(mask, pressed, configured);
    }

    fn set_buttons(&mut self, buttons: u16, configured: bool) {
        if self.buttons != buttons {
            self.buttons = buttons;
            self.enqueue_current_report(configured);
        }
    }

    fn set_hat(&mut self, hat: Option<u8>, configured: bool) {
        let hat = match hat {
            Some(v) if v <= 7 => v,
            _ => 8,
        };
        if self.hat != hat {
            self.hat = hat;
            self.enqueue_current_report(configured);
        }
    }

    fn set_axes(&mut self, x: i8, y: i8, rx: i8, ry: i8, configured: bool) {
        let x = x.clamp(-127, 127);
        let y = y.clamp(-127, 127);
        let rx = rx.clamp(-127, 127);
        let ry = ry.clamp(-127, 127);

        if self.x != x || self.y != y || self.rx != rx || self.ry != ry {
            self.x = x;
            self.y = y;
            self.rx = rx;
            self.ry = ry;
            self.enqueue_current_report(configured);
        }
    }

    fn set_report(&mut self, report: GamepadReport, configured: bool) {
        let hat = match report.hat {
            v if v <= 7 => v,
            _ => 8,
        };
        let x = report.x.clamp(-127, 127);
        let y = report.y.clamp(-127, 127);
        let rx = report.rx.clamp(-127, 127);
        let ry = report.ry.clamp(-127, 127);

        if self.buttons == report.buttons
            && self.hat == hat
            && self.x == x
            && self.y == y
            && self.rx == rx
            && self.ry == ry
        {
            return;
        }

        self.buttons = report.buttons;
        self.hat = hat;
        self.x = x;
        self.y = y;
        self.rx = rx;
        self.ry = ry;
        self.enqueue_current_report(configured);
    }

    fn current_input_report(&self) -> GamepadReport {
        GamepadReport {
            buttons: self.buttons,
            hat: self.hat,
            x: self.x,
            y: self.y,
            rx: self.rx,
            ry: self.ry,
        }
    }

    fn enqueue_current_report(&mut self, configured: bool) {
        if !configured {
            return;
        }
        let report = self.current_input_report().to_bytes();
        if report != self.last_report {
            self.last_report = report;
            if self.pending_reports.len() >= MAX_PENDING_GAMEPAD_REPORTS {
                self.pending_reports.pop_front();
            }
            self.pending_reports.push_back(report);
        }
    }

    fn poll_interrupt_in(&mut self) -> Option<Vec<u8>> {
        self.pending_reports.pop_front().map(|r| r.to_vec())
    }
}

#[derive(Debug)]
pub struct UsbCompositeHidInput {
    address: u8,
    configuration: u8,
    remote_wakeup_enabled: bool,
    remote_wakeup_pending: bool,
    suspended: bool,
    keyboard: KeyboardInterface,
    mouse: MouseInterface,
    gamepad: GamepadInterface,
    keyboard_interrupt_in_halted: bool,
    mouse_interrupt_in_halted: bool,
    gamepad_interrupt_in_halted: bool,
}

/// Shareable handle for a USB composite HID device (keyboard + mouse + gamepad).
///
/// This consumes a single root hub port while exposing three HID interfaces.
#[derive(Clone, Debug)]
pub struct UsbCompositeHidInputHandle(Rc<RefCell<UsbCompositeHidInput>>);

impl UsbCompositeHidInputHandle {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbCompositeHidInput::new())))
    }

    pub fn configured(&self) -> bool {
        self.0.borrow().configuration != 0
    }

    /// Returns the current HID boot keyboard LED bitmask as last set by the guest OS.
    ///
    /// Bit assignments follow the standard HID LED usages used by the boot keyboard output report:
    /// - bit 0: Num Lock
    /// - bit 1: Caps Lock
    /// - bit 2: Scroll Lock
    /// - bit 3: Compose
    /// - bit 4: Kana
    pub fn keyboard_leds(&self) -> u8 {
        self.0.borrow().keyboard.leds
    }

    pub fn key_event(&self, usage: u8, pressed: bool) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.keyboard.key_event(usage, pressed, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn mouse_button_event(&self, button_bit: u8, pressed: bool) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.mouse.button_event(button_bit, pressed, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn mouse_movement(&self, dx: i32, dy: i32) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.mouse.movement(dx, dy, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn mouse_wheel(&self, delta: i32) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.mouse.wheel(delta, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn mouse_hwheel(&self, delta: i32) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.mouse.hwheel(delta, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    /// Inject vertical + horizontal wheel deltas (AC Pan) in a single mouse report.
    ///
    /// This allows callers to represent diagonal scrolling as one HID frame instead of emitting
    /// separate wheel/hwheel updates.
    pub fn mouse_wheel2(&self, wheel: i32, hwheel: i32) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.mouse.wheel2(wheel, hwheel, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn gamepad_button_event(&self, button_idx: u8, pressed: bool) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad.button_event(button_idx, pressed, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn gamepad_buttons_mask_event(&self, button_mask: u16, pressed: bool) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad
            .buttons_mask_event(button_mask, pressed, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn gamepad_set_buttons(&self, buttons: u16) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad.set_buttons(buttons, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn gamepad_set_hat(&self, hat: Option<u8>) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad.set_hat(hat, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    pub fn gamepad_set_axes(&self, x: i8, y: i8, rx: i8, ry: i8) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad.set_axes(x, y, rx, ry, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }

    /// Updates the entire 8-byte gamepad report state in one call.
    pub fn gamepad_set_report(&self, report: GamepadReport) {
        let mut dev = self.0.borrow_mut();
        let configured = dev.configuration != 0;
        dev.gamepad.set_report(report, configured);
        if dev.suspended && dev.remote_wakeup_enabled && configured {
            dev.remote_wakeup_pending = true;
        }
    }
}

impl Default for UsbCompositeHidInputHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbCompositeHidInputHandle {
    fn reset_host_state_for_restore(&mut self) {
        self.0.borrow_mut().reset_host_state_for_restore();
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        self.0
            .borrow_mut()
            .handle_control_request(setup, data_stage)
    }

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        self.0.borrow_mut().handle_interrupt_in(ep_addr)
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.0.borrow_mut().set_suspended(suspended);
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        self.0.borrow_mut().poll_remote_wakeup()
    }
}

impl Default for UsbCompositeHidInput {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UsbCompositeHidInput {
    const DEVICE_ID: [u8; 4] = *b"UCMP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_REMOTE_WAKEUP_PENDING: u16 = 4;
        const TAG_SUSPENDED: u16 = 5;

        const TAG_KBD_IDLE_RATE: u16 = 10;
        const TAG_KBD_PROTOCOL: u16 = 11;
        const TAG_KBD_LEDS: u16 = 12;
        const TAG_KBD_MODIFIERS: u16 = 13;
        const TAG_KBD_PRESSED_KEYS: u16 = 14;
        const TAG_KBD_LAST_REPORT: u16 = 15;
        const TAG_KBD_PENDING_REPORTS: u16 = 16;

        const TAG_MOUSE_IDLE_RATE: u16 = 20;
        const TAG_MOUSE_PROTOCOL: u16 = 21;
        const TAG_MOUSE_BUTTONS: u16 = 22;
        const TAG_MOUSE_DX: u16 = 23;
        const TAG_MOUSE_DY: u16 = 24;
        const TAG_MOUSE_WHEEL: u16 = 25;
        const TAG_MOUSE_PENDING_REPORTS: u16 = 26;
        const TAG_MOUSE_HWHEEL: u16 = 27;

        const TAG_GAMEPAD_BUTTONS: u16 = 30;
        const TAG_GAMEPAD_HAT: u16 = 31;
        const TAG_GAMEPAD_X: u16 = 32;
        const TAG_GAMEPAD_Y: u16 = 33;
        const TAG_GAMEPAD_RX: u16 = 34;
        const TAG_GAMEPAD_RY: u16 = 35;
        const TAG_GAMEPAD_LAST_REPORT: u16 = 36;
        const TAG_GAMEPAD_PENDING_REPORTS: u16 = 37;

        const TAG_KBD_INTERRUPT_HALTED: u16 = 40;
        const TAG_MOUSE_INTERRUPT_HALTED: u16 = 41;
        const TAG_GAMEPAD_INTERRUPT_HALTED: u16 = 42;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_REMOTE_WAKEUP_PENDING, self.remote_wakeup_pending);
        w.field_bool(TAG_SUSPENDED, self.suspended);

        w.field_u8(TAG_KBD_IDLE_RATE, self.keyboard.idle_rate);
        w.field_u8(TAG_KBD_PROTOCOL, self.keyboard.protocol as u8);
        w.field_u8(TAG_KBD_LEDS, self.keyboard.leds);
        w.field_u8(TAG_KBD_MODIFIERS, self.keyboard.modifiers);
        w.field_bytes(
            TAG_KBD_PRESSED_KEYS,
            Encoder::new().vec_u8(&self.keyboard.pressed_keys).finish(),
        );
        w.field_bytes(TAG_KBD_LAST_REPORT, self.keyboard.last_report.to_vec());
        let pending_kbd: Vec<Vec<u8>> = self
            .keyboard
            .pending_reports
            .iter()
            .map(|r| r.to_vec())
            .collect();
        w.field_bytes(
            TAG_KBD_PENDING_REPORTS,
            Encoder::new().vec_bytes(&pending_kbd).finish(),
        );

        w.field_u8(TAG_MOUSE_IDLE_RATE, self.mouse.idle_rate);
        w.field_u8(TAG_MOUSE_PROTOCOL, self.mouse.protocol as u8);
        w.field_u8(TAG_MOUSE_BUTTONS, self.mouse.buttons);
        w.field_i32(TAG_MOUSE_DX, self.mouse.dx);
        w.field_i32(TAG_MOUSE_DY, self.mouse.dy);
        w.field_i32(TAG_MOUSE_WHEEL, self.mouse.wheel);
        w.field_i32(TAG_MOUSE_HWHEEL, self.mouse.hwheel);
        let pending_mouse: Vec<Vec<u8>> = self
            .mouse
            .pending_reports
            .iter()
            .map(|r| {
                vec![
                    r.buttons,
                    r.x as u8,
                    r.y as u8,
                    r.wheel as u8,
                    r.hwheel as u8,
                ]
            })
            .collect();
        w.field_bytes(
            TAG_MOUSE_PENDING_REPORTS,
            Encoder::new().vec_bytes(&pending_mouse).finish(),
        );

        w.field_u16(TAG_GAMEPAD_BUTTONS, self.gamepad.buttons);
        w.field_u8(TAG_GAMEPAD_HAT, self.gamepad.hat);
        w.field_u8(TAG_GAMEPAD_X, self.gamepad.x as u8);
        w.field_u8(TAG_GAMEPAD_Y, self.gamepad.y as u8);
        w.field_u8(TAG_GAMEPAD_RX, self.gamepad.rx as u8);
        w.field_u8(TAG_GAMEPAD_RY, self.gamepad.ry as u8);
        w.field_bytes(TAG_GAMEPAD_LAST_REPORT, self.gamepad.last_report.to_vec());
        let pending_gp: Vec<Vec<u8>> = self
            .gamepad
            .pending_reports
            .iter()
            .map(|r| r.to_vec())
            .collect();
        w.field_bytes(
            TAG_GAMEPAD_PENDING_REPORTS,
            Encoder::new().vec_bytes(&pending_gp).finish(),
        );

        w.field_bool(TAG_KBD_INTERRUPT_HALTED, self.keyboard_interrupt_in_halted);
        w.field_bool(TAG_MOUSE_INTERRUPT_HALTED, self.mouse_interrupt_in_halted);
        w.field_bool(
            TAG_GAMEPAD_INTERRUPT_HALTED,
            self.gamepad_interrupt_in_halted,
        );

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_CONFIGURATION: u16 = 2;
        const TAG_REMOTE_WAKEUP: u16 = 3;
        const TAG_REMOTE_WAKEUP_PENDING: u16 = 4;
        const TAG_SUSPENDED: u16 = 5;

        const TAG_KBD_IDLE_RATE: u16 = 10;
        const TAG_KBD_PROTOCOL: u16 = 11;
        const TAG_KBD_LEDS: u16 = 12;
        const TAG_KBD_MODIFIERS: u16 = 13;
        const TAG_KBD_PRESSED_KEYS: u16 = 14;
        const TAG_KBD_LAST_REPORT: u16 = 15;
        const TAG_KBD_PENDING_REPORTS: u16 = 16;

        const TAG_MOUSE_IDLE_RATE: u16 = 20;
        const TAG_MOUSE_PROTOCOL: u16 = 21;
        const TAG_MOUSE_BUTTONS: u16 = 22;
        const TAG_MOUSE_DX: u16 = 23;
        const TAG_MOUSE_DY: u16 = 24;
        const TAG_MOUSE_WHEEL: u16 = 25;
        const TAG_MOUSE_PENDING_REPORTS: u16 = 26;
        const TAG_MOUSE_HWHEEL: u16 = 27;

        const TAG_GAMEPAD_BUTTONS: u16 = 30;
        const TAG_GAMEPAD_HAT: u16 = 31;
        const TAG_GAMEPAD_X: u16 = 32;
        const TAG_GAMEPAD_Y: u16 = 33;
        const TAG_GAMEPAD_RX: u16 = 34;
        const TAG_GAMEPAD_RY: u16 = 35;
        const TAG_GAMEPAD_LAST_REPORT: u16 = 36;
        const TAG_GAMEPAD_PENDING_REPORTS: u16 = 37;

        const TAG_KBD_INTERRUPT_HALTED: u16 = 40;
        const TAG_MOUSE_INTERRUPT_HALTED: u16 = 41;
        const TAG_GAMEPAD_INTERRUPT_HALTED: u16 = 42;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        *self = Self::new();

        let address = r.u8(TAG_ADDRESS)?.unwrap_or(0);
        self.address = if address <= 127 { address } else { 0 };
        let configuration = r.u8(TAG_CONFIGURATION)?.unwrap_or(0);
        self.configuration = if configuration == 0 { 0 } else { 1 };
        self.remote_wakeup_enabled = r.bool(TAG_REMOTE_WAKEUP)?.unwrap_or(false);
        self.remote_wakeup_pending = r.bool(TAG_REMOTE_WAKEUP_PENDING)?.unwrap_or(false);
        self.suspended = r.bool(TAG_SUSPENDED)?.unwrap_or(false);

        self.keyboard.idle_rate = r.u8(TAG_KBD_IDLE_RATE)?.unwrap_or(0);
        if let Some(protocol) = r.u8(TAG_KBD_PROTOCOL)? {
            self.keyboard.protocol = match protocol {
                0 => HidProtocol::Boot,
                1 => HidProtocol::Report,
                _ => return Err(SnapshotError::InvalidFieldEncoding("hid protocol")),
            };
        }
        self.keyboard.leds = r.u8(TAG_KBD_LEDS)?.unwrap_or(0) & super::keyboard::KEYBOARD_LED_MASK;
        self.keyboard.modifiers = r.u8(TAG_KBD_MODIFIERS)?.unwrap_or(0);
        if let Some(buf) = r.bytes(TAG_KBD_PRESSED_KEYS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PRESSED_KEYS {
                return Err(SnapshotError::InvalidFieldEncoding("keyboard pressed keys"));
            }
            self.keyboard.pressed_keys = d.bytes_vec(count)?;
            // Snapshot bytes are untrusted; filter out of range usages and dedupe so a corrupt
            // snapshot cannot force the keyboard into ErrorRollOver mode by repeating the same key
            // >6 times.
            let mut seen = [false; 256];
            self.keyboard.pressed_keys.retain(|&k| {
                if k == 0 || k > super::keyboard::KEY_USAGE_MAX {
                    return false;
                }
                let idx = k as usize;
                if seen[idx] {
                    return false;
                }
                seen[idx] = true;
                true
            });
            d.finish()?;
        }
        if let Some(buf) = r.bytes(TAG_KBD_LAST_REPORT) {
            if buf.len() != self.keyboard.last_report.len() {
                return Err(SnapshotError::InvalidFieldEncoding("keyboard last report"));
            }
            self.keyboard.last_report.copy_from_slice(buf);
            self.keyboard.last_report =
                super::keyboard::sanitize_keyboard_report_bytes(self.keyboard.last_report);
        }
        if let Some(buf) = r.bytes(TAG_KBD_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.keyboard.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_KEYBOARD_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "keyboard pending reports",
                ));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if len != self.keyboard.last_report.len() {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "keyboard report length",
                    ));
                }
                let report = d.bytes_vec(len)?;
                self.keyboard.pending_reports.push_back(
                    super::keyboard::sanitize_keyboard_report_bytes(
                        report.try_into().expect("len checked"),
                    ),
                );
            }
            d.finish()?;
        }

        self.mouse.idle_rate = r.u8(TAG_MOUSE_IDLE_RATE)?.unwrap_or(0);
        if let Some(protocol) = r.u8(TAG_MOUSE_PROTOCOL)? {
            self.mouse.protocol = match protocol {
                0 => HidProtocol::Boot,
                1 => HidProtocol::Report,
                _ => return Err(SnapshotError::InvalidFieldEncoding("hid protocol")),
            };
        }
        // Button state is a 5-bit mask (left/right/middle/back/forward). Clamp to avoid carrying
        // arbitrary padding bits from untrusted snapshots into subsequent state transitions.
        self.mouse.buttons = r.u8(TAG_MOUSE_BUTTONS)?.unwrap_or(0) & 0x1f;
        self.mouse.dx = r.i32(TAG_MOUSE_DX)?.unwrap_or(0);
        self.mouse.dy = r.i32(TAG_MOUSE_DY)?.unwrap_or(0);
        self.mouse.wheel = r.i32(TAG_MOUSE_WHEEL)?.unwrap_or(0);
        self.mouse.hwheel = r.i32(TAG_MOUSE_HWHEEL)?.unwrap_or(0);
        if let Some(buf) = r.bytes(TAG_MOUSE_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.mouse.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_MOUSE_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding("mouse pending reports"));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if len != 4 && len != 5 {
                    return Err(SnapshotError::InvalidFieldEncoding("mouse report length"));
                }
                let report = d.bytes(len)?;
                let hwheel = if len == 5 { report[4] as i8 } else { 0 };
                self.mouse.pending_reports.push_back(MouseReport {
                    buttons: report[0] & 0x1f,
                    x: report[1] as i8,
                    y: report[2] as i8,
                    wheel: report[3] as i8,
                    hwheel,
                });
            }
            d.finish()?;
        }

        self.gamepad.buttons = r.u16(TAG_GAMEPAD_BUTTONS)?.unwrap_or(0);
        let hat = r.u8(TAG_GAMEPAD_HAT)?.unwrap_or(8);
        self.gamepad.hat = if hat <= 8 { hat } else { 8 };
        self.gamepad.x = (r.u8(TAG_GAMEPAD_X)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.gamepad.y = (r.u8(TAG_GAMEPAD_Y)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.gamepad.rx = (r.u8(TAG_GAMEPAD_RX)?.unwrap_or(0) as i8).clamp(-127, 127);
        self.gamepad.ry = (r.u8(TAG_GAMEPAD_RY)?.unwrap_or(0) as i8).clamp(-127, 127);
        if let Some(buf) = r.bytes(TAG_GAMEPAD_LAST_REPORT) {
            if buf.len() != self.gamepad.last_report.len() {
                return Err(SnapshotError::InvalidFieldEncoding("gamepad last report"));
            }
            let mut report = [0u8; 8];
            report.copy_from_slice(buf);
            self.gamepad.last_report = super::gamepad::sanitize_gamepad_report_bytes(report);
        }
        if let Some(buf) = r.bytes(TAG_GAMEPAD_PENDING_REPORTS) {
            let mut d = Decoder::new(buf);
            self.gamepad.pending_reports.clear();
            let count = d.u32()? as usize;
            if count > MAX_PENDING_GAMEPAD_REPORTS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "gamepad pending reports",
                ));
            }
            for _ in 0..count {
                let len = d.u32()? as usize;
                if len != self.gamepad.last_report.len() {
                    return Err(SnapshotError::InvalidFieldEncoding("gamepad report length"));
                }
                let report = d.bytes_vec(len)?;
                let report = report.try_into().expect("len checked");
                self.gamepad
                    .pending_reports
                    .push_back(super::gamepad::sanitize_gamepad_report_bytes(report));
            }
            d.finish()?;
        }

        self.keyboard_interrupt_in_halted = r.bool(TAG_KBD_INTERRUPT_HALTED)?.unwrap_or(false);
        self.mouse_interrupt_in_halted = r.bool(TAG_MOUSE_INTERRUPT_HALTED)?.unwrap_or(false);
        self.gamepad_interrupt_in_halted = r.bool(TAG_GAMEPAD_INTERRUPT_HALTED)?.unwrap_or(false);

        Ok(())
    }
}

impl IoSnapshot for UsbCompositeHidInputHandle {
    const DEVICE_ID: [u8; 4] = UsbCompositeHidInput::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = UsbCompositeHidInput::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.0.borrow_mut().load_state(bytes)
    }
}

impl UsbCompositeHidInput {
    pub fn new() -> Self {
        Self {
            address: 0,
            configuration: 0,
            remote_wakeup_enabled: false,
            remote_wakeup_pending: false,
            suspended: false,
            keyboard: KeyboardInterface::new(),
            mouse: MouseInterface::new(),
            gamepad: GamepadInterface::new(),
            keyboard_interrupt_in_halted: false,
            mouse_interrupt_in_halted: false,
            gamepad_interrupt_in_halted: false,
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB Composite HID")),
            _ => None,
        }
    }

    fn hid_descriptor_bytes(report_len: u16) -> [u8; 9] {
        [
            0x09,                    // bLength
            USB_DESCRIPTOR_TYPE_HID, // bDescriptorType
            0x11,
            0x01,                           // bcdHID (1.11)
            0x00,                           // bCountryCode
            0x01,                           // bNumDescriptors
            USB_DESCRIPTOR_TYPE_HID_REPORT, // bDescriptorType (Report)
            (report_len & 0x00ff) as u8,
            (report_len >> 8) as u8,
        ]
    }

    fn report_descriptor_for_interface(interface: u8) -> Option<&'static [u8]> {
        match interface {
            KEYBOARD_INTERFACE => Some(&super::keyboard::HID_REPORT_DESCRIPTOR),
            MOUSE_INTERFACE => Some(&super::mouse::HID_REPORT_DESCRIPTOR),
            GAMEPAD_INTERFACE => Some(&super::gamepad::HID_REPORT_DESCRIPTOR),
            _ => None,
        }
    }

    fn hid_descriptor_for_interface(interface: u8) -> Option<[u8; 9]> {
        Self::report_descriptor_for_interface(interface).map(|report| {
            let len = report.len() as u16;
            Self::hid_descriptor_bytes(len)
        })
    }

    fn clear_reports(&mut self) {
        self.keyboard.clear_reports();
        self.mouse.clear_reports();
        self.gamepad.clear_reports();
    }

    fn interrupt_halted(&self, ep: u8) -> Option<bool> {
        match ep {
            KEYBOARD_INTERRUPT_IN_EP => Some(self.keyboard_interrupt_in_halted),
            MOUSE_INTERRUPT_IN_EP => Some(self.mouse_interrupt_in_halted),
            GAMEPAD_INTERRUPT_IN_EP => Some(self.gamepad_interrupt_in_halted),
            _ => None,
        }
    }

    fn set_interrupt_halted(&mut self, ep: u8, halted: bool) -> bool {
        match ep {
            KEYBOARD_INTERRUPT_IN_EP => {
                self.keyboard_interrupt_in_halted = halted;
                true
            }
            MOUSE_INTERRUPT_IN_EP => {
                self.mouse_interrupt_in_halted = halted;
                true
            }
            GAMEPAD_INTERRUPT_IN_EP => {
                self.gamepad_interrupt_in_halted = halted;
                true
            }
            _ => false,
        }
    }
}

impl UsbDeviceModel for UsbCompositeHidInput {
    fn reset(&mut self) {
        *self = Self::new();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let mut status: u16 = 0;
                    if self.remote_wakeup_enabled {
                        status |= 1 << 1;
                    }
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice
                            || setup.w_index != 0
                            || setup.w_length != 0
                        {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = false;
                        self.remote_wakeup_pending = false;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_FEATURE => match setup.w_value {
                    USB_FEATURE_DEVICE_REMOTE_WAKEUP => {
                        if setup.request_direction() != RequestDirection::HostToDevice
                            || setup.w_index != 0
                            || setup.w_length != 0
                        {
                            return ControlResponse::Stall;
                        }
                        self.remote_wakeup_enabled = true;
                        ControlResponse::Ack
                    }
                    _ => ControlResponse::Stall,
                },
                USB_REQUEST_SET_ADDRESS => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value > 127 {
                        return ControlResponse::Stall;
                    }
                    self.address = (setup.w_value & 0x00ff) as u8;
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let desc_index = setup.descriptor_index();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(DEVICE_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(CONFIG_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    let prev = self.configuration;
                    self.configuration = config;
                    if self.configuration == 0 {
                        self.clear_reports();
                        self.remote_wakeup_pending = false;
                    } else if prev == 0 {
                        // Do not buffer interrupt reports while unconfigured; when becoming
                        // configured, enqueue the current state for any held inputs so they are
                        // visible without requiring a new input transition.
                        self.clear_reports();
                        self.remote_wakeup_pending = false;

                        // Reset report baselines so the current state compares against defaults
                        // rather than the last report from a previous configuration.
                        self.keyboard.last_report = [0; 8];
                        self.gamepad.last_report = GamepadReport {
                            buttons: 0,
                            hat: 8,
                            x: 0,
                            y: 0,
                            rx: 0,
                            ry: 0,
                        }
                        .to_bytes();

                        // Drop any motion accumulated while unconfigured to avoid replaying it
                        // later as a cursor "jump".
                        self.mouse.dx = 0;
                        self.mouse.dy = 0;
                        self.mouse.wheel = 0;
                        self.mouse.hwheel = 0;

                        self.keyboard.enqueue_current_report();
                        let mouse_visible_mask = match self.mouse.protocol {
                            HidProtocol::Boot => 0x07,
                            HidProtocol::Report => 0x1f,
                        };
                        if (self.mouse.buttons & mouse_visible_mask) != 0 {
                            self.mouse.push_report(
                                MouseReport {
                                    buttons: self.mouse.buttons,
                                    x: 0,
                                    y: 0,
                                    wheel: 0,
                                    hwheel: 0,
                                },
                                true,
                            );
                        }
                        self.gamepad.enqueue_current_report(true);
                    }
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Interface) => {
                let interface = (setup.w_index & 0x00ff) as u8;
                match setup.b_request {
                    USB_REQUEST_GET_STATUS => {
                        if setup.request_direction() != RequestDirection::DeviceToHost
                            || setup.w_value != 0
                        {
                            return ControlResponse::Stall;
                        }
                        if !matches!(
                            interface,
                            KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE
                        ) {
                            return ControlResponse::Stall;
                        }
                        ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                    }
                    USB_REQUEST_GET_INTERFACE => {
                        if setup.request_direction() != RequestDirection::DeviceToHost
                            || setup.w_value != 0
                        {
                            return ControlResponse::Stall;
                        }
                        if matches!(
                            interface,
                            KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE
                        ) {
                            ControlResponse::Data(clamp_response(vec![0], setup.w_length))
                        } else {
                            ControlResponse::Stall
                        }
                    }
                    USB_REQUEST_SET_INTERFACE => {
                        if setup.request_direction() != RequestDirection::HostToDevice {
                            return ControlResponse::Stall;
                        }
                        if matches!(
                            interface,
                            KEYBOARD_INTERFACE | MOUSE_INTERFACE | GAMEPAD_INTERFACE
                        ) && setup.w_value == 0
                            && setup.w_length == 0
                        {
                            ControlResponse::Ack
                        } else {
                            ControlResponse::Stall
                        }
                    }
                    USB_REQUEST_GET_DESCRIPTOR => {
                        if setup.request_direction() != RequestDirection::DeviceToHost {
                            return ControlResponse::Stall;
                        }
                        let desc_type = setup.descriptor_type();
                        let data = match desc_type {
                            USB_DESCRIPTOR_TYPE_HID_REPORT => {
                                Self::report_descriptor_for_interface(interface).map(|d| d.to_vec())
                            }
                            USB_DESCRIPTOR_TYPE_HID => {
                                Self::hid_descriptor_for_interface(interface).map(|d| d.to_vec())
                            }
                            _ => None,
                        };
                        data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                            .unwrap_or(ControlResponse::Stall)
                    }
                    _ => ControlResponse::Stall,
                }
            }
            (RequestType::Standard, RequestRecipient::Endpoint) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    let Some(halted) = self.interrupt_halted(ep) else {
                        return ControlResponse::Stall;
                    };
                    let status: u16 = if halted { 1 } else { 0 };
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT
                        && self.set_interrupt_halted(ep, false)
                    {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if setup.w_value == USB_FEATURE_ENDPOINT_HALT
                        && self.set_interrupt_halted(ep, true)
                    {
                        ControlResponse::Ack
                    } else {
                        ControlResponse::Stall
                    }
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Interface) => {
                let interface = (setup.w_index & 0x00ff) as u8;
                match interface {
                    KEYBOARD_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            let report_type = (setup.w_value >> 8) as u8;
                            match report_type {
                                1 => ControlResponse::Data(clamp_response(
                                    self.keyboard.current_input_report().to_bytes().to_vec(),
                                    setup.w_length,
                                )),
                                2 => ControlResponse::Data(clamp_response(
                                    vec![self.keyboard.leds],
                                    setup.w_length,
                                )),
                                _ => ControlResponse::Stall,
                            }
                        }
                        HID_REQUEST_SET_REPORT => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            let report_type = (setup.w_value >> 8) as u8;
                            match (report_type, data_stage) {
                                (2, Some(data)) if !data.is_empty() => {
                                    // HID boot keyboard output report defines 5 LED bits and 3
                                    // constant padding bits; ignore the padding.
                                    self.keyboard.leds =
                                        data[0] & super::keyboard::KEYBOARD_LED_MASK;
                                    ControlResponse::Ack
                                }
                                _ => ControlResponse::Stall,
                            }
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.keyboard.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.keyboard.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.keyboard.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.keyboard.protocol = proto;
                                ControlResponse::Ack
                            } else {
                                ControlResponse::Stall
                            }
                        }
                        _ => ControlResponse::Stall,
                    },
                    MOUSE_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            let report = MouseReport {
                                buttons: self.mouse.buttons,
                                x: 0,
                                y: 0,
                                wheel: 0,
                                hwheel: 0,
                            }
                            .to_bytes(self.mouse.protocol);
                            ControlResponse::Data(clamp_response(report, setup.w_length))
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.mouse.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.mouse.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.mouse.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.mouse.protocol = proto;
                                ControlResponse::Ack
                            } else {
                                ControlResponse::Stall
                            }
                        }
                        _ => ControlResponse::Stall,
                    },
                    GAMEPAD_INTERFACE => match setup.b_request {
                        HID_REQUEST_GET_REPORT => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                self.gamepad.current_input_report().to_bytes().to_vec(),
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_GET_IDLE => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.gamepad.idle_rate],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_IDLE => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            self.gamepad.idle_rate = (setup.w_value >> 8) as u8;
                            ControlResponse::Ack
                        }
                        HID_REQUEST_GET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::DeviceToHost {
                                return ControlResponse::Stall;
                            }
                            ControlResponse::Data(clamp_response(
                                vec![self.gamepad.protocol as u8],
                                setup.w_length,
                            ))
                        }
                        HID_REQUEST_SET_PROTOCOL => {
                            if setup.request_direction() != RequestDirection::HostToDevice {
                                return ControlResponse::Stall;
                            }
                            if let Some(proto) = HidProtocol::from_u16(setup.w_value) {
                                self.gamepad.protocol = proto;
                                ControlResponse::Ack
                            } else {
                                ControlResponse::Stall
                            }
                        }
                        _ => ControlResponse::Stall,
                    },
                    _ => ControlResponse::Stall,
                }
            }
            _ => ControlResponse::Stall,
        }
    }

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        if self.configuration == 0 {
            return UsbInResult::Nak;
        }

        match ep_addr {
            KEYBOARD_INTERRUPT_IN_EP => {
                if self.keyboard_interrupt_in_halted {
                    return UsbInResult::Stall;
                }
                match self.keyboard.poll_interrupt_in() {
                    Some(data) => UsbInResult::Data(data),
                    None => UsbInResult::Nak,
                }
            }
            MOUSE_INTERRUPT_IN_EP => {
                if self.mouse_interrupt_in_halted {
                    return UsbInResult::Stall;
                }
                match self.mouse.poll_interrupt_in() {
                    Some(data) => UsbInResult::Data(data),
                    None => UsbInResult::Nak,
                }
            }
            GAMEPAD_INTERRUPT_IN_EP => {
                if self.gamepad_interrupt_in_halted {
                    return UsbInResult::Stall;
                }
                match self.gamepad.poll_interrupt_in() {
                    Some(data) => UsbInResult::Data(data),
                    None => UsbInResult::Nak,
                }
            }
            _ => UsbInResult::Stall,
        }
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        if self.remote_wakeup_pending
            && self.remote_wakeup_enabled
            && self.configuration != 0
            && self.suspended
        {
            self.remote_wakeup_pending = false;
            true
        } else {
            false
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        self.remote_wakeup_pending = false;
    }
}

fn modifier_bit(usage: u8) -> Option<u8> {
    (0xe0..=0xe7)
        .contains(&usage)
        .then(|| 1u8 << (usage - 0xe0))
}

// USB device descriptor (Composite HID: keyboard + mouse + gamepad).
static DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength
    USB_DESCRIPTOR_TYPE_DEVICE,
    0x00,
    0x02, // bcdUSB (2.00)
    0x00, // bDeviceClass (per interface)
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol
    0x40, // bMaxPacketSize0 (64)
    0x34,
    0x12, // idVendor (0x1234)
    0x04,
    0x00, // idProduct (0x0004)
    0x00,
    0x01, // bcdDevice (1.00)
    0x01, // iManufacturer
    0x02, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations
];

// USB configuration descriptor tree:
//   Config(9) + 3 * (Interface(9) + HID(9) + Endpoint(7)) = 84 bytes
static CONFIG_DESCRIPTOR: [u8; 84] = [
    // Configuration descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_CONFIGURATION,
    84,
    0x00, // wTotalLength
    0x03, // bNumInterfaces
    0x01, // bConfigurationValue
    0x00, // iConfiguration
    0xa0, // bmAttributes (bus powered + remote wake)
    50,   // bMaxPower (100mA)
    // Interface 0: Keyboard
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    KEYBOARD_INTERFACE, // bInterfaceNumber
    0x00,               // bAlternateSetting
    0x01,               // bNumEndpoints
    0x03,               // bInterfaceClass (HID)
    0x01,               // bInterfaceSubClass (Boot)
    0x01,               // bInterfaceProtocol (Keyboard)
    0x00,               // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    super::keyboard::HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    KEYBOARD_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                     // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
    // Interface 1: Mouse
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    MOUSE_INTERFACE, // bInterfaceNumber
    0x00,            // bAlternateSetting
    0x01,            // bNumEndpoints
    0x03,            // bInterfaceClass (HID)
    0x01,            // bInterfaceSubClass (Boot)
    0x02,            // bInterfaceProtocol (Mouse)
    0x00,            // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    super::mouse::HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    MOUSE_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                  // bmAttributes (Interrupt)
    0x05,
    0x00, // wMaxPacketSize (5)
    0x0a, // bInterval (10ms)
    // Interface 2: Gamepad
    0x09, // bLength
    super::USB_DESCRIPTOR_TYPE_INTERFACE,
    GAMEPAD_INTERFACE, // bInterfaceNumber
    0x00,              // bAlternateSetting
    0x01,              // bNumEndpoints
    0x03,              // bInterfaceClass (HID)
    0x00,              // bInterfaceSubClass
    0x00,              // bInterfaceProtocol
    0x00,              // iInterface
    // HID descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_HID,
    0x11,
    0x01, // bcdHID (1.11)
    0x00, // bCountryCode
    0x01, // bNumDescriptors
    USB_DESCRIPTOR_TYPE_HID_REPORT,
    super::gamepad::HID_REPORT_DESCRIPTOR.len() as u8,
    0x00, // wDescriptorLength
    // Endpoint descriptor (Interrupt IN)
    0x07, // bLength
    super::USB_DESCRIPTOR_TYPE_ENDPOINT,
    GAMEPAD_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,                    // bmAttributes (Interrupt)
    0x08,
    0x00, // wMaxPacketSize (8)
    0x0a, // bInterval (10ms)
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn configure(dev: &mut UsbCompositeHidInputHandle) {
        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: USB_REQUEST_SET_CONFIGURATION,
                    w_value: 1,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
    }

    #[test]
    fn config_descriptor_has_three_interfaces_and_endpoints() {
        let mut dev = UsbCompositeHidInput::new();
        let cfg = match dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8,
                w_index: 0,
                w_length: CONFIG_DESCRIPTOR.len() as u16,
            },
            None,
        ) {
            ControlResponse::Data(data) => data,
            other => panic!("expected Data response, got {other:?}"),
        };
        assert_eq!(cfg[0], 0x09);
        assert_eq!(cfg[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(w_le(&cfg, 2) as usize, cfg.len());
        assert_eq!(cfg[4], 3);

        let mut ifaces = 0;
        let mut eps = Vec::new();
        let mut off = 9usize;
        while off + 2 <= cfg.len() {
            let len = cfg[off] as usize;
            let dtype = cfg[off + 1];
            match dtype {
                super::super::USB_DESCRIPTOR_TYPE_INTERFACE => ifaces += 1,
                super::super::USB_DESCRIPTOR_TYPE_ENDPOINT => eps.push(cfg[off + 2]),
                _ => {}
            }
            off += len;
            if len == 0 {
                break;
            }
        }

        assert_eq!(ifaces, 3);
        assert_eq!(eps.len(), 3);
        eps.sort_unstable();
        eps.dedup();
        assert_eq!(eps, vec![0x81, 0x82, 0x83]);
    }

    #[test]
    fn hid_descriptors_reference_correct_report_lengths() {
        let mut dev = UsbCompositeHidInput::new();
        let cfg = match dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: USB_REQUEST_GET_DESCRIPTOR,
                w_value: (USB_DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8,
                w_index: 0,
                w_length: CONFIG_DESCRIPTOR.len() as u16,
            },
            None,
        ) {
            ControlResponse::Data(data) => data,
            other => panic!("expected Data response, got {other:?}"),
        };

        // Layout is deterministic: HID descriptors start after each 9-byte interface descriptor.
        // Interface0 HID at 18, Interface1 HID at 43, Interface2 HID at 68.
        let hid0 = &cfg[18..27];
        let hid1 = &cfg[43..52];
        let hid2 = &cfg[68..77];

        assert_eq!(hid0[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(
            w_le(hid0, 7) as usize,
            super::super::keyboard::HID_REPORT_DESCRIPTOR.len()
        );

        assert_eq!(hid1[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(
            w_le(hid1, 7) as usize,
            super::super::mouse::HID_REPORT_DESCRIPTOR.len()
        );

        assert_eq!(hid2[1], USB_DESCRIPTOR_TYPE_HID);
        assert_eq!(
            w_le(hid2, 7) as usize,
            super::super::gamepad::HID_REPORT_DESCRIPTOR.len()
        );
    }

    #[test]
    fn get_descriptor_report_dispatches_by_interface_number() {
        let mut dev = UsbCompositeHidInput::new();

        for (iface, expected) in [
            (
                KEYBOARD_INTERFACE,
                &super::super::keyboard::HID_REPORT_DESCRIPTOR[..],
            ),
            (
                MOUSE_INTERFACE,
                &super::super::mouse::HID_REPORT_DESCRIPTOR[..],
            ),
            (
                GAMEPAD_INTERFACE,
                &super::super::gamepad::HID_REPORT_DESCRIPTOR[..],
            ),
        ] {
            let resp = dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x81,
                    b_request: USB_REQUEST_GET_DESCRIPTOR,
                    w_value: (USB_DESCRIPTOR_TYPE_HID_REPORT as u16) << 8,
                    w_index: iface as u16,
                    w_length: expected.len() as u16,
                },
                None,
            );
            assert_eq!(resp, ControlResponse::Data(expected.to_vec()));
        }
    }

    #[test]
    fn configuration_enqueues_held_keyboard_key_state() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.key_event(0x04, true);

        assert_eq!(
            dev.handle_in_transfer(KEYBOARD_INTERRUPT_IN_EP, 8),
            UsbInResult::Nak
        );

        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(KEYBOARD_INTERRUPT_IN_EP, 8),
            UsbInResult::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0])
        );
    }

    #[test]
    fn configuration_does_not_replay_transient_keyboard_keypress() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.key_event(0x04, true);
        dev.key_event(0x04, false);

        assert_eq!(
            dev.handle_in_transfer(KEYBOARD_INTERRUPT_IN_EP, 8),
            UsbInResult::Nak
        );

        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(KEYBOARD_INTERRUPT_IN_EP, 8),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configuration_does_not_replay_unconfigured_mouse_motion() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.mouse_movement(10, 0);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );

        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configuration_does_not_replay_unconfigured_mouse_wheel2() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.mouse_wheel2(5, 7);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );

        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configuration_enqueues_held_mouse_button_state() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.mouse_button_event(0x01, true);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );

        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x01, 0x00, 0x00, 0x00, 0x00])
        );
    }

    #[test]
    fn configuration_does_not_enqueue_invalid_mouse_button_state() {
        let mut dev = UsbCompositeHidInputHandle::new();
        dev.mouse_button_event(0x20, true);

        configure(&mut dev);
        assert_eq!(dev.0.borrow().mouse.buttons, 0);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configured_mouse_motion_saturates_without_overflow() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        // Simulate a corrupt snapshot that restores a huge accumulated delta, then ensure further
        // host-side motion injection can't overflow and panic in debug builds.
        dev.0.borrow_mut().mouse.dx = i32::MAX;
        dev.mouse_movement(1, 0);

        assert_eq!(
            dev.0.borrow().mouse.pending_reports.len(),
            MAX_PENDING_MOUSE_REPORTS
        );

        let mut first = None;
        let mut last = None;
        let mut count = 0usize;
        loop {
            match dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5) {
                UsbInResult::Data(data) => {
                    if first.is_none() {
                        first = Some(data.clone());
                    }
                    last = Some(data);
                    count += 1;
                }
                UsbInResult::Nak => break,
                UsbInResult::Stall => panic!("unexpected STALL on mouse interrupt IN"),
                UsbInResult::Timeout => panic!("unexpected TIMEOUT on mouse interrupt IN"),
            }
        }

        assert_eq!(count, MAX_PENDING_MOUSE_REPORTS);
        assert_eq!(first.unwrap(), vec![0x00, 127u8, 0, 0, 0]);
        assert_eq!(last.unwrap(), vec![0x00, 127u8, 0, 0, 0]);
    }

    #[test]
    fn configured_mouse_wheel2_emits_single_report_with_both_axes() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        dev.mouse_wheel2(5, 7);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 5, 7])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configured_mouse_wheel2_splits_large_deltas_into_multiple_reports() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        dev.mouse_wheel2(300, -300);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 127u8, (-127i8) as u8])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 127u8, (-127i8) as u8])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 46u8, (-46i8) as u8])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configured_mouse_wheel_and_hwheel_emit_two_separate_reports() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        dev.mouse_wheel(1);
        dev.mouse_hwheel(2);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 1, 0])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Data(vec![0x00, 0x00, 0x00, 0, 2])
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn boot_protocol_mouse_wheel_is_ignored() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_PROTOCOL,
                    w_value: 0, // boot protocol
                    w_index: MOUSE_INTERFACE as u16,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        dev.mouse_wheel2(5, 7);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn boot_protocol_mouse_side_buttons_are_ignored() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_PROTOCOL,
                    w_value: 0, // boot protocol
                    w_index: MOUSE_INTERFACE as u16,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        dev.mouse_button_event(0x08, true);
        dev.mouse_button_event(0x08, false);
        dev.mouse_button_event(0x10, true);
        dev.mouse_button_event(0x10, false);

        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn boot_protocol_mouse_side_button_still_triggers_remote_wakeup() {
        let mut dev = UsbCompositeHidInputHandle::new();
        configure(&mut dev);

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00, // HostToDevice | Standard | Device
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: USB_FEATURE_DEVICE_REMOTE_WAKEUP,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        dev.set_suspended(true);

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_PROTOCOL,
                    w_value: 0, // boot protocol
                    w_index: MOUSE_INTERFACE as u16,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        dev.mouse_button_event(0x08, true);
        assert!(dev.poll_remote_wakeup());
        assert!(
            !dev.poll_remote_wakeup(),
            "remote wakeup should be edge-triggered"
        );
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn configuration_does_not_enqueue_held_mouse_side_button_in_boot_protocol() {
        let mut dev = UsbCompositeHidInputHandle::new();

        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x21, // HostToDevice | Class | Interface
                    b_request: HID_REQUEST_SET_PROTOCOL,
                    w_value: 0, // boot protocol
                    w_index: MOUSE_INTERFACE as u16,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );

        dev.mouse_button_event(0x08, true);
        configure(&mut dev);
        assert_eq!(
            dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
            UsbInResult::Nak
        );
    }

    #[test]
    fn snapshot_restore_rejects_oversized_pressed_keys() {
        const TAG_KBD_PRESSED_KEYS: u16 = 14;

        let snapshot = {
            let mut w = SnapshotWriter::new(
                UsbCompositeHidInput::DEVICE_ID,
                UsbCompositeHidInput::DEVICE_VERSION,
            );
            w.field_bytes(
                TAG_KBD_PRESSED_KEYS,
                Encoder::new().u32(MAX_PRESSED_KEYS as u32 + 1).finish(),
            );
            w.finish()
        };

        let mut dev = UsbCompositeHidInput::new();
        match dev.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("keyboard pressed keys")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_restore_rejects_oversized_keyboard_pending_reports_count() {
        const TAG_KBD_PENDING_REPORTS: u16 = 16;

        let snapshot = {
            let mut w = SnapshotWriter::new(
                UsbCompositeHidInput::DEVICE_ID,
                UsbCompositeHidInput::DEVICE_VERSION,
            );
            w.field_bytes(
                TAG_KBD_PENDING_REPORTS,
                Encoder::new()
                    .u32(MAX_PENDING_KEYBOARD_REPORTS as u32 + 1)
                    .finish(),
            );
            w.finish()
        };

        let mut dev = UsbCompositeHidInput::new();
        match dev.load_state(&snapshot) {
            Err(SnapshotError::InvalidFieldEncoding("keyboard pending reports")) => {}
            other => panic!("expected InvalidFieldEncoding, got {other:?}"),
        }
    }
}
