#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RealModeCpu {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub esp: u32,

    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub ip: u16,

    /// Real-mode FLAGS register bits. Only a small subset are modeled.
    pub flags: u32,
}

impl RealModeCpu {
    pub const FLAG_CF: u32 = 1 << 0;

    pub fn seg_off(seg: u16, off: u16) -> u32 {
        (seg as u32) * 16 + (off as u32)
    }

    pub fn phys_ip(&self) -> u32 {
        Self::seg_off(self.cs, self.ip)
    }

    pub fn carry(&self) -> bool {
        (self.flags & Self::FLAG_CF) != 0
    }

    pub fn set_carry(&mut self, carry: bool) {
        if carry {
            self.flags |= Self::FLAG_CF;
        } else {
            self.flags &= !Self::FLAG_CF;
        }
    }

    pub fn ax(&self) -> u16 {
        self.eax as u16
    }

    pub fn set_ax(&mut self, val: u16) {
        self.eax = (self.eax & 0xFFFF_0000) | (val as u32);
    }

    pub fn ah(&self) -> u8 {
        (self.eax >> 8) as u8
    }

    pub fn set_ah(&mut self, val: u8) {
        self.eax = (self.eax & 0xFFFF_00FF) | ((val as u32) << 8);
    }

    pub fn al(&self) -> u8 {
        self.eax as u8
    }

    pub fn set_al(&mut self, val: u8) {
        self.eax = (self.eax & 0xFFFF_FF00) | (val as u32);
    }

    pub fn bx(&self) -> u16 {
        self.ebx as u16
    }

    pub fn set_bx(&mut self, val: u16) {
        self.ebx = (self.ebx & 0xFFFF_0000) | (val as u32);
    }

    pub fn cx(&self) -> u16 {
        self.ecx as u16
    }

    pub fn set_cx(&mut self, val: u16) {
        self.ecx = (self.ecx & 0xFFFF_0000) | (val as u32);
    }

    pub fn dx(&self) -> u16 {
        self.edx as u16
    }

    pub fn set_dx(&mut self, val: u16) {
        self.edx = (self.edx & 0xFFFF_0000) | (val as u32);
    }
}

