use std::any::Any;

use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};

use super::capabilities::{PciCapability, PCI_CONFIG_SPACE_SIZE};

/// PCI capability ID for MSI-X.
pub const PCI_CAP_ID_MSIX: u8 = 0x11;

const MSIX_CAP_LEN: u8 = 0x0c;
const MSIX_TABLE_ENTRY_SIZE: usize = 16;

#[derive(Debug, Clone)]
pub struct MsixCapability {
    offset: u8,

    table_size: u16,
    enabled: bool,
    function_mask: bool,

    table_bir: u8,
    table_offset: u32,
    pba_bir: u8,
    pba_offset: u32,

    /// Raw MSI-X table bytes, little-endian, length = `table_size * 16`.
    table: Vec<u8>,
    /// Pending bit array words (bit per vector).
    pba: Vec<u64>,
}

impl MsixCapability {
    pub fn new(table_size: u16, table_bir: u8, table_offset: u32, pba_bir: u8, pba_offset: u32) -> Self {
        assert!(table_size > 0, "MSI-X table size must be non-zero");
        assert!(
            (table_offset & 0x7) == 0,
            "MSI-X table offset must be 8-byte aligned"
        );
        assert!(
            (pba_offset & 0x7) == 0,
            "MSI-X PBA offset must be 8-byte aligned"
        );

        let table_bytes = usize::from(table_size) * MSIX_TABLE_ENTRY_SIZE;
        let pba_words = (usize::from(table_size) + 63) / 64;
        Self {
            offset: 0,
            table_size,
            enabled: false,
            function_mask: false,
            table_bir: table_bir & 0x7,
            table_offset,
            pba_bir: pba_bir & 0x7,
            pba_offset,
            table: vec![0u8; table_bytes],
            pba: vec![0u64; pba_words],
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn function_masked(&self) -> bool {
        self.function_mask
    }

    pub fn table_size(&self) -> u16 {
        self.table_size
    }

    pub fn table_bir(&self) -> u8 {
        self.table_bir
    }

    pub fn table_offset(&self) -> u32 {
        self.table_offset
    }

    pub fn table_len_bytes(&self) -> usize {
        self.table.len()
    }

    pub fn pba_bir(&self) -> u8 {
        self.pba_bir
    }

    pub fn pba_offset(&self) -> u32 {
        self.pba_offset
    }

    pub fn pba_len_bytes(&self) -> usize {
        self.pba.len() * 8
    }

    pub fn table_read(&self, offset: u64, data: &mut [u8]) {
        let start = offset as usize;
        for (i, out) in data.iter_mut().enumerate() {
            *out = self.table.get(start + i).copied().unwrap_or(0);
        }
    }

    pub fn table_write(&mut self, offset: u64, data: &[u8]) {
        let start = offset as usize;
        for (i, b) in data.iter().enumerate() {
            if let Some(slot) = self.table.get_mut(start + i) {
                *slot = *b;
            }
        }
    }

    pub fn pba_read(&self, offset: u64, data: &mut [u8]) {
        let start = offset as usize;
        let len_bytes = self.pba_len_bytes();
        for (i, out) in data.iter_mut().enumerate() {
            let idx = start + i;
            if idx >= len_bytes {
                *out = 0;
                continue;
            }
            let word = idx / 8;
            let byte = idx % 8;
            *out = ((self.pba[word] >> (byte * 8)) & 0xff) as u8;
        }
    }

    /// MSI-X Pending Bit Array is read-only from the guest's perspective. Writes are ignored.
    pub fn pba_write(&mut self, _offset: u64, _data: &[u8]) {}

    fn message_control(&self) -> u16 {
        let mut ctrl = (self.table_size - 1) & 0x07ff;
        if self.function_mask {
            ctrl |= 1 << 14;
        }
        if self.enabled {
            ctrl |= 1 << 15;
        }
        ctrl
    }

    fn table_offset_bir(&self) -> u32 {
        (self.table_offset & !0x7) | u32::from(self.table_bir & 0x7)
    }

    fn pba_offset_bir(&self) -> u32 {
        (self.pba_offset & !0x7) | u32::from(self.pba_bir & 0x7)
    }

    fn pending_word_and_mask(vector: u16) -> Option<(usize, u64)> {
        let vector_usize = usize::from(vector);
        let word = vector_usize / 64;
        let bit = vector_usize % 64;
        Some((word, 1u64 << bit))
    }

    fn set_pending(&mut self, vector: u16, pending: bool) {
        let Some((word, mask)) = Self::pending_word_and_mask(vector) else {
            return;
        };
        if word >= self.pba.len() {
            return;
        }
        if pending {
            self.pba[word] |= mask;
        } else {
            self.pba[word] &= !mask;
        }
    }

    fn entry_base(&self, vector: u16) -> Option<usize> {
        let vector_usize = usize::from(vector);
        if vector_usize >= usize::from(self.table_size) {
            return None;
        }
        let base = vector_usize.checked_mul(MSIX_TABLE_ENTRY_SIZE)?;
        if base + MSIX_TABLE_ENTRY_SIZE > self.table.len() {
            return None;
        }
        Some(base)
    }

    fn entry_masked(&self, vector: u16) -> Option<bool> {
        let base = self.entry_base(vector)?;
        let ctrl = u32::from_le_bytes(self.table[base + 12..base + 16].try_into().unwrap());
        Some((ctrl & 1) != 0)
    }

    fn entry_message(&self, vector: u16) -> Option<MsiMessage> {
        let base = self.entry_base(vector)?;
        let addr_low = u32::from_le_bytes(self.table[base..base + 4].try_into().unwrap()) as u64;
        let addr_high =
            u32::from_le_bytes(self.table[base + 4..base + 8].try_into().unwrap()) as u64;
        let addr = addr_low | (addr_high << 32);
        let data =
            u32::from_le_bytes(self.table[base + 8..base + 12].try_into().unwrap()) as u16;
        Some(MsiMessage { address: addr, data })
    }

    /// Returns the MSI message that should be delivered for the given table entry index.
    ///
    /// - When MSI-X is disabled, or the vector is masked, this returns `None` and sets the pending
    ///   bit for the vector.
    /// - When delivery is successful, the pending bit is cleared.
    pub fn trigger(&mut self, vector: u16) -> Option<MsiMessage> {
        if !self.enabled {
            return None;
        }
        if vector >= self.table_size {
            return None;
        }
        if self.function_mask {
            self.set_pending(vector, true);
            return None;
        }
        if self.entry_masked(vector).unwrap_or(true) {
            self.set_pending(vector, true);
            return None;
        }
        let msg = self.entry_message(vector)?;
        if msg.address == 0 {
            self.set_pending(vector, true);
            return None;
        }

        self.set_pending(vector, false);
        Some(msg)
    }

    /// Triggers an MSI-X delivery into the provided platform MSI sink.
    pub fn trigger_into(&mut self, vector: u16, platform: &mut impl MsiTrigger) -> bool {
        let Some(msg) = self.trigger(vector) else {
            return false;
        };
        platform.trigger_msi(msg);
        true
    }

    fn write_u16(config: &mut [u8; PCI_CONFIG_SPACE_SIZE], offset: usize, value: u16) {
        let bytes = value.to_le_bytes();
        config[offset] = bytes[0];
        config[offset + 1] = bytes[1];
    }

    fn write_u32(config: &mut [u8; PCI_CONFIG_SPACE_SIZE], offset: usize, value: u32) {
        let bytes = value.to_le_bytes();
        config[offset..offset + 4].copy_from_slice(&bytes);
    }

    fn read_u16(config: &[u8; PCI_CONFIG_SPACE_SIZE], offset: usize) -> u16 {
        u16::from_le_bytes([config[offset], config[offset + 1]])
    }
}

impl PciCapability for MsixCapability {
    fn id(&self) -> u8 {
        PCI_CAP_ID_MSIX
    }

    fn offset(&self) -> u8 {
        self.offset
    }

    fn set_offset(&mut self, offset: u8) {
        self.offset = offset;
    }

    fn len(&self) -> u8 {
        MSIX_CAP_LEN
    }

    fn sync_to_config(&self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        assert!(base + MSIX_CAP_LEN as usize <= PCI_CONFIG_SPACE_SIZE);

        config[base] = PCI_CAP_ID_MSIX;
        Self::write_u16(config, base + 0x02, self.message_control());
        Self::write_u32(config, base + 0x04, self.table_offset_bir());
        Self::write_u32(config, base + 0x08, self.pba_offset_bir());
    }

    fn sync_from_config(&mut self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        assert!(base + MSIX_CAP_LEN as usize <= PCI_CONFIG_SPACE_SIZE);

        let ctrl = Self::read_u16(config, base + 0x02);
        self.enabled = (ctrl & (1 << 15)) != 0;
        self.function_mask = (ctrl & (1 << 14)) != 0;
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::MsixCapability;
    use crate::pci::config::PciConfigSpace;
    use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

    #[test]
    fn capability_list_traversal_finds_msix() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));

        let caps = config.capability_list();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].id, super::PCI_CAP_ID_MSIX);
        assert_eq!(caps[0].offset, 0x40);
    }

    #[test]
    fn programming_msix_updates_device_state() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));
        let cap_offset = config.find_capability(super::PCI_CAP_ID_MSIX).unwrap() as u16;

        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        // Table size is N-1 in bits 0..=10.
        assert_eq!(ctrl & 0x07ff, 1);
        config.write(cap_offset + 0x02, 2, (ctrl | (1 << 15)) as u32);

        let msix = config.capability::<MsixCapability>().unwrap();
        assert!(msix.enabled());
    }

    #[test]
    fn trigger_msix_delivers_vector_to_lapic() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));
        let cap_offset = config.find_capability(super::PCI_CAP_ID_MSIX).unwrap() as u16;

        // Enable MSI-X.
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, (ctrl | (1 << 15)) as u32);

        // Program table entry 1.
        {
            let msix = config.capability_mut::<MsixCapability>().unwrap();
            let base = 1u64 * 16;
            msix.table_write(base + 0x0, &0xfee0_0000u32.to_le_bytes());
            msix.table_write(base + 0x4, &0u32.to_le_bytes());
            msix.table_write(base + 0x8, &0x0045u32.to_le_bytes());
            msix.table_write(base + 0xc, &0u32.to_le_bytes()); // unmasked
        }

        let mut interrupts = PlatformInterrupts::new();
        interrupts.set_mode(PlatformInterruptMode::Apic);

        let msix = config.capability_mut::<MsixCapability>().unwrap();
        assert!(msix.trigger_into(1, &mut interrupts));
        assert_eq!(interrupts.get_pending(), Some(0x45));
    }

    #[test]
    fn masked_vector_sets_pending_bit_in_pba() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsixCapability::new(2, 0, 0x1000, 0, 0x2000)));
        let cap_offset = config.find_capability(super::PCI_CAP_ID_MSIX).unwrap() as u16;

        // Enable MSI-X.
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, (ctrl | (1 << 15)) as u32);

        {
            let msix = config.capability_mut::<MsixCapability>().unwrap();
            // Program table entry 1 with mask bit set in vector control.
            let base = 1u64 * 16;
            msix.table_write(base + 0x0, &0xfee0_0000u32.to_le_bytes());
            msix.table_write(base + 0x4, &0u32.to_le_bytes());
            msix.table_write(base + 0x8, &0x0045u32.to_le_bytes());
            msix.table_write(base + 0xc, &1u32.to_le_bytes()); // masked
        }

        let mut interrupts = PlatformInterrupts::new();
        interrupts.set_mode(PlatformInterruptMode::Apic);

        let msix = config.capability_mut::<MsixCapability>().unwrap();
        assert!(!msix.trigger_into(1, &mut interrupts));
        assert_eq!(interrupts.get_pending(), None);

        let mut pba = [0u8; 8];
        msix.pba_read(0, &mut pba);
        let bits = u64::from_le_bytes(pba);
        assert_eq!(bits & (1 << 1), 1 << 1);
    }
}

