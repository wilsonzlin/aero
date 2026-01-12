use aero_devices_input::Ps2MouseButton;

use crate::io::input::i8042::SharedI8042Controller;
use crate::io::usb::hid::composite::UsbCompositeHidInputHandle;
use crate::io::usb::hid::gamepad::{GamepadReport, UsbHidGamepadHandle};
use crate::io::usb::hid::hid_usage_from_js_code;
use crate::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use crate::io::usb::hid::mouse::UsbHidMouseHandle;
use crate::io::virtio::devices::input as vio_input;
use crate::io::virtio::devices::input::VirtioInputHub;
use crate::io::virtio::vio_core::VirtQueueError;
use memory::GuestMemory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRoutingPolicy {
    Ps2Only,
    VirtioOnly,
    UsbOnly,
    Auto,
}

pub struct InputPipeline {
    pub ps2: Option<SharedI8042Controller>,
    pub virtio: Option<VirtioInputHub>,
    pub usb_keyboard: Option<UsbHidKeyboardHandle>,
    pub usb_mouse: Option<UsbHidMouseHandle>,
    pub usb_gamepad: Option<UsbHidGamepadHandle>,
    pub usb_composite: Option<UsbCompositeHidInputHandle>,
    pub policy: InputRoutingPolicy,
}

impl std::fmt::Debug for InputPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputPipeline")
            .field("ps2", &self.ps2.is_some())
            .field("virtio", &self.virtio)
            .field("usb_keyboard", &self.usb_keyboard)
            .field("usb_mouse", &self.usb_mouse)
            .field("usb_gamepad", &self.usb_gamepad)
            .field("usb_composite", &self.usb_composite)
            .field("policy", &self.policy)
            .finish()
    }
}

impl InputPipeline {
    pub fn new(
        ps2: Option<SharedI8042Controller>,
        virtio: Option<VirtioInputHub>,
        policy: InputRoutingPolicy,
    ) -> Self {
        Self {
            ps2,
            virtio,
            usb_keyboard: None,
            usb_mouse: None,
            usb_gamepad: None,
            usb_composite: None,
            policy,
        }
    }

    pub fn with_usb_hid(
        mut self,
        keyboard: UsbHidKeyboardHandle,
        mouse: UsbHidMouseHandle,
    ) -> Self {
        self.usb_keyboard = Some(keyboard);
        self.usb_mouse = Some(mouse);
        self
    }

    pub fn with_usb_composite_hid(mut self, composite: UsbCompositeHidInputHandle) -> Self {
        self.usb_composite = Some(composite);
        self
    }

    pub fn with_usb_gamepad(mut self, gamepad: UsbHidGamepadHandle) -> Self {
        self.usb_gamepad = Some(gamepad);
        self
    }

    pub fn handle_key(
        &mut self,
        mem: &mut impl GuestMemory,
        code: &str,
        pressed: bool,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::Ps2Only => self.inject_key_ps2(code, pressed),
            InputRoutingPolicy::VirtioOnly => self.inject_key_virtio(mem, code, pressed)?,
            InputRoutingPolicy::UsbOnly => self.inject_key_usb(code, pressed),
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.keyboard.driver_ok()) {
                    self.inject_key_virtio(mem, code, pressed)?
                } else if self.usb_composite.as_ref().is_some_and(|d| d.configured())
                    || self
                        .usb_keyboard
                        .as_ref()
                        .is_some_and(|kbd| kbd.configured())
                {
                    self.inject_key_usb(code, pressed)
                } else {
                    self.inject_key_ps2(code, pressed)
                }
            }
        }
        Ok(())
    }

    pub fn handle_mouse_move(
        &mut self,
        mem: &mut impl GuestMemory,
        dx: i32,
        dy: i32,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::Ps2Only => self.inject_mouse_move_ps2(dx, dy),
            InputRoutingPolicy::VirtioOnly => self.inject_mouse_move_virtio(mem, dx, dy)?,
            InputRoutingPolicy::UsbOnly => self.inject_mouse_move_usb(dx, dy),
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_move_virtio(mem, dx, dy)?
                } else if self.usb_composite.as_ref().is_some_and(|d| d.configured())
                    || self
                        .usb_mouse
                        .as_ref()
                        .is_some_and(|mouse| mouse.configured())
                {
                    self.inject_mouse_move_usb(dx, dy)
                } else {
                    self.inject_mouse_move_ps2(dx, dy)
                }
            }
        }
        Ok(())
    }

    pub fn handle_mouse_button(
        &mut self,
        mem: &mut impl GuestMemory,
        button: Ps2MouseButton,
        pressed: bool,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::Ps2Only => self.inject_mouse_button_ps2(button, pressed),
            InputRoutingPolicy::VirtioOnly => {
                self.inject_mouse_button_virtio(mem, button, pressed)?
            }
            InputRoutingPolicy::UsbOnly => self.inject_mouse_button_usb(button, pressed),
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_button_virtio(mem, button, pressed)?
                } else if self.usb_composite.as_ref().is_some_and(|d| d.configured())
                    || self
                        .usb_mouse
                        .as_ref()
                        .is_some_and(|mouse| mouse.configured())
                {
                    self.inject_mouse_button_usb(button, pressed)
                } else {
                    self.inject_mouse_button_ps2(button, pressed)
                }
            }
        }
        Ok(())
    }

    pub fn handle_mouse_wheel(
        &mut self,
        mem: &mut impl GuestMemory,
        delta: i32,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::Ps2Only => self.inject_mouse_wheel_ps2(delta),
            InputRoutingPolicy::VirtioOnly => self.inject_mouse_wheel_virtio(mem, delta)?,
            InputRoutingPolicy::UsbOnly => self.inject_mouse_wheel_usb(delta),
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_wheel_virtio(mem, delta)?
                } else if self.usb_composite.as_ref().is_some_and(|d| d.configured())
                    || self
                        .usb_mouse
                        .as_ref()
                        .is_some_and(|mouse| mouse.configured())
                {
                    self.inject_mouse_wheel_usb(delta)
                } else {
                    self.inject_mouse_wheel_ps2(delta)
                }
            }
        }
        Ok(())
    }

    pub fn handle_gamepad_buttons(&mut self, buttons: u16) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::UsbOnly | InputRoutingPolicy::Auto => {
                self.inject_gamepad_buttons_usb(buttons);
            }
            InputRoutingPolicy::Ps2Only | InputRoutingPolicy::VirtioOnly => {}
        }
        Ok(())
    }

    pub fn handle_gamepad_button(
        &mut self,
        button_idx: u8,
        pressed: bool,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::UsbOnly | InputRoutingPolicy::Auto => {
                self.inject_gamepad_button_usb(button_idx, pressed);
            }
            InputRoutingPolicy::Ps2Only | InputRoutingPolicy::VirtioOnly => {}
        }
        Ok(())
    }

    pub fn handle_gamepad_hat(&mut self, hat: Option<u8>) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::UsbOnly | InputRoutingPolicy::Auto => {
                self.inject_gamepad_hat_usb(hat);
            }
            InputRoutingPolicy::Ps2Only | InputRoutingPolicy::VirtioOnly => {}
        }
        Ok(())
    }

    pub fn handle_gamepad_axes(
        &mut self,
        x: i8,
        y: i8,
        rx: i8,
        ry: i8,
    ) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::UsbOnly | InputRoutingPolicy::Auto => {
                self.inject_gamepad_axes_usb(x, y, rx, ry);
            }
            InputRoutingPolicy::Ps2Only | InputRoutingPolicy::VirtioOnly => {}
        }
        Ok(())
    }

    /// Updates the entire gamepad state (8-byte input report) in a single call.
    ///
    /// This is primarily intended for host-side polling loops (e.g. browser Gamepad API),
    /// where the full state is refreshed at a fixed rate and should enqueue at most one report
    /// per poll.
    pub fn handle_gamepad_report(&mut self, report: GamepadReport) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::UsbOnly | InputRoutingPolicy::Auto => {
                self.inject_gamepad_report_usb(report);
            }
            InputRoutingPolicy::Ps2Only | InputRoutingPolicy::VirtioOnly => {}
        }
        Ok(())
    }

    fn inject_key_ps2(&mut self, code: &str, pressed: bool) {
        let Some(i8042) = self.ps2.as_ref() else {
            return;
        };
        i8042.borrow_mut().inject_browser_key(code, pressed);
    }

    fn inject_key_virtio(
        &mut self,
        mem: &mut impl GuestMemory,
        code: &str,
        pressed: bool,
    ) -> Result<(), VirtQueueError> {
        let Some(virtio) = self.virtio.as_mut() else {
            return Ok(());
        };
        let Some(key_code) = js_code_to_linux_key(code) else {
            return Ok(());
        };
        virtio.keyboard.inject_key(mem, key_code, pressed)?;
        Ok(())
    }

    fn inject_key_usb(&mut self, code: &str, pressed: bool) {
        let Some(usage) = hid_usage_from_js_code(code) else {
            return;
        };
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.key_event(usage, pressed);
            return;
        }
        let Some(kbd) = self.usb_keyboard.as_ref().filter(|k| k.configured()) else {
            return;
        };
        kbd.key_event(usage, pressed);
    }

    fn inject_mouse_move_ps2(&mut self, dx: i32, dy: i32) {
        let Some(i8042) = self.ps2.as_ref() else {
            return;
        };
        i8042.borrow_mut().inject_mouse_motion(dx, dy, 0);
    }

    fn inject_mouse_move_virtio(
        &mut self,
        mem: &mut impl GuestMemory,
        dx: i32,
        dy: i32,
    ) -> Result<(), VirtQueueError> {
        let Some(virtio) = self.virtio.as_mut() else {
            return Ok(());
        };
        virtio.mouse.inject_rel_move(mem, dx, dy)?;
        Ok(())
    }

    fn inject_mouse_move_usb(&mut self, dx: i32, dy: i32) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.mouse_movement(dx, dy);
            return;
        }
        let Some(mouse) = self.usb_mouse.as_ref().filter(|m| m.configured()) else {
            return;
        };
        mouse.movement(dx, dy);
    }

    fn inject_mouse_button_ps2(&mut self, button: Ps2MouseButton, pressed: bool) {
        let Some(i8042) = self.ps2.as_ref() else {
            return;
        };
        i8042.borrow_mut().inject_mouse_button(button, pressed);
    }

    fn inject_mouse_button_virtio(
        &mut self,
        mem: &mut impl GuestMemory,
        button: Ps2MouseButton,
        pressed: bool,
    ) -> Result<(), VirtQueueError> {
        let Some(virtio) = self.virtio.as_mut() else {
            return Ok(());
        };
        let code = match button {
            Ps2MouseButton::Left => vio_input::BTN_LEFT,
            Ps2MouseButton::Right => vio_input::BTN_RIGHT,
            Ps2MouseButton::Middle => vio_input::BTN_MIDDLE,
        };
        virtio.mouse.inject_button(mem, code, pressed)?;
        Ok(())
    }

    fn inject_mouse_button_usb(&mut self, button: Ps2MouseButton, pressed: bool) {
        let bit = match button {
            Ps2MouseButton::Left => 0x01,
            Ps2MouseButton::Right => 0x02,
            Ps2MouseButton::Middle => 0x04,
        };
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.mouse_button_event(bit, pressed);
            return;
        }
        let Some(mouse) = self.usb_mouse.as_ref().filter(|m| m.configured()) else {
            return;
        };
        mouse.button_event(bit, pressed);
    }

    fn inject_mouse_wheel_ps2(&mut self, delta: i32) {
        let Some(i8042) = self.ps2.as_ref() else {
            return;
        };
        i8042.borrow_mut().inject_mouse_motion(0, 0, delta);
    }

    fn inject_mouse_wheel_virtio(
        &mut self,
        mem: &mut impl GuestMemory,
        delta: i32,
    ) -> Result<(), VirtQueueError> {
        let Some(virtio) = self.virtio.as_mut() else {
            return Ok(());
        };
        virtio.mouse.inject_wheel(mem, delta)?;
        Ok(())
    }

    fn inject_mouse_wheel_usb(&mut self, delta: i32) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.mouse_wheel(delta);
            return;
        }
        let Some(mouse) = self.usb_mouse.as_ref().filter(|m| m.configured()) else {
            return;
        };
        mouse.wheel(delta);
    }

    fn inject_gamepad_buttons_usb(&mut self, buttons: u16) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.gamepad_set_buttons(buttons);
            return;
        }
        let Some(pad) = self.usb_gamepad.as_ref().filter(|d| d.configured()) else {
            return;
        };
        pad.set_buttons(buttons);
    }

    fn inject_gamepad_button_usb(&mut self, button_idx: u8, pressed: bool) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.gamepad_button_event(button_idx, pressed);
            return;
        }
        let Some(pad) = self.usb_gamepad.as_ref().filter(|d| d.configured()) else {
            return;
        };
        pad.button_event(button_idx, pressed);
    }

    fn inject_gamepad_hat_usb(&mut self, hat: Option<u8>) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.gamepad_set_hat(hat);
            return;
        }
        let Some(pad) = self.usb_gamepad.as_ref().filter(|d| d.configured()) else {
            return;
        };
        pad.set_hat(hat);
    }

    fn inject_gamepad_axes_usb(&mut self, x: i8, y: i8, rx: i8, ry: i8) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.gamepad_set_axes(x, y, rx, ry);
            return;
        }
        let Some(pad) = self.usb_gamepad.as_ref().filter(|d| d.configured()) else {
            return;
        };
        pad.set_axes(x, y, rx, ry);
    }

    fn inject_gamepad_report_usb(&mut self, report: GamepadReport) {
        if let Some(dev) = self.usb_composite.as_ref().filter(|d| d.configured()) {
            dev.gamepad_set_report(report);
            return;
        }
        let Some(pad) = self.usb_gamepad.as_ref().filter(|d| d.configured()) else {
            return;
        };
        pad.set_report(report);
    }
}

fn js_code_to_linux_key(code: &str) -> Option<u16> {
    match code {
        "Escape" => Some(vio_input::KEY_ESC),
        "Digit1" => Some(vio_input::KEY_1),
        "Digit2" => Some(vio_input::KEY_2),
        "Digit3" => Some(vio_input::KEY_3),
        "Digit4" => Some(vio_input::KEY_4),
        "Digit5" => Some(vio_input::KEY_5),
        "Digit6" => Some(vio_input::KEY_6),
        "Digit7" => Some(vio_input::KEY_7),
        "Digit8" => Some(vio_input::KEY_8),
        "Digit9" => Some(vio_input::KEY_9),
        "Digit0" => Some(vio_input::KEY_0),
        "Minus" => Some(vio_input::KEY_MINUS),
        "Equal" => Some(vio_input::KEY_EQUAL),
        "Backspace" => Some(vio_input::KEY_BACKSPACE),
        "Tab" => Some(vio_input::KEY_TAB),
        "KeyQ" => Some(vio_input::KEY_Q),
        "KeyW" => Some(vio_input::KEY_W),
        "KeyE" => Some(vio_input::KEY_E),
        "KeyR" => Some(vio_input::KEY_R),
        "KeyT" => Some(vio_input::KEY_T),
        "KeyY" => Some(vio_input::KEY_Y),
        "KeyU" => Some(vio_input::KEY_U),
        "KeyI" => Some(vio_input::KEY_I),
        "KeyO" => Some(vio_input::KEY_O),
        "KeyP" => Some(vio_input::KEY_P),
        "BracketLeft" => Some(vio_input::KEY_LEFTBRACE),
        "BracketRight" => Some(vio_input::KEY_RIGHTBRACE),
        "Enter" => Some(vio_input::KEY_ENTER),
        "ControlLeft" => Some(vio_input::KEY_LEFTCTRL),
        "KeyA" => Some(vio_input::KEY_A),
        "KeyS" => Some(vio_input::KEY_S),
        "KeyD" => Some(vio_input::KEY_D),
        "KeyF" => Some(vio_input::KEY_F),
        "KeyG" => Some(vio_input::KEY_G),
        "KeyH" => Some(vio_input::KEY_H),
        "KeyJ" => Some(vio_input::KEY_J),
        "KeyK" => Some(vio_input::KEY_K),
        "KeyL" => Some(vio_input::KEY_L),
        "Semicolon" => Some(vio_input::KEY_SEMICOLON),
        "Quote" => Some(vio_input::KEY_APOSTROPHE),
        "Backquote" => Some(vio_input::KEY_GRAVE),
        "ShiftLeft" => Some(vio_input::KEY_LEFTSHIFT),
        "Backslash" => Some(vio_input::KEY_BACKSLASH),
        "KeyZ" => Some(vio_input::KEY_Z),
        "KeyX" => Some(vio_input::KEY_X),
        "KeyC" => Some(vio_input::KEY_C),
        "KeyV" => Some(vio_input::KEY_V),
        "KeyB" => Some(vio_input::KEY_B),
        "KeyN" => Some(vio_input::KEY_N),
        "KeyM" => Some(vio_input::KEY_M),
        "Comma" => Some(vio_input::KEY_COMMA),
        "Period" => Some(vio_input::KEY_DOT),
        "Slash" => Some(vio_input::KEY_SLASH),
        "ShiftRight" => Some(vio_input::KEY_RIGHTSHIFT),
        "AltLeft" => Some(vio_input::KEY_LEFTALT),
        "Space" => Some(vio_input::KEY_SPACE),
        "CapsLock" => Some(vio_input::KEY_CAPSLOCK),
        "F1" => Some(vio_input::KEY_F1),
        "F2" => Some(vio_input::KEY_F2),
        "F3" => Some(vio_input::KEY_F3),
        "F4" => Some(vio_input::KEY_F4),
        "F5" => Some(vio_input::KEY_F5),
        "F6" => Some(vio_input::KEY_F6),
        "F7" => Some(vio_input::KEY_F7),
        "F8" => Some(vio_input::KEY_F8),
        "F9" => Some(vio_input::KEY_F9),
        "F10" => Some(vio_input::KEY_F10),
        "F11" => Some(vio_input::KEY_F11),
        "F12" => Some(vio_input::KEY_F12),
        "NumLock" => Some(vio_input::KEY_NUMLOCK),
        "ScrollLock" => Some(vio_input::KEY_SCROLLLOCK),
        "ControlRight" => Some(vio_input::KEY_RIGHTCTRL),
        "AltRight" => Some(vio_input::KEY_RIGHTALT),
        "MetaLeft" | "OSLeft" => Some(vio_input::KEY_LEFTMETA),
        "MetaRight" | "OSRight" => Some(vio_input::KEY_RIGHTMETA),
        "Home" => Some(vio_input::KEY_HOME),
        "PageUp" => Some(vio_input::KEY_PAGEUP),
        "ArrowUp" => Some(vio_input::KEY_UP),
        "ArrowLeft" => Some(vio_input::KEY_LEFT),
        "ArrowRight" => Some(vio_input::KEY_RIGHT),
        "End" => Some(vio_input::KEY_END),
        "ArrowDown" => Some(vio_input::KEY_DOWN),
        "PageDown" => Some(vio_input::KEY_PAGEDOWN),
        "Insert" => Some(vio_input::KEY_INSERT),
        "Delete" => Some(vio_input::KEY_DELETE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::PortIO;
    use crate::io::usb::core::UsbInResult;
    use crate::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};
    use crate::io::virtio::devices::input::{
        VirtioInputDevice, VirtioInputDeviceKind, VirtioInputEvent, EV_KEY,
    };
    use crate::io::virtio::vio_core::{Descriptor, VirtQueue, VRING_DESC_F_WRITE};
    use memory::DenseMemory;

    fn write_desc(mem: &mut DenseMemory, base: u64, index: u16, desc: Descriptor) {
        let off = base + (index as u64) * 16;
        mem.write_u64_le(off, desc.addr).unwrap();
        mem.write_u32_le(off + 8, desc.len).unwrap();
        mem.write_u16_le(off + 12, desc.flags).unwrap();
        mem.write_u16_le(off + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, avail: u64, heads: &[u16]) {
        mem.write_u16_le(avail, 0).unwrap();
        mem.write_u16_le(avail + 2, heads.len() as u16).unwrap();
        for (i, head) in heads.iter().enumerate() {
            mem.write_u16_le(avail + 4 + (i as u64) * 2, *head).unwrap();
        }
    }

    fn init_used(mem: &mut DenseMemory, used: u64) {
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();
    }

    fn drain_i8042_output(ps2: &SharedI8042Controller) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let status = ps2.port_read(0x64, 1) as u8;
            if status & 0x01 == 0 {
                break;
            }
            out.push(ps2.port_read(0x60, 1) as u8);
        }
        out
    }

    fn configure_usb_device(dev: &mut impl UsbDeviceModel) {
        assert_eq!(
            dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: 0x09, // SET_CONFIGURATION
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
    fn auto_routing_uses_ps2_until_virtio_driver_ok() {
        let mut mem = DenseMemory::new(0x8000).unwrap();
        let ps2 = crate::io::input::i8042::new_shared_controller();

        let desc_base = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf0 = 0x0100;
        let buf1 = 0x0200;

        write_desc(
            &mut mem,
            desc_base,
            0,
            Descriptor {
                addr: buf0,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_base,
            1,
            Descriptor {
                addr: buf1,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, &[0, 1]);
        init_used(&mut mem, used);

        let keyboard = VirtioInputDevice::new(
            VirtioInputDeviceKind::Keyboard,
            VirtQueue::new(8, desc_base, avail, used),
            VirtQueue::new(8, 0, 0, 0),
        );
        let mouse = VirtioInputDevice::new(
            VirtioInputDeviceKind::Mouse,
            VirtQueue::new(8, 0, 0, 0),
            VirtQueue::new(8, 0, 0, 0),
        );
        let virtio = VirtioInputHub::new(keyboard, mouse);

        let mut pipeline =
            InputPipeline::new(Some(ps2.clone()), Some(virtio), InputRoutingPolicy::Auto);
        pipeline.handle_key(&mut mem, "KeyA", true).unwrap();

        let ps2 = pipeline.ps2.as_ref().unwrap();
        assert_eq!(drain_i8042_output(ps2), vec![0x1E]);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 0);

        pipeline
            .virtio
            .as_mut()
            .unwrap()
            .keyboard
            .set_status(crate::io::virtio::devices::input::VIRTIO_STATUS_DRIVER_OK);

        pipeline.handle_key(&mut mem, "KeyA", true).unwrap();

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 2);

        let mut bytes = [0u8; VirtioInputEvent::BYTE_SIZE];
        mem.read_into(buf0, &mut bytes).unwrap();
        let ev = VirtioInputEvent::from_bytes_le(bytes);
        assert_eq!(ev.typ, EV_KEY);
    }

    #[test]
    fn ps2_routing_emits_extended_and_sequence_scancodes() {
        let mut mem = DenseMemory::new(0x8000).unwrap();
        let ps2 = crate::io::input::i8042::new_shared_controller();

        let mut pipeline = InputPipeline::new(Some(ps2.clone()), None, InputRoutingPolicy::Ps2Only);

        // Extended key: ArrowUp is translated to Set-1 bytes (E0 48 / E0 C8).
        pipeline.handle_key(&mut mem, "ArrowUp", true).unwrap();
        pipeline.handle_key(&mut mem, "ArrowUp", false).unwrap();

        let bytes = drain_i8042_output(pipeline.ps2.as_ref().unwrap());
        assert_eq!(bytes, vec![0xE0, 0x48, 0xE0, 0xC8]);

        // Sequence keys: PrintScreen and Pause have multi-byte make/break.
        pipeline.handle_key(&mut mem, "PrintScreen", true).unwrap();
        pipeline.handle_key(&mut mem, "PrintScreen", false).unwrap();
        pipeline.handle_key(&mut mem, "Pause", true).unwrap();
        pipeline.handle_key(&mut mem, "Pause", false).unwrap();

        let bytes = drain_i8042_output(pipeline.ps2.as_ref().unwrap());

        assert_eq!(
            bytes,
            vec![
                // PrintScreen make/break.
                0xE0, 0x2A, 0xE0, 0x37, 0xE0, 0xB7, 0xE0, 0xAA,
                // Pause make only (Set-2 -> Set-1 translation enabled by default).
                0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5,
            ]
        );
    }

    #[test]
    fn usb_composite_gamepad_injection_emits_reports() {
        let mut composite = UsbCompositeHidInputHandle::new();
        configure_usb_device(&mut composite);

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::UsbOnly)
            .with_usb_composite_hid(composite.clone());

        pipeline.handle_gamepad_buttons(0x0001).unwrap();
        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_button(2, true).unwrap();
        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_hat(Some(2)).unwrap();
        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_axes(1, -1, 5, -5).unwrap();
        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x01, 0xff, 0x05, 0xfb, 0x00])
        );

        assert_eq!(composite.handle_in_transfer(0x83, 8), UsbInResult::Nak);
    }

    #[test]
    fn usb_gamepad_injection_emits_reports() {
        let mut gamepad = UsbHidGamepadHandle::new();
        configure_usb_device(&mut gamepad);

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::Auto)
            .with_usb_gamepad(gamepad.clone());

        pipeline.handle_gamepad_buttons(0x0001).unwrap();
        assert_eq!(
            gamepad.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_button(2, true).unwrap();
        assert_eq!(
            gamepad.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_hat(Some(2)).unwrap();
        assert_eq!(
            gamepad.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00])
        );

        pipeline.handle_gamepad_axes(1, -1, 5, -5).unwrap();
        assert_eq!(
            gamepad.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x01, 0xff, 0x05, 0xfb, 0x00])
        );

        assert_eq!(gamepad.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }

    #[test]
    fn usb_composite_gamepad_report_injection_emits_single_report() {
        let mut composite = UsbCompositeHidInputHandle::new();
        configure_usb_device(&mut composite);

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::UsbOnly)
            .with_usb_composite_hid(composite.clone());

        let report = GamepadReport {
            buttons: 0x0003,
            hat: 2,
            x: 1,
            y: -1,
            rx: 5,
            ry: -5,
        };

        pipeline.handle_gamepad_report(report).unwrap();
        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x01, 0xff, 0x05, 0xfb, 0x00])
        );

        // Re-sending an identical report should not enqueue another update.
        pipeline.handle_gamepad_report(report).unwrap();
        assert_eq!(composite.handle_in_transfer(0x83, 8), UsbInResult::Nak);
    }

    #[test]
    fn usb_gamepad_report_injection_emits_single_report() {
        let mut gamepad = UsbHidGamepadHandle::new();
        configure_usb_device(&mut gamepad);

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::UsbOnly)
            .with_usb_gamepad(gamepad.clone());

        let report = GamepadReport {
            buttons: 0x0003,
            hat: 2,
            x: 1,
            y: -1,
            rx: 5,
            ry: -5,
        };

        pipeline.handle_gamepad_report(report).unwrap();
        assert_eq!(
            gamepad.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x03, 0x00, 0x02, 0x01, 0xff, 0x05, 0xfb, 0x00])
        );

        pipeline.handle_gamepad_report(report).unwrap();
        assert_eq!(gamepad.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }

    #[test]
    fn gamepad_injection_is_ignored_until_configured() {
        let mut composite = UsbCompositeHidInputHandle::new();
        let mut gamepad = UsbHidGamepadHandle::new();

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::UsbOnly)
            .with_usb_composite_hid(composite.clone())
            .with_usb_gamepad(gamepad.clone());

        pipeline.handle_gamepad_buttons(0x0001).unwrap();

        configure_usb_device(&mut composite);
        configure_usb_device(&mut gamepad);

        assert_eq!(composite.handle_in_transfer(0x83, 8), UsbInResult::Nak);
        assert_eq!(gamepad.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }

    #[test]
    fn usb_gamepad_routing_prefers_composite_device() {
        let mut composite = UsbCompositeHidInputHandle::new();
        let mut gamepad = UsbHidGamepadHandle::new();
        configure_usb_device(&mut composite);
        configure_usb_device(&mut gamepad);

        let mut pipeline = InputPipeline::new(None, None, InputRoutingPolicy::UsbOnly)
            .with_usb_composite_hid(composite.clone())
            .with_usb_gamepad(gamepad.clone());

        pipeline.handle_gamepad_buttons(0x0001).unwrap();

        assert_eq!(
            composite.handle_in_transfer(0x83, 8),
            UsbInResult::Data(vec![0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        );
        assert_eq!(gamepad.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }
}
