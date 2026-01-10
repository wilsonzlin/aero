use super::error::EmuException;

/// Physical memory interface used by the interpreter.
///
/// The interpreter performs all address translation itself (segmentation/paging/etc.) and only
/// calls the bus with a physical address.
pub trait MemoryBus {
    fn read_u8(&mut self, paddr: u64) -> Result<u8, EmuException>;
    fn write_u8(&mut self, paddr: u64, value: u8) -> Result<(), EmuException>;

    fn read_u16(&mut self, paddr: u64) -> Result<u16, EmuException> {
        let lo = self.read_u8(paddr)? as u16;
        let hi = self.read_u8(paddr + 1)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_u32(&mut self, paddr: u64) -> Result<u32, EmuException> {
        let b0 = self.read_u8(paddr)? as u32;
        let b1 = self.read_u8(paddr + 1)? as u32;
        let b2 = self.read_u8(paddr + 2)? as u32;
        let b3 = self.read_u8(paddr + 3)? as u32;
        Ok(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24))
    }

    fn read_u64(&mut self, paddr: u64) -> Result<u64, EmuException> {
        let lo = self.read_u32(paddr)? as u64;
        let hi = self.read_u32(paddr + 4)? as u64;
        Ok(lo | (hi << 32))
    }

    fn write_u16(&mut self, paddr: u64, value: u16) -> Result<(), EmuException> {
        self.write_u8(paddr, value as u8)?;
        self.write_u8(paddr + 1, (value >> 8) as u8)?;
        Ok(())
    }

    fn write_u32(&mut self, paddr: u64, value: u32) -> Result<(), EmuException> {
        self.write_u8(paddr, value as u8)?;
        self.write_u8(paddr + 1, (value >> 8) as u8)?;
        self.write_u8(paddr + 2, (value >> 16) as u8)?;
        self.write_u8(paddr + 3, (value >> 24) as u8)?;
        Ok(())
    }

    fn write_u64(&mut self, paddr: u64, value: u64) -> Result<(), EmuException> {
        self.write_u32(paddr, value as u32)?;
        self.write_u32(paddr + 4, (value >> 32) as u32)?;
        Ok(())
    }

    fn read_bytes(&mut self, paddr: u64, buf: &mut [u8]) -> Result<(), EmuException> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_u8(paddr + i as u64)?;
        }
        Ok(())
    }

    fn write_bytes(&mut self, paddr: u64, buf: &[u8]) -> Result<(), EmuException> {
        for (i, b) in buf.iter().enumerate() {
            self.write_u8(paddr + i as u64, *b)?;
        }
        Ok(())
    }
}

pub trait PortIo {
    fn in_u8(&mut self, port: u16) -> u8;
    fn in_u16(&mut self, port: u16) -> u16 {
        let lo = self.in_u8(port) as u16;
        let hi = self.in_u8(port + 1) as u16;
        lo | (hi << 8)
    }
    fn in_u32(&mut self, port: u16) -> u32 {
        let lo = self.in_u16(port) as u32;
        let hi = self.in_u16(port + 2) as u32;
        lo | (hi << 16)
    }

    fn out_u8(&mut self, port: u16, value: u8);
    fn out_u16(&mut self, port: u16, value: u16) {
        self.out_u8(port, value as u8);
        self.out_u8(port + 1, (value >> 8) as u8);
    }
    fn out_u32(&mut self, port: u16, value: u32) {
        self.out_u16(port, value as u16);
        self.out_u16(port + 2, (value >> 16) as u16);
    }
}
