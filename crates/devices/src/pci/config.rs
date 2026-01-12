use super::capabilities::{
    PciCapability, PciCapabilityInfo, PCI_CAP_PTR_OFFSET, PCI_CONFIG_SPACE_SIZE,
    PCI_STATUS_CAPABILITIES_LIST, PCI_STATUS_OFFSET,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciVendorDeviceId {
    pub vendor_id: u16,
    pub device_id: u16,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciSubsystemIds {
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciClassCode {
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision_id: u8,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciBarKind {
    Io,
    Mmio32,
    Mmio64,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciBarDefinition {
    Io { size: u32 },
    Mmio32 { size: u32, prefetchable: bool },
    Mmio64 { size: u64, prefetchable: bool },
}

impl PciBarDefinition {
    pub fn kind(&self) -> PciBarKind {
        match self {
            Self::Io { .. } => PciBarKind::Io,
            Self::Mmio32 { .. } => PciBarKind::Mmio32,
            Self::Mmio64 { .. } => PciBarKind::Mmio64,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::Io { size } => u64::from(*size),
            Self::Mmio32 { size, .. } => u64::from(*size),
            Self::Mmio64 { size, .. } => *size,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciBarRange {
    pub kind: PciBarKind,
    pub base: u64,
    pub size: u64,
}

impl PciBarRange {
    pub fn end_exclusive(&self) -> u64 {
        self.base.saturating_add(self.size)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciCommandChange {
    Unchanged,
    Changed { old: u16, new: u16 },
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciBarChange {
    Unchanged,
    Changed { old: PciBarRange, new: PciBarRange },
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciConfigWriteEffects {
    pub command: PciCommandChange,
    pub bar: Option<(u8, PciBarChange)>,
}

impl Default for PciConfigWriteEffects {
    fn default() -> Self {
        Self {
            command: PciCommandChange::Unchanged,
            bar: None,
        }
    }
}

#[derive(Debug, Clone)]
struct PciBarState {
    def: Option<PciBarDefinition>,
    base: u64,
    probe: bool,
}

impl PciBarState {
    fn range(&self) -> Option<PciBarRange> {
        let def = self.def?;
        Some(PciBarRange {
            kind: def.kind(),
            base: self.base,
            size: def.size(),
        })
    }

    fn set_base(&mut self, base: u64) {
        self.base = base;
        self.probe = false;
    }
}

/// PCI configuration space for a Type 0 (endpoint) header.
///
/// This is a small framework that supports:
/// - Standard 256-byte config space reads/writes
/// - PCI capabilities list (used by MSI)
/// - BAR size probing (write 0xFFFF_FFFF then read back size mask)
/// - Tracking BAR address assignments for a resource allocator
pub struct PciConfigSpace {
    bytes: [u8; PCI_CONFIG_SPACE_SIZE],
    capabilities: Vec<Box<dyn PciCapability>>,
    next_cap_offset: u8,
    last_cap_offset: Option<u8>,
    bars: [PciBarState; 6],
}

/// Serializable PCI config-space runtime state.
///
/// Captures the guest-visible config-space bytes plus the internal BAR decode/probe state needed
/// to restore deterministic read/write behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PciConfigSpaceState {
    pub bytes: [u8; PCI_CONFIG_SPACE_SIZE],
    pub bar_base: [u64; 6],
    pub bar_probe: [bool; 6],
}

impl PciConfigSpace {
    pub const INTERRUPT_LINE_OFFSET: u16 = 0x3C;
    pub const INTERRUPT_PIN_OFFSET: u16 = 0x3D;

    pub fn new(vendor_id: u16, device_id: u16) -> Self {
        let mut bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
        bytes[0x00..0x02].copy_from_slice(&vendor_id.to_le_bytes());
        bytes[0x02..0x04].copy_from_slice(&device_id.to_le_bytes());
        bytes[0x0e] = 0x00; // header type (type 0)

        let bars = core::array::from_fn(|_| PciBarState {
            def: None,
            base: 0,
            probe: false,
        });

        Self {
            bytes,
            capabilities: Vec::new(),
            next_cap_offset: 0x40,
            last_cap_offset: None,
            bars,
        }
    }

    pub fn vendor_device_id(&self) -> PciVendorDeviceId {
        PciVendorDeviceId {
            vendor_id: u16::from_le_bytes([self.bytes[0x00], self.bytes[0x01]]),
            device_id: u16::from_le_bytes([self.bytes[0x02], self.bytes[0x03]]),
        }
    }

    pub fn class_code(&self) -> PciClassCode {
        PciClassCode {
            revision_id: self.bytes[0x08],
            prog_if: self.bytes[0x09],
            subclass: self.bytes[0x0a],
            class: self.bytes[0x0b],
        }
    }

    pub fn set_class_code(&mut self, class: u8, subclass: u8, prog_if: u8, revision_id: u8) {
        self.bytes[0x08] = revision_id;
        self.bytes[0x09] = prog_if;
        self.bytes[0x0a] = subclass;
        self.bytes[0x0b] = class;
    }

    pub fn set_subsystem_ids(&mut self, ids: PciSubsystemIds) {
        self.bytes[0x2c..0x2e].copy_from_slice(&ids.subsystem_vendor_id.to_le_bytes());
        self.bytes[0x2e..0x30].copy_from_slice(&ids.subsystem_id.to_le_bytes());
    }

    pub fn command(&self) -> u16 {
        u16::from_le_bytes([self.bytes[0x04], self.bytes[0x05]])
    }

    pub fn set_command(&mut self, command: u16) {
        self.bytes[0x04..0x06].copy_from_slice(&command.to_le_bytes());
    }

    pub fn set_bar_definition(&mut self, index: u8, def: PciBarDefinition) {
        let index = usize::from(index);
        assert!(index < self.bars.len());

        self.bars[index].def = Some(def);
        self.bars[index].base = 0;
        self.bars[index].probe = false;
        self.write_bar_base_to_bytes(index, 0);
    }

    pub fn bar_definition(&self, index: u8) -> Option<PciBarDefinition> {
        self.bars.get(usize::from(index)).and_then(|bar| bar.def)
    }

    pub fn bar_range(&self, index: u8) -> Option<PciBarRange> {
        self.bars
            .get(usize::from(index))
            .and_then(|bar| bar.range())
    }

    pub fn set_bar_base(&mut self, index: u8, base: u64) {
        let index = usize::from(index);
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        let base = bar.def.map_or(base, |def| Self::mask_bar_base(def, base));
        bar.set_base(base);
        self.write_bar_base_to_bytes(index, base);
    }

    pub fn add_capability(&mut self, mut capability: Box<dyn PciCapability>) -> u8 {
        let offset = self.allocate_capability_offset(capability.len());
        capability.set_offset(offset);

        let base = offset as usize;
        self.bytes[base] = capability.id();
        self.bytes[base + 1] = 0;

        if let Some(prev) = self.last_cap_offset {
            self.bytes[prev as usize + 1] = offset;
        } else {
            self.bytes[PCI_CAP_PTR_OFFSET] = offset;
            self.set_status_bit(PCI_STATUS_CAPABILITIES_LIST);
        }

        self.last_cap_offset = Some(offset);
        capability.sync_to_config(&mut self.bytes);
        self.capabilities.push(capability);

        offset
    }

    pub fn read(&mut self, offset: u16, size: usize) -> u32 {
        assert!(matches!(size, 1 | 2 | 4));
        self.sync_capabilities_to_config();

        let offset = offset as usize;
        assert!(offset + size <= PCI_CONFIG_SPACE_SIZE);

        if (0x10..=0x27).contains(&offset) {
            let aligned = offset & !0x3;
            let bar_index = (aligned - 0x10) / 4;
            let value = self.read_bar_register(bar_index);
            let shifted = value >> ((offset - aligned) * 8);
            let mask = match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => unreachable!(),
            };
            return shifted & mask;
        }

        let mut value = 0u32;
        for i in 0..size {
            value |= (self.bytes[offset + i] as u32) << (8 * i);
        }
        value
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
        let _ = self.write_with_effects(offset, size, value);
    }

    pub fn write_with_effects(
        &mut self,
        offset: u16,
        size: usize,
        value: u32,
    ) -> PciConfigWriteEffects {
        assert!(matches!(size, 1 | 2 | 4));
        let offset = offset as usize;
        assert!(offset + size <= PCI_CONFIG_SPACE_SIZE);

        let mut effects = PciConfigWriteEffects::default();

        if (0x10..=0x27).contains(&offset) {
            // BAR writes must be 32-bit aligned accesses.
            let aligned = offset & !0x3;
            assert_eq!(aligned, offset, "BAR writes must be 32-bit aligned");
            assert_eq!(size, 4, "BAR writes must be 32-bit");
            let bar_index = (aligned - 0x10) / 4;
            let (logical_bar, change) = self.write_bar_register(bar_index, value);
            effects.bar = Some((logical_bar as u8, change));
            return effects;
        }

        // The command register is 16 bits at 0x04..=0x05. Guests typically write it as a
        // 16-bit word, but the config mechanism supports byte and dword accesses too.
        let command_overlaps = offset < 0x06 && offset + size > 0x04;
        let old_command = if command_overlaps { self.command() } else { 0 };

        for i in 0..size {
            let addr = offset + i;
            if self.is_read_only_byte(addr) {
                continue;
            }
            self.bytes[addr] = ((value >> (8 * i)) & 0xff) as u8;
        }

        if command_overlaps {
            let new_command = self.command();
            if old_command != new_command {
                effects.command = PciCommandChange::Changed {
                    old: old_command,
                    new: new_command,
                };
            }
        }

        self.sync_capabilities_from_config();
        self.sync_capabilities_to_config();

        effects
    }

    pub fn interrupt_line(&mut self) -> u8 {
        self.read(Self::INTERRUPT_LINE_OFFSET, 1) as u8
    }

    pub fn set_interrupt_line(&mut self, line: u8) {
        self.write(Self::INTERRUPT_LINE_OFFSET, 1, u32::from(line));
    }

    pub fn interrupt_pin(&mut self) -> u8 {
        self.read(Self::INTERRUPT_PIN_OFFSET, 1) as u8
    }

    pub fn set_interrupt_pin(&mut self, pin: u8) {
        self.write(Self::INTERRUPT_PIN_OFFSET, 1, u32::from(pin));
    }

    pub fn capability_list(&mut self) -> Vec<PciCapabilityInfo> {
        self.sync_capabilities_to_config();

        let mut caps = Vec::new();
        let mut offset = self.bytes[PCI_CAP_PTR_OFFSET];
        let mut seen = [false; PCI_CONFIG_SPACE_SIZE];

        while offset != 0 {
            let off = offset as usize;
            if off + 1 >= PCI_CONFIG_SPACE_SIZE {
                break;
            }
            if seen[off] {
                break;
            }
            seen[off] = true;

            let id = self.bytes[off];
            caps.push(PciCapabilityInfo { id, offset });

            offset = self.bytes[off + 1];
        }

        caps
    }

    pub fn find_capability(&mut self, id: u8) -> Option<u8> {
        self.capability_list()
            .into_iter()
            .find(|cap| cap.id == id)
            .map(|cap| cap.offset)
    }

    pub fn capability<T: 'static>(&self) -> Option<&T> {
        self.capabilities
            .iter()
            .find_map(|cap| cap.as_any().downcast_ref::<T>())
    }

    pub fn capability_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.capabilities
            .iter_mut()
            .find_map(|cap| cap.as_any_mut().downcast_mut::<T>())
    }

    pub fn snapshot_state(&self) -> PciConfigSpaceState {
        // Bring capability-backed bytes (MSI, etc.) up to date so the snapshot reflects what the
        // guest would observe on the next config-space read. Snapshotting should not mutate the
        // device, so this synchronizes into a temporary buffer.
        let mut bytes = self.bytes;
        for cap in &self.capabilities {
            cap.sync_to_config(&mut bytes);
        }

        PciConfigSpaceState {
            bytes,
            bar_base: core::array::from_fn(|index| self.bars[index].base),
            bar_probe: core::array::from_fn(|index| self.bars[index].probe),
        }
    }

    pub fn restore_state(&mut self, state: &PciConfigSpaceState) {
        self.bytes = state.bytes;
        for i in 0..self.bars.len() {
            self.bars[i].base = state.bar_base[i];
            self.bars[i].probe = state.bar_probe[i];
        }

        // Restore BAR bytes for known BAR definitions, ensuring the raw config-space bytes match
        // the emulation state used by BAR reads/writes.
        for i in 0..self.bars.len() {
            if self.bars[i].def.is_some() {
                let base = self.bars[i].base;
                self.write_bar_base_to_bytes(i, base);
            }
        }

        self.sync_capabilities_from_config();
        self.sync_capabilities_to_config();
    }

    fn allocate_capability_offset(&mut self, len: u8) -> u8 {
        let offset = self.next_cap_offset;
        let mut next = offset as usize + len as usize;
        next = (next + 3) & !3;
        assert!(next <= PCI_CONFIG_SPACE_SIZE);
        self.next_cap_offset = next as u8;
        offset
    }

    fn set_status_bit(&mut self, bit: u16) {
        let current = u16::from_le_bytes([
            self.bytes[PCI_STATUS_OFFSET],
            self.bytes[PCI_STATUS_OFFSET + 1],
        ]);
        let new = current | bit;
        self.bytes[PCI_STATUS_OFFSET..PCI_STATUS_OFFSET + 2].copy_from_slice(&new.to_le_bytes());
    }

    fn sync_capabilities_to_config(&mut self) {
        for cap in &self.capabilities {
            cap.sync_to_config(&mut self.bytes);
        }
    }

    fn sync_capabilities_from_config(&mut self) {
        for cap in &mut self.capabilities {
            cap.sync_from_config(&mut self.bytes);
        }
    }

    fn is_read_only_byte(&self, addr: usize) -> bool {
        if addr < 0x04 {
            return true;
        }
        if addr == PCI_CAP_PTR_OFFSET {
            return true;
        }

        for cap in &self.capabilities {
            let base = cap.offset() as usize;
            if addr == base || addr == base + 1 {
                return true;
            }
        }

        false
    }

    fn read_bar_register(&self, bar_index: usize) -> u32 {
        if bar_index >= self.bars.len() {
            return 0;
        }

        // High dword of a 64-bit BAR: consult the previous BAR's definition/state.
        if self.bars[bar_index].def.is_none() && bar_index > 0 {
            if let Some(PciBarDefinition::Mmio64 { size, .. }) = self.bars[bar_index - 1].def {
                let low = &self.bars[bar_index - 1];
                if low.probe {
                    return (!(size.saturating_sub(1)) >> 32) as u32;
                }
                return (low.base >> 32) as u32;
            }
        }

        let bar = &self.bars[bar_index];
        let Some(def) = bar.def else {
            return self.read_u32_from_bytes(0x10 + bar_index * 4);
        };

        if bar.probe {
            return match def {
                PciBarDefinition::Io { size } => {
                    let mask = !(size.saturating_sub(1)) & 0xFFFF_FFFC;
                    mask | 0x1
                }
                PciBarDefinition::Mmio32 { size, prefetchable } => {
                    let mut mask = !(size.saturating_sub(1)) & 0xFFFF_FFF0;
                    if prefetchable {
                        mask |= 1 << 3;
                    }
                    mask
                }
                PciBarDefinition::Mmio64 { size, prefetchable } => {
                    let mut mask = !(size.saturating_sub(1)) as u32 & 0xFFFF_FFF0;
                    // bits 2:1 = 0b10 indicate 64-bit
                    mask |= 0b10 << 1;
                    if prefetchable {
                        mask |= 1 << 3;
                    }
                    mask
                }
            };
        }

        match def {
            PciBarDefinition::Io { .. } => (bar.base as u32 & 0xFFFF_FFFC) | 0x1,
            PciBarDefinition::Mmio32 { prefetchable, .. } => {
                let mut val = bar.base as u32 & 0xFFFF_FFF0;
                if prefetchable {
                    val |= 1 << 3;
                }
                val
            }
            PciBarDefinition::Mmio64 { prefetchable, .. } => {
                let mut val = bar.base as u32 & 0xFFFF_FFF0;
                val |= 0b10 << 1;
                if prefetchable {
                    val |= 1 << 3;
                }
                val
            }
        }
    }

    fn write_bar_base_to_bytes(&mut self, bar_index: usize, base: u64) {
        let offset = 0x10 + bar_index * 4;
        self.bytes[offset..offset + 4].copy_from_slice(&(base as u32).to_le_bytes());

        if matches!(
            self.bars.get(bar_index).and_then(|bar| bar.def),
            Some(PciBarDefinition::Mmio64 { .. })
        ) && bar_index + 1 < self.bars.len()
        {
            let hi_off = 0x10 + (bar_index + 1) * 4;
            self.bytes[hi_off..hi_off + 4].copy_from_slice(&((base >> 32) as u32).to_le_bytes());
        }
    }

    fn mask_bar_base(def: PciBarDefinition, base: u64) -> u64 {
        match def {
            PciBarDefinition::Io { size } => {
                let mask = u64::from(!(size.saturating_sub(1)) & 0xFFFF_FFFC);
                base & mask
            }
            PciBarDefinition::Mmio32 { size, .. } => {
                let mask = u64::from(!(size.saturating_sub(1)) & 0xFFFF_FFF0);
                base & mask
            }
            PciBarDefinition::Mmio64 { size, .. } => base & !(size.saturating_sub(1)) & !0xF,
        }
    }

    fn write_bar_register(&mut self, bar_index: usize, value: u32) -> (usize, PciBarChange) {
        if bar_index >= self.bars.len() {
            return (bar_index, PciBarChange::Unchanged);
        }

        // High dword of a 64-bit BAR.
        if self.bars[bar_index].def.is_none()
            && bar_index > 0
            && matches!(
                self.bars[bar_index - 1].def,
                Some(PciBarDefinition::Mmio64 { .. })
            )
        {
            return self.write_bar64_high(bar_index - 1, value);
        }

        let Some(def) = self.bars[bar_index].def else {
            // Unknown BAR type, just store raw.
            let offset = 0x10 + bar_index * 4;
            self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            return (bar_index, PciBarChange::Unchanged);
        };

        if value == 0xFFFF_FFFF {
            self.bars[bar_index].probe = true;
            return (bar_index, PciBarChange::Unchanged);
        }

        let old_range = PciBarRange {
            kind: def.kind(),
            base: self.bars[bar_index].base,
            size: def.size(),
        };

        let new_base = {
            let base = match def {
                PciBarDefinition::Io { .. } => u64::from(value & 0xFFFF_FFFC),
                PciBarDefinition::Mmio32 { .. } => u64::from(value & 0xFFFF_FFF0),
                PciBarDefinition::Mmio64 { .. } => {
                    let low_base = u64::from(value & 0xFFFF_FFF0);
                    let high = self.bars[bar_index].base >> 32;
                    low_base | (high << 32)
                }
            };
            Self::mask_bar_base(def, base)
        };

        self.bars[bar_index].set_base(new_base);
        self.write_bar_base_to_bytes(bar_index, new_base);

        let new_range = PciBarRange {
            kind: def.kind(),
            base: new_base,
            size: def.size(),
        };
        if old_range == new_range {
            (bar_index, PciBarChange::Unchanged)
        } else {
            (
                bar_index,
                PciBarChange::Changed {
                    old: old_range,
                    new: new_range,
                },
            )
        }
    }

    fn write_bar64_high(&mut self, low_index: usize, value: u32) -> (usize, PciBarChange) {
        let Some(def @ PciBarDefinition::Mmio64 { size, .. }) = self.bars[low_index].def else {
            return (low_index, PciBarChange::Unchanged);
        };

        if value == 0xFFFF_FFFF {
            self.bars[low_index].probe = true;
            return (low_index, PciBarChange::Unchanged);
        }

        let old_range = PciBarRange {
            kind: PciBarKind::Mmio64,
            base: self.bars[low_index].base,
            size,
        };

        let low_part = self.bars[low_index].base & 0xFFFF_FFF0;
        let new_base = Self::mask_bar_base(def, low_part | (u64::from(value) << 32));

        self.bars[low_index].set_base(new_base);
        self.write_bar_base_to_bytes(low_index, new_base);

        let new_range = PciBarRange {
            kind: PciBarKind::Mmio64,
            base: new_base,
            size,
        };
        if old_range == new_range {
            (low_index, PciBarChange::Unchanged)
        } else {
            (
                low_index,
                PciBarChange::Changed {
                    old: old_range,
                    new: new_range,
                },
            )
        }
    }

    fn read_u32_from_bytes(&self, offset: usize) -> u32 {
        u32::from_le_bytes([
            self.bytes[offset],
            self.bytes[offset + 1],
            self.bytes[offset + 2],
            self.bytes[offset + 3],
        ])
    }
}

pub trait PciDevice {
    fn config(&self) -> &PciConfigSpace;
    fn config_mut(&mut self) -> &mut PciConfigSpace;

    fn reset(&mut self) {
        // Default: clear command register (BARs remain programmed by firmware / allocator).
        self.config_mut().set_command(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{PciBarDefinition, PciConfigSpace};
    use crate::pci::msi::{MsiCapability, PCI_CAP_ID_MSI};

    #[test]
    fn capability_list_traversal_finds_msi() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));

        let caps = config.capability_list();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].id, PCI_CAP_ID_MSI);
        assert_eq!(caps[0].offset, 0x40);
    }

    #[test]
    fn programming_msi_updates_device_state() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));
        let cap_offset = config.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

        config.write(cap_offset + 0x04, 4, 0xfee0_0000);
        config.write(cap_offset + 0x08, 4, 0);
        config.write(cap_offset + 0x0c, 2, 0x0045);

        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

        let msi = config.capability::<MsiCapability>().unwrap();
        assert!(msi.enabled());
        assert_eq!(msi.message_address(), 0xfee0_0000);
        assert_eq!(msi.message_data(), 0x0045);
    }

    #[test]
    fn bar_writes_are_masked_to_bar_size_alignment() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });

        // Mask to BAR size (0x1000) rather than only to 16 bytes.
        cfg.write(0x10, 4, 0x1234_5678);
        assert_eq!(cfg.bar_range(0).unwrap().base, 0x1234_5000);
        assert_eq!(cfg.read(0x10, 4), 0x1234_5000);

        // Internal callers (firmware/allocator) use `set_bar_base`; it should apply the same mask.
        cfg.set_bar_base(0, 0x1234_5678);
        assert_eq!(cfg.bar_range(0).unwrap().base, 0x1234_5000);

        // I/O BARs mask base to size (0x20) and always return bit0 set in the raw register.
        cfg.write(0x14, 4, 0x1234_5678);
        assert_eq!(cfg.bar_range(1).unwrap().base, 0x1234_5660);
        assert_eq!(cfg.read(0x14, 4), 0x1234_5661);
    }
}
