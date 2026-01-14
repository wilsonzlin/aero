use alloc::boxed::Box;
use alloc::vec::Vec;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotResult};

use crate::device::AttachedUsbDevice;
use crate::{UsbDeviceModel, UsbSpeed};

/// Owner of a muxed USB 2.0 physical port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usb2PortOwner {
    /// The EHCI controller owns the port.
    Ehci,
    /// A USB 1.1 companion controller (UHCI in Aero) owns the port.
    Companion,
}

/// Shared USB 2.0 root port multiplexer used to model EHCI + companion routing.
///
/// This is an MVP abstraction focused on `CONFIGFLAG` and `PORT_OWNER` handoff semantics:
///
/// - One physical port can be visible to either the EHCI controller *or* its companion.
/// - The attached device model is shared and moves between controllers based on ownership.
/// - Split transactions / TT behaviour are **not** modelled.
///
/// The mux exposes per-controller register views:
/// - UHCI: 16-bit `PORTSC` registers (Intel UHCI style).
/// - EHCI: 32-bit `PORTSC` registers + global `CONFIGFLAG` routing.
///
/// Design notes: see `docs/usb-ehci.md` (companion controller discussion).
pub struct Usb2PortMux {
    configflag: bool,
    ports: Vec<Usb2MuxPort>,
}

impl Usb2PortMux {
    pub fn new(num_ports: usize) -> Self {
        Self {
            configflag: false,
            ports: (0..num_ports).map(|_| Usb2MuxPort::new()).collect(),
        }
    }

    pub fn num_ports(&self) -> usize {
        self.ports.len()
    }

    pub fn configflag(&self) -> bool {
        self.configflag
    }

    pub fn set_configflag(&mut self, configflag: bool) {
        if self.configflag == configflag {
            return;
        }
        self.configflag = configflag;
        for idx in 0..self.ports.len() {
            self.recompute_owner(idx);
        }
    }

    /// Restores CONFIGFLAG without triggering runtime side effects.
    ///
    /// At runtime, a CONFIGFLAG change can trigger ownership transitions that reset attached
    /// devices. During snapshot restore we instead want to preserve device state, so this updates
    /// the mux routing decision without calling `transfer_owner`.
    pub(crate) fn set_configflag_for_restore(&mut self, configflag: bool) {
        self.configflag = configflag;
        for p in &mut self.ports {
            p.effective_owner = if !self.configflag || p.port_owner {
                Usb2PortOwner::Companion
            } else {
                Usb2PortOwner::Ehci
            };
        }
    }

    pub fn attach(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        p.device = Some(AttachedUsbDevice::new(model));
        p.on_physical_attach();
    }

    pub fn detach(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        p.device = None;
        p.on_physical_detach();
    }

    pub fn port_device(&self, port: usize) -> Option<&AttachedUsbDevice> {
        self.ports.get(port)?.device.as_ref()
    }

    pub fn port_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device.as_mut()
    }

    pub fn uhci_read_portsc(&self, port: usize) -> u16 {
        self.ports
            .get(port)
            .map(|p| p.uhci.read_portsc(p.device.as_ref()))
            .unwrap_or(0)
    }

    pub fn uhci_write_portsc_masked(&mut self, port: usize, value: u16, write_mask: u16) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        p.uhci.write_portsc(value, write_mask, &mut p.device);
    }

    pub fn uhci_tick_1ms(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        if p.effective_owner != Usb2PortOwner::Companion {
            return;
        }
        p.uhci.tick_1ms(&mut p.device, RemoteWakeBehavior::ResumeDetect);
    }

    pub fn uhci_bus_reset(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        p.uhci.bus_reset(&mut p.device);
    }

    pub fn uhci_force_enable_for_tests(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.uhci.force_enable_for_tests(&mut p.device);
        }
    }

    pub fn uhci_force_resume_detect_for_tests(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.uhci.force_resume_detect_for_tests();
        }
    }

    pub(crate) fn uhci_port_routable(&self, port: usize) -> bool {
        let Some(p) = self.ports.get(port) else {
            return false;
        };
        p.effective_owner == Usb2PortOwner::Companion && p.uhci.routable()
    }

    pub(crate) fn save_snapshot_uhci_port_record(&self, port: usize) -> Vec<u8> {
        let Some(p) = self.ports.get(port) else {
            return Vec::new();
        };
        p.save_snapshot_record(ViewKind::Uhci)
    }

    pub(crate) fn load_snapshot_uhci_port_record(
        &mut self,
        port: usize,
        buf: &[u8],
    ) -> SnapshotResult<()> {
        let Some(p) = self.ports.get_mut(port) else {
            return Err(SnapshotError::InvalidFieldEncoding("usb2 mux port"));
        };
        p.load_snapshot_record(ViewKind::Uhci, buf)?;
        Ok(())
    }

    pub fn ehci_read_portsc(&self, port: usize) -> u32 {
        self.ports
            .get(port)
            .map(|p| p.ehci_read_portsc())
            .unwrap_or(0)
    }

    pub fn ehci_write_portsc_masked(&mut self, port: usize, value: u32, write_mask: u32) {
        // PORT_OWNER writes can trigger ownership transitions. Handle ownership first so the
        // subsequent PORTSC state mutations apply to the currently visible controller.
        const PORT_OWNER: u32 = 1 << 13;
        const CSC: u32 = 1 << 1;
        const PEDC: u32 = 1 << 3;

        let mut write_mask = write_mask;

        // PORT_OWNER is only writable when the EHCI-visible port is disabled.
        if write_mask & PORT_OWNER != 0 {
            let Some(p) = self.ports.get(port) else {
                return;
            };
            if p.ehci.enabled {
                write_mask &= !PORT_OWNER;
            }
        }

        let owner_changed = if write_mask & PORT_OWNER != 0 {
            let Some(p) = self.ports.get_mut(port) else {
                return;
            };
            let want_companion = value & PORT_OWNER != 0;
            if p.port_owner != want_companion {
                p.port_owner = want_companion;
                true
            } else {
                false
            }
        } else {
            false
        };
        if owner_changed {
            self.recompute_owner(port);
        }

        let Some(p) = self.ports.get_mut(port) else {
            return;
        };

        // When the port is owned by the companion controller, EHCI should not drive reset/enable,
        // but it should still allow software to clear latched change bits.
        if p.effective_owner != Usb2PortOwner::Ehci {
            write_mask &= CSC | PEDC;
        }

        p.ehci.write_portsc_ehci(value, write_mask, &mut p.device);
    }

    pub fn ehci_tick_1ms(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        if p.effective_owner != Usb2PortOwner::Ehci {
            return;
        }
        p.ehci.tick_1ms(&mut p.device, RemoteWakeBehavior::EnterResume);
    }

    pub fn ehci_bus_reset(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        p.ehci.bus_reset(&mut p.device);
    }

    pub fn ehci_force_enable_for_tests(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.ehci.force_enable_for_tests(&mut p.device);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn ehci_port_routable(&self, port: usize) -> bool {
        let Some(p) = self.ports.get(port) else {
            return false;
        };
        p.effective_owner == Usb2PortOwner::Ehci && p.ehci.routable()
    }

    pub(crate) fn save_snapshot_ehci_port_record(&self, port: usize) -> Vec<u8> {
        let Some(p) = self.ports.get(port) else {
            return Vec::new();
        };

        // Prefix the per-view port snapshot record with the EHCI PORT_OWNER latch so restore can
        // reconstruct routing decisions without relying on external defaults.
        let view = p.save_snapshot_record(ViewKind::Ehci);
        Encoder::new()
            .bool(p.port_owner)
            .u32(view.len() as u32)
            .bytes(&view)
            .finish()
    }

    pub(crate) fn load_snapshot_ehci_port_record(
        &mut self,
        port: usize,
        buf: &[u8],
    ) -> SnapshotResult<()> {
        let Some(p) = self.ports.get_mut(port) else {
            return Err(SnapshotError::InvalidFieldEncoding("usb2 mux port"));
        };

        let mut d = Decoder::new(buf);
        p.port_owner = d.bool()?;
        let view_len = d.u32()? as usize;
        let view = d.bytes(view_len)?;
        d.finish()?;

        p.load_snapshot_record(ViewKind::Ehci, view)?;

        // Recompute effective ownership without triggering transfer_owner/reset side effects.
        p.effective_owner = if !self.configflag || p.port_owner {
            Usb2PortOwner::Companion
        } else {
            Usb2PortOwner::Ehci
        };

        Ok(())
    }

    pub fn ehci_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        for p in &mut self.ports {
            if p.effective_owner != Usb2PortOwner::Ehci || !p.ehci.routable() {
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

    fn recompute_owner(&mut self, port: usize) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };

        let prev = p.effective_owner;
        let next = if !self.configflag || p.port_owner {
            Usb2PortOwner::Companion
        } else {
            Usb2PortOwner::Ehci
        };
        if prev == next {
            return;
        }
        p.transfer_owner(next);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewKind {
    Uhci,
    #[allow(dead_code)]
    Ehci,
}

const MAX_USB_DEVICE_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

struct Usb2MuxPort {
    device: Option<AttachedUsbDevice>,

    /// Stored value of the EHCI `PORT_OWNER` bit (1 = companion, 0 = EHCI).
    port_owner: bool,

    effective_owner: Usb2PortOwner,
    uhci: PortLogic,
    ehci: PortLogic,
}

impl Usb2MuxPort {
    fn new() -> Self {
        Self {
            device: None,
            // Default to companion ownership until the guest sets CONFIGFLAG and clears PORT_OWNER.
            port_owner: true,
            effective_owner: Usb2PortOwner::Companion,
            uhci: PortLogic::new(),
            ehci: PortLogic::new(),
        }
    }

    fn on_physical_attach(&mut self) {
        // Treat the attach as a connect event on whichever controller currently owns the port.
        self.uhci.on_attach(
            self.effective_owner == Usb2PortOwner::Companion,
            &mut self.device,
        );
        self.ehci.on_attach(
            self.effective_owner == Usb2PortOwner::Ehci,
            &mut self.device,
        );
    }

    fn on_physical_detach(&mut self) {
        self.uhci.on_detach(&mut self.device);
        self.ehci.on_detach(&mut self.device);
    }

    fn transfer_owner(&mut self, new_owner: Usb2PortOwner) {
        self.effective_owner = new_owner;
        if let Some(dev) = self.device.as_mut() {
            // Ownership handoff is modelled as a logical disconnect/reconnect, so reset the device
            // state and ensure enumeration restarts from address 0.
            dev.reset();
        }
        self.uhci
            .on_owner_change(new_owner == Usb2PortOwner::Companion, &mut self.device);
        self.ehci
            .on_owner_change(new_owner == Usb2PortOwner::Ehci, &mut self.device);
    }

    fn ehci_read_portsc(&self) -> u32 {
        // EHCI PORTSC bits (subset):
        const CCS: u32 = 1 << 0;
        const CSC: u32 = 1 << 1;
        const PED: u32 = 1 << 2;
        const PEDC: u32 = 1 << 3;
        const FPR: u32 = 1 << 6;
        const SUSP: u32 = 1 << 7;
        const PR: u32 = 1 << 8;
        const HSP: u32 = 1 << 9;
        const LS_MASK: u32 = 0b11 << 10;
        const PP: u32 = 1 << 12;
        const PORT_OWNER: u32 = 1 << 13;

        let mut v = 0u32;
        v |= PP;
        if self.effective_owner == Usb2PortOwner::Companion {
            v |= PORT_OWNER;
        }

        // `PORTSC.CCS` reflects physical connection status even when the port is owned by a
        // companion controller (PORT_OWNER=1). When the mux routes the device to UHCI, EHCI should
        // still be able to see the D+/D- line state so the guest driver can decide whether to claim
        // ownership.
        if let Some(dev) = self.device.as_ref() {
            v |= CCS;

            // Speed reporting:
            // - High-speed devices set HSP (only when EHCI owns the port).
            // - Full/low-speed devices are distinguished via LS (line status).
            let speed = dev.speed();
            if self.effective_owner == Usb2PortOwner::Ehci && speed == UsbSpeed::High {
                v |= HSP;
            }

            if !self.ehci.reset {
                // EHCI 1.0 spec 2.3.9:
                // - 0b10 = J-state (D+ high) -> full-speed idle
                // - 0b01 = K-state (D- high) -> low-speed idle + resume signaling
                // - 0b00 = SE0/undefined     -> treat as high-speed / no device
                let ls = if self.ehci.resuming && speed != UsbSpeed::High {
                    0b01
                } else {
                    match speed {
                        UsbSpeed::High => 0b00,
                        UsbSpeed::Full => 0b10,
                        UsbSpeed::Low => 0b01,
                    }
                };
                v = (v & !LS_MASK) | ((ls as u32) << 10);
            }
        }
        if self.ehci.connect_change {
            v |= CSC;
        }
        if self.ehci.enabled {
            v |= PED;
        }
        if self.ehci.enable_change {
            v |= PEDC;
        }
        if self.ehci.suspended {
            v |= SUSP;
        }
        if self.ehci.resuming {
            v |= FPR;
        }
        if self.ehci.reset {
            v |= PR;
        }
        v
    }

    fn save_snapshot_record(&self, view: ViewKind) -> Vec<u8> {
        let st = match view {
            ViewKind::Uhci => &self.uhci,
            ViewKind::Ehci => &self.ehci,
        };

        let mut rec = Encoder::new()
            .bool(st.connected)
            .bool(st.connect_change)
            .bool(st.enabled)
            .bool(st.enable_change)
            .bool(st.resume_detect)
            .bool(st.reset)
            .u8(st.reset_countdown_ms)
            .bool(st.suspended)
            .bool(st.resuming)
            .u8(st.resume_countdown_ms)
            .bool(self.device.is_some());

        if let Some(dev) = self.device.as_ref() {
            let dev_state = dev.save_state();
            rec = rec.u32(dev_state.len() as u32).bytes(&dev_state);
        }

        rec.finish()
    }

    fn load_snapshot_record(&mut self, view: ViewKind, buf: &[u8]) -> SnapshotResult<()> {
        let mut pd = Decoder::new(buf);
        let st = match view {
            ViewKind::Uhci => &mut self.uhci,
            ViewKind::Ehci => &mut self.ehci,
        };

        st.connected = pd.bool()?;
        st.connect_change = pd.bool()?;
        st.enabled = pd.bool()?;
        st.enable_change = pd.bool()?;
        st.resume_detect = pd.bool()?;
        st.reset = pd.bool()?;
        st.reset_countdown_ms = pd.u8()?;
        st.suspended = pd.bool()?;
        st.resuming = pd.bool()?;
        st.resume_countdown_ms = pd.u8()?;
        let has_device_state = pd.bool()?;
        let device_state = if has_device_state {
            let len = pd.u32()? as usize;
            if len > MAX_USB_DEVICE_SNAPSHOT_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "usb2 mux device snapshot",
                ));
            }
            Some(pd.bytes(len)?)
        } else {
            None
        };
        pd.finish()?;

        if let Some(device_state) = device_state {
            if let Some(dev) = self.device.as_mut() {
                dev.load_state(device_state)?;
            } else if let Some(mut dev) = AttachedUsbDevice::try_new_from_snapshot(device_state)? {
                // `try_new_from_snapshot` only selects the concrete device model; the wrapper state
                // must still be restored from the snapshot bytes.
                dev.load_state(device_state)?;
                self.device = Some(dev);
            }
        } else {
            // Snapshot indicates no device attached.
            self.device = None;
        }

        // Ensure the device model observes the restored suspended state.
        if let Some(dev) = self.device.as_mut() {
            dev.model_mut().set_suspended(st.suspended);
        }

        Ok(())
    }
}

/// Shared port status logic (reset timers, enable/suspend state, etc).
///
/// This intentionally matches the UHCI root hub behaviour in `hub::RootHub` closely so a device
/// can be handed between EHCI and UHCI without having two independent copies of the device model.
#[derive(Clone)]
struct PortLogic {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteWakeBehavior {
    /// Model remote wakeup as a UHCI-style "Resume Detect" latch (PORTSC.RD).
    ResumeDetect,
    /// Model remote wakeup as an EHCI-style resume signaling window (PORTSC.FPR asserted for 20ms).
    EnterResume,
}

impl PortLogic {
    fn new() -> Self {
        Self {
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

    fn routable(&self) -> bool {
        self.enabled && !self.suspended && !self.resuming
    }

    fn set_suspended(&mut self, suspended: bool, dev: &mut Option<AttachedUsbDevice>) {
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        if let Some(dev) = dev.as_mut() {
            dev.model_mut().set_suspended(suspended);
        }
    }

    fn on_attach(&mut self, visible: bool, dev: &mut Option<AttachedUsbDevice>) {
        self.resume_detect = false;
        self.set_suspended(false, dev);
        self.resuming = false;
        self.resume_countdown_ms = 0;

        let was_connected = self.connected;
        self.connected = visible;
        // Connecting a new device appears as a connection change and disables the port until reset.
        if visible {
            self.connect_change = true;
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        } else if was_connected {
            self.connect_change = true;
        }
    }

    fn on_detach(&mut self, dev: &mut Option<AttachedUsbDevice>) {
        self.resume_detect = false;
        self.set_suspended(false, dev);
        self.resuming = false;
        self.resume_countdown_ms = 0;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
        }
    }

    fn on_owner_change(&mut self, visible: bool, dev: &mut Option<AttachedUsbDevice>) {
        // Model ownership transfer as a logical disconnect/reconnect.
        self.on_detach(dev);
        if visible {
            self.connected = true;
            self.connect_change = true;
        }
    }

    fn read_portsc(&self, dev: Option<&AttachedUsbDevice>) -> u16 {
        // UHCI root hub PORTSC bits (see `hub::Port::read_portsc`).
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
        if let Some(dev) = dev {
            if dev.speed() == UsbSpeed::Low {
                v |= LSDA;
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

    fn write_portsc(&mut self, value: u16, write_mask: u16, dev: &mut Option<AttachedUsbDevice>) {
        // UHCI root hub PORTSC bits.
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
        // Resume Detect is a latched status bit; model as W1C (see `hub::Port::write_portsc`).
        if write_mask & RD != 0 && value & RD != 0 {
            self.resume_detect = false;
        }

        // Port reset: 50ms, reset device state.
        if write_mask & PR != 0 && value & PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;
            self.resume_detect = false;
            self.set_suspended(false, dev);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            if let Some(dev) = dev.as_mut() {
                dev.reset();
            }
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        }

        if self.reset {
            // While reset asserted, ignore suspend/resume/enable writes.
            return;
        }

        // Port enable (read/write).
        if write_mask & PED != 0 {
            let want_enabled = value & PED != 0;
            if want_enabled {
                if self.connected && !self.enabled {
                    self.enabled = true;
                    self.enable_change = true;
                }
            } else if self.enabled {
                self.enabled = false;
                self.enable_change = true;
                self.set_suspended(false, dev);
                self.resuming = false;
                self.resume_countdown_ms = 0;
            }
        }

        if !self.connected {
            self.set_suspended(false, dev);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            return;
        }

        if write_mask & SUSP != 0 {
            let want_suspended = value & SUSP != 0;
            if want_suspended {
                if !self.resuming {
                    self.set_suspended(true, dev);
                }
            } else {
                self.set_suspended(false, dev);
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

    fn bus_reset(&mut self, dev: &mut Option<AttachedUsbDevice>) {
        self.resume_detect = false;
        self.set_suspended(false, dev);
        self.resuming = false;
        self.resume_countdown_ms = 0;
        if let Some(dev) = dev.as_mut() {
            dev.reset();
        }
    }

    fn tick_1ms(&mut self, dev: &mut Option<AttachedUsbDevice>, remote_wake: RemoteWakeBehavior) {
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
                self.set_suspended(false, dev);
            }
        }

        if self.enabled && self.suspended && !self.resuming {
            if let Some(dev) = dev.as_mut() {
                if dev.model_mut().poll_remote_wakeup() {
                    match remote_wake {
                        RemoteWakeBehavior::ResumeDetect => {
                            self.resume_detect = true;
                        }
                        RemoteWakeBehavior::EnterResume => {
                            self.resuming = true;
                            self.resume_countdown_ms = 20;
                        }
                    }
                }
            }
        }

        if !self.routable() {
            return;
        }
        if let Some(dev) = dev.as_mut() {
            dev.tick_1ms();
        }
    }

    fn force_enable_for_tests(&mut self, dev: &mut Option<AttachedUsbDevice>) {
        self.enabled = true;
        self.enable_change = true;
        self.set_suspended(false, dev);
        self.resuming = false;
        self.resume_countdown_ms = 0;
    }

    fn force_resume_detect_for_tests(&mut self) {
        self.resume_detect = true;
    }
}

impl PortLogic {
    fn write_portsc_ehci(
        &mut self,
        value: u32,
        write_mask: u32,
        dev: &mut Option<AttachedUsbDevice>,
    ) {
        // EHCI PORTSC bits (subset).
        const CSC: u32 = 1 << 1;
        const PED: u32 = 1 << 2;
        const PEDC: u32 = 1 << 3;
        const FPR: u32 = 1 << 6;
        const SUSP: u32 = 1 << 7;
        const PR: u32 = 1 << 8;

        // W1C status change bits.
        if write_mask & CSC != 0 && value & CSC != 0 {
            self.connect_change = false;
        }
        if write_mask & PEDC != 0 && value & PEDC != 0 {
            self.enable_change = false;
        }

        if write_mask & PR != 0 && value & PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;
            self.resume_detect = false;
            self.set_suspended(false, dev);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            if let Some(dev) = dev.as_mut() {
                dev.reset();
            }
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        }

        if self.reset {
            return;
        }

        if write_mask & PED != 0 {
            let want_enabled = value & PED != 0;
            if want_enabled {
                if self.connected && !self.enabled {
                    self.enabled = true;
                    self.enable_change = true;
                }
            } else if self.enabled {
                self.enabled = false;
                self.enable_change = true;
                self.set_suspended(false, dev);
                self.resuming = false;
                self.resume_countdown_ms = 0;
            }
        }

        if !self.connected {
            self.set_suspended(false, dev);
            self.resuming = false;
            self.resume_countdown_ms = 0;
            return;
        }

        if write_mask & SUSP != 0 {
            let want_suspended = value & SUSP != 0;
            if want_suspended {
                if !self.resuming {
                    self.set_suspended(true, dev);
                }
            } else {
                self.set_suspended(false, dev);
            }
        }

        if write_mask & FPR != 0 {
            let want_resuming = value & FPR != 0;
            if want_resuming {
                self.resuming = true;
                self.resume_countdown_ms = 20;
            } else {
                self.resuming = false;
                self.resume_countdown_ms = 0;
            }
        }
    }
}
