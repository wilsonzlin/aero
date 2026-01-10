use std::any::Any;

pub const PCI_CONFIG_SPACE_SIZE: usize = 256;
pub const PCI_STATUS_OFFSET: usize = 0x06;
pub const PCI_CAP_PTR_OFFSET: usize = 0x34;
pub const PCI_STATUS_CAPABILITIES_LIST: u16 = 1 << 4;

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
