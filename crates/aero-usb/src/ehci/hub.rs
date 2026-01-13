use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::device::AttachedUsbDevice;
use crate::{UsbDeviceModel, UsbHubAttachError};

use super::regs::*;

struct Port {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    over_current: bool,
    over_current_change: bool,
    reset: bool,
    reset_countdown_ms: u8,
    suspended: bool,
    resuming: bool,
    resume_countdown_ms: u8,
    powered: bool,
    /// Port ownership bit (`PORTSC.PORT_OWNER`, bit 13).
    ///
    /// When `true`, the port is treated as owned by a companion controller (UHCI/OHCI). Aero does
    /// not yet model companion controllers, so `PORT_OWNER=1` conservatively makes the attached
    /// device unreachable from the EHCI controller (while still reporting physical connection via
    /// `CCS`).
    port_owner: bool,
}

impl Port {
    fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_change: false,
            enabled: false,
            enable_change: false,
            over_current: false,
            over_current_change: false,
            reset: false,
            reset_countdown_ms: 0,
            suspended: false,
            resuming: false,
            resume_countdown_ms: 0,
            // Many EHCI controllers power ports by default; we model ports as powered-on after reset
            // while still allowing software to explicitly toggle power via PORTSC.PP.
            powered: true,
            // Conservative default: ports start routed to a companion controller until
            // CONFIGFLAG=1 claims them for EHCI.
            port_owner: true,
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        if let Some(dev) = self.device.as_mut() {
            dev.model_mut().set_suspended(suspended);
        }
    }

    fn attach(&mut self, model: Box<dyn UsbDeviceModel>) {
        self.device = Some(AttachedUsbDevice::new(model));
        if !self.connected {
            self.connected = true;
        }
        self.connect_change = true;

        // Connecting a device disables the port until the host performs the reset/enable sequence.
        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
        }

        self.set_suspended(false);
        self.resuming = false;
        self.resume_countdown_ms = 0;
        self.reset = false;
        self.reset_countdown_ms = 0;
    }

    fn detach(&mut self) {
        self.device = None;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
        }

        self.set_suspended(false);
        self.resuming = false;
        self.resume_countdown_ms = 0;
        self.reset = false;
        self.reset_countdown_ms = 0;
    }

    fn read_portsc(&self) -> u32 {
        let mut v = 0u32;

        if self.connected {
            v |= PORTSC_CCS;
        }
        if self.connect_change {
            v |= PORTSC_CSC;
        }
        if self.enabled {
            v |= PORTSC_PED;
        }
        if self.enable_change {
            v |= PORTSC_PEDC;
        }
        if self.over_current {
            v |= PORTSC_OCA;
        }
        if self.over_current_change {
            v |= PORTSC_OCC;
        }
        if self.resuming {
            v |= PORTSC_FPR;
        }
        if self.suspended {
            v |= PORTSC_SUSP;
        }
        if self.reset {
            v |= PORTSC_PR;
        }
        if self.powered {
            v |= PORTSC_PP;
        }
        if self.port_owner {
            v |= PORTSC_PO;
        }

        v
    }

    fn set_port_owner(&mut self, owner: bool) -> bool {
        if self.port_owner == owner {
            return false;
        }
        self.port_owner = owner;

        // If the port is handed off to a companion controller, EHCI should no longer report it as
        // enabled and should drop any in-progress reset/suspend/resume state.
        if owner {
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
            self.set_suspended(false);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            self.reset = false;
            self.reset_countdown_ms = 0;
        }

        true
    }

    fn write_portsc(&mut self, value: u32, write_mask: u32) {
        // Write-1-to-clear change bits.
        if write_mask & PORTSC_CSC != 0 && value & PORTSC_CSC != 0 {
            self.connect_change = false;
        }
        if write_mask & PORTSC_PEDC != 0 && value & PORTSC_PEDC != 0 {
            self.enable_change = false;
        }
        if write_mask & PORTSC_OCC != 0 && value & PORTSC_OCC != 0 {
            self.over_current_change = false;
        }

        // PORT_OWNER is only writable when the port is disabled.
        if write_mask & PORTSC_PO != 0 && !self.enabled {
            let want_owner = value & PORTSC_PO != 0;
            self.set_port_owner(want_owner);
        }

        // When the port is owned by a companion controller, EHCI should not drive reset/enable/etc.
        if self.port_owner {
            return;
        }

        // Port power (R/W when HCSPARAMS.PPC=1).
        if write_mask & PORTSC_PP != 0 {
            let want_powered = value & PORTSC_PP != 0;
            if self.powered != want_powered {
                self.powered = want_powered;
                if !want_powered {
                    // Powering off forces the port into a disabled, non-suspended state.
                    if self.enabled {
                        self.enabled = false;
                        self.enable_change = true;
                    }
                    self.set_suspended(false);
                    self.resuming = false;
                    self.resume_countdown_ms = 0;
                    self.reset = false;
                    self.reset_countdown_ms = 0;
                }
            }
        }

        // Port reset: model a deterministic 50ms reset and reset attached device state.
        if write_mask & PORTSC_PR != 0 && value & PORTSC_PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;

            self.set_suspended(false);
            self.resuming = false;
            self.resume_countdown_ms = 0;

            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }

            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        }

        if self.reset {
            // While the port reset signal is active, suspend/resume/enable writes are ignored.
            return;
        }

        // Port enable/disable.
        if write_mask & PORTSC_PED != 0 {
            let want_enabled = value & PORTSC_PED != 0;
            if want_enabled {
                // Hardware only enables when a device is present, the port is powered, and the port
                // is not already enabled.
                if self.connected && self.powered && !self.enabled {
                    self.enabled = true;
                    self.enable_change = true;
                }
            } else if self.enabled {
                self.enabled = false;
                self.enable_change = true;
                self.set_suspended(false);
                self.resuming = false;
                self.resume_countdown_ms = 0;
            }
        }

        if !self.connected || !self.powered {
            self.set_suspended(false);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            return;
        }

        // Suspend.
        if write_mask & PORTSC_SUSP != 0 {
            let want_suspended = value & PORTSC_SUSP != 0;
            if want_suspended {
                if !self.resuming {
                    self.set_suspended(true);
                }
            } else {
                self.set_suspended(false);
            }
        }

        // Force Port Resume.
        if write_mask & PORTSC_FPR != 0 {
            let want_resuming = value & PORTSC_FPR != 0;
            if want_resuming {
                self.resuming = true;
                self.resume_countdown_ms = 20;
            } else {
                self.resuming = false;
                self.resume_countdown_ms = 0;
            }
        }
    }

    fn tick_1ms(&mut self) {
        if self.reset {
            self.reset_countdown_ms = self.reset_countdown_ms.saturating_sub(1);
            if self.reset_countdown_ms == 0 {
                self.reset = false;
                if self.connected && self.powered && !self.enabled {
                    // After reset deasserts, EHCI ports become enabled by hardware.
                    self.enabled = true;
                    self.enable_change = true;
                }
            }
        }

        if self.resuming {
            self.resume_countdown_ms = self.resume_countdown_ms.saturating_sub(1);
            if self.resume_countdown_ms == 0 {
                self.resuming = false;
                self.set_suspended(false);
            }
        }
    }
}

/// EHCI root hub exposed via PORTSC registers.
///
/// This is a minimal model intended for Windows driver bring-up. By default we expose 6 ports
/// (`EhciController::new()`); use `EhciController::new_with_port_count()` to override.
pub struct RootHub {
    ports: Vec<Port>,
}

impl RootHub {
    pub fn new(num_ports: usize) -> Self {
        let mut ports = Vec::with_capacity(num_ports);
        ports.extend((0..num_ports).map(|_| Port::new()));
        Self { ports }
    }

    pub fn num_ports(&self) -> usize {
        self.ports.len()
    }

    pub fn attach(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        if let Some(p) = self.ports.get_mut(port) {
            p.attach(model);
        }
    }

    pub fn detach(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.detach();
        }
    }

    pub fn attach_at_path(
        &mut self,
        path: &[u8],
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbHubAttachError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        let root_port = root_port as usize;
        if root_port >= self.ports.len() {
            return Err(UsbHubAttachError::InvalidPort);
        }

        // If only a root port is provided, attach directly to the root hub.
        if rest.is_empty() {
            if self.ports[root_port].device.is_some() {
                return Err(UsbHubAttachError::PortOccupied);
            }
            self.attach(root_port, model);
            return Ok(());
        }

        let p = &mut self.ports[root_port];
        let Some(root_dev) = p.device.as_mut() else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for &hop in hub_path {
            hub_dev = hub_dev.model_mut().hub_port_device_mut(hop)?;
        }
        hub_dev.model_mut().hub_attach_device(leaf_port, model)
    }

    pub fn detach_at_path(&mut self, path: &[u8]) -> Result<(), UsbHubAttachError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        let root_port = root_port as usize;
        if root_port >= self.ports.len() {
            return Err(UsbHubAttachError::InvalidPort);
        }

        // If only a root port is provided, detach directly from the root hub.
        if rest.is_empty() {
            if self.ports[root_port].device.is_none() {
                return Err(UsbHubAttachError::NoDevice);
            }
            self.detach(root_port);
            return Ok(());
        }

        let p = &mut self.ports[root_port];
        let Some(root_dev) = p.device.as_mut() else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for &hop in hub_path {
            hub_dev = hub_dev.model_mut().hub_port_device_mut(hop)?;
        }
        hub_dev.model_mut().hub_detach_device(leaf_port)
    }

    /// Returns the device currently attached to the specified root port, regardless of whether the
    /// port is powered/enabled.
    pub fn port_device(&self, port: usize) -> Option<&AttachedUsbDevice> {
        self.ports.get(port)?.device.as_ref()
    }

    /// Mutable variant of [`RootHub::port_device`].
    pub fn port_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device.as_mut()
    }

    pub fn read_portsc(&self, port: usize) -> u32 {
        self.ports.get(port).map(|p| p.read_portsc()).unwrap_or(0)
    }

    pub(crate) fn set_all_port_owner(&mut self, owner: bool) -> bool {
        let mut changed = false;
        for p in &mut self.ports {
            if p.set_port_owner(owner) {
                changed = true;
            }
        }
        changed
    }

    pub fn write_portsc(&mut self, port: usize, value: u32) {
        self.write_portsc_masked(port, value, 0xffff_ffff);
    }

    pub(crate) fn write_portsc_masked(&mut self, port: usize, value: u32, write_mask: u32) {
        if let Some(p) = self.ports.get_mut(port) {
            p.write_portsc(value, write_mask);
        }
    }

    pub fn tick_1ms(&mut self) {
        for p in &mut self.ports {
            p.tick_1ms();

            if !(p.enabled && p.powered) || p.suspended || p.resuming {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                dev.tick_1ms();
            }
        }
    }

    pub fn any_port_change(&self) -> bool {
        self.ports.iter().any(|p| {
            p.connect_change || p.enable_change || p.over_current_change
        })
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        for p in &mut self.ports {
            if p.port_owner || !(p.enabled && p.powered) || p.suspended || p.resuming {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                if let Some(found) = dev.device_mut_for_address(address) {
                    return Some(found);
                }
            }
        }
        None
    }
}

impl Default for RootHub {
    fn default() -> Self {
        Self::new(6)
    }
}
