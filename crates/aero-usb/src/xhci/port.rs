use alloc::boxed::Box;

use super::regs::{
    PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PLS_MASK, PORTSC_PLS_SHIFT, PORTSC_PP,
    PORTSC_PR, PORTSC_PRC, PORTSC_PS_MASK, PORTSC_PS_SHIFT,
};
use crate::device::AttachedUsbDevice;
use crate::{UsbDeviceModel, UsbSpeed};

// Reset signalling for USB2 ports is ~50ms (similar to the UHCI root hub model).
const RESET_DURATION_MS: u16 = 50;

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

        self.link_state = if self.connected {
            XhciUsb2LinkState::U0
        } else {
            XhciUsb2LinkState::U3
        };
    }

    pub(crate) fn has_device(&self) -> bool {
        self.device.is_some()
    }

    pub(crate) fn device_mut(&mut self) -> Option<&mut AttachedUsbDevice> {
        self.device.as_mut()
    }

    pub(crate) fn attach(&mut self, model: Box<dyn UsbDeviceModel>) -> bool {
        self.device = Some(AttachedUsbDevice::new(model));
        self.speed = self.device.as_ref().map(|d| d.speed());
        self.link_state = XhciUsb2LinkState::U0;

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
        if let Some(speed) = self.speed {
            v |= ((port_speed_id(speed) as u32) << PORTSC_PS_SHIFT) & PORTSC_PS_MASK;
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

        // Port Reset (PR): writing 1 starts a reset. Hardware clears PR when complete.
        if value & PORTSC_PR != 0 && !self.reset && self.connected {
            self.reset = true;
            self.reset_timer_ms = RESET_DURATION_MS;

            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }

            // Per spec, the port is disabled during reset. If that changes PED, surface PEC.
            if self.enabled {
                self.enabled = false;
                changed |= self.set_port_enabled_change();
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
                    changed |= self.set_port_enabled_change();
                }
            }
        }

        if self.enabled {
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
}
