use wasm_bindgen::prelude::*;

use aero_usb::device::AttachedUsbDevice;
use aero_usb::passthrough::{UsbHostCompletion, UsbHostCompletionIn, UsbHostCompletionOut};
use aero_usb::{SetupPacket as BusSetupPacket, UsbSpeed, UsbWebUsbPassthroughDevice};

// -------------------------------------------------------------------------------------------------
// Extremely small EHCI-like harness
// -------------------------------------------------------------------------------------------------
//
// This is a developer-facing WebUSB passthrough harness that intentionally does *not* integrate
// with the main VM device graph. It exists to validate the WebUSB action↔completion plumbing and
// basic "EHCI-style" interrupt/status reporting (USBSTS bits + IRQ level) without mutating VM
// state.
//
// The harness does **not** attempt to model the full EHCI DMA schedule (qTD/QH traversal). Instead
// it drives the `AttachedUsbDevice` control-pipe state machine directly and exposes a tiny subset
// of EHCI operational register semantics:
// - USBSTS.USBINT (bit 0): latched when a control transfer completes successfully.
// - USBSTS.USBERRINT (bit 1): latched on stall/timeout/error.
// - USBSTS.PCD (bit 2): latched when the harness-attached device connects/disconnects.
//
// This mirrors the "dev harness" philosophy of `WebUsbUhciPassthroughHarness`: prove the end-to-end
// WebUSB passthrough pipeline in the browser without requiring a guest OS driver.

const USBSTS_USBINT: u32 = 1 << 0;
const USBSTS_USBERRINT: u32 = 1 << 1;
const USBSTS_PCD: u32 = 1 << 2;
const USBSTS_MASK: u32 = USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_PCD;

const USBINTR_USBINT: u32 = 1 << 0;
const USBINTR_USBERRINT: u32 = 1 << 1;
const USBINTR_PCD: u32 = 1 << 2;
const USBINTR_MASK: u32 = USBINTR_USBINT | USBINTR_USBERRINT | USBINTR_PCD;

const GET_DESCRIPTOR: u8 = 0x06;
const DESC_DEVICE: u16 = 0x0100;
const DESC_CONFIGURATION: u16 = 0x0200;

// Keep the harness safe even if a caller requests absurd descriptor sizes.
const MAX_DESCRIPTOR_BYTES: usize = 4096;
// Control endpoint max packet size is <= 64 for full-speed/high-speed devices.
const MAX_CONTROL_PACKET: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    DeviceDescriptor,
    ConfigDescriptor9,
    ConfigDescriptorFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingStage {
    Setup,
    DataIn,
    StatusOut,
}

#[derive(Debug, Clone)]
struct PendingControlIn {
    kind: PendingKind,
    setup: BusSetupPacket,
    stage: PendingStage,
    /// Requested length (`setup.w_length` as usize), clamped.
    requested: usize,
    /// Bytes received so far.
    received: Vec<u8>,
}

impl PendingControlIn {
    fn new(kind: PendingKind, setup: BusSetupPacket) -> Self {
        let requested = (setup.w_length as usize).min(MAX_DESCRIPTOR_BYTES);
        Self {
            kind,
            setup,
            stage: PendingStage::Setup,
            requested,
            received: Vec::new(),
        }
    }

    fn remaining(&self) -> usize {
        self.requested.saturating_sub(self.received.len())
    }
}

#[wasm_bindgen]
pub struct WebUsbEhciPassthroughHarness {
    controller_attached: bool,
    device: Option<AttachedUsbDevice>,
    webusb: Option<UsbWebUsbPassthroughDevice>,

    usbsts: u32,
    usbintr: u32,
    irq_level: bool,

    pending: Option<PendingControlIn>,
    last_error: Option<String>,

    device_descriptor: Vec<u8>,
    config_descriptor: Vec<u8>,
    config_total_len: usize,
}

impl WebUsbEhciPassthroughHarness {
    fn update_irq(&mut self) {
        self.usbsts &= USBSTS_MASK;
        self.usbintr &= USBINTR_MASK;
        self.irq_level = (self.usbsts & self.usbintr & USBSTS_MASK) != 0;
    }

    fn set_error(&mut self, message: String) {
        self.last_error = Some(message);
        self.usbsts |= USBSTS_USBERRINT;
        self.update_irq();
        self.pending = None;
    }

    fn clear_results(&mut self) {
        self.device_descriptor.clear();
        self.config_descriptor.clear();
        self.config_total_len = 0;
    }

    fn ensure_device(&mut self) -> Result<&mut AttachedUsbDevice, JsValue> {
        if !self.controller_attached {
            return Err(js_sys::Error::new("EHCI harness controller is not attached").into());
        }
        self.device.as_mut().ok_or_else(|| {
            js_sys::Error::new("EHCI harness passthrough device is not attached").into()
        })
    }

    fn begin_control_in(
        &mut self,
        kind: PendingKind,
        setup: BusSetupPacket,
    ) -> Result<(), JsValue> {
        let _ = self.ensure_device()?;
        self.last_error = None;
        self.pending = Some(PendingControlIn::new(kind, setup));
        Ok(())
    }

    fn maybe_finish_pending(&mut self, finished: PendingControlIn) {
        // Latch an interrupt for the completed transfer.
        self.usbsts |= USBSTS_USBINT;

        match finished.kind {
            PendingKind::DeviceDescriptor => {
                self.device_descriptor = finished.received;
            }
            PendingKind::ConfigDescriptor9 => {
                // Parse wTotalLength from the 9-byte config descriptor header.
                let bytes = finished.received;
                if bytes.len() >= 4 {
                    let total = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
                    self.config_total_len = total.clamp(9, MAX_DESCRIPTOR_BYTES);
                } else {
                    self.config_total_len = 0;
                }

                // Preserve the short config header bytes as the current config descriptor until we
                // successfully fetch the full payload.
                self.config_descriptor = bytes;

                // Chain into the full config descriptor read if we learned a non-zero wTotalLength.
                if self.config_total_len >= 9 {
                    let len_u16 = (self.config_total_len.min(u16::MAX as usize)) as u16;
                    let setup = BusSetupPacket {
                        bm_request_type: 0x80,
                        b_request: GET_DESCRIPTOR,
                        w_value: DESC_CONFIGURATION,
                        w_index: 0,
                        w_length: len_u16,
                    };
                    self.pending = Some(PendingControlIn::new(
                        PendingKind::ConfigDescriptorFull,
                        setup,
                    ));
                }
            }
            PendingKind::ConfigDescriptorFull => {
                self.config_descriptor = finished.received;
            }
        }

        self.update_irq();
    }

    fn step_once(&mut self) {
        if !self.controller_attached {
            return;
        }
        let Some(mut pending) = self.pending.take() else {
            self.update_irq();
            return;
        };

        let Some(dev) = self.device.as_mut() else {
            self.set_error("EHCI harness device missing".to_string());
            return;
        };

        match pending.stage {
            PendingStage::Setup => match dev.handle_setup(pending.setup) {
                aero_usb::device::UsbOutResult::Ack => {
                    pending.stage = if pending.requested == 0 {
                        PendingStage::StatusOut
                    } else {
                        PendingStage::DataIn
                    };
                    self.pending = Some(pending);
                }
                aero_usb::device::UsbOutResult::Nak => {
                    // SETUP should not NAK in real hardware, but keep this defensive.
                    self.pending = Some(pending);
                }
                aero_usb::device::UsbOutResult::Stall => {
                    self.set_error("USB STALL during SETUP stage".to_string());
                }
                aero_usb::device::UsbOutResult::Timeout => {
                    self.set_error("USB timeout during SETUP stage".to_string());
                }
            },
            PendingStage::DataIn => {
                let remaining = pending.remaining();
                if remaining == 0 {
                    pending.stage = PendingStage::StatusOut;
                    self.pending = Some(pending);
                    return;
                }

                let max_len = remaining.min(MAX_CONTROL_PACKET).max(1);
                match dev.handle_in(0, max_len) {
                    aero_usb::device::UsbInResult::Data(chunk) => {
                        pending.received.extend_from_slice(&chunk);
                        if pending.received.len() > pending.requested {
                            pending.received.truncate(pending.requested);
                        }

                        // End the DATA stage on a short packet or once we have the requested length.
                        if chunk.len() < max_len || pending.received.len() >= pending.requested {
                            pending.stage = PendingStage::StatusOut;
                        }
                        self.pending = Some(pending);
                    }
                    aero_usb::device::UsbInResult::Nak => {
                        self.pending = Some(pending);
                    }
                    aero_usb::device::UsbInResult::Stall => {
                        self.set_error("USB STALL during DATA stage".to_string());
                    }
                    aero_usb::device::UsbInResult::Timeout => {
                        self.set_error("USB timeout during DATA stage".to_string());
                    }
                }
            }
            PendingStage::StatusOut => match dev.handle_out(0, &[]) {
                aero_usb::device::UsbOutResult::Ack => {
                    self.maybe_finish_pending(pending);
                }
                aero_usb::device::UsbOutResult::Nak => {
                    self.pending = Some(pending);
                }
                aero_usb::device::UsbOutResult::Stall => {
                    self.set_error("USB STALL during STATUS stage".to_string());
                }
                aero_usb::device::UsbOutResult::Timeout => {
                    self.set_error("USB timeout during STATUS stage".to_string());
                }
            },
        }
    }
}

#[wasm_bindgen]
impl WebUsbEhciPassthroughHarness {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            controller_attached: false,
            device: None,
            webusb: None,
            usbsts: 0,
            usbintr: 0,
            irq_level: false,
            pending: None,
            last_error: None,
            device_descriptor: Vec::new(),
            config_descriptor: Vec::new(),
            config_total_len: 0,
        }
    }

    /// Attach the EHCI controller model (resets USBSTS/USBINTR state).
    pub fn attach_controller(&mut self) {
        self.controller_attached = true;
        self.device = None;
        self.webusb = None;
        self.pending = None;
        self.last_error = None;
        self.clear_results();
        self.usbsts = 0;
        self.usbintr = USBINTR_MASK;
        self.update_irq();
    }

    /// Detach the EHCI controller model.
    pub fn detach_controller(&mut self) {
        self.controller_attached = false;
        self.device = None;
        self.webusb = None;
        self.pending = None;
        self.update_irq();
    }

    /// Attach a WebUSB passthrough device to the harness controller.
    pub fn attach_device(&mut self) -> Result<(), JsValue> {
        if !self.controller_attached {
            return Err(js_sys::Error::new("Cannot attach device: controller not attached").into());
        }
        if self.device.is_some() {
            return Ok(());
        }
        // EHCI presents attached devices as high-speed. Ensure the passthrough device model
        // advertises a high-speed view so the guest sees unmodified high-speed descriptors.
        let webusb = UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High);
        let attached = AttachedUsbDevice::new(Box::new(webusb.clone()));
        self.webusb = Some(webusb);
        self.device = Some(attached);
        self.usbsts |= USBSTS_PCD;
        self.clear_results();
        self.pending = None;
        self.last_error = None;
        self.update_irq();
        Ok(())
    }

    /// Detach the WebUSB passthrough device from the controller.
    pub fn detach_device(&mut self) {
        if self.device.is_some() {
            self.device = None;
            self.webusb = None;
            self.pending = None;
            self.usbsts |= USBSTS_PCD;
            self.update_irq();
        }
    }

    /// Queue a GET_DESCRIPTOR(Device, 18) request.
    pub fn cmd_get_device_descriptor(&mut self) -> Result<(), JsValue> {
        let setup = BusSetupPacket {
            bm_request_type: 0x80,
            b_request: GET_DESCRIPTOR,
            w_value: DESC_DEVICE,
            w_index: 0,
            w_length: 18,
        };
        self.begin_control_in(PendingKind::DeviceDescriptor, setup)
    }

    /// Queue a GET_DESCRIPTOR(Config, 9) followed by a GET_DESCRIPTOR(Config, wTotalLength).
    pub fn cmd_get_config_descriptor(&mut self) -> Result<(), JsValue> {
        let setup = BusSetupPacket {
            bm_request_type: 0x80,
            b_request: GET_DESCRIPTOR,
            w_value: DESC_CONFIGURATION,
            w_index: 0,
            w_length: 9,
        };
        self.begin_control_in(PendingKind::ConfigDescriptor9, setup)
    }

    /// Advance the harness state machine. This should be called from the worker tick loop.
    pub fn tick(&mut self) {
        // Avoid infinite loops if a device model misbehaves; we only need a few stage transitions
        // per tick for responsiveness.
        for _ in 0..8 {
            let before_pending = self
                .pending
                .as_ref()
                .map(|p| (p.kind, p.stage, p.received.len()));
            let before_error = self.last_error.is_some();
            self.step_once();
            let after_pending = self
                .pending
                .as_ref()
                .map(|p| (p.kind, p.stage, p.received.len()));
            let after_error = self.last_error.is_some();

            // Stop if we didn't make any progress this iteration (or if we hit an error).
            if before_pending == after_pending && before_error == after_error {
                break;
            }
            if after_error {
                break;
            }
        }
    }

    /// EHCI USBSTS bits (subset) as a u32.
    pub fn usbsts(&self) -> u32 {
        self.usbsts & USBSTS_MASK
    }

    /// Current IRQ line level (computed from USBSTS & USBINTR).
    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn controller_attached(&self) -> bool {
        self.controller_attached
    }

    pub fn device_attached(&self) -> bool {
        self.device.is_some()
    }

    /// Return the last error string (if any). Exposed as `string | null`.
    pub fn last_error(&self) -> JsValue {
        match self.last_error.as_ref() {
            Some(msg) => JsValue::from_str(msg),
            None => JsValue::NULL,
        }
    }

    /// Clear (W1C) the specified USBSTS bits.
    pub fn clear_usbsts(&mut self, bits: u32) {
        self.usbsts &= !(bits & USBSTS_MASK);
        self.update_irq();
    }

    /// Drain queued `UsbHostAction` objects from the attached WebUSB passthrough device.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let Some(dev) = self.webusb.as_ref() else {
            return Ok(JsValue::NULL);
        };
        let actions: Vec<aero_usb::passthrough::UsbHostAction> = dev.drain_actions();
        if actions.is_empty() {
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Push a single host completion into the harness.
    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // Surface a minimal EHCI-like interrupt/status behavior:
        //
        // The WebUSB proxy contract models *full* control transfers (`controlTransferIn/Out`) as a
        // single action+completion pair, so it is reasonable to latch USBINT/USBERRINT at completion
        // time. This ensures the JS/TS runtime can observe USBSTS changes immediately when a host
        // completion is delivered (even if no further host actions are drained on the next tick).
        match &completion {
            UsbHostCompletion::ControlIn { result, .. }
            | UsbHostCompletion::BulkIn { result, .. } => match result {
                UsbHostCompletionIn::Success { .. } => {
                    self.usbsts |= USBSTS_USBINT;
                }
                UsbHostCompletionIn::Stall => {
                    self.usbsts |= USBSTS_USBERRINT;
                }
                UsbHostCompletionIn::Error { message } => {
                    self.usbsts |= USBSTS_USBERRINT;
                    self.last_error.get_or_insert_with(|| message.clone());
                }
            },
            UsbHostCompletion::ControlOut { result, .. }
            | UsbHostCompletion::BulkOut { result, .. } => match result {
                UsbHostCompletionOut::Success { .. } => {
                    self.usbsts |= USBSTS_USBINT;
                }
                UsbHostCompletionOut::Stall => {
                    self.usbsts |= USBSTS_USBERRINT;
                }
                UsbHostCompletionOut::Error { message } => {
                    self.usbsts |= USBSTS_USBERRINT;
                    self.last_error.get_or_insert_with(|| message.clone());
                }
            },
        }
        if let Some(dev) = self.webusb.as_ref() {
            dev.push_completion(completion);
        }
        self.update_irq();
        Ok(())
    }

    pub fn device_descriptor(&self) -> JsValue {
        if self.device_descriptor.is_empty() {
            JsValue::NULL
        } else {
            js_sys::Uint8Array::from(self.device_descriptor.as_slice()).into()
        }
    }

    pub fn config_descriptor(&self) -> JsValue {
        if self.config_descriptor.is_empty() {
            JsValue::NULL
        } else {
            js_sys::Uint8Array::from(self.config_descriptor.as_slice()).into()
        }
    }

    pub fn config_total_len(&self) -> u32 {
        self.config_total_len as u32
    }
}
