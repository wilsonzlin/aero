use crate::pci::config::{PciBarChange, PciCommandChange, PciConfigSpace, PciConfigWriteEffects};
use crate::pci::{PciBarKind, PciBarRange, PciBdf, PciDevice};
use crate::pci::{PciResourceAllocator, PciResourceError};
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
            .map(|((bdf, bar), range)| PciMappedBar { bdf: *bdf, bar: *bar, range: *range })
            .collect()
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

        // Allocate BARs in deterministic order: ascending BDF then BAR index.
        for bdf in self.iter_device_addrs().collect::<Vec<_>>() {
            let dev = self.devices.get_mut(&bdf).expect("device disappeared");
            for bar_index in 0u8..6u8 {
                let def = dev.config().bar_definition(bar_index);
                let Some(def) = def else { continue };

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

        if let Some((bar, change)) = effects.bar {
            if let PciBarChange::Changed { old, new } = change {
                // BAR updates only affect mappings if decoding is enabled.
                self.apply_bar_change(bdf, command, bar, old, new);
            }
        }
    }

    fn refresh_device_decoding(&mut self, bdf: PciBdf, command: u16, bar_ranges: &[Option<PciBarRange>; 6]) {
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

/// Emulation of PCI Configuration Mechanism #1 (0xCF8/0xCFC).
#[derive(Debug, Default)]
pub struct PciConfigMechanism1 {
    addr: u32,
}

impl PciConfigMechanism1 {
    pub fn new() -> Self {
        Self { addr: 0 }
    }

    pub fn io_read(&mut self, pci: &mut PciBus, port: u16, size: u8) -> u32 {
        match port {
            0xCF8 => {
                // Only 32-bit reads are meaningful, but return the stored value.
                read_u32_part(self.addr, port, size)
            }
            0xCFC..=0xCFF => {
                if (self.addr & 0x8000_0000) == 0 {
                    return all_ones(size);
                }
                let bus = ((self.addr >> 16) & 0xFF) as u8;
                let device = ((self.addr >> 11) & 0x1F) as u8;
                let function = ((self.addr >> 8) & 0x07) as u8;
                let reg = (self.addr & 0xFC) as u16;
                let offset = reg + u16::from(port - 0xCFC);
                pci.read_config(PciBdf::new(bus, device, function), offset, size)
            }
            _ => all_ones(size),
        }
    }

    pub fn io_write(&mut self, pci: &mut PciBus, port: u16, size: u8, value: u32) {
        match port {
            0xCF8 => {
                self.addr = write_u32_part(self.addr, size, value);
            }
            0xCFC..=0xCFF => {
                if (self.addr & 0x8000_0000) == 0 {
                    return;
                }
                let bus = ((self.addr >> 16) & 0xFF) as u8;
                let device = ((self.addr >> 11) & 0x1F) as u8;
                let function = ((self.addr >> 8) & 0x07) as u8;
                let reg = (self.addr & 0xFC) as u16;
                let offset = reg + u16::from(port - 0xCFC);
                pci.write_config(PciBdf::new(bus, device, function), offset, size, value);
            }
            _ => {}
        }
    }
}

fn read_u32_part(value: u32, _port: u16, size: u8) -> u32 {
    match size {
        1 => value & 0xFF,
        2 => value & 0xFFFF,
        4 => value,
        _ => panic!("invalid read size {size}"),
    }
}

fn write_u32_part(old: u32, size: u8, value: u32) -> u32 {
    match size {
        1 => (old & !0xFF) | (value & 0xFF),
        2 => (old & !0xFFFF) | (value & 0xFFFF),
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

        cfg.io_write(&mut bus, 0xCF8, 4, cfg_addr(0x12, 3, 5, 0x00));
        assert_eq!(cfg.io_read(&mut bus, 0xCFC, 4), 0xABCD_1234);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 2, 2), 0xABCD);
        assert_eq!(cfg.io_read(&mut bus, 0xCFC + 1, 1), 0x12);

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
}
