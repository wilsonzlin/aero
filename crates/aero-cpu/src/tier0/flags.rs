use super::cpu::CpuMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Cf,
    Pf,
    Af,
    Zf,
    Sf,
    Tf,
    If,
    Df,
    Of,
}

pub const FLAG_CF: u64 = 1 << 0;
pub const FLAG_PF: u64 = 1 << 2;
pub const FLAG_AF: u64 = 1 << 4;
pub const FLAG_ZF: u64 = 1 << 6;
pub const FLAG_SF: u64 = 1 << 7;
pub const FLAG_TF: u64 = 1 << 8;
pub const FLAG_IF: u64 = 1 << 9;
pub const FLAG_DF: u64 = 1 << 10;
pub const FLAG_OF: u64 = 1 << 11;

pub const FLAGS_ARITH_MASK: u64 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyOp {
    Add { carry_in: u8 },
    Sub { borrow_in: u8 },
    Logic,
}

#[derive(Debug, Clone, Copy)]
pub struct LazyFlags {
    pub op: LazyOp,
    pub size_bits: u32,
    pub lhs: u64,
    pub rhs: u64,
    pub result: u64,
}

impl LazyFlags {
    #[inline]
    pub fn mask(&self) -> u64 {
        match self.size_bits {
            8 => 0xff,
            16 => 0xffff,
            32 => 0xffff_ffff,
            64 => u64::MAX,
            other => panic!("unsupported operand size: {other}"),
        }
    }

    #[inline]
    pub fn masked_result(&self) -> u64 {
        self.result & self.mask()
    }

    #[inline]
    pub fn sign_bit(&self) -> u64 {
        1u64 << (self.size_bits - 1)
    }

    pub fn cf(&self) -> bool {
        let mask = self.mask();
        let lhs = self.lhs & mask;
        let rhs = self.rhs & mask;
        match self.op {
            LazyOp::Add { carry_in } => {
                let sum = lhs as u128 + rhs as u128 + carry_in as u128;
                sum > mask as u128
            }
            LazyOp::Sub { borrow_in } => {
                let sub = rhs as u128 + borrow_in as u128;
                sub > lhs as u128
            }
            LazyOp::Logic => false,
        }
    }

    pub fn of(&self) -> bool {
        let mask = self.mask();
        let lhs = self.lhs & mask;
        let rhs = self.rhs & mask;
        let res = self.masked_result();
        let sign = self.sign_bit();
        match self.op {
            LazyOp::Add { .. } => ((lhs ^ res) & (rhs ^ res) & sign) != 0,
            LazyOp::Sub { .. } => ((lhs ^ rhs) & (lhs ^ res) & sign) != 0,
            LazyOp::Logic => false,
        }
    }

    pub fn af(&self) -> bool {
        let lhs = self.lhs & 0xF;
        let rhs = self.rhs & 0xF;
        match self.op {
            LazyOp::Add { carry_in } => lhs + rhs + carry_in as u64 > 0xF,
            LazyOp::Sub { borrow_in } => (rhs + borrow_in as u64) > lhs,
            LazyOp::Logic => false,
        }
    }

    pub fn zf(&self) -> bool {
        self.masked_result() == 0
    }

    pub fn sf(&self) -> bool {
        (self.masked_result() & self.sign_bit()) != 0
    }

    pub fn pf(&self) -> bool {
        parity_even(self.masked_result() as u8)
    }
}

#[inline]
pub fn parity_even(byte: u8) -> bool {
    byte.count_ones() % 2 == 0
}

pub fn ip_mask_for_mode(mode: CpuMode) -> u64 {
    match mode {
        CpuMode::Real => 0xffff,
        CpuMode::Protected => 0xffff_ffff,
        CpuMode::Long => u64::MAX,
    }
}
