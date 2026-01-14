use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use core::cell::{Ref, RefCell, RefMut};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotResult};

use crate::device::AttachedUsbDevice;
use crate::hub::{RootHubDevice, RootHubDeviceMut};
use crate::usb2_port::Usb2PortMux;
use crate::{UsbDeviceModel, UsbHubAttachError, UsbSpeed};

use super::regs::*;

const MAX_USB_DEVICE_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

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
    /// When `true`, the port is treated as owned by a companion controller (UHCI/OHCI). If this
    /// root hub port is backed by a [`Usb2PortMux`], ownership handoff is handled by the mux and
    /// the device will be routed to the companion controller; otherwise `PORT_OWNER=1` makes the
    /// device unreachable from EHCI.
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

    fn propagate_suspended_state(&mut self) {
        if let Some(dev) = self.device.as_mut() {
            dev.model_mut().set_suspended(self.suspended);
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
 
            // Speed reporting:
            //
            // Guest EHCI drivers consult PORTSC.HSP + PORTSC.LS (line status) to determine the
            // attached device speed and decide whether to hand off a port to a companion controller.
            //
            // Contract (matches `Usb2PortMux` / EHCI 1.0 spec line-status encoding):
            // - High-speed devices set HSP (when EHCI-owned) and clear the LS bits.
            // - Full-speed devices clear HSP and report idle J-state via LS=0b10.
            // - Low-speed devices clear HSP and report idle K-state via LS=0b01.
            // - While resuming at full/low speed, report a K-state (LS=0b01).
            if let Some(dev) = self.device.as_ref() {
                let speed = dev.speed();

                // PORTSC.HSP is only meaningful when EHCI owns the port (PORT_OWNER=0).
                if !self.port_owner && speed == UsbSpeed::High {
                    v |= PORTSC_HSP;
                }

                if !self.reset {
                    let ls: u32 = if speed == UsbSpeed::High {
                        0
                    } else if self.resuming {
                        0b01
                    } else {
                        match speed {
                            UsbSpeed::High => 0,
                            UsbSpeed::Full => 0b10, // J-state (D+ high)
                            UsbSpeed::Low => 0b01,  // K-state (D- high)
                        }
                    };
                    v |= ls << 10;
                }
            } else if !self.reset {
                // No device object (should be rare); preserve previous MVP behaviour.
                let ls = if self.resuming { 0b01 } else { 0b10 };
                v |= (ls as u32) << 10;
            }
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

enum RootHubPortSlot {
    Local(Port),
    Usb2Mux {
        mux: Rc<RefCell<Usb2PortMux>>,
        port: usize,
    },
}

impl RootHubPortSlot {
    fn has_device(&self) -> bool {
        match self {
            RootHubPortSlot::Local(p) => p.device.is_some(),
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow().port_device(*port).is_some(),
        }
    }

    fn attach(&mut self, model: Box<dyn UsbDeviceModel>) {
        match self {
            RootHubPortSlot::Local(p) => p.attach(model),
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow_mut().attach(*port, model),
        }
    }

    fn detach(&mut self) {
        match self {
            RootHubPortSlot::Local(p) => p.detach(),
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow_mut().detach(*port),
        }
    }

    fn port_device(&self) -> Option<RootHubDevice<'_>> {
        match self {
            RootHubPortSlot::Local(p) => p.device.as_ref().map(RootHubDevice::Direct),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let mux_ref = mux.borrow();
                mux_ref.port_device(*port)?;
                Some(RootHubDevice::Muxed(Ref::map(mux_ref, |m| {
                    m.port_device(*port).expect("checked is_some above")
                })))
            }
        }
    }

    fn port_device_mut(&mut self) -> Option<RootHubDeviceMut<'_>> {
        match self {
            RootHubPortSlot::Local(p) => p.device.as_mut().map(RootHubDeviceMut::Direct),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let mux_ref = mux.borrow_mut();
                mux_ref.port_device(*port)?;
                Some(RootHubDeviceMut::Muxed(RefMut::map(mux_ref, |m| {
                    m.port_device_mut(*port).expect("checked is_some above")
                })))
            }
        }
    }

    fn read_portsc(&self) -> u32 {
        match self {
            RootHubPortSlot::Local(p) => p.read_portsc(),
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow().ehci_read_portsc(*port),
        }
    }

    fn write_portsc_masked(&mut self, value: u32, mut write_mask: u32) {
        match self {
            RootHubPortSlot::Local(p) => p.write_portsc(value, write_mask),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                // PORT_OWNER is only writable when the port is disabled. Match the local root hub
                // semantics by masking out the PO bit if the mux's EHCI view reports it enabled.
                if write_mask & PORTSC_PO != 0 {
                    let cur = mux.borrow().ehci_read_portsc(*port);
                    if cur & PORTSC_PED != 0 {
                        write_mask &= !PORTSC_PO;
                    }
                }
                mux.borrow_mut()
                    .ehci_write_portsc_masked(*port, value, write_mask);
            }
        }
    }

    fn any_port_change(&self) -> bool {
        match self {
            RootHubPortSlot::Local(p) => p.connect_change || p.enable_change || p.over_current_change,
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let st = mux.borrow().ehci_read_portsc(*port);
                (st & (PORTSC_CSC | PORTSC_PEDC)) != 0
            }
        }
    }

    fn set_port_owner(&mut self, owner: bool) -> bool {
        match self {
            RootHubPortSlot::Local(p) => p.set_port_owner(owner),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let before = mux.borrow().ehci_read_portsc(*port) & PORTSC_PO != 0;
                let value = if owner { PORTSC_PO } else { 0 };
                mux.borrow_mut()
                    .ehci_write_portsc_masked(*port, value, PORTSC_PO);
                let after = mux.borrow().ehci_read_portsc(*port) & PORTSC_PO != 0;
                before != after
            }
        }
    }

    fn save_snapshot_record(&self) -> Vec<u8> {
        match self {
            RootHubPortSlot::Local(port) => {
                let mut rec = Encoder::new()
                    .bool(port.connected)
                    .bool(port.connect_change)
                    .bool(port.enabled)
                    .bool(port.enable_change)
                    .bool(port.over_current)
                    .bool(port.over_current_change)
                    .bool(port.reset)
                    .u8(port.reset_countdown_ms)
                    .bool(port.suspended)
                    .bool(port.resuming)
                    .u8(port.resume_countdown_ms)
                    .bool(port.powered)
                    .bool(port.port_owner)
                    .bool(port.device.is_some());

                if let Some(dev) = port.device.as_ref() {
                    let dev_state = dev.save_state();
                    rec = rec.u32(dev_state.len() as u32).bytes(&dev_state);
                }

                rec.finish()
            }
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow().save_snapshot_ehci_port_record(*port),
        }
    }

    fn load_snapshot_record(&mut self, buf: &[u8]) -> SnapshotResult<()> {
        match self {
            RootHubPortSlot::Local(port) => {
                let mut pd = Decoder::new(buf);
                port.connected = pd.bool()?;
                port.connect_change = pd.bool()?;
                port.enabled = pd.bool()?;
                port.enable_change = pd.bool()?;
                port.over_current = pd.bool()?;
                port.over_current_change = pd.bool()?;
                port.reset = pd.bool()?;
                port.reset_countdown_ms = pd.u8()?;
                port.suspended = pd.bool()?;
                port.resuming = pd.bool()?;
                port.resume_countdown_ms = pd.u8()?;
                port.powered = pd.bool()?;
                port.port_owner = pd.bool()?;

                let has_device_state = pd.bool()?;
                let device_state = if has_device_state {
                    let len = pd.u32()? as usize;
                    if len > MAX_USB_DEVICE_SNAPSHOT_BYTES {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "usb device snapshot too large",
                        ));
                    }
                    Some(pd.bytes(len)?)
                } else {
                    None
                };
                pd.finish()?;

                if let Some(state) = device_state {
                    if port.device.is_none() {
                        if let Some(dev) = AttachedUsbDevice::try_new_from_snapshot(state)? {
                            port.device = Some(dev);
                        }
                    }
                    if let Some(dev) = port.device.as_mut() {
                        dev.load_state(state)?;
                    }
                } else {
                    // Snapshot indicates no device attached.
                    port.device = None;
                }

                // Ensure the device model observes the restored port suspended state.
                port.propagate_suspended_state();

                Ok(())
            }
            RootHubPortSlot::Usb2Mux { mux, port } => mux
                .borrow_mut()
                .load_snapshot_ehci_port_record(*port, buf),
        }
    }
}

/// EHCI root hub exposed via PORTSC registers.
///
/// This is a minimal model intended for Windows driver bring-up. By default we expose 6 ports
/// (`EhciController::new()`); use `EhciController::new_with_port_count()` to override.
pub struct RootHub {
    ports: Vec<RootHubPortSlot>,
}

impl RootHub {
    pub fn new(num_ports: usize) -> Self {
        let ports = (0..num_ports)
            .map(|_| RootHubPortSlot::Local(Port::new()))
            .collect();
        Self { ports }
    }

    pub fn num_ports(&self) -> usize {
        self.ports.len()
    }

    /// Replaces `root_port` backing storage with a shared USB2 mux port.
    pub fn attach_usb2_port_mux(
        &mut self,
        root_port: usize,
        mux: Rc<RefCell<Usb2PortMux>>,
        mux_port: usize,
    ) {
        if root_port >= self.ports.len() {
            return;
        }
        self.ports[root_port] = RootHubPortSlot::Usb2Mux {
            mux,
            port: mux_port,
        };
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
            if self.ports[root_port].has_device() {
                return Err(UsbHubAttachError::PortOccupied);
            }
            self.attach(root_port, model);
            return Ok(());
        }

        let Some(mut root_dev) = self.port_device_mut(root_port) else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev: &mut AttachedUsbDevice = &mut root_dev;
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
            if !self.ports[root_port].has_device() {
                return Err(UsbHubAttachError::NoDevice);
            }
            self.detach(root_port);
            return Ok(());
        }

        let Some(mut root_dev) = self.port_device_mut(root_port) else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev: &mut AttachedUsbDevice = &mut root_dev;
        for &hop in hub_path {
            hub_dev = hub_dev.model_mut().hub_port_device_mut(hop)?;
        }
        hub_dev.model_mut().hub_detach_device(leaf_port)
    }

    /// Returns the device currently attached to the specified root port, regardless of whether the
    /// port is powered/enabled.
    pub fn port_device(&self, port: usize) -> Option<RootHubDevice<'_>> {
        self.ports.get(port)?.port_device()
    }

    /// Mutable variant of [`RootHub::port_device`].
    pub fn port_device_mut(&mut self, port: usize) -> Option<RootHubDeviceMut<'_>> {
        self.ports.get_mut(port)?.port_device_mut()
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

    pub(crate) fn set_configflag(&mut self, configflag: bool) {
        for p in &mut self.ports {
            if let RootHubPortSlot::Usb2Mux { mux, .. } = p {
                mux.borrow_mut().set_configflag(configflag);
            }
        }
    }

    pub(crate) fn set_configflag_for_restore(&mut self, configflag: bool) {
        for p in &mut self.ports {
            if let RootHubPortSlot::Usb2Mux { mux, .. } = p {
                mux.borrow_mut().set_configflag_for_restore(configflag);
            }
        }
    }

    pub fn write_portsc(&mut self, port: usize, value: u32) {
        self.write_portsc_masked(port, value, 0xffff_ffff);
    }

    pub(crate) fn write_portsc_masked(&mut self, port: usize, value: u32, write_mask: u32) {
        if let Some(p) = self.ports.get_mut(port) {
            p.write_portsc_masked(value, write_mask);
        }
    }

    pub fn tick_1ms(&mut self) {
        for p in &mut self.ports {
            match p {
                RootHubPortSlot::Local(local) => {
                    local.tick_1ms();

                    if !(local.enabled && local.powered) || local.suspended || local.resuming {
                        continue;
                    }
                    if let Some(dev) = local.device.as_mut() {
                        dev.tick_1ms();
                    }
                }
                RootHubPortSlot::Usb2Mux { mux, port } => {
                    mux.borrow_mut().ehci_tick_1ms(*port);
                }
            }
        }
    }

    pub fn any_port_change(&self) -> bool {
        self.ports.iter().any(|p| p.any_port_change())
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<RootHubDeviceMut<'_>> {
        for p in &mut self.ports {
            match p {
                RootHubPortSlot::Local(port) => {
                    if port.port_owner || !(port.enabled && port.powered) || port.suspended || port.resuming {
                        continue;
                    }
                    if let Some(dev) = port.device.as_mut() {
                        if let Some(found) = dev.device_mut_for_address(address) {
                            return Some(RootHubDeviceMut::Direct(found));
                        }
                    }
                }
                RootHubPortSlot::Usb2Mux { mux, port } => {
                    let mut mux_ref = mux.borrow_mut();
                    if !mux_ref.ehci_port_routable(*port) {
                        continue;
                    }

                    // Ensure device exists and address is reachable before mapping the RefMut.
                    {
                        let Some(root_dev) = mux_ref.port_device_mut(*port) else {
                            continue;
                        };
                        if root_dev.device_mut_for_address(address).is_none() {
                            continue;
                        }
                    }

                    let root_ref = RefMut::map(mux_ref, |m| {
                        m.port_device_mut(*port)
                            .expect("checked device exists above")
                    });
                    let found_ref = RefMut::map(root_ref, |dev| {
                        dev.device_mut_for_address(address)
                            .expect("checked address exists above")
                    });
                    return Some(RootHubDeviceMut::Muxed(found_ref));
                }
            }
        }
        None
    }

    pub(crate) fn save_snapshot_ports(&self) -> Vec<u8> {
        let mut port_records = Vec::with_capacity(self.ports.len());
        for port in &self.ports {
            port_records.push(port.save_snapshot_record());
        }

        Encoder::new().vec_bytes(&port_records).finish()
    }

    pub(crate) fn load_snapshot_ports(&mut self, buf: &[u8]) -> SnapshotResult<()> {
        let mut d = Decoder::new(buf);
        let count = d.u32()? as usize;
        if count != self.ports.len() {
            return Err(SnapshotError::InvalidFieldEncoding("root hub ports"));
        }

        for port in &mut self.ports {
            let len = d.u32()? as usize;
            let rec = d.bytes(len)?;
            port.load_snapshot_record(rec)?;
        }
        d.finish()?;

        Ok(())
    }
}

impl Default for RootHub {
    fn default() -> Self {
        Self::new(6)
    }
}
