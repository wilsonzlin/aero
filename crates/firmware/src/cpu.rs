pub const FLAG_CF: u64 = 1 << 0;

/// Minimal register file used by firmware-side BIOS interrupt handlers.
///
/// The full emulator has a richer CPU model; the firmware crate keeps just the
/// subset needed by BIOS services and unit tests.
#[derive(Debug, Default, Clone)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rflags: u64,

    pub ds: u16,
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
        ((self.rbx >> 8) & 0xFF) as u8
    }

    pub fn set_bh(&mut self, value: u8) {
        self.rbx = (self.rbx & !0xFF00) | ((value as u64) << 8);
    }

    pub fn bl(&self) -> u8 {
        (self.rbx & 0xFF) as u8
    }

    pub fn set_bl(&mut self, value: u8) {
        self.rbx = (self.rbx & !0xFF) | (value as u64);
    }

    pub fn cx(&self) -> u16 {
        (self.rcx & 0xFFFF) as u16
    }

    pub fn set_cx(&mut self, value: u16) {
        self.rcx = (self.rcx & !0xFFFF) | (value as u64);
    }

    pub fn ch(&self) -> u8 {
        (self.cx() >> 8) as u8
    }

    pub fn cl(&self) -> u8 {
        (self.cx() & 0xFF) as u8
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

    pub fn dh(&self) -> u8 {
        (self.dx() >> 8) as u8
    }

    pub fn dl(&self) -> u8 {
        (self.dx() & 0xFF) as u8
    }

    pub fn set_dh(&mut self, value: u8) {
        let dx = self.dx();
        self.set_dx(((value as u16) << 8) | (dx & 0x00FF));
    }

    pub fn set_dl(&mut self, value: u8) {
        let dx = self.dx();
        self.set_dx((dx & 0xFF00) | (value as u16));
    }

    pub fn si(&self) -> u16 {
        (self.rsi & 0xFFFF) as u16
    }

    pub fn bp(&self) -> u16 {
        (self.rbp & 0xFFFF) as u16
    }

    pub fn set_bp(&mut self, value: u16) {
        self.rbp = (self.rbp & !0xFFFF) | (value as u64);
    }

    pub fn set_si(&mut self, value: u16) {
        self.rsi = (self.rsi & !0xFFFF) | (value as u64);
    }

    pub fn di(&self) -> u16 {
        (self.rdi & 0xFFFF) as u16
    }

    pub fn set_di(&mut self, value: u16) {
        self.rdi = (self.rdi & !0xFFFF) | (value as u64);
    }

    pub fn ds(&self) -> u16 {
        self.ds
    }

    pub fn set_ds(&mut self, value: u16) {
        self.ds = value;
    }

    pub fn es(&self) -> u16 {
        self.es
    }

    pub fn set_es(&mut self, value: u16) {
        self.es = value;
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
