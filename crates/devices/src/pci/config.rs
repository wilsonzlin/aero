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
    pub const HEADER_TYPE_OFFSET: u16 = 0x0E;

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

    pub fn header_type(&self) -> u8 {
        self.bytes[usize::from(Self::HEADER_TYPE_OFFSET)]
    }

    pub fn set_header_type(&mut self, header_type: u8) {
        // Header Type (0x0E) is read-only from the guest's perspective. Allow device/platform code
        // to set it directly.
        self.bytes[usize::from(Self::HEADER_TYPE_OFFSET)] = header_type;
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

        match def {
            PciBarDefinition::Io { size } => {
                assert!(
                    size.is_power_of_two(),
                    "PCI I/O BAR size must be a power of two"
                );
                assert!(size >= 4, "PCI I/O BAR size must be at least 4 bytes");
            }
            PciBarDefinition::Mmio32 { size, .. } => {
                assert!(
                    size.is_power_of_two(),
                    "PCI MMIO32 BAR size must be a power of two"
                );
                assert!(size >= 0x10, "PCI MMIO BAR size must be at least 16 bytes");
            }
            PciBarDefinition::Mmio64 { size, .. } => {
                assert!(
                    size.is_power_of_two(),
                    "PCI MMIO64 BAR size must be a power of two"
                );
                assert!(size >= 0x10, "PCI MMIO BAR size must be at least 16 bytes");
            }
        }

        // BAR layout constraints:
        // - A 64-bit BAR consumes the next BAR slot as its high dword.
        // - Therefore, BAR(N) cannot be independently defined when BAR(N-1) is a 64-bit BAR.
        //
        // This is a programming-time invariant for device models/profiles; assert to keep the
        // config space internally consistent and to avoid stale values being exposed via config
        // reads.
        if index > 0
            && matches!(
                self.bars[index - 1].def,
                Some(PciBarDefinition::Mmio64 { .. })
            )
        {
            panic!("BAR{index} overlaps 64-bit BAR{} high dword", index - 1);
        }

        // If we're overwriting an existing 64-bit BAR definition, clear the stale high dword
        // config bytes now that BAR(N+1) will no longer be treated as the implicit high register.
        if matches!(self.bars[index].def, Some(PciBarDefinition::Mmio64 { .. }))
            && index + 1 < self.bars.len()
        {
            let hi_off = 0x10 + (index + 1) * 4;
            self.bytes[hi_off..hi_off + 4].fill(0);
            self.bars[index + 1].def = None;
            self.bars[index + 1].base = 0;
            self.bars[index + 1].probe = false;
        }

        // If the new BAR is 64-bit, clear and reserve BAR(N+1) as the high dword slot.
        if matches!(def, PciBarDefinition::Mmio64 { .. }) {
            assert!(index + 1 < self.bars.len(), "64-bit BAR must not be BAR5");
            assert!(
                self.bars[index + 1].def.is_none(),
                "BAR{} overlaps 64-bit BAR{index}",
                index + 1
            );
            let hi_off = 0x10 + (index + 1) * 4;
            self.bytes[hi_off..hi_off + 4].fill(0);
            self.bars[index + 1].def = None;
            self.bars[index + 1].base = 0;
            self.bars[index + 1].probe = false;
        }

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

    /// Synchronize capability-backed state into the raw config space byte array.
    ///
    /// PCI config space reads and writes already synchronize capabilities implicitly, but device
    /// models may mutate capability state internally (e.g. MSI pending bits when a vector is
    /// masked). Calling this method is a convenient way to ensure the raw config-space byte image
    /// reflects those internal updates immediately.
    pub fn sync_capabilities(&mut self) {
        self.sync_capabilities_to_config();
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

        // Ensure the raw backing bytes reflect the latest capability state before applying a config
        // write.
        //
        // Device models may mutate capability state internally (for example, MSI pending bits when a
        // vector is masked). If a guest then performs a config-space write (for example, to unmask
        // MSI), `sync_capabilities_from_config()` below must see the up-to-date pending bits in the
        // byte array, otherwise it would clobber the device-managed fields.
        self.sync_capabilities_to_config();

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
        // Interrupt Pin (0x3D) is read-only from the guest's perspective. Allow device/platform
        // code to set it directly.
        self.bytes[usize::from(Self::INTERRUPT_PIN_OFFSET)] = pin;
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

    /// Disables MSI/MSI-X interrupt delivery in PCI configuration space.
    ///
    /// This is intended for platform/device reset flows: real hardware clears MSI/MSI-X enable
    /// bits on reset so a reboot starts from a sane interrupt baseline.
    ///
    /// In addition to disabling delivery, this also clears any device-latched pending state:
    /// - MSI per-vector mask/pending bits (when present), and
    /// - MSI-X PBA pending bits.
    ///
    /// Without this, a device reset followed by re-enabling MSI/MSI-X could spuriously deliver
    /// stale interrupts.
    ///
    /// This is capability-aware and defensive: it first locates the capabilities via the standard
    /// PCI capabilities list, and then bounds-checks the computed config-space offsets.
    pub fn disable_msi_msix(&mut self) {
        // Disable MSI by clearing Message Control bit 0 (MSI Enable).
        //
        // Also clear per-vector mask/pending registers when present so the next boot starts with a
        // deterministic MSI state (real hardware resets these to 0 as part of function reset).
        if let Some(cap) = self.find_capability(crate::pci::msi::PCI_CAP_ID_MSI) {
            let ctrl_off = usize::from(cap).saturating_add(0x02);
            if ctrl_off.saturating_add(2) <= PCI_CONFIG_SPACE_SIZE {
                if let Some(msi) = self.capability_mut::<crate::pci::MsiCapability>() {
                    // MSI pending bits are device-managed and read-only to the guest; clear them
                    // directly rather than via a config-space write.
                    msi.clear_pending_bits();
                }

                let off = u16::from(cap).saturating_add(0x02);
                let ctrl = self.read(off, 2) as u16;
                self.write(off, 2, u32::from(ctrl & !0x0001));

                let is_64bit = (ctrl & (1 << 7)) != 0;
                let per_vector_masking = (ctrl & (1 << 8)) != 0;
                if per_vector_masking {
                    let mask_off = if is_64bit {
                        u16::from(cap).saturating_add(0x10)
                    } else {
                        u16::from(cap).saturating_add(0x0c)
                    };

                    // These registers are present only when `per_vector_masking` is set. They are
                    // expected to reset to 0.
                    if usize::from(mask_off).saturating_add(4) <= PCI_CONFIG_SPACE_SIZE {
                        self.write(mask_off, 4, 0);
                    }
                }
            }
        }

        // Disable MSI-X by clearing Message Control bits 15 (MSI-X Enable) and 14 (Function Mask).
        //
        // Also clear any latched pending bits in the MSI-X Pending Bit Array (PBA) so a subsequent
        // re-enable cannot deliver stale interrupts after reset.
        if let Some(cap) = self.find_capability(crate::pci::msix::PCI_CAP_ID_MSIX) {
            let ctrl_off = usize::from(cap).saturating_add(0x02);
            if ctrl_off.saturating_add(2) <= PCI_CONFIG_SPACE_SIZE {
                let off = u16::from(cap).saturating_add(0x02);
                let ctrl = self.read(off, 2) as u16;
                self.write(off, 2, u32::from(ctrl & !((1 << 15) | (1 << 14))));
            }
        }

        if let Some(msix) = self.capability_mut::<crate::pci::MsixCapability>() {
            msix.clear_pba_pending_bits();
        }
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
        // Several PCI header bytes are read-only and defined by the device model (vendor/device
        // ID, class code, header type, subsystem IDs, interrupt pin). Guests cannot change them,
        // but snapshots may be corrupt/hostile. Preserve the model-defined values from the
        // currently constructed config space rather than trusting the snapshot bytes.
        let ro_vendor_device: [u8; 4] = self.bytes[0x00..0x04].try_into().unwrap();
        let ro_class_code: [u8; 4] = self.bytes[0x08..0x0c].try_into().unwrap();
        let ro_header_type = self.bytes[usize::from(Self::HEADER_TYPE_OFFSET)];
        let ro_subsystem: [u8; 4] = self.bytes[0x2c..0x30].try_into().unwrap();
        let ro_interrupt_pin = self.bytes[usize::from(Self::INTERRUPT_PIN_OFFSET)];

        self.bytes = state.bytes;
        self.bytes[0x00..0x04].copy_from_slice(&ro_vendor_device);
        self.bytes[0x08..0x0c].copy_from_slice(&ro_class_code);
        self.bytes[usize::from(Self::HEADER_TYPE_OFFSET)] = ro_header_type;
        self.bytes[0x2c..0x30].copy_from_slice(&ro_subsystem);
        self.bytes[usize::from(Self::INTERRUPT_PIN_OFFSET)] = ro_interrupt_pin;

        for i in 0..self.bars.len() {
            // BAR base alignment is a guest-visible hardware invariant: real devices mask BAR base
            // writes to the BAR size alignment (in addition to the config-space flag bits).
            //
            // Snapshots may come from older versions or hostile inputs; normalize restored BAR
            // bases through the same mask so the post-restore config space and BAR routing behave
            // like real hardware.
            self.bars[i].base = self
                .bars
                .get(i)
                .and_then(|bar| bar.def)
                .map_or(state.bar_base[i], |def| {
                    Self::mask_bar_base(def, state.bar_base[i])
                });
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
        self.sync_capabilities_list_to_config();
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

    fn clear_status_bit(&mut self, bit: u16) {
        let current = u16::from_le_bytes([
            self.bytes[PCI_STATUS_OFFSET],
            self.bytes[PCI_STATUS_OFFSET + 1],
        ]);
        let new = current & !bit;
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

    /// Rebuild the PCI capabilities list pointers (and related read-only header bytes) from the
    /// internally registered capabilities.
    ///
    /// Guests cannot modify these bytes (they are treated as read-only in `write_with_effects`),
    /// but snapshot restore may provide corrupt or hostile config bytes. Keeping the list pointer
    /// chain consistent avoids panics in code that traverses the list and then performs bounded
    /// reads/writes relative to the discovered offsets.
    fn sync_capabilities_list_to_config(&mut self) {
        if self.capabilities.is_empty() {
            self.bytes[PCI_CAP_PTR_OFFSET] = 0;
            self.clear_status_bit(PCI_STATUS_CAPABILITIES_LIST);
            return;
        }

        self.bytes[PCI_CAP_PTR_OFFSET] = self.capabilities[0].offset();
        self.set_status_bit(PCI_STATUS_CAPABILITIES_LIST);

        for (index, cap) in self.capabilities.iter().enumerate() {
            let base = cap.offset() as usize;
            // Offsets are allocated with bounds checks; this is defensive against corrupt
            // capability implementations.
            if base + 1 >= PCI_CONFIG_SPACE_SIZE {
                continue;
            }

            self.bytes[base] = cap.id();
            self.bytes[base + 1] = self
                .capabilities
                .get(index + 1)
                .map_or(0, |next| next.offset());
        }
    }

    fn is_read_only_byte(&self, addr: usize) -> bool {
        if addr < 0x04 {
            return true;
        }
        // Revision ID / Class Code bytes (0x08..=0x0B) are read-only.
        if (0x08..=0x0B).contains(&addr) {
            return true;
        }
        // Header Type (0x0E) is read-only.
        if addr == usize::from(Self::HEADER_TYPE_OFFSET) {
            return true;
        }
        // The Status register (0x06..=0x07) is largely read-only / RW1C on real hardware. Guests
        // commonly perform 32-bit writes to the Command register at 0x04 and write zeros in the
        // upper 16 bits; those writes must not clobber device-managed status bits such as the
        // Capabilities List flag.
        if (PCI_STATUS_OFFSET..PCI_STATUS_OFFSET + 2).contains(&addr) {
            return true;
        }
        // Interrupt Pin (0x3D) is read-only. Guests may perform 32-bit writes to Interrupt Line
        // (0x3C) with zeros in the upper bytes; those writes must not clobber the device-reported
        // pin.
        if addr == usize::from(Self::INTERRUPT_PIN_OFFSET) {
            return true;
        }
        // Subsystem IDs (0x2C..=0x2F) are read-only.
        if (0x2C..0x30).contains(&addr) {
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

            // MSI pending bits are device-managed and read-only from the guest's perspective.
            //
            // We enforce this at the config-space write layer rather than inside `MsiCapability` so
            // snapshot restore can still round-trip pending bits via `PciConfigSpaceState::bytes`.
            if let Some(msi) = cap
                .as_any()
                .downcast_ref::<crate::pci::msi::MsiCapability>()
            {
                if msi.per_vector_masking() {
                    let pending_off = base + if msi.is_64bit() { 0x14 } else { 0x10 };
                    if addr >= pending_off && addr < pending_off + 4 {
                        return true;
                    }
                }
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
        //
        // Also clear MSI/MSI-X enable state so a platform reset (machine reset / BIOS POST) starts
        // from a sane baseline. This also clears any latched MSI/MSI-X pending state (MSI pending
        // bits, MSI-X PBA bits) so a reset cannot later deliver stale interrupts.
        //
        // Some device models reset their internal MSI/MSI-X routing (e.g. vector selects) but rely
        // on PCI capability enable bits to decide whether to suppress legacy INTx; leaving MSI-X
        // enabled across reset can therefore result in a device that never interrupts until the
        // guest reprograms MSI-X.
        let cfg = self.config_mut();
        cfg.set_command(0);
        cfg.disable_msi_msix();
    }
}

#[cfg(test)]
mod tests {
    use super::{PciBarDefinition, PciConfigSpace, PciDevice, PciSubsystemIds};
    use crate::pci::capabilities::{
        PCI_CAP_PTR_OFFSET, PCI_STATUS_CAPABILITIES_LIST, PCI_STATUS_OFFSET,
    };
    use crate::pci::msi::{MsiCapability, PCI_CAP_ID_MSI};
    use crate::pci::msix::{MsixCapability, PCI_CAP_ID_MSIX};
    use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};

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
    fn dword_write_to_command_does_not_clobber_status_register() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));

        let status_before = config.read(0x06, 2) as u16;
        assert_ne!(
            status_before & PCI_STATUS_CAPABILITIES_LIST,
            0,
            "sanity check: capabilities list bit should be set after adding a capability",
        );

        // Guests may write the Command register using a 32-bit write, with zeros in the upper
        // 16 bits. The upper half maps to the Status register, which must not be clobbered.
        config.write(0x04, 4, 0x0000_0006); // MEM + BME

        let status_after = config.read(0x06, 2) as u16;
        assert_eq!(
            status_after, status_before,
            "32-bit Command writes must not overwrite the Status register"
        );

        let command_after = config.read(0x04, 2) as u16;
        assert_eq!(command_after, 0x0006);
    }

    #[test]
    fn dword_write_to_interrupt_line_does_not_clobber_interrupt_pin() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.set_interrupt_pin(1);

        let pin_before = config.read(PciConfigSpace::INTERRUPT_PIN_OFFSET, 1) as u8;
        assert_eq!(pin_before, 1);

        // Guests may write Interrupt Line using a 32-bit access; upper bytes include Interrupt Pin
        // and should not be clobbered.
        config.write(0x3C, 4, 0x0000_000A);

        let pin_after = config.read(PciConfigSpace::INTERRUPT_PIN_OFFSET, 1) as u8;
        assert_eq!(pin_after, pin_before);
        assert_eq!(
            config.read(PciConfigSpace::INTERRUPT_LINE_OFFSET, 1) as u8,
            0x0A
        );
    }

    #[test]
    fn dword_write_to_cache_line_size_does_not_clobber_header_type() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.set_header_type(0x80); // multifunction bit

        // Dword write at 0x0C spans:
        // - Cache Line Size (0x0C)
        // - Latency Timer (0x0D)
        // - Header Type (0x0E, read-only)
        // - BIST (0x0F)
        config.write(0x0C, 4, 0x12_00_11_22);

        assert_eq!(config.header_type(), 0x80);
        assert_eq!(config.read(0x0C, 1) as u8, 0x22);
        assert_eq!(config.read(0x0D, 1) as u8, 0x11);
        assert_eq!(config.read(0x0F, 1) as u8, 0x12);
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
    fn config_write_preserves_device_managed_msi_pending_bits() {
        // Regression test: device models may mutate MSI capability state internally (pending bits)
        // without immediately syncing it into the raw backing byte array. A subsequent config-space
        // write must not clobber that device-managed state.
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));
        let cap_offset = config.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

        // Program and enable MSI.
        config.write(cap_offset + 0x04, 4, 0xfee0_0000);
        config.write(cap_offset + 0x08, 4, 0);
        config.write(cap_offset + 0x0c, 2, 0x0045);
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

        let is_64bit = (ctrl & (1 << 7)) != 0;
        let per_vector_masking = (ctrl & (1 << 8)) != 0;
        assert!(
            per_vector_masking,
            "test requires per-vector masking support"
        );
        let mask_off = if is_64bit {
            cap_offset + 0x10
        } else {
            cap_offset + 0x0c
        };

        // Mask the vector so triggering will set the pending bit instead of delivering.
        config.write(mask_off, 4, 1);

        struct Sink;
        impl MsiTrigger for Sink {
            fn trigger_msi(&mut self, _message: MsiMessage) {}
        }

        // Trigger while masked to set the pending bit in the capability state.
        {
            let msi = config.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
            assert_eq!(msi.pending_bits() & 1, 1);
        }

        // Unmask via a config-space write. This write must not clear the pending bit.
        config.write(mask_off, 4, 0);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );
    }

    #[test]
    fn config_write_preserves_device_managed_msi_pending_bits_32bit() {
        // Same as `config_write_preserves_device_managed_msi_pending_bits`, but covering the 32-bit
        // MSI capability layout (no Message Address High dword).
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new_with_config(false, true)));
        let cap_offset = config.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

        // Program and enable MSI (32-bit layout).
        config.write(cap_offset + 0x04, 4, 0xfee0_0000);
        config.write(cap_offset + 0x08, 2, 0x0045);
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        assert_eq!(
            ctrl & (1 << 7),
            0,
            "test requires 32-bit MSI capability layout"
        );
        config.write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

        let per_vector_masking = (ctrl & (1 << 8)) != 0;
        assert!(
            per_vector_masking,
            "test requires per-vector masking support"
        );
        let mask_off = cap_offset + 0x0c;

        // Mask the vector so triggering will set the pending bit instead of delivering.
        config.write(mask_off, 4, 1);

        struct Sink;
        impl MsiTrigger for Sink {
            fn trigger_msi(&mut self, _message: MsiMessage) {}
        }

        // Trigger while masked to set the pending bit in the capability state.
        {
            let msi = config.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
            assert_eq!(msi.pending_bits() & 1, 1);
        }

        // Unmask via a config-space write. This write must not clear the pending bit.
        config.write(mask_off, 4, 0);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );
    }

    #[test]
    fn msi_pending_bits_register_is_read_only() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));
        let cap_offset = config.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

        // Enable MSI.
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

        let is_64bit = (ctrl & (1 << 7)) != 0;
        let per_vector_masking = (ctrl & (1 << 8)) != 0;
        assert!(
            per_vector_masking,
            "test requires per-vector masking support"
        );
        let mask_off = if is_64bit {
            cap_offset + 0x10
        } else {
            cap_offset + 0x0c
        };
        let pending_off = if is_64bit {
            cap_offset + 0x14
        } else {
            cap_offset + 0x10
        };

        // Guest writes to Pending Bits must be ignored.
        config.write(pending_off, 4, 1);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            0
        );

        // Latch a pending bit via a masked trigger.
        config.write(mask_off, 4, 1);
        struct Sink;
        impl MsiTrigger for Sink {
            fn trigger_msi(&mut self, _message: MsiMessage) {}
        }
        {
            let msi = config.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
        }
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );

        // Guest writes must not be able to clear it either.
        config.write(pending_off, 4, 0);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );
    }

    #[test]
    fn msi_pending_bits_register_is_read_only_32bit() {
        // Same as `msi_pending_bits_register_is_read_only`, but covering the 32-bit MSI capability
        // layout.
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new_with_config(false, true)));
        let cap_offset = config.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

        // Enable MSI.
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        assert_eq!(
            ctrl & (1 << 7),
            0,
            "test requires 32-bit MSI capability layout"
        );
        config.write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

        let per_vector_masking = (ctrl & (1 << 8)) != 0;
        assert!(
            per_vector_masking,
            "test requires per-vector masking support"
        );
        let mask_off = cap_offset + 0x0c;
        let pending_off = cap_offset + 0x10;

        // Guest writes to Pending Bits must be ignored.
        config.write(pending_off, 4, 1);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            0
        );

        // Latch a pending bit via a masked trigger.
        config.write(mask_off, 4, 1);
        struct Sink;
        impl MsiTrigger for Sink {
            fn trigger_msi(&mut self, _message: MsiMessage) {}
        }
        {
            // Program a valid MSI address to ensure the pending bit is latched due to masking (not
            // because the address is invalid).
            config.write(cap_offset + 0x04, 4, 0xfee0_0000);
            config.write(cap_offset + 0x08, 2, 0x0045);
            let msi = config.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
        }
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );

        // Guest writes must not be able to clear it either.
        config.write(pending_off, 4, 0);
        assert_eq!(
            config.capability::<MsiCapability>().unwrap().pending_bits() & 1,
            1
        );
    }

    #[test]
    fn pci_device_reset_disables_msi_and_msix_but_preserves_bars() {
        struct Dev {
            cfg: PciConfigSpace,
        }

        impl PciDevice for Dev {
            fn config(&self) -> &PciConfigSpace {
                &self.cfg
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.cfg
            }
        }

        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        cfg.set_bar_base(0, 0x1234_5000);

        cfg.add_capability(Box::new(MsiCapability::new()));
        cfg.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));

        // Enable MSI.
        let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
        let msi_ctrl = cfg.read(msi_off + 0x02, 2) as u16;
        cfg.write(msi_off + 0x02, 2, u32::from(msi_ctrl | 0x0001));

        // Set the MSI per-vector mask and pending bits so we can assert reset clears them.
        let is_64bit = (msi_ctrl & (1 << 7)) != 0;
        let per_vector_masking = (msi_ctrl & (1 << 8)) != 0;
        if per_vector_masking {
            let mask_off = if is_64bit {
                msi_off + 0x10
            } else {
                msi_off + 0x0c
            };
            cfg.write(mask_off, 4, 1);

            struct Sink;
            impl MsiTrigger for Sink {
                fn trigger_msi(&mut self, _message: MsiMessage) {}
            }

            // Triggering while masked should latch the pending bit (guest cannot write the pending
            // bits register directly).
            let msi = cfg.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
        }

        // Enable MSI-X and set Function Mask.
        let msix_off = cfg.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
        let msix_ctrl = cfg.read(msix_off + 0x02, 2) as u16;
        cfg.write(
            msix_off + 0x02,
            2,
            u32::from(msix_ctrl | (1 << 15) | (1 << 14)),
        );

        assert!(cfg.capability::<MsiCapability>().unwrap().enabled());
        assert_eq!(cfg.capability::<MsiCapability>().unwrap().mask_bits(), 1);
        assert_eq!(cfg.capability::<MsiCapability>().unwrap().pending_bits(), 1);
        assert!(cfg.capability::<MsixCapability>().unwrap().enabled());
        assert!(cfg
            .capability::<MsixCapability>()
            .unwrap()
            .function_masked());
        {
            // Triggering a masked MSI-X vector should set the PBA pending bit; reset must clear it.
            let msix = cfg.capability_mut::<MsixCapability>().unwrap();
            assert!(msix.trigger(0).is_none());
            assert_eq!(msix.snapshot_pba()[0] & 1, 1);
        }

        let mut dev = Dev { cfg };
        dev.reset();

        assert!(!dev.cfg.capability::<MsiCapability>().unwrap().enabled());
        assert_eq!(
            dev.cfg.capability::<MsiCapability>().unwrap().mask_bits(),
            0
        );
        assert_eq!(
            dev.cfg
                .capability::<MsiCapability>()
                .unwrap()
                .pending_bits(),
            0
        );
        assert!(!dev.cfg.capability::<MsixCapability>().unwrap().enabled());
        assert!(!dev
            .cfg
            .capability::<MsixCapability>()
            .unwrap()
            .function_masked());
        assert_eq!(
            dev.cfg
                .capability::<MsixCapability>()
                .unwrap()
                .snapshot_pba()[0]
                & 1,
            0,
            "reset should clear MSI-X PBA pending bits"
        );

        // BAR base programming is preserved across reset.
        assert_eq!(dev.cfg.bar_range(0).unwrap().base, 0x1234_5000);
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

    #[test]
    fn restore_state_masks_bar_bases_to_bar_size_alignment() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });

        let mut state = cfg.snapshot_state();
        // Inject misaligned bases; restore_state should normalize them to hardware behavior.
        state.bar_base[0] = 0x1234_5678;
        state.bar_base[1] = 0x1234_5678;

        let mut restored = PciConfigSpace::new(0xabcd, 0xef01);
        restored.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        restored.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
        restored.restore_state(&state);

        assert_eq!(restored.bar_range(0).unwrap().base, 0x1234_5000);
        assert_eq!(restored.read(0x10, 4), 0x1234_5000);

        assert_eq!(restored.bar_range(1).unwrap().base, 0x1234_5660);
        assert_eq!(restored.read(0x14, 4), 0x1234_5661);
    }

    #[test]
    fn restore_state_rebuilds_capability_list_pointers() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        let msi_off = cfg.add_capability(Box::new(MsiCapability::new()));
        cfg.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));
        let mut state = cfg.snapshot_state();

        // Corrupt the capabilities list so it points at a bogus MSI-X capability near the end of
        // config space. If left unsanitized, `find_capability()` would return this offset and
        // subsequent reads relative to it could panic due to out-of-bounds accesses.
        state.bytes[PCI_CAP_PTR_OFFSET] = 0xfd;
        state.bytes[0xfd] = PCI_CAP_ID_MSIX;
        state.bytes[0xfe] = msi_off;
        state.bytes[msi_off as usize + 1] = 0;

        // Clear the Capabilities List status bit too; restore should re-assert it.
        let status = u16::from_le_bytes([
            state.bytes[PCI_STATUS_OFFSET],
            state.bytes[PCI_STATUS_OFFSET + 1],
        ]);
        let status = status & !PCI_STATUS_CAPABILITIES_LIST;
        state.bytes[PCI_STATUS_OFFSET..PCI_STATUS_OFFSET + 2]
            .copy_from_slice(&status.to_le_bytes());

        let mut restored = PciConfigSpace::new(0xabcd, 0xef01);
        let expected_msi = restored.add_capability(Box::new(MsiCapability::new()));
        let expected_msix =
            restored.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));
        restored.restore_state(&state);

        let status_after = restored.read(0x06, 2) as u16;
        assert_ne!(
            status_after & PCI_STATUS_CAPABILITIES_LIST,
            0,
            "restore should re-assert the Capabilities List status bit"
        );

        assert_eq!(restored.find_capability(PCI_CAP_ID_MSI), Some(expected_msi));
        assert_eq!(
            restored.find_capability(PCI_CAP_ID_MSIX),
            Some(expected_msix)
        );

        // Verify the returned offset is safe to use for subsequent reads.
        let msix_off = restored.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
        let _ = restored.read(msix_off + 0x02, 2);
    }

    #[test]
    fn restore_state_preserves_read_only_header_bytes() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_class_code(0x01, 0x02, 0x03, 0x04);
        cfg.set_header_type(0x80); // multifunction bit
        cfg.set_subsystem_ids(PciSubsystemIds {
            subsystem_vendor_id: 0xabcd,
            subsystem_id: 0xef01,
        });
        cfg.set_interrupt_pin(1);

        let mut state = cfg.snapshot_state();
        // Corrupt various read-only header bytes in the snapshot image. Restore must keep the
        // device-model-defined values from the target config space instead of trusting snapshot
        // bytes.
        state.bytes[0x00..0x04].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        state.bytes[0x08..0x0c].copy_from_slice(&0xfeed_faceu32.to_le_bytes());
        state.bytes[0x0e] = 0x00;
        state.bytes[0x2c..0x30].copy_from_slice(&0x1111_2222u32.to_le_bytes());
        state.bytes[0x3d] = 0x04;

        let mut restored = PciConfigSpace::new(0x1234, 0x5678);
        restored.set_class_code(0x01, 0x02, 0x03, 0x04);
        restored.set_header_type(0x80);
        restored.set_subsystem_ids(PciSubsystemIds {
            subsystem_vendor_id: 0xabcd,
            subsystem_id: 0xef01,
        });
        restored.set_interrupt_pin(1);
        restored.restore_state(&state);

        assert_eq!(restored.vendor_device_id().vendor_id, 0x1234);
        assert_eq!(restored.vendor_device_id().device_id, 0x5678);
        assert_eq!(restored.class_code(), cfg.class_code());
        assert_eq!(restored.header_type(), 0x80);
        assert_eq!(restored.read(0x2c, 2) as u16, 0xabcd);
        assert_eq!(restored.read(0x2e, 2) as u16, 0xef01);
        assert_eq!(restored.interrupt_pin(), 1);
    }

    #[test]
    fn redefining_a_64bit_bar_clears_the_stale_high_dword_register() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);

        // Define BAR0 as a 64-bit MMIO BAR and program a base above 4GiB so BAR1 (high dword) is
        // non-zero.
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x4000,
                prefetchable: false,
            },
        );
        cfg.write(0x10, 4, 0x2345_6000);
        cfg.write(0x14, 4, 0x0000_0001);
        assert_eq!(cfg.read(0x10, 4), 0x2345_4004);
        assert_eq!(cfg.read(0x14, 4), 0x0000_0001);

        // Redefine BAR0 as a 32-bit BAR. BAR1 is no longer part of BAR0 and should read as zero
        // (stale high bits must not leak through config reads).
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        assert_eq!(cfg.read(0x10, 4), 0);
        assert_eq!(cfg.read(0x14, 4), 0);
    }

    #[test]
    #[should_panic]
    fn bar_definitions_cannot_overlap_a_64bit_bar_high_dword_slot() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x4000,
                prefetchable: false,
            },
        );

        // BAR1 is consumed as the high dword of BAR0 when BAR0 is 64-bit.
        cfg.set_bar_definition(
            1,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
    }

    #[test]
    #[should_panic]
    fn bar_size_must_be_power_of_two() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(0, PciBarDefinition::Io { size: 0x30 });
    }

    #[test]
    #[should_panic]
    fn mmio_bar_size_must_be_at_least_16_bytes() {
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x8,
                prefetchable: false,
            },
        );
    }
}
