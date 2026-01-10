use crate::io::storage::disk::DiskBackend;

pub const OPC_ADMIN_DELETE_IO_SQ: u8 = 0x00;
pub const OPC_ADMIN_CREATE_IO_SQ: u8 = 0x01;
pub const OPC_ADMIN_DELETE_IO_CQ: u8 = 0x04;
pub const OPC_ADMIN_CREATE_IO_CQ: u8 = 0x05;
pub const OPC_ADMIN_IDENTIFY: u8 = 0x06;
pub const OPC_ADMIN_SET_FEATURES: u8 = 0x09;
pub const OPC_ADMIN_GET_FEATURES: u8 = 0x0a;

pub const OPC_NVM_FLUSH: u8 = 0x00;
pub const OPC_NVM_WRITE: u8 = 0x01;
pub const OPC_NVM_READ: u8 = 0x02;

pub const FID_NUMBER_OF_QUEUES: u8 = 0x07;

const STATUS_SCT_GENERIC: u8 = 0;
const STATUS_SCT_COMMAND_SPECIFIC: u8 = 1;

const STATUS_SUCCESS: u16 = 0;
const STATUS_INVALID_OPCODE: u16 = 1;
const STATUS_INVALID_FIELD: u16 = 2;
const STATUS_INVALID_NAMESPACE: u16 = 0x0b;
const STATUS_LBA_OUT_OF_RANGE: u16 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvmeStatus {
    pub sct: u8,
    pub sc: u16,
    pub dnr: bool,
}

impl NvmeStatus {
    pub const fn success() -> Self {
        Self {
            sct: STATUS_SCT_GENERIC,
            sc: STATUS_SUCCESS,
            dnr: false,
        }
    }

    pub const fn invalid_opcode() -> Self {
        Self {
            sct: STATUS_SCT_GENERIC,
            sc: STATUS_INVALID_OPCODE,
            dnr: true,
        }
    }

    pub const fn invalid_field() -> Self {
        Self {
            sct: STATUS_SCT_GENERIC,
            sc: STATUS_INVALID_FIELD,
            dnr: true,
        }
    }

    pub const fn invalid_namespace() -> Self {
        Self {
            sct: STATUS_SCT_COMMAND_SPECIFIC,
            sc: STATUS_INVALID_NAMESPACE,
            dnr: true,
        }
    }

    pub const fn lba_out_of_range() -> Self {
        Self {
            sct: STATUS_SCT_COMMAND_SPECIFIC,
            sc: STATUS_LBA_OUT_OF_RANGE,
            dnr: true,
        }
    }

    pub fn to_cqe_status_field(self, phase: bool) -> u16 {
        let mut status = 0u16;
        if phase {
            status |= 1;
        }
        status |= (self.sc & 0xff) << 1;
        status |= ((self.sct as u16) & 0x7) << 9;
        if self.dnr {
            status |= 1 << 13;
        }
        status
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvmeCommand {
    pub opc: u8,
    pub cid: u16,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
}

impl NvmeCommand {
    pub fn from_dwords(dwords: &[u32; 16]) -> Self {
        let dw0 = dwords[0];
        let opc = (dw0 & 0xff) as u8;
        let cid = (dw0 >> 16) as u16;
        let nsid = dwords[1];
        let prp1 = (dwords[6] as u64) | ((dwords[7] as u64) << 32);
        let prp2 = (dwords[8] as u64) | ((dwords[9] as u64) << 32);
        Self {
            opc,
            cid,
            nsid,
            prp1,
            prp2,
            cdw10: dwords[10],
            cdw11: dwords[11],
            cdw12: dwords[12],
        }
    }

    pub fn identify_cns(self) -> u8 {
        (self.cdw10 & 0xff) as u8
    }

    pub fn feature_id(self) -> u8 {
        (self.cdw10 & 0xff) as u8
    }

    pub fn qid(self) -> u16 {
        (self.cdw10 & 0xffff) as u16
    }

    pub fn qsize(self) -> u16 {
        ((self.cdw10 >> 16) as u16).wrapping_add(1)
    }

    pub fn slba(self) -> u64 {
        (self.cdw10 as u64) | ((self.cdw11 as u64) << 32)
    }

    pub fn nlb(self) -> u32 {
        (self.cdw12 & 0xffff).wrapping_add(1)
    }
}

pub fn build_identify_controller(nsid_count: u32, mdts: u8) -> [u8; 4096] {
    let mut buf = [0u8; 4096];

    buf[0..2].copy_from_slice(&0x1b36u16.to_le_bytes());
    buf[2..4].copy_from_slice(&0x1b36u16.to_le_bytes());

    let sn = b"AERO0000000000000000";
    buf[4..24].copy_from_slice(sn);

    buf[24..64].fill(b' ');
    let mn = b"Aero NVMe VirtualDrive";
    buf[24..24 + mn.len()].copy_from_slice(mn);

    let fr = b"0.1.0   ";
    buf[64..72].copy_from_slice(fr);

    buf[77] = mdts;

    let nn_offset = 516;
    buf[nn_offset..nn_offset + 4].copy_from_slice(&nsid_count.to_le_bytes());

    buf
}

pub fn build_identify_namespace(disk: &dyn DiskBackend) -> [u8; 4096] {
    let mut buf = [0u8; 4096];
    let total = disk.total_sectors();
    buf[0..8].copy_from_slice(&total.to_le_bytes());
    buf[8..16].copy_from_slice(&total.to_le_bytes());
    buf[16..24].copy_from_slice(&total.to_le_bytes());

    buf[25] = 0;
    buf[26] = 0;

    let lbaf0_off = 128;
    let sector_size = disk.sector_size().max(1);
    let lbads = sector_size.trailing_zeros() as u8;
    buf[lbaf0_off + 2] = lbads;

    buf
}

