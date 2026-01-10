//! NVMe (NVM Express) PCI storage controller emulation.
//!
//! This crate intentionally stays small and self-contained: the only external
//! inputs are a disk backend (block storage) and a memory bus (guest physical
//! memory access for DMA).
//!
//! The implementation targets a minimal feature set needed for booting and
//! installing guests while providing a higher-performance path than AHCI.
//! Supported:
//! - BAR0 register set (CAP/VS/CC/CSTS/AQA/ASQ/ACQ + doorbells)
//! - Admin queues (submission/completion)
//! - I/O queues (submission/completion)
//! - Admin commands: IDENTIFY, CREATE IO SQ/CQ
//! - NVM commands: READ, WRITE, FLUSH
//! - PRP (PRP1/PRP2 + PRP lists). SGL is not supported.
//!
//! Interrupts:
//! - Only legacy INTx is modelled here (via [`NvmeController::intx_level`]).
//!   MSI/MSI-X are intentionally omitted for now.

use std::collections::HashMap;

const PAGE_SIZE: usize = 4096;
const NVME_SECTOR_SIZE: usize = 512;

/// Errors returned by the emulated controller when it cannot access guest memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    OutOfBounds { addr: u64, len: usize },
}

/// Guest physical memory access used for DMA.
pub trait MemoryBus {
    fn read_physical(&self, paddr: u64, buf: &mut [u8]) -> Result<(), MemoryError>;
    fn write_physical(&mut self, paddr: u64, buf: &[u8]) -> Result<(), MemoryError>;

    fn read_u16(&self, paddr: u64) -> Result<u16, MemoryError> {
        let mut buf = [0u8; 2];
        self.read_physical(paddr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&self, paddr: u64) -> Result<u32, MemoryError> {
        let mut buf = [0u8; 4];
        self.read_physical(paddr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&self, paddr: u64) -> Result<u64, MemoryError> {
        let mut buf = [0u8; 8];
        self.read_physical(paddr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn write_u16(&mut self, paddr: u64, val: u16) -> Result<(), MemoryError> {
        self.write_physical(paddr, &val.to_le_bytes())
    }

    fn write_u32(&mut self, paddr: u64, val: u32) -> Result<(), MemoryError> {
        self.write_physical(paddr, &val.to_le_bytes())
    }

    fn write_u64(&mut self, paddr: u64, val: u64) -> Result<(), MemoryError> {
        self.write_physical(paddr, &val.to_le_bytes())
    }
}

/// Errors returned by disk backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskError {
    Io,
    OutOfRange,
}

/// Block storage abstraction. The controller speaks in 512-byte LBAs.
pub trait DiskBackend: Send {
    fn read_sectors(&self, lba: u64, buffer: &mut [u8]) -> Result<(), DiskError>;
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> Result<(), DiskError>;
    fn flush(&mut self) -> Result<(), DiskError>;
    fn capacity(&self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NvmeStatus {
    sct: u8,
    sc: u8,
    dnr: bool,
}

impl NvmeStatus {
    const SUCCESS: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0,
        dnr: false,
    };

    const INVALID_OPCODE: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0x1,
        dnr: true,
    };

    const INVALID_FIELD: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0x2,
        dnr: true,
    };

    const INVALID_NS: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0xb,
        dnr: true,
    };

    const INVALID_QID: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0x1c,
        dnr: true,
    };

    fn encode_without_phase(self) -> u16 {
        let mut val: u16 = 0;
        val |= (self.sc as u16) << 1;
        val |= (self.sct as u16) << 9;
        if self.dnr {
            val |= 1 << 14;
        }
        val
    }
}

#[derive(Debug, Clone, Copy)]
struct NvmeCommand {
    opc: u8,
    cid: u16,
    nsid: u32,
    psdt: u8,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    #[allow(dead_code)]
    cdw13: u32,
    #[allow(dead_code)]
    cdw14: u32,
    #[allow(dead_code)]
    cdw15: u32,
}

impl NvmeCommand {
    fn parse(bytes: [u8; 64]) -> NvmeCommand {
        let dw0 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        NvmeCommand {
            opc: (dw0 & 0xff) as u8,
            psdt: ((dw0 >> 14) & 0x3) as u8,
            cid: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            nsid: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            prp1: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            prp2: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            cdw10: u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            cdw11: u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
            cdw12: u32::from_le_bytes(bytes[48..52].try_into().unwrap()),
            cdw13: u32::from_le_bytes(bytes[52..56].try_into().unwrap()),
            cdw14: u32::from_le_bytes(bytes[56..60].try_into().unwrap()),
            cdw15: u32::from_le_bytes(bytes[60..64].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CqEntry {
    dw0: u32,
    dw1: u32,
    sqhd: u16,
    sqid: u16,
    cid: u16,
    status: u16,
}

impl CqEntry {
    fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.dw0.to_le_bytes());
        out[4..8].copy_from_slice(&self.dw1.to_le_bytes());
        let dw2 = (self.sqid as u32) << 16 | self.sqhd as u32;
        out[8..12].copy_from_slice(&dw2.to_le_bytes());
        let dw3 = (self.status as u32) << 16 | self.cid as u32;
        out[12..16].copy_from_slice(&dw3.to_le_bytes());
        out
    }
}

#[derive(Debug)]
struct CompletionQueue {
    #[allow(dead_code)]
    id: u16,
    size: u16,
    base: u64,
    head: u16,
    tail: u16,
    phase: bool,
    irq_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
struct SubmissionQueue {
    id: u16,
    size: u16,
    base: u64,
    head: u16,
    tail: u16,
    cqid: u16,
}

/// NVMe controller state machine + BAR0 register space.
pub struct NvmeController {
    disk: Box<dyn DiskBackend>,

    // Registers (BAR0)
    cap: u64,
    vs: u32,
    intms: u32,
    cc: u32,
    csts: u32,
    aqa: u32,
    asq: u64,
    acq: u64,

    admin_sq: Option<SubmissionQueue>,
    admin_cq: Option<CompletionQueue>,
    io_sqs: HashMap<u16, SubmissionQueue>,
    io_cqs: HashMap<u16, CompletionQueue>,

    /// Legacy INTx level (asserted = true). MSI/MSI-X are not modelled.
    pub intx_level: bool,
}

impl NvmeController {
    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let mqes: u64 = 127; // 128 entries max per queue, expressed as 0-based.
        let dstrd: u64 = 0; // 4-byte doorbell stride.
        let mpsmin: u64 = 0; // 2^(12 + 0) = 4KiB.
        let mpsmax: u64 = 0;
        let css_nvm: u64 = 1; // NVM command set supported.
        let cap = (mqes & 0xffff)
            | (css_nvm << 37)
            | (mpsmin << 48)
            | (mpsmax << 52)
            | (dstrd << 32);

        NvmeController {
            disk,
            cap,
            vs: 0x0001_0400, // NVMe 1.4.0
            intms: 0,
            cc: 0,
            csts: 0,
            aqa: 0,
            asq: 0,
            acq: 0,
            admin_sq: None,
            admin_cq: None,
            io_sqs: HashMap::new(),
            io_cqs: HashMap::new(),
            intx_level: false,
        }
    }

    pub fn bar0_len(&self) -> u64 {
        // Registers (0x0..0x1000) + a small doorbell region for a few queues.
        0x4000
    }

    pub fn mmio_read(&self, offset: u64, size: usize) -> u64 {
        match (offset, size) {
            (0x0000, 8) => self.cap,
            (0x0008, 4) => self.vs as u64,
            (0x000c, 4) => self.intms as u64,
            (0x0010, 4) => 0, // INTMC is write-only.
            (0x0014, 4) => self.cc as u64,
            (0x001c, 4) => self.csts as u64,
            (0x0024, 4) => self.aqa as u64,
            (0x0028, 8) => self.asq,
            (0x0030, 8) => self.acq,
            _ => 0,
        }
    }

    pub fn mmio_write(
        &mut self,
        offset: u64,
        size: usize,
        value: u64,
        memory: &mut dyn MemoryBus,
    ) -> Result<(), MemoryError> {
        match (offset, size) {
            (0x000c, 4) => {
                self.intms |= value as u32;
                self.refresh_intx_level();
            }
            (0x0010, 4) => {
                self.intms &= !(value as u32);
                self.refresh_intx_level();
            }
            (0x0014, 4) => {
                let prev_en = self.cc & 1 != 0;
                self.cc = value as u32;
                let new_en = self.cc & 1 != 0;
                if !prev_en && new_en {
                    self.enable(memory)?;
                } else if prev_en && !new_en {
                    self.disable();
                }
            }
            (0x0024, 4) => {
                if self.cc & 1 == 0 {
                    self.aqa = value as u32;
                }
            }
            (0x0028, 8) => {
                if self.cc & 1 == 0 {
                    self.asq = value;
                }
            }
            (0x0030, 8) => {
                if self.cc & 1 == 0 {
                    self.acq = value;
                }
            }
            _ if offset >= 0x1000 && size == 4 => {
                self.write_doorbell(offset, value as u32, memory)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn enable(&mut self, _memory: &mut dyn MemoryBus) -> Result<(), MemoryError> {
        // Basic validation: admin queues must be configured and page aligned.
        let asqs = ((self.aqa >> 16) & 0x0fff) as u16 + 1;
        let acqs = (self.aqa & 0x0fff) as u16 + 1;
        if asqs == 0 || acqs == 0 || self.asq == 0 || self.acq == 0 {
            self.csts = 0;
            return Ok(());
        }
        if self.asq & (PAGE_SIZE as u64 - 1) != 0 || self.acq & (PAGE_SIZE as u64 - 1) != 0 {
            self.csts = 0;
            return Ok(());
        }

        self.admin_sq = Some(SubmissionQueue {
            id: 0,
            size: asqs,
            base: self.asq,
            head: 0,
            tail: 0,
            cqid: 0,
        });
        self.admin_cq = Some(CompletionQueue {
            id: 0,
            size: acqs,
            base: self.acq,
            head: 0,
            tail: 0,
            phase: true,
            irq_enabled: true,
        });
        self.io_sqs.clear();
        self.io_cqs.clear();
        self.csts = 1; // RDY
        self.refresh_intx_level();
        Ok(())
    }

    fn disable(&mut self) {
        self.csts = 0;
        self.admin_sq = None;
        self.admin_cq = None;
        self.io_sqs.clear();
        self.io_cqs.clear();
        self.intx_level = false;
    }

    fn write_doorbell(
        &mut self,
        offset: u64,
        value: u32,
        memory: &mut dyn MemoryBus,
    ) -> Result<(), MemoryError> {
        if self.csts & 1 == 0 {
            return Ok(());
        }
        let stride = 4u64 << ((self.cap >> 32) & 0xf);
        let idx = (offset - 0x1000) / stride;
        let qid = (idx / 2) as u16;
        let is_cq = idx % 2 == 1;
        let val = value as u16;

        if is_cq {
            self.set_cq_head(qid, val);
            self.refresh_intx_level();
            return Ok(());
        }

        self.set_sq_tail(qid, val);
        self.process_sq(qid, memory)?;
        Ok(())
    }

    fn set_sq_tail(&mut self, qid: u16, tail: u16) {
        if qid == 0 {
            if let Some(ref mut sq) = self.admin_sq {
                sq.tail = tail;
            }
            return;
        }
        if let Some(sq) = self.io_sqs.get_mut(&qid) {
            sq.tail = tail;
        }
    }

    fn set_cq_head(&mut self, qid: u16, head: u16) {
        if qid == 0 {
            if let Some(ref mut cq) = self.admin_cq {
                cq.head = head;
            }
            return;
        }
        if let Some(cq) = self.io_cqs.get_mut(&qid) {
            cq.head = head;
        }
    }

    fn refresh_intx_level(&mut self) {
        if self.intms & 1 != 0 {
            self.intx_level = false;
            return;
        }

        let mut pending = false;
        if let Some(ref cq) = self.admin_cq {
            pending |= cq.head != cq.tail && cq.irq_enabled;
        }
        for cq in self.io_cqs.values() {
            pending |= cq.head != cq.tail && cq.irq_enabled;
        }
        self.intx_level = pending;
    }

    fn process_sq(&mut self, qid: u16, memory: &mut dyn MemoryBus) -> Result<(), MemoryError> {
        if qid == 0 {
            return self.process_queue_pair_admin(memory);
        }
        self.process_queue_pair_io(qid, memory)
    }

    fn process_queue_pair_admin(&mut self, memory: &mut dyn MemoryBus) -> Result<(), MemoryError> {
        let mut sq = match self.admin_sq.take() {
            Some(sq) => sq,
            None => return Ok(()),
        };
        let mut cq = match self.admin_cq.take() {
            Some(cq) => cq,
            None => {
                self.admin_sq = Some(sq);
                return Ok(());
            }
        };

        while sq.head != sq.tail {
            let cmd = read_command(sq.base, sq.head, memory)?;
            let (status, result) = self.execute_admin(cmd, memory)?;
            sq.head = sq.head.wrapping_add(1) % sq.size;
            post_completion(&mut cq, &sq, cmd.cid, status, result, memory)?;
        }

        self.admin_sq = Some(sq);
        self.admin_cq = Some(cq);
        self.refresh_intx_level();
        Ok(())
    }

    fn process_queue_pair_io(
        &mut self,
        qid: u16,
        memory: &mut dyn MemoryBus,
    ) -> Result<(), MemoryError> {
        let cqid = match self.io_sqs.get(&qid).map(|sq| sq.cqid) {
            Some(cqid) => cqid,
            None => return Ok(()),
        };

        loop {
            let head = self.io_sqs.get(&qid).unwrap().head;
            let tail = self.io_sqs.get(&qid).unwrap().tail;
            if head == tail {
                break;
            }

            let cmd = {
                let sq = self.io_sqs.get(&qid).unwrap();
                read_command(sq.base, sq.head, memory)?
            };

            let (status, result) = self.execute_io(cmd, memory)?;

            {
                let sq = self.io_sqs.get_mut(&qid).unwrap();
                sq.head = sq.head.wrapping_add(1) % sq.size;
            }

            let (sq_snapshot, cq) = {
                let sq_snapshot = *self.io_sqs.get(&qid).unwrap();
                let cq = self.io_cqs.get_mut(&cqid).unwrap();
                (sq_snapshot, cq)
            };

            post_completion(cq, &sq_snapshot, cmd.cid, status, result, memory)?;
        }

        self.refresh_intx_level();
        Ok(())
    }

    fn execute_admin(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> Result<(NvmeStatus, u32), MemoryError> {
        if cmd.psdt != 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        match cmd.opc {
            0x06 => self.cmd_identify(cmd, memory),
            0x05 => self.cmd_create_io_cq(cmd),
            0x01 => self.cmd_create_io_sq(cmd),
            _ => Ok((NvmeStatus::INVALID_OPCODE, 0)),
        }
    }

    fn execute_io(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> Result<(NvmeStatus, u32), MemoryError> {
        if cmd.psdt != 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        match cmd.opc {
            0x00 => self.cmd_flush(),
            0x01 => self.cmd_write(cmd, memory),
            0x02 => self.cmd_read(cmd, memory),
            _ => Ok((NvmeStatus::INVALID_OPCODE, 0)),
        }
    }

    fn cmd_identify(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> Result<(NvmeStatus, u32), MemoryError> {
        let cns = (cmd.cdw10 & 0xff) as u8;
        let data = match cns {
            0x01 => self.identify_controller(),
            0x00 => self.identify_namespace(cmd.nsid),
            _ => return Ok((NvmeStatus::INVALID_FIELD, 0)),
        };

        let status = self.dma_write_prp(memory, cmd.prp1, cmd.prp2, &data)?;
        Ok((status, 0))
    }

    fn cmd_create_io_cq(&mut self, cmd: NvmeCommand) -> Result<(NvmeStatus, u32), MemoryError> {
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        let qsize = ((cmd.cdw10 >> 16) & 0xffff) as u16 + 1;
        if qsize == 0 || qsize > 128 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        if cmd.prp1 == 0 || cmd.prp1 & (PAGE_SIZE as u64 - 1) != 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        let flags = (cmd.cdw11 & 0xffff) as u16;
        let ien = flags & 0x2 != 0;

        self.io_cqs.insert(
            qid,
            CompletionQueue {
                id: qid,
                size: qsize,
                base: cmd.prp1,
                head: 0,
                tail: 0,
                phase: true,
                irq_enabled: ien,
            },
        );
        Ok((NvmeStatus::SUCCESS, 0))
    }

    fn cmd_create_io_sq(&mut self, cmd: NvmeCommand) -> Result<(NvmeStatus, u32), MemoryError> {
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        let qsize = ((cmd.cdw10 >> 16) & 0xffff) as u16 + 1;
        if qsize == 0 || qsize > 128 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        if cmd.prp1 == 0 || cmd.prp1 & (PAGE_SIZE as u64 - 1) != 0 {
            return Ok((NvmeStatus::INVALID_FIELD, 0));
        }

        let cqid = ((cmd.cdw11 >> 16) & 0xffff) as u16;
        if !self.io_cqs.contains_key(&cqid) {
            return Ok((NvmeStatus::INVALID_QID, 0));
        }

        self.io_sqs.insert(
            qid,
            SubmissionQueue {
                id: qid,
                size: qsize,
                base: cmd.prp1,
                head: 0,
                tail: 0,
                cqid,
            },
        );
        Ok((NvmeStatus::SUCCESS, 0))
    }

    fn cmd_read(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> Result<(NvmeStatus, u32), MemoryError> {
        if cmd.nsid != 1 {
            return Ok((NvmeStatus::INVALID_NS, 0));
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u32;
        let blocks = nlb as usize + 1;
        let len = blocks * NVME_SECTOR_SIZE;

        let mut data = vec![0u8; len];
        let status = match self.disk.read_sectors(slba, &mut data) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        if status != NvmeStatus::SUCCESS {
            return Ok((status, 0));
        }

        let status = self.dma_write_prp(memory, cmd.prp1, cmd.prp2, &data)?;
        Ok((status, 0))
    }

    fn cmd_write(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> Result<(NvmeStatus, u32), MemoryError> {
        if cmd.nsid != 1 {
            return Ok((NvmeStatus::INVALID_NS, 0));
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u32;
        let blocks = nlb as usize + 1;
        let len = blocks * NVME_SECTOR_SIZE;

        let mut data = vec![0u8; len];
        let status = self.dma_read_prp(memory, cmd.prp1, cmd.prp2, &mut data)?;
        if status != NvmeStatus::SUCCESS {
            return Ok((status, 0));
        }

        let status = match self.disk.write_sectors(slba, &data) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };

        Ok((status, 0))
    }

    fn cmd_flush(&mut self) -> Result<(NvmeStatus, u32), MemoryError> {
        let status = match self.disk.flush() {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        Ok((status, 0))
    }

    fn dma_write_prp(
        &self,
        memory: &mut dyn MemoryBus,
        prp1: u64,
        prp2: u64,
        data: &[u8],
    ) -> Result<NvmeStatus, MemoryError> {
        let segs = match prp_segments(memory, prp1, prp2, data.len()) {
            Ok(segs) => segs,
            Err(status) => return Ok(status),
        };
        let mut offset = 0usize;
        for (addr, len) in segs {
            memory.write_physical(addr, &data[offset..offset + len])?;
            offset += len;
        }
        Ok(NvmeStatus::SUCCESS)
    }

    fn dma_read_prp(
        &self,
        memory: &dyn MemoryBus,
        prp1: u64,
        prp2: u64,
        data: &mut [u8],
    ) -> Result<NvmeStatus, MemoryError> {
        let segs = match prp_segments(memory, prp1, prp2, data.len()) {
            Ok(segs) => segs,
            Err(status) => return Ok(status),
        };
        let mut offset = 0usize;
        for (addr, len) in segs {
            memory.read_physical(addr, &mut data[offset..offset + len])?;
            offset += len;
        }
        Ok(NvmeStatus::SUCCESS)
    }

    fn identify_controller(&self) -> Vec<u8> {
        let mut data = vec![0u8; 4096];

        // VID (u16) / SSVID (u16)
        data[0..2].copy_from_slice(&0x1d1du16.to_le_bytes());
        data[2..4].copy_from_slice(&0x1d1du16.to_le_bytes());

        // Serial Number (20 bytes) - ASCII, space padded.
        write_ascii_padded(&mut data[4..24], "AERO0000000000000001");
        // Model Number (40 bytes)
        write_ascii_padded(&mut data[24..64], "Aero NVMe Controller");
        // Firmware Revision (8 bytes)
        write_ascii_padded(&mut data[64..72], "0.1");

        // NN (Number of Namespaces) at offset 516 (0x204)
        data[516..520].copy_from_slice(&1u32.to_le_bytes());

        // MDTS (Maximum Data Transfer Size) at offset 77 (0x4d) (power of two, 0 = unlimited)
        data[77] = 0;

        data
    }

    fn identify_namespace(&self, nsid: u32) -> Vec<u8> {
        let mut data = vec![0u8; 4096];
        if nsid != 1 {
            return data;
        }

        let nsze = self.disk.capacity();
        data[0..8].copy_from_slice(&nsze.to_le_bytes()); // NSZE
        data[8..16].copy_from_slice(&nsze.to_le_bytes()); // NCAP
        data[16..24].copy_from_slice(&nsze.to_le_bytes()); // NUSE

        // FLBAS at offset 26 (0x1a): format 0, metadata 0
        data[26] = 0;

        // LBAF0 at offset 128 (0x80): MS=0, LBADS=9 (512 bytes), RP=0
        data[128 + 2] = 9;

        data
    }
}

fn read_command(
    sq_base: u64,
    head: u16,
    memory: &dyn MemoryBus,
) -> Result<NvmeCommand, MemoryError> {
    let mut bytes = [0u8; 64];
    let addr = sq_base + head as u64 * 64;
    memory.read_physical(addr, &mut bytes)?;
    Ok(NvmeCommand::parse(bytes))
}

fn post_completion(
    cq: &mut CompletionQueue,
    sq: &SubmissionQueue,
    cid: u16,
    status: NvmeStatus,
    result: u32,
    memory: &mut dyn MemoryBus,
) -> Result<(), MemoryError> {
    let next_tail = cq.tail.wrapping_add(1) % cq.size;
    if next_tail == cq.head {
        // Completion queue full. The spec expects the host to avoid this.
        return Ok(());
    }

    let status_with_phase = status.encode_without_phase() | (cq.phase as u16);
    let entry = CqEntry {
        dw0: result,
        dw1: 0,
        sqhd: sq.head,
        sqid: sq.id,
        cid,
        status: status_with_phase,
    };

    let addr = cq.base + cq.tail as u64 * 16;
    memory.write_physical(addr, &entry.to_bytes())?;

    cq.tail = next_tail;
    if cq.tail == 0 {
        cq.phase = !cq.phase;
    }
    Ok(())
}

fn write_ascii_padded(dst: &mut [u8], s: &str) {
    dst.fill(b' ');
    let bytes = s.as_bytes();
    let len = bytes.len().min(dst.len());
    dst[..len].copy_from_slice(&bytes[..len]);
}

fn prp_segments(
    memory: &dyn MemoryBus,
    prp1: u64,
    prp2: u64,
    len: usize,
) -> Result<Vec<(u64, usize)>, NvmeStatus> {
    if len == 0 {
        return Ok(Vec::new());
    }

    if prp1 == 0 {
        return Err(NvmeStatus::INVALID_FIELD);
    }

    let page_mask = PAGE_SIZE as u64 - 1;
    let first_offset = (prp1 & page_mask) as usize;
    let first_len = (PAGE_SIZE - first_offset).min(len);

    let mut segs = Vec::new();
    segs.push((prp1, first_len));
    let mut remaining = len - first_len;
    if remaining == 0 {
        return Ok(segs);
    }

    if remaining <= PAGE_SIZE {
        if prp2 == 0 || prp2 & page_mask != 0 {
            return Err(NvmeStatus::INVALID_FIELD);
        }
        segs.push((prp2, remaining));
        return Ok(segs);
    }

    if prp2 == 0 || prp2 & page_mask != 0 {
        return Err(NvmeStatus::INVALID_FIELD);
    }

    let entries_per_list = PAGE_SIZE / 8;
    let mut list_addr = prp2;
    while remaining > 0 {
        let pages_needed = (remaining + PAGE_SIZE - 1) / PAGE_SIZE;
        let max_pages_this_list = if pages_needed > entries_per_list {
            // Chained PRP list: last entry is a pointer to the next list.
            entries_per_list - 1
        } else {
            pages_needed
        };

        for entry_index in 0..max_pages_this_list {
            let entry_addr = list_addr + entry_index as u64 * 8;
            let page = match memory.read_u64(entry_addr) {
                Ok(val) => val,
                Err(_) => return Err(NvmeStatus::INVALID_FIELD),
            };
            if page == 0 || page & page_mask != 0 {
                return Err(NvmeStatus::INVALID_FIELD);
            }

            let chunk = remaining.min(PAGE_SIZE);
            segs.push((page, chunk));
            remaining -= chunk;
            if remaining == 0 {
                break;
            }
        }

        if remaining == 0 {
            break;
        }

        // Need another PRP list page.
        let chain_ptr_addr = list_addr + (entries_per_list as u64 - 1) * 8;
        list_addr = match memory.read_u64(chain_ptr_addr) {
            Ok(next) if next != 0 && next & page_mask == 0 => next,
            _ => return Err(NvmeStatus::INVALID_FIELD),
        };
    }

    Ok(segs)
}

/// A minimal PCI wrapper. A full PCI bus is out of scope for this crate but we
/// still provide enough config space to allow enumeration.
pub struct NvmePciDevice {
    pub controller: NvmeController,
    config_space: [u8; 256],
}

impl NvmePciDevice {
    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let controller = NvmeController::new(disk);
        let mut config_space = [0u8; 256];

        // Vendor / Device.
        config_space[0x00..0x02].copy_from_slice(&0x1d1du16.to_le_bytes());
        config_space[0x02..0x04].copy_from_slice(&0x1u16.to_le_bytes());

        // Class code: Mass Storage (0x01), NVM (0x08), NVMe (0x02).
        config_space[0x09] = 0x02;
        config_space[0x0a] = 0x08;
        config_space[0x0b] = 0x01;

        NvmePciDevice {
            controller,
            config_space,
        }
    }

    pub fn config_read_u32(&self, offset: u16) -> u32 {
        let off = offset as usize;
        if off + 4 > self.config_space.len() {
            return 0xffff_ffff;
        }
        u32::from_le_bytes(self.config_space[off..off + 4].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TestMem {
        buf: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self { buf: vec![0u8; size] }
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&self, paddr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
            let start = paddr as usize;
            let end = start.checked_add(buf.len()).ok_or(MemoryError::OutOfBounds {
                addr: paddr,
                len: buf.len(),
            })?;
            if end > self.buf.len() {
                return Err(MemoryError::OutOfBounds {
                    addr: paddr,
                    len: buf.len(),
                });
            }
            buf.copy_from_slice(&self.buf[start..end]);
            Ok(())
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) -> Result<(), MemoryError> {
            let start = paddr as usize;
            let end = start.checked_add(buf.len()).ok_or(MemoryError::OutOfBounds {
                addr: paddr,
                len: buf.len(),
            })?;
            if end > self.buf.len() {
                return Err(MemoryError::OutOfBounds {
                    addr: paddr,
                    len: buf.len(),
                });
            }
            self.buf[start..end].copy_from_slice(buf);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestDisk {
        data: Arc<Mutex<Vec<u8>>>,
        flushed: Arc<Mutex<u32>>,
    }

    impl TestDisk {
        fn new(sectors: u64) -> Self {
            Self {
                data: Arc::new(Mutex::new(vec![0u8; sectors as usize * NVME_SECTOR_SIZE])),
                flushed: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl DiskBackend for TestDisk {
        fn read_sectors(&self, lba: u64, buffer: &mut [u8]) -> Result<(), DiskError> {
            let offset = lba as usize * NVME_SECTOR_SIZE;
            let end = offset + buffer.len();
            let data = self.data.lock().unwrap();
            if end > data.len() {
                return Err(DiskError::OutOfRange);
            }
            buffer.copy_from_slice(&data[offset..end]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> Result<(), DiskError> {
            let offset = lba as usize * NVME_SECTOR_SIZE;
            let end = offset + buffer.len();
            let mut data = self.data.lock().unwrap();
            if end > data.len() {
                return Err(DiskError::OutOfRange);
            }
            data[offset..end].copy_from_slice(buffer);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), DiskError> {
            *self.flushed.lock().unwrap() += 1;
            Ok(())
        }

        fn capacity(&self) -> u64 {
            (self.data.lock().unwrap().len() / NVME_SECTOR_SIZE) as u64
        }
    }

    fn build_command(opc: u8) -> [u8; 64] {
        let mut cmd = [0u8; 64];
        cmd[0] = opc;
        cmd
    }

    fn set_cid(cmd: &mut [u8; 64], cid: u16) {
        cmd[2..4].copy_from_slice(&cid.to_le_bytes());
    }

    fn set_nsid(cmd: &mut [u8; 64], nsid: u32) {
        cmd[4..8].copy_from_slice(&nsid.to_le_bytes());
    }

    fn set_prp1(cmd: &mut [u8; 64], prp1: u64) {
        cmd[24..32].copy_from_slice(&prp1.to_le_bytes());
    }

    fn set_cdw10(cmd: &mut [u8; 64], val: u32) {
        cmd[40..44].copy_from_slice(&val.to_le_bytes());
    }

    fn set_cdw11(cmd: &mut [u8; 64], val: u32) {
        cmd[44..48].copy_from_slice(&val.to_le_bytes());
    }

    fn set_cdw12(cmd: &mut [u8; 64], val: u32) {
        cmd[48..52].copy_from_slice(&val.to_le_bytes());
    }

    fn read_cqe(mem: &TestMem, addr: u64) -> CqEntry {
        let mut bytes = [0u8; 16];
        mem.read_physical(addr, &mut bytes).unwrap();
        let dw0 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let dw1 = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let dw2 = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        CqEntry {
            dw0,
            dw1,
            sqhd: (dw2 & 0xffff) as u16,
            sqid: (dw2 >> 16) as u16,
            cid: (dw3 & 0xffff) as u16,
            status: (dw3 >> 16) as u16,
        }
    }

    #[test]
    fn registers_enable_sets_rdy() {
        let disk = Box::new(TestDisk::new(1024));
        let mut ctrl = NvmeController::new(disk);
        let mut mem = TestMem::new(1024 * 1024);

        ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem).unwrap(); // 16/16 queues
        ctrl.mmio_write(0x0028, 8, 0x10000, &mut mem).unwrap();
        ctrl.mmio_write(0x0030, 8, 0x20000, &mut mem).unwrap();
        ctrl.mmio_write(0x0014, 4, 1, &mut mem).unwrap();

        assert_eq!(ctrl.mmio_read(0x001c, 4) & 1, 1);
    }

    #[test]
    fn admin_identify_controller_writes_data_and_completion() {
        let disk = Box::new(TestDisk::new(1024));
        let mut ctrl = NvmeController::new(disk);
        let mut mem = TestMem::new(1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let id_buf = 0x30000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem).unwrap();
        ctrl.mmio_write(0x0028, 8, asq, &mut mem).unwrap();
        ctrl.mmio_write(0x0030, 8, acq, &mut mem).unwrap();
        ctrl.mmio_write(0x0014, 4, 1, &mut mem).unwrap();

        let mut cmd = build_command(0x06);
        set_cid(&mut cmd, 0x1234);
        set_prp1(&mut cmd, id_buf);
        set_cdw10(&mut cmd, 0x01); // CNS=1 (controller)

        mem.write_physical(asq, &cmd).unwrap();
        ctrl.mmio_write(0x1000, 4, 1, &mut mem).unwrap(); // SQ0 tail = 1

        let cqe = read_cqe(&mem, acq);
        assert_eq!(cqe.cid, 0x1234);
        assert_eq!(cqe.sqid, 0);
        assert_eq!(cqe.status & 0x1, 1); // phase
        assert_eq!(cqe.status & !0x1, 0); // success

        let vid = mem.read_u16(id_buf).unwrap();
        assert_eq!(vid, 0x1d1d);
    }

    #[test]
    fn create_io_queues_and_rw_roundtrip() {
        let disk = TestDisk::new(1024);
        let disk_data = disk.data.clone();
        let flush_count = disk.flushed.clone();
        let mut ctrl = NvmeController::new(Box::new(disk));
        let mut mem = TestMem::new(2 * 1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;
        let write_buf = 0x60000;
        let read_buf = 0x61000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem).unwrap();
        ctrl.mmio_write(0x0028, 8, asq, &mut mem).unwrap();
        ctrl.mmio_write(0x0030, 8, acq, &mut mem).unwrap();
        ctrl.mmio_write(0x0014, 4, 1, &mut mem).unwrap();

        // Create IO CQ (qid=1, size=16, PC+IEN).
        let mut cmd = build_command(0x05);
        set_cid(&mut cmd, 1);
        set_prp1(&mut cmd, io_cq);
        set_cdw10(&mut cmd, (15u32 << 16) | 1);
        set_cdw11(&mut cmd, 0x3);
        mem.write_physical(asq + 0 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1000, 4, 1, &mut mem).unwrap();

        // Create IO SQ (qid=1, size=16, CQID=1).
        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 2);
        set_prp1(&mut cmd, io_sq);
        set_cdw10(&mut cmd, (15u32 << 16) | 1);
        set_cdw11(&mut cmd, 1u32 << 16);
        mem.write_physical(asq + 1 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1000, 4, 2, &mut mem).unwrap();

        // WRITE 1 sector at LBA 0.
        let payload: Vec<u8> = (0..NVME_SECTOR_SIZE as u32).map(|v| (v & 0xff) as u8).collect();
        mem.write_physical(write_buf, &payload).unwrap();

        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, write_buf);
        set_cdw10(&mut cmd, 0); // slba low
        set_cdw11(&mut cmd, 0); // slba high
        set_cdw12(&mut cmd, 0); // nlb = 0
        mem.write_physical(io_sq + 0 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1008, 4, 1, &mut mem).unwrap(); // SQ1 tail = 1

        let cqe = read_cqe(&mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & !0x1, 0);

        // READ it back.
        let mut cmd = build_command(0x02);
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 1 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1008, 4, 2, &mut mem).unwrap(); // SQ1 tail = 2

        let cqe = read_cqe(&mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & !0x1, 0);

        let mut out = vec![0u8; NVME_SECTOR_SIZE];
        mem.read_physical(read_buf, &mut out).unwrap();
        assert_eq!(out, payload);

        // FLUSH.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 2 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1008, 4, 3, &mut mem).unwrap();
        assert_eq!(*flush_count.lock().unwrap(), 1);

        // Sanity: disk image contains the written sector.
        let data = disk_data.lock().unwrap();
        assert_eq!(&data[..NVME_SECTOR_SIZE], payload.as_slice());
    }

    #[test]
    fn cq_phase_toggles_on_wrap() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(Box::new(disk));
        let mut mem = TestMem::new(2 * 1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem).unwrap();
        ctrl.mmio_write(0x0028, 8, asq, &mut mem).unwrap();
        ctrl.mmio_write(0x0030, 8, acq, &mut mem).unwrap();
        ctrl.mmio_write(0x0014, 4, 1, &mut mem).unwrap();

        // Create IO CQ (qid=1, size=2).
        let mut cmd = build_command(0x05);
        set_cid(&mut cmd, 1);
        set_prp1(&mut cmd, io_cq);
        set_cdw10(&mut cmd, (1u32 << 16) | 1);
        set_cdw11(&mut cmd, 0x3); // PC+IEN
        mem.write_physical(asq + 0 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1000, 4, 1, &mut mem).unwrap();

        // Create IO SQ (qid=1, size=2, cqid=1).
        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 2);
        set_prp1(&mut cmd, io_sq);
        set_cdw10(&mut cmd, (1u32 << 16) | 1);
        set_cdw11(&mut cmd, 1u32 << 16);
        mem.write_physical(asq + 1 * 64, &cmd).unwrap();
        ctrl.mmio_write(0x1000, 4, 2, &mut mem).unwrap();

        // Consume admin CQ entries (2 completions from queue creation) so INTx reflects I/O CQ.
        ctrl.mmio_write(0x1004, 4, 2, &mut mem).unwrap();

        let sq_tail_db = 0x1008;
        let cq_head_db = 0x100c;

        // 1) FLUSH at SQ slot 0, CQ slot 0, phase=1.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 0 * 64, &cmd).unwrap();
        ctrl.mmio_write(sq_tail_db, 4, 1, &mut mem).unwrap();
        assert!(ctrl.intx_level);

        let cqe = read_cqe(&mem, io_cq + 0 * 16);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & 0x1, 1);
        assert_eq!(cqe.status & !0x1, 0);

        ctrl.mmio_write(cq_head_db, 4, 1, &mut mem).unwrap();
        assert!(!ctrl.intx_level);

        // 2) FLUSH at SQ slot 1, CQ slot 1, phase=1 (tail wraps after posting).
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 1 * 64, &cmd).unwrap();
        ctrl.mmio_write(sq_tail_db, 4, 0, &mut mem).unwrap();
        assert!(ctrl.intx_level);

        let cqe = read_cqe(&mem, io_cq + 1 * 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & 0x1, 1);
        assert_eq!(cqe.status & !0x1, 0);

        ctrl.mmio_write(cq_head_db, 4, 0, &mut mem).unwrap();
        assert!(!ctrl.intx_level);

        // 3) FLUSH at SQ slot 0 again, CQ slot 0 again, phase toggles to 0.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 0 * 64, &cmd).unwrap();
        ctrl.mmio_write(sq_tail_db, 4, 1, &mut mem).unwrap();

        let cqe = read_cqe(&mem, io_cq + 0 * 16);
        assert_eq!(cqe.cid, 0x12);
        assert_eq!(cqe.status & 0x1, 0);
        assert_eq!(cqe.status & !0x1, 0);
    }
}
