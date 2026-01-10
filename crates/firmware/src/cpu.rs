pub const FLAG_CF: u64 = 1 << 0;

#[derive(Debug, Default, Clone)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rflags: u64,
}

impl CpuState {
    pub fn ah(&self) -> u8 {
        ((self.rax >> 8) & 0xFF) as u8
    }

    pub fn set_ah(&mut self, value: u8) {
        self.rax = (self.rax & !0xFF00) | ((value as u64) << 8);
    }

    pub fn al(&self) -> u8 {
        (self.rax & 0xFF) as u8
    }

    pub fn set_al(&mut self, value: u8) {
        self.rax = (self.rax & !0xFF) | (value as u64);
    }

    pub fn cx(&self) -> u16 {
        (self.rcx & 0xFFFF) as u16
    }

    pub fn set_cx(&mut self, value: u16) {
        self.rcx = (self.rcx & !0xFFFF) | (value as u64);
    }

    pub fn set_ch(&mut self, value: u8) {
        let cx = self.cx();
        self.set_cx(((value as u16) << 8) | (cx & 0x00FF));
    }

    pub fn set_cl(&mut self, value: u8) {
        let cx = self.cx();
        self.set_cx((cx & 0xFF00) | (value as u16));
    }

    pub fn dx(&self) -> u16 {
        (self.rdx & 0xFFFF) as u16
    }

    pub fn set_dx(&mut self, value: u16) {
        self.rdx = (self.rdx & !0xFFFF) | (value as u64);
    }

    pub fn set_dh(&mut self, value: u8) {
        let dx = self.dx();
        self.set_dx(((value as u16) << 8) | (dx & 0x00FF));
    }

    pub fn set_dl(&mut self, value: u8) {
        let dx = self.dx();
        self.set_dx((dx & 0xFF00) | (value as u16));
    }

    pub fn cf(&self) -> bool {
        (self.rflags & FLAG_CF) != 0
    }

    pub fn set_cf(&mut self) {
        self.rflags |= FLAG_CF;
    }

    pub fn clear_cf(&mut self) {
        self.rflags &= !FLAG_CF;
    }
}
