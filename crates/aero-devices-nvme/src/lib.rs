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

use std::collections::{BTreeMap, HashMap};

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::{profile, PciBarDefinition, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_io_snapshot::io::storage::state::{
    NvmeCompletionQueueState, NvmeControllerState, NvmeSubmissionQueueState,
};
/// Adapter allowing [`aero_storage::VirtualDisk`] implementations (e.g. `RawDisk`,
/// `AeroSparseDisk`, `BlockCachedDisk`) to be used as an NVMe [`DiskBackend`].
///
/// NVMe is currently hard-coded to 512-byte sectors. The [`from_virtual_disk`] convenience
/// constructor rejects disks whose byte capacity is not a multiple of 512 (since capacity is
/// reported to the guest in whole LBAs).
pub use aero_storage_adapters::AeroVirtualDiskAsNvmeBackend as AeroStorageDiskAdapter;
use memory::{MemoryBus, MmioHandler};

mod aero_storage_adapter;
pub use aero_storage_adapter::NvmeDiskFromAeroStorage;

const PAGE_SIZE: usize = 4096;

/// Errors returned by disk backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskError {
    Io,
    OutOfRange {
        lba: u64,
        sectors: u64,
        capacity_sectors: u64,
    },
    UnalignedBuffer {
        len: usize,
        sector_size: u32,
    },
}

pub type DiskResult<T> = Result<T, DiskError>;

/// Block storage abstraction. The controller speaks in the disk's sector size (typically 512B).
pub trait DiskBackend: Send {
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()>;
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;
}

impl DiskBackend for AeroStorageDiskAdapter {
    fn sector_size(&self) -> u32 {
        AeroStorageDiskAdapter::SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        // If the disk capacity is not a multiple of 512, expose the largest full-sector span.
        self.capacity_bytes() / u64::from(Self::sector_size(self))
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()> {
        let sector_size = self.sector_size() as usize;
        if !buffer.len().is_multiple_of(sector_size) {
            return Err(DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size(),
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let capacity = self.total_sectors();
        if end_lba > capacity {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: capacity,
            });
        }

        let offset = lba
            .checked_mul(u64::from(self.sector_size()))
            .ok_or(DiskError::Io)?;
        // Any unexpected disk-layer error is surfaced as a generic I/O failure at the NVMe level
        // (should be rare after the range/alignment pre-checks above).
        self.disk_mut().read_at(offset, buffer).map_err(|_| DiskError::Io)
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()> {
        let sector_size = self.sector_size() as usize;
        if !buffer.len().is_multiple_of(sector_size) {
            return Err(DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size(),
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let capacity = self.total_sectors();
        if end_lba > capacity {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: capacity,
            });
        }

        let offset = lba
            .checked_mul(u64::from(self.sector_size()))
            .ok_or(DiskError::Io)?;
        self.disk_mut()
            .write_at(offset, buffer)
            .map_err(|_| DiskError::Io)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.disk_mut().flush().map_err(|_| DiskError::Io)
    }
}

/// Convenience helper to wrap an [`aero_storage::VirtualDisk`] as an NVMe [`DiskBackend`].
///
/// Returns `Err(DiskError::Io)` if the disk capacity is not a multiple of 512 bytes (NVMe LBAs are
/// currently fixed at 512 bytes in this device model).
pub fn from_virtual_disk(
    d: Box<dyn aero_storage::VirtualDisk + Send>,
) -> DiskResult<Box<dyn DiskBackend>> {
    // NVMe currently reports capacity in whole 512-byte LBAs.
    // Reject disks that cannot be represented losslessly.
    if !d.capacity_bytes().is_multiple_of(u64::from(AeroStorageDiskAdapter::SECTOR_SIZE)) {
        return Err(DiskError::Io);
    }
    Ok(Box::new(AeroStorageDiskAdapter::new(d)))
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
        sct: 1,
        sc: 0xb,
        dnr: true,
    };

    const INVALID_QID: NvmeStatus = NvmeStatus {
        sct: 0,
        sc: 0x1c,
        dnr: true,
    };

    const LBA_OUT_OF_RANGE: NvmeStatus = NvmeStatus {
        sct: 1,
        sc: 0x80,
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

    /// Submission queue doorbell writes that have not yet been processed.
    ///
    /// MMIO handlers are not allowed to access guest memory (DMA), so we defer SQ processing to an
    /// explicit [`NvmeController::process`] step performed by the platform.
    pending_sq_tail: BTreeMap<u16, u16>,

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
        let cap =
            (mqes & 0xffff) | (css_nvm << 37) | (mpsmin << 48) | (mpsmax << 52) | (dstrd << 32);

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
            pending_sq_tail: BTreeMap::new(),
            intx_level: false,
        }
    }

    /// Construct an NVMe controller from an [`aero_storage::VirtualDisk`].
    ///
    /// This is a convenience wrapper around [`from_virtual_disk`] that returns an error if the
    /// disk capacity is not a multiple of 512 bytes.
    pub fn try_new_from_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    /// Like [`NvmeController::try_new_from_virtual_disk`], but accepts any concrete
    /// `aero_storage` disk type.
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    pub const fn bar0_len() -> u64 {
        // Registers (0x0..0x1000) + a small doorbell region for a few queues.
        0x4000
    }

    pub fn mmio_read(&self, offset: u64, size: usize) -> u64 {
        // `PhysicalMemoryBus` issues naturally-aligned MMIO reads in sizes 1/2/4/8. Guests may also
        // access 64-bit registers via two 32-bit operations (e.g. CAP, ASQ, ACQ).
        //
        // Implement reads by slicing bytes out of the containing 32-bit word.
        let size = size.clamp(1, 8);
        let mut out = 0u64;

        for i in 0..size {
            let byte_off = match offset.checked_add(i as u64) {
                Some(v) => v,
                None => break,
            };
            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let word = self.mmio_read_u32(word_off);
            let byte = ((word >> shift) & 0xFF) as u64;
            out |= byte << (i * 8);
        }

        out
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        // Like `mmio_read`, implement writes by collecting byte-lane updates into 32-bit writes.
        // This supports:
        // - 64-bit registers written via two 32-bit operations (ASQ/ACQ)
        // - sub-word accesses where the bus chooses 1/2-byte operations.
        let size = size.clamp(1, 8);

        let mut idx = 0usize;
        while idx < size {
            let byte_off = match offset.checked_add(idx as u64) {
                Some(v) => v,
                None => break,
            };
            let word_off = byte_off & !3;

            // Collect all bytes that fall into this 32-bit word.
            let mut be_mask = 0u32;
            let mut write_val = 0u32;
            while idx < size {
                let off = match offset.checked_add(idx as u64) {
                    Some(v) => v,
                    None => break,
                };
                if (off & !3) != word_off {
                    break;
                }

                let byte_idx_in_word = (off & 3) as usize;
                let shift = (byte_idx_in_word * 8) as u32;
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
                write_val |= byte << shift;
                be_mask |= 0xFFu32 << shift;
                idx += 1;
            }

            if be_mask == 0 {
                continue;
            }

            self.mmio_write_u32(word_off, write_val, be_mask);
        }
    }

    fn mmio_read_u32(&self, offset: u64) -> u32 {
        match offset {
            0x0000 => self.cap as u32,
            0x0004 => (self.cap >> 32) as u32,
            0x0008 => self.vs,
            0x000c => self.intms,
            0x0010 => 0, // INTMC is write-only.
            0x0014 => self.cc,
            0x001c => self.csts,
            0x0024 => self.aqa,
            0x0028 => self.asq as u32,
            0x002c => (self.asq >> 32) as u32,
            0x0030 => self.acq as u32,
            0x0034 => (self.acq >> 32) as u32,
            _ if offset >= 0x1000 => {
                if self.csts & 1 == 0 {
                    return 0;
                }

                let stride = 4u64 << ((self.cap >> 32) & 0xf);
                if stride == 0 {
                    return 0;
                }
                // Ignore reserved words within a stride larger than 4 bytes.
                if ((offset - 0x1000) % stride) >= 4 {
                    return 0;
                }

                let idx = (offset - 0x1000) / stride;
                let qid = (idx / 2) as u16;
                let is_cq = idx % 2 == 1;
                let val = if is_cq {
                    if qid == 0 {
                        self.admin_cq.as_ref().map(|cq| cq.head).unwrap_or(0)
                    } else {
                        self.io_cqs.get(&qid).map(|cq| cq.head).unwrap_or(0)
                    }
                } else if qid == 0 {
                    self.admin_sq.as_ref().map(|sq| sq.tail).unwrap_or(0)
                } else {
                    self.io_sqs.get(&qid).map(|sq| sq.tail).unwrap_or(0)
                };
                u32::from(val)
            }
            _ => 0,
        }
    }

    fn mmio_write_u32(&mut self, offset: u64, write_val: u32, be_mask: u32) {
        match offset {
            // INTMS: write-1-to-set bits in the interrupt mask.
            0x000c => {
                self.intms |= write_val & be_mask;
                self.refresh_intx_level();
            }
            // INTMC: write-1-to-clear bits in the interrupt mask.
            0x0010 => {
                self.intms &= !(write_val & be_mask);
                self.refresh_intx_level();
            }
            // CC: full register write (with byte-enables).
            0x0014 => {
                let prev_en = (self.cc & 1) != 0;
                let merged = (self.cc & !be_mask) | (write_val & be_mask);
                self.cc = merged;
                let new_en = (self.cc & 1) != 0;
                if !prev_en && new_en {
                    self.enable();
                } else if prev_en && !new_en {
                    self.disable();
                }
            }
            // AQA: writable only when CC.EN=0.
            0x0024 => {
                if self.cc & 1 == 0 {
                    self.aqa = (self.aqa & !be_mask) | (write_val & be_mask);
                }
            }
            // ASQ (low/high dword): writable only when CC.EN=0.
            0x0028 => {
                if self.cc & 1 == 0 {
                    let cur = self.asq as u32;
                    let merged = (cur & !be_mask) | (write_val & be_mask);
                    self.asq = (self.asq & 0xffff_ffff_0000_0000) | (merged as u64);
                }
            }
            0x002c => {
                if self.cc & 1 == 0 {
                    let cur = (self.asq >> 32) as u32;
                    let merged = (cur & !be_mask) | (write_val & be_mask);
                    self.asq = (self.asq & 0x0000_0000_ffff_ffff) | ((merged as u64) << 32);
                }
            }
            // ACQ (low/high dword): writable only when CC.EN=0.
            0x0030 => {
                if self.cc & 1 == 0 {
                    let cur = self.acq as u32;
                    let merged = (cur & !be_mask) | (write_val & be_mask);
                    self.acq = (self.acq & 0xffff_ffff_0000_0000) | (merged as u64);
                }
            }
            0x0034 => {
                if self.cc & 1 == 0 {
                    let cur = (self.acq >> 32) as u32;
                    let merged = (cur & !be_mask) | (write_val & be_mask);
                    self.acq = (self.acq & 0x0000_0000_ffff_ffff) | ((merged as u64) << 32);
                }
            }
            // Doorbells: treat the 32-bit register as a whole (but allow byte enables).
            _ if offset >= 0x1000 => {
                let stride = 4u64 << ((self.cap >> 32) & 0xf);
                if stride == 0 {
                    return;
                }
                // Ignore reserved words within a stride larger than 4 bytes.
                if ((offset - 0x1000) % stride) >= 4 {
                    return;
                }

                let idx = (offset - 0x1000) / stride;
                let qid = (idx / 2) as u16;
                let is_cq = idx % 2 == 1;

                // Doorbell values are 16-bit; preserve the unwritten bytes to avoid surprising
                // behaviour if a guest uses sub-dword writes.
                let val = if self.csts & 1 == 0 {
                    0u16
                } else if is_cq {
                    if qid == 0 {
                        self.admin_cq.as_ref().map(|cq| cq.head).unwrap_or(0)
                    } else {
                        self.io_cqs.get(&qid).map(|cq| cq.head).unwrap_or(0)
                    }
                } else if qid == 0 {
                    self.admin_sq.as_ref().map(|sq| sq.tail).unwrap_or(0)
                } else {
                    self.io_sqs.get(&qid).map(|sq| sq.tail).unwrap_or(0)
                };

                let current = u32::from(val);
                let merged = (current & !be_mask) | (write_val & be_mask);

                // MMIO handlers are not allowed to DMA. For SQ doorbells this records the new tail
                // and defers submission/completion processing to `NvmeController::process()`.
                self.write_doorbell(offset, merged);
            }
            _ => {}
        }
    }

    fn enable(&mut self) {
        // Basic validation: admin queues must be configured and page aligned.
        // NVMe AQA:
        // - bits 11:0  = ACQS (0-based)
        // - bits 27:16 = ASQS (0-based)
        let acqs = (self.aqa & 0x0fff) as u16 + 1;
        let asqs = ((self.aqa >> 16) & 0x0fff) as u16 + 1;
        if asqs == 0 || acqs == 0 || self.asq == 0 || self.acq == 0 {
            self.csts = 0;
            return;
        }
        if self.asq & (PAGE_SIZE as u64 - 1) != 0 || self.acq & (PAGE_SIZE as u64 - 1) != 0 {
            self.csts = 0;
            return;
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
        self.pending_sq_tail.clear();
        self.csts = 1; // RDY
        self.refresh_intx_level();
    }

    fn disable(&mut self) {
        self.csts = 0;
        self.admin_sq = None;
        self.admin_cq = None;
        self.io_sqs.clear();
        self.io_cqs.clear();
        self.pending_sq_tail.clear();
        self.intx_level = false;
    }

    fn write_doorbell(&mut self, offset: u64, value: u32) {
        if self.csts & 1 == 0 {
            return;
        }
        let stride = 4u64 << ((self.cap >> 32) & 0xf);
        let idx = (offset - 0x1000) / stride;
        let qid = (idx / 2) as u16;
        let is_cq = idx % 2 == 1;
        let val = value as u16;

        if is_cq {
            self.set_cq_head(qid, val);
            self.refresh_intx_level();
            return;
        }

        self.set_sq_tail(qid, val);
        self.pending_sq_tail.insert(qid, val);
    }

    fn set_sq_tail(&mut self, qid: u16, tail: u16) {
        if qid == 0 {
            if let Some(ref mut sq) = self.admin_sq {
                sq.tail = tail % sq.size;
            }
            return;
        }
        if let Some(sq) = self.io_sqs.get_mut(&qid) {
            sq.tail = tail % sq.size;
        }
    }

    fn set_cq_head(&mut self, qid: u16, head: u16) {
        if qid == 0 {
            if let Some(ref mut cq) = self.admin_cq {
                cq.head = head % cq.size;
            }
            return;
        }
        if let Some(cq) = self.io_cqs.get_mut(&qid) {
            cq.head = head % cq.size;
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

    fn process_sq(&mut self, qid: u16, memory: &mut dyn MemoryBus) {
        if qid == 0 {
            self.process_queue_pair_admin(memory);
            return;
        }
        self.process_queue_pair_io(qid, memory)
    }

    /// Process any DMA work that was made pending by MMIO doorbell writes.
    ///
    /// This is intended to be called from a platform "device processing" step, where the caller
    /// has access to guest physical memory (`memory::MemoryBus`).
    pub fn process(&mut self, memory: &mut dyn MemoryBus) {
        if self.csts & 1 == 0 {
            self.pending_sq_tail.clear();
            return;
        }

        let pending = std::mem::take(&mut self.pending_sq_tail);
        for (qid, tail) in pending {
            // Re-apply the most recently written tail value in case the queue was created after
            // the doorbell write (possible when admin queue processing is deferred).
            self.set_sq_tail(qid, tail);
            self.process_sq(qid, memory);
        }

        // Ensure interrupt level stays coherent even if no queues were processed.
        self.refresh_intx_level();
    }

    fn process_queue_pair_admin(&mut self, memory: &mut dyn MemoryBus) {
        let mut sq = match self.admin_sq.take() {
            Some(sq) => sq,
            None => return,
        };
        let mut cq = match self.admin_cq.take() {
            Some(cq) => cq,
            None => {
                self.admin_sq = Some(sq);
                return;
            }
        };

        while sq.head != sq.tail {
            let cmd = read_command(sq.base, sq.head, memory);
            let (status, result) = self.execute_admin(cmd, memory);
            sq.head = sq.head.wrapping_add(1) % sq.size;
            post_completion(&mut cq, &sq, cmd.cid, status, result, memory);
        }

        self.admin_sq = Some(sq);
        self.admin_cq = Some(cq);
        self.refresh_intx_level();
    }

    fn process_queue_pair_io(&mut self, qid: u16, memory: &mut dyn MemoryBus) {
        let cqid = match self.io_sqs.get(&qid).map(|sq| sq.cqid) {
            Some(cqid) => cqid,
            None => return,
        };

        loop {
            let head = self.io_sqs.get(&qid).unwrap().head;
            let tail = self.io_sqs.get(&qid).unwrap().tail;
            if head == tail {
                break;
            }

            let cmd = {
                let sq = self.io_sqs.get(&qid).unwrap();
                read_command(sq.base, sq.head, memory)
            };

            let (status, result) = self.execute_io(cmd, memory);

            {
                let sq = self.io_sqs.get_mut(&qid).unwrap();
                sq.head = sq.head.wrapping_add(1) % sq.size;
            }

            let (sq_snapshot, cq) = {
                let sq_snapshot = *self.io_sqs.get(&qid).unwrap();
                let cq = self.io_cqs.get_mut(&cqid).unwrap();
                (sq_snapshot, cq)
            };

            post_completion(cq, &sq_snapshot, cmd.cid, status, result, memory);
        }

        self.refresh_intx_level();
    }

    fn execute_admin(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        match cmd.opc {
            0x06 => self.cmd_identify(cmd, memory),
            0x05 => self.cmd_create_io_cq(cmd),
            0x01 => self.cmd_create_io_sq(cmd),
            _ => (NvmeStatus::INVALID_OPCODE, 0),
        }
    }

    fn execute_io(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        match cmd.opc {
            0x00 => self.cmd_flush(),
            0x01 => self.cmd_write(cmd, memory),
            0x02 => self.cmd_read(cmd, memory),
            _ => (NvmeStatus::INVALID_OPCODE, 0),
        }
    }

    fn cmd_identify(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        let cns = (cmd.cdw10 & 0xff) as u8;
        let data = match cns {
            0x01 => self.identify_controller(),
            0x00 => self.identify_namespace(cmd.nsid),
            _ => return (NvmeStatus::INVALID_FIELD, 0),
        };

        let status = self.dma_write_prp(memory, cmd.prp1, cmd.prp2, &data);
        (status, 0)
    }

    fn cmd_create_io_cq(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let qsize = ((cmd.cdw10 >> 16) & 0xffff) as u16 + 1;
        if qsize == 0 || qsize > 128 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        if cmd.prp1 == 0 || cmd.prp1 & (PAGE_SIZE as u64 - 1) != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
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
        (NvmeStatus::SUCCESS, 0)
    }

    fn cmd_create_io_sq(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let qsize = ((cmd.cdw10 >> 16) & 0xffff) as u16 + 1;
        if qsize == 0 || qsize > 128 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        if cmd.prp1 == 0 || cmd.prp1 & (PAGE_SIZE as u64 - 1) != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let cqid = (cmd.cdw11 & 0xffff) as u16;
        if !self.io_cqs.contains_key(&cqid) {
            return (NvmeStatus::INVALID_QID, 0);
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
        (NvmeStatus::SUCCESS, 0)
    }

    fn cmd_read(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u64;
        let sectors = nlb + 1;
        let sector_size = self.disk.sector_size() as usize;

        if slba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
        }

        let len = sectors as usize * sector_size;

        let mut data = vec![0u8; len];
        let status = match self.disk.read_sectors(slba, &mut data) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        if status != NvmeStatus::SUCCESS {
            return (status, 0);
        }

        let status = self.dma_write_prp(memory, cmd.prp1, cmd.prp2, &data);
        (status, 0)
    }

    fn cmd_write(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u64;
        let sectors = nlb + 1;
        let sector_size = self.disk.sector_size() as usize;

        if slba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
        }

        let len = sectors as usize * sector_size;

        let mut data = vec![0u8; len];
        let status = self.dma_read_prp(memory, cmd.prp1, cmd.prp2, &mut data);
        if status != NvmeStatus::SUCCESS {
            return (status, 0);
        }

        let status = match self.disk.write_sectors(slba, &data) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };

        (status, 0)
    }

    fn cmd_flush(&mut self) -> (NvmeStatus, u32) {
        let status = match self.disk.flush() {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        (status, 0)
    }

    fn dma_write_prp(
        &self,
        memory: &mut dyn MemoryBus,
        prp1: u64,
        prp2: u64,
        data: &[u8],
    ) -> NvmeStatus {
        let segs = match prp_segments(memory, prp1, prp2, data.len()) {
            Ok(segs) => segs,
            Err(status) => return status,
        };
        let mut offset = 0usize;
        for (addr, len) in segs {
            memory.write_physical(addr, &data[offset..offset + len]);
            offset += len;
        }
        NvmeStatus::SUCCESS
    }

    fn dma_read_prp(
        &self,
        memory: &mut dyn MemoryBus,
        prp1: u64,
        prp2: u64,
        data: &mut [u8],
    ) -> NvmeStatus {
        let segs = match prp_segments(memory, prp1, prp2, data.len()) {
            Ok(segs) => segs,
            Err(status) => return status,
        };
        let mut offset = 0usize;
        for (addr, len) in segs {
            memory.read_physical(addr, &mut data[offset..offset + len]);
            offset += len;
        }
        NvmeStatus::SUCCESS
    }

    fn identify_controller(&self) -> Vec<u8> {
        let mut data = vec![0u8; 4096];

        // VID (u16) / SSVID (u16)
        data[0..2].copy_from_slice(&0x1b36u16.to_le_bytes());
        data[2..4].copy_from_slice(&0x1b36u16.to_le_bytes());

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

        // SQES/CQES at offset 512 (0x200) for OS queue entry size negotiation.
        data[512] = 0x66; // SQE min/max = 2^6 = 64 bytes
        data[513] = 0x44; // CQE min/max = 2^4 = 16 bytes

        data
    }

    fn identify_namespace(&self, nsid: u32) -> Vec<u8> {
        let mut data = vec![0u8; 4096];
        if nsid != 1 {
            return data;
        }

        let nsze = self.disk.total_sectors();
        data[0..8].copy_from_slice(&nsze.to_le_bytes()); // NSZE
        data[8..16].copy_from_slice(&nsze.to_le_bytes()); // NCAP
        data[16..24].copy_from_slice(&nsze.to_le_bytes()); // NUSE

        // FLBAS at offset 26 (0x1a): format 0, metadata 0
        data[26] = 0;

        // LBAF0 at offset 128 (0x80): MS=0, LBADS=9 (512 bytes), RP=0
        let sector_size = self.disk.sector_size().max(1);
        data[128 + 2] = sector_size.trailing_zeros() as u8;

        data
    }
}

impl IoSnapshot for NvmeController {
    const DEVICE_ID: [u8; 4] = <NvmeControllerState as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = <NvmeControllerState as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        // INTMC is write-only in this device model; we store 0 for compatibility with the shared
        // snapshot state struct.
        //
        // This controller processes commands synchronously, so there is no meaningful in-flight state.
        let state = NvmeControllerState {
            cap: self.cap,
            vs: self.vs,
            intms: self.intms,
            intmc: 0,
            cc: self.cc,
            csts: self.csts,
            aqa: self.aqa,
            asq: self.asq,
            acq: self.acq,
            admin_sq: self.admin_sq.as_ref().map(|sq| NvmeSubmissionQueueState {
                qid: sq.id,
                base: sq.base,
                size: sq.size,
                head: sq.head,
                tail: sq.tail,
                cqid: sq.cqid,
            }),
            admin_cq: self.admin_cq.as_ref().map(|cq| NvmeCompletionQueueState {
                qid: cq.id,
                base: cq.base,
                size: cq.size,
                head: cq.head,
                tail: cq.tail,
                phase: cq.phase,
                irq_enabled: cq.irq_enabled,
            }),
            io_sqs: self
                .io_sqs
                .values()
                .map(|sq| NvmeSubmissionQueueState {
                    qid: sq.id,
                    base: sq.base,
                    size: sq.size,
                    head: sq.head,
                    tail: sq.tail,
                    cqid: sq.cqid,
                })
                .collect(),
            io_cqs: self
                .io_cqs
                .values()
                .map(|cq| NvmeCompletionQueueState {
                    qid: cq.id,
                    base: cq.base,
                    size: cq.size,
                    head: cq.head,
                    tail: cq.tail,
                    phase: cq.phase,
                    irq_enabled: cq.irq_enabled,
                })
                .collect(),
            intx_level: self.intx_level,
            in_flight: Vec::new(),
        };

        state.save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut state = NvmeControllerState::default();
        state.load_state(bytes)?;

        self.cap = state.cap;
        self.vs = state.vs;
        self.intms = state.intms;
        self.cc = state.cc;
        self.csts = state.csts;
        self.aqa = state.aqa;
        self.asq = state.asq;
        self.acq = state.acq;

        self.admin_sq = state.admin_sq.map(|sq| SubmissionQueue {
            id: sq.qid,
            size: sq.size,
            base: sq.base,
            head: sq.head,
            tail: sq.tail,
            cqid: sq.cqid,
        });

        self.admin_cq = state.admin_cq.map(|cq| CompletionQueue {
            id: cq.qid,
            size: cq.size,
            base: cq.base,
            head: cq.head,
            tail: cq.tail,
            phase: cq.phase,
            irq_enabled: cq.irq_enabled,
        });

        self.io_sqs.clear();
        for sq in state.io_sqs {
            self.io_sqs.insert(
                sq.qid,
                SubmissionQueue {
                    id: sq.qid,
                    size: sq.size,
                    base: sq.base,
                    head: sq.head,
                    tail: sq.tail,
                    cqid: sq.cqid,
                },
            );
        }

        self.io_cqs.clear();
        for cq in state.io_cqs {
            self.io_cqs.insert(
                cq.qid,
                CompletionQueue {
                    id: cq.qid,
                    size: cq.size,
                    base: cq.base,
                    head: cq.head,
                    tail: cq.tail,
                    phase: cq.phase,
                    irq_enabled: cq.irq_enabled,
                },
            );
        }

        self.pending_sq_tail.clear();

        // Recompute derived INTx level so it stays coherent with restored masks + queue state.
        self.refresh_intx_level();
        Ok(())
    }
}

fn read_command(sq_base: u64, head: u16, memory: &mut dyn MemoryBus) -> NvmeCommand {
    let mut bytes = [0u8; 64];
    let addr = sq_base + head as u64 * 64;
    memory.read_physical(addr, &mut bytes);
    NvmeCommand::parse(bytes)
}

fn post_completion(
    cq: &mut CompletionQueue,
    sq: &SubmissionQueue,
    cid: u16,
    status: NvmeStatus,
    result: u32,
    memory: &mut dyn MemoryBus,
) {
    let next_tail = cq.tail.wrapping_add(1) % cq.size;
    if next_tail == cq.head {
        // Completion queue full. The spec expects the host to avoid this.
        return;
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
    memory.write_physical(addr, &entry.to_bytes());

    cq.tail = next_tail;
    if cq.tail == 0 {
        cq.phase = !cq.phase;
    }
}

fn write_ascii_padded(dst: &mut [u8], s: &str) {
    dst.fill(b' ');
    let bytes = s.as_bytes();
    let len = bytes.len().min(dst.len());
    dst[..len].copy_from_slice(&bytes[..len]);
}

fn prp_segments(
    memory: &mut dyn MemoryBus,
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
        let pages_needed = remaining.div_ceil(PAGE_SIZE);
        let max_pages_this_list = if pages_needed > entries_per_list {
            // Chained PRP list: last entry is a pointer to the next list.
            entries_per_list - 1
        } else {
            pages_needed
        };

        for entry_index in 0..max_pages_this_list {
            let entry_addr = list_addr + entry_index as u64 * 8;
            let page = memory.read_u64(entry_addr);
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
        let next = memory.read_u64(chain_ptr_addr);
        if next == 0 || next & page_mask != 0 {
            return Err(NvmeStatus::INVALID_FIELD);
        }
        list_addr = next;
    }

    Ok(segs)
}

/// NVMe PCI device model (PCI config space + BAR0 MMIO registers).
pub struct NvmePciDevice {
    config: PciConfigSpace,
    pub controller: NvmeController,
}

impl NvmePciDevice {
    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let controller = NvmeController::new(disk);
        let mut config = profile::NVME_CONTROLLER.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: NvmeController::bar0_len(),
                prefetchable: false,
            },
        );
        Self { config, controller }
    }

    /// Construct an NVMe PCI device from an [`aero_storage::VirtualDisk`].
    ///
    /// This is a convenience wrapper around [`from_virtual_disk`] that returns an error if the
    /// disk capacity is not a multiple of 512 bytes.
    pub fn try_new_from_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    /// Like [`NvmePciDevice::try_new_from_virtual_disk`], but accepts any concrete
    /// `aero_storage` disk type.
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    pub fn irq_level(&self) -> bool {
        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
        self.controller.intx_level
    }

    pub fn pci_read_u32(&mut self, offset: u16) -> u32 {
        self.config.read(offset, 4)
    }

    pub fn pci_write_u32(&mut self, offset: u16, value: u32) {
        self.config.write(offset, 4, value);
    }

    pub fn controller(&self) -> &NvmeController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut NvmeController {
        &mut self.controller
    }

    /// Process any DMA work that was made pending by MMIO doorbell writes.
    pub fn process(&mut self, memory: &mut dyn MemoryBus) {
        self.controller.process(memory);
    }
}

impl Default for NvmePciDevice {
    fn default() -> Self {
        // Default to a small in-memory disk.
        #[derive(Clone)]
        struct EmptyDisk;
        impl DiskBackend for EmptyDisk {
            fn sector_size(&self) -> u32 {
                512
            }
            fn total_sectors(&self) -> u64 {
                0
            }
            fn read_sectors(&mut self, _lba: u64, _buffer: &mut [u8]) -> DiskResult<()> {
                Err(DiskError::OutOfRange {
                    lba: 0,
                    sectors: 0,
                    capacity_sectors: 0,
                })
            }
            fn write_sectors(&mut self, _lba: u64, _buffer: &[u8]) -> DiskResult<()> {
                Err(DiskError::OutOfRange {
                    lba: 0,
                    sectors: 0,
                    capacity_sectors: 0,
                })
            }
            fn flush(&mut self) -> DiskResult<()> {
                Ok(())
            }
        }

        Self::new(Box::new(EmptyDisk))
    }
}

impl PciDevice for NvmePciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

impl MmioHandler for NvmePciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.controller.mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.controller.mmio_write(offset, size, value)
    }
}

impl IoSnapshot for NvmePciDevice {
    const DEVICE_ID: [u8; 4] = *b"NVMP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());
        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_PCI) {
            match r.header().device_version.minor {
                0 => {
                    // Backward-compat: NVMP 1.0 stored a bespoke PCI config model.
                    let mut d = Decoder::new(buf);
                    let bar0 = d.u64()?;
                    let bar0_probe = d.bool()?;
                    let command = d.u16()?;
                    let status = d.u16()?;
                    let interrupt_line = d.u8()?;
                    d.finish()?;

                    let mut state = self.config.snapshot_state();
                    state.bytes[0x04..0x06].copy_from_slice(&command.to_le_bytes());
                    state.bytes[0x06..0x08].copy_from_slice(&status.to_le_bytes());
                    state.bytes[PciConfigSpace::INTERRUPT_LINE_OFFSET as usize] = interrupt_line;
                    state.bar_base[0] = bar0;
                    state.bar_probe[0] = bar0_probe;
                    self.config.restore_state(&state);
                }
                _ => {
                    let mut d = Decoder::new(buf);
                    let mut config_bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
                    config_bytes.copy_from_slice(d.bytes(PCI_CONFIG_SPACE_SIZE)?);

                    let mut bar_base = [0u64; 6];
                    let mut bar_probe = [false; 6];
                    for i in 0..6 {
                        bar_base[i] = d.u64()?;
                        bar_probe[i] = d.bool()?;
                    }
                    d.finish()?;

                    self.config.restore_state(&PciConfigSpaceState {
                        bytes: config_bytes,
                        bar_base,
                        bar_probe,
                    });
                }
            }
        }

        if let Some(buf) = r.bytes(TAG_CONTROLLER) {
            self.controller.load_state(buf)?;
        } else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing nvme controller state",
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_storage::{MemBackend, RawDisk, VirtualDisk as StorageVirtualDisk, SECTOR_SIZE};
    use std::sync::{Arc, Mutex};

    struct TestMem {
        buf: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self {
                buf: vec![0u8; size],
            }
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            assert!(end <= self.buf.len(), "out-of-bounds DMA read");
            buf.copy_from_slice(&self.buf[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            assert!(end <= self.buf.len(), "out-of-bounds DMA write");
            self.buf[start..end].copy_from_slice(buf);
        }
    }

    #[derive(Clone)]
    struct TestDisk {
        inner: Arc<Mutex<RawDisk<MemBackend>>>,
        flushed: Arc<Mutex<u32>>,
    }

    impl TestDisk {
        fn new(sectors: u64) -> Self {
            let capacity_bytes = sectors * SECTOR_SIZE as u64;
            let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
            Self {
                inner: Arc::new(Mutex::new(disk)),
                flushed: Arc::new(Mutex::new(0)),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, RawDisk<MemBackend>> {
            self.inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn flush_count(&self) -> u32 {
            *self
                .flushed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl StorageVirtualDisk for TestDisk {
        fn capacity_bytes(&self) -> u64 {
            self.lock().capacity_bytes()
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            self.lock().read_at(offset, buf)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
            self.lock().write_at(offset, buf)
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            *self
                .flushed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
            self.lock().flush()
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

    fn read_cqe(mem: &mut TestMem, addr: u64) -> CqEntry {
        let mut bytes = [0u8; 16];
        mem.read_physical(addr, &mut bytes);
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
    fn pci_bar0_probe_and_program() {
        let disk = TestDisk::new(1024);
        let mut dev = NvmePciDevice::new(from_virtual_disk(Box::new(disk)).unwrap());

        dev.config_mut().write(0x10, 4, 0xffff_ffff);
        let mask_lo = dev.config_mut().read(0x10, 4);
        let mask_hi = dev.config_mut().read(0x14, 4);
        let size = NvmeController::bar0_len() as u32;
        assert_eq!(mask_lo, (!(size - 1)) & 0xffff_fff0 | 0x4);
        assert_eq!(mask_hi, 0xffff_ffff);

        dev.config_mut().write(0x10, 4, 0xfebf_0000);
        dev.config_mut().write(0x14, 4, 0);
        assert_eq!(dev.config_mut().read(0x10, 4), 0xfebf_0004);
        assert_eq!(dev.config_mut().read(0x14, 4), 0);

        assert_eq!(
            dev.pci_read_u32(0x00),
            (profile::PCI_DEVICE_ID_QEMU_NVME as u32) << 16
                | (profile::PCI_VENDOR_ID_REDHAT_QEMU as u32)
        );
        assert_eq!(dev.config_mut().read(0x08, 4), 0x01_08_02_00);
    }

    #[test]
    fn registers_enable_sets_rdy() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        ctrl.mmio_write(0x0024, 4, 0x000f_000f); // 16/16 queues
        ctrl.mmio_write(0x0028, 8, 0x10000);
        ctrl.mmio_write(0x0030, 8, 0x20000);
        ctrl.mmio_write(0x0014, 4, 1);

        assert_eq!(ctrl.mmio_read(0x001c, 4) & 1, 1);
    }

    #[test]
    fn cap_can_be_read_as_two_dwords() {
        let disk = TestDisk::new(1024);
        let ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let cap64 = ctrl.mmio_read(0x0000, 8);
        let cap_lo = ctrl.mmio_read(0x0000, 4) as u32;
        let cap_hi = ctrl.mmio_read(0x0004, 4) as u32;
        let cap32 = (cap_lo as u64) | ((cap_hi as u64) << 32);
        assert_eq!(cap64, cap32);
    }

    #[test]
    fn aqa_fields_map_to_admin_queue_sizes() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // ASQS = 8 entries, ACQS = 4 entries (both are 0-based in AQA).
        ctrl.mmio_write(0x0024, 4, (7u64 << 16) | 3);
        ctrl.mmio_write(0x0028, 8, 0x10000);
        ctrl.mmio_write(0x0030, 8, 0x20000);
        ctrl.mmio_write(0x0014, 4, 1);

        assert_eq!(ctrl.admin_sq.as_ref().unwrap().size, 8);
        assert_eq!(ctrl.admin_cq.as_ref().unwrap().size, 4);
    }

    #[test]
    fn admin_identify_controller_supports_split_asq_acq_writes() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(1024 * 1024);

        let asq = 0x10000u64;
        let acq = 0x20000u64;
        let id_buf = 0x30000u64;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f);

        // Program ASQ/ACQ via two 32-bit writes each, as many guests do.
        ctrl.mmio_write(0x0028, 4, asq as u32 as u64);
        ctrl.mmio_write(0x002c, 4, (asq >> 32) as u32 as u64);
        ctrl.mmio_write(0x0030, 4, acq as u32 as u64);
        ctrl.mmio_write(0x0034, 4, (acq >> 32) as u32 as u64);

        ctrl.mmio_write(0x0014, 4, 1);
        assert_eq!(ctrl.mmio_read(0x001c, 4) & 1, 1, "CSTS.RDY should be set");

        let mut cmd = build_command(0x06);
        set_cid(&mut cmd, 0x1234);
        set_prp1(&mut cmd, id_buf);
        set_cdw10(&mut cmd, 0x01); // CNS=1 (controller)

        mem.write_physical(asq, &cmd);
        ctrl.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, acq);
        assert_eq!(cqe.cid, 0x1234);
        assert_eq!(cqe.sqid, 0);
        assert_eq!(cqe.status & 0x1, 1); // phase
        assert_eq!(cqe.status & !0x1, 0); // success

        // Identify data should have been DMA'd into guest memory.
        let vid = mem.read_u16(id_buf);
        assert_eq!(vid, 0x1b36);
    }

    #[test]
    fn admin_identify_controller_writes_data_and_completion() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let id_buf = 0x30000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f);
        ctrl.mmio_write(0x0028, 8, asq);
        ctrl.mmio_write(0x0030, 8, acq);
        ctrl.mmio_write(0x0014, 4, 1);

        let mut cmd = build_command(0x06);
        set_cid(&mut cmd, 0x1234);
        set_prp1(&mut cmd, id_buf);
        set_cdw10(&mut cmd, 0x01); // CNS=1 (controller)

        mem.write_physical(asq, &cmd);
        ctrl.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, acq);
        assert_eq!(cqe.cid, 0x1234);
        assert_eq!(cqe.sqid, 0);
        assert_eq!(cqe.status & 0x1, 1); // phase
        assert_eq!(cqe.status & !0x1, 0); // success

        let vid = mem.read_u16(id_buf);
        assert_eq!(vid, 0x1b36);
    }

    #[test]
    fn create_io_queues_and_rw_roundtrip() {
        let disk = TestDisk::new(1024);
        let disk_state = disk.clone();
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(2 * 1024 * 1024);
        let sector_size = 512usize;

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;
        let write_buf = 0x60000;
        let read_buf = 0x61000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f);
        ctrl.mmio_write(0x0028, 8, asq);
        ctrl.mmio_write(0x0030, 8, acq);
        ctrl.mmio_write(0x0014, 4, 1);

        // Create IO CQ (qid=1, size=16, PC+IEN).
        let mut cmd = build_command(0x05);
        set_cid(&mut cmd, 1);
        set_prp1(&mut cmd, io_cq);
        set_cdw10(&mut cmd, (15u32 << 16) | 1);
        set_cdw11(&mut cmd, 0x3);
        mem.write_physical(asq, &cmd);
        ctrl.mmio_write(0x1000, 4, 1);
        ctrl.process(&mut mem);

        // Create IO SQ (qid=1, size=16, CQID=1).
        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 2);
        set_prp1(&mut cmd, io_sq);
        set_cdw10(&mut cmd, (15u32 << 16) | 1);
        set_cdw11(&mut cmd, 1);
        mem.write_physical(asq + 64, &cmd);
        ctrl.mmio_write(0x1000, 4, 2);
        ctrl.process(&mut mem);

        // WRITE 1 sector at LBA 0.
        let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();
        mem.write_physical(write_buf, &payload);

        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, write_buf);
        set_cdw10(&mut cmd, 0); // slba low
        set_cdw11(&mut cmd, 0); // slba high
        set_cdw12(&mut cmd, 0); // nlb = 0
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & !0x1, 0);

        // READ it back.
        let mut cmd = build_command(0x02);
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 2); // SQ1 tail = 2
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & !0x1, 0);

        let mut out = vec![0u8; sector_size];
        mem.read_physical(read_buf, &mut out);
        assert_eq!(out, payload);

        // FLUSH.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 2 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 3);
        ctrl.process(&mut mem);
        assert_eq!(disk_state.flush_count(), 1);

        // Sanity: disk image contains the written sector.
        let mut disk = disk_state.clone();
        let mut data = vec![0u8; sector_size];
        disk.read_at(0, &mut data).unwrap();
        assert_eq!(data, payload.as_slice());
    }

    #[test]
    fn cq_phase_toggles_on_wrap() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(2 * 1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;

        ctrl.mmio_write(0x0024, 4, 0x000f_000f);
        ctrl.mmio_write(0x0028, 8, asq);
        ctrl.mmio_write(0x0030, 8, acq);
        ctrl.mmio_write(0x0014, 4, 1);

        // Create IO CQ (qid=1, size=2).
        let mut cmd = build_command(0x05);
        set_cid(&mut cmd, 1);
        set_prp1(&mut cmd, io_cq);
        set_cdw10(&mut cmd, (1u32 << 16) | 1);
        set_cdw11(&mut cmd, 0x3); // PC+IEN
        mem.write_physical(asq, &cmd);
        ctrl.mmio_write(0x1000, 4, 1);
        ctrl.process(&mut mem);

        // Create IO SQ (qid=1, size=2, cqid=1).
        let mut cmd = build_command(0x01);
        set_cid(&mut cmd, 2);
        set_prp1(&mut cmd, io_sq);
        set_cdw10(&mut cmd, (1u32 << 16) | 1);
        set_cdw11(&mut cmd, 1);
        mem.write_physical(asq + 64, &cmd);
        ctrl.mmio_write(0x1000, 4, 2);
        ctrl.process(&mut mem);

        // Consume admin CQ entries (2 completions from queue creation) so INTx reflects I/O CQ.
        ctrl.mmio_write(0x1004, 4, 2);

        let sq_tail_db = 0x1008;
        let cq_head_db = 0x100c;

        // 1) FLUSH at SQ slot 0, CQ slot 0, phase=1.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(sq_tail_db, 4, 1);
        ctrl.process(&mut mem);
        assert!(ctrl.intx_level);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & 0x1, 1);
        assert_eq!(cqe.status & !0x1, 0);

        ctrl.mmio_write(cq_head_db, 4, 1);
        assert!(!ctrl.intx_level);

        // 2) FLUSH at SQ slot 1, CQ slot 1, phase=1 (tail wraps after posting).
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(sq_tail_db, 4, 0);
        ctrl.process(&mut mem);
        assert!(ctrl.intx_level);

        let cqe = read_cqe(&mut mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & 0x1, 1);
        assert_eq!(cqe.status & !0x1, 0);

        ctrl.mmio_write(cq_head_db, 4, 0);
        assert!(!ctrl.intx_level);

        // 3) FLUSH at SQ slot 0 again, CQ slot 0 again, phase toggles to 0.
        let mut cmd = build_command(0x00);
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(sq_tail_db, 4, 1);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x12);
        assert_eq!(cqe.status & 0x1, 0);
        assert_eq!(cqe.status & !0x1, 0);
    }
}
