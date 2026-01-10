pub const NVME_REG_CAP: u64 = 0x0000;
pub const NVME_REG_CAP_HI: u64 = NVME_REG_CAP + 4;
pub const NVME_REG_VS: u64 = 0x0008;
pub const NVME_REG_INTMS: u64 = 0x000c;
pub const NVME_REG_INTMC: u64 = 0x0010;
pub const NVME_REG_CC: u64 = 0x0014;
pub const NVME_REG_CSTS: u64 = 0x001c;
pub const NVME_REG_AQA: u64 = 0x0024;
pub const NVME_REG_ASQ: u64 = 0x0028;
pub const NVME_REG_ASQ_HI: u64 = NVME_REG_ASQ + 4;
pub const NVME_REG_ACQ: u64 = 0x0030;
pub const NVME_REG_ACQ_HI: u64 = NVME_REG_ACQ + 4;

pub const NVME_DOORBELL_BASE: u64 = 0x1000;

pub const CC_EN: u32 = 1 << 0;

pub const CSTS_RDY: u32 = 1 << 0;
pub const CSTS_CFS: u32 = 1 << 1;

