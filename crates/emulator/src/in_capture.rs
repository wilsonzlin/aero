use crate::io::ps2::{Ps2Controller, Ps2MouseButton};
use crate::io::virtio::devices::input::{
    VirtioInputHub, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KEY_A, KEY_B, KEY_ENTER, KEY_ESC, KEY_SPACE, KEY_TAB,
};
use crate::io::virtio::vio_core::VirtQueueError;
use memory::GuestMemory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRoutingPolicy {
    Ps2Only,
    VirtioOnly,
    Auto,
}

#[derive(Debug)]
pub struct InputPipeline {
    pub ps2: Option<Ps2Controller>,
    pub virtio: Option<VirtioInputHub>,
    pub policy: InputRoutingPolicy,
}

impl InputPipeline {
    pub fn new(ps2: Option<Ps2Controller>, virtio: Option<VirtioInputHub>, policy: InputRoutingPolicy) -> Self {
        Self { ps2, virtio, policy }
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
            InputRoutingPolicy::Auto => {
                if self
                    .virtio
                    .as_ref()
                    .is_some_and(|v| v.keyboard.driver_ok())
                {
                    self.inject_key_virtio(mem, code, pressed)?
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
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_move_virtio(mem, dx, dy)?
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
            InputRoutingPolicy::VirtioOnly => self.inject_mouse_button_virtio(mem, button, pressed)?,
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_button_virtio(mem, button, pressed)?
                } else {
                    self.inject_mouse_button_ps2(button, pressed)
                }
            }
        }
        Ok(())
    }

    pub fn handle_mouse_wheel(&mut self, mem: &mut impl GuestMemory, delta: i32) -> Result<(), VirtQueueError> {
        match self.policy {
            InputRoutingPolicy::Ps2Only => self.inject_mouse_wheel_ps2(delta),
            InputRoutingPolicy::VirtioOnly => self.inject_mouse_wheel_virtio(mem, delta)?,
            InputRoutingPolicy::Auto => {
                if self.virtio.as_ref().is_some_and(|v| v.mouse.driver_ok()) {
                    self.inject_mouse_wheel_virtio(mem, delta)?
                } else {
                    self.inject_mouse_wheel_ps2(delta)
                }
            }
        }
        Ok(())
    }

    fn inject_key_ps2(&mut self, code: &str, pressed: bool) {
        let Some(ps2) = self.ps2.as_mut() else {
            return;
        };
        let Some(scancode) = js_code_to_ps2_set2_scancode(code) else {
            return;
        };
        ps2.keyboard.inject_scancode_set2(scancode, pressed);
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

    fn inject_mouse_move_ps2(&mut self, dx: i32, dy: i32) {
        let Some(ps2) = self.ps2.as_mut() else {
            return;
        };
        ps2.mouse.inject_move(dx, dy);
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

    fn inject_mouse_button_ps2(&mut self, button: Ps2MouseButton, pressed: bool) {
        let Some(ps2) = self.ps2.as_mut() else {
            return;
        };
        ps2.mouse.inject_button(button, pressed);
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
            Ps2MouseButton::Left => BTN_LEFT,
            Ps2MouseButton::Right => BTN_RIGHT,
            Ps2MouseButton::Middle => BTN_MIDDLE,
        };
        virtio.mouse.inject_button(mem, code, pressed)?;
        Ok(())
    }

    fn inject_mouse_wheel_ps2(&mut self, delta: i32) {
        let Some(ps2) = self.ps2.as_mut() else {
            return;
        };
        ps2.mouse.inject_wheel(delta);
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
}

fn js_code_to_linux_key(code: &str) -> Option<u16> {
    match code {
        "Escape" => Some(KEY_ESC),
        "Tab" => Some(KEY_TAB),
        "Enter" => Some(KEY_ENTER),
        "Space" => Some(KEY_SPACE),
        "KeyA" => Some(KEY_A),
        "KeyB" => Some(KEY_B),
        _ => None,
    }
}

fn js_code_to_ps2_set2_scancode(code: &str) -> Option<u8> {
    match code {
        "Escape" => Some(0x76),
        "Tab" => Some(0x0D),
        "Enter" => Some(0x5A),
        "Space" => Some(0x29),
        "KeyA" => Some(0x1C),
        "KeyB" => Some(0x32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::virtio::devices::input::{VirtioInputDevice, VirtioInputDeviceKind, VirtioInputEvent, EV_KEY};
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
            mem.write_u16_le(avail + 4 + (i as u64) * 2, *head)
                .unwrap();
        }
    }

    fn init_used(mem: &mut DenseMemory, used: u64) {
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();
    }

    #[test]
    fn auto_routing_uses_ps2_until_virtio_driver_ok() {
        let mut mem = DenseMemory::new(0x8000).unwrap();
        let ps2 = Ps2Controller::default();

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

        let mut pipeline = InputPipeline::new(Some(ps2), Some(virtio), InputRoutingPolicy::Auto);
        pipeline.handle_key(&mut mem, "KeyA", true).unwrap();

        let ps2 = pipeline.ps2.as_ref().unwrap();
        assert_eq!(ps2.keyboard.scancodes.len(), 1);
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
}
