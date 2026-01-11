const FLAG_CF: u16 = 1 << 0;

/// Minimal real-mode register file for legacy BIOS interrupt handler unit tests.
#[derive(Debug, Default, Clone)]
pub struct RealModeCpu {
    ax: u16,
    bx: u16,
    flags: u16,
}

impl RealModeCpu {
    pub fn ax(&self) -> u16 {
        self.ax
    }

    pub fn set_ax(&mut self, value: u16) {
        self.ax = value;
    }

    pub fn ah(&self) -> u8 {
        (self.ax >> 8) as u8
    }

    pub fn set_ah(&mut self, value: u8) {
        self.ax = (self.ax & 0x00ff) | ((value as u16) << 8);
    }

    pub fn al(&self) -> u8 {
        (self.ax & 0x00ff) as u8
    }

    pub fn set_al(&mut self, value: u8) {
        self.ax = (self.ax & 0xff00) | (value as u16);
    }

    pub fn bx(&self) -> u16 {
        self.bx
    }

    pub fn set_bx(&mut self, value: u16) {
        self.bx = value;
    }

    pub fn carry(&self) -> bool {
        (self.flags & FLAG_CF) != 0
    }

    pub fn clear_cf(&mut self) {
        self.flags &= !FLAG_CF;
    }

    pub fn set_cf(&mut self) {
        self.flags |= FLAG_CF;
    }
}

