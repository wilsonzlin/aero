use std::any::Any;

use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};

use super::capabilities::{PciCapability, PCI_CONFIG_SPACE_SIZE};

pub const PCI_CAP_ID_MSI: u8 = 0x05;

#[derive(Debug, Clone)]
pub struct MsiCapability {
    offset: u8,
    enabled: bool,
    is_64bit: bool,
    per_vector_masking: bool,
    message_address: u64,
    message_data: u16,
    mask_bits: u32,
    pending_bits: u32,
}

impl MsiCapability {
    pub fn new() -> Self {
        Self {
            offset: 0,
            enabled: false,
            is_64bit: true,
            per_vector_masking: true,
            message_address: 0,
            message_data: 0,
            mask_bits: 0,
            pending_bits: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn message_address(&self) -> u64 {
        self.message_address
    }

    pub fn message_data(&self) -> u16 {
        self.message_data
    }

    pub fn message(&self) -> MsiMessage {
        MsiMessage {
            address: self.message_address,
            data: self.message_data,
        }
    }

    /// Triggers a single-vector MSI delivery if MSI is enabled.
    ///
    /// This implementation models the optional per-vector mask/pending registers in a
    /// simplified way:
    /// - If the single supported vector is masked, delivery is suppressed and the pending bit
    ///   is set.
    /// - The pending bit is cleared when the device successfully delivers a message.
    /// - The pending bit is *not* automatically re-delivered on unmask; callers should
    ///   re-trigger if they rely on that behavior.
    pub fn trigger(&mut self, platform: &mut impl MsiTrigger) -> bool {
        if !self.enabled {
            return false;
        }

        if self.per_vector_masking && (self.mask_bits & 1) != 0 {
            self.pending_bits |= 1;
            return false;
        }

        self.pending_bits &= !1;
        platform.trigger_msi(self.message());
        true
    }

    fn len_internal(&self) -> u8 {
        match (self.is_64bit, self.per_vector_masking) {
            (true, true) => 0x18,
            (true, false) => 0x10,
            (false, true) => 0x14,
            (false, false) => 0x0c,
        }
    }

    fn message_control(&self) -> u16 {
        let mut ctrl = 0u16;
        if self.enabled {
            ctrl |= 1;
        }
        if self.is_64bit {
            ctrl |= 1 << 7;
        }
        if self.per_vector_masking {
            ctrl |= 1 << 8;
        }
        ctrl
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

    fn read_u32(config: &[u8; PCI_CONFIG_SPACE_SIZE], offset: usize) -> u32 {
        u32::from_le_bytes([
            config[offset],
            config[offset + 1],
            config[offset + 2],
            config[offset + 3],
        ])
    }
}

impl PciCapability for MsiCapability {
    fn id(&self) -> u8 {
        PCI_CAP_ID_MSI
    }

    fn offset(&self) -> u8 {
        self.offset
    }

    fn set_offset(&mut self, offset: u8) {
        self.offset = offset;
    }

    fn len(&self) -> u8 {
        self.len_internal()
    }

    fn sync_to_config(&self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        assert!(base + self.len_internal() as usize <= PCI_CONFIG_SPACE_SIZE);

        config[base] = PCI_CAP_ID_MSI;
        Self::write_u16(config, base + 0x02, self.message_control());

        let addr = self.message_address;
        Self::write_u32(config, base + 0x04, addr as u32);
        if self.is_64bit {
            Self::write_u32(config, base + 0x08, (addr >> 32) as u32);
            Self::write_u16(config, base + 0x0c, self.message_data);
            config[base + 0x0e] = 0;
            config[base + 0x0f] = 0;

            if self.per_vector_masking {
                Self::write_u32(config, base + 0x10, self.mask_bits);
                Self::write_u32(config, base + 0x14, self.pending_bits);
            }
        } else {
            Self::write_u16(config, base + 0x08, self.message_data);
            config[base + 0x0a] = 0;
            config[base + 0x0b] = 0;
            if self.per_vector_masking {
                Self::write_u32(config, base + 0x0c, self.mask_bits);
                Self::write_u32(config, base + 0x10, self.pending_bits);
            }
        }
    }

    fn sync_from_config(&mut self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        assert!(base + self.len_internal() as usize <= PCI_CONFIG_SPACE_SIZE);

        let ctrl = Self::read_u16(config, base + 0x02);
        self.enabled = (ctrl & 1) != 0;

        let addr_low = Self::read_u32(config, base + 0x04) as u64;
        let addr = if self.is_64bit {
            let addr_high = Self::read_u32(config, base + 0x08) as u64;
            addr_low | (addr_high << 32)
        } else {
            addr_low
        };
        self.message_address = addr;

        self.message_data = if self.is_64bit {
            Self::read_u16(config, base + 0x0c)
        } else {
            Self::read_u16(config, base + 0x08)
        };

        if self.per_vector_masking {
            self.mask_bits = if self.is_64bit {
                Self::read_u32(config, base + 0x10)
            } else {
                Self::read_u32(config, base + 0x0c)
            };
        }
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
    use super::MsiCapability;
    use crate::pci::config::PciConfigSpace;
    use aero_platform::interrupts::{
        InterruptController, PlatformInterruptMode, PlatformInterrupts,
    };

    #[test]
    fn trigger_msi_delivers_vector_to_lapic() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));

        let cap_offset = config.find_capability(super::PCI_CAP_ID_MSI).unwrap() as u16;
        config.write(cap_offset + 0x04, 4, 0xfee0_0000);
        config.write(cap_offset + 0x08, 4, 0);
        config.write(cap_offset + 0x0c, 2, 0x0045);
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

        let mut interrupts = PlatformInterrupts::new();
        interrupts.set_mode(PlatformInterruptMode::Apic);
        let msi = config.capability_mut::<MsiCapability>().unwrap();
        assert!(msi.trigger(&mut interrupts));
        assert_eq!(interrupts.get_pending(), Some(0x45));
    }

    #[test]
    fn trigger_msi_broadcast_destination_delivers_to_single_cpu() {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));

        let cap_offset = config.find_capability(super::PCI_CAP_ID_MSI).unwrap() as u16;
        // MSI address with destination ID 0xFF (broadcast in xAPIC physical mode).
        config.write(cap_offset + 0x04, 4, 0xfeef_f000);
        config.write(cap_offset + 0x08, 4, 0);
        config.write(cap_offset + 0x0c, 2, 0x0045);
        let ctrl = config.read(cap_offset + 0x02, 2) as u16;
        config.write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

        let mut interrupts = PlatformInterrupts::new();
        interrupts.set_mode(PlatformInterruptMode::Apic);
        let msi = config.capability_mut::<MsiCapability>().unwrap();
        assert!(msi.trigger(&mut interrupts));
        assert_eq!(interrupts.get_pending(), Some(0x45));
    }
}
