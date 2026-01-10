//! ATA FIS helpers (H2D Register FIS parsing and D2H Register FIS generation).

pub const FIS_TYPE_REG_H2D: u8 = 0x27;
pub const FIS_TYPE_REG_D2H: u8 = 0x34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegH2dFis {
    pub command: u8,
    pub feature_low: u8,
    pub feature_high: u8,
    pub lba: u64,
    pub device: u8,
    pub sector_count: u16,
    pub control: u8,
}

impl RegH2dFis {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 20 {
            return None;
        }
        if bytes[0] != FIS_TYPE_REG_H2D {
            return None;
        }
        // Byte 1 bit7 must be 1 for "command" FIS.
        if bytes[1] & 0x80 == 0 {
            return None;
        }

        let lba0 = bytes[4] as u64;
        let lba1 = (bytes[5] as u64) << 8;
        let lba2 = (bytes[6] as u64) << 16;
        let lba3 = (bytes[8] as u64) << 24;
        let lba4 = (bytes[9] as u64) << 32;
        let lba5 = (bytes[10] as u64) << 40;
        let lba = lba0 | lba1 | lba2 | lba3 | lba4 | lba5;

        let sector_count = (bytes[12] as u16) | ((bytes[13] as u16) << 8);

        Some(Self {
            command: bytes[2],
            feature_low: bytes[3],
            feature_high: bytes[11],
            lba,
            device: bytes[7],
            sector_count,
            control: bytes[15],
        })
    }
}

pub fn build_reg_d2h_fis(status: u8, error: u8) -> [u8; 20] {
    let mut fis = [0u8; 20];
    fis[0] = FIS_TYPE_REG_D2H;
    // Byte 1: interrupt bit (bit6) + PMP (bits0-3). We set interrupt=1.
    fis[1] = 1 << 6;
    fis[2] = status;
    fis[3] = error;
    fis
}
