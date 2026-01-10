use super::GuestMemory;

pub const REG_CTRL: u32 = 0x0000;
pub const REG_STATUS: u32 = 0x0008;
pub const REG_EECD: u32 = 0x0010;
pub const REG_EERD: u32 = 0x0014;
pub const REG_CTRL_EXT: u32 = 0x0018;
pub const REG_MDIC: u32 = 0x0020;

pub const REG_ICR: u32 = 0x00c0;
pub const REG_ICS: u32 = 0x00c8;
pub const REG_IMS: u32 = 0x00d0;
pub const REG_IMC: u32 = 0x00d8;

pub const REG_RCTL: u32 = 0x0100;
pub const REG_TCTL: u32 = 0x0400;

pub const REG_RDBAL: u32 = 0x2800;
pub const REG_RDBAH: u32 = 0x2804;
pub const REG_RDLEN: u32 = 0x2808;
pub const REG_RDH: u32 = 0x2810;
pub const REG_RDT: u32 = 0x2818;

pub const REG_TDBAL: u32 = 0x3800;
pub const REG_TDBAH: u32 = 0x3804;
pub const REG_TDLEN: u32 = 0x3808;
pub const REG_TDH: u32 = 0x3810;
pub const REG_TDT: u32 = 0x3818;

pub const REG_MTA: u32 = 0x5200;
pub const REG_RAL0: u32 = 0x5400;
pub const REG_RAH0: u32 = 0x5404;

pub const CTRL_RST: u32 = 1 << 26;

pub const STATUS_FD: u32 = 1 << 0;
pub const STATUS_LU: u32 = 1 << 1;
pub const STATUS_SPEED_1000: u32 = 1 << 7;

pub const EECD_EE_PRES: u32 = 1 << 8;

pub const EERD_START: u32 = 1 << 0;
pub const EERD_DONE: u32 = 1 << 4;
pub const EERD_ADDR_SHIFT: u32 = 8;
pub const EERD_DATA_SHIFT: u32 = 16;

pub const MDIC_DATA_MASK: u32 = 0x0000_ffff;
pub const MDIC_REG_SHIFT: u32 = 16;
pub const MDIC_REG_MASK: u32 = 0x001f_0000;
pub const MDIC_PHY_SHIFT: u32 = 21;
pub const MDIC_PHY_MASK: u32 = 0x03e0_0000;
pub const MDIC_OP_WRITE: u32 = 0x0400_0000;
pub const MDIC_OP_READ: u32 = 0x0800_0000;
pub const MDIC_READY: u32 = 0x1000_0000;

pub const ICR_TXDW: u32 = 1 << 0;
pub const ICR_RXT0: u32 = 1 << 7;

pub const RCTL_EN: u32 = 1 << 1;
pub const RCTL_BSIZE_MASK: u32 = 0b11 << 16;
pub const RCTL_BSEX: u32 = 1 << 25;

pub const TCTL_EN: u32 = 1 << 1;

pub const TXD_CMD_EOP: u8 = 1 << 0;
pub const TXD_STAT_DD: u8 = 1 << 0;

pub const RXD_STAT_DD: u8 = 1 << 0;
pub const RXD_STAT_EOP: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct TxDesc {
    pub buffer_addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

impl TxDesc {
    pub fn read<M: GuestMemory>(mem: &M, addr: u64) -> Self {
        let buffer_addr = mem.read_u64(addr);
        let length = mem.read_u16(addr + 8);
        let cso = mem.read_u8(addr + 10);
        let cmd = mem.read_u8(addr + 11);
        let status = mem.read_u8(addr + 12);
        let css = mem.read_u8(addr + 13);
        let special = mem.read_u16(addr + 14);
        Self {
            buffer_addr,
            length,
            cso,
            cmd,
            status,
            css,
            special,
        }
    }

    pub fn write<M: GuestMemory>(&self, mem: &mut M, addr: u64) {
        mem.write_u64(addr, self.buffer_addr);
        mem.write_u16(addr + 8, self.length);
        mem.write_u8(addr + 10, self.cso);
        mem.write_u8(addr + 11, self.cmd);
        mem.write_u8(addr + 12, self.status);
        mem.write_u8(addr + 13, self.css);
        mem.write_u16(addr + 14, self.special);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RxDesc {
    pub buffer_addr: u64,
    pub length: u16,
    pub csum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

impl RxDesc {
    pub fn read<M: GuestMemory>(mem: &M, addr: u64) -> Self {
        let buffer_addr = mem.read_u64(addr);
        let length = mem.read_u16(addr + 8);
        let csum = mem.read_u16(addr + 10);
        let status = mem.read_u8(addr + 12);
        let errors = mem.read_u8(addr + 13);
        let special = mem.read_u16(addr + 14);
        Self {
            buffer_addr,
            length,
            csum,
            status,
            errors,
            special,
        }
    }

    pub fn write<M: GuestMemory>(&self, mem: &mut M, addr: u64) {
        mem.write_u64(addr, self.buffer_addr);
        mem.write_u16(addr + 8, self.length);
        mem.write_u16(addr + 10, self.csum);
        mem.write_u8(addr + 12, self.status);
        mem.write_u8(addr + 13, self.errors);
        mem.write_u16(addr + 14, self.special);
    }
}
