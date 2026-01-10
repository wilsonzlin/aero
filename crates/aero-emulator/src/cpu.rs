pub const FLAG_CF: u64 = 1 << 0;

#[derive(Debug, Clone, Copy, Default)]
pub struct Segment {
    pub selector: u16,
}

impl Segment {
    pub fn base(self) -> u64 {
        (self.selector as u64) << 4
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub rip: u64,
    pub rflags: u64,
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub ss: Segment,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            rip: 0,
            rflags: 0,
            cs: Segment::default(),
            ds: Segment::default(),
            es: Segment::default(),
            ss: Segment::default(),
        }
    }
}

impl CpuState {
    pub fn cf(&self) -> bool {
        self.rflags & FLAG_CF != 0
    }

    pub fn set_cf(&mut self, value: bool) {
        if value {
            self.rflags |= FLAG_CF;
        } else {
            self.rflags &= !FLAG_CF;
        }
    }

    pub fn ax(&self) -> u16 {
        self.rax as u16
    }

    pub fn set_ax(&mut self, value: u16) {
        self.rax = (self.rax & !0xFFFF) | value as u64;
    }

    pub fn ah(&self) -> u8 {
        (self.rax >> 8) as u8
    }

    pub fn set_ah(&mut self, value: u8) {
        self.rax = (self.rax & !0xFF00) | ((value as u64) << 8);
    }

    pub fn al(&self) -> u8 {
        self.rax as u8
    }

    pub fn set_al(&mut self, value: u8) {
        self.rax = (self.rax & !0xFF) | value as u64;
    }

    pub fn bx(&self) -> u16 {
        self.rbx as u16
    }

    pub fn set_bx(&mut self, value: u16) {
        self.rbx = (self.rbx & !0xFFFF) | value as u64;
    }

    pub fn bh(&self) -> u8 {
        (self.rbx >> 8) as u8
    }

    pub fn set_bh(&mut self, value: u8) {
        self.rbx = (self.rbx & !0xFF00) | ((value as u64) << 8);
    }

    pub fn bl(&self) -> u8 {
        self.rbx as u8
    }

    pub fn set_bl(&mut self, value: u8) {
        self.rbx = (self.rbx & !0xFF) | value as u64;
    }

    pub fn cx(&self) -> u16 {
        self.rcx as u16
    }

    pub fn set_cx(&mut self, value: u16) {
        self.rcx = (self.rcx & !0xFFFF) | value as u64;
    }

    pub fn ch(&self) -> u8 {
        (self.rcx >> 8) as u8
    }

    pub fn set_ch(&mut self, value: u8) {
        self.rcx = (self.rcx & !0xFF00) | ((value as u64) << 8);
    }

    pub fn cl(&self) -> u8 {
        self.rcx as u8
    }

    pub fn set_cl(&mut self, value: u8) {
        self.rcx = (self.rcx & !0xFF) | value as u64;
    }

    pub fn dx(&self) -> u16 {
        self.rdx as u16
    }

    pub fn set_dx(&mut self, value: u16) {
        self.rdx = (self.rdx & !0xFFFF) | value as u64;
    }

    pub fn dh(&self) -> u8 {
        (self.rdx >> 8) as u8
    }

    pub fn set_dh(&mut self, value: u8) {
        self.rdx = (self.rdx & !0xFF00) | ((value as u64) << 8);
    }

    pub fn dl(&self) -> u8 {
        self.rdx as u8
    }

    pub fn set_dl(&mut self, value: u8) {
        self.rdx = (self.rdx & !0xFF) | value as u64;
    }

    pub fn es_di(&self) -> u64 {
        self.es.base() + (self.rdi & 0xFFFF)
    }

    pub fn es_bp(&self) -> u64 {
        self.es.base() + (self.rbp & 0xFFFF)
    }
}
