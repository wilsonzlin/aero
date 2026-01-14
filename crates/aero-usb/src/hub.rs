use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use core::cell::{Ref, RefCell, RefMut};
use core::ops::{Deref, DerefMut};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::{AttachedUsbDevice, UsbInResult};
use crate::usb2_port::Usb2PortMux;
use crate::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
    UsbHubAttachError, UsbSpeed,
};

/// Object-safe traversal interface for USB hubs.
///
/// External hub device models implement this trait and expose it via
/// [`UsbDeviceModel::as_hub`] / [`UsbDeviceModel::as_hub_mut`]. The UHCI schedule walker then
/// resolves device addresses by recursively walking through hub topology.
pub trait UsbHub {
    /// Advances hub internal time by 1ms.
    ///
    /// Hub implementations should update any pending port reset timers and recurse into nested hubs
    /// so time-based events propagate down the topology.
    fn tick_1ms(&mut self);

    /// Returns a mutable reference to a reachable downstream device with the given USB address.
    ///
    /// Implementations should only consider devices behind ports that are connected and enabled
    /// (and powered, if modelled), and should treat suspended ports as unreachable until resumed.
    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice>;

    /// Returns the device currently attached to `port`, if any.
    ///
    /// This accessor is used by topology configuration helpers (e.g. attaching devices behind
    /// nested hubs) and does not need to apply reachability rules.
    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice>;

    /// Attaches a new device model to the given downstream port.
    fn attach_downstream(&mut self, port: usize, model: Box<dyn UsbDeviceModel>);

    /// Detaches the device (if any) from the given downstream port.
    fn detach_downstream(&mut self, port: usize);

    /// Number of downstream ports on this hub.
    fn num_ports(&self) -> usize;
}

const MAX_USB_DEVICE_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
#[cfg(test)]
mod remote_wakeup_tests;
#[cfg(test)]
mod reset_tests;

struct Port {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    resume_detect: bool,
    reset: bool,
    reset_countdown_ms: u8,
    suspended: bool,
    resuming: bool,
    resume_countdown_ms: u8,
}

impl Port {
    fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_change: false,
            enabled: false,
            enable_change: false,
            resume_detect: false,
            reset: false,
            reset_countdown_ms: 0,
            suspended: false,
            resuming: false,
            resume_countdown_ms: 0,
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

    fn read_portsc(&self) -> u16 {
        const CCS: u16 = 1 << 0;
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const LS_J_FS: u16 = 0b01 << 4;
        const LS_K_FS: u16 = 0b10 << 4;
        const RD: u16 = 1 << 6;
        const LSDA: u16 = 1 << 8;
        const PR: u16 = 1 << 9;
        const SUSP: u16 = 1 << 12;
        const RESUME: u16 = 1 << 13;

        let mut v = 0u16;
        if self.connected {
            v |= CCS;
            if !self.reset {
                if self.resuming {
                    v |= LS_K_FS;
                } else {
                    v |= LS_J_FS;
                }
            }
        }
        if self.connect_change {
            v |= CSC;
        }
        if self.enabled {
            v |= PED;
        }
        if self.enable_change {
            v |= PEDC;
        }
        if self.resume_detect {
            v |= RD;
        }
        if let Some(dev) = self.device.as_ref() {
            match dev.speed() {
                UsbSpeed::Low => v |= LSDA,
                // UHCI is USB 1.1 (low/full speed). The root hub PORTSC LSDA bit only reports
                // low-speed; treat high-speed devices as full-speed here.
                UsbSpeed::Full | UsbSpeed::High => {}
            }
        }
        if self.reset {
            v |= PR;
        }
        if self.suspended {
            v |= SUSP;
        }
        if self.resuming {
            v |= RESUME;
        }
        v
    }

    fn write_portsc(&mut self, value: u16, write_mask: u16) {
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const RD: u16 = 1 << 6;
        const PR: u16 = 1 << 9;
        const SUSP: u16 = 1 << 12;
        const RESUME: u16 = 1 << 13;

        // Write-1-to-clear status change bits.
        if write_mask & CSC != 0 && value & CSC != 0 {
            self.connect_change = false;
        }
        if write_mask & PEDC != 0 && value & PEDC != 0 {
            self.enable_change = false;
        }
        // Resume Detect is a latched status bit (remote wake). Model it as W1C so tests can
        // manipulate it without needing a full remote-wakeup implementation.
        if write_mask & RD != 0 && value & RD != 0 {
            self.resume_detect = false;
        }

        // Port reset: model a 50ms reset and reset attached device state.
        if write_mask & PR != 0 && value & PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;
            self.resume_detect = false;
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

        // Port enable (read/write).
        if write_mask & PED != 0 {
            let want_enabled = value & PED != 0;
            // Hardware only allows enabling a port when a device is actually present.
            if want_enabled {
                if self.connected && !self.enabled {
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

        if !self.connected {
            self.set_suspended(false);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            return;
        }

        if write_mask & SUSP != 0 {
            let want_suspended = value & SUSP != 0;
            // Latch the suspend bit. While a port is enabled, we treat this as "suspended" for
            // reachability and ticking purposes.
            if want_suspended {
                if !self.resuming {
                    self.set_suspended(true);
                }
            } else {
                self.set_suspended(false);
            }
        }

        if write_mask & RESUME != 0 {
            let want_resuming = value & RESUME != 0;
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
                if self.connected && !self.enabled {
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

/// Immutable reference to an [`AttachedUsbDevice`] that may be stored either directly in the root
/// hub or behind a shared USB 2.0 port mux (`RefCell`).
pub enum RootHubDevice<'a> {
    Direct(&'a AttachedUsbDevice),
    Muxed(Ref<'a, AttachedUsbDevice>),
}

impl Deref for RootHubDevice<'_> {
    type Target = AttachedUsbDevice;

    fn deref(&self) -> &Self::Target {
        match self {
            RootHubDevice::Direct(dev) => dev,
            RootHubDevice::Muxed(dev) => dev.deref(),
        }
    }
}

/// Mutable reference to an [`AttachedUsbDevice`] that may be stored either directly in the root
/// hub or behind a shared USB 2.0 port mux (`RefCell`).
pub enum RootHubDeviceMut<'a> {
    Direct(&'a mut AttachedUsbDevice),
    Muxed(RefMut<'a, AttachedUsbDevice>),
}

impl Deref for RootHubDeviceMut<'_> {
    type Target = AttachedUsbDevice;

    fn deref(&self) -> &Self::Target {
        match self {
            RootHubDeviceMut::Direct(dev) => dev,
            RootHubDeviceMut::Muxed(dev) => dev.deref(),
        }
    }
}

impl DerefMut for RootHubDeviceMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            RootHubDeviceMut::Direct(dev) => dev,
            RootHubDeviceMut::Muxed(dev) => dev.deref_mut(),
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

    fn read_portsc(&self) -> u16 {
        match self {
            RootHubPortSlot::Local(p) => p.read_portsc(),
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow().uhci_read_portsc(*port),
        }
    }

    fn write_portsc_masked(&mut self, value: u16, write_mask: u16) {
        match self {
            RootHubPortSlot::Local(p) => p.write_portsc(value, write_mask),
            RootHubPortSlot::Usb2Mux { mux, port } => mux
                .borrow_mut()
                .uhci_write_portsc_masked(*port, value, write_mask),
        }
    }

    fn bus_reset(&mut self) {
        match self {
            RootHubPortSlot::Local(p) => {
                p.resume_detect = false;
                p.set_suspended(false);
                p.resuming = false;
                p.resume_countdown_ms = 0;
                if let Some(dev) = p.device.as_mut() {
                    dev.reset();
                }
            }
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow_mut().uhci_bus_reset(*port),
        }
    }

    fn attach(&mut self, model: Box<dyn UsbDeviceModel>) {
        match self {
            RootHubPortSlot::Local(p) => {
                p.device = Some(AttachedUsbDevice::new(model));
                p.resume_detect = false;
                p.set_suspended(false);
                p.resuming = false;
                p.resume_countdown_ms = 0;
                if !p.connected {
                    p.connected = true;
                }
                p.connect_change = true;
                if p.enabled {
                    p.enabled = false;
                    p.enable_change = true;
                }
            }
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow_mut().attach(*port, model),
        }
    }

    fn detach(&mut self) {
        match self {
            RootHubPortSlot::Local(p) => {
                p.device = None;
                p.resume_detect = false;
                p.set_suspended(false);
                p.resuming = false;
                p.resume_countdown_ms = 0;
                if p.connected {
                    p.connected = false;
                    p.connect_change = true;
                }
                if p.enabled {
                    p.enabled = false;
                    p.enable_change = true;
                }
            }
            RootHubPortSlot::Usb2Mux { mux, port } => mux.borrow_mut().detach(*port),
        }
    }

    fn port_device(&self) -> Option<RootHubDevice<'_>> {
        match self {
            RootHubPortSlot::Local(p) => p.device.as_ref().map(RootHubDevice::Direct),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let mux_ref = mux.borrow();
                let dev_ref = Ref::filter_map(mux_ref, |m| m.port_device(*port)).ok()?;
                Some(RootHubDevice::Muxed(dev_ref))
            }
        }
    }

    fn port_device_mut(&mut self) -> Option<RootHubDeviceMut<'_>> {
        match self {
            RootHubPortSlot::Local(p) => p.device.as_mut().map(RootHubDeviceMut::Direct),
            RootHubPortSlot::Usb2Mux { mux, port } => {
                let mux_ref = mux.borrow_mut();
                let dev_ref = RefMut::filter_map(mux_ref, |m| m.port_device_mut(*port)).ok()?;
                Some(RootHubDeviceMut::Muxed(dev_ref))
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
                    .bool(port.resume_detect)
                    .bool(port.reset)
                    .u8(port.reset_countdown_ms)
                    .bool(port.suspended)
                    .bool(port.resuming)
                    .u8(port.resume_countdown_ms)
                    .bool(port.device.is_some());

                if let Some(dev) = port.device.as_ref() {
                    let dev_state = dev.save_state();
                    rec = rec.u32(dev_state.len() as u32).bytes(&dev_state);
                }

                rec.finish()
            }
            RootHubPortSlot::Usb2Mux { mux, port } => {
                mux.borrow().save_snapshot_uhci_port_record(*port)
            }
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
                port.resume_detect = pd.bool()?;
                port.reset = pd.bool()?;
                port.reset_countdown_ms = pd.u8()?;
                port.suspended = pd.bool()?;
                port.resuming = pd.bool()?;
                port.resume_countdown_ms = pd.u8()?;
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

                if let Some(device_state) = device_state {
                    if let Some(dev) = port.device.as_mut() {
                        dev.load_state(device_state)?;
                    } else if let Some(mut dev) =
                        AttachedUsbDevice::try_new_from_snapshot(device_state)?
                    {
                        // `try_new_from_snapshot` only chooses the concrete device model; the rest
                        // of the device wrapper state (address, pending control transfer, model
                        // internals, etc.) still needs to be loaded.
                        dev.load_state(device_state)?;
                        port.device = Some(dev);
                    }
                } else {
                    // Snapshot indicates no device attached.
                    port.device = None;
                }

                // Ensure the device model observes the restored suspended state.
                if let Some(dev) = port.device.as_mut() {
                    dev.model_mut().set_suspended(port.suspended);
                }

                Ok(())
            }
            RootHubPortSlot::Usb2Mux { mux, port } => {
                mux.borrow_mut().load_snapshot_uhci_port_record(*port, buf)
            }
        }
    }
}

/// UHCI "root hub" exposed via PORTSC registers.
pub struct RootHub {
    ports: [RootHubPortSlot; 2],
}

impl RootHub {
    pub fn new() -> Self {
        Self {
            ports: [
                RootHubPortSlot::Local(Port::new()),
                RootHubPortSlot::Local(Port::new()),
            ],
        }
    }

    /// Replaces `root_port` backing storage with a shared USB2 mux port.
    ///
    /// This allows an UHCI controller instance to act as an EHCI companion controller for shared
    /// physical ports.
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

    pub fn bus_reset(&mut self) {
        for p in &mut self.ports {
            p.bus_reset();
        }
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
        };

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
        };

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
    /// port is enabled/suspended.
    ///
    /// This is intended for host-side introspection (snapshotting, action draining, etc). Guest
    /// reachability is handled by [`RootHub::device_mut_for_address`].
    pub fn port_device(&self, port: usize) -> Option<RootHubDevice<'_>> {
        self.ports.get(port)?.port_device()
    }

    /// Mutable variant of [`RootHub::port_device`].
    pub fn port_device_mut(&mut self, port: usize) -> Option<RootHubDeviceMut<'_>> {
        self.ports.get_mut(port)?.port_device_mut()
    }

    pub fn read_portsc(&self, port: usize) -> u16 {
        self.ports[port].read_portsc()
    }

    pub fn write_portsc(&mut self, port: usize, value: u16) {
        self.write_portsc_masked(port, value, 0xffff);
    }

    pub(crate) fn write_portsc_masked(&mut self, port: usize, value: u16, write_mask: u16) {
        self.ports[port].write_portsc_masked(value, write_mask);
    }

    pub fn tick_1ms(&mut self) {
        for p in &mut self.ports {
            match p {
                RootHubPortSlot::Local(local) => {
                    local.tick_1ms();
                    if local.enabled && local.suspended && !local.resuming {
                        if let Some(dev) = local.device.as_mut() {
                            if dev.model_mut().poll_remote_wakeup() {
                                local.resume_detect = true;
                            }
                        }
                    }

                    if !local.enabled || local.suspended || local.resuming {
                        continue;
                    }
                    if let Some(dev) = local.device.as_mut() {
                        dev.tick_1ms();
                    }
                }
                RootHubPortSlot::Usb2Mux { mux, port } => {
                    mux.borrow_mut().uhci_tick_1ms(*port);
                }
            }
        }
    }

    pub fn force_enable_for_tests(&mut self, port: usize) {
        match &mut self.ports[port] {
            RootHubPortSlot::Local(p) => {
                p.enabled = true;
                p.enable_change = true;
                p.set_suspended(false);
                p.resuming = false;
                p.resume_countdown_ms = 0;
            }
            RootHubPortSlot::Usb2Mux { mux, port } => {
                mux.borrow_mut().uhci_force_enable_for_tests(*port);
            }
        }
    }

    pub fn force_resume_detect_for_tests(&mut self, port: usize) {
        match &mut self.ports[port] {
            RootHubPortSlot::Local(p) => p.resume_detect = true,
            RootHubPortSlot::Usb2Mux { mux, port } => {
                mux.borrow_mut().uhci_force_resume_detect_for_tests(*port);
            }
        }
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<RootHubDeviceMut<'_>> {
        for p in &mut self.ports {
            match p {
                RootHubPortSlot::Local(port) => {
                    if !port.enabled || port.suspended || port.resuming {
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
                    if !mux_ref.uhci_port_routable(*port) {
                        continue;
                    }

                    // Ensure the device exists and the address is reachable before mapping the
                    // RefMut to the nested device reference.
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
}

impl Default for RootHub {
    fn default() -> Self {
        Self::new()
    }
}

impl RootHub {
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

const USB_DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_STRING: u8 = 0x03;
const USB_DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
const USB_DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;
const USB_DESCRIPTOR_TYPE_HUB: u8 = 0x29;

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_REQUEST_GET_CONFIGURATION: u8 = 0x08;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;
const USB_REQUEST_GET_INTERFACE: u8 = 0x0a;
const USB_REQUEST_SET_INTERFACE: u8 = 0x0b;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0;
const USB_FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 1;

const HUB_FEATURE_C_HUB_LOCAL_POWER: u16 = 0;
const HUB_FEATURE_C_HUB_OVER_CURRENT: u16 = 1;

const HUB_PORT_FEATURE_ENABLE: u16 = 1;
const HUB_PORT_FEATURE_SUSPEND: u16 = 2;
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_CONNECTION: u16 = 16;
const HUB_PORT_FEATURE_C_PORT_ENABLE: u16 = 17;
const HUB_PORT_FEATURE_C_PORT_SUSPEND: u16 = 18;
const HUB_PORT_FEATURE_C_PORT_OVER_CURRENT: u16 = 19;
const HUB_PORT_FEATURE_C_PORT_RESET: u16 = 20;

const HUB_PORT_STATUS_CONNECTION: u16 = 1 << 0;
const HUB_PORT_STATUS_ENABLE: u16 = 1 << 1;
const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
const HUB_PORT_STATUS_RESET: u16 = 1 << 4;
const HUB_PORT_STATUS_POWER: u16 = 1 << 8;

const HUB_PORT_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_PORT_CHANGE_ENABLE: u16 = 1 << 1;
const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;
const HUB_PORT_CHANGE_RESET: u16 = 1 << 4;

const HUB_INTERRUPT_IN_EP: u8 = 0x81;

const DEFAULT_HUB_NUM_PORTS: usize = 4;

struct HubPort {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    suspended: bool,
    suspend_change: bool,
    powered: bool,
    reset: bool,
    reset_countdown_ms: u8,
    reset_change: bool,
}

impl HubPort {
    fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_change: false,
            enabled: false,
            enable_change: false,
            suspended: false,
            suspend_change: false,
            powered: false,
            reset: false,
            reset_countdown_ms: 0,
            reset_change: false,
        }
    }

    fn attach(&mut self, model: Box<dyn UsbDeviceModel>) {
        self.device = Some(AttachedUsbDevice::new(model));
        self.connected = true;
        self.connect_change = true;
        self.suspended = false;
        self.suspend_change = false;
        self.set_enabled(false);
    }

    fn detach(&mut self) {
        self.device = None;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
        self.suspended = false;
        self.suspend_change = false;
        self.set_enabled(false);
    }

    fn set_powered(&mut self, powered: bool) {
        if powered == self.powered {
            return;
        }
        self.powered = powered;
        if !self.powered {
            // Losing VBUS effectively power-cycles the downstream device.
            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }
            self.suspended = false;
            self.suspend_change = false;
            self.set_enabled(false);
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            if !(self.powered && self.connected) {
                return;
            }
            if self.reset {
                return;
            }
        }

        if enabled != self.enabled {
            self.enabled = enabled;
            self.enable_change = true;
        }

        if !self.enabled {
            self.suspended = false;
            self.suspend_change = false;
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if suspended {
            if !(self.enabled && self.powered && self.connected) {
                return;
            }
            if self.reset {
                return;
            }
        }

        if suspended != self.suspended {
            self.suspended = suspended;
            self.suspend_change = true;
        }
    }

    fn start_reset(&mut self) {
        if self.reset {
            return;
        }
        self.reset = true;
        self.reset_countdown_ms = 50;
        self.reset_change = false;

        if let Some(dev) = self.device.as_mut() {
            dev.reset();
        }

        self.suspended = false;
        self.suspend_change = false;

        if self.enabled {
            self.set_enabled(false);
        }
    }

    fn tick_1ms(&mut self) {
        if self.reset {
            self.reset_countdown_ms = self.reset_countdown_ms.saturating_sub(1);
            if self.reset_countdown_ms == 0 {
                self.reset = false;
                self.reset_change = true;

                let should_enable = self.powered && self.connected;
                if should_enable && !self.enabled {
                    self.set_enabled(true);
                }
            }
        }
    }

    fn port_status(&self) -> u16 {
        let mut st = 0u16;
        if self.connected {
            st |= HUB_PORT_STATUS_CONNECTION;
        }
        if self.enabled {
            st |= HUB_PORT_STATUS_ENABLE;
        }
        if self.suspended {
            st |= HUB_PORT_STATUS_SUSPEND;
        }
        if self.reset {
            st |= HUB_PORT_STATUS_RESET;
        }
        if self.powered {
            st |= HUB_PORT_STATUS_POWER;
        }
        st
    }

    fn port_change(&self) -> u16 {
        let mut ch = 0u16;
        if self.connect_change {
            ch |= HUB_PORT_CHANGE_CONNECTION;
        }
        if self.enable_change {
            ch |= HUB_PORT_CHANGE_ENABLE;
        }
        if self.suspend_change {
            ch |= HUB_PORT_CHANGE_SUSPEND;
        }
        if self.reset_change {
            ch |= HUB_PORT_CHANGE_RESET;
        }
        ch
    }

    fn has_change(&self) -> bool {
        self.connect_change || self.enable_change || self.suspend_change || self.reset_change
    }
}

/// External USB 1.1 hub device (class 0x09).
///
/// This is a USB device which, once enumerated, exposes downstream ports and forwards packets
/// based on the downstream device address. The UHCI controller uses topology-aware routing to
/// locate devices attached behind hubs.
pub struct UsbHubDevice {
    configuration: u8,
    remote_wakeup_enabled: bool,
    upstream_suspended: bool,
    ports: Vec<HubPort>,
    interrupt_bitmap_len: usize,
    interrupt_ep_halted: bool,
    config_descriptor: Vec<u8>,
    hub_descriptor: Vec<u8>,
}

impl UsbHubDevice {
    pub fn new() -> Self {
        Self::new_with_ports(DEFAULT_HUB_NUM_PORTS)
    }

    pub fn with_port_count(num_ports: u8) -> Self {
        Self::new_with_ports(num_ports as usize)
    }

    pub fn new_with_ports(num_ports: usize) -> Self {
        assert!(
            (1..=u8::MAX as usize).contains(&num_ports),
            "hub port count must be 1..=255"
        );

        let interrupt_bitmap_len = hub_bitmap_len(num_ports);
        Self {
            configuration: 0,
            remote_wakeup_enabled: false,
            upstream_suspended: false,
            ports: (0..num_ports).map(|_| HubPort::new()).collect(),
            interrupt_bitmap_len,
            interrupt_ep_halted: false,
            config_descriptor: build_hub_config_descriptor(interrupt_bitmap_len),
            hub_descriptor: build_hub_descriptor(num_ports),
        }
    }

    pub fn attach(&mut self, port: u8, model: Box<dyn UsbDeviceModel>) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if idx >= self.ports.len() {
            return;
        }
        let upstream_suspended = self.upstream_suspended;
        let port = &mut self.ports[idx];
        port.attach(model);

        // Ensure newly attached devices observe the current upstream+port suspended state.
        //
        // This matters for host-side hotplug while the upstream link is suspended: device models
        // (especially HID) should only request remote wakeup while suspended.
        if let Some(dev) = port.device.as_mut() {
            dev.model_mut()
                .set_suspended(upstream_suspended || port.suspended);
        }
    }

    #[allow(dead_code)]
    pub fn detach(&mut self, port: u8) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if idx >= self.ports.len() {
            return;
        }
        self.ports[idx].detach();
    }

    fn port_mut(&mut self, port: u16) -> Option<&mut HubPort> {
        if port == 0 {
            return None;
        }
        let idx = (port - 1) as usize;
        self.ports.get_mut(idx)
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB Hub")),
            _ => None,
        }
    }

    fn poll_remote_wakeup_internal(&mut self) -> bool {
        if self.configuration == 0 {
            return false;
        }

        if !self.upstream_suspended {
            return false;
        }

        // If the upstream link is suspended and a downstream device requests remote wakeup, the
        // hub may propagate the resume event upstream when DEVICE_REMOTE_WAKEUP is enabled on the
        // hub itself.
        //
        // We still drain downstream remote wake signals even when propagation is disabled to avoid
        // "replaying" stale wake events if the guest later enables remote wakeup without an
        // additional user action.
        let mut wake_requested = false;
        for port in &mut self.ports {
            if !(port.enabled && port.powered) {
                continue;
            }
            let wake = match port.device.as_mut() {
                Some(dev) => dev.model_mut().poll_remote_wakeup(),
                None => false,
            };
            if !wake {
                continue;
            }
            wake_requested = true;

            // If the downstream port was selectively suspended, remote wake should resume it so
            // that the device is active once the upstream link resumes. Only apply this when the
            // wake will actually propagate upstream.
            if self.remote_wakeup_enabled && port.suspended {
                port.set_suspended(false);
            }
        }

        wake_requested && self.remote_wakeup_enabled
    }
}

impl Default for UsbHubDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for UsbHubDevice {
    const DEVICE_ID: [u8; 4] = *b"UHUB";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_CONFIGURATION: u16 = 1;
        const TAG_REMOTE_WAKEUP: u16 = 2;
        const TAG_UPSTREAM_SUSPENDED: u16 = 3;
        const TAG_INTERRUPT_HALTED: u16 = 4;
        const TAG_NUM_PORTS: u16 = 5;
        const TAG_PORTS: u16 = 6;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u8(TAG_CONFIGURATION, self.configuration);
        w.field_bool(TAG_REMOTE_WAKEUP, self.remote_wakeup_enabled);
        w.field_bool(TAG_UPSTREAM_SUSPENDED, self.upstream_suspended);
        w.field_bool(TAG_INTERRUPT_HALTED, self.interrupt_ep_halted);
        w.field_u32(TAG_NUM_PORTS, self.ports.len() as u32);

        let mut port_records = Vec::with_capacity(self.ports.len());
        for port in &self.ports {
            let mut rec = Encoder::new()
                .bool(port.connected)
                .bool(port.connect_change)
                .bool(port.enabled)
                .bool(port.enable_change)
                .bool(port.suspended)
                .bool(port.suspend_change)
                .bool(port.powered)
                .bool(port.reset)
                .u8(port.reset_countdown_ms)
                .bool(port.reset_change)
                .bool(port.device.is_some());

            if let Some(dev) = port.device.as_ref() {
                let dev_state = dev.save_state();
                rec = rec.u32(dev_state.len() as u32).bytes(&dev_state);
            }

            port_records.push(rec.finish());
        }
        w.field_bytes(TAG_PORTS, Encoder::new().vec_bytes(&port_records).finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_CONFIGURATION: u16 = 1;
        const TAG_REMOTE_WAKEUP: u16 = 2;
        const TAG_UPSTREAM_SUSPENDED: u16 = 3;
        const TAG_INTERRUPT_HALTED: u16 = 4;
        const TAG_NUM_PORTS: u16 = 5;
        const TAG_PORTS: u16 = 6;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset hub-local control state, but preserve any attached downstream devices.
        self.configuration = 0;
        self.remote_wakeup_enabled = false;
        self.upstream_suspended = false;
        self.interrupt_ep_halted = false;

        if let Some(num_ports) = r.u32(TAG_NUM_PORTS)? {
            if num_ports as usize != self.ports.len() {
                return Err(SnapshotError::InvalidFieldEncoding("hub port count"));
            }
        }

        let configuration = r.u8(TAG_CONFIGURATION)?.unwrap_or(0);
        // Hubs only expose a single configuration value (1); treat any non-zero snapshot value as
        // configured to preserve older snapshots while rejecting impossible values.
        self.configuration = if configuration == 0 { 0 } else { 1 };
        self.remote_wakeup_enabled = r.bool(TAG_REMOTE_WAKEUP)?.unwrap_or(false);
        self.upstream_suspended = r.bool(TAG_UPSTREAM_SUSPENDED)?.unwrap_or(false);
        self.interrupt_ep_halted = r.bool(TAG_INTERRUPT_HALTED)?.unwrap_or(false);

        if let Some(buf) = r.bytes(TAG_PORTS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count != self.ports.len() {
                return Err(SnapshotError::InvalidFieldEncoding("hub ports"));
            }

            for port in &mut self.ports {
                let rec_len = d.u32()? as usize;
                let rec = d.bytes(rec_len)?;
                let mut pd = Decoder::new(rec);
                port.connected = pd.bool()?;
                port.connect_change = pd.bool()?;
                port.enabled = pd.bool()?;
                port.enable_change = pd.bool()?;
                port.suspended = pd.bool()?;
                port.suspend_change = pd.bool()?;
                port.powered = pd.bool()?;
                port.reset = pd.bool()?;
                port.reset_countdown_ms = pd.u8()?;
                port.reset_change = pd.bool()?;
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
            }
            d.finish()?;
        }

        // Ensure downstream device models observe restored upstream/port suspend state.
        //
        // Some device models (e.g. host-integrated passthrough devices) are not snapshot-capable,
        // so their `suspended` state must be re-derived from hub/port state during restore.
        self.set_suspended(self.upstream_suspended);

        Ok(())
    }
}

impl UsbDeviceModel for UsbHubDevice {
    fn reset(&mut self) {
        self.configuration = 0;
        self.remote_wakeup_enabled = false;
        self.upstream_suspended = false;
        self.interrupt_ep_halted = false;

        for port in &mut self.ports {
            let was_enabled = port.enabled;

            // Preserve physical attachment, but clear any host-visible state that is invalidated
            // by an upstream bus reset.
            port.connected = port.device.is_some();
            port.connect_change = port.connected;

            port.enabled = false;
            // An upstream bus reset disables downstream ports; flag a change if the port was
            // previously enabled so the host can notice once the hub is reconfigured.
            port.enable_change = was_enabled;
            port.suspended = false;
            port.suspend_change = false;
            port.reset = false;
            port.reset_countdown_ms = 0;
            port.reset_change = false;
            if let Some(dev) = port.device.as_mut() {
                dev.reset();
                // A bus reset exits suspend. Ensure the downstream device model observes the
                // cleared suspend state even if its `reset` implementation is a no-op.
                dev.model_mut().set_suspended(false);
            }
        }
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    // USB 2.0 spec 9.4.5: bit1 is Remote Wakeup.
                    let status: u16 = u16::from(self.remote_wakeup_enabled) << 1;
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value != USB_FEATURE_DEVICE_REMOTE_WAKEUP {
                        return ControlResponse::Stall;
                    }
                    self.remote_wakeup_enabled = false;
                    ControlResponse::Ack
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value != USB_FEATURE_DEVICE_REMOTE_WAKEUP {
                        return ControlResponse::Stall;
                    }
                    self.remote_wakeup_enabled = true;
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let desc_index = setup.descriptor_index();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(HUB_DEVICE_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(self.config_descriptor.clone()),
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        // Accept hub descriptor fetch as both a class request (the common case) and
                        // a standard request. Some host stacks probe descriptor type 0x29 using a
                        // standard GET_DESCRIPTOR despite it being class-specific.
                        USB_DESCRIPTOR_TYPE_HUB => Some(self.hub_descriptor.clone()),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    self.configuration = config;
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Interface) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    // Hub has a single interface (0). Interface GET_STATUS has no defined flags.
                    if setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                }
                USB_REQUEST_GET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0], setup.w_length))
                }
                USB_REQUEST_SET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != 0 || setup.w_value != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Ack
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Endpoint) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if ep != HUB_INTERRUPT_IN_EP {
                        return ControlResponse::Stall;
                    }
                    let status: u16 = u16::from(self.interrupt_ep_halted);
                    ControlResponse::Data(clamp_response(
                        status.to_le_bytes().to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_CLEAR_FEATURE | USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value != USB_FEATURE_ENDPOINT_HALT {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if ep != HUB_INTERRUPT_IN_EP {
                        return ControlResponse::Stall;
                    }
                    self.interrupt_ep_halted = setup.b_request == USB_REQUEST_SET_FEATURE;
                    ControlResponse::Ack
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.descriptor_type() != USB_DESCRIPTOR_TYPE_HUB {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(
                        self.hub_descriptor.clone(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0, 0, 0], setup.w_length))
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    match setup.w_value {
                        HUB_FEATURE_C_HUB_LOCAL_POWER | HUB_FEATURE_C_HUB_OVER_CURRENT => {
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Other) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    let st = port.port_status().to_le_bytes();
                    let ch = port.port_change().to_le_bytes();
                    ControlResponse::Data(clamp_response(
                        vec![st[0], st[1], ch[0], ch[1]],
                        setup.w_length,
                    ))
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let upstream_suspended = self.upstream_suspended;
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    match setup.w_value {
                        HUB_PORT_FEATURE_ENABLE => {
                            port.set_enabled(true);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_SUSPEND => {
                            port.set_suspended(true);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_POWER => {
                            port.set_powered(true);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_RESET => {
                            port.start_reset();
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let upstream_suspended = self.upstream_suspended;
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    match setup.w_value {
                        HUB_PORT_FEATURE_ENABLE => {
                            port.set_enabled(false);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_SUSPEND => {
                            port.set_suspended(false);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_POWER => {
                            port.set_powered(false);
                            if let Some(dev) = port.device.as_mut() {
                                dev.model_mut()
                                    .set_suspended(upstream_suspended || port.suspended);
                            }
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_CONNECTION => {
                            port.connect_change = false;
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_ENABLE => {
                            port.enable_change = false;
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_SUSPEND => {
                            port.suspend_change = false;
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_OVER_CURRENT => ControlResponse::Ack,
                        HUB_PORT_FEATURE_C_PORT_RESET => {
                            port.reset_change = false;
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                _ => ControlResponse::Stall,
            },
            _ => ControlResponse::Stall,
        }
    }

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        if ep_addr != HUB_INTERRUPT_IN_EP {
            return UsbInResult::Stall;
        }
        if self.configuration == 0 {
            return UsbInResult::Nak;
        }
        if self.interrupt_ep_halted {
            return UsbInResult::Stall;
        }

        let mut bitmap = vec![0u8; self.interrupt_bitmap_len];
        for (idx, port) in self.ports.iter().enumerate() {
            if !port.has_change() {
                continue;
            }
            let bit = idx + 1;
            bitmap[bit / 8] |= 1u8 << (bit % 8);
        }
        if bitmap.iter().any(|&b| b != 0) {
            UsbInResult::Data(bitmap)
        } else {
            UsbInResult::Nak
        }
    }

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        Some(self)
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        Some(self)
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.upstream_suspended = suspended;
        for port in &mut self.ports {
            if let Some(dev) = port.device.as_mut() {
                dev.model_mut().set_suspended(suspended || port.suspended);
            }
        }
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        self.poll_remote_wakeup_internal()
    }
}

impl UsbHub for UsbHubDevice {
    fn tick_1ms(&mut self) {
        for port in &mut self.ports {
            port.tick_1ms();
            if !port.enabled {
                continue;
            }

            // Remote wakeup for selectively-suspended downstream ports.
            //
            // When the hub itself is active (upstream not suspended) but an individual port is
            // suspended via SET_FEATURE(PORT_SUSPEND), a downstream device may signal remote wake.
            // Model this as the port leaving the suspended state and latching a suspend-change bit
            // so the host can observe the resume via the hub interrupt endpoint.
            if !self.upstream_suspended && port.powered && port.suspended {
                let wake = match port.device.as_mut() {
                    Some(dev) => dev.model_mut().poll_remote_wakeup(),
                    None => false,
                };
                if wake {
                    port.set_suspended(false);
                    if let Some(dev) = port.device.as_mut() {
                        dev.model_mut()
                            .set_suspended(self.upstream_suspended || port.suspended);
                    }
                }
            }

            // Do not tick downstream devices while their port is suspended; this matches root hub
            // behaviour (traffic is quiesced during suspend).
            if port.suspended {
                continue;
            }
            if let Some(dev) = port.device.as_mut() {
                dev.tick_1ms();
            }
        }
    }

    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        if self.upstream_suspended {
            return None;
        }
        for port in &mut self.ports {
            if !(port.enabled && port.powered) || port.suspended {
                continue;
            }
            if let Some(dev) = port.device.as_mut() {
                if let Some(found) = dev.device_mut_for_address(address) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device.as_mut()
    }

    fn attach_downstream(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        if let Some(p) = self.ports.get_mut(port) {
            let upstream_suspended = self.upstream_suspended;
            p.attach(model);
            if let Some(dev) = p.device.as_mut() {
                dev.model_mut()
                    .set_suspended(upstream_suspended || p.suspended);
            }
        }
    }

    fn detach_downstream(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.detach();
        }
    }

    fn num_ports(&self) -> usize {
        self.ports.len()
    }
}

fn hub_bitmap_len(num_ports: usize) -> usize {
    num_ports.saturating_add(1).div_ceil(8)
}

fn build_hub_config_descriptor(interrupt_bitmap_len: usize) -> Vec<u8> {
    let max_packet_size: u16 = interrupt_bitmap_len
        .try_into()
        .expect("interrupt bitmap length fits in u16");
    let w_max_packet_size = max_packet_size.to_le_bytes();

    // Config(9) + Interface(9) + Endpoint(7) = 25 bytes
    vec![
        // Configuration descriptor
        0x09, // bLength
        USB_DESCRIPTOR_TYPE_CONFIGURATION,
        25,
        0x00, // wTotalLength
        0x01, // bNumInterfaces
        0x01, // bConfigurationValue
        0x00, // iConfiguration
        0xa0, // bmAttributes (bus powered + remote wake)
        50,   // bMaxPower (100mA)
        // Interface descriptor
        0x09, // bLength
        USB_DESCRIPTOR_TYPE_INTERFACE,
        0x00, // bInterfaceNumber
        0x00, // bAlternateSetting
        0x01, // bNumEndpoints
        0x09, // bInterfaceClass (Hub)
        0x00, // bInterfaceSubClass
        0x00, // bInterfaceProtocol
        0x00, // iInterface
        // Endpoint descriptor (Interrupt IN)
        0x07, // bLength
        USB_DESCRIPTOR_TYPE_ENDPOINT,
        HUB_INTERRUPT_IN_EP, // bEndpointAddress
        0x03,                // bmAttributes (Interrupt)
        w_max_packet_size[0],
        w_max_packet_size[1], // wMaxPacketSize
        0x0c,                 // bInterval
    ]
}

fn build_hub_descriptor(num_ports: usize) -> Vec<u8> {
    // USB 2.0 hub class spec 11.23.2.1:
    // - bits 0..=1: power switching mode (01b = per-port).
    // - bits 3..=4: over-current protection mode (10b = no over-current reporting).
    const HUB_W_HUB_CHARACTERISTICS: u16 = 0x0011;

    let bitmap_len = hub_bitmap_len(num_ports);
    let mut port_pwr_ctrl_mask = vec![0u8; bitmap_len];
    for port in 1..=num_ports {
        let byte = port / 8;
        let bit = port % 8;
        port_pwr_ctrl_mask[byte] |= 1u8 << bit;
    }

    let mut desc = Vec::with_capacity(7 + 2 * bitmap_len);
    desc.push((7 + 2 * bitmap_len) as u8); // bLength
    desc.push(USB_DESCRIPTOR_TYPE_HUB);
    desc.push(num_ports as u8); // bNbrPorts
    desc.extend_from_slice(&HUB_W_HUB_CHARACTERISTICS.to_le_bytes()); // wHubCharacteristics
    desc.push(0x01); // bPwrOn2PwrGood (2ms)
    desc.push(0x00); // bHubContrCurrent
    desc.extend(std::iter::repeat_n(0u8, bitmap_len)); // DeviceRemovable
    desc.extend_from_slice(&port_pwr_ctrl_mask); // PortPwrCtrlMask
    desc
}

fn clamp_response(mut data: Vec<u8>, setup_w_length: u16) -> Vec<u8> {
    let requested = setup_w_length as usize;
    if data.len() > requested {
        data.truncate(requested);
    }
    data
}

fn build_string_descriptor_utf16le(s: &str) -> Vec<u8> {
    // USB string descriptors encode `bLength` as a u8, and strings are UTF-16LE. This caps the
    // total descriptor size to 254 bytes (2-byte header + up to 126 UTF-16 code units) and avoids
    // truncating surrogate pairs mid-character.
    const MAX_LEN: usize = 254;

    let mut out = Vec::with_capacity(MAX_LEN);
    out.push(0); // bLength placeholder
    out.push(USB_DESCRIPTOR_TYPE_STRING);
    for ch in s.chars() {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        let needed = units.len() * 2;
        if out.len() + needed > MAX_LEN {
            break;
        }
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out[0] = out.len() as u8;
    out
}

static HUB_DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength
    USB_DESCRIPTOR_TYPE_DEVICE,
    0x10,
    0x01, // bcdUSB (1.10)
    0x09, // bDeviceClass (Hub)
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol (Full-speed hub)
    0x40, // bMaxPacketSize0 (64)
    0x34,
    0x12, // idVendor (0x1234)
    0x02,
    0x00, // idProduct (0x0002)
    0x00,
    0x01, // bcdDevice (1.00)
    0x01, // iManufacturer
    0x02, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::UsbHidKeyboardHandle;

    #[test]
    fn hub_drains_downstream_remote_wakeup_requests_when_remote_wakeup_disabled() {
        #[derive(Default)]
        struct WakeState {
            suspended: bool,
            wake_pending: bool,
            polls: u32,
        }

        #[derive(Clone)]
        struct WakeDevice(Rc<RefCell<WakeState>>);

        impl WakeDevice {
            fn new() -> Self {
                Self(Rc::new(RefCell::new(WakeState::default())))
            }

            fn request_wake(&self) {
                self.0.borrow_mut().wake_pending = true;
            }

            fn wake_pending(&self) -> bool {
                self.0.borrow().wake_pending
            }

            fn polls(&self) -> u32 {
                self.0.borrow().polls
            }
        }

        impl UsbDeviceModel for WakeDevice {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }

            fn set_suspended(&mut self, suspended: bool) {
                self.0.borrow_mut().suspended = suspended;
            }

            fn poll_remote_wakeup(&mut self) -> bool {
                let mut st = self.0.borrow_mut();
                st.polls += 1;
                if st.suspended && st.wake_pending {
                    st.wake_pending = false;
                    true
                } else {
                    false
                }
            }
        }

        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.configuration = 1;
        hub.remote_wakeup_enabled = false;
        let wake = WakeDevice::new();
        hub.attach(1, Box::new(wake.clone()));

        // Ensure the downstream port is both powered and enabled so remote-wakeup polling reaches it.
        hub.ports[0].set_powered(true);
        hub.ports[0].set_enabled(true);
        hub.ports[0].set_suspended(true);
        assert!(
            hub.ports[0].suspended,
            "expected downstream port to be selectively suspended"
        );

        // Suspend the upstream link so `poll_remote_wakeup_internal` is active.
        hub.set_suspended(true);

        wake.request_wake();
        assert!(wake.wake_pending(), "expected wake request to be pending");
        assert_eq!(wake.polls(), 0);

        // Hub remote wake is disabled, so the wake must not propagate upstream...
        assert!(
            !hub.poll_remote_wakeup_internal(),
            "hub must not propagate remote wakeup when DEVICE_REMOTE_WAKEUP is disabled"
        );

        // ...but the wake signal should still be drained so it cannot be replayed later.
        assert!(
            !wake.wake_pending(),
            "expected hub to drain downstream wake request even when propagation is disabled"
        );
        assert!(
            hub.ports[0].suspended,
            "hub must not resume selectively suspended port when DEVICE_REMOTE_WAKEUP is disabled"
        );
        assert_eq!(wake.polls(), 1);

        // After enabling hub remote wakeup, a stale wake event should not be replayed.
        hub.remote_wakeup_enabled = true;
        assert!(
            !hub.poll_remote_wakeup_internal(),
            "unexpected wake propagation after enabling hub remote wake without a new event"
        );
        assert_eq!(wake.polls(), 2);
    }

    #[test]
    fn hub_propagates_downstream_remote_wakeup_when_remote_wakeup_enabled() {
        #[derive(Default)]
        struct WakeState {
            suspended: bool,
            wake_pending: bool,
        }

        #[derive(Clone)]
        struct WakeDevice(Rc<RefCell<WakeState>>);

        impl WakeDevice {
            fn new() -> Self {
                Self(Rc::new(RefCell::new(WakeState::default())))
            }

            fn request_wake(&self) {
                self.0.borrow_mut().wake_pending = true;
            }

            fn wake_pending(&self) -> bool {
                self.0.borrow().wake_pending
            }
        }

        impl UsbDeviceModel for WakeDevice {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }

            fn set_suspended(&mut self, suspended: bool) {
                self.0.borrow_mut().suspended = suspended;
            }

            fn poll_remote_wakeup(&mut self) -> bool {
                let mut st = self.0.borrow_mut();
                if st.suspended && st.wake_pending {
                    st.wake_pending = false;
                    true
                } else {
                    false
                }
            }
        }

        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.configuration = 1;
        hub.remote_wakeup_enabled = true;
        let wake = WakeDevice::new();
        hub.attach(1, Box::new(wake.clone()));

        // Ensure the downstream port is both powered and enabled so remote-wakeup polling reaches it.
        hub.ports[0].set_powered(true);
        hub.ports[0].set_enabled(true);

        // Selectively suspend the downstream port so we can observe that remote wakeup resumes it
        // when the wake actually propagates upstream.
        hub.ports[0].set_suspended(true);
        assert!(
            hub.ports[0].suspended,
            "expected downstream port to be selectively suspended"
        );

        // Suspend the upstream link so `poll_remote_wakeup_internal` is active.
        hub.set_suspended(true);

        wake.request_wake();
        assert!(wake.wake_pending(), "expected wake request to be pending");

        assert!(
            hub.poll_remote_wakeup_internal(),
            "expected hub to propagate remote wakeup when DEVICE_REMOTE_WAKEUP is enabled"
        );
        assert!(
            !wake.wake_pending(),
            "expected hub to drain downstream wake request after propagation"
        );
        assert!(
            !hub.ports[0].suspended,
            "expected hub to resume selectively suspended port when wake propagates upstream"
        );

        // Remote wakeup polling should be edge-triggered (drained). Do not allow replay without a
        // new wake request.
        assert!(
            !hub.poll_remote_wakeup_internal(),
            "expected downstream wake request to be drained"
        );
    }

    #[test]
    fn hub_remote_wakeup_resumes_selectively_suspended_port() {
        #[derive(Default)]
        struct WakeState {
            suspended: bool,
            wake_pending: bool,
        }

        #[derive(Clone)]
        struct WakeDevice(Rc<RefCell<WakeState>>);

        impl WakeDevice {
            fn new() -> Self {
                Self(Rc::new(RefCell::new(WakeState::default())))
            }

            fn request_wake(&self) {
                self.0.borrow_mut().wake_pending = true;
            }
        }

        impl UsbDeviceModel for WakeDevice {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }

            fn set_suspended(&mut self, suspended: bool) {
                self.0.borrow_mut().suspended = suspended;
            }

            fn poll_remote_wakeup(&mut self) -> bool {
                let mut st = self.0.borrow_mut();
                if st.suspended && st.wake_pending {
                    st.wake_pending = false;
                    true
                } else {
                    false
                }
            }
        }

        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.configuration = 1;
        hub.remote_wakeup_enabled = false;
        hub.upstream_suspended = false;

        let wake = WakeDevice::new();
        hub.attach(1, Box::new(wake.clone()));
        hub.ports[0].set_powered(true);
        hub.ports[0].set_enabled(true);

        // Clear the initial connect/enable change bits so the interrupt endpoint stays idle until
        // the remote wake event resumes the port.
        hub.ports[0].connect_change = false;
        hub.ports[0].enable_change = false;
        hub.ports[0].suspend_change = false;

        // Suspend the downstream port (selective suspend). This should update the device model's
        // suspended state so it can report a remote wake event.
        let suspend = SetupPacket {
            bm_request_type: 0x23, // Host-to-device | Class | Other (port)
            b_request: USB_REQUEST_SET_FEATURE,
            w_value: HUB_PORT_FEATURE_SUSPEND,
            w_index: 1,
            w_length: 0,
        };
        assert_eq!(
            hub.handle_control_request(suspend, None),
            ControlResponse::Ack
        );
        assert!(hub.ports[0].suspended);
        assert!(wake.0.borrow().suspended);

        // Clear the port's suspend-change bit so the next interrupt reflects only the resume.
        let clear_suspend_change = SetupPacket {
            bm_request_type: 0x23,
            b_request: USB_REQUEST_CLEAR_FEATURE,
            w_value: HUB_PORT_FEATURE_C_PORT_SUSPEND,
            w_index: 1,
            w_length: 0,
        };
        assert_eq!(
            hub.handle_control_request(clear_suspend_change, None),
            ControlResponse::Ack
        );
        assert!(!hub.ports[0].suspend_change);
        assert_eq!(
            hub.handle_interrupt_in(HUB_INTERRUPT_IN_EP),
            UsbInResult::Nak
        );

        wake.request_wake();
        UsbHub::tick_1ms(&mut hub);

        assert!(
            !hub.ports[0].suspended,
            "expected remote wake to resume selectively suspended port"
        );
        assert!(
            hub.ports[0].suspend_change,
            "expected resume to latch suspend-change bit"
        );
        assert!(
            !wake.0.borrow().suspended,
            "expected resumed port to update device suspended state"
        );

        let bitmap = match hub.handle_interrupt_in(HUB_INTERRUPT_IN_EP) {
            UsbInResult::Data(b) => b,
            other => panic!("expected interrupt IN data after resume, got {other:?}"),
        };
        assert_eq!(bitmap.len(), hub.interrupt_bitmap_len);
        assert_ne!(
            bitmap[0] & (1 << 1),
            0,
            "expected port 1 change bit in interrupt bitmap"
        );
    }

    #[test]
    fn hub_attach_sets_device_suspended_state_when_upstream_suspended() {
        #[derive(Default)]
        struct State {
            suspended: bool,
        }

        #[derive(Clone)]
        struct Device(Rc<RefCell<State>>);

        impl UsbDeviceModel for Device {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }

            fn set_suspended(&mut self, suspended: bool) {
                self.0.borrow_mut().suspended = suspended;
            }
        }

        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.configuration = 1;
        hub.set_suspended(true);

        let dev = Device(Rc::new(RefCell::new(State::default())));
        hub.attach(1, Box::new(dev.clone()));

        assert!(
            dev.0.borrow().suspended,
            "expected newly attached device to observe upstream suspended state"
        );

        hub.set_suspended(false);
        assert!(
            !dev.0.borrow().suspended,
            "expected device to resume when upstream resumes"
        );
    }

    #[test]
    fn hub_disable_clears_device_suspended_state() {
        #[derive(Default)]
        struct State {
            suspended: bool,
        }

        #[derive(Clone)]
        struct Device(Rc<RefCell<State>>);

        impl UsbDeviceModel for Device {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }

            fn set_suspended(&mut self, suspended: bool) {
                self.0.borrow_mut().suspended = suspended;
            }
        }

        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.configuration = 1;

        let dev = Device(Rc::new(RefCell::new(State::default())));
        hub.attach(1, Box::new(dev.clone()));
        hub.ports[0].set_powered(true);
        hub.ports[0].set_enabled(true);

        let suspend = SetupPacket {
            bm_request_type: 0x23, // Host-to-device | Class | Other (port)
            b_request: USB_REQUEST_SET_FEATURE,
            w_value: HUB_PORT_FEATURE_SUSPEND,
            w_index: 1,
            w_length: 0,
        };
        assert_eq!(hub.handle_control_request(suspend, None), ControlResponse::Ack);
        assert!(hub.ports[0].suspended);
        assert!(dev.0.borrow().suspended);

        let disable_port = SetupPacket {
            bm_request_type: 0x23,
            b_request: USB_REQUEST_CLEAR_FEATURE,
            w_value: HUB_PORT_FEATURE_ENABLE,
            w_index: 1,
            w_length: 0,
        };
        assert_eq!(
            hub.handle_control_request(disable_port, None),
            ControlResponse::Ack
        );
        assert!(!hub.ports[0].enabled, "expected port to become disabled");
        assert!(
            !hub.ports[0].suspended,
            "expected disabling the port to clear port suspend state"
        );
        assert!(
            !dev.0.borrow().suspended,
            "expected disabling the port to update device suspended state"
        );
    }

    #[test]
    fn root_hub_portsc_lsda_is_set_only_for_low_speed() {
        const LSDA: u16 = 1 << 8;

        struct SpeedModel {
            speed: UsbSpeed,
        }

        impl UsbDeviceModel for SpeedModel {
            fn speed(&self) -> UsbSpeed {
                self.speed
            }

            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }
        }

        for speed in [UsbSpeed::Full, UsbSpeed::Low, UsbSpeed::High] {
            let mut hub = RootHub::new();
            hub.attach(0, Box::new(SpeedModel { speed }));
            let portsc = hub.read_portsc(0);
            let expected_lsda = matches!(speed, UsbSpeed::Low);
            assert_eq!((portsc & LSDA) != 0, expected_lsda, "speed={speed:?}");
        }
    }

    #[test]
    fn string_descriptors_are_capped_to_u8_length_and_remain_valid_utf16() {
        let long = "😀".repeat(1000);
        let desc = build_string_descriptor_utf16le(&long);
        assert_eq!(desc.len(), 254);
        assert_eq!(desc[0] as usize, desc.len());
        assert_eq!(desc[1], USB_DESCRIPTOR_TYPE_STRING);

        let payload = &desc[2..];
        assert_eq!(payload.len() % 2, 0);
        let units: Vec<u16> = payload
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16(&units).expect("payload must be valid UTF-16");
    }

    #[test]
    fn external_hub_snapshot_restore_detaches_downstream_devices() {
        let mut hub = UsbHubDevice::new_with_ports(1);
        hub.attach(1, Box::new(UsbHidKeyboardHandle::new()));
        assert!(hub.ports[0].device.is_some(), "expected attached device");

        let snapshot = UsbHubDevice::new_with_ports(1).save_state();
        hub.load_state(&snapshot).expect("snapshot restore");

        assert!(
            hub.ports[0].device.is_none(),
            "snapshot restore should detach missing downstream device"
        );
    }
}
