use crate::pci::config::{
    PciBarChange, PciCommandChange, PciConfigSpace, PciConfigSpaceState, PciConfigWriteEffects,
};
use crate::pci::{PciBarKind, PciBarRange, PciBdf, PciDevice};
use crate::pci::{PciResourceAllocator, PciResourceError};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PciMappedBar {
    pub bdf: PciBdf,
    pub bar: u8,
    pub range: PciBarRange,
}

#[derive(Default)]
pub struct PciBus {
    devices: BTreeMap<PciBdf, Box<dyn PciDevice>>,
    mapped_bars: BTreeMap<(PciBdf, u8), PciBarRange>,
}

impl PciBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_device(&mut self, bdf: PciBdf, device: Box<dyn PciDevice>) {
        let prev = self.devices.insert(bdf, device);
        assert!(prev.is_none(), "duplicate PCI BDF {bdf:?}");
    }

    pub fn device_config(&self, bdf: PciBdf) -> Option<&PciConfigSpace> {
        self.devices.get(&bdf).map(|dev| dev.config())
    }

    pub fn device_config_mut(&mut self, bdf: PciBdf) -> Option<&mut PciConfigSpace> {
        self.devices.get_mut(&bdf).map(|dev| dev.config_mut())
    }

    pub fn iter_device_addrs(&self) -> impl Iterator<Item = PciBdf> + '_ {
        self.devices.keys().copied()
    }

    pub fn mapped_bars(&self) -> Vec<PciMappedBar> {
        self.mapped_bars
            .iter()
            .map(|((bdf, bar), range)| PciMappedBar {
                bdf: *bdf,
                bar: *bar,
                range: *range,
            })
            .collect()
    }

    /// Returns the currently decoded BAR range for a single BAR, if any.
    ///
    /// This consults the bus' internal BAR decode tracking (`mapped_bars`), which already respects
    /// the PCI command register I/O + memory decode enable bits and BAR relocation.
    pub fn mapped_bar_range(&self, bdf: PciBdf, bar: u8) -> Option<PciBarRange> {
        self.mapped_bars.get(&(bdf, bar)).copied()
    }

    pub fn mapped_mmio_bars(&self) -> Vec<PciMappedBar> {
        self.mapped_bars()
            .into_iter()
            .filter(|mapped| matches!(mapped.range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64))
            .collect()
    }

    pub fn mapped_io_bars(&self) -> Vec<PciMappedBar> {
        self.mapped_bars()
            .into_iter()
            .filter(|mapped| matches!(mapped.range.kind, PciBarKind::Io))
            .collect()
    }

    pub fn reset(&mut self, allocator: &mut PciResourceAllocator) -> Result<(), PciResourceError> {
        allocator.reset();
        self.mapped_bars.clear();

        for addr in self.iter_device_addrs().collect::<Vec<_>>() {
            let dev = self.devices.get_mut(&addr).expect("device disappeared");
            dev.reset();
        }

        // Some devices may come with fixed BAR assignments that firmware should preserve (e.g.
        // legacy compatible IDE controllers, or platform-chosen I/O BAR bases). Reserve any
        // non-zero BAR ranges so newly allocated BARs do not overlap them if additional devices
        // are added and POST is re-run.
        for bdf in self.iter_device_addrs().collect::<Vec<_>>() {
            let dev = self.devices.get_mut(&bdf).expect("device disappeared");
            for bar_index in 0u8..6u8 {
                let Some(range) = dev.config().bar_range(bar_index) else {
                    continue;
                };
                if range.base == 0 {
                    continue;
                }
                allocator.reserve_range(range);
            }
        }

        // Allocate BARs in deterministic order: ascending BDF then BAR index.
        for bdf in self.iter_device_addrs().collect::<Vec<_>>() {
            let dev = self.devices.get_mut(&bdf).expect("device disappeared");
            for bar_index in 0u8..6u8 {
                let def = dev.config().bar_definition(bar_index);
                let Some(def) = def else { continue };

                // Some devices (e.g. legacy-compatible IDE controllers) may come with fixed BAR
                // assignments that firmware should preserve. If the BAR already has a non-zero
                // base address, keep it rather than allocating a new one.
                if let Some(range) = dev.config().bar_range(bar_index) {
                    if range.base != 0 {
                        continue;
                    }
                }

                let base = allocator.allocate_bar(def)?;
                dev.config_mut().set_bar_base(bar_index, base);
            }
        }

        // BARs decode only when command register enables them, so mappings start empty.
        Ok(())
    }

    pub fn read_config(&mut self, bdf: PciBdf, offset: u16, size: u8) -> u32 {
        let Some(dev) = self.devices.get_mut(&bdf) else {
            // Non-existent device functions return all 1s, sized to the access width.
            // (e.g. 0xFF for byte reads, 0xFFFF for word reads, 0xFFFF_FFFF for dword reads)
            return all_ones(size);
        };
        dev.config_mut().read(offset, usize::from(size))
    }

    pub fn write_config(&mut self, bdf: PciBdf, offset: u16, size: u8, value: u32) {
        let effects = {
            let Some(dev) = self.devices.get_mut(&bdf) else {
                return;
            };
            dev.config_mut()
                .write_with_effects(offset, usize::from(size), value)
        };

        let (command, bar_ranges) = {
            let Some(dev) = self.devices.get(&bdf) else {
                return;
            };
            let cfg = dev.config();
            let command = cfg.command();
            let bar_ranges = core::array::from_fn(|index| cfg.bar_range(index as u8));
            (command, bar_ranges)
        };

        self.apply_config_write_effects(bdf, command, &bar_ranges, effects);
    }

    fn apply_config_write_effects(
        &mut self,
        bdf: PciBdf,
        command: u16,
        bar_ranges: &[Option<PciBarRange>; 6],
        effects: PciConfigWriteEffects,
    ) {
        if let PciCommandChange::Changed { old: _, new: _ } = effects.command {
            self.refresh_device_decoding(bdf, command, bar_ranges);
        }

        if let Some((bar, PciBarChange::Changed { old, new })) = effects.bar {
            // BAR updates only affect mappings if decoding is enabled.
            self.apply_bar_change(bdf, command, bar, old, new);
        }
    }

    fn refresh_device_decoding(
        &mut self,
        bdf: PciBdf,
        command: u16,
        bar_ranges: &[Option<PciBarRange>; 6],
    ) {
        // Drop all existing mappings for this device, then re-add those that are enabled.
        let keys = self
            .mapped_bars
            .keys()
            .filter(|(mapped_addr, _)| *mapped_addr == bdf)
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            self.mapped_bars.remove(&key);
        }

        let io_enabled = (command & 0x1) != 0;
        let mem_enabled = (command & 0x2) != 0;

        for (bar, range) in bar_ranges.iter().enumerate() {
            let Some(range) = range else { continue };
            if range.base == 0 {
                continue;
            }
            match range.kind {
                PciBarKind::Io if io_enabled => {
                    self.mapped_bars.insert((bdf, bar as u8), *range);
                }
                PciBarKind::Mmio32 | PciBarKind::Mmio64 if mem_enabled => {
                    self.mapped_bars.insert((bdf, bar as u8), *range);
                }
                _ => {}
            }
        }
    }

    fn apply_bar_change(
        &mut self,
        bdf: PciBdf,
        command: u16,
        bar: u8,
        old: PciBarRange,
        new: PciBarRange,
    ) {
        // Remove old mapping if present.
        self.mapped_bars.remove(&(bdf, bar));

        let io_enabled = (command & 0x1) != 0;
        let mem_enabled = (command & 0x2) != 0;

        if new.base == 0 {
            return;
        }

        match new.kind {
            PciBarKind::Io if io_enabled => {
                self.mapped_bars.insert((bdf, bar), new);
            }
            PciBarKind::Mmio32 | PciBarKind::Mmio64 if mem_enabled => {
                self.mapped_bars.insert((bdf, bar), new);
            }
            _ => {
                // Decoding disabled; keep unmapped.
            }
        }

        // Preserve old range so we can debug / extend with MMIO bus integration later.
        let _ = old;
    }
}

#[derive(Debug, Clone, Default)]
pub struct PciBusSnapshot {
    devices: Vec<PciDeviceSnapshot>,
}

#[derive(Debug, Clone)]
struct PciDeviceSnapshot {
    bdf: PciBdf,
    config: PciConfigSpaceState,
}

impl PciBusSnapshot {
    pub fn save_from(bus: &PciBus) -> Self {
        let mut devices = Vec::with_capacity(bus.devices.len());
        for (bdf, dev) in bus.devices.iter() {
            devices.push(PciDeviceSnapshot {
                bdf: *bdf,
                config: dev.config().snapshot_state(),
            });
        }
        Self { devices }
    }

    pub fn restore_into(&self, bus: &mut PciBus) -> SnapshotResult<()> {
        for entry in &self.devices {
            let Some(dev) = bus.devices.get_mut(&entry.bdf) else {
                continue;
            };

            // Snapshot restore is only valid when the bus topology matches (same BDFs and same
            // device types). To stay forward-compatible with machine profiles that may repurpose
            // a BDF for a different device, validate the PCI identity before applying the saved
            // config-space image.
            let current_id = dev.config().vendor_device_id();
            let snapshot_vendor_id =
                u16::from_le_bytes([entry.config.bytes[0], entry.config.bytes[1]]);
            let snapshot_device_id =
                u16::from_le_bytes([entry.config.bytes[2], entry.config.bytes[3]]);
            if current_id.vendor_id != snapshot_vendor_id
                || current_id.device_id != snapshot_device_id
            {
                continue;
            }

            dev.config_mut().restore_state(&entry.config);
        }

        bus.mapped_bars.clear();
        for bdf in bus.iter_device_addrs().collect::<Vec<_>>() {
            let (command, bar_ranges) = {
                let Some(dev) = bus.devices.get(&bdf) else {
                    continue;
                };
                let cfg = dev.config();
                (
                    cfg.command(),
                    core::array::from_fn(|index| cfg.bar_range(index as u8)),
                )
            };

            bus.refresh_device_decoding(bdf, command, &bar_ranges);
        }

        Ok(())
    }
}

impl IoSnapshot for PciBusSnapshot {
    const DEVICE_ID: [u8; 4] = *b"PCIB";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_DEVICES: u16 = 1;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let mut enc = Encoder::new().u32(self.devices.len() as u32);
        for entry in &self.devices {
            enc = enc
                .u8(entry.bdf.bus)
                .u8(entry.bdf.device)
                .u8(entry.bdf.function)
                .bytes(&entry.config.bytes);

            for i in 0..6 {
                enc = enc
                    .u64(entry.config.bar_base[i])
                    .bool(entry.config.bar_probe[i]);
            }
        }

        w.field_bytes(TAG_DEVICES, enc.finish());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        use crate::pci::capabilities::PCI_CONFIG_SPACE_SIZE;

        const TAG_DEVICES: u16 = 1;
        const MAX_PCI_FUNCTIONS: usize = 256 * 32 * 8;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.devices.clear();

        let Some(buf) = r.bytes(TAG_DEVICES) else {
            return Ok(());
        };

        let mut d = Decoder::new(buf);
        let count = d.u32()? as usize;
        if count > MAX_PCI_FUNCTIONS {
            return Err(SnapshotError::InvalidFieldEncoding(
                "too many PCI BDF entries",
            ));
        }
        let mut by_bdf = BTreeMap::new();
        for _ in 0..count {
            let bus = d.u8()?;
            let device = d.u8()?;
            let function = d.u8()?;
            if device >= 32 || function >= 8 {
                return Err(SnapshotError::InvalidFieldEncoding("invalid PCI BDF"));
            }
            let bdf = PciBdf::new(bus, device, function);

            let mut config_bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
            config_bytes.copy_from_slice(d.bytes(PCI_CONFIG_SPACE_SIZE)?);

            let mut bar_base = [0u64; 6];
            let mut bar_probe = [false; 6];
            for i in 0..6 {
                bar_base[i] = d.u64()?;
                bar_probe[i] = d.bool()?;
            }

            let config = PciConfigSpaceState {
                bytes: config_bytes,
                bar_base,
                bar_probe,
            };
            if by_bdf.insert(bdf, config).is_some() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "duplicate PCI BDF entry",
                ));
            }
        }
        d.finish()?;

        self.devices = by_bdf
            .into_iter()
            .map(|(bdf, config)| PciDeviceSnapshot { bdf, config })
            .collect();

        Ok(())
    }
}

/// Emulation of PCI Configuration Mechanism #1 (0xCF8/0xCFC).
#[derive(Debug, Default)]
pub struct PciConfigMechanism1 {
    addr: u32,
}

const CONFIG_ADDRESS_PORT: u16 = 0xCF8;
const CONFIG_ADDRESS_PORT_END: u16 = CONFIG_ADDRESS_PORT + 3;
const CONFIG_DATA_PORT: u16 = 0xCFC;
const CONFIG_DATA_PORT_END: u16 = CONFIG_DATA_PORT + 3;

impl PciConfigMechanism1 {
    pub fn new() -> Self {
        Self { addr: 0 }
    }

    pub fn io_read(&mut self, pci: &mut PciBus, port: u16, size: u8) -> u32 {
        match port {
            CONFIG_ADDRESS_PORT..=CONFIG_ADDRESS_PORT_END => {
                if !valid_dword_window_access(port, size, CONFIG_ADDRESS_PORT) {
                    return all_ones(size);
                }
                read_u32_part(self.addr, port, size, CONFIG_ADDRESS_PORT)
            }
            CONFIG_DATA_PORT..=CONFIG_DATA_PORT_END => {
                if !valid_dword_window_access(port, size, CONFIG_DATA_PORT) {
                    return all_ones(size);
                }
                if (self.addr & 0x8000_0000) == 0 {
                    return all_ones(size);
                }
                let bus = ((self.addr >> 16) & 0xFF) as u8;
                let device = ((self.addr >> 11) & 0x1F) as u8;
                let function = ((self.addr >> 8) & 0x07) as u8;
                let reg = (self.addr & 0xFC) as u16;
                let offset = reg + (port - CONFIG_DATA_PORT);
                pci.read_config(PciBdf::new(bus, device, function), offset, size)
            }
            _ => all_ones(size),
        }
    }

    pub fn io_write(&mut self, pci: &mut PciBus, port: u16, size: u8, value: u32) {
        match port {
            CONFIG_ADDRESS_PORT..=CONFIG_ADDRESS_PORT_END => {
                if !valid_dword_window_access(port, size, CONFIG_ADDRESS_PORT) {
                    return;
                }
                self.addr = write_u32_part(self.addr, port, size, value, CONFIG_ADDRESS_PORT);
                // Bits 1:0 are reserved (DWORD-aligned register number) and always read back as 0.
                self.addr &= !0x3;
            }
            CONFIG_DATA_PORT..=CONFIG_DATA_PORT_END => {
                if !valid_dword_window_access(port, size, CONFIG_DATA_PORT) {
                    return;
                }
                if (self.addr & 0x8000_0000) == 0 {
                    return;
                }
                let bus = ((self.addr >> 16) & 0xFF) as u8;
                let device = ((self.addr >> 11) & 0x1F) as u8;
                let function = ((self.addr >> 8) & 0x07) as u8;
                let reg = (self.addr & 0xFC) as u16;
                let offset = reg + (port - CONFIG_DATA_PORT);
                pci.write_config(PciBdf::new(bus, device, function), offset, size, value);
            }
            _ => {}
        }
    }
}

impl IoSnapshot for PciConfigMechanism1 {
    const DEVICE_ID: [u8; 4] = *b"PCF1";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ADDR: u16 = 1;
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_ADDR, self.addr & !0x3);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ADDR: u16 = 1;
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;
        self.addr = r.u32(TAG_ADDR)?.unwrap_or(0) & !0x3;
        Ok(())
    }
}

fn valid_dword_window_access(port: u16, size: u8, base_port: u16) -> bool {
    let offset = port - base_port;
    match size {
        1 | 2 | 4 => u32::from(offset) + u32::from(size) <= 4,
        _ => false,
    }
}

fn read_u32_part(value: u32, port: u16, size: u8, base_port: u16) -> u32 {
    let shift = u32::from(port - base_port) * 8;
    match size {
        1 => (value >> shift) & 0xFF,
        2 => (value >> shift) & 0xFFFF,
        4 => value,
        _ => panic!("invalid read size {size}"),
    }
}

fn write_u32_part(old: u32, port: u16, size: u8, value: u32, base_port: u16) -> u32 {
    let shift = u32::from(port - base_port) * 8;
    match size {
        1 => {
            let mask = 0xFFu32 << shift;
            (old & !mask) | ((value & 0xFF) << shift)
        }
        2 => {
            let mask = 0xFFFFu32 << shift;
            (old & !mask) | ((value & 0xFFFF) << shift)
        }
        4 => value,
        _ => panic!("invalid write size {size}"),
    }
}

fn all_ones(size: u8) -> u32 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        4 => 0xFFFF_FFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[cfg(test)]
mod tests {
    use super::{PciBus, PciConfigMechanism1};
    use crate::pci::config::{PciBarDefinition, PciConfigSpace, PciDevice};
    use crate::pci::PciBdf;

    fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        0x8000_0000
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | ((offset as u32) & 0xFC)
    }

    struct Stub {
        cfg: PciConfigSpace,
    }

    impl Stub {
        fn new(vendor_id: u16, device_id: u16) -> Self {
            Self {
                cfg: PciConfigSpace::new(vendor_id, device_id),
            }
        }
    }

    impl PciDevice for Stub {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    #[test]
    fn config_address_decode_and_subword_access() {
        let mut bus = PciBus::new();
        let mut cfg = PciConfigMechanism1::new();

        let mut dev = Stub::new(0x1234, 0xABCD);
        dev.cfg.set_class_code(0x01, 0x06, 0x01, 0x02);
        bus.add_device(PciBdf::new(0x12, 3, 5), Box::new(dev));

        let addr0 = cfg_addr(0x12, 3, 5, 0x00);
        cfg.io_write(&mut bus, 0xCF8, 4, addr0);
        assert_eq!(cfg.io_read(&mut bus, 0xCF8, 4), addr0);
        assert_eq!(cfg.io_read(&mut bus, 0xCF8 + 1, 1), (addr0 >> 8) & 0xFF);
        assert_eq!(cfg.io_read(&mut bus, 0xCF8 + 2, 2), (addr0 >> 16) & 0xFFFF);

        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xABCD_1234);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 2, 2), 0xABCD);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 1, 1), 0x12);

        // Byte writes to 0xCFB should update the enable bit (bit 31).
        cfg.io_write(&mut bus, 0xCF8 + 3, 1, 0x00);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xFFFF_FFFF);
        cfg.io_write(&mut bus, 0xCF8 + 3, 1, 0x80);

        cfg.io_write(&mut bus, 0xCF8, 4, cfg_addr(0x12, 3, 5, 0x08));
        // revision=0x02 prog_if=0x01 subclass=0x06 class=0x01
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0x0106_0102);
    }

    #[test]
    fn reads_from_absent_devices_return_all_ones_by_width() {
        let mut bus = PciBus::new();
        let mut cfg = PciConfigMechanism1::new();

        cfg.io_write(&mut bus, 0xCF8, 4, cfg_addr(0, 10, 0, 0));
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xFFFF_FFFF);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 3, 1), 0xFF);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 2, 2), 0xFFFF);

        // Disabled enable bit should also float high.
        cfg.io_write(&mut bus, 0xCF8, 4, 0);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 1), 0xFF);
    }

    #[test]
    fn bar_size_probe_via_config_mech1() {
        let mut bus = PciBus::new();
        let mut cfg = PciConfigMechanism1::new();

        let mut dev = Stub::new(0x1234, 0x0001);
        dev.cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        dev.cfg
            .set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
        bus.add_device(PciBdf::new(0, 1, 0), Box::new(dev));

        // Probe BAR0 (MMIO).
        cfg.io_write(&mut bus, 0xCF8, 4, cfg_addr(0, 1, 0, 0x10));
        cfg.io_write(&mut bus, 0xCFC, 4, 0xFFFF_FFFF);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xFFFF_F000);

        // Program BAR0 and read back.
        cfg.io_write(&mut bus, 0xCFC, 4, 0x1234_5000);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0x1234_5000);

        // Probe BAR1 (I/O).
        cfg.io_write(&mut bus, 0xCF8, 4, cfg_addr(0, 1, 0, 0x14));
        cfg.io_write(&mut bus, 0xCFC, 4, 0xFFFF_FFFF);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xFFFF_FFE1);

        cfg.io_write(&mut bus, 0xCFC, 4, 0x0000_0C20);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0x0000_0C21);
    }

    #[test]
    fn command_register_byte_write_updates_decoding() {
        let mut bus = PciBus::new();
        let bdf = PciBdf::new(0, 4, 0);

        let mut dev = Stub::new(0x1234, 0x0002);
        dev.cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        dev.cfg.set_bar_base(0, 0xE000_0000);
        bus.add_device(bdf, Box::new(dev));

        assert!(bus.mapped_bars().is_empty());

        // Enable memory decoding via a byte write to the command register.
        bus.write_config(bdf, 0x04, 1, 0x02);
        let mapped = bus.mapped_mmio_bars();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].bdf, bdf);
        assert_eq!(mapped[0].bar, 0);

        // Disable decoding again and ensure the mapping is dropped.
        bus.write_config(bdf, 0x04, 1, 0x00);
        assert!(bus.mapped_bars().is_empty());

        // Dword writes that cover the command register should also refresh decoding.
        bus.write_config(bdf, 0x04, 4, 0x0000_0002);
        assert_eq!(bus.mapped_mmio_bars().len(), 1);
    }
}
