use super::capabilities::{
    PciCapability, PciCapabilityInfo, PCI_CAP_PTR_OFFSET, PCI_CONFIG_SPACE_SIZE,
    PCI_STATUS_CAPABILITIES_LIST, PCI_STATUS_OFFSET,
};

pub struct PciConfigSpace {
    bytes: [u8; PCI_CONFIG_SPACE_SIZE],
    capabilities: Vec<Box<dyn PciCapability>>,
    next_cap_offset: u8,
    last_cap_offset: Option<u8>,
}

impl PciConfigSpace {
    pub const INTERRUPT_LINE_OFFSET: u16 = 0x3C;
    pub const INTERRUPT_PIN_OFFSET: u16 = 0x3D;

    pub fn new(vendor_id: u16, device_id: u16) -> Self {
        let mut bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
        bytes[0x00..0x02].copy_from_slice(&vendor_id.to_le_bytes());
        bytes[0x02..0x04].copy_from_slice(&device_id.to_le_bytes());
        bytes[0x0e] = 0x00;

        Self {
            bytes,
            capabilities: Vec::new(),
            next_cap_offset: 0x40,
            last_cap_offset: None,
        }
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
        let mut value = 0u32;
        for i in 0..size {
            value |= (self.bytes[offset + i] as u32) << (8 * i);
        }
        value
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
        assert!(matches!(size, 1 | 2 | 4));
        let offset = offset as usize;
        assert!(offset + size <= PCI_CONFIG_SPACE_SIZE);

        for i in 0..size {
            let addr = offset + i;
            if self.is_read_only_byte(addr) {
                continue;
            }
            self.bytes[addr] = ((value >> (8 * i)) & 0xff) as u8;
        }

        self.sync_capabilities_from_config();
        self.sync_capabilities_to_config();
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
}

#[cfg(test)]
mod tests {
    use super::PciConfigSpace;
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
}
