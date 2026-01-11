use core::{any::Any, fmt};

use crate::hub::UsbHub;
use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSpeed {
    Full,
    Low,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbPid {
    Setup,
    In,
    Out,
}

impl UsbPid {
    pub fn from_u8(pid: u8) -> Option<Self> {
        match pid {
            0x2D => Some(Self::Setup),
            0x69 => Some(Self::In),
            0xE1 => Some(Self::Out),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHandshake {
    /// Transaction completed successfully.
    Ack { bytes: usize },
    /// Device is temporarily unable to respond.
    Nak,
    /// Endpoint halted.
    Stall,
    /// No response (timeout).
    Timeout,
}

impl UsbHandshake {
    pub fn is_complete(self) -> bool {
        matches!(
            self,
            UsbHandshake::Ack { .. } | UsbHandshake::Stall | UsbHandshake::Timeout
        )
    }
}

#[derive(Clone, Copy)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl fmt::Debug for SetupPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupPacket")
            .field("request_type", &format_args!("{:#04x}", self.request_type))
            .field("request", &format_args!("{:#04x}", self.request))
            .field("value", &format_args!("{:#06x}", self.value))
            .field("index", &format_args!("{:#06x}", self.index))
            .field("length", &self.length)
            .finish()
    }
}

impl SetupPacket {
    pub fn parse(bytes: [u8; 8]) -> Self {
        Self {
            request_type: bytes[0],
            request: bytes[1],
            value: u16::from_le_bytes([bytes[2], bytes[3]]),
            index: u16::from_le_bytes([bytes[4], bytes[5]]),
            length: u16::from_le_bytes([bytes[6], bytes[7]]),
        }
    }
}

pub trait UsbDevice {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        None
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        None
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    /// Advances device state by 1ms.
    ///
    /// This is primarily used for hub port reset timers and for propagating time through nested
    /// hub topologies. Most device models do not require timers and can rely on the default no-op
    /// implementation.
    fn tick_1ms(&mut self) {}

    fn reset(&mut self);

    fn address(&self) -> u8;

    fn handle_setup(&mut self, setup: SetupPacket);

    /// Handle an OUT transaction to endpoint number `ep`.
    ///
    /// `ep` is the endpoint **number** (0..=15), not the endpoint address (e.g. not `0x01`).
    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake;

    /// Handle an IN transaction to endpoint number `ep`.
    ///
    /// `ep` is the endpoint **number** (0..=15), not the endpoint address (e.g. not `0x81`).
    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake;
}

pub struct UsbPort {
    pub connected: bool,
    pub enabled: bool,
    pub device: Option<Box<dyn UsbDevice>>,
}

impl UsbPort {
    fn empty() -> Self {
        Self {
            connected: false,
            enabled: false,
            device: None,
        }
    }
}

pub struct UsbBus {
    ports: Vec<UsbPort>,
}

impl UsbBus {
    pub fn new(num_ports: usize) -> Self {
        Self {
            ports: (0..num_ports).map(|_| UsbPort::empty()).collect(),
        }
    }

    pub fn num_ports(&self) -> usize {
        self.ports.len()
    }

    pub fn port(&self, idx: usize) -> Option<&UsbPort> {
        self.ports.get(idx)
    }

    pub fn port_mut(&mut self, idx: usize) -> Option<&mut UsbPort> {
        self.ports.get_mut(idx)
    }

    pub fn connect(&mut self, idx: usize, mut device: Box<dyn UsbDevice>) {
        device.reset();
        let port = self
            .ports
            .get_mut(idx)
            .unwrap_or_else(|| panic!("invalid port index {idx}"));
        port.connected = true;
        port.enabled = false;
        port.device = Some(device);
    }

    pub fn disconnect(&mut self, idx: usize) {
        let port = self
            .ports
            .get_mut(idx)
            .unwrap_or_else(|| panic!("invalid port index {idx}"));
        port.connected = false;
        port.enabled = false;
        port.device = None;
    }

    pub fn reset_port(&mut self, idx: usize) {
        if let Some(port) = self.ports.get_mut(idx) {
            // USB port reset temporarily disables the port; it is re-enabled once the hub/host
            // controller completes the reset sequence.
            port.enabled = false;
            if let Some(dev) = port.device.as_mut() {
                dev.reset();
            }
        }
    }

    pub fn tick_1ms(&mut self) {
        for port in &mut self.ports {
            if !port.connected || !port.enabled {
                continue;
            }
            let Some(dev) = port.device.as_mut() else {
                continue;
            };
            dev.tick_1ms();
        }
    }

    /// Attaches `device` at a topology path (root port → hub port → ...).
    ///
    /// The first element of `path` addresses a root port and is zero-based (like
    /// [`UsbBus::connect`]). Subsequent elements address hub ports and are **1-based** (matching
    /// USB hub port numbering used by hub class requests).
    pub fn attach_at_path(&mut self, path: &[usize], device: Box<dyn UsbDevice>) {
        let Some((&root, rest)) = path.split_first() else {
            panic!("USB topology path must not be empty");
        };

        if rest.is_empty() {
            self.connect(root, device);
            return;
        }

        let port = self
            .ports
            .get_mut(root)
            .unwrap_or_else(|| panic!("invalid port index {root}"));
        let Some(root_dev) = port.device.as_mut() else {
            panic!("no device attached at root port {root}");
        };

        let mut current: &mut dyn UsbDevice = root_dev.as_mut();
        for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
            let hub = current
                .as_hub_mut()
                .unwrap_or_else(|| panic!("device at depth {depth} is not a USB hub"));
            let hub_idx = hub_port
                .checked_sub(1)
                .unwrap_or_else(|| panic!("hub port numbers are 1-based (got 0 at depth {depth})"));
            let num_ports = hub.num_ports();
            if hub_idx >= num_ports {
                panic!("invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)");
            }
            current = hub.downstream_device_mut(hub_idx).unwrap_or_else(|| {
                panic!("no device attached at hub port {hub_port} (depth {depth})")
            });
        }

        let hub = current
            .as_hub_mut()
            .unwrap_or_else(|| panic!("device at depth {} is not a USB hub", rest.len() - 1));
        let last_port = rest[rest.len() - 1];
        let hub_idx = last_port.checked_sub(1).unwrap_or_else(|| {
            panic!(
                "hub port numbers are 1-based (got 0 at depth {})",
                rest.len() - 1
            )
        });
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            panic!(
                "invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
                rest.len() - 1
            );
        }
        hub.attach_downstream(hub_idx, device);
    }

    /// Detaches any device attached at the given topology `path`.
    ///
    /// See [`UsbBus::attach_at_path`] for path numbering conventions.
    pub fn detach_at_path(&mut self, path: &[usize]) {
        let Some((&root, rest)) = path.split_first() else {
            panic!("USB topology path must not be empty");
        };

        if rest.is_empty() {
            self.disconnect(root);
            return;
        }

        let port = self
            .ports
            .get_mut(root)
            .unwrap_or_else(|| panic!("invalid port index {root}"));
        let Some(root_dev) = port.device.as_mut() else {
            panic!("no device attached at root port {root}");
        };

        let mut current: &mut dyn UsbDevice = root_dev.as_mut();
        for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
            let hub = current
                .as_hub_mut()
                .unwrap_or_else(|| panic!("device at depth {depth} is not a USB hub"));
            let hub_idx = hub_port
                .checked_sub(1)
                .unwrap_or_else(|| panic!("hub port numbers are 1-based (got 0 at depth {depth})"));
            let num_ports = hub.num_ports();
            if hub_idx >= num_ports {
                panic!("invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)");
            }
            current = hub.downstream_device_mut(hub_idx).unwrap_or_else(|| {
                panic!("no device attached at hub port {hub_port} (depth {depth})")
            });
        }

        let hub = current
            .as_hub_mut()
            .unwrap_or_else(|| panic!("device at depth {} is not a USB hub", rest.len() - 1));
        let last_port = rest[rest.len() - 1];
        let hub_idx = last_port.checked_sub(1).unwrap_or_else(|| {
            panic!(
                "hub port numbers are 1-based (got 0 at depth {})",
                rest.len() - 1
            )
        });
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            panic!(
                "invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
                rest.len() - 1
            );
        }
        hub.detach_downstream(hub_idx);
    }

    pub fn device_mut_for_address(&mut self, addr: u8) -> Option<&mut dyn UsbDevice> {
        self.find_device_mut(addr)
    }

    fn find_device_mut(&mut self, addr: u8) -> Option<&mut dyn UsbDevice> {
        for port in &mut self.ports {
            if !port.connected || !port.enabled {
                continue;
            }
            let Some(dev) = port.device.as_mut() else {
                continue;
            };
            if dev.address() == addr {
                return Some(dev.as_mut());
            }
            if let Some(hub) = dev.as_hub_mut() {
                if let Some(found) = hub.downstream_device_mut_for_address(addr) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub fn handle_setup(&mut self, addr: u8, setup: SetupPacket) -> UsbHandshake {
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_setup(setup);
        UsbHandshake::Ack { bytes: 8 }
    }

    pub fn handle_out(&mut self, addr: u8, ep: u8, data: &[u8]) -> UsbHandshake {
        debug_assert!(
            (ep & 0xF0) == 0,
            "UsbBus::handle_out expects an endpoint number (0..=15), got {ep:#04x}"
        );
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_out(ep, data)
    }

    pub fn handle_in(&mut self, addr: u8, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        debug_assert!(
            (ep & 0xF0) == 0,
            "UsbBus::handle_in expects an endpoint number (0..=15), got {ep:#04x}"
        );
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_in(ep, buf)
    }
}

impl IoSnapshot for UsbBus {
    const DEVICE_ID: [u8; 4] = *b"USBB";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        use aero_io_snapshot::io::state::codec::Encoder;

        const TAG_PORT_COUNT: u16 = 1;
        const TAG_ROOT_PORTS: u16 = 2;
        const TAG_ADDRESSES_IN_USE: u16 = 3;
        const TAG_DEVICES: u16 = 4;

        const ADDR_BYTES: usize = 16; // 128 bits.

        fn set_addr_bit(bits: &mut [u8; ADDR_BYTES], addr: u8) {
            if addr >= 128 {
                return;
            }
            let idx = (addr / 8) as usize;
            let bit = addr % 8;
            bits[idx] |= 1u8 << bit;
        }

        fn collect_addresses(dev: &dyn UsbDevice, bits: &mut [u8; ADDR_BYTES]) {
            set_addr_bit(bits, dev.address());
            if let Some(hub) = dev.as_hub() {
                for port in 0..hub.num_ports() {
                    if let Some(child) = hub.downstream_device(port) {
                        collect_addresses(child, bits);
                    }
                }
            }
        }

        fn save_device(dev: &dyn UsbDevice) -> Vec<u8> {
            if let Some(hub) = dev.as_any().downcast_ref::<crate::hub::UsbHubDevice>() {
                return hub.save_state();
            }
            if let Some(kb) = dev.as_any().downcast_ref::<crate::hid::UsbHidKeyboard>() {
                return kb.save_state();
            }
            if let Some(mouse) = dev.as_any().downcast_ref::<crate::hid::UsbHidMouse>() {
                return mouse.save_state();
            }
            if let Some(gamepad) = dev.as_any().downcast_ref::<crate::hid::UsbHidGamepad>() {
                return gamepad.save_state();
            }
            if let Some(comp) = dev
                .as_any()
                .downcast_ref::<crate::hid::UsbHidCompositeInput>()
            {
                return comp.save_state();
            }
            if let Some(hidp) = dev
                .as_any()
                .downcast_ref::<crate::hid::passthrough::UsbHidPassthrough>()
            {
                return hidp.save_state();
            }
            if let Some(webusb) = dev
                .as_any()
                .downcast_ref::<crate::UsbWebUsbPassthroughDevice>()
            {
                return webusb.save_state();
            }
            panic!("USB device type is not snapshotable");
        }

        fn collect_devices(
            dev: &dyn UsbDevice,
            path: &mut Vec<u8>,
            out: &mut std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
        ) {
            out.insert(path.clone(), save_device(dev));
            if let Some(hub) = dev.as_hub() {
                for port_idx in 0..hub.num_ports() {
                    let Some(child) = hub.downstream_device(port_idx) else {
                        continue;
                    };
                    let port_num = u8::try_from(port_idx + 1).unwrap_or(u8::MAX);
                    path.push(port_num);
                    collect_devices(child, path, out);
                    path.pop();
                }
            }
        }

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let port_count = u16::try_from(self.ports.len()).unwrap_or(u16::MAX);
        w.field_u16(TAG_PORT_COUNT, port_count);

        let mut root_enc = Encoder::new().u32(self.ports.len() as u32);
        for port in &self.ports {
            root_enc = root_enc.bool(port.connected).bool(port.enabled);
        }
        w.field_bytes(TAG_ROOT_PORTS, root_enc.finish());

        let mut addr_bits = [0u8; ADDR_BYTES];
        for port in &self.ports {
            if let Some(dev) = port.device.as_deref() {
                collect_addresses(dev, &mut addr_bits);
            }
        }
        w.field_bytes(TAG_ADDRESSES_IN_USE, addr_bits.to_vec());

        let mut devices = std::collections::BTreeMap::<Vec<u8>, Vec<u8>>::new();
        for (idx, port) in self.ports.iter().enumerate() {
            let Some(dev) = port.device.as_deref() else {
                continue;
            };
            let root = u8::try_from(idx).unwrap_or(u8::MAX);
            let mut path = vec![root];
            collect_devices(dev, &mut path, &mut devices);
        }

        let mut dev_enc = Encoder::new().u32(devices.len() as u32);
        for (path, snap) in devices {
            dev_enc = dev_enc
                .u8(path.len() as u8)
                .bytes(&path)
                .u32(snap.len() as u32)
                .bytes(&snap);
        }
        w.field_bytes(TAG_DEVICES, dev_enc.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PORT_COUNT: u16 = 1;
        const TAG_ROOT_PORTS: u16 = 2;
        const TAG_ADDRESSES_IN_USE: u16 = 3;
        const TAG_DEVICES: u16 = 4;

        const MAX_PORTS: usize = 255;
        const MAX_DEVICES: usize = 1024;
        const MAX_PATH_LEN: usize = 32;

        fn peek_device_id(bytes: &[u8]) -> SnapshotResult<[u8; 4]> {
            if bytes.len() < 12 {
                return Err(SnapshotError::UnexpectedEof);
            }
            if bytes[0..4] != *b"AERO" {
                return Err(SnapshotError::InvalidMagic);
            }
            Ok([bytes[8], bytes[9], bytes[10], bytes[11]])
        }

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let port_count = r.u16(TAG_PORT_COUNT)?.unwrap_or(0) as usize;
        if port_count == 0 || port_count > MAX_PORTS {
            return Err(SnapshotError::InvalidFieldEncoding("invalid port count"));
        }

        // Root port state.
        let mut root_connected = vec![false; port_count];
        let mut root_enabled = vec![false; port_count];
        if let Some(buf) = r.bytes(TAG_ROOT_PORTS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count != port_count {
                return Err(SnapshotError::InvalidFieldEncoding("port count mismatch"));
            }
            for i in 0..count {
                root_connected[i] = d.bool()?;
                root_enabled[i] = d.bool()?;
            }
            d.finish()?;
        }

        // Decode device entries.
        #[derive(Clone)]
        struct Entry<'a> {
            path: Vec<u8>,
            snap: &'a [u8],
        }

        let mut entries = Vec::<Entry<'_>>::new();
        let mut seen_paths = std::collections::BTreeSet::<Vec<u8>>::new();
        if let Some(buf) = r.bytes(TAG_DEVICES) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_DEVICES {
                return Err(SnapshotError::InvalidFieldEncoding("too many usb devices"));
            }
            for _ in 0..count {
                let path_len = d.u8()? as usize;
                if path_len == 0 || path_len > MAX_PATH_LEN {
                    return Err(SnapshotError::InvalidFieldEncoding("invalid topology path"));
                }
                let path = d.bytes(path_len)?.to_vec();
                if !seen_paths.insert(path.clone()) {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "duplicate USB topology path",
                    ));
                }
                let snap_len = d.u32()? as usize;
                let snap = d.bytes(snap_len)?;
                entries.push(Entry { path, snap });
            }
            d.finish()?;
        }

        // Reset bus ports to a known empty state.
        self.ports = (0..port_count).map(|_| UsbPort::empty()).collect();

        // Restore topology: attach devices depth-first (parents before children).
        entries.sort_by(|a, b| a.path.len().cmp(&b.path.len()).then(a.path.cmp(&b.path)));

        for entry in &entries {
            let device_id = peek_device_id(entry.snap)?;
            let device: Box<dyn UsbDevice> = match &device_id {
                b"UHUB" => {
                    let ports = crate::hub::UsbHubDevice::snapshot_port_count(entry.snap)?;
                    Box::new(crate::hub::UsbHubDevice::new_with_ports(ports))
                }
                b"UKBD" => Box::new(crate::hid::UsbHidKeyboard::new()),
                b"UMSE" => Box::new(crate::hid::UsbHidMouse::new()),
                b"UGPD" => Box::new(crate::hid::UsbHidGamepad::new()),
                b"UCMP" => Box::new(crate::hid::UsbHidCompositeInput::new()),
                b"HIDP" => Box::new(crate::hid::passthrough::UsbHidPassthrough::default()),
                b"WUSB" => Box::new(crate::UsbWebUsbPassthroughDevice::new()),
                _ => return Err(SnapshotError::InvalidFieldEncoding("unknown USB device id")),
            };

            let path: Vec<usize> = entry.path.iter().map(|&v| v as usize).collect();
            self.attach_at_path_restore(&path, device)?;
        }

        // Restore root port connection/enabled state after attaching devices (connect() forces
        // enabled=false).
        for (idx, port) in self.ports.iter_mut().enumerate() {
            port.connected = root_connected.get(idx).copied().unwrap_or(false);
            port.enabled = root_enabled.get(idx).copied().unwrap_or(false);
            if port.connected != port.device.is_some() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "root port device presence mismatch",
                ));
            }
        }

        // Load device state after the full topology has been constructed so hub port state can be
        // restored without being clobbered by attach-side effects.
        for entry in entries {
            let path: Vec<usize> = entry.path.iter().map(|&v| v as usize).collect();
            let dev = self
                .device_at_path_mut(&path)
                .ok_or(SnapshotError::InvalidFieldEncoding(
                    "device missing at topology path",
                ))?;
            let device_id = peek_device_id(entry.snap)?;
            match &device_id {
                b"UHUB" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hub::UsbHubDevice>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"UKBD" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hid::UsbHidKeyboard>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"UMSE" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hid::UsbHidMouse>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"UGPD" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hid::UsbHidGamepad>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"UCMP" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hid::UsbHidCompositeInput>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"HIDP" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::hid::passthrough::UsbHidPassthrough>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                b"WUSB" => dev
                    .as_any_mut()
                    .downcast_mut::<crate::UsbWebUsbPassthroughDevice>()
                    .ok_or(SnapshotError::InvalidFieldEncoding("device type mismatch"))?
                    .load_state(entry.snap)?,
                _ => return Err(SnapshotError::InvalidFieldEncoding("unknown USB device id")),
            }
        }

        // `TAG_ADDRESSES_IN_USE` is currently informational; ignore for restore.
        let _ = r.bytes(TAG_ADDRESSES_IN_USE);

        Ok(())
    }
}

impl UsbBus {
    fn attach_at_path_restore(
        &mut self,
        path: &[usize],
        mut device: Box<dyn UsbDevice>,
    ) -> SnapshotResult<()> {
        let Some((&root, rest)) = path.split_first() else {
            return Err(SnapshotError::InvalidFieldEncoding("empty topology path"));
        };
        if root >= self.ports.len() {
            return Err(SnapshotError::InvalidFieldEncoding("invalid root port"));
        }

        if rest.is_empty() {
            device.reset();
            let port = &mut self.ports[root];
            port.connected = true;
            port.enabled = false;
            port.device = Some(device);
            return Ok(());
        }

        let port = &mut self.ports[root];
        let Some(root_dev) = port.device.as_mut() else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing device at root port",
            ));
        };

        let mut current: &mut dyn UsbDevice = root_dev.as_mut();
        for &hub_port in &rest[..rest.len() - 1] {
            let hub = current
                .as_hub_mut()
                .ok_or(SnapshotError::InvalidFieldEncoding("device is not a hub"))?;
            let idx = hub_port
                .checked_sub(1)
                .ok_or(SnapshotError::InvalidFieldEncoding(
                    "hub port numbers are 1-based",
                ))?;
            if idx >= hub.num_ports() {
                return Err(SnapshotError::InvalidFieldEncoding("invalid hub port"));
            }
            current = hub
                .downstream_device_mut(idx)
                .ok_or(SnapshotError::InvalidFieldEncoding(
                    "missing intermediate hub device",
                ))?;
        }

        let hub = current
            .as_hub_mut()
            .ok_or(SnapshotError::InvalidFieldEncoding("device is not a hub"))?;
        let last_port = rest[rest.len() - 1];
        let idx = last_port
            .checked_sub(1)
            .ok_or(SnapshotError::InvalidFieldEncoding(
                "hub port numbers are 1-based",
            ))?;
        if idx >= hub.num_ports() {
            return Err(SnapshotError::InvalidFieldEncoding("invalid hub port"));
        }
        device.reset();
        hub.attach_downstream(idx, device);
        Ok(())
    }

    fn device_at_path_mut(&mut self, path: &[usize]) -> Option<&mut dyn UsbDevice> {
        let Some((&root, rest)) = path.split_first() else {
            return None;
        };
        let port = self.ports.get_mut(root)?;
        let dev = port.device.as_mut()?.as_mut();
        if rest.is_empty() {
            return Some(dev);
        }
        let mut current: &mut dyn UsbDevice = dev;
        for &hub_port in rest {
            let hub = current.as_hub_mut()?;
            let idx = hub_port.checked_sub(1)?;
            current = hub.downstream_device_mut(idx)?;
        }
        Some(current)
    }
}
