use crate::io::virtio::vio_core::{Descriptor, VirtQueue, VirtQueueError, VRING_DESC_F_WRITE};
use memory::{GuestMemory, GuestMemoryError};
use std::collections::VecDeque;

pub const VIRTIO_ID_INPUT: u16 = 18;

pub const VIRTIO_STATUS_DRIVER_OK: u8 = 0x04;

pub const VIRTIO_INPUT_CFG_UNSET: u8 = 0x00;
pub const VIRTIO_INPUT_CFG_ID_NAME: u8 = 0x01;
pub const VIRTIO_INPUT_CFG_ID_SERIAL: u8 = 0x02;
pub const VIRTIO_INPUT_CFG_ID_DEVIDS: u8 = 0x03;
pub const VIRTIO_INPUT_CFG_PROP_BITS: u8 = 0x10;
pub const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;
pub const VIRTIO_INPUT_CFG_ABS_INFO: u8 = 0x12;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_LED: u16 = 0x11;

pub const SYN_REPORT: u16 = 0x00;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;

pub const LED_NUML: u16 = 0x00;
pub const LED_CAPSL: u16 = 0x01;
pub const LED_SCROLLL: u16 = 0x02;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_F1: u16 = 59;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_NUMLOCK: u16 = 69;
pub const KEY_SCROLLLOCK: u16 = 70;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_UP: u16 = 103;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct VirtioInputEvent {
    pub typ: u16,
    pub code: u16,
    pub value: i32,
}

impl VirtioInputEvent {
    pub const BYTE_SIZE: usize = 8;

    pub fn to_bytes_le(self) -> [u8; Self::BYTE_SIZE] {
        let mut out = [0u8; Self::BYTE_SIZE];
        out[0..2].copy_from_slice(&self.typ.to_le_bytes());
        out[2..4].copy_from_slice(&self.code.to_le_bytes());
        out[4..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }

    pub fn from_bytes_le(bytes: [u8; Self::BYTE_SIZE]) -> Self {
        let typ = u16::from_le_bytes([bytes[0], bytes[1]]);
        let code = u16::from_le_bytes([bytes[2], bytes[3]]);
        let value = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        Self { typ, code, value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioInputDeviceKind {
    Keyboard,
    Mouse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioInputDevids {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

impl VirtioInputDevids {
    pub fn to_bytes_le(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&self.bustype.to_le_bytes());
        out[2..4].copy_from_slice(&self.vendor.to_le_bytes());
        out[4..6].copy_from_slice(&self.product.to_le_bytes());
        out[6..8].copy_from_slice(&self.version.to_le_bytes());
        out
    }
}

#[derive(Debug, Clone)]
struct VirtioInputBitmaps {
    ev: [u8; 128],
    key: [u8; 128],
    rel: [u8; 128],
    led: [u8; 128],
}

impl VirtioInputBitmaps {
    fn empty() -> Self {
        Self {
            ev: [0u8; 128],
            key: [0u8; 128],
            rel: [0u8; 128],
            led: [0u8; 128],
        }
    }

    fn set(bitmap: &mut [u8; 128], bit: u16) {
        let bit = bit as usize;
        if bit / 8 >= bitmap.len() {
            return;
        }
        bitmap[bit / 8] |= 1u8 << (bit % 8);
    }

    fn with_bits(bits: &[u16]) -> [u8; 128] {
        let mut bitmap = [0u8; 128];
        for &bit in bits {
            Self::set(&mut bitmap, bit);
        }
        bitmap
    }

    fn for_keyboard() -> Self {
        let mut bitmaps = Self::empty();
        bitmaps.ev = Self::with_bits(&[EV_SYN, EV_KEY, EV_LED]);
        bitmaps.key = Self::with_bits(&[
            KEY_ESC,
            KEY_1,
            KEY_2,
            KEY_3,
            KEY_4,
            KEY_5,
            KEY_6,
            KEY_7,
            KEY_8,
            KEY_9,
            KEY_0,
            KEY_MINUS,
            KEY_EQUAL,
            KEY_BACKSPACE,
            KEY_TAB,
            KEY_Q,
            KEY_W,
            KEY_E,
            KEY_R,
            KEY_T,
            KEY_Y,
            KEY_U,
            KEY_I,
            KEY_O,
            KEY_P,
            KEY_LEFTBRACE,
            KEY_RIGHTBRACE,
            KEY_ENTER,
            KEY_LEFTCTRL,
            KEY_A,
            KEY_S,
            KEY_D,
            KEY_F,
            KEY_G,
            KEY_H,
            KEY_J,
            KEY_K,
            KEY_L,
            KEY_SEMICOLON,
            KEY_APOSTROPHE,
            KEY_GRAVE,
            KEY_LEFTSHIFT,
            KEY_BACKSLASH,
            KEY_Z,
            KEY_X,
            KEY_C,
            KEY_V,
            KEY_B,
            KEY_N,
            KEY_M,
            KEY_COMMA,
            KEY_DOT,
            KEY_SLASH,
            KEY_RIGHTSHIFT,
            KEY_LEFTALT,
            KEY_RIGHTALT,
            KEY_SPACE,
            KEY_CAPSLOCK,
            KEY_F1,
            KEY_F2,
            KEY_F3,
            KEY_F4,
            KEY_F5,
            KEY_F6,
            KEY_F7,
            KEY_F8,
            KEY_F9,
            KEY_F10,
            KEY_F11,
            KEY_F12,
            KEY_NUMLOCK,
            KEY_SCROLLLOCK,
            KEY_RIGHTCTRL,
            KEY_LEFTMETA,
            KEY_RIGHTMETA,
            KEY_HOME,
            KEY_UP,
            KEY_PAGEUP,
            KEY_DOWN,
            KEY_LEFT,
            KEY_RIGHT,
            KEY_END,
            KEY_PAGEDOWN,
            KEY_INSERT,
            KEY_DELETE,
        ]);
        bitmaps.led = Self::with_bits(&[LED_NUML, LED_CAPSL, LED_SCROLLL]);
        bitmaps
    }

    fn for_mouse() -> Self {
        let mut bitmaps = Self::empty();
        bitmaps.ev = Self::with_bits(&[EV_SYN, EV_KEY, EV_REL]);
        bitmaps.key = Self::with_bits(&[BTN_LEFT, BTN_RIGHT, BTN_MIDDLE]);
        bitmaps.rel = Self::with_bits(&[REL_X, REL_Y, REL_WHEEL]);
        bitmaps
    }
}

#[derive(Debug)]
pub struct VirtioInputDevice {
    kind: VirtioInputDeviceKind,
    name: String,
    serial: String,
    devids: VirtioInputDevids,
    status: u8,
    config_select: u8,
    config_subsel: u8,
    pub event_vq: VirtQueue,
    pub status_vq: VirtQueue,
    pending_events: VecDeque<VirtioInputEvent>,
    led_state: u8,
    bitmaps: VirtioInputBitmaps,
    isr_queue: bool,
}

impl VirtioInputDevice {
    pub fn new(kind: VirtioInputDeviceKind, event_vq: VirtQueue, status_vq: VirtQueue) -> Self {
        let (name, bitmaps) = match kind {
            VirtioInputDeviceKind::Keyboard => (
                "Aero Virtio Keyboard".to_string(),
                VirtioInputBitmaps::for_keyboard(),
            ),
            VirtioInputDeviceKind::Mouse => (
                "Aero Virtio Mouse".to_string(),
                VirtioInputBitmaps::for_mouse(),
            ),
        };
        Self {
            kind,
            name,
            serial: "0".to_string(),
            devids: VirtioInputDevids {
                bustype: 0x06,
                vendor: 0x1AF4,
                product: match kind {
                    VirtioInputDeviceKind::Keyboard => 0x0001,
                    VirtioInputDeviceKind::Mouse => 0x0002,
                },
                version: 0x0001,
            },
            status: 0,
            config_select: VIRTIO_INPUT_CFG_UNSET,
            config_subsel: 0,
            event_vq,
            status_vq,
            pending_events: VecDeque::new(),
            led_state: 0,
            bitmaps,
            isr_queue: false,
        }
    }

    pub fn kind(&self) -> VirtioInputDeviceKind {
        self.kind
    }

    pub fn driver_ok(&self) -> bool {
        self.status & VIRTIO_STATUS_DRIVER_OK != 0
    }

    pub fn set_status(&mut self, status: u8) {
        self.status = status;
    }

    pub fn take_isr(&mut self) -> u8 {
        let isr = if self.isr_queue { 0x1 } else { 0x0 };
        self.isr_queue = false;
        isr
    }

    pub fn led_state(&self) -> u8 {
        self.led_state
    }

    pub fn notify_event(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        self.process_pending_events(mem)
    }

    pub fn notify_status(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        self.process_status_queue(mem)
    }

    pub fn inject_key(
        &mut self,
        mem: &mut impl GuestMemory,
        code: u16,
        pressed: bool,
    ) -> Result<bool, VirtQueueError> {
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_KEY,
            code,
            value: if pressed { 1 } else { 0 },
        });
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        self.process_pending_events(mem)
    }

    pub fn inject_rel_move(
        &mut self,
        mem: &mut impl GuestMemory,
        dx: i32,
        dy: i32,
    ) -> Result<bool, VirtQueueError> {
        if dx != 0 {
            self.pending_events.push_back(VirtioInputEvent {
                typ: EV_REL,
                code: REL_X,
                value: dx,
            });
        }
        if dy != 0 {
            self.pending_events.push_back(VirtioInputEvent {
                typ: EV_REL,
                code: REL_Y,
                value: dy,
            });
        }
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        self.process_pending_events(mem)
    }

    pub fn inject_wheel(
        &mut self,
        mem: &mut impl GuestMemory,
        delta: i32,
    ) -> Result<bool, VirtQueueError> {
        if delta == 0 {
            return Ok(false);
        }
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_REL,
            code: REL_WHEEL,
            value: delta,
        });
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        self.process_pending_events(mem)
    }

    pub fn inject_button(
        &mut self,
        mem: &mut impl GuestMemory,
        code: u16,
        pressed: bool,
    ) -> Result<bool, VirtQueueError> {
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_KEY,
            code,
            value: if pressed { 1 } else { 0 },
        });
        self.pending_events.push_back(VirtioInputEvent {
            typ: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        self.process_pending_events(mem)
    }

    fn process_pending_events(
        &mut self,
        mem: &mut impl GuestMemory,
    ) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(event) = self.pending_events.pop_front() {
            let chain = match self.event_vq.pop_available(mem)? {
                Some(chain) => chain,
                None => {
                    self.pending_events.push_front(event);
                    break;
                }
            };

            let bytes = event.to_bytes_le();
            let written = match write_chain(mem, &chain.descriptors, 0, &bytes) {
                Ok(written) => written,
                Err(VirtQueueError::GuestMemory(_)) => 0,
                Err(other) => return Err(other),
            };

            if self.event_vq.push_used(mem, &chain, written as u32)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    fn process_status_queue(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.status_vq.pop_available(mem)? {
            let mut bytes = [0u8; VirtioInputEvent::BYTE_SIZE];
            match read_chain_exact(mem, &chain.descriptors, 0, &mut bytes) {
                Ok(()) => self.handle_status_event(VirtioInputEvent::from_bytes_le(bytes)),
                Err(VirtQueueError::DescriptorChainTooShort { .. }) => {}
                Err(VirtQueueError::GuestMemory(_)) => {}
                Err(other) => return Err(other),
            }

            if self.status_vq.push_used(mem, &chain, 0)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    fn handle_status_event(&mut self, event: VirtioInputEvent) {
        if self.kind != VirtioInputDeviceKind::Keyboard {
            return;
        }
        if event.typ != EV_LED {
            return;
        }
        let bit = match event.code {
            LED_NUML => 1u8 << 0,
            LED_CAPSL => 1u8 << 1,
            LED_SCROLLL => 1u8 << 2,
            _ => return,
        };
        if event.value != 0 {
            self.led_state |= bit;
        } else {
            self.led_state &= !bit;
        }
    }

    pub fn write_config(&mut self, offset: usize, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            match offset.saturating_add(i) {
                0 => self.config_select = byte,
                1 => self.config_subsel = byte,
                _ => {}
            }
        }
    }

    pub fn read_config(&self, offset: usize, len: usize) -> Vec<u8> {
        let bytes = self.config_bytes();
        let offset = offset.min(bytes.len());
        let end = offset.saturating_add(len).min(bytes.len());
        bytes[offset..end].to_vec()
    }

    fn config_bytes(&self) -> Vec<u8> {
        let mut cfg = vec![0u8; 8 + 128];
        cfg[0] = self.config_select;
        cfg[1] = self.config_subsel;
        let (size, payload) = self.config_payload();
        cfg[2] = size;
        cfg[8..8 + 128].copy_from_slice(&payload);
        cfg
    }

    fn config_payload(&self) -> (u8, [u8; 128]) {
        let mut payload = [0u8; 128];
        match self.config_select {
            VIRTIO_INPUT_CFG_ID_NAME => {
                let bytes = self.name.as_bytes();
                let len = (bytes.len() + 1).min(payload.len());
                payload[..len - 1].copy_from_slice(&bytes[..len - 1]);
                payload[len - 1] = 0;
                (len as u8, payload)
            }
            VIRTIO_INPUT_CFG_ID_SERIAL => {
                let bytes = self.serial.as_bytes();
                let len = (bytes.len() + 1).min(payload.len());
                payload[..len - 1].copy_from_slice(&bytes[..len - 1]);
                payload[len - 1] = 0;
                (len as u8, payload)
            }
            VIRTIO_INPUT_CFG_ID_DEVIDS => {
                payload[..8].copy_from_slice(&self.devids.to_bytes_le());
                (8, payload)
            }
            VIRTIO_INPUT_CFG_PROP_BITS => (0, payload),
            VIRTIO_INPUT_CFG_EV_BITS => {
                let bitmap = match self.config_subsel {
                    0 => &self.bitmaps.ev,
                    x if x == EV_KEY as u8 => &self.bitmaps.key,
                    x if x == EV_REL as u8 => &self.bitmaps.rel,
                    x if x == EV_LED as u8 => &self.bitmaps.led,
                    _ => &[0u8; 128],
                };
                payload.copy_from_slice(bitmap);
                (payload.len() as u8, payload)
            }
            _ => (0, payload),
        }
    }
}

#[derive(Debug)]
pub struct VirtioInputHub {
    pub keyboard: VirtioInputDevice,
    pub mouse: VirtioInputDevice,
}

impl VirtioInputHub {
    pub fn new(keyboard: VirtioInputDevice, mouse: VirtioInputDevice) -> Self {
        Self { keyboard, mouse }
    }
}

fn read_chain_exact(
    mem: &impl GuestMemory,
    descs: &[Descriptor],
    mut offset: usize,
    out: &mut [u8],
) -> Result<(), VirtQueueError> {
    let mut written = 0usize;

    for desc in descs {
        let desc_len = desc.len as usize;

        if offset >= desc_len {
            offset -= desc_len;
            continue;
        }

        let available = desc_len - offset;
        let to_read = usize::min(available, out.len() - written);
        let addr = desc.addr.checked_add(offset as u64).ok_or_else(|| {
            VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                paddr: desc.addr,
                len: to_read,
                size: mem.size(),
            })
        })?;
        mem.read_into(addr, &mut out[written..written + to_read])?;
        written += to_read;
        offset = 0;

        if written == out.len() {
            return Ok(());
        }
    }

    Err(VirtQueueError::DescriptorChainTooShort {
        requested: out.len(),
    })
}

fn write_chain(
    mem: &mut impl GuestMemory,
    descs: &[Descriptor],
    mut offset: usize,
    data: &[u8],
) -> Result<usize, VirtQueueError> {
    let mut remaining = data;
    let mut written = 0usize;

    for desc in descs {
        if desc.flags & VRING_DESC_F_WRITE == 0 && offset == 0 {
            break;
        }

        let desc_len = desc.len as usize;
        if offset >= desc_len {
            offset -= desc_len;
            continue;
        }

        if desc.flags & VRING_DESC_F_WRITE == 0 {
            break;
        }

        let available = desc_len - offset;
        let to_write = usize::min(available, remaining.len());
        let addr = desc.addr.checked_add(offset as u64).ok_or_else(|| {
            VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                paddr: desc.addr,
                len: to_write,
                size: mem.size(),
            })
        })?;
        mem.write_from(addr, &remaining[..to_write])?;
        written += to_write;
        remaining = &remaining[to_write..];
        offset = 0;

        if remaining.is_empty() {
            break;
        }
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::virtio::vio_core::VRING_AVAIL_F_NO_INTERRUPT;
    use memory::DenseMemory;

    fn write_desc(mem: &mut DenseMemory, base: u64, index: u16, desc: Descriptor) {
        let off = base + (index as u64) * 16;
        mem.write_u64_le(off, desc.addr).unwrap();
        mem.write_u32_le(off + 8, desc.len).unwrap();
        mem.write_u16_le(off + 12, desc.flags).unwrap();
        mem.write_u16_le(off + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, avail: u64, flags: u16, heads: &[u16]) {
        mem.write_u16_le(avail, flags).unwrap();
        mem.write_u16_le(avail + 2, heads.len() as u16).unwrap();
        for (i, head) in heads.iter().enumerate() {
            mem.write_u16_le(avail + 4 + (i as u64) * 2, *head).unwrap();
        }
    }

    fn init_used(mem: &mut DenseMemory, used: u64) {
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();
    }

    #[test]
    fn keyboard_event_injection_completes_descriptors() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let event_desc = 0x1000;
        let event_avail = 0x2000;
        let event_used = 0x3000;

        let status_desc = 0x4000;
        let status_avail = 0x5000;
        let status_used = 0x6000;

        let buf0 = 0x0100;
        let buf1 = 0x0200;

        write_desc(
            &mut mem,
            event_desc,
            0,
            Descriptor {
                addr: buf0,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, event_avail, 0, &[0]);
        init_used(&mut mem, event_used);

        init_avail(&mut mem, status_avail, VRING_AVAIL_F_NO_INTERRUPT, &[]);
        init_used(&mut mem, status_used);

        let event_vq = VirtQueue::new(8, event_desc, event_avail, event_used);
        let status_vq = VirtQueue::new(8, status_desc, status_avail, status_used);

        let mut dev = VirtioInputDevice::new(VirtioInputDeviceKind::Keyboard, event_vq, status_vq);
        dev.set_status(VIRTIO_STATUS_DRIVER_OK);

        let irq = dev.inject_key(&mut mem, KEY_A, true).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let used_idx = mem.read_u16_le(event_used + 2).unwrap();
        assert_eq!(used_idx, 1);

        let used_id = mem.read_u32_le(event_used + 4).unwrap();
        let used_len = mem.read_u32_le(event_used + 8).unwrap();
        assert_eq!(used_id, 0);
        assert_eq!(used_len, VirtioInputEvent::BYTE_SIZE as u32);

        let mut ev_bytes = [0u8; VirtioInputEvent::BYTE_SIZE];
        mem.read_into(buf0, &mut ev_bytes).unwrap();
        let ev = VirtioInputEvent::from_bytes_le(ev_bytes);
        assert_eq!(ev.typ, EV_KEY);
        assert_eq!(ev.code, KEY_A);
        assert_eq!(ev.value, 1);

        assert_eq!(dev.pending_events.len(), 1);

        write_desc(
            &mut mem,
            event_desc,
            1,
            Descriptor {
                addr: buf1,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        mem.write_u16_le(event_avail + 2, 2).unwrap();
        mem.write_u16_le(event_avail + 4 + 2, 1).unwrap();

        let irq = dev.notify_event(&mut mem).unwrap();
        assert!(irq);

        let used_idx = mem.read_u16_le(event_used + 2).unwrap();
        assert_eq!(used_idx, 2);

        let used_id = mem.read_u32_le(event_used + 12).unwrap();
        let used_len = mem.read_u32_le(event_used + 16).unwrap();
        assert_eq!(used_id, 1);
        assert_eq!(used_len, VirtioInputEvent::BYTE_SIZE as u32);

        mem.read_into(buf1, &mut ev_bytes).unwrap();
        let ev = VirtioInputEvent::from_bytes_le(ev_bytes);
        assert_eq!(ev.typ, EV_SYN);
        assert_eq!(ev.code, SYN_REPORT);
        assert_eq!(ev.value, 0);
    }

    #[test]
    fn status_queue_led_updates_are_applied() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let event_vq = VirtQueue::new(8, 0, 0, 0);

        let status_desc = 0x1000;
        let status_avail = 0x2000;
        let status_used = 0x3000;
        let status_buf = 0x0400;

        let event = VirtioInputEvent {
            typ: EV_LED,
            code: LED_CAPSL,
            value: 1,
        };
        mem.write_from(status_buf, &event.to_bytes_le()).unwrap();

        write_desc(
            &mut mem,
            status_desc,
            0,
            Descriptor {
                addr: status_buf,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: 0,
                next: 0,
            },
        );

        init_avail(&mut mem, status_avail, 0, &[0]);
        init_used(&mut mem, status_used);

        let status_vq = VirtQueue::new(8, status_desc, status_avail, status_used);

        let mut dev = VirtioInputDevice::new(VirtioInputDeviceKind::Keyboard, event_vq, status_vq);
        dev.notify_status(&mut mem).unwrap();

        assert_eq!(dev.led_state() & (1u8 << 1), 1u8 << 1);

        let used_idx = mem.read_u16_le(status_used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }

    #[test]
    fn config_queries_expose_name_and_event_bits() {
        let event_vq = VirtQueue::new(8, 0, 0, 0);
        let status_vq = VirtQueue::new(8, 0, 0, 0);
        let mut dev = VirtioInputDevice::new(VirtioInputDeviceKind::Keyboard, event_vq, status_vq);

        dev.write_config(0, &[VIRTIO_INPUT_CFG_ID_NAME]);
        let name_len = dev.read_config(2, 1)[0] as usize;
        let name_payload = dev.read_config(8, name_len);
        assert!(name_payload.starts_with(b"Aero Virtio Keyboard"));

        dev.write_config(0, &[VIRTIO_INPUT_CFG_EV_BITS, 0]);
        let ev_bitmap = dev.read_config(8, 128);
        assert_ne!(ev_bitmap[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)), 0);
        assert_ne!(ev_bitmap[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)), 0);
        assert_ne!(ev_bitmap[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8)), 0);

        dev.write_config(1, &[EV_KEY as u8]);
        let key_bitmap = dev.read_config(8, 128);
        assert_ne!(key_bitmap[(KEY_A / 8) as usize] & (1u8 << (KEY_A % 8)), 0);
        // Contract-required keys for Win7 virtio-input support.
        assert_ne!(key_bitmap[(KEY_F1 / 8) as usize] & (1u8 << (KEY_F1 % 8)), 0);
        assert_ne!(
            key_bitmap[(KEY_F12 / 8) as usize] & (1u8 << (KEY_F12 % 8)),
            0
        );
        assert_ne!(
            key_bitmap[(KEY_NUMLOCK / 8) as usize] & (1u8 << (KEY_NUMLOCK % 8)),
            0
        );
        assert_ne!(
            key_bitmap[(KEY_SCROLLLOCK / 8) as usize] & (1u8 << (KEY_SCROLLLOCK % 8)),
            0
        );

        dev.write_config(1, &[EV_LED as u8]);
        let led_bitmap = dev.read_config(8, 128);
        assert_ne!(
            led_bitmap[(LED_NUML / 8) as usize] & (1u8 << (LED_NUML % 8)),
            0
        );
        assert_ne!(
            led_bitmap[(LED_CAPSL / 8) as usize] & (1u8 << (LED_CAPSL % 8)),
            0
        );
        assert_ne!(
            led_bitmap[(LED_SCROLLL / 8) as usize] & (1u8 << (LED_SCROLLL % 8)),
            0
        );

        // Mouse variant: event types include SYN/KEY/REL and excludes LED.
        let event_vq = VirtQueue::new(8, 0, 0, 0);
        let status_vq = VirtQueue::new(8, 0, 0, 0);
        let mut mouse = VirtioInputDevice::new(VirtioInputDeviceKind::Mouse, event_vq, status_vq);

        mouse.write_config(0, &[VIRTIO_INPUT_CFG_ID_NAME]);
        let name_len = mouse.read_config(2, 1)[0] as usize;
        let name_payload = mouse.read_config(8, name_len);
        assert!(name_payload.starts_with(b"Aero Virtio Mouse"));

        mouse.write_config(0, &[VIRTIO_INPUT_CFG_EV_BITS, 0]);
        let ev_bitmap = mouse.read_config(8, 128);
        assert_ne!(ev_bitmap[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)), 0);
        assert_ne!(ev_bitmap[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)), 0);
        assert_ne!(ev_bitmap[(EV_REL / 8) as usize] & (1u8 << (EV_REL % 8)), 0);
        assert_eq!(ev_bitmap[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8)), 0);

        mouse.write_config(1, &[EV_KEY as u8]);
        let key_bitmap = mouse.read_config(8, 128);
        assert_ne!(
            key_bitmap[(BTN_LEFT / 8) as usize] & (1u8 << (BTN_LEFT % 8)),
            0
        );

        mouse.write_config(1, &[EV_REL as u8]);
        let rel_bitmap = mouse.read_config(8, 128);
        assert_ne!(rel_bitmap[(REL_X / 8) as usize] & (1u8 << (REL_X % 8)), 0);
        assert_ne!(rel_bitmap[(REL_Y / 8) as usize] & (1u8 << (REL_Y % 8)), 0);
        assert_ne!(
            rel_bitmap[(REL_WHEEL / 8) as usize] & (1u8 << (REL_WHEEL % 8)),
            0
        );
    }

    #[test]
    fn event_queue_completes_used_ring_on_guest_memory_errors() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let event_desc = 0x1000;
        let event_avail = 0x2000;
        let event_used = 0x3000;

        let status_desc = 0x4000;
        let status_avail = 0x5000;
        let status_used = 0x6000;

        write_desc(
            &mut mem,
            event_desc,
            0,
            Descriptor {
                addr: u64::MAX - 4,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, event_avail, 0, &[0]);
        init_used(&mut mem, event_used);

        init_avail(&mut mem, status_avail, VRING_AVAIL_F_NO_INTERRUPT, &[]);
        init_used(&mut mem, status_used);

        let event_vq = VirtQueue::new(8, event_desc, event_avail, event_used);
        let status_vq = VirtQueue::new(8, status_desc, status_avail, status_used);

        let mut dev = VirtioInputDevice::new(VirtioInputDeviceKind::Keyboard, event_vq, status_vq);
        dev.set_status(VIRTIO_STATUS_DRIVER_OK);

        let irq = dev.inject_key(&mut mem, KEY_A, true).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let used_idx = mem.read_u16_le(event_used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }

    #[test]
    fn status_queue_completes_used_ring_on_guest_memory_errors() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let event_vq = VirtQueue::new(8, 0, 0, 0);

        let status_desc = 0x1000;
        let status_avail = 0x2000;
        let status_used = 0x3000;

        write_desc(
            &mut mem,
            status_desc,
            0,
            Descriptor {
                addr: u64::MAX - 4,
                len: VirtioInputEvent::BYTE_SIZE as u32,
                flags: 0,
                next: 0,
            },
        );

        init_avail(&mut mem, status_avail, 0, &[0]);
        init_used(&mut mem, status_used);

        let status_vq = VirtQueue::new(8, status_desc, status_avail, status_used);

        let mut dev = VirtioInputDevice::new(VirtioInputDeviceKind::Keyboard, event_vq, status_vq);
        let irq = dev.notify_status(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let used_idx = mem.read_u16_le(status_used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }
}
