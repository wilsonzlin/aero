pub const FLAG_CF: u64 = 1 << 0;

#[derive(Debug, Default, Clone)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rdi: u64,
    pub rflags: u64,
    pub es: u16,
}

impl CpuState {
    pub fn ax(&self) -> u16 {
        (self.rax & 0xFFFF) as u16
    }

    pub fn set_ax(&mut self, value: u16) {
        self.rax = (self.rax & !0xFFFF) | (value as u64);
    }

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

    pub fn bx(&self) -> u16 {
        (self.rbx & 0xFFFF) as u16
    }

    pub fn set_bx(&mut self, value: u16) {
        self.rbx = (self.rbx & !0xFFFF) | (value as u64);
    }

    pub fn bh(&self) -> u8 {
        (self.bx() >> 8) as u8
    }

    pub fn set_bh(&mut self, value: u8) {
        let bx = self.bx();
        self.set_bx(((value as u16) << 8) | (bx & 0x00FF));
    }

    pub fn bl(&self) -> u8 {
        (self.bx() & 0xFF) as u8
    }

    pub fn set_bl(&mut self, value: u8) {
        let bx = self.bx();
        self.set_bx((bx & 0xFF00) | (value as u16));
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

    pub fn di(&self) -> u16 {
        (self.rdi & 0xFFFF) as u16
    }

    pub fn set_di(&mut self, value: u16) {
        self.rdi = (self.rdi & !0xFFFF) | (value as u64);
    }

    pub fn es(&self) -> u16 {
        self.es
    }

    pub fn set_es(&mut self, value: u16) {
        self.es = value;
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
