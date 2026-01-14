use alloc::boxed::Box;
use alloc::vec::Vec;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotResult};

use super::regs::{
    PORTSC_CCS, PORTSC_CSC, PORTSC_LWS, PORTSC_PEC, PORTSC_PED, PORTSC_PLC, PORTSC_PLS_MASK,
    PORTSC_PLS_SHIFT, PORTSC_PP, PORTSC_PR, PORTSC_PRC, PORTSC_PS_MASK, PORTSC_PS_SHIFT,
};
use crate::device::AttachedUsbDevice;
use crate::{UsbDeviceModel, UsbSpeed};

// Reset signalling for USB2 ports is ~50ms (similar to the UHCI root hub model).
const RESET_DURATION_MS: u16 = 50;

/// Maximum bytes allowed for a nested `AttachedUsbDevice` snapshot when restoring.
///
/// This mirrors the limits used by hubs (`crate::hub`).
const MAX_USB_DEVICE_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XhciUsb2LinkState {
    /// U0 (active).
    U0,
    /// U3 (suspend).
    U3,
}

impl XhciUsb2LinkState {
    fn pls_bits(self) -> u32 {
        match self {
            // xHCI spec: PLS encoding.
            XhciUsb2LinkState::U0 => 0,
            XhciUsb2LinkState::U3 => 3,
        }
    }
}

fn link_state_from_pls_bits(bits: u8) -> SnapshotResult<XhciUsb2LinkState> {
    match bits {
        0 => Ok(XhciUsb2LinkState::U0),
        3 => Ok(XhciUsb2LinkState::U3),
        _ => Err(SnapshotError::InvalidFieldEncoding("xhci port link state")),
    }
}

fn usb_speed_from_port_speed_id(psiv: u8) -> SnapshotResult<UsbSpeed> {
    // Keep in sync with `port_speed_id` / Supported Protocol PSI IDs (USB2 only).
    match psiv {
        1 => Ok(UsbSpeed::Full),
        2 => Ok(UsbSpeed::Low),
        3 => Ok(UsbSpeed::High),
        _ => Err(SnapshotError::InvalidFieldEncoding("xhci port speed")),
    }
}

pub(crate) fn port_speed_id(speed: UsbSpeed) -> u8 {
    // xHCI spec uses speed IDs. For USB2:
    // - 1: full-speed
    // - 2: low-speed
    // - 3: high-speed
    match speed {
        UsbSpeed::Full => 1,
        UsbSpeed::Low => 2,
        UsbSpeed::High => 3,
    }
}

/// Internal model of an xHCI root hub port.
pub(crate) struct XhciPort {
    device: Option<AttachedUsbDevice>,

    connected: bool,
    connect_status_change: bool,

    enabled: bool,
    port_enabled_change: bool,

    reset: bool,
    reset_timer_ms: u16,
    port_reset_change: bool,

    link_state: XhciUsb2LinkState,
    port_link_state_change: bool,
    speed: Option<UsbSpeed>,
}

impl XhciPort {
    pub(crate) fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_status_change: false,
            enabled: false,
            port_enabled_change: false,
            reset: false,
            reset_timer_ms: 0,
            port_reset_change: false,
            link_state: XhciUsb2LinkState::U3,
            port_link_state_change: false,
            speed: None,
        }
    }

    pub(crate) fn host_controller_reset(&mut self) {
        if let Some(dev) = self.device.as_mut() {
            dev.reset();
        }

        // Keep the physical connection state but reset software-visible port state. This mirrors
        // the "fresh boot" state (a connected port starts disabled until the host issues a port
        // reset).
        self.enabled = false;
        self.reset = false;
        self.reset_timer_ms = 0;

        self.connect_status_change = false;
        self.port_enabled_change = false;
        self.port_reset_change = false;
        self.port_link_state_change = false;

        self.link_state = if self.connected {
            XhciUsb2LinkState::U0
        } else {
            XhciUsb2LinkState::U3
        };
        self.sync_device_suspended_state();
    }

    pub(crate) fn has_device(&self) -> bool {
        self.device.is_some()
    }

    pub(crate) fn device(&self) -> Option<&AttachedUsbDevice> {
        self.device.as_ref()
    }
    pub(crate) fn device_mut(&mut self) -> Option<&mut AttachedUsbDevice> {
        self.device.as_mut()
    }

    pub(crate) fn save_snapshot_record(&self) -> Vec<u8> {
        let mut rec = Encoder::new()
            .bool(self.connected)
            .bool(self.connect_status_change)
            .bool(self.enabled)
            .bool(self.port_enabled_change)
            .bool(self.reset)
            .u16(self.reset_timer_ms)
            .bool(self.port_reset_change)
            .u8(self.link_state.pls_bits() as u8)
            .bool(self.speed.is_some());

        if let Some(speed) = self.speed {
            rec = rec.u8(port_speed_id(speed));
        }

        rec = rec.bool(self.device.is_some());
        if let Some(dev) = self.device.as_ref() {
            let dev_state = dev.save_state();
            rec = rec.u32(dev_state.len() as u32).bytes(&dev_state);
        }

        rec = rec.bool(self.port_link_state_change);
        rec.finish()
    }

    pub(crate) fn load_snapshot_record(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut d = Decoder::new(bytes);

        self.connected = d.bool()?;
        self.connect_status_change = d.bool()?;
        self.enabled = d.bool()?;
        self.port_enabled_change = d.bool()?;
        self.reset = d.bool()?;
        self.reset_timer_ms = d.u16()?;
        self.port_reset_change = d.bool()?;

        let pls_bits = d.u8()?;
        self.link_state = link_state_from_pls_bits(pls_bits)?;

        let has_speed = d.bool()?;
        self.speed = if has_speed {
            Some(usb_speed_from_port_speed_id(d.u8()?)?)
        } else {
            None
        };

        let has_device_state = d.bool()?;
        let device_state = if has_device_state {
            let len = d.u32()? as usize;
            if len > MAX_USB_DEVICE_SNAPSHOT_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "usb device snapshot too large",
                ));
            }
            Some(d.bytes(len)?)
        } else {
            None
        };

        // v1.1+: optional Port Link State Change (PLC) bit snapshot. Older snapshots did not encode
        // it; treat missing bytes as "false" so restores remain forward compatible.
        let port_link_state_change = match d.bool() {
            Ok(v) => v,
            Err(SnapshotError::UnexpectedEof) => false,
            Err(e) => return Err(e),
        };
        d.finish()?;
        self.port_link_state_change = port_link_state_change;

        if let Some(device_state) = device_state {
            if self.device.is_none() {
                if let Some(dev) = AttachedUsbDevice::try_new_from_snapshot(device_state)? {
                    self.device = Some(dev);
                }
            }
            if let Some(dev) = self.device.as_mut() {
                dev.load_state(device_state)?;
            }
        } else {
            // Snapshot indicates no device attached.
            self.device = None;
        }

        self.sync_device_suspended_state();

        Ok(())
    }

    pub(crate) fn attach(&mut self, model: Box<dyn UsbDeviceModel>) -> bool {
        self.device = Some(AttachedUsbDevice::new(model));
        self.speed = self.device.as_ref().map(|d| d.speed());
        self.link_state = XhciUsb2LinkState::U0;
        self.sync_device_suspended_state();

        let mut changed = false;

        // Connecting a new device effectively disables the port until the host performs the
        // reset/enable sequence.
        if self.enabled {
            self.enabled = false;
            changed |= self.set_port_enabled_change();
        }

        if !self.connected {
            self.connected = true;
        }
        changed |= self.set_connect_status_change();

        // Cancel any in-flight reset.
        self.reset = false;
        self.reset_timer_ms = 0;
        self.port_reset_change = false;

        changed
    }

    pub(crate) fn detach(&mut self) -> bool {
        let mut changed = false;

        self.device = None;
        self.speed = None;
        self.link_state = XhciUsb2LinkState::U3;
        self.port_link_state_change = false;

        if self.connected {
            self.connected = false;
            changed |= self.set_connect_status_change();
        }

        if self.enabled {
            self.enabled = false;
            changed |= self.set_port_enabled_change();
        }

        // Cancel any in-flight reset.
        self.reset = false;
        self.reset_timer_ms = 0;
        self.port_reset_change = false;

        changed
    }

    pub(crate) fn read_portsc(&self) -> u32 {
        let mut v = 0u32;

        // Root hub ports are always powered in this model.
        v |= PORTSC_PP;

        if self.connected {
            v |= PORTSC_CCS;
        }
        if self.enabled {
            v |= PORTSC_PED;
        }
        if self.reset {
            v |= PORTSC_PR;
        }

        // Port Link State.
        v |= (self.link_state.pls_bits() << PORTSC_PLS_SHIFT) & PORTSC_PLS_MASK;

        // Port speed ID. Report 0 when not connected.
        if self.connected {
            if let Some(speed) = self.speed {
                v |= ((port_speed_id(speed) as u32) << PORTSC_PS_SHIFT) & PORTSC_PS_MASK;
            }
        }

        // Change bits.
        if self.connect_status_change {
            v |= PORTSC_CSC;
        }
        if self.port_enabled_change {
            v |= PORTSC_PEC;
        }
        if self.port_reset_change {
            v |= PORTSC_PRC;
        }
        if self.port_link_state_change {
            v |= PORTSC_PLC;
        }

        v
    }

    /// Writes to PORTSC and returns `true` if a Port Status Change Event should be generated.
    pub(crate) fn write_portsc(&mut self, value: u32) -> bool {
        let mut changed = false;

        // Write-1-to-clear change bits.
        if value & PORTSC_CSC != 0 {
            self.connect_status_change = false;
        }
        if value & PORTSC_PEC != 0 {
            self.port_enabled_change = false;
        }
        if value & PORTSC_PRC != 0 {
            self.port_reset_change = false;
        }
        if value & PORTSC_PLC != 0 {
            self.port_link_state_change = false;
        }

        // Port Reset (PR): writing 1 starts a reset. Hardware clears PR when complete.
        if value & PORTSC_PR != 0 && !self.reset && self.connected {
            self.reset = true;
            self.reset_timer_ms = RESET_DURATION_MS;

            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }
            // Reset exits suspend.
            self.link_state = XhciUsb2LinkState::U0;
            self.sync_device_suspended_state();

            // Per spec, the port is disabled during reset. If that changes PED, surface PEC.
            if self.enabled {
                self.enabled = false;
                changed |= self.set_port_enabled_change();
            }
        }

        if self.reset {
            // Ignore link-state writes while reset is active.
            return changed;
        }

        // Port Link State changes are requested by writing PLS alongside the LWS strobe.
        if (value & PORTSC_LWS) != 0 {
            let pls = ((value & PORTSC_PLS_MASK) >> PORTSC_PLS_SHIFT) as u8;
            let target = match pls {
                0 => Some(XhciUsb2LinkState::U0),
                3 => Some(XhciUsb2LinkState::U3),
                _ => None,
            };

            if let Some(target) = target {
                if self.connected && self.enabled {
                    changed |= self.set_link_state(target);
                }
            }
        }

        changed
    }

    /// Advances port-internal time by 1ms and returns `true` if a Port Status Change Event should
    /// be generated.
    pub(crate) fn tick_1ms(&mut self) -> bool {
        let mut changed = false;

        if self.reset {
            self.reset_timer_ms = self.reset_timer_ms.saturating_sub(1);
            if self.reset_timer_ms == 0 {
                self.reset = false;
                changed |= self.set_port_reset_change();

                // If a device is still present, the port becomes enabled after reset completes.
                if self.connected && !self.enabled {
                    self.enabled = true;
                    self.link_state = XhciUsb2LinkState::U0;
                    self.sync_device_suspended_state();
                    changed |= self.set_port_enabled_change();
                }
            }
        }

        // Remote wakeup for suspended (U3) ports.
        if self.enabled && self.link_state == XhciUsb2LinkState::U3 {
            let wake = match self.device.as_mut() {
                Some(dev) => dev.model_mut().poll_remote_wakeup(),
                None => false,
            };
            if wake {
                changed |= self.set_link_state(XhciUsb2LinkState::U0);
            }
        }

        if self.enabled && self.link_state == XhciUsb2LinkState::U0 {
            if let Some(dev) = self.device.as_mut() {
                dev.tick_1ms();
            }
        }

        changed
    }

    fn set_connect_status_change(&mut self) -> bool {
        // Return true only when the bit transitions 0->1, so callers can decide whether to emit an
        // event.
        if self.connect_status_change {
            return false;
        }
        self.connect_status_change = true;
        true
    }

    fn set_port_enabled_change(&mut self) -> bool {
        if self.port_enabled_change {
            return false;
        }
        self.port_enabled_change = true;
        true
    }

    fn set_port_reset_change(&mut self) -> bool {
        if self.port_reset_change {
            return false;
        }
        self.port_reset_change = true;
        true
    }

    fn set_link_state(&mut self, state: XhciUsb2LinkState) -> bool {
        if self.link_state == state {
            // Even if the link state doesn't change, keep the downstream device's suspended state
            // in sync so snapshot restores and host-side changes cannot leave it stale.
            self.sync_device_suspended_state();
            return false;
        }

        self.link_state = state;
        self.sync_device_suspended_state();

        if self.port_link_state_change {
            return false;
        }
        self.port_link_state_change = true;
        true
    }

    fn sync_device_suspended_state(&mut self) {
        if let Some(dev) = self.device.as_mut() {
            dev.model_mut()
                .set_suspended(self.link_state == XhciUsb2LinkState::U3);
        }
    }

    pub(crate) fn save_snapshot(&self) -> Vec<u8> {
        // Reuse the legacy `save_snapshot_record` encoding so older xHCI snapshots that stored
        // per-port state under TAG_PORTS can still be decoded by newer builds.
        self.save_snapshot_record()
    }

    pub(crate) fn load_snapshot(&mut self, buf: &[u8]) -> SnapshotResult<()> {
        self.load_snapshot_record(buf)
    }
}
