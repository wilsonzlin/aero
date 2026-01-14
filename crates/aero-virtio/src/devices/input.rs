use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use std::collections::VecDeque;

pub const VIRTIO_DEVICE_TYPE_INPUT: u16 = 18;

pub const VIRTIO_INPUT_SUBSYSTEM_KEYBOARD: u16 = 0x0010;
pub const VIRTIO_INPUT_SUBSYSTEM_MOUSE: u16 = 0x0011;
pub const VIRTIO_INPUT_SUBSYSTEM_TABLET: u16 = 0x0012;

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
pub const EV_ABS: u16 = 0x03;
pub const EV_LED: u16 = 0x11;

pub const SYN_REPORT: u16 = 0x00;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
// Linux input ABI: horizontal wheel (tilt wheel). Often surfaced to HID as "AC Pan".
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;

pub const LED_NUML: u16 = 0x00;
pub const LED_CAPSL: u16 = 0x01;
pub const LED_SCROLLL: u16 = 0x02;
pub const LED_COMPOSE: u16 = 0x03;
pub const LED_KANA: u16 = 0x04;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const BTN_FORWARD: u16 = 0x115;
pub const BTN_BACK: u16 = 0x116;
pub const BTN_TASK: u16 = 0x117;

// Keyboard key codes (Linux input ABI).
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
pub const KEY_KPASTERISK: u16 = 55;
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
pub const KEY_KP7: u16 = 71;
pub const KEY_KP8: u16 = 72;
pub const KEY_KP9: u16 = 73;
pub const KEY_KPMINUS: u16 = 74;
pub const KEY_KP4: u16 = 75;
pub const KEY_KP5: u16 = 76;
pub const KEY_KP6: u16 = 77;
pub const KEY_KPPLUS: u16 = 78;
pub const KEY_KP1: u16 = 79;
pub const KEY_KP2: u16 = 80;
pub const KEY_KP3: u16 = 81;
pub const KEY_KP0: u16 = 82;
pub const KEY_KPDOT: u16 = 83;
pub const KEY_102ND: u16 = 86;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_RO: u16 = 89;
pub const KEY_KPENTER: u16 = 96;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_KPSLASH: u16 = 98;
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
// Consumer/media keys (used by the Windows 7 virtio-input driver to expose a Consumer Control HID collection).
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_KPEQUAL: u16 = 117;
pub const KEY_PAUSE: u16 = 119;
pub const KEY_KPCOMMA: u16 = 121;
pub const KEY_YEN: u16 = 124;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_MENU: u16 = 139;
pub const KEY_NEXTSONG: u16 = 163;
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_PREVIOUSSONG: u16 = 165;
pub const KEY_STOPCD: u16 = 166;

// Host-side safety: cap how many events we will buffer when the guest is not consuming the
// virtio-input event queue (e.g. driver stalled or malicious guest). Real virtio-input devices do
// not provide infinite buffering; dropping oldest events is preferable to unbounded growth.
const MAX_PENDING_EVENTS: usize = 4096;

// Host-side safety: cap how much statusq payload we will parse per chain. The spec only requires
// consuming and completing statusq buffers; parsing is a best-effort diagnostic feature.
const MAX_STATUSQ_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioInputDeviceKind {
    Keyboard,
    Mouse,
    Tablet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioInputAbsInfo {
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub res: i32,
}

impl VirtioInputAbsInfo {
    fn to_le_bytes(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&self.min.to_le_bytes());
        out[4..8].copy_from_slice(&self.max.to_le_bytes());
        out[8..12].copy_from_slice(&self.fuzz.to_le_bytes());
        out[12..16].copy_from_slice(&self.flat.to_le_bytes());
        out[16..20].copy_from_slice(&self.res.to_le_bytes());
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioInputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

impl VirtioInputEvent {
    fn to_le_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&self.type_.to_le_bytes());
        out[2..4].copy_from_slice(&self.code.to_le_bytes());
        out[4..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }

    fn from_le_bytes(bytes: [u8; 8]) -> Self {
        let type_ = u16::from_le_bytes([bytes[0], bytes[1]]);
        let code = u16::from_le_bytes([bytes[2], bytes[3]]);
        let value = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        Self { type_, code, value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioInputDevids {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

impl VirtioInputDevids {
    pub fn to_le_bytes(self) -> [u8; 8] {
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
    abs: [u8; 128],
    led: [u8; 128],
}

impl VirtioInputBitmaps {
    fn empty() -> Self {
        Self {
            ev: [0u8; 128],
            key: [0u8; 128],
            rel: [0u8; 128],
            abs: [0u8; 128],
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
            KEY_KPASTERISK,
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
            KEY_KP7,
            KEY_KP8,
            KEY_KP9,
            KEY_KPMINUS,
            KEY_KP4,
            KEY_KP5,
            KEY_KP6,
            KEY_KPPLUS,
            KEY_KP1,
            KEY_KP2,
            KEY_KP3,
            KEY_KP0,
            KEY_KPDOT,
            KEY_102ND,
            KEY_RO,
            KEY_KPENTER,
            KEY_RIGHTCTRL,
            KEY_KPSLASH,
            KEY_SYSRQ,
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
            KEY_MUTE,
            KEY_VOLUMEDOWN,
            KEY_VOLUMEUP,
            KEY_KPEQUAL,
            KEY_PAUSE,
            KEY_KPCOMMA,
            KEY_YEN,
            KEY_MENU,
            KEY_NEXTSONG,
            KEY_PLAYPAUSE,
            KEY_PREVIOUSSONG,
            KEY_STOPCD,
        ]);
        bitmaps.led = Self::with_bits(&[LED_NUML, LED_CAPSL, LED_SCROLLL, LED_COMPOSE, LED_KANA]);
        bitmaps
    }

    fn for_mouse() -> Self {
        let mut bitmaps = Self::empty();
        bitmaps.ev = Self::with_bits(&[EV_SYN, EV_KEY, EV_REL]);
        bitmaps.key = Self::with_bits(&[
            BTN_LEFT,
            BTN_RIGHT,
            BTN_MIDDLE,
            BTN_SIDE,
            BTN_EXTRA,
            BTN_FORWARD,
            BTN_BACK,
            BTN_TASK,
        ]);
        bitmaps.rel = Self::with_bits(&[REL_X, REL_Y, REL_WHEEL, REL_HWHEEL]);
        bitmaps
    }

    fn for_tablet() -> Self {
        let mut bitmaps = Self::empty();
        bitmaps.ev = Self::with_bits(&[EV_SYN, EV_KEY, EV_ABS]);
        // Expose the same 8-button set as the relative mouse; the Win7 virtio-input tablet HID
        // descriptor advertises 8 buttons.
        bitmaps.key = Self::with_bits(&[
            BTN_LEFT,
            BTN_RIGHT,
            BTN_MIDDLE,
            BTN_SIDE,
            BTN_EXTRA,
            BTN_FORWARD,
            BTN_BACK,
            BTN_TASK,
        ]);
        bitmaps.abs = Self::with_bits(&[ABS_X, ABS_Y]);
        bitmaps
    }
}

pub struct VirtioInput {
    kind: VirtioInputDeviceKind,
    name: String,
    serial: String,
    devids: VirtioInputDevids,
    config_select: u8,
    config_subsel: u8,
    bitmaps: VirtioInputBitmaps,

    pending: VecDeque<VirtioInputEvent>,
    buffers: VecDeque<DescriptorChain>,

    /// Keyboard LED state as last reported by the guest driver via `statusq` (queue 1).
    ///
    /// Bit mapping:
    /// - bit0: Num Lock (`LED_NUML`)
    /// - bit1: Caps Lock (`LED_CAPSL`)
    /// - bit2: Scroll Lock (`LED_SCROLLL`)
    /// - bit3: Compose (`LED_COMPOSE`)
    /// - bit4: Kana (`LED_KANA`)
    leds_mask: u8,
}

impl VirtioInput {
    pub fn new(kind: VirtioInputDeviceKind) -> Self {
        let (name, devids, bitmaps) = match kind {
            VirtioInputDeviceKind::Keyboard => (
                "Aero Virtio Keyboard".to_string(),
                VirtioInputDevids {
                    bustype: 0x0006,
                    vendor: 0x1af4,
                    product: 0x0001,
                    version: 0x0001,
                },
                VirtioInputBitmaps::for_keyboard(),
            ),
            VirtioInputDeviceKind::Mouse => (
                "Aero Virtio Mouse".to_string(),
                VirtioInputDevids {
                    bustype: 0x0006,
                    vendor: 0x1af4,
                    product: 0x0002,
                    version: 0x0001,
                },
                VirtioInputBitmaps::for_mouse(),
            ),
            VirtioInputDeviceKind::Tablet => (
                "Aero Virtio Tablet".to_string(),
                VirtioInputDevids {
                    bustype: 0x0006,
                    vendor: 0x1af4,
                    product: 0x0003,
                    version: 0x0001,
                },
                VirtioInputBitmaps::for_tablet(),
            ),
        };

        Self {
            kind,
            name,
            serial: "0".to_string(),
            devids,
            config_select: VIRTIO_INPUT_CFG_UNSET,
            config_subsel: 0,
            bitmaps,
            pending: VecDeque::new(),
            buffers: VecDeque::new(),
            leds_mask: 0,
        }
    }

    pub fn push_event(&mut self, event: VirtioInputEvent) {
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.pending.pop_front();
        }
        self.pending.push_back(event);
    }

    pub fn inject_key(&mut self, code: u16, pressed: bool) {
        // Linux input defines code 0 as KEY_RESERVED / BTN_RESERVED. Treat it as a no-op so host
        // injection cannot generate spurious events for an invalid code.
        if code == 0 {
            return;
        }
        self.push_event(VirtioInputEvent {
            type_: EV_KEY,
            code,
            value: if pressed { 1 } else { 0 },
        });
        self.push_event(VirtioInputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
    }

    pub fn inject_rel_move(&mut self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        if dx != 0 {
            self.push_event(VirtioInputEvent {
                type_: EV_REL,
                code: REL_X,
                value: dx,
            });
        }
        if dy != 0 {
            self.push_event(VirtioInputEvent {
                type_: EV_REL,
                code: REL_Y,
                value: dy,
            });
        }
        self.push_event(VirtioInputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
    }

    pub fn inject_wheel(&mut self, delta: i32) {
        self.inject_wheel2(delta, 0);
    }

    pub fn inject_hwheel(&mut self, delta: i32) {
        self.inject_wheel2(0, delta);
    }

    /// Inject a vertical + horizontal scroll (wheel) update and terminate it with a single
    /// `SYN_REPORT`.
    ///
    /// This matches how a physical pointing device may report both axes within one frame.
    pub fn inject_wheel2(&mut self, wheel: i32, hwheel: i32) {
        if wheel == 0 && hwheel == 0 {
            return;
        }
        if wheel != 0 {
            self.push_event(VirtioInputEvent {
                type_: EV_REL,
                code: REL_WHEEL,
                value: wheel,
            });
        }
        if hwheel != 0 {
            self.push_event(VirtioInputEvent {
                type_: EV_REL,
                code: REL_HWHEEL,
                value: hwheel,
            });
        }
        self.push_event(VirtioInputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
    }

    pub fn inject_button(&mut self, code: u16, pressed: bool) {
        if code == 0 {
            return;
        }
        self.push_event(VirtioInputEvent {
            type_: EV_KEY,
            code,
            value: if pressed { 1 } else { 0 },
        });
        self.push_event(VirtioInputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
    }

    pub fn inject_abs_move(&mut self, x: i32, y: i32) {
        self.push_event(VirtioInputEvent {
            type_: EV_ABS,
            code: ABS_X,
            value: x,
        });
        self.push_event(VirtioInputEvent {
            type_: EV_ABS,
            code: ABS_Y,
            value: y,
        });
        self.push_event(VirtioInputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
    }

    /// Number of input events currently queued for delivery to the guest.
    ///
    /// This is primarily intended for host-side diagnostics and unit tests.
    pub fn pending_events_len(&self) -> usize {
        self.pending.len()
    }

    pub fn kind(&self) -> VirtioInputDeviceKind {
        self.kind
    }

    /// Return the current guest-reported keyboard LED state bitmask.
    ///
    /// Bit mapping:
    /// - bit0: Num Lock (`LED_NUML`)
    /// - bit1: Caps Lock (`LED_CAPSL`)
    /// - bit2: Scroll Lock (`LED_SCROLLL`)
    /// - bit3: Compose (`LED_COMPOSE`)
    /// - bit4: Kana (`LED_KANA`)
    pub fn leds_mask(&self) -> u8 {
        self.leds_mask
    }

    /// Number of host-injected events currently buffered for delivery to the guest.
    ///
    /// This is primarily intended for lightweight integration tests that want to assert that
    /// injection APIs enqueue events without needing to fully simulate virtqueues.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

impl Default for VirtioInput {
    fn default() -> Self {
        Self::new(VirtioInputDeviceKind::Keyboard)
    }
}

impl VirtioDevice for VirtioInput {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_INPUT
    }

    fn subsystem_device_id(&self) -> u16 {
        match self.kind {
            VirtioInputDeviceKind::Keyboard => VIRTIO_INPUT_SUBSYSTEM_KEYBOARD,
            VirtioInputDeviceKind::Mouse => VIRTIO_INPUT_SUBSYSTEM_MOUSE,
            VirtioInputDeviceKind::Tablet => VIRTIO_INPUT_SUBSYSTEM_TABLET,
        }
    }

    fn pci_header_type(&self) -> u8 {
        match self.kind {
            // Mark the keyboard (function 0) as multi-function so guests can discover the mouse
            // function (function 1) on the same device.
            VirtioInputDeviceKind::Keyboard => 0x80,
            VirtioInputDeviceKind::Mouse => 0x00,
            VirtioInputDeviceKind::Tablet => 0x00,
        }
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC
    }

    fn set_features(&mut self, _features: u64) {}

    fn num_queues(&self) -> u16 {
        // eventq + statusq.
        2
    }

    fn queue_max_size(&self, _queue: u16) -> u16 {
        64
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        match queue_index {
            // eventq
            0 => {
                // Prevent unbounded growth if a corrupted/malicious driver repeatedly publishes
                // event buffers (e.g. by moving `avail.idx` far ahead and causing the transport to
                // re-consume stale ring entries). A correct driver cannot have more outstanding
                // event buffers than the queue size.
                let max_buffers = queue.size() as usize;
                if max_buffers != 0 && self.buffers.len() >= max_buffers {
                    return queue
                        .add_used(mem, chain.head_index(), 0)
                        .map_err(|_| VirtioDeviceError::IoError);
                }

                self.buffers.push_back(chain);
                self.flush_events(queue, mem)
            }
            // statusq
            1 => {
                {
                    // Virtio-input statusq: the guest may post LED/output events.
                    //
                    // Contract v1 only requires us to consume and complete the chain, but we still
                    // parse and track the common keyboard LEDs for host diagnostics and parity with
                    // real virtio-input behaviour.
                    let mem_ro: &dyn GuestMemory = &*mem;
                    self.process_statusq_chain(&chain, mem_ro);
                }
                queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)
            }
            _ => Err(VirtioDeviceError::Unsupported),
        }
    }

    fn poll_queue(
        &mut self,
        queue_index: u16,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        if queue_index != 0 {
            return Ok(false);
        }
        self.flush_events(queue, mem)
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        let bytes = self.config_bytes();
        let start = _offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = *bytes.get(start + i).unwrap_or(&0);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            match offset.saturating_add(i as u64) {
                0 => self.config_select = byte,
                1 => self.config_subsel = byte,
                _ => {}
            }
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.buffers.clear();
        self.config_select = VIRTIO_INPUT_CFG_UNSET;
        self.config_subsel = 0;
        self.leds_mask = 0;
    }

    fn snapshot_device_state(&self) -> Option<Vec<u8>> {
        // Keep this encoding small and forward-compatible:
        // - byte0: version
        // - byte1: config_select
        // - byte2: config_subsel
        // - byte3: leds_mask (HID-style bit layout; masked on restore)
        Some(vec![
            1,
            self.config_select,
            self.config_subsel,
            self.leds_mask,
        ])
    }

    fn restore_device_state(&mut self, bytes: &[u8]) {
        // Snapshots may be untrusted/corrupt. Best-effort restore and clamp fields so we don't
        // permanently break virtio-input config reads.
        if bytes.len() < 4 {
            return;
        }
        if bytes[0] != 1 {
            return;
        }
        self.config_select = bytes[1];
        self.config_subsel = bytes[2];
        self.leds_mask = bytes[3] & 0x1F;
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl VirtioInput {
    fn flush_events(
        &mut self,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let mut need_irq = false;
        while let Some(chain) = self.buffers.pop_front() {
            let Some(event) = self.pending.pop_front() else {
                self.buffers.push_front(chain);
                break;
            };

            let bytes = event.to_le_bytes();
            let descs = chain.descriptors();
            if descs.is_empty() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }

            let mut written = 0usize;
            for d in descs {
                if !d.is_write_only() {
                    return Err(VirtioDeviceError::BadDescriptorChain);
                }
                if written == bytes.len() {
                    break;
                }
                let take = (d.len as usize).min(bytes.len() - written);
                mem.write(d.addr, &bytes[written..written + take])
                    .map_err(|_| VirtioDeviceError::IoError)?;
                written += take;
            }

            need_irq |= queue
                .add_used(mem, chain.head_index(), written as u32)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        Ok(need_irq)
    }

    fn config_bytes(&self) -> [u8; 8 + 128] {
        let mut cfg = [0u8; 8 + 128];
        cfg[0] = self.config_select;
        cfg[1] = self.config_subsel;

        let (size, payload) = self.config_payload();
        cfg[2] = size;
        cfg[8..].copy_from_slice(&payload);
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
                payload[..8].copy_from_slice(&self.devids.to_le_bytes());
                (8, payload)
            }
            VIRTIO_INPUT_CFG_PROP_BITS => (0, payload),
            VIRTIO_INPUT_CFG_ABS_INFO => {
                // Only the tablet variant advertises EV_ABS. For keyboard/mouse, treat ABS_INFO as
                // absent.
                if self.kind != VirtioInputDeviceKind::Tablet {
                    return (0, payload);
                }

                // For Aero contract v1, expose an absolute coordinate range matching the HID
                // logical range used by the Win7 virtio-input tablet HID descriptor.
                const DEFAULT_MIN: i32 = 0;
                const DEFAULT_MAX: i32 = 32767;

                let abs = match u16::from(self.config_subsel) {
                    ABS_X | ABS_Y => Some(VirtioInputAbsInfo {
                        min: DEFAULT_MIN,
                        max: DEFAULT_MAX,
                        fuzz: 0,
                        flat: 0,
                        res: 0,
                    }),
                    _ => None,
                };

                if let Some(abs) = abs {
                    let bytes = abs.to_le_bytes();
                    payload[..bytes.len()].copy_from_slice(&bytes);
                    (bytes.len() as u8, payload)
                } else {
                    (0, payload)
                }
            }
            VIRTIO_INPUT_CFG_EV_BITS => {
                let bitmap = match self.config_subsel {
                    0 => Some(&self.bitmaps.ev),
                    x if x == EV_KEY as u8 => Some(&self.bitmaps.key),
                    x if x == EV_REL as u8 => Some(&self.bitmaps.rel),
                    x if x == EV_ABS as u8 => Some(&self.bitmaps.abs),
                    x if x == EV_LED as u8 => Some(&self.bitmaps.led),
                    _ => None,
                };

                if let Some(bitmap) = bitmap {
                    payload.copy_from_slice(bitmap);
                    (payload.len() as u8, payload)
                } else {
                    (0, payload)
                }
            }
            _ => (0, payload),
        }
    }

    fn process_statusq_chain(&mut self, chain: &DescriptorChain, mem: &dyn GuestMemory) {
        // Only the keyboard variant advertises EV_LED support. Still consume buffers for other
        // variants, but ignore any payload.
        if self.kind != VirtioInputDeviceKind::Keyboard {
            return;
        }

        let mut staged_mask = self.leds_mask;
        let mut budget = MAX_STATUSQ_BYTES;

        let mut pending = [0u8; 8];
        let mut pending_len = 0usize;

        'descs: for desc in chain.descriptors() {
            // Statusq buffers are guest→device, but some guests might accidentally set the WRITE
            // flag. We treat the descriptor flags as advisory and always attempt to parse the
            // payload.
            let mut addr = desc.addr;
            let mut remaining = desc.len as usize;
            let mut scratch = [0u8; 256];

            while remaining != 0 {
                if budget == 0 {
                    break 'descs;
                }
                let take = remaining.min(scratch.len()).min(budget);
                if mem.read(addr, &mut scratch[..take]).is_err() {
                    break 'descs;
                }

                let mut slice = &scratch[..take];

                if pending_len != 0 {
                    let take = (8 - pending_len).min(slice.len());
                    pending[pending_len..pending_len + take].copy_from_slice(&slice[..take]);
                    pending_len += take;
                    slice = &slice[take..];

                    if pending_len == 8 {
                        let event = VirtioInputEvent::from_le_bytes(pending);
                        self.process_statusq_event(event, &mut staged_mask);
                        pending_len = 0;
                    }
                }

                while slice.len() >= 8 {
                    let (evt, rest) = slice.split_at(8);
                    let event = VirtioInputEvent::from_le_bytes(evt.try_into().unwrap());
                    self.process_statusq_event(event, &mut staged_mask);
                    slice = rest;
                }

                if !slice.is_empty() {
                    pending[..slice.len()].copy_from_slice(slice);
                    pending_len = slice.len();
                }

                addr = match addr.checked_add(take as u64) {
                    Some(v) => v,
                    None => break 'descs,
                };
                remaining -= take;
                budget -= take;
            }
        }

        // If the guest didn't send a terminating SYN_REPORT, still apply any LED updates.
        self.leds_mask = staged_mask;
    }

    fn process_statusq_event(&mut self, event: VirtioInputEvent, staged_mask: &mut u8) {
        match event.type_ {
            EV_LED => {
                let bit = match event.code {
                    LED_NUML => 0,
                    LED_CAPSL => 1,
                    LED_SCROLLL => 2,
                    LED_COMPOSE => 3,
                    LED_KANA => 4,
                    _ => return,
                };
                let flag = 1u8 << bit;
                if event.value != 0 {
                    *staged_mask |= flag;
                } else {
                    *staged_mask &= !flag;
                }
            }
            EV_SYN if event.code == SYN_REPORT => {
                self.leds_mask = *staged_mask;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestRam};
    use crate::queue::{PoppedDescriptorChain, VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_WRITE};

    #[test]
    fn inject_wheel2_emits_one_syn_report() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Mouse);
        dev.inject_wheel2(2, -3);
        assert_eq!(
            dev.pending.len(),
            3,
            "inject_wheel2 should emit REL_WHEEL, REL_HWHEEL, and a single SYN_REPORT"
        );
        let events: Vec<VirtioInputEvent> = dev.pending.iter().copied().collect();
        assert_eq!(
            events,
            vec![
                VirtioInputEvent {
                    type_: EV_REL,
                    code: REL_WHEEL,
                    value: 2,
                },
                VirtioInputEvent {
                    type_: EV_REL,
                    code: REL_HWHEEL,
                    value: -3,
                },
                VirtioInputEvent {
                    type_: EV_SYN,
                    code: SYN_REPORT,
                    value: 0,
                },
            ]
        );
    }

    #[test]
    fn inject_rel_move_is_noop_when_deltas_are_zero() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Mouse);
        dev.inject_rel_move(0, 0);
        assert!(
            dev.pending.is_empty(),
            "inject_rel_move(0,0) should not enqueue a standalone SYN_REPORT"
        );
    }

    #[test]
    fn inject_key_ignores_reserved_code_zero() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
        dev.inject_key(0, true);
        dev.inject_key(0, false);
        assert!(dev.pending.is_empty());
    }

    #[test]
    fn inject_button_ignores_reserved_code_zero() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Mouse);
        dev.inject_button(0, true);
        dev.inject_button(0, false);
        assert!(dev.pending.is_empty());
    }

    #[test]
    fn pending_event_queue_is_bounded() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Mouse);
        for i in 0..(MAX_PENDING_EVENTS + 100) {
            dev.push_event(VirtioInputEvent {
                type_: EV_REL,
                code: REL_X,
                value: i as i32,
            });
        }

        assert_eq!(dev.pending.len(), MAX_PENDING_EVENTS);
        assert_eq!(dev.pending.front().unwrap().value, 100);
        assert_eq!(
            dev.pending.back().unwrap().value,
            (MAX_PENDING_EVENTS + 99) as i32
        );
    }

    fn write_desc(
        mem: &mut GuestRam,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    #[test]
    fn event_buffer_queue_is_bounded() {
        let mut dev = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
        let mut mem = GuestRam::new(0x10000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        for i in 0..qsize {
            let buf_addr = 0x4000 + u64::from(i) * 0x100;
            write_desc(&mut mem, desc_table, i, buf_addr, 8, VIRTQ_DESC_F_WRITE, 0);
        }

        // Malicious: claim there are 1000 available entries, but only provide `qsize` ring slots.
        let avail_idx = 1000u16;
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, avail_idx).unwrap();
        for i in 0..qsize {
            write_u16_le(&mut mem, avail + 4 + u64::from(i) * 2, i).unwrap();
        }
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        for _ in 0..avail_idx {
            let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
                PoppedDescriptorChain::Chain(chain) => chain,
                PoppedDescriptorChain::Invalid { error, .. } => {
                    panic!("unexpected descriptor chain parse error: {error:?}")
                }
            };
            dev.process_queue(0, chain, &mut queue, &mut mem).unwrap();
        }

        assert_eq!(dev.buffers.len(), qsize as usize);
        assert_eq!(
            read_u16_le(&mem, used + 2).unwrap(),
            avail_idx - qsize,
            "extra event buffers should be completed with used.len=0 once the internal queue is full"
        );
    }
}
