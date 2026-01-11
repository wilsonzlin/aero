pub const REG_USBCMD: u16 = 0x00;
pub const REG_USBSTS: u16 = 0x02;
pub const REG_USBINTR: u16 = 0x04;
pub const REG_FRNUM: u16 = 0x06;
pub const REG_FLBASEADD: u16 = 0x08;
pub const REG_SOFMOD: u16 = 0x0c;
pub const REG_PORTSC1: u16 = 0x10;
pub const REG_PORTSC2: u16 = 0x12;

// UHCI 1.1 spec, section 2.1.1 "USB Command (USBCMD)".
pub const USBCMD_RS: u16 = 1 << 0;
pub const USBCMD_HCRESET: u16 = 1 << 1;
pub const USBCMD_GRESET: u16 = 1 << 2;
pub const USBCMD_EGSM: u16 = 1 << 3;
pub const USBCMD_FGR: u16 = 1 << 4;
pub const USBCMD_SWDBG: u16 = 1 << 5;
pub const USBCMD_CF: u16 = 1 << 6;
pub const USBCMD_MAXP: u16 = 1 << 7;

/// Bits we preserve on writes to [`REG_USBCMD`].
///
/// Reset bits (`HCRESET`) are intentionally excluded since they are treated as self-clearing in
/// hardware and are implemented via side-effects in the I/O handler.
pub const USBCMD_WRITE_MASK: u16 = USBCMD_RS
    | USBCMD_GRESET
    | USBCMD_EGSM
    | USBCMD_FGR
    | USBCMD_SWDBG
    | USBCMD_CF
    | USBCMD_MAXP;

// UHCI 1.1 spec, section 2.1.2 "USB Status (USBSTS)".
pub const USBSTS_USBINT: u16 = 1 << 0;
pub const USBSTS_USBERRINT: u16 = 1 << 1;
pub const USBSTS_RESUMEDETECT: u16 = 1 << 2;
pub const USBSTS_HSE: u16 = 1 << 3;
pub const USBSTS_HCPROCESSERR: u16 = 1 << 4;
pub const USBSTS_HCHALTED: u16 = 1 << 5;

/// Bits which are write-1-to-clear in [`REG_USBSTS`].
pub const USBSTS_W1C_MASK: u16 = USBSTS_USBINT
    | USBSTS_USBERRINT
    | USBSTS_RESUMEDETECT
    | USBSTS_HSE
    | USBSTS_HCPROCESSERR;

pub const USBINTR_TIMEOUT_CRC: u16 = 1 << 0;
pub const USBINTR_RESUME: u16 = 1 << 1;
pub const USBINTR_IOC: u16 = 1 << 2;
pub const USBINTR_SHORT_PACKET: u16 = 1 << 3;

#[derive(Debug, Clone)]
pub struct UhciRegs {
    pub usbcmd: u16,
    pub usbsts: u16,
    pub usbintr: u16,
    pub frnum: u16,
    pub flbaseadd: u32,
    pub sofmod: u8,
}

impl UhciRegs {
    pub fn new() -> Self {
        let mut regs = Self {
            usbcmd: USBCMD_MAXP,
            usbsts: 0,
            usbintr: 0,
            frnum: 0,
            flbaseadd: 0,
            sofmod: 64,
        };
        regs.update_halted();
        regs
    }

    pub fn update_halted(&mut self) {
        if self.usbcmd & USBCMD_RS == 0 {
            self.usbsts |= USBSTS_HCHALTED;
        } else {
            self.usbsts &= !USBSTS_HCHALTED;
        }
    }
}

impl Default for UhciRegs {
    fn default() -> Self {
        Self::new()
    }
}
