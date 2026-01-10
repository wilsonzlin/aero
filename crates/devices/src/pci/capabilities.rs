use std::any::Any;

pub const PCI_CONFIG_SPACE_SIZE: usize = 256;
pub const PCI_STATUS_OFFSET: usize = 0x06;
pub const PCI_CAP_PTR_OFFSET: usize = 0x34;
pub const PCI_STATUS_CAPABILITIES_LIST: u16 = 1 << 4;
pub const PCI_CAP_ID_VENDOR_SPECIFIC: u8 = 0x09;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciCapabilityInfo {
    pub id: u8,
    pub offset: u8,
}

pub trait PciCapability: Any {
    fn id(&self) -> u8;
    fn offset(&self) -> u8;
    fn set_offset(&mut self, offset: u8);
    fn len(&self) -> u8;

    fn sync_to_config(&self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]);
    fn sync_from_config(&mut self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]);

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Debug, Clone)]
pub struct VendorSpecificCapability {
    offset: u8,
    payload: Vec<u8>,
}

impl VendorSpecificCapability {
    pub fn new(payload: Vec<u8>) -> Self {
        Self { offset: 0, payload }
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

impl PciCapability for VendorSpecificCapability {
    fn id(&self) -> u8 {
        PCI_CAP_ID_VENDOR_SPECIFIC
    }

    fn offset(&self) -> u8 {
        self.offset
    }

    fn set_offset(&mut self, offset: u8) {
        self.offset = offset;
    }

    fn len(&self) -> u8 {
        u8::try_from(2 + self.payload.len()).expect("vendor capability too large for config space")
    }

    fn sync_to_config(&self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        let len = self.len() as usize;
        assert!(base + len <= PCI_CONFIG_SPACE_SIZE);
        config[base] = PCI_CAP_ID_VENDOR_SPECIFIC;
        config[base + 2..base + len].copy_from_slice(&self.payload);
    }

    fn sync_from_config(&mut self, config: &mut [u8; PCI_CONFIG_SPACE_SIZE]) {
        let base = self.offset as usize;
        let len = self.len() as usize;
        assert!(base + len <= PCI_CONFIG_SPACE_SIZE);
        self.payload.clear();
        self.payload
            .extend_from_slice(&config[base + 2..base + len]);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
