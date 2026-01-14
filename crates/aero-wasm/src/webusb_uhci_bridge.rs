#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::UsbWebUsbPassthroughDevice;
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::UhciController;

use crate::guest_memory_bus::{GuestMemoryBus, NoDmaMemory, wasm_memory_byte_len};

const WEBUSB_UHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"WUHB";
const WEBUSB_UHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

// UHCI register offsets (0x20 bytes).
const REG_USBCMD: u16 = 0x00;

const ROOT_PORT_EXTERNAL_HUB: usize = 0;
const ROOT_PORT_WEBUSB: usize = 1;
// Must match `web/src/usb/uhci_external_hub.ts::DEFAULT_EXTERNAL_HUB_PORT_COUNT`.
const EXTERNAL_HUB_PORT_COUNT: u8 = 16;

#[wasm_bindgen]
pub struct WebUsbUhciBridge {
    guest_base: u32,
    controller: UhciController,
    /// WebUSB passthrough device handle.
    ///
    /// This handle is kept alive across disconnect/reconnect so host action IDs remain monotonic.
    webusb: Option<UsbWebUsbPassthroughDevice>,
    webusb_connected: bool,
    pci_command: u16,
}

#[wasm_bindgen]
impl WebUsbUhciBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32) -> Self {
        let mut controller = UhciController::new();
        controller.hub_mut().attach(
            ROOT_PORT_EXTERNAL_HUB,
            Box::new(UsbHubDevice::with_port_count(EXTERNAL_HUB_PORT_COUNT)),
        );

        Self {
            guest_base,
            controller,
            webusb: None,
            webusb_connected: false,
            pci_command: 0,
        }
    }

    /// Mirror the guest-written PCI command register (0x04, low 16 bits) into the WASM device
    /// wrapper.
    ///
    /// This is used to enforce PCI Bus Master Enable gating for DMA. In a JS runtime, the PCI
    /// configuration space lives in TypeScript (`PciBus`), so the WASM bridge must be updated via
    /// this explicit hook.
    pub fn set_pci_command(&mut self, command: u32) {
        self.pci_command = (command & 0xffff) as u16;
    }

    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        let Ok(offset) = u16::try_from(offset) else {
            return 0xffff_ffff;
        };
        let Ok(size) = usize::try_from(size) else {
            return 0xffff_ffff;
        };

        match size {
            0 => 0,
            1 | 2 | 4 => self.controller.io_read(offset, size),
            _ => 0xffff_ffff,
        }
    }

    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        let Ok(offset) = u16::try_from(offset) else {
            return;
        };
        let Ok(size) = usize::try_from(size) else {
            return;
        };
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.controller.io_write(offset, size, value);
    }

    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }
        // Only DMA when PCI Bus Master Enable is set (command bit 2). When bus mastering is
        // disabled the controller must not read/write guest RAM, but it should still advance its
        // internal frame counter and root hub state (port reset timing, remote wake, etc).
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mem_bytes = wasm_memory_byte_len();
            let guest_size = mem_bytes
                .saturating_sub(self.guest_base as u64)
                .min(crate::guest_layout::PCI_MMIO_BASE);
            let mut mem = GuestMemoryBus::new(self.guest_base, guest_size);
            for _ in 0..frames {
                self.controller.tick_1ms(&mut mem);
            }
        } else {
            let mut mem = NoDmaMemory;
            for _ in 0..frames {
                self.controller.tick_1ms(&mut mem);
            }
        }
    }

    pub fn irq_level(&self) -> bool {
        self.controller.irq_level()
    }

    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self.webusb_connected;

        match (was_connected, connected) {
            (true, true) | (false, false) => return,
            (false, true) => {
                let dev = self
                    .webusb
                    .get_or_insert_with(UsbWebUsbPassthroughDevice::new);
                self.controller
                    .hub_mut()
                    .attach(ROOT_PORT_WEBUSB, Box::new(dev.clone()));
                self.webusb_connected = true;
            }
            (true, false) => {
                self.controller.hub_mut().detach(ROOT_PORT_WEBUSB);
                self.webusb_connected = false;
                // Preserve pre-existing semantics: disconnect drops queued/in-flight host state, but
                // we keep the handle alive so `UsbPassthroughDevice.next_id` remains monotonic.
                if let Some(dev) = self.webusb.as_ref() {
                    dev.reset();
                }
            }
        };
    }

    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        }
        let Some(dev) = self.webusb.as_ref() else {
            return Ok(JsValue::NULL);
        };
        let actions: Vec<UsbHostAction> = dev.drain_actions();
        if actions.is_empty() {
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                dev.push_completion(completion);
            }
        }

        Ok(())
    }

    pub fn reset(&mut self) {
        self.controller.io_write(
            REG_USBCMD,
            2,
            u32::from(aero_usb::uhci::regs::USBCMD_HCRESET),
        );

        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                dev.reset();
            }
        }
    }

    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        }
        let Some(dev) = self.webusb.as_ref() else {
            return Ok(JsValue::NULL);
        };
        let summary = dev.pending_summary();
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows `aero_usb::hub::RootHub`:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are hub ports (1-based).
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        if path.len() == 1 && path[0] as usize == ROOT_PORT_EXTERNAL_HUB {
            return Err(
                js_sys::Error::new("Cannot detach the external USB hub from root port 0").into(),
            );
        }
        crate::uhci_controller_bridge::detach_device_at_path(&mut self.controller, &path)
    }

    /// Attach a WebHID-backed USB HID device at the given topology path.
    pub fn attach_webhid_device(
        &mut self,
        path: JsValue,
        device: &crate::WebHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        validate_webhid_attach_path(&path)?;
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.controller,
            &path,
            Box::new(device.as_usb_device()),
        )
    }

    /// Attach a generic USB HID passthrough device at the given topology path.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        validate_webhid_attach_path(&path)?;
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.controller,
            &path,
            Box::new(device.as_usb_device()),
        )
    }

    /// Serialize the current bridge state into a deterministic snapshot blob.
    ///
    /// Format: top-level `aero-io-snapshot` TLV with:
    /// - tag 1: `aero_usb::uhci::UhciController` snapshot bytes
    /// - tag 2: IRQ latch (`irq_level`) (redundant; derived from UHCI state)
    /// - tag 3: external hub (`UsbHubDevice`) snapshot bytes
    /// - tag 4: WebUSB passthrough device (`UsbWebUsbPassthroughDevice`) snapshot bytes
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;
        const TAG_EXTERNAL_HUB: u16 = 3;
        const TAG_WEBUSB_DEVICE: u16 = 4;

        let mut w = SnapshotWriter::new(
            WEBUSB_UHCI_BRIDGE_DEVICE_ID,
            WEBUSB_UHCI_BRIDGE_DEVICE_VERSION,
        );
        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());
        w.field_bool(TAG_IRQ_ASSERTED, self.controller.irq_level());

        if let Some(hub_state) = self.with_external_hub(|hub| hub.save_state()) {
            w.field_bytes(TAG_EXTERNAL_HUB, hub_state);
        }
        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                // Persist the WebUSB passthrough device's USB-visible state (address, control-transfer
                // stage, etc) so that after restoring a VM snapshot the guest's TD retries can make
                // forward progress. Host-side action queues are cleared on restore (see `load_state`).
                w.field_bytes(TAG_WEBUSB_DEVICE, dev.save_state());
            }
        }
        w.finish()
    }

    /// Restore bridge state from a snapshot blob produced by [`save_state`].
    ///
    /// WebUSB host actions are backed by JS Promises and cannot be resumed after restoring a VM
    /// snapshot. As part of restore we drop the passthrough device's host-action queues and
    /// in-flight maps so the guest's UHCI TD retries will re-emit host actions instead of waiting
    /// forever for completions that will never arrive.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_EXTERNAL_HUB: u16 = 3;
        const TAG_WEBUSB_DEVICE: u16 = 4;

        let r = SnapshotReader::parse(bytes, WEBUSB_UHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB UHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(WEBUSB_UHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB UHCI bridge snapshot: {e}")))?;

        // Ensure the external hub + passthrough device exist before restoring so the bridge retains
        // owned handles for host-side integration (draining actions / pushing completions).
        //
        // Note: the underlying `aero-usb` controller snapshot can now reconstruct common USB device
        // models on its own, but the bridge still needs explicit `UsbHubDevice` / WebUSB handles.
        if r.bytes(TAG_EXTERNAL_HUB).is_some() && self.with_external_hub(|_| ()).is_none() {
            self.controller.hub_mut().attach(
                ROOT_PORT_EXTERNAL_HUB,
                Box::new(UsbHubDevice::with_port_count(EXTERNAL_HUB_PORT_COUNT)),
            );
        }
        self.set_connected(r.bytes(TAG_WEBUSB_DEVICE).is_some());

        let controller_bytes = r.bytes(TAG_CONTROLLER).ok_or_else(|| {
            JsValue::from_str("WebUSB UHCI bridge snapshot missing controller state")
        })?;
        self.controller
            .load_state(controller_bytes)
            .map_err(|e| JsValue::from_str(&format!("Invalid UHCI controller snapshot: {e}")))?;

        if let Some(buf) = r.bytes(TAG_EXTERNAL_HUB) {
            let Some(res) = self.with_external_hub_mut(|hub| hub.load_state(buf)) else {
                return Err(JsValue::from_str(
                    "WebUSB UHCI bridge missing external hub device",
                ));
            };
            res.map_err(|e| JsValue::from_str(&format!("Invalid external hub snapshot: {e}")))?;
        }

        if let Some(buf) = r.bytes(TAG_WEBUSB_DEVICE) {
            let Some(dev) = self.webusb.as_mut() else {
                return Err(JsValue::from_str(
                    "WebUSB UHCI bridge snapshot contains WebUSB device state but WebUSB is not connected",
                ));
            };
            dev.load_state(buf)
                .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB device snapshot: {e}")))?;
            dev.reset_host_state_for_restore();
        }

        Ok(())
    }

    /// Snapshot the full UHCI + USB device tree state as deterministic bytes.
    ///
    /// The returned bytes represent only the USB stack state (controller + devices), not guest RAM.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore UHCI + USB device state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

impl WebUsbUhciBridge {
    fn with_external_hub<R>(&self, f: impl FnOnce(&UsbHubDevice) -> R) -> Option<R> {
        let dev = self.controller.hub().port_device(ROOT_PORT_EXTERNAL_HUB)?;
        let any = dev.model() as &dyn core::any::Any;
        let hub = any.downcast_ref::<UsbHubDevice>()?;
        Some(f(hub))
    }

    fn with_external_hub_mut<R>(&mut self, f: impl FnOnce(&mut UsbHubDevice) -> R) -> Option<R> {
        let mut dev = self
            .controller
            .hub_mut()
            .port_device_mut(ROOT_PORT_EXTERNAL_HUB)?;
        let any = dev.model_mut() as &mut dyn core::any::Any;
        let hub = any.downcast_mut::<UsbHubDevice>()?;
        Some(f(hub))
    }
}

fn validate_webhid_attach_path(path: &[u8]) -> Result<(), JsValue> {
    if path.len() < 2 {
        return Err(js_sys::Error::new(
            "WebHID devices must attach behind the external hub (expected path like [0, <hubPort>])",
        )
        .into());
    }
    if path[0] as usize != ROOT_PORT_EXTERNAL_HUB {
        return Err(js_sys::Error::new(
            "WebHID devices must attach behind the external hub on root port 0",
        )
        .into());
    }
    Ok(())
}
