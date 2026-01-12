//! Minimal NVMe (PCIe) controller emulation.

mod commands;
mod queue;
mod registers;

use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use crate::io::storage::disk::{DiskBackend, DiskError};
use commands::{
    build_identify_controller, build_identify_namespace, NvmeCommand, NvmeStatus,
    FID_NUMBER_OF_QUEUES, OPC_ADMIN_ASYNC_EVENT_REQUEST, OPC_ADMIN_CREATE_IO_CQ,
    OPC_ADMIN_CREATE_IO_SQ, OPC_ADMIN_DELETE_IO_CQ, OPC_ADMIN_DELETE_IO_SQ, OPC_ADMIN_GET_FEATURES,
    OPC_ADMIN_GET_LOG_PAGE, OPC_ADMIN_IDENTIFY, OPC_ADMIN_KEEP_ALIVE, OPC_ADMIN_SET_FEATURES,
    OPC_NVM_FLUSH, OPC_NVM_READ, OPC_NVM_WRITE,
};
use memory::MemoryBus;
use queue::{
    dma_read, dma_write, dma_write_zeros, read_command_dwords, CompletionQueue, PrpError,
    QueuePair, SubmissionQueue, NVME_MAX_DMA_BYTES,
};
use registers::*;
use std::collections::HashMap;

// DoS guards / implementation limits.
const NVME_MAX_PAGE_SIZE: usize = 64 * 1024;
const NVME_MAX_QUEUE_ENTRIES: u16 = 256;

pub struct NvmeController {
    cap: u64,
    vs: u32,
    intms: u32,
    cc: u32,
    csts: u32,
    aqa: u32,
    asq: u64,
    acq: u64,
    page_size: usize,
    admin: Option<QueuePair>,
    io_sqs: HashMap<u16, SubmissionQueue>,
    io_cqs: HashMap<u16, CompletionQueue>,
    features: HashMap<u8, u32>,
    pending_aer: Vec<u16>,
    disk: Box<dyn DiskBackend>,
    num_io_sqs: u16,
    num_io_cqs: u16,
    irq_level: bool,
}

impl NvmeController {
    pub const BAR0_SIZE: u64 = 0x4000;

    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let mqes = 0xffu64;
        let cqr = 1u64 << 16;
        // NVMe CAP.TO is in 500ms units; keep it non-zero so guests don't treat the
        // controller as immediately timing out.
        let to = 0x0au64 << 24;
        let css_nvm = 1u64 << 37;
        let cap = mqes | cqr | to | css_nvm;

        Self {
            cap,
            vs: 0x0001_0300,
            intms: 0,
            cc: 0,
            csts: 0,
            aqa: 0,
            asq: 0,
            acq: 0,
            page_size: 4096,
            admin: None,
            io_sqs: HashMap::new(),
            io_cqs: HashMap::new(),
            features: HashMap::new(),
            pending_aer: Vec::new(),
            disk,
            num_io_sqs: 1,
            num_io_cqs: 1,
            irq_level: false,
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    fn doorbell_stride(&self) -> u64 {
        4
    }

    fn enable(&mut self) {
        self.csts &= !CSTS_CFS;
        let mps = (self.cc >> 7) & 0xf;
        let page_size = 4096usize.checked_shl(mps).unwrap_or(0);
        if !(4096..=NVME_MAX_PAGE_SIZE).contains(&page_size) {
            // Unsupported memory page size; fail the enable attempt rather than risking huge
            // allocations (e.g. zero-fill buffers sized to `page_size`).
            self.page_size = 4096;
            self.csts |= CSTS_CFS;
            self.csts &= !CSTS_RDY;
            self.admin = None;
            self.io_sqs.clear();
            self.io_cqs.clear();
            self.features.clear();
            self.pending_aer.clear();
            self.irq_level = false;
            return;
        }

        self.csts |= CSTS_RDY;
        self.page_size = page_size;

        let asqs = ((self.aqa & 0xffff) + 1)
            .min(NVME_MAX_QUEUE_ENTRIES as u32)
            .max(1) as u16;
        let acqs = (((self.aqa >> 16) & 0xffff) + 1)
            .min(NVME_MAX_QUEUE_ENTRIES as u32)
            .max(1) as u16;
        let admin_sq = SubmissionQueue {
            id: 0,
            base: self.asq,
            size: asqs.max(1),
            head: 0,
            tail: 0,
            cqid: 0,
        };
        let admin_cq = CompletionQueue {
            base: self.acq,
            size: acqs.max(1),
            head: 0,
            tail: 0,
            phase: true,
            host_phase: true,
        };
        self.admin = Some(QueuePair {
            sq: admin_sq,
            cq: admin_cq,
        });

        self.io_sqs.clear();
        self.io_cqs.clear();
        self.features.clear();
        self.pending_aer.clear();
        self.irq_level = false;
    }

    fn disable(&mut self) {
        self.csts &= !CSTS_RDY;
        self.admin = None;
        self.io_sqs.clear();
        self.io_cqs.clear();
        self.features.clear();
        self.pending_aer.clear();
        self.irq_level = false;
    }

    fn update_irq_level(&mut self) {
        if self.intms != 0 {
            self.irq_level = false;
            return;
        }

        let mut pending = false;
        if let Some(admin) = self.admin.as_ref() {
            pending |= !admin.cq.is_empty();
        }
        if !pending {
            pending = self.io_cqs.values().any(|cq| !cq.is_empty());
        }
        self.irq_level = pending;
    }

    fn mmio_read_dword(&mut self, offset: u64) -> u32 {
        match offset {
            NVME_REG_CAP => self.cap as u32,
            NVME_REG_CAP_HI => (self.cap >> 32) as u32,
            NVME_REG_VS => self.vs,
            NVME_REG_INTMS => self.intms,
            NVME_REG_CC => self.cc,
            NVME_REG_CSTS => self.csts,
            NVME_REG_AQA => self.aqa,
            NVME_REG_ASQ => self.asq as u32,
            NVME_REG_ASQ_HI => (self.asq >> 32) as u32,
            NVME_REG_ACQ => self.acq as u32,
            NVME_REG_ACQ_HI => (self.acq >> 32) as u32,
            _ if offset >= NVME_DOORBELL_BASE => self.doorbell_read(offset),
            _ => 0,
        }
    }

    fn doorbell_read(&self, offset: u64) -> u32 {
        let stride = self.doorbell_stride();
        let rel = offset.saturating_sub(NVME_DOORBELL_BASE);
        let db_index = rel / stride;
        let qid = (db_index / 2) as u16;
        let is_cq = db_index % 2 == 1;

        if qid == 0 {
            let Some(admin) = self.admin.as_ref() else {
                return 0;
            };
            return if is_cq {
                u32::from(admin.cq.head)
            } else {
                u32::from(admin.sq.tail)
            };
        }

        if is_cq {
            self.io_cqs.get(&qid).map_or(0, |cq| u32::from(cq.head))
        } else {
            self.io_sqs.get(&qid).map_or(0, |sq| u32::from(sq.tail))
        }
    }

    fn mmio_write_dword(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        match offset {
            NVME_REG_INTMS => {
                self.intms |= value;
                self.update_irq_level();
            }
            NVME_REG_INTMC => {
                self.intms &= !value;
                self.update_irq_level();
            }
            NVME_REG_CC => {
                let prev_en = self.cc & CC_EN != 0;
                self.cc = value;
                let next_en = self.cc & CC_EN != 0;
                match (prev_en, next_en) {
                    (false, true) => self.enable(),
                    (true, false) => self.disable(),
                    _ => {}
                }
            }
            NVME_REG_AQA => self.aqa = value,
            NVME_REG_ASQ => self.asq = (self.asq & 0xffff_ffff_0000_0000) | value as u64,
            NVME_REG_ASQ_HI => {
                self.asq = (self.asq & 0x0000_0000_ffff_ffff) | ((value as u64) << 32)
            }
            NVME_REG_ACQ => self.acq = (self.acq & 0xffff_ffff_0000_0000) | value as u64,
            NVME_REG_ACQ_HI => {
                self.acq = (self.acq & 0x0000_0000_ffff_ffff) | ((value as u64) << 32)
            }
            _ if offset >= NVME_DOORBELL_BASE => self.handle_doorbell(mem, offset, value),
            _ => {}
        }
    }

    fn handle_doorbell(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        let stride = self.doorbell_stride();
        let rel = offset - NVME_DOORBELL_BASE;
        let db_index = rel / stride;
        let qid = (db_index / 2) as u16;
        let is_cq = db_index % 2 == 1;
        if is_cq {
            self.update_cq_head(qid, value as u16);
        } else {
            self.update_sq_tail_and_process(mem, qid, value as u16);
        }
    }

    fn update_cq_head(&mut self, qid: u16, head: u16) {
        if qid == 0 {
            if let Some(admin) = self.admin.as_mut() {
                admin.cq.update_head(head);
            }
            self.update_irq_level();
            return;
        }
        if let Some(cq) = self.io_cqs.get_mut(&qid) {
            cq.update_head(head);
        }
        self.update_irq_level();
    }

    fn update_sq_tail_and_process(&mut self, mem: &mut dyn MemoryBus, qid: u16, tail: u16) {
        if qid == 0 {
            if let Some(admin) = self.admin.as_mut() {
                admin.sq.tail = tail % admin.sq.size;
            }
            self.process_queue(mem, 0);
            return;
        }
        if let Some(sq) = self.io_sqs.get_mut(&qid) {
            sq.tail = tail % sq.size;
        }
        self.process_queue(mem, qid);
    }

    fn update_sq_tail(&mut self, qid: u16, tail: u16) {
        if qid == 0 {
            if let Some(admin) = self.admin.as_mut() {
                admin.sq.tail = tail % admin.sq.size;
            }
            return;
        }
        if let Some(sq) = self.io_sqs.get_mut(&qid) {
            sq.tail = tail % sq.size;
        }
    }

    fn has_pending_submissions(&self) -> bool {
        if let Some(admin) = self.admin.as_ref() {
            if admin.sq.head != admin.sq.tail {
                return true;
            }
        }
        self.io_sqs.values().any(|sq| sq.head != sq.tail)
    }

    fn process_all_queues(&mut self, mem: &mut dyn MemoryBus) {
        self.process_queue(mem, 0);
        let qids: Vec<u16> = self.io_sqs.keys().copied().collect();
        for qid in qids {
            self.process_queue(mem, qid);
        }
    }

    fn process_queue(&mut self, mem: &mut dyn MemoryBus, qid: u16) {
        if self.csts & CSTS_RDY == 0 {
            return;
        }

        if qid == 0 {
            let Some(mut admin) = self.admin.take() else {
                return;
            };
            self.process_submission_queue(mem, &mut admin.sq, &mut admin.cq, true);
            self.admin = Some(admin);
            self.update_irq_level();
            return;
        }

        let Some(mut sq) = self.io_sqs.remove(&qid) else {
            return;
        };
        let cqid = sq.cqid;
        let Some(mut cq) = self.io_cqs.remove(&cqid) else {
            self.io_sqs.insert(qid, sq);
            return;
        };
        self.process_submission_queue(mem, &mut sq, &mut cq, false);
        self.io_cqs.insert(cqid, cq);
        self.io_sqs.insert(qid, sq);
        self.update_irq_level();
    }

    fn process_submission_queue(
        &mut self,
        mem: &mut dyn MemoryBus,
        sq: &mut SubmissionQueue,
        cq: &mut CompletionQueue,
        admin: bool,
    ) {
        while sq.head != sq.tail {
            if cq.is_full() {
                break;
            }

            let cmd_addr = sq.base + (sq.head as u64) * 64;
            let dwords = read_command_dwords(mem, cmd_addr);
            let cmd = NvmeCommand::from_dwords(&dwords);

            sq.head = (sq.head + 1) % sq.size;

            if admin {
                if let Some((dw0, status)) = self.execute_admin(mem, cmd) {
                    let status_field = status.to_cqe_status_field(cq.phase);
                    cq.push(mem, dw0, sq.head, sq.id, cmd.cid, status_field);
                }
            } else {
                let (dw0, status) = self.execute_io(mem, cmd);
                let status_field = status.to_cqe_status_field(cq.phase);
                cq.push(mem, dw0, sq.head, sq.id, cmd.cid, status_field);
            }
        }
    }

    fn execute_admin(
        &mut self,
        mem: &mut dyn MemoryBus,
        cmd: NvmeCommand,
    ) -> Option<(u32, NvmeStatus)> {
        match cmd.opc {
            OPC_ADMIN_IDENTIFY => Some(self.cmd_identify(mem, cmd)),
            OPC_ADMIN_GET_LOG_PAGE => Some(self.cmd_get_log_page(mem, cmd)),
            OPC_ADMIN_ASYNC_EVENT_REQUEST => {
                self.pending_aer.push(cmd.cid);
                None
            }
            OPC_ADMIN_KEEP_ALIVE => Some((0, NvmeStatus::success())),
            OPC_ADMIN_GET_FEATURES => Some(self.cmd_get_features(cmd)),
            OPC_ADMIN_SET_FEATURES => Some(self.cmd_set_features(cmd)),
            OPC_ADMIN_CREATE_IO_CQ => Some(self.cmd_create_io_cq(cmd)),
            OPC_ADMIN_CREATE_IO_SQ => Some(self.cmd_create_io_sq(cmd)),
            OPC_ADMIN_DELETE_IO_CQ => Some(self.cmd_delete_io_cq(cmd)),
            OPC_ADMIN_DELETE_IO_SQ => Some(self.cmd_delete_io_sq(cmd)),
            _ => Some((0, NvmeStatus::invalid_opcode())),
        }
    }

    fn execute_io(&mut self, mem: &mut dyn MemoryBus, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        match cmd.opc {
            OPC_NVM_FLUSH => self.cmd_flush(cmd),
            OPC_NVM_READ => self.cmd_read(mem, cmd),
            OPC_NVM_WRITE => self.cmd_write(mem, cmd),
            _ => (0, NvmeStatus::invalid_opcode()),
        }
    }

    fn cmd_identify(&mut self, mem: &mut dyn MemoryBus, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        match cmd.identify_cns() {
            1 => {
                // Advertise a relatively large max transfer size so guests can issue
                // multi-page PRP transfers without falling back to tiny requests.
                // Max transfer = 2^MDTS * min page size (typically 4KiB).
                let mdts = 10;
                let data = build_identify_controller(1, mdts);
                if dma_write(mem, cmd.prp1, cmd.prp2, &data, self.page_size).is_err() {
                    return (0, NvmeStatus::invalid_field());
                }
                (0, NvmeStatus::success())
            }
            0 => {
                if cmd.nsid != 1 {
                    return (0, NvmeStatus::invalid_namespace());
                }
                let data = build_identify_namespace(&*self.disk);
                if dma_write(mem, cmd.prp1, cmd.prp2, &data, self.page_size).is_err() {
                    return (0, NvmeStatus::invalid_field());
                }
                (0, NvmeStatus::success())
            }
            2 => {
                // Active Namespace ID list (Identify CNS=2).
                //
                // Linux/Windows may use this to enumerate namespaces instead of iterating
                // 1..=NN. We only expose a single namespace (NSID 1) for now.
                let start = if cmd.nsid == 0 { 1 } else { cmd.nsid };
                let mut data = [0u8; 4096];
                if start <= 1 {
                    data[0..4].copy_from_slice(&1u32.to_le_bytes());
                }
                if dma_write(mem, cmd.prp1, cmd.prp2, &data, self.page_size).is_err() {
                    return (0, NvmeStatus::invalid_field());
                }
                (0, NvmeStatus::success())
            }
            _ => (0, NvmeStatus::invalid_field()),
        }
    }

    fn cmd_get_log_page(&mut self, mem: &mut dyn MemoryBus, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        let Some(len) = cmd.log_page_len_bytes() else {
            return (0, NvmeStatus::invalid_field());
        };
        // We currently return zeroed data for all log pages. This is enough for OS drivers
        // to make forward progress during initialization.
        let _ = cmd.log_page_lid();
        let _ = cmd.log_page_offset_bytes();

        if dma_write_zeros(mem, cmd.prp1, cmd.prp2, len, self.page_size).is_err() {
            return (0, NvmeStatus::invalid_field());
        }
        (0, NvmeStatus::success())
    }

    fn cmd_get_features(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        match cmd.feature_id() {
            FID_NUMBER_OF_QUEUES => {
                let nsq = self.num_io_sqs.saturating_sub(1) as u32;
                let ncq = self.num_io_cqs.saturating_sub(1) as u32;
                let val = (nsq << 16) | (ncq & 0xffff);
                (val, NvmeStatus::success())
            }
            fid => {
                let val = self.features.get(&fid).copied().unwrap_or(0);
                (val, NvmeStatus::success())
            }
        }
    }

    fn cmd_set_features(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        match cmd.feature_id() {
            FID_NUMBER_OF_QUEUES => {
                let requested = cmd.cdw11;
                let _req_ncq = (requested & 0xffff) + 1;
                let _req_nsq = (requested >> 16) + 1;
                // We only support a single IO SQ/CQ pair for now; clamp any request to 1.
                self.num_io_cqs = 1;
                self.num_io_sqs = 1;
                let val = ((self.num_io_sqs.saturating_sub(1) as u32) << 16)
                    | (self.num_io_cqs.saturating_sub(1) as u32);
                (val, NvmeStatus::success())
            }
            fid => {
                self.features.insert(fid, cmd.cdw11);
                (cmd.cdw11, NvmeStatus::success())
            }
        }
    }

    fn cmd_create_io_cq(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        let qid = cmd.qid();
        if qid == 0 || self.io_cqs.contains_key(&qid) {
            return (0, NvmeStatus::invalid_field());
        }
        // DoS guard: keep the number of guest-created IO queues bounded. Otherwise a malicious
        // guest could create thousands of queues and force O(n) scans in interrupt/doorbell paths.
        if self.num_io_cqs == 0 || self.io_cqs.len() >= usize::from(self.num_io_cqs) {
            return (0, NvmeStatus::invalid_field());
        }
        let size = cmd.qsize();
        if size == 0
            || size > NVME_MAX_QUEUE_ENTRIES
            || !cmd.prp1.is_multiple_of(self.page_size as u64)
        {
            return (0, NvmeStatus::invalid_field());
        }

        let cq = CompletionQueue {
            base: cmd.prp1,
            size,
            head: 0,
            tail: 0,
            phase: true,
            host_phase: true,
        };
        self.io_cqs.insert(qid, cq);
        (0, NvmeStatus::success())
    }

    fn cmd_create_io_sq(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        let qid = cmd.qid();
        if qid == 0 || self.io_sqs.contains_key(&qid) {
            return (0, NvmeStatus::invalid_field());
        }
        // DoS guard: keep the number of guest-created IO queues bounded.
        if self.num_io_sqs == 0 || self.io_sqs.len() >= usize::from(self.num_io_sqs) {
            return (0, NvmeStatus::invalid_field());
        }
        let size = cmd.qsize();
        if size == 0
            || size > NVME_MAX_QUEUE_ENTRIES
            || !cmd.prp1.is_multiple_of(self.page_size as u64)
        {
            return (0, NvmeStatus::invalid_field());
        }

        let cqid = (cmd.cdw11 & 0xffff) as u16;
        if !self.io_cqs.contains_key(&cqid) {
            return (0, NvmeStatus::invalid_field());
        }

        let sq = SubmissionQueue {
            id: qid,
            base: cmd.prp1,
            size,
            head: 0,
            tail: 0,
            cqid,
        };
        self.io_sqs.insert(qid, sq);
        (0, NvmeStatus::success())
    }

    fn cmd_delete_io_cq(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        let qid = cmd.qid();
        if qid == 0 || self.io_cqs.remove(&qid).is_none() {
            return (0, NvmeStatus::invalid_field());
        }
        (0, NvmeStatus::success())
    }

    fn cmd_delete_io_sq(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        let qid = cmd.qid();
        if qid == 0 || self.io_sqs.remove(&qid).is_none() {
            return (0, NvmeStatus::invalid_field());
        }
        (0, NvmeStatus::success())
    }

    fn cmd_flush(&mut self, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        if cmd.nsid != 1 {
            return (0, NvmeStatus::invalid_namespace());
        }
        match self.disk.flush() {
            Ok(()) => (0, NvmeStatus::success()),
            Err(_) => (0, NvmeStatus::invalid_field()),
        }
    }

    fn cmd_read(&mut self, mem: &mut dyn MemoryBus, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        if cmd.nsid != 1 {
            return (0, NvmeStatus::invalid_namespace());
        }
        let page_size = self.page_size;
        let sector_size = self.disk.sector_size() as usize;
        let slba = cmd.slba();
        let nlb = cmd.nlb() as u64;
        if slba
            .checked_add(nlb)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (0, NvmeStatus::lba_out_of_range());
        }

        let Some(len) = (nlb as usize).checked_mul(sector_size) else {
            return (0, NvmeStatus::invalid_field());
        };
        if len > NVME_MAX_DMA_BYTES {
            return (0, NvmeStatus::invalid_field());
        }
        let mut data = Vec::new();
        if data.try_reserve_exact(len).is_err() {
            return (0, NvmeStatus::invalid_field());
        }
        data.resize(len, 0);
        if self.disk.read_sectors(slba, &mut data).is_err() {
            return (0, NvmeStatus::invalid_field());
        }
        if dma_write(mem, cmd.prp1, cmd.prp2, &data, page_size).is_err() {
            return (0, NvmeStatus::invalid_field());
        }
        (0, NvmeStatus::success())
    }

    fn cmd_write(&mut self, mem: &mut dyn MemoryBus, cmd: NvmeCommand) -> (u32, NvmeStatus) {
        if cmd.nsid != 1 {
            return (0, NvmeStatus::invalid_namespace());
        }
        let page_size = self.page_size;
        let sector_size = self.disk.sector_size() as usize;
        let slba = cmd.slba();
        let nlb = cmd.nlb() as u64;
        if slba
            .checked_add(nlb)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (0, NvmeStatus::lba_out_of_range());
        }

        let Some(len) = (nlb as usize).checked_mul(sector_size) else {
            return (0, NvmeStatus::invalid_field());
        };
        if len > NVME_MAX_DMA_BYTES {
            return (0, NvmeStatus::invalid_field());
        }
        let data = match dma_read(mem, cmd.prp1, cmd.prp2, len, page_size) {
            Ok(data) => data,
            Err(_) => return (0, NvmeStatus::invalid_field()),
        };
        if self.disk.write_sectors(slba, &data).is_err() {
            return (0, NvmeStatus::invalid_field());
        }
        (0, NvmeStatus::success())
    }
}

impl MmioDevice for NvmeController {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value = self.mmio_read_dword(aligned);
        match size {
            1 => (value >> shift) & 0xff,
            2 => (value >> shift) & 0xffff,
            4 => value,
            _ => 0,
        }
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let value32 = match size {
            1 => (value & 0xff) << shift,
            2 => (value & 0xffff) << shift,
            4 => value,
            _ => return,
        };

        let merged = if size == 4 {
            value32
        } else {
            let cur = self.mmio_read(mem, aligned, 4);
            let mask = match size {
                1 => 0xffu32 << shift,
                2 => 0xffffu32 << shift,
                _ => 0,
            };
            (cur & !mask) | value32
        };

        self.mmio_write_dword(mem, aligned, merged);
    }
}

impl NvmeController {
    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        self.mmio_write(mem, offset, 4, value);
    }
}

pub struct NvmePciDevice {
    config: PciConfigSpace,
    pub bar0: u64,
    bar0_probe: bool,
    deferred_processing: bool,
    pub controller: NvmeController,
}

impl NvmePciDevice {
    pub fn new(controller: NvmeController, bar0: u64) -> Self {
        let mut config = PciConfigSpace::new();
        config.set_u16(0x00, 0x1b36);
        config.set_u16(0x02, 0x0010);

        config.write(0x09, 1, 0x02);
        config.write(0x0a, 1, 0x08);
        config.write(0x0b, 1, 0x01);

        // BAR0/BAR1: 64-bit non-prefetchable MMIO.
        config.set_u32(0x10, (bar0 as u32 & 0xffff_fff0) | 0x4);
        config.set_u32(0x14, (bar0 >> 32) as u32);
        config.write(0x3d, 1, 1);

        Self {
            config,
            bar0,
            bar0_probe: false,
            deferred_processing: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_level()
    }
}

impl PciDevice for NvmePciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x10 && size == 4 {
            return if self.bar0_probe {
                (!(NvmeController::BAR0_SIZE as u32 - 1) & 0xffff_fff0) | 0x4
            } else {
                (self.bar0 as u32 & 0xffff_fff0) | 0x4
            };
        }
        if offset == 0x14 && size == 4 {
            return if self.bar0_probe {
                0xffff_ffff
            } else {
                (self.bar0 >> 32) as u32
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        let prev_command = self.config.read(0x04, 2) as u16;
        if offset == 0x10 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 = 0;
                self.config.write(offset, size, 0);
                self.update_deferred_processing(prev_command);
                return;
            }

            self.bar0_probe = false;
            let addr_lo = (value & 0xffff_fff0) as u64;
            self.bar0 = (self.bar0 & 0xffff_ffff_0000_0000) | addr_lo;
            self.config.write(offset, size, (addr_lo as u32) | 0x4);
            self.update_deferred_processing(prev_command);
            return;
        }
        if offset == 0x14 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 &= 0xffff_ffff;
                self.config.write(offset, size, 0);
                self.update_deferred_processing(prev_command);
                return;
            }

            self.bar0_probe = false;
            self.bar0 = (self.bar0 & 0x0000_0000_ffff_ffff) | ((value as u64) << 32);
            self.config.write(offset, size, value);
            self.update_deferred_processing(prev_command);
            return;
        }
        self.config.write(offset, size, value);
        self.update_deferred_processing(prev_command);
    }
}

impl MmioDevice for NvmePciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        let command = self.config.read(0x04, 2) as u16;
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if (command & (1 << 1)) == 0 {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }

        self.maybe_process_deferred(mem, command);
        self.controller.mmio_read(mem, offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let command = self.config.read(0x04, 2) as u16;
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if (command & (1 << 1)) == 0 {
            return;
        }

        self.maybe_process_deferred(mem, command);

        // Gate bus-master DMA on PCI command Bus Master Enable (bit 2).
        //
        // For NVMe, DMA is triggered by SQ tail doorbell writes. When BME is clear, latch the
        // tail pointer but do not execute queue processing or touch guest memory.
        if (command & (1 << 2)) == 0 {
            let aligned = offset & !3;
            if aligned >= NVME_DOORBELL_BASE {
                let stride = self.controller.doorbell_stride();
                let rel = aligned - NVME_DOORBELL_BASE;
                let db_index = rel / stride;
                let qid = (db_index / 2) as u16;
                let is_cq = db_index % 2 == 1;

                if !is_cq {
                    let shift = (offset & 3) * 8;
                    let value32 = match size {
                        1 => (value & 0xff) << shift,
                        2 => (value & 0xffff) << shift,
                        4 => value,
                        _ => return,
                    };

                    let merged = if size == 4 {
                        value32
                    } else {
                        let cur = self.controller.mmio_read(mem, aligned, 4);
                        let mask = match size {
                            1 => 0xffu32 << shift,
                            2 => 0xffffu32 << shift,
                            _ => 0,
                        };
                        (cur & !mask) | value32
                    };

                    self.controller.update_sq_tail(qid, merged as u16);
                    self.deferred_processing = self.controller.has_pending_submissions();
                    return;
                }
            }
        }

        self.controller.mmio_write(mem, offset, size, value);
    }
}

impl NvmePciDevice {
    fn update_deferred_processing(&mut self, prev_command: u16) {
        let command = self.config.read(0x04, 2) as u16;
        let prev_bme = (prev_command & (1 << 2)) != 0;
        let next_bme = (command & (1 << 2)) != 0;

        if !prev_bme && next_bme && self.controller.has_pending_submissions() {
            self.deferred_processing = true;
        }
    }

    fn maybe_process_deferred(&mut self, mem: &mut dyn MemoryBus, command: u16) {
        if !self.deferred_processing {
            return;
        }
        if (command & (1 << 2)) == 0 {
            return;
        }
        self.controller.process_all_queues(mem);
        self.deferred_processing = self.controller.has_pending_submissions();
    }
}

impl From<DiskError> for NvmeStatus {
    fn from(_: DiskError) -> Self {
        NvmeStatus::invalid_field()
    }
}

impl From<PrpError> for NvmeStatus {
    fn from(_: PrpError) -> Self {
        NvmeStatus::invalid_field()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::pci::{MmioDevice, PciDevice};
    use crate::io::storage::disk::MemDisk;
    use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
    use aero_storage_adapters::AeroVirtualDiskAsNvmeBackend;

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
            let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
            let end = start.checked_add(len).expect("address wrap");
            assert!(end <= self.data.len(), "out-of-bounds physical access");
            start..end
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let range = self.range(paddr, buf.len());
            buf.copy_from_slice(&self.data[range]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let range = self.range(paddr, buf.len());
            self.data[range].copy_from_slice(buf);
        }
    }

    fn write_cmd(mem: &mut VecMemory, addr: u64, dwords: [u32; 16]) {
        for (idx, dw) in dwords.iter().enumerate() {
            mem.write_u32(addr + (idx as u64) * 4, *dw);
        }
    }

    fn read_cqe(mem: &mut VecMemory, cq_base: u64, index: u16) -> (u32, u32, u32, u32) {
        let addr = cq_base + (index as u64) * 16;
        (
            mem.read_u32(addr),
            mem.read_u32(addr + 4),
            mem.read_u32(addr + 8),
            mem.read_u32(addr + 12),
        )
    }

    fn setup_controller(mem: &mut VecMemory) -> NvmeController {
        let disk = RawDisk::create(MemBackend::new(), 20_000u64 * SECTOR_SIZE as u64).unwrap();
        let disk = Box::new(AeroVirtualDiskAsNvmeBackend::new(Box::new(disk)));
        let mut ctrl = NvmeController::new(disk);

        ctrl.mmio_write_u32(mem, NVME_REG_AQA, 0x0003_0003);
        ctrl.mmio_write_u32(mem, NVME_REG_ASQ, 0x10_000);
        ctrl.mmio_write_u32(mem, NVME_REG_ASQ_HI, 0);
        ctrl.mmio_write_u32(mem, NVME_REG_ACQ, 0x20_000);
        ctrl.mmio_write_u32(mem, NVME_REG_ACQ_HI, 0);
        ctrl.mmio_write_u32(mem, NVME_REG_CC, (6 << 16) | (4 << 20) | CC_EN);
        ctrl
    }

    #[test]
    fn pci_wrapper_gates_bar0_mmio_on_pci_command_mem_bit() {
        let disk = Box::new(MemDisk::new(16));
        let ctrl = NvmeController::new(disk);
        let mut dev = NvmePciDevice::new(ctrl, 0xfebf_0000);
        let mut mem = VecMemory::new(0x10000);

        // With COMMAND.MEM clear, reads should float high and writes should be ignored.
        assert_eq!(dev.mmio_read(&mut mem, NVME_REG_CAP, 4), u32::MAX);
        dev.mmio_write(&mut mem, NVME_REG_CC, 4, CC_EN);

        // Enable MMIO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 1);
        assert_ne!(dev.mmio_read(&mut mem, NVME_REG_CAP, 4), u32::MAX);
        assert_eq!(dev.mmio_read(&mut mem, NVME_REG_CC, 4) & CC_EN, 0);
    }

    #[test]
    fn pci_wrapper_gates_nvme_dma_on_pci_command_bme_bit() {
        let disk = Box::new(MemDisk::new(16));
        let ctrl = NvmeController::new(disk);
        let mut dev = NvmePciDevice::new(ctrl, 0xfebf_0000);
        let mut mem = VecMemory::new(4 * 1024 * 1024);

        // Enable MMIO decoding but leave bus mastering disabled.
        dev.config_write(0x04, 2, 1 << 1);

        dev.mmio_write(&mut mem, NVME_REG_AQA, 4, 0x0003_0003);
        dev.mmio_write(&mut mem, NVME_REG_ASQ, 4, 0x10_000);
        dev.mmio_write(&mut mem, NVME_REG_ASQ_HI, 4, 0);
        dev.mmio_write(&mut mem, NVME_REG_ACQ, 4, 0x20_000);
        dev.mmio_write(&mut mem, NVME_REG_ACQ_HI, 4, 0);
        dev.mmio_write(&mut mem, NVME_REG_CC, 4, (6 << 16) | (4 << 20) | CC_EN);

        let identify_buf = 0x30_000u64;
        for i in 0..4096u64 {
            mem.write_u8(identify_buf + i, 0xaa);
        }
        for i in 0..16u64 {
            mem.write_u8(0x20_000 + i, 0xee);
        }

        let cid = 0x1234u16;
        let cmd_addr = 0x10_000u64;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
        cmd[6] = identify_buf as u32;
        cmd[7] = (identify_buf >> 32) as u32;
        cmd[10] = 1;
        write_cmd(&mut mem, cmd_addr, cmd);

        // Ring SQ0 tail; with BME disabled the command must not DMA or complete.
        dev.mmio_write(&mut mem, NVME_DOORBELL_BASE, 4, 1);

        assert_eq!(mem.read_u16(identify_buf), 0xaaaa);
        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        assert_eq!(dw3, 0xeeee_eeee);
        assert!(!dev.controller.irq_level());

        // Enable bus mastering; the pending command should complete on the next MMIO access.
        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
        let _ = dev.mmio_read(&mut mem, NVME_REG_CSTS, 4);

        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        assert_eq!((dw3 & 0xffff) as u16, cid);
        let status = (dw3 >> 16) as u16;
        assert_eq!(status & 1, 1);
        assert_eq!((status >> 1) & 0xff, 0);

        let vid = mem.read_u16(identify_buf);
        assert_eq!(vid, 0x1b36);
        assert!(dev.controller.irq_level());
    }

    #[test]
    fn pci_wrapper_gates_nvme_intx_on_pci_command_intx_disable_bit() {
        let disk = Box::new(MemDisk::new(16));
        let ctrl = NvmeController::new(disk);
        let mut dev = NvmePciDevice::new(ctrl, 0xfebf_0000);
        let mut mem = VecMemory::new(4 * 1024 * 1024);

        // Enable MMIO decode + bus mastering so the admin command executes and asserts the IRQ.
        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));

        dev.mmio_write(&mut mem, NVME_REG_AQA, 4, 0x0003_0003);
        dev.mmio_write(&mut mem, NVME_REG_ASQ, 4, 0x10_000);
        dev.mmio_write(&mut mem, NVME_REG_ASQ_HI, 4, 0);
        dev.mmio_write(&mut mem, NVME_REG_ACQ, 4, 0x20_000);
        dev.mmio_write(&mut mem, NVME_REG_ACQ_HI, 4, 0);
        dev.mmio_write(&mut mem, NVME_REG_CC, 4, (6 << 16) | (4 << 20) | CC_EN);

        let identify_buf = 0x30_000u64;
        for i in 0..4096u64 {
            mem.write_u8(identify_buf + i, 0xaa);
        }

        let cid = 0x1234u16;
        let cmd_addr = 0x10_000u64;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
        cmd[6] = identify_buf as u32;
        cmd[7] = (identify_buf >> 32) as u32;
        cmd[10] = 1;
        write_cmd(&mut mem, cmd_addr, cmd);

        // Ring SQ0 tail; the command should complete and raise an IRQ.
        dev.mmio_write(&mut mem, NVME_DOORBELL_BASE, 4, 1);

        assert!(dev.controller.irq_level(), "device model asserts IRQ line");
        assert!(dev.irq_level(), "wrapper forwards IRQ when INTX is enabled");

        dev.config_write(0x04, 2, (1 << 1) | (1 << 2) | (1 << 10));
        assert!(
            dev.controller.irq_level(),
            "device model retains pending interrupt"
        );
        assert!(
            !dev.irq_level(),
            "wrapper must suppress IRQ when INTX is disabled"
        );

        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
        assert!(dev.irq_level());
    }

    #[test]
    fn admin_identify_controller() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let identify_buf = 0x30_000u64;
        let cid = 0x1234u16;
        let cmd_addr = 0x10_000u64;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
        cmd[6] = identify_buf as u32;
        cmd[7] = (identify_buf >> 32) as u32;
        cmd[10] = 1;
        write_cmd(&mut mem, cmd_addr, cmd);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);

        let (_dw0, _dw1, _dw2, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        assert_eq!((dw3 & 0xffff) as u16, cid);
        let status = (dw3 >> 16) as u16;
        assert_eq!(status & 1, 1);
        assert_eq!((status >> 1) & 0xff, 0);

        let vid = mem.read_u16(identify_buf);
        assert_eq!(vid, 0x1b36);
    }

    fn admin_create_io_queues(
        ctrl: &mut NvmeController,
        mem: &mut VecMemory,
        io_sq: u64,
        io_cq: u64,
    ) {
        let mut cid = 1u16;

        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_CREATE_IO_CQ as u32 | ((cid as u32) << 16);
        cmd[6] = io_cq as u32;
        cmd[7] = (io_cq >> 32) as u32;
        cmd[10] = 1 | ((3u32) << 16);
        write_cmd(mem, 0x10_000, cmd);
        ctrl.mmio_write_u32(mem, NVME_DOORBELL_BASE, 1);
        ctrl.mmio_write_u32(mem, NVME_DOORBELL_BASE + 4, 1);

        cid += 1;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_CREATE_IO_SQ as u32 | ((cid as u32) << 16);
        cmd[6] = io_sq as u32;
        cmd[7] = (io_sq >> 32) as u32;
        cmd[10] = 1 | ((3u32) << 16);
        cmd[11] = 1;
        write_cmd(mem, 0x10_000 + 64, cmd);
        ctrl.mmio_write_u32(mem, NVME_DOORBELL_BASE, 2);
        ctrl.mmio_write_u32(mem, NVME_DOORBELL_BASE + 4, 2);
    }

    #[test]
    fn create_io_queue_count_is_capped() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let cq_base = 0x40_000u64;
        let sq_base = 0x50_000u64;

        let cq1 = NvmeCommand {
            opc: OPC_ADMIN_CREATE_IO_CQ,
            cid: 0x100,
            nsid: 0,
            prp1: cq_base,
            prp2: 0,
            cdw10: 1 | ((3u32) << 16), // qid=1, qsize=4
            cdw11: 0,
            cdw12: 0,
        };
        let (_dw0, st) = ctrl.cmd_create_io_cq(cq1);
        assert_eq!(st, NvmeStatus::success());

        let cq2 = NvmeCommand {
            cdw10: 2 | ((3u32) << 16), // qid=2, qsize=4
            prp1: cq_base + 0x1000,
            ..cq1
        };
        let (_dw0, st) = ctrl.cmd_create_io_cq(cq2);
        assert_eq!(st, NvmeStatus::invalid_field());
        assert_eq!(ctrl.io_cqs.len(), 1);

        let sq1 = NvmeCommand {
            opc: OPC_ADMIN_CREATE_IO_SQ,
            cid: 0x101,
            nsid: 0,
            prp1: sq_base,
            prp2: 0,
            cdw10: 1 | ((3u32) << 16), // qid=1, qsize=4
            cdw11: 1,                  // cqid=1
            cdw12: 0,
        };
        let (_dw0, st) = ctrl.cmd_create_io_sq(sq1);
        assert_eq!(st, NvmeStatus::success());

        let sq2 = NvmeCommand {
            cdw10: 2 | ((3u32) << 16), // qid=2, qsize=4
            prp1: sq_base + 0x1000,
            ..sq1
        };
        let (_dw0, st) = ctrl.cmd_create_io_sq(sq2);
        assert_eq!(st, NvmeStatus::invalid_field());
        assert_eq!(ctrl.io_sqs.len(), 1);
    }

    #[test]
    fn io_write_read_with_chained_prp_lists() {
        let mut mem = VecMemory::new(32 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let io_sq_base = 0x40_000u64;
        let io_cq_base = 0x50_000u64;
        admin_create_io_queues(&mut ctrl, &mut mem, io_sq_base, io_cq_base);

        let page_size = 4096u64;
        let total_pages = 514u64;
        let data_len = (total_pages * page_size) as usize;
        let sectors = data_len / 512;
        let slba = 100u64;

        let data_base = 0x100_000u64;
        for i in 0..data_len {
            mem.write_u8(data_base + i as u64, (i as u8).wrapping_add(0x5a));
        }

        let prp_list1 = data_base + total_pages * page_size;
        let prp_list2 = prp_list1 + page_size;
        for idx in 0..511u64 {
            let addr = data_base + (idx + 1) * page_size;
            mem.write_u64(prp_list1 + idx * 8, addr);
        }
        mem.write_u64(prp_list1 + 511 * 8, prp_list2);
        mem.write_u64(prp_list2, data_base + 512 * page_size);
        mem.write_u64(prp_list2 + 8, data_base + 513 * page_size);

        let cid = 0x9000u16;
        let mut write_cmd_dw = [0u32; 16];
        write_cmd_dw[0] = OPC_NVM_WRITE as u32 | ((cid as u32) << 16);
        write_cmd_dw[1] = 1;
        write_cmd_dw[6] = data_base as u32;
        write_cmd_dw[7] = (data_base >> 32) as u32;
        write_cmd_dw[8] = prp_list1 as u32;
        write_cmd_dw[9] = (prp_list1 >> 32) as u32;
        write_cmd_dw[10] = slba as u32;
        write_cmd_dw[11] = (slba >> 32) as u32;
        write_cmd_dw[12] = (sectors as u32) - 1;
        write_cmd(&mut mem, io_sq_base, write_cmd_dw);
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 8, 1);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 12, 1);
        let (_dw0, _dw1, _dw2, dw3) = read_cqe(&mut mem, io_cq_base, 0);
        assert_eq!((dw3 & 0xffff) as u16, cid);
        let status = (dw3 >> 16) as u16;
        assert_eq!((status >> 1) & 0xff, 0);

        let read_base = prp_list2 + page_size;
        mem.write_physical(read_base, &vec![0u8; data_len]);

        let read_list1 = read_base + total_pages * page_size;
        let read_list2 = read_list1 + page_size;
        for idx in 0..511u64 {
            let addr = read_base + (idx + 1) * page_size;
            mem.write_u64(read_list1 + idx * 8, addr);
        }
        mem.write_u64(read_list1 + 511 * 8, read_list2);
        mem.write_u64(read_list2, read_base + 512 * page_size);
        mem.write_u64(read_list2 + 8, read_base + 513 * page_size);

        let cid2 = 0x9001u16;
        let mut read_cmd_dw = [0u32; 16];
        read_cmd_dw[0] = OPC_NVM_READ as u32 | ((cid2 as u32) << 16);
        read_cmd_dw[1] = 1;
        read_cmd_dw[6] = read_base as u32;
        read_cmd_dw[7] = (read_base >> 32) as u32;
        read_cmd_dw[8] = read_list1 as u32;
        read_cmd_dw[9] = (read_list1 >> 32) as u32;
        read_cmd_dw[10] = slba as u32;
        read_cmd_dw[11] = (slba >> 32) as u32;
        read_cmd_dw[12] = (sectors as u32) - 1;
        write_cmd(&mut mem, io_sq_base + 64, read_cmd_dw);
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 8, 2);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 12, 2);
        let (_dw0, _dw1, _dw2, dw3) = read_cqe(&mut mem, io_cq_base, 1);
        assert_eq!((dw3 & 0xffff) as u16, cid2);
        let status = (dw3 >> 16) as u16;
        assert_eq!((status >> 1) & 0xff, 0);

        for i in 0..data_len {
            let expected = (i as u8).wrapping_add(0x5a);
            let actual = mem.read_u8(read_base + i as u64);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn completion_phase_wraparound() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let disk = RawDisk::create(MemBackend::new(), 10_000u64 * SECTOR_SIZE as u64).unwrap();
        let disk = Box::new(AeroVirtualDiskAsNvmeBackend::new(Box::new(disk)));
        let mut ctrl = NvmeController::new(disk);

        ctrl.mmio_write_u32(&mut mem, NVME_REG_AQA, 0x0001_0003);
        ctrl.mmio_write_u32(&mut mem, NVME_REG_ASQ, 0x10_000);
        ctrl.mmio_write_u32(&mut mem, NVME_REG_ACQ, 0x20_000);
        ctrl.mmio_write_u32(&mut mem, NVME_REG_CC, CC_EN);

        let identify_buf = 0x30_000u64;
        for (idx, cid) in [0x10u16, 0x11u16, 0x12u16].into_iter().enumerate() {
            let mut cmd = [0u32; 16];
            cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
            cmd[6] = identify_buf as u32;
            cmd[7] = (identify_buf >> 32) as u32;
            cmd[10] = 1;
            write_cmd(&mut mem, 0x10_000 + (idx as u64) * 64, cmd);
        }

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 3);

        let (_, _, _, dw3a) = read_cqe(&mut mem, 0x20_000, 0);
        let status_a = (dw3a >> 16) as u16;
        assert_eq!(status_a & 1, 1);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 4, 1);
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 3);

        let (_, _, _, dw3b) = read_cqe(&mut mem, 0x20_000, 0);
        let status_b = (dw3b >> 16) as u16;
        assert_eq!(status_b & 1, 0);
    }

    #[test]
    fn doorbell_reads_reflect_queue_pointers() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let identify_buf = 0x30_000u64;
        let cid = 0x2222u16;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
        cmd[6] = identify_buf as u32;
        cmd[7] = (identify_buf >> 32) as u32;
        cmd[10] = 1;
        write_cmd(&mut mem, 0x10_000, cmd);

        // Post the command via SQ0 tail doorbell and verify SQ/CQ doorbell reads.
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);
        assert_eq!(ctrl.mmio_read_u32(&mut mem, NVME_DOORBELL_BASE), 1);
        assert_eq!(ctrl.mmio_read_u32(&mut mem, NVME_DOORBELL_BASE + 4), 0);

        // Consume CQE0 and verify head updates are reflected.
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE + 4, 1);
        assert_eq!(ctrl.mmio_read_u32(&mut mem, NVME_DOORBELL_BASE + 4), 1);
    }

    #[test]
    fn set_get_features_roundtrip_for_unknown_fid() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let fid = 0x06u8; // Volatile Write Cache (we store but don't otherwise model).

        let cid_set = 0x3000u16;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_SET_FEATURES as u32 | ((cid_set as u32) << 16);
        cmd[10] = fid as u32;
        cmd[11] = 0x1234_5678;
        write_cmd(&mut mem, 0x10_000, cmd);
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);

        let (dw0, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        assert_eq!(dw0, 0x1234_5678);
        assert_eq!((dw3 & 0xffff) as u16, cid_set);

        let cid_get = 0x3001u16;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_GET_FEATURES as u32 | ((cid_get as u32) << 16);
        cmd[10] = fid as u32;
        write_cmd(&mut mem, 0x10_000 + 64, cmd);
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 2);

        let (dw0, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 1);
        assert_eq!(dw0, 0x1234_5678);
        assert_eq!((dw3 & 0xffff) as u16, cid_get);
    }

    #[test]
    fn admin_identify_active_namespace_list() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let buf = 0x30_000u64;
        // Pre-fill so the test catches a missing DMA write.
        for i in 0..4096u64 {
            mem.write_u8(buf + i, 0xaa);
        }

        let cid = 0x4000u16;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid as u32) << 16);
        cmd[1] = 0; // start NSID=0 means start from 1
        cmd[6] = buf as u32;
        cmd[7] = (buf >> 32) as u32;
        cmd[10] = 2; // CNS=2 (active namespace list)
        write_cmd(&mut mem, 0x10_000, cmd);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);

        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        let status = (dw3 >> 16) as u16;
        assert_eq!((status >> 1) & 0xff, 0);

        assert_eq!(mem.read_u32(buf), 1);
        assert_eq!(mem.read_u32(buf + 4), 0);
    }

    #[test]
    fn admin_get_log_page_writes_zeros() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let buf = 0x30_000u64;
        let len = 512usize;
        for i in 0..len {
            mem.write_u8(buf + i as u64, 0xcc);
        }

        let cid = 0x4001u16;
        let numd = (len / 4).saturating_sub(1) as u32;
        let mut cmd = [0u32; 16];
        cmd[0] = OPC_ADMIN_GET_LOG_PAGE as u32 | ((cid as u32) << 16);
        cmd[6] = buf as u32;
        cmd[7] = (buf >> 32) as u32;
        // LID in bits 7:0, NUMD in bits 31:16.
        cmd[10] = 2u32 | (numd << 16);
        cmd[11] = 0;
        write_cmd(&mut mem, 0x10_000, cmd);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);

        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        let status = (dw3 >> 16) as u16;
        assert_eq!((status >> 1) & 0xff, 0);

        for i in 0..len {
            assert_eq!(mem.read_u8(buf + i as u64), 0);
        }
    }

    #[test]
    fn async_event_request_is_deferred_without_completion() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        // Pre-fill CQ so we can detect any writes.
        for i in 0..(4 * 16) {
            mem.write_u8(0x20_000 + i as u64, 0xee);
        }

        let identify_buf = 0x30_000u64;

        let cid_aer = 0x5000u16;
        let mut aer_cmd = [0u32; 16];
        aer_cmd[0] = OPC_ADMIN_ASYNC_EVENT_REQUEST as u32 | ((cid_aer as u32) << 16);
        write_cmd(&mut mem, 0x10_000, aer_cmd);

        let cid_ident = 0x5001u16;
        let mut ident_cmd = [0u32; 16];
        ident_cmd[0] = OPC_ADMIN_IDENTIFY as u32 | ((cid_ident as u32) << 16);
        ident_cmd[6] = identify_buf as u32;
        ident_cmd[7] = (identify_buf >> 32) as u32;
        ident_cmd[10] = 1;
        write_cmd(&mut mem, 0x10_000 + 64, ident_cmd);

        // Ring SQ0 tail = 2; only Identify should complete.
        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 2);

        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        assert_eq!((dw3 & 0xffff) as u16, cid_ident);
        let status = (dw3 >> 16) as u16;
        assert_eq!((status >> 1) & 0xff, 0);
        assert!(ctrl.irq_level());

        // CQE[1] must be untouched (AER did not complete).
        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 1);
        assert_eq!(dw3, 0xeeee_eeee);
    }

    #[test]
    fn invalid_opcode_sets_dnr_bit() {
        let mut mem = VecMemory::new(4 * 1024 * 1024);
        let mut ctrl = setup_controller(&mut mem);

        let cid = 0x6000u16;
        let mut cmd = [0u32; 16];
        cmd[0] = 0xffu32 | ((cid as u32) << 16); // invalid admin opcode
        write_cmd(&mut mem, 0x10_000, cmd);

        ctrl.mmio_write_u32(&mut mem, NVME_DOORBELL_BASE, 1);

        let (_, _, _, dw3) = read_cqe(&mut mem, 0x20_000, 0);
        let status = (dw3 >> 16) as u16;
        // DNR is bit 15 of the status field.
        assert_ne!(status & (1 << 15), 0);
        // SC=1 (invalid opcode).
        assert_eq!((status >> 1) & 0xff, 1);
    }
}
