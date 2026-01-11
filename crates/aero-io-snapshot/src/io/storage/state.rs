use std::collections::{BTreeMap, BTreeSet};

use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};

// ----------------------------------------
// Disk layer state (host-side)
// ----------------------------------------

pub trait DiskBackend {
    fn read_at(&self, offset: u64, buf: &mut [u8]);
    fn write_at(&mut self, offset: u64, data: &[u8]);
    fn flush(&mut self);
}

/// Host-side disk state that can be snapshotted independently of the underlying backing store.
///
/// The actual disk contents are assumed to live in an external backend (OPFS/IndexedDB/etc).
/// Dirty write-back state is flushed before snapshot at the coordinator layer.
pub struct DiskLayerState {
    pub backend_key: String,
    pub sector_size: usize,
    pub size_bytes: u64,

    // Optional read cache (hot sectors).
    pub read_cache: BTreeMap<u64, Vec<u8>>,

    // Buffered writes (dirty sectors).
    pub write_cache: BTreeMap<u64, Vec<u8>>,
    pub dirty_sectors: BTreeSet<u64>,
    pub flush_in_progress: bool,

    backend: Option<Box<dyn DiskBackend>>,
}

impl std::fmt::Debug for DiskLayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskLayerState")
            .field("backend_key", &self.backend_key)
            .field("sector_size", &self.sector_size)
            .field("size_bytes", &self.size_bytes)
            .field("read_cache", &self.read_cache)
            .field("write_cache", &self.write_cache)
            .field("dirty_sectors", &self.dirty_sectors)
            .field("flush_in_progress", &self.flush_in_progress)
            .field("backend_attached", &self.backend.is_some())
            .finish()
    }
}

impl PartialEq for DiskLayerState {
    fn eq(&self, other: &Self) -> bool {
        self.backend_key == other.backend_key
            && self.sector_size == other.sector_size
            && self.size_bytes == other.size_bytes
            && self.read_cache == other.read_cache
            && self.write_cache == other.write_cache
            && self.dirty_sectors == other.dirty_sectors
            && self.flush_in_progress == other.flush_in_progress
    }
}

impl Eq for DiskLayerState {}

impl DiskLayerState {
    pub fn new(backend_key: impl Into<String>, size_bytes: u64, sector_size: usize) -> Self {
        Self {
            backend_key: backend_key.into(),
            sector_size,
            size_bytes,
            read_cache: BTreeMap::new(),
            write_cache: BTreeMap::new(),
            dirty_sectors: BTreeSet::new(),
            flush_in_progress: false,
            backend: None,
        }
    }

    pub fn attach_backend(&mut self, backend: Box<dyn DiskBackend>) {
        self.backend = Some(backend);
    }

    pub fn read_sector(&mut self, lba: u64) -> Vec<u8> {
        if let Some(data) = self.write_cache.get(&lba) {
            return data.clone();
        }
        if let Some(data) = self.read_cache.get(&lba) {
            return data.clone();
        }

        let mut out = vec![0u8; self.sector_size];
        if let Some(backend) = &self.backend {
            backend.read_at(lba * self.sector_size as u64, &mut out);
        }
        self.read_cache.insert(lba, out.clone());
        out
    }

    pub fn write_sector(&mut self, lba: u64, data: &[u8]) {
        assert_eq!(data.len(), self.sector_size);
        self.write_cache.insert(lba, data.to_vec());
        self.dirty_sectors.insert(lba);
    }

    pub fn flush(&mut self) {
        let Some(backend) = self.backend.as_mut() else {
            self.write_cache.clear();
            self.dirty_sectors.clear();
            return;
        };

        self.flush_in_progress = true;
        for lba in self.dirty_sectors.iter().copied().collect::<Vec<_>>() {
            if let Some(data) = self.write_cache.get(&lba) {
                backend.write_at(lba * self.sector_size as u64, data);
            }
        }
        backend.flush();

        self.write_cache.clear();
        self.dirty_sectors.clear();
        self.flush_in_progress = false;
    }
}

impl IoSnapshot for DiskLayerState {
    const DEVICE_ID: [u8; 4] = *b"DSK0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_BACKEND_KEY: u16 = 1;
        const TAG_SECTOR_SIZE: u16 = 2;
        const TAG_SIZE_BYTES: u16 = 3;
        const TAG_READ_CACHE: u16 = 4;
        const TAG_WRITE_CACHE: u16 = 5;
        const TAG_DIRTY_SECTORS: u16 = 6;
        const TAG_FLUSH_IN_PROGRESS: u16 = 7;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_BACKEND_KEY, self.backend_key.as_bytes().to_vec());
        w.field_u32(TAG_SECTOR_SIZE, self.sector_size as u32);
        w.field_u64(TAG_SIZE_BYTES, self.size_bytes);
        w.field_bool(TAG_FLUSH_IN_PROGRESS, self.flush_in_progress);

        let mut read_entries = Encoder::new().u32(self.read_cache.len() as u32);
        for (lba, data) in &self.read_cache {
            read_entries = read_entries.u64(*lba).u32(data.len() as u32).bytes(data);
        }
        w.field_bytes(TAG_READ_CACHE, read_entries.finish());

        let mut write_entries = Encoder::new().u32(self.write_cache.len() as u32);
        for (lba, data) in &self.write_cache {
            write_entries = write_entries.u64(*lba).u32(data.len() as u32).bytes(data);
        }
        w.field_bytes(TAG_WRITE_CACHE, write_entries.finish());

        let mut dirty = Encoder::new().u32(self.dirty_sectors.len() as u32);
        for lba in &self.dirty_sectors {
            dirty = dirty.u64(*lba);
        }
        w.field_bytes(TAG_DIRTY_SECTORS, dirty.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_BACKEND_KEY: u16 = 1;
        const TAG_SECTOR_SIZE: u16 = 2;
        const TAG_SIZE_BYTES: u16 = 3;
        const TAG_READ_CACHE: u16 = 4;
        const TAG_WRITE_CACHE: u16 = 5;
        const TAG_DIRTY_SECTORS: u16 = 6;
        const TAG_FLUSH_IN_PROGRESS: u16 = 7;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(key) = r.bytes(TAG_BACKEND_KEY) {
            self.backend_key = String::from_utf8(key.to_vec())
                .map_err(|_| SnapshotError::InvalidFieldEncoding("backend_key utf8"))?;
        }
        if let Some(sector) = r.u32(TAG_SECTOR_SIZE)? {
            self.sector_size = sector as usize;
        }
        if let Some(size) = r.u64(TAG_SIZE_BYTES)? {
            self.size_bytes = size;
        }
        if let Some(v) = r.bool(TAG_FLUSH_IN_PROGRESS)? {
            self.flush_in_progress = v;
        }

        self.read_cache.clear();
        if let Some(buf) = r.bytes(TAG_READ_CACHE) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            for _ in 0..count {
                let lba = d.u64()?;
                let len = d.u32()? as usize;
                let data = d.bytes(len)?.to_vec();
                self.read_cache.insert(lba, data);
            }
            d.finish()?;
        }

        self.write_cache.clear();
        if let Some(buf) = r.bytes(TAG_WRITE_CACHE) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            for _ in 0..count {
                let lba = d.u64()?;
                let len = d.u32()? as usize;
                let data = d.bytes(len)?.to_vec();
                self.write_cache.insert(lba, data);
            }
            d.finish()?;
        }

        self.dirty_sectors.clear();
        if let Some(buf) = r.bytes(TAG_DIRTY_SECTORS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            for _ in 0..count {
                self.dirty_sectors.insert(d.u64()?);
            }
            d.finish()?;
        }

        self.backend = None;
        Ok(())
    }
}

// ----------------------------------------
// IDE controller (placeholder state)
// ----------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeInFlightCommandState {
    pub lba: u32,
    pub sector_count: u16,
    pub is_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeControllerState {
    pub command: u8,
    pub status: u8,
    pub error: u8,
    pub sector_count: u16,
    pub lba: u32,
    pub dma_active: bool,
    pub in_flight: Option<IdeInFlightCommandState>,
}

impl Default for IdeControllerState {
    fn default() -> Self {
        Self {
            command: 0,
            status: 0,
            error: 0,
            sector_count: 0,
            lba: 0,
            dma_active: false,
            in_flight: None,
        }
    }
}

impl IoSnapshot for IdeControllerState {
    const DEVICE_ID: [u8; 4] = *b"IDE0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_COMMAND: u16 = 1;
        const TAG_STATUS: u16 = 2;
        const TAG_ERROR: u16 = 3;
        const TAG_SECTOR_COUNT: u16 = 4;
        const TAG_LBA: u16 = 5;
        const TAG_DMA_ACTIVE: u16 = 6;
        const TAG_IN_FLIGHT: u16 = 7;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u8(TAG_COMMAND, self.command);
        w.field_u8(TAG_STATUS, self.status);
        w.field_u8(TAG_ERROR, self.error);
        w.field_u16(TAG_SECTOR_COUNT, self.sector_count);
        w.field_u32(TAG_LBA, self.lba);
        w.field_bool(TAG_DMA_ACTIVE, self.dma_active);

        if let Some(cmd) = &self.in_flight {
            let bytes = Encoder::new()
                .u32(cmd.lba)
                .u16(cmd.sector_count)
                .bool(cmd.is_write)
                .finish();
            w.field_bytes(TAG_IN_FLIGHT, bytes);
        }
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_COMMAND: u16 = 1;
        const TAG_STATUS: u16 = 2;
        const TAG_ERROR: u16 = 3;
        const TAG_SECTOR_COUNT: u16 = 4;
        const TAG_LBA: u16 = 5;
        const TAG_DMA_ACTIVE: u16 = 6;
        const TAG_IN_FLIGHT: u16 = 7;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.command = r.u8(TAG_COMMAND)?.unwrap_or(0);
        self.status = r.u8(TAG_STATUS)?.unwrap_or(0);
        self.error = r.u8(TAG_ERROR)?.unwrap_or(0);
        self.sector_count = r.u16(TAG_SECTOR_COUNT)?.unwrap_or(0);
        self.lba = r.u32(TAG_LBA)?.unwrap_or(0);
        self.dma_active = r.bool(TAG_DMA_ACTIVE)?.unwrap_or(false);

        self.in_flight = if let Some(buf) = r.bytes(TAG_IN_FLIGHT) {
            let mut d = Decoder::new(buf);
            let lba = d.u32()?;
            let sector_count = d.u16()?;
            let is_write = d.bool()?;
            d.finish()?;
            Some(IdeInFlightCommandState {
                lba,
                sector_count,
                is_write,
            })
        } else {
            None
        };
        Ok(())
    }
}

// ----------------------------------------
// NVMe controller (placeholder state)
// ----------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmeSubmissionQueueState {
    pub qid: u16,
    pub base: u64,
    pub size: u16,
    pub head: u16,
    pub tail: u16,
    pub cqid: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmeCompletionQueueState {
    pub qid: u16,
    pub base: u64,
    pub size: u16,
    pub head: u16,
    pub tail: u16,
    pub phase: bool,
    pub irq_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmeInFlightCommandState {
    pub cid: u16,
    pub opcode: u8,
    pub lba: u64,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmeControllerState {
    pub cap: u64,
    pub vs: u32,
    pub intms: u32,
    pub intmc: u32,
    pub cc: u32,
    pub csts: u32,
    pub aqa: u32,
    pub asq: u64,
    pub acq: u64,
    pub admin_sq: Option<NvmeSubmissionQueueState>,
    pub admin_cq: Option<NvmeCompletionQueueState>,
    pub io_sqs: Vec<NvmeSubmissionQueueState>,
    pub io_cqs: Vec<NvmeCompletionQueueState>,
    pub intx_level: bool,
    pub in_flight: Vec<NvmeInFlightCommandState>,
}

impl Default for NvmeControllerState {
    fn default() -> Self {
        Self {
            cap: 0,
            vs: 0,
            intms: 0,
            intmc: 0,
            cc: 0,
            csts: 0,
            aqa: 0,
            asq: 0,
            acq: 0,
            admin_sq: None,
            admin_cq: None,
            io_sqs: Vec::new(),
            io_cqs: Vec::new(),
            intx_level: false,
            in_flight: Vec::new(),
        }
    }
}

impl IoSnapshot for NvmeControllerState {
    const DEVICE_ID: [u8; 4] = *b"NVME";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_REGS: u16 = 1;
        // Legacy queue tags kept for forward compatibility (NVME 1.0).
        const TAG_ADMIN_QUEUES: u16 = 2;
        const TAG_IO_QUEUES: u16 = 3;
        const TAG_IN_FLIGHT: u16 = 4;
        // Extended queue state for deterministic resume (NVME 1.1+).
        const TAG_ADMIN_SQ: u16 = 5;
        const TAG_ADMIN_CQ: u16 = 6;
        const TAG_IO_SQS: u16 = 7;
        const TAG_IO_CQS: u16 = 8;
        const TAG_INTX_LEVEL: u16 = 9;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        let regs = Encoder::new()
            .u64(self.cap)
            .u32(self.vs)
            .u32(self.intms)
            .u32(self.intmc)
            .u32(self.cc)
            .u32(self.csts)
            .u32(self.aqa)
            .u64(self.asq)
            .u64(self.acq)
            .finish();
        w.field_bytes(TAG_REGS, regs);

        // Old admin queue encoding: base/size/head/tail for SQ and CQ.
        if let (Some(sq), Some(cq)) = (self.admin_sq.as_ref(), self.admin_cq.as_ref()) {
            let admin = Encoder::new()
                .u64(sq.base)
                .u16(sq.size)
                .u16(sq.head)
                .u16(sq.tail)
                .u64(cq.base)
                .u16(cq.size)
                .u16(cq.head)
                .u16(cq.tail)
                .finish();
            w.field_bytes(TAG_ADMIN_QUEUES, admin);
        }

        if let Some(sq) = self.admin_sq.as_ref() {
            let admin_sq = Encoder::new()
                .u16(sq.qid)
                .u64(sq.base)
                .u16(sq.size)
                .u16(sq.head)
                .u16(sq.tail)
                .u16(sq.cqid)
                .finish();
            w.field_bytes(TAG_ADMIN_SQ, admin_sq);
        }

        if let Some(cq) = self.admin_cq.as_ref() {
            let admin_cq = Encoder::new()
                .u16(cq.qid)
                .u64(cq.base)
                .u16(cq.size)
                .u16(cq.head)
                .u16(cq.tail)
                .bool(cq.phase)
                .bool(cq.irq_enabled)
                .finish();
            w.field_bytes(TAG_ADMIN_CQ, admin_cq);
        }

        // Old IO queue encoding: ordered list of SQ/CQ pairs without explicit qids.
        // We preserve it by encoding SQs in ascending qid order and pairing each SQ
        // with its mapped CQ (cqid), if present.
        let mut sqs_sorted = self.io_sqs.clone();
        sqs_sorted.sort_by_key(|sq| sq.qid);
        let mut cq_by_qid: BTreeMap<u16, NvmeCompletionQueueState> = BTreeMap::new();
        for cq in &self.io_cqs {
            cq_by_qid.insert(cq.qid, cq.clone());
        }
        let mut ioqs = Encoder::new().u32(sqs_sorted.len() as u32);
        for sq in &sqs_sorted {
            let cq = cq_by_qid.get(&sq.cqid);
            ioqs = ioqs
                .u64(sq.base)
                .u16(sq.size)
                .u16(sq.head)
                .u16(sq.tail)
                .u64(cq.map_or(0, |cq| cq.base))
                .u16(cq.map_or(0, |cq| cq.size))
                .u16(cq.map_or(0, |cq| cq.head))
                .u16(cq.map_or(0, |cq| cq.tail));
        }
        w.field_bytes(TAG_IO_QUEUES, ioqs.finish());

        // Deterministic extended queue state (qid-sorted).
        let mut io_sqs = self.io_sqs.clone();
        io_sqs.sort_by_key(|sq| sq.qid);
        let mut io_sqs_enc = Encoder::new().u32(io_sqs.len() as u32);
        for sq in &io_sqs {
            io_sqs_enc = io_sqs_enc
                .u16(sq.qid)
                .u64(sq.base)
                .u16(sq.size)
                .u16(sq.head)
                .u16(sq.tail)
                .u16(sq.cqid);
        }
        w.field_bytes(TAG_IO_SQS, io_sqs_enc.finish());

        let mut io_cqs = self.io_cqs.clone();
        io_cqs.sort_by_key(|cq| cq.qid);
        let mut io_cqs_enc = Encoder::new().u32(io_cqs.len() as u32);
        for cq in &io_cqs {
            io_cqs_enc = io_cqs_enc
                .u16(cq.qid)
                .u64(cq.base)
                .u16(cq.size)
                .u16(cq.head)
                .u16(cq.tail)
                .bool(cq.phase)
                .bool(cq.irq_enabled);
        }
        w.field_bytes(TAG_IO_CQS, io_cqs_enc.finish());

        w.field_bool(TAG_INTX_LEVEL, self.intx_level);

        let mut inflight = Encoder::new().u32(self.in_flight.len() as u32);
        for cmd in &self.in_flight {
            inflight = inflight
                .u16(cmd.cid)
                .u8(cmd.opcode)
                .u64(cmd.lba)
                .u32(cmd.length);
        }
        w.field_bytes(TAG_IN_FLIGHT, inflight.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_REGS: u16 = 1;
        const TAG_ADMIN_QUEUES: u16 = 2;
        const TAG_IO_QUEUES: u16 = 3;
        const TAG_IN_FLIGHT: u16 = 4;
        const TAG_ADMIN_SQ: u16 = 5;
        const TAG_ADMIN_CQ: u16 = 6;
        const TAG_IO_SQS: u16 = 7;
        const TAG_IO_CQS: u16 = 8;
        const TAG_INTX_LEVEL: u16 = 9;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_REGS) {
            let mut d = Decoder::new(buf);
            self.cap = d.u64()?;
            self.vs = d.u32()?;
            self.intms = d.u32()?;
            self.intmc = d.u32()?;
            self.cc = d.u32()?;
            self.csts = d.u32()?;
            self.aqa = d.u32()?;
            self.asq = d.u64()?;
            self.acq = d.u64()?;
            d.finish()?;
        }

        // Reset queue state to a deterministic baseline before applying snapshot fields.
        self.admin_sq = None;
        self.admin_cq = None;
        self.io_sqs.clear();
        self.io_cqs.clear();

        // Legacy admin queue state (no cqid/phase/irq).
        if let Some(buf) = r.bytes(TAG_ADMIN_QUEUES) {
            let mut d = Decoder::new(buf);
            let sq_base = d.u64()?;
            let sq_size = d.u16()?;
            let sq_head = d.u16()?;
            let sq_tail = d.u16()?;
            let cq_base = d.u64()?;
            let cq_size = d.u16()?;
            let cq_head = d.u16()?;
            let cq_tail = d.u16()?;
            d.finish()?;

            self.admin_sq = Some(NvmeSubmissionQueueState {
                qid: 0,
                base: sq_base,
                size: sq_size,
                head: sq_head,
                tail: sq_tail,
                cqid: 0,
            });
            self.admin_cq = Some(NvmeCompletionQueueState {
                qid: 0,
                base: cq_base,
                size: cq_size,
                head: cq_head,
                tail: cq_tail,
                phase: true,
                irq_enabled: true,
            });
        }

        // Extended admin queue state.
        if let Some(buf) = r.bytes(TAG_ADMIN_SQ) {
            let mut d = Decoder::new(buf);
            let qid = d.u16()?;
            let base = d.u64()?;
            let size = d.u16()?;
            let head = d.u16()?;
            let tail = d.u16()?;
            let cqid = d.u16()?;
            d.finish()?;
            self.admin_sq = Some(NvmeSubmissionQueueState {
                qid,
                base,
                size,
                head,
                tail,
                cqid,
            });
        }

        if let Some(buf) = r.bytes(TAG_ADMIN_CQ) {
            let mut d = Decoder::new(buf);
            let qid = d.u16()?;
            let base = d.u64()?;
            let size = d.u16()?;
            let head = d.u16()?;
            let tail = d.u16()?;
            let phase = d.bool()?;
            let irq_enabled = d.bool()?;
            d.finish()?;
            self.admin_cq = Some(NvmeCompletionQueueState {
                qid,
                base,
                size,
                head,
                tail,
                phase,
                irq_enabled,
            });
        }

        // Legacy IO queues (no qid/cqid/phase/irq). We map them to qid=1..N.
        if let Some(buf) = r.bytes(TAG_IO_QUEUES) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            self.io_sqs.reserve(count);
            self.io_cqs.reserve(count);
            for idx in 0..count {
                let qid = idx as u16 + 1;
                let sq = NvmeSubmissionQueueState {
                    qid,
                    base: d.u64()?,
                    size: d.u16()?,
                    head: d.u16()?,
                    tail: d.u16()?,
                    cqid: qid,
                };
                let cq = NvmeCompletionQueueState {
                    qid,
                    base: d.u64()?,
                    size: d.u16()?,
                    head: d.u16()?,
                    tail: d.u16()?,
                    phase: true,
                    irq_enabled: true,
                };
                self.io_sqs.push(sq);
                self.io_cqs.push(cq);
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_IO_SQS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            self.io_sqs.clear();
            self.io_sqs.reserve(count);
            for _ in 0..count {
                self.io_sqs.push(NvmeSubmissionQueueState {
                    qid: d.u16()?,
                    base: d.u64()?,
                    size: d.u16()?,
                    head: d.u16()?,
                    tail: d.u16()?,
                    cqid: d.u16()?,
                });
            }
            d.finish()?;
            self.io_sqs.sort_by_key(|sq| sq.qid);
        }

        if let Some(buf) = r.bytes(TAG_IO_CQS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            self.io_cqs.clear();
            self.io_cqs.reserve(count);
            for _ in 0..count {
                self.io_cqs.push(NvmeCompletionQueueState {
                    qid: d.u16()?,
                    base: d.u64()?,
                    size: d.u16()?,
                    head: d.u16()?,
                    tail: d.u16()?,
                    phase: d.bool()?,
                    irq_enabled: d.bool()?,
                });
            }
            d.finish()?;
            self.io_cqs.sort_by_key(|cq| cq.qid);
        }

        self.intx_level = r.bool(TAG_INTX_LEVEL)?.unwrap_or(false);

        self.in_flight.clear();
        if let Some(buf) = r.bytes(TAG_IN_FLIGHT) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            self.in_flight.reserve(count);
            for _ in 0..count {
                self.in_flight.push(NvmeInFlightCommandState {
                    cid: d.u16()?,
                    opcode: d.u8()?,
                    lba: d.u64()?,
                    length: d.u32()?,
                });
            }
            d.finish()?;
        }

        Ok(())
    }
}
