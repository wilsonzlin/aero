/// Abstraction over the guest's physical memory.
///
/// UHCI descriptors (Frame List, QHs, TDs) live in guest RAM; the host controller must be able to
/// read and write them.
pub trait GuestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]);
    fn write(&mut self, addr: u32, buf: &[u8]);

    fn read_u8(&self, addr: u32) -> u8 {
        let mut b = [0u8; 1];
        self.read(addr, &mut b);
        b[0]
    }

    fn read_u16(&self, addr: u32) -> u16 {
        let mut b = [0u8; 2];
        self.read(addr, &mut b);
        u16::from_le_bytes(b)
    }

    fn read_u32(&self, addr: u32) -> u32 {
        let mut b = [0u8; 4];
        self.read(addr, &mut b);
        u32::from_le_bytes(b)
    }

    fn write_u8(&mut self, addr: u32, value: u8) {
        self.write(addr, &[value]);
    }

    fn write_u16(&mut self, addr: u32, value: u16) {
        self.write(addr, &value.to_le_bytes());
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        self.write(addr, &value.to_le_bytes());
    }
}
