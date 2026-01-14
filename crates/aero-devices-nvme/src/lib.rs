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
//! - Admin commands: IDENTIFY, CREATE/DELETE IO SQ/CQ, GET/SET FEATURES
//! - NVM commands: READ, WRITE, FLUSH, WRITE ZEROES, DSM (deallocate)
//! - PRP (PRP1/PRP2 + PRP lists)
//! - Limited SGL support for data transfers (Data Block + Segment/Last Segment chaining)
//!
//! ## Best-effort semantics
//!
//! - `WRITE ZEROES` materializes and writes a zero-filled buffer (bounded by
//!   [`NVME_MAX_DMA_BYTES`]) so reads of the range return zeros.
//! - `DSM deallocate` validates the range list and best-effort forwards discard/TRIM requests to the
//!   backend (via [`DiskBackend::discard_sectors`]). Backends that cannot reclaim storage may treat
//!   discard as a no-op success.
//!
//! Interrupts:
//! - Legacy INTx is modelled via [`NvmeController::intx_level`].
//! - [`NvmePciDevice`] exposes MSI and MSI-X PCI capabilities:
//!   - When MSI is enabled and the platform attaches an [`aero_platform::interrupts::msi::MsiTrigger`]
//!     sink, NVMe completions trigger MSI deliveries instead of asserting INTx.
//!   - MSI-X exposes a BAR0-backed MSI-X table/PBA region (currently a single vector). The device
//!     model routes guest BAR0 MMIO accesses to the table/PBA so guests can configure vectors.
//!     When MSI-X is enabled and the platform attaches an [`aero_platform::interrupts::msi::MsiTrigger`]
//!     sink, NVMe completions trigger MSI-X (vector 0) deliveries instead of asserting INTx.

use std::collections::{BTreeMap, HashMap};

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::{
    profile, MsiCapability, MsixCapability, PciBarDefinition, PciConfigSpace, PciConfigSpaceState,
    PciDevice,
};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_io_snapshot::io::storage::state::{
    NvmeCompletionQueueState, NvmeControllerState, NvmeSubmissionQueueState,
};
use aero_platform::interrupts::msi::MsiTrigger;
use aero_storage::DiskError as StorageDiskError;
/// Adapter allowing [`aero_storage::VirtualDisk`] implementations (e.g. `RawDisk`,
/// `AeroSparseDisk`, `BlockCachedDisk`) to be used as an NVMe [`DiskBackend`].
///
/// NVMe is currently hard-coded to 512-byte sectors, and capacity is reported to the guest in
/// whole 512-byte LBAs via [`DiskBackend::total_sectors`].
///
/// Prefer constructing backends via [`from_virtual_disk`] (or
/// [`NvmeController::try_new_from_virtual_disk`]), which reject disks whose byte capacity is not a
/// multiple of 512.
pub use aero_storage_adapters::AeroVirtualDiskAsNvmeBackend as AeroStorageDiskAdapter;
use memory::{MemoryBus, MmioHandler};

mod aero_storage_adapter;
pub use aero_storage_adapter::NvmeDiskFromAeroStorage;

mod nvme_as_aero_storage;
/// Reverse adapter for layering `aero-storage` disk wrappers (cache/sparse/overlay/etc) on top of
/// an existing NVMe [`DiskBackend`] implementation.
///
/// This is the inverse of [`from_virtual_disk`] / [`NvmeDiskFromAeroStorage`].
pub use nvme_as_aero_storage::NvmeBackendAsAeroVirtualDisk;

const PAGE_SIZE: usize = 4096;
const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;
// DoS guard: cap per-request DMA buffers. This should match the MDTS value we advertise in
// Identify Controller (4MiB for 4KiB pages).
const NVME_MAX_DMA_BYTES: usize = 4 * 1024 * 1024;
// DoS guard: cap the number of SGL descriptors processed for a single command.
//
// The maximum transfer is 4MiB; even with highly fragmented 512-byte segments this would be ~8192
// descriptors, so 16384 provides headroom while still bounding worst-case work.
const NVME_MAX_SGL_DESCRIPTORS: usize = 16 * 1024;
// Maximum number of entries per submission/completion queue supported by this controller.
//
// This must match CAP.MQES (0-based), which we currently hard-code to 128 entries.
const NVME_MAX_QUEUE_ENTRIES: u16 = 128;
// DoS guard: cap the number of guest-created I/O queues. Otherwise a malicious guest can create
// thousands of queues and force O(n) scans in interrupt paths and snapshot serialization.
const NVME_MAX_IO_QUEUES: usize = 256;
// DoS guard: cap pending doorbell updates. Guests can write doorbells for arbitrary QIDs within
// the mapped BAR0 window; without a cap this could grow `pending_sq_tail` without bound between
// processing ticks.
const NVME_MAX_PENDING_SQ_TAIL_UPDATES: usize = NVME_MAX_IO_QUEUES + 1; // + admin SQ0

// -----------------------------------------------------------------------------
// MSI-X configuration (BAR0-backed MSI-X table + PBA)
// -----------------------------------------------------------------------------
//
// We expose a minimal MSI-X capability so guests can bind modern NVMe drivers that prefer MSI-X
// over legacy INTx. The model delivers interrupts through MSI-X vector 0 when MSI-X is enabled and
// the platform provides an MSI sink, while still exposing the BAR-backed table/PBA for guest
// programming.
//
// Layout:
// - Table: BAR0 + 0x3000, one 16-byte entry (vector 0)
// - PBA: immediately following the table, 8-byte aligned
pub const NVME_MSIX_TABLE_SIZE: u16 = profile::NVME_MSIX_TABLE_SIZE;
pub const NVME_MSIX_TABLE_BAR: u8 = profile::NVME_MSIX_TABLE_BAR;
pub const NVME_MSIX_TABLE_OFFSET: u32 = profile::NVME_MSIX_TABLE_OFFSET;
pub const NVME_MSIX_PBA_BAR: u8 = profile::NVME_MSIX_PBA_BAR;
pub const NVME_MSIX_PBA_OFFSET: u32 = profile::NVME_MSIX_PBA_OFFSET;

#[deprecated(
    note = "NVMe MSI-X capability is now included in the canonical PCI profile (aero_devices::pci::profile::NVME_CONTROLLER). \
Callers should build config space from the profile instead of manually adding MSI-X."
)]
pub fn add_nvme_msix_capability(config: &mut PciConfigSpace) {
    // Avoid accidentally inserting multiple MSI-X capabilities if callers already built config
    // space from the canonical profile.
    if config.capability::<MsixCapability>().is_some() {
        return;
    }

    let table_size = NVME_MSIX_TABLE_SIZE;
    let table_offset = NVME_MSIX_TABLE_OFFSET;
    let pba_offset = NVME_MSIX_PBA_OFFSET;

    // Keep the layout within the fixed BAR0 window.
    debug_assert!(u64::from(pba_offset) < NvmeController::bar0_len());

    config.add_capability(Box::new(MsixCapability::new(
        table_size,
        NVME_MSIX_TABLE_BAR,
        table_offset,
        NVME_MSIX_PBA_BAR,
        pba_offset,
    )));
}

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

/// Block storage abstraction used by the NVMe controller model.
///
/// # Canonical trait note
///
/// The repo-wide canonical synchronous disk trait is [`aero_storage::VirtualDisk`]. This NVMe
/// crate keeps its own `DiskBackend` trait for now (custom error type + explicit sector methods),
/// but most call sites should construct an NVMe backend from an `aero_storage` disk via
/// [`from_virtual_disk`] / [`NvmeController::try_new_from_virtual_disk`].
///
/// If you need the reverse direction (treating an existing NVMe backend as an `aero-storage`
/// [`aero_storage::VirtualDisk`] so you can layer disk wrappers on top), use
/// [`NvmeBackendAsAeroVirtualDisk`].
///
/// See `docs/20-storage-trait-consolidation.md`.
#[cfg(not(target_arch = "wasm32"))]
pub trait DiskBackend: Send {
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()>;
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;

    /// Best-effort deallocation (discard/TRIM) of the given LBA range.
    ///
    /// Backends that cannot reclaim storage may implement this as a no-op success.
    fn discard_sectors(&mut self, _lba: u64, _sectors: u64) -> DiskResult<()> {
        Ok(())
    }
}

/// wasm32 build: disk backends do not need to be `Send`.
///
/// Browser disk handles (OPFS, JS-backed sparse formats, etc.) are often `!Send` because they wrap
/// JS objects. Keeping this trait `!Send` allows the full-system wasm build to compile while still
/// preventing accidental cross-thread use of non-`Send` backends.
#[cfg(target_arch = "wasm32")]
pub trait DiskBackend {
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()>;
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()>;
    fn flush(&mut self) -> DiskResult<()>;

    fn discard_sectors(&mut self, _lba: u64, _sectors: u64) -> DiskResult<()> {
        Ok(())
    }
}

fn map_storage_error_to_nvme(err: StorageDiskError) -> DiskError {
    // Keep this exhaustive so that adding new variants to `aero_storage::DiskError` forces disk
    // adapter call sites to consider how those errors should be surfaced at the NVMe layer.
    match err {
        StorageDiskError::UnalignedLength { .. }
        | StorageDiskError::OutOfBounds { .. }
        | StorageDiskError::OffsetOverflow
        | StorageDiskError::CorruptImage(_)
        | StorageDiskError::Unsupported(_)
        | StorageDiskError::InvalidSparseHeader(_)
        | StorageDiskError::InvalidConfig(_)
        | StorageDiskError::CorruptSparseImage(_)
        | StorageDiskError::NotSupported(_)
        | StorageDiskError::QuotaExceeded
        | StorageDiskError::InUse
        | StorageDiskError::InvalidState(_)
        | StorageDiskError::BackendUnavailable
        | StorageDiskError::Io(_) => DiskError::Io,
    }
}

impl DiskBackend for AeroStorageDiskAdapter {
    fn sector_size(&self) -> u32 {
        AeroStorageDiskAdapter::SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        // If the disk capacity is not a multiple of 512, expose the largest full-sector span.
        //
        // Note: [`from_virtual_disk`] rejects such disks; this truncation is only relevant when
        // constructing the adapter directly.
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
        self.disk_mut()
            .read_at(offset, buffer)
            .map_err(map_storage_error_to_nvme)
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
            .map_err(map_storage_error_to_nvme)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.disk_mut().flush().map_err(map_storage_error_to_nvme)
    }

    fn discard_sectors(&mut self, lba: u64, sectors: u64) -> DiskResult<()> {
        if sectors == 0 {
            return Ok(());
        }

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

        let sector_size = u64::from(self.sector_size());
        let offset = lba.checked_mul(sector_size).ok_or(DiskError::Io)?;
        let len = sectors.checked_mul(sector_size).ok_or(DiskError::Io)?;

        self.disk_mut()
            .discard_range(offset, len)
            .map_err(map_storage_error_to_nvme)
    }
}

/// Convenience helper to wrap an [`aero_storage::VirtualDisk`] as an NVMe [`DiskBackend`].
///
/// Returns `Err(DiskError::Io)` if the disk capacity is not a multiple of 512 bytes (NVMe LBAs are
/// currently fixed at 512 bytes in this device model).
#[cfg(not(target_arch = "wasm32"))]
pub fn from_virtual_disk(
    d: Box<dyn aero_storage::VirtualDisk + Send>,
) -> DiskResult<Box<dyn DiskBackend>> {
    // NVMe currently reports capacity in whole 512-byte LBAs.
    // Reject disks that cannot be represented losslessly.
    if !d
        .capacity_bytes()
        .is_multiple_of(u64::from(AeroStorageDiskAdapter::SECTOR_SIZE))
    {
        return Err(DiskError::Io);
    }
    Ok(Box::new(AeroStorageDiskAdapter::new(d)))
}

#[cfg(target_arch = "wasm32")]
pub fn from_virtual_disk(
    d: Box<dyn aero_storage::VirtualDisk>,
) -> DiskResult<Box<dyn DiskBackend>> {
    if !d
        .capacity_bytes()
        .is_multiple_of(u64::from(AeroStorageDiskAdapter::SECTOR_SIZE))
    {
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

    // --- Admin command feature state ---
    // The NVMe spec defines these as 0-based queue counts: 0 means 1 queue.
    feature_num_io_sqs: u16,
    feature_num_io_cqs: u16,
    feature_interrupt_coalescing: u16,
    feature_volatile_write_cache: bool,

    admin_sq: Option<SubmissionQueue>,
    admin_cq: Option<CompletionQueue>,
    io_sqs: HashMap<u16, SubmissionQueue>,
    io_cqs: HashMap<u16, CompletionQueue>,

    /// Submission queue doorbell writes that have not yet been processed.
    ///
    /// MMIO handlers are not allowed to access guest memory (DMA), so we defer SQ processing to an
    /// explicit [`NvmeController::process`] step performed by the platform.
    pending_sq_tail: BTreeMap<u16, u16>,

    /// Legacy INTx derived level (asserted = true).
    ///
    /// This controller model only derives the legacy level from queue state; the PCI wrapper
    /// (`NvmePciDevice`) may translate this into edge-triggered MSI deliveries when MSI is enabled.
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

        // Default to advertising/supporting the full bounded IO queue count. Real guests typically
        // issue SET FEATURES (Number of Queues) during init, but keeping the default generous
        // improves compatibility with simpler drivers and existing unit tests.
        let max_io_queues_0based = (NVME_MAX_IO_QUEUES as u16).saturating_sub(1);

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
            feature_num_io_sqs: max_io_queues_0based,
            feature_num_io_cqs: max_io_queues_0based,
            feature_interrupt_coalescing: 0,
            feature_volatile_write_cache: false,
            admin_sq: None,
            admin_cq: None,
            io_sqs: HashMap::new(),
            io_cqs: HashMap::new(),
            pending_sq_tail: BTreeMap::new(),
            intx_level: false,
        }
    }

    pub fn reset(&mut self) {
        // Preserve the attached disk backend while restoring the controller register space and
        // runtime queues back to their power-on defaults.
        let mqes: u64 = 127; // 128 entries max per queue, expressed as 0-based.
        let dstrd: u64 = 0; // 4-byte doorbell stride.
        let mpsmin: u64 = 0; // 2^(12 + 0) = 4KiB.
        let mpsmax: u64 = 0;
        let css_nvm: u64 = 1; // NVM command set supported.

        self.cap =
            (mqes & 0xffff) | (css_nvm << 37) | (mpsmin << 48) | (mpsmax << 52) | (dstrd << 32);
        self.vs = 0x0001_0400; // NVMe 1.4.0
        self.intms = 0;
        self.cc = 0;
        self.csts = 0;
        self.aqa = 0;
        self.asq = 0;
        self.acq = 0;

        let max_io_queues_0based = (NVME_MAX_IO_QUEUES as u16).saturating_sub(1);
        self.feature_num_io_sqs = max_io_queues_0based;
        self.feature_num_io_cqs = max_io_queues_0based;
        self.feature_interrupt_coalescing = 0;
        self.feature_volatile_write_cache = false;

        self.admin_sq = None;
        self.admin_cq = None;
        self.io_sqs.clear();
        self.io_cqs.clear();
        self.pending_sq_tail.clear();
        self.intx_level = false;
    }

    /// Construct an NVMe controller from an [`aero_storage::VirtualDisk`].
    ///
    /// This is a convenience wrapper around [`from_virtual_disk`] that returns an error if the
    /// disk capacity is not a multiple of 512 bytes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new_from_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    /// Like [`NvmeController::try_new_from_virtual_disk`], but accepts any concrete
    /// `aero_storage` disk type.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn try_new_from_virtual_disk(disk: Box<dyn aero_storage::VirtualDisk>) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    pub const fn bar0_len() -> u64 {
        // Registers (0x0..0x1000) + doorbells + a small region reserved for PCI-level structures
        // (e.g. MSI-X table/PBA).
        profile::NVME_BAR0_SIZE
    }

    pub fn mmio_read(&self, offset: u64, size: usize) -> u64 {
        // `PhysicalMemoryBus` issues naturally-aligned MMIO reads in sizes 1/2/4/8. Guests may also
        // access 64-bit registers via two 32-bit operations (e.g. CAP, ASQ, ACQ).
        //
        // Implement reads by slicing bytes out of the containing 32-bit word.
        if size == 0 {
            return 0;
        }
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
        if size == 0 {
            return;
        }
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
        // Validate CC.MPS (Memory Page Size) against CAP.MPSMIN/MPSMAX.
        //
        // This device model currently supports a single page size: 4KiB (MPS=0). CAP advertises
        // this by setting both MPSMIN and MPSMAX to 0.
        //
        // Rejecting unsupported page sizes avoids panic paths later when the controller decodes
        // PRPs/queue base addresses using the assumed page size.
        let mps = (self.cc >> 7) & 0xf;
        let mpsmin = ((self.cap >> 48) & 0xf) as u32;
        let mpsmax = ((self.cap >> 52) & 0xf) as u32;
        if mps < mpsmin || mps > mpsmax {
            // Controller Fatal Status (CSTS.CFS). RDY must remain clear.
            self.csts = 1 << 1;
            return;
        }

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
        if asqs > NVME_MAX_QUEUE_ENTRIES || acqs > NVME_MAX_QUEUE_ENTRIES {
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
        // Avoid unbounded growth of the pending doorbell map. This is a per-tick work queue; real
        // guests keep it small.
        //
        // Note: we always allow updating an existing entry. If the map is full and the guest rings
        // a doorbell for a brand new QID, we drop it to keep memory usage bounded.
        //
        // SQ0 (admin) is special: it must always be accepted to avoid stalling admin processing. If
        // the map is full and SQ0 is not yet present, evict one other entry to make room so the
        // overall map size stays capped.
        if let std::collections::btree_map::Entry::Occupied(mut entry) =
            self.pending_sq_tail.entry(qid)
        {
            entry.insert(val);
            return;
        }

        if self.pending_sq_tail.len() >= NVME_MAX_PENDING_SQ_TAIL_UPDATES {
            if qid == 0 {
                // Evict the largest QID (arbitrary but deterministic) to make room for SQ0.
                let _ = self.pending_sq_tail.pop_last();
            } else {
                return;
            }
        }

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
        match cmd.opc {
            0x00 => self.cmd_delete_io_sq(cmd),
            0x04 => self.cmd_delete_io_cq(cmd),
            0x09 => self.cmd_set_features(cmd),
            0x0a => self.cmd_get_features(cmd),
            0x06 => {
                // IDENTIFY transfers data. Some guests use SGL even for admin commands, so accept
                // both PRP and SGL data pointer formats here.
                if cmd.psdt > 1 {
                    return (NvmeStatus::INVALID_FIELD, 0);
                }
                self.cmd_identify(cmd, memory)
            }
            0x05 => self.cmd_create_io_cq(cmd),
            0x01 => self.cmd_create_io_sq(cmd),
            _ => (NvmeStatus::INVALID_OPCODE, 0),
        }
    }

    fn execute_io(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        // Support both PRP (PSDT=0) and SGL (PSDT=1) for data transfer commands.
        if cmd.psdt > 1 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        match cmd.opc {
            0x00 => self.cmd_flush(),
            0x01 => self.cmd_write(cmd, memory),
            0x02 => self.cmd_read(cmd, memory),
            0x08 => self.cmd_write_zeroes(cmd),
            0x09 => self.cmd_dataset_management(cmd, memory),
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

        let status = self.dma_write(memory, cmd.psdt, cmd.prp1, cmd.prp2, &data);
        (status, 0)
    }

    fn cmd_create_io_cq(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let max_qid = self.feature_num_io_cqs.saturating_add(1);
        if qid > max_qid {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        if self.io_cqs.contains_key(&qid) {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        if self.io_cqs.len() >= NVME_MAX_IO_QUEUES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        // The CQ size field is 0-based; use wrapping arithmetic so guests can't trigger a panic
        // under overflow-check builds by passing 0xFFFF (which would encode 65536 entries).
        let qsize = (((cmd.cdw10 >> 16) & 0xffff) as u16).wrapping_add(1);
        if qsize == 0 || qsize > NVME_MAX_QUEUE_ENTRIES {
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
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let max_qid = self.feature_num_io_sqs.saturating_add(1);
        if qid > max_qid {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        if self.io_sqs.contains_key(&qid) {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        if self.io_sqs.len() >= NVME_MAX_IO_QUEUES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        // The SQ size field is 0-based; use wrapping arithmetic so guests can't trigger a panic
        // under overflow-check builds by passing 0xFFFF (which would encode 65536 entries).
        let qsize = (((cmd.cdw10 >> 16) & 0xffff) as u16).wrapping_add(1);
        if qsize == 0 || qsize > NVME_MAX_QUEUE_ENTRIES {
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

    fn cmd_delete_io_sq(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_QID, 0);
        }
        if self.io_sqs.remove(&qid).is_none() {
            return (NvmeStatus::INVALID_QID, 0);
        }
        // Clear any pending doorbell update so recreating the queue can't accidentally pick up an
        // old tail value.
        self.pending_sq_tail.remove(&qid);
        // Keep derived interrupt level coherent after any queue topology changes.
        self.refresh_intx_level();
        (NvmeStatus::SUCCESS, 0)
    }

    fn cmd_delete_io_cq(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let qid = (cmd.cdw10 & 0xffff) as u16;
        if qid == 0 {
            return (NvmeStatus::INVALID_QID, 0);
        }
        if !self.io_cqs.contains_key(&qid) {
            return (NvmeStatus::INVALID_QID, 0);
        }
        // NVMe requires SQs to be deleted before the CQ they target.
        if self.io_sqs.values().any(|sq| sq.cqid == qid) {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        self.io_cqs.remove(&qid);
        self.refresh_intx_level();
        (NvmeStatus::SUCCESS, 0)
    }

    fn cmd_get_features(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let fid = (cmd.cdw10 & 0xff) as u8;
        let sel = ((cmd.cdw10 >> 8) & 0x7) as u8;

        match fid {
            // Number of Queues:
            // - SEL=0/1: current/default, 0-based values (NSQR in [31:16], NCQR in [15:0]).
            // - SEL=3: supported capabilities (max supported queues, also 0-based).
            //
            // Other SEL values are not implemented.
            0x07 if sel == 3 => {
                let max = (NVME_MAX_IO_QUEUES as u16).saturating_sub(1);
                (NvmeStatus::SUCCESS, (u32::from(max) << 16) | u32::from(max))
            }
            // Number of Queues: 0-based values (NSQR in [31:16], NCQR in [15:0]).
            0x07 if sel <= 1 => (
                NvmeStatus::SUCCESS,
                (u32::from(self.feature_num_io_sqs) << 16) | u32::from(self.feature_num_io_cqs),
            ),
            // Interrupt Coalescing: return the raw 16-bit value in DW0.
            0x08 if sel <= 1 => (NvmeStatus::SUCCESS, u32::from(self.feature_interrupt_coalescing)),
            // Volatile Write Cache: bit 0.
            0x06 if sel <= 1 => (
                NvmeStatus::SUCCESS,
                u32::from(self.feature_volatile_write_cache as u8),
            ),
            _ => (NvmeStatus::INVALID_FIELD, 0),
        }
    }

    fn cmd_set_features(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        if cmd.psdt != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let fid = (cmd.cdw10 & 0xff) as u8;
        match fid {
            // Number of Queues: request in CDW11, 0-based (value 0 means 1 queue).
            0x07 => {
                let req_cqs = (cmd.cdw11 & 0xffff) as u32 + 1;
                let req_sqs = ((cmd.cdw11 >> 16) & 0xffff) as u32 + 1;
                let max = NVME_MAX_IO_QUEUES as u32;
                let alloc_cqs = req_cqs.min(max);
                let alloc_sqs = req_sqs.min(max);

                // Reject attempts to shrink below the highest existing queue ID. Guests should tear
                // down queues before requesting fewer resources.
                let required_cqs = self.io_cqs.keys().copied().max().unwrap_or(0) as u32;
                let required_sqs = self.io_sqs.keys().copied().max().unwrap_or(0) as u32;
                if alloc_cqs < required_cqs || alloc_sqs < required_sqs {
                    return (NvmeStatus::INVALID_FIELD, 0);
                }

                self.feature_num_io_cqs = (alloc_cqs - 1) as u16;
                self.feature_num_io_sqs = (alloc_sqs - 1) as u16;
                let result =
                    (u32::from(self.feature_num_io_sqs) << 16) | u32::from(self.feature_num_io_cqs);
                (NvmeStatus::SUCCESS, result)
            }
            // Interrupt Coalescing: store raw value (DW11 bits 15:0).
            0x08 => {
                self.feature_interrupt_coalescing = (cmd.cdw11 & 0xffff) as u16;
                (
                    NvmeStatus::SUCCESS,
                    u32::from(self.feature_interrupt_coalescing),
                )
            }
            // Volatile Write Cache: enable flag in DW11 bit 0.
            0x06 => {
                self.feature_volatile_write_cache = (cmd.cdw11 & 1) != 0;
                (
                    NvmeStatus::SUCCESS,
                    u32::from(self.feature_volatile_write_cache as u8),
                )
            }
            _ => (NvmeStatus::INVALID_FIELD, 0),
        }
    }

    fn cmd_read(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u64;
        let sectors = nlb + 1;
        let sector_size = self.disk.sector_size() as u64;
        if sector_size == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        if slba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
        }

        let Some(len_u64) = sectors.checked_mul(sector_size) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        let Ok(len) = usize::try_from(len_u64) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        if len > NVME_MAX_DMA_BYTES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let mut data = Vec::new();
        if data.try_reserve_exact(len).is_err() {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        data.resize(len, 0);
        let status = match self.disk.read_sectors(slba, &mut data) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        if status != NvmeStatus::SUCCESS {
            return (status, 0);
        }

        let status = self.dma_write(memory, cmd.psdt, cmd.prp1, cmd.prp2, &data);
        (status, 0)
    }

    fn cmd_write(&mut self, cmd: NvmeCommand, memory: &mut dyn MemoryBus) -> (NvmeStatus, u32) {
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u64;
        let sectors = nlb + 1;
        let sector_size = self.disk.sector_size() as u64;
        if sector_size == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        if slba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
        }

        let Some(len_u64) = sectors.checked_mul(sector_size) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        let Ok(len) = usize::try_from(len_u64) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        if len > NVME_MAX_DMA_BYTES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let mut data = Vec::new();
        if data.try_reserve_exact(len).is_err() {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        data.resize(len, 0);
        let status = self.dma_read(memory, cmd.psdt, cmd.prp1, cmd.prp2, &mut data);
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

    fn cmd_write_zeroes(&mut self, cmd: NvmeCommand) -> (NvmeStatus, u32) {
        // Best-effort implementation: materialize a zero-filled buffer (bounded by
        // `NVME_MAX_DMA_BYTES`) and issue a normal backend write. This ensures guests observe zeros
        // when they read the range back even if the backend has no fast "write zeroes" primitive.
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        let slba = (cmd.cdw11 as u64) << 32 | cmd.cdw10 as u64;
        let nlb = (cmd.cdw12 & 0xffff) as u64;
        let sectors = nlb + 1;
        let sector_size = self.disk.sector_size() as u64;
        if sector_size == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        if slba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.total_sectors())
        {
            return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
        }

        let Some(len_u64) = sectors.checked_mul(sector_size) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        let Ok(len) = usize::try_from(len_u64) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        // DoS guard: cap total zeroed bytes per command to the advertised MDTS-equivalent limit.
        if len > NVME_MAX_DMA_BYTES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let mut zeros = Vec::new();
        if zeros.try_reserve_exact(len).is_err() {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        zeros.resize(len, 0);

        let status = match self.disk.write_sectors(slba, &zeros) {
            Ok(()) => NvmeStatus::SUCCESS,
            Err(_) => NvmeStatus::INVALID_FIELD,
        };
        (status, 0)
    }

    fn cmd_dataset_management(
        &mut self,
        cmd: NvmeCommand,
        memory: &mut dyn MemoryBus,
    ) -> (NvmeStatus, u32) {
        if cmd.nsid != 1 {
            return (NvmeStatus::INVALID_NS, 0);
        }

        // Best-effort implementation: support the Deallocate attribute, validate the DSM range
        // list, and attempt to forward discard/TRIM requests to the backend (bounded by
        // `NVME_MAX_DMA_BYTES`).
        //
        // Note: NVMe deallocate is advisory. Backends that cannot reclaim storage may implement
        // discard as a no-op success; failures are ignored after validation.

        const DSM_ATTR_INTEGRAL_DATASET_FOR_READ: u32 = 1 << 0;
        const DSM_ATTR_INTEGRAL_DATASET_FOR_WRITE: u32 = 1 << 1;
        const DSM_ATTR_DEALLOCATE: u32 = 1 << 2;
        const DSM_ATTR_KNOWN_MASK: u32 = DSM_ATTR_INTEGRAL_DATASET_FOR_READ
            | DSM_ATTR_INTEGRAL_DATASET_FOR_WRITE
            | DSM_ATTR_DEALLOCATE;

        if cmd.cdw11 & !DSM_ATTR_KNOWN_MASK != 0 {
            // Reject unknown attributes.
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        // CDW10[7:0] contains the 0-based number of ranges (NR). Higher bits are reserved.
        if cmd.cdw10 & !0xff != 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        // If the command does not request Deallocate, treat it as a no-op success. This matches
        // the intent of the "integral dataset" hint bits and improves guest compatibility.
        if cmd.cdw11 & DSM_ATTR_DEALLOCATE == 0 {
            return (NvmeStatus::SUCCESS, 0);
        }

        let ranges = (cmd.cdw10 & 0xff) + 1;

        // DSM range definition entries are 16 bytes each.
        let Some(list_bytes_u64) = u64::from(ranges).checked_mul(16) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        let Ok(list_bytes) = usize::try_from(list_bytes_u64) else {
            return (NvmeStatus::INVALID_FIELD, 0);
        };
        if list_bytes > NVME_MAX_DMA_BYTES {
            return (NvmeStatus::INVALID_FIELD, 0);
        }

        let mut ranges_buf = Vec::new();
        if ranges_buf.try_reserve_exact(list_bytes).is_err() {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        ranges_buf.resize(list_bytes, 0);
        let status = self.dma_read(memory, cmd.psdt, cmd.prp1, cmd.prp2, &mut ranges_buf);
        if status != NvmeStatus::SUCCESS {
            return (status, 0);
        }

        let sector_size = self.disk.sector_size() as u64;
        if sector_size == 0 {
            return (NvmeStatus::INVALID_FIELD, 0);
        }
        let capacity = self.disk.total_sectors();
        let mut parsed: Vec<(u64, u64)> = Vec::with_capacity(ranges as usize);
        let mut total_bytes: u64 = 0;
        for i in 0..ranges {
            let off = (i as usize) * 16;
            // DSM range definition entry:
            // - CDW0: Context Attributes (ignored)
            // - CDW1: NLB (0-based)
            // - CDW2-3: SLBA
            let nlb = u32::from_le_bytes(ranges_buf[off + 4..off + 8].try_into().unwrap());
            let slba = u64::from_le_bytes(ranges_buf[off + 8..off + 16].try_into().unwrap());

            let Some(sectors) = (nlb as u64).checked_add(1) else {
                return (NvmeStatus::INVALID_FIELD, 0);
            };
            if slba
                .checked_add(sectors)
                .is_none_or(|end| end > capacity)
            {
                return (NvmeStatus::LBA_OUT_OF_RANGE, 0);
            }

            let Some(bytes) = sectors.checked_mul(sector_size) else {
                return (NvmeStatus::INVALID_FIELD, 0);
            };
            total_bytes = match total_bytes.checked_add(bytes) {
                Some(v) => v,
                None => return (NvmeStatus::INVALID_FIELD, 0),
            };
            if total_bytes > NVME_MAX_DMA_BYTES as u64 {
                return (NvmeStatus::INVALID_FIELD, 0);
            }

            parsed.push((slba, sectors));
        }

        // Best-effort: attempt to discard/deallocate on backends that support it, but ignore
        // failures (NVMe deallocate is advisory).
        for (slba, sectors) in parsed {
            let _ = self.disk.discard_sectors(slba, sectors);
        }

        (NvmeStatus::SUCCESS, 0)
    }

    fn dma_write(
        &self,
        memory: &mut dyn MemoryBus,
        psdt: u8,
        dptr1: u64,
        dptr2: u64,
        data: &[u8],
    ) -> NvmeStatus {
        let segs = match dma_segments(memory, psdt, dptr1, dptr2, data.len()) {
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

    fn dma_read(
        &self,
        memory: &mut dyn MemoryBus,
        psdt: u8,
        dptr1: u64,
        dptr2: u64,
        data: &mut [u8],
    ) -> NvmeStatus {
        let segs = match dma_segments(memory, psdt, dptr1, dptr2, data.len()) {
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

        // ONCS (Optional NVM Command Support) at offset 520 (0x208).
        //
        // Advertise support for:
        // - Dataset Management (DSM) (bit 2)
        // - Write Zeroes (bit 3)
        let oncs: u16 = (1 << 2) | (1 << 3);
        data[520..522].copy_from_slice(&oncs.to_le_bytes());

        // MDTS (Maximum Data Transfer Size) at offset 77 (0x4d).
        // Max transfer = 2^MDTS * min page size (4KiB for this device).
        data[77] = 10; // 4MiB

        // SQES/CQES at offset 512 (0x200) for OS queue entry size negotiation.
        data[512] = 0x66; // SQE min/max = 2^6 = 64 bytes
        data[513] = 0x44; // CQE min/max = 2^4 = 16 bytes

        // VWC (Volatile Write Cache) at offset 525 (0x20d): advertise support so guests that
        // probe the feature via GET/SET FEATURES (FID=0x06) behave as expected.
        data[525] = 0x1;

        // SGLS (SGL Support) at offset 536 (0x218).
        //
        // NVMe 1.4: bit 0 = Data Block, bit 2 = Segment, bit 3 = Last Segment.
        // (Keyed SGLs / bit-bucket descriptors are not implemented.)
        let sgls: u32 = (1 << 0) | (1 << 2) | (1 << 3);
        data[536..540].copy_from_slice(&sgls.to_le_bytes());

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

        // NSFEAT at offset 24 (0x18).
        //
        // Advertise thin provisioning so guests can issue DSM/TRIM (deallocate) commands. Reads
        // after deallocate are not required to return deterministic zeros (DLFEAT remains 0).
        data[24] = 1 << 0;

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
            feature_num_io_sqs: self.feature_num_io_sqs,
            feature_num_io_cqs: self.feature_num_io_cqs,
            feature_interrupt_coalescing: self.feature_interrupt_coalescing,
            feature_volatile_write_cache: self.feature_volatile_write_cache,
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

        // Snapshots may be loaded from untrusted sources. Keep controller restore bounded by
        // rejecting snapshots that contain absurd numbers of IO queues (even if the underlying
        // `NvmeControllerState` decoder allows them for compatibility with other implementations).
        if state.io_sqs.len() > NVME_MAX_IO_QUEUES || state.io_cqs.len() > NVME_MAX_IO_QUEUES {
            return Err(SnapshotError::InvalidFieldEncoding("nvme io queue count"));
        }

        let max_io_queues_0based = (NVME_MAX_IO_QUEUES as u16).saturating_sub(1);
        if state.feature_num_io_sqs > max_io_queues_0based || state.feature_num_io_cqs > max_io_queues_0based {
            return Err(SnapshotError::InvalidFieldEncoding(
                "nvme feature num queues",
            ));
        }
        if state.csts & 1 != 0 && state.cc & 1 == 0 {
            return Err(SnapshotError::InvalidFieldEncoding(
                "nvme cc disabled but csts.rdy set",
            ));
        }
        if state.csts & 1 != 0 && (state.admin_sq.is_none() || state.admin_cq.is_none()) {
            return Err(SnapshotError::InvalidFieldEncoding(
                "nvme missing admin queues while enabled",
            ));
        }

        fn validate_queue_base(base: u64) -> SnapshotResult<()> {
            if base == 0 || base & (PAGE_SIZE as u64 - 1) != 0 {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "nvme queue base address",
                ));
            }
            Ok(())
        }

        fn validate_sq(sq: &NvmeSubmissionQueueState) -> SnapshotResult<()> {
            if sq.size == 0 || sq.size > NVME_MAX_QUEUE_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding("nvme sq size"));
            }
            if sq.head >= sq.size || sq.tail >= sq.size {
                return Err(SnapshotError::InvalidFieldEncoding("nvme sq head/tail"));
            }
            validate_queue_base(sq.base)?;
            Ok(())
        }

        fn validate_cq(cq: &NvmeCompletionQueueState) -> SnapshotResult<()> {
            if cq.size == 0 || cq.size > NVME_MAX_QUEUE_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding("nvme cq size"));
            }
            if cq.head >= cq.size || cq.tail >= cq.size {
                return Err(SnapshotError::InvalidFieldEncoding("nvme cq head/tail"));
            }
            validate_queue_base(cq.base)?;
            Ok(())
        }

        if let Some(ref sq) = state.admin_sq {
            if sq.qid != 0 || sq.cqid != 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme admin sq qid"));
            }
            validate_sq(sq)?;
        }
        if let Some(ref cq) = state.admin_cq {
            if cq.qid != 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme admin cq qid"));
            }
            validate_cq(cq)?;
        }

        let mut io_cq_qids = std::collections::HashSet::new();
        for cq in &state.io_cqs {
            if cq.qid == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io cq qid"));
            }
            if cq.qid > max_io_queues_0based.saturating_add(1) {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io cq qid"));
            }
            if !io_cq_qids.insert(cq.qid) {
                return Err(SnapshotError::InvalidFieldEncoding("nvme duplicate io cq"));
            }
            validate_cq(cq)?;
        }

        let mut io_sq_qids = std::collections::HashSet::new();
        for sq in &state.io_sqs {
            if sq.qid == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io sq qid"));
            }
            if sq.qid > max_io_queues_0based.saturating_add(1) {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io sq qid"));
            }
            if !io_sq_qids.insert(sq.qid) {
                return Err(SnapshotError::InvalidFieldEncoding("nvme duplicate io sq"));
            }
            if sq.cqid == 0 || !io_cq_qids.contains(&sq.cqid) {
                return Err(SnapshotError::InvalidFieldEncoding("nvme sq cqid"));
            }
            validate_sq(sq)?;
        }

        self.cap = state.cap;
        self.vs = state.vs;
        self.intms = state.intms;
        self.cc = state.cc;
        self.csts = state.csts;
        self.aqa = state.aqa;
        self.asq = state.asq;
        self.acq = state.acq;

        self.feature_num_io_sqs = state.feature_num_io_sqs;
        self.feature_num_io_cqs = state.feature_num_io_cqs;
        self.feature_interrupt_coalescing = state.feature_interrupt_coalescing;
        self.feature_volatile_write_cache = state.feature_volatile_write_cache;

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

        // Keep feature state coherent with the restored queue IDs. Older snapshots may not include
        // feature state (or may include values that pre-date later queue creation); bump the
        // reported limits so all restored queues remain representable.
        if let Some(max_sq_qid) = self.io_sqs.keys().copied().max() {
            self.feature_num_io_sqs = self
                .feature_num_io_sqs
                .max(max_sq_qid.saturating_sub(1));
        }
        if let Some(max_cq_qid) = self.io_cqs.keys().copied().max() {
            self.feature_num_io_cqs = self
                .feature_num_io_cqs
                .max(max_cq_qid.saturating_sub(1));
        }
        self.feature_num_io_sqs = self.feature_num_io_sqs.min(max_io_queues_0based);
        self.feature_num_io_cqs = self.feature_num_io_cqs.min(max_io_queues_0based);

        self.pending_sq_tail.clear();
        if self.csts & 1 != 0 {
            // Doorbell writes are tracked in `pending_sq_tail` so DMA processing can be deferred
            // out of the MMIO handler. The snapshot state does not include the pending doorbell
            // map; reconstruct it deterministically from the restored queue head/tail pointers so
            // a restore taken between a doorbell write and the next `process()` call can still make
            // forward progress.
            if let Some(sq) = self.admin_sq.as_ref() {
                if sq.head != sq.tail {
                    self.pending_sq_tail.insert(0, sq.tail);
                }
            }
            for (qid, sq) in &self.io_sqs {
                if sq.head != sq.tail {
                    self.pending_sq_tail.insert(*qid, sq.tail);
                }
            }
        }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SglDescriptorType {
    DataBlock,
    Segment,
    LastSegment,
}

#[derive(Debug, Clone, Copy)]
struct SglDescriptor {
    addr: u64,
    length: u32,
    dtype: SglDescriptorType,
    subtype: u8,
}

impl SglDescriptor {
    fn parse(bytes: [u8; 16]) -> Result<Self, NvmeStatus> {
        let addr = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let type_byte = bytes[15];
        let subtype = type_byte >> 4;
        // For address-based SGLs (subtype=0), NVMe reserves bytes 12..=14 and requires them to be
        // zero. Non-zero values typically indicate keyed/transport SGLs, which we do not support.
        if subtype == 0 && (bytes[12] | bytes[13] | bytes[14]) != 0 {
            return Err(NvmeStatus::INVALID_FIELD);
        }
        let dtype = match type_byte & 0x0f {
            0x0 => SglDescriptorType::DataBlock,
            0x2 => SglDescriptorType::Segment,
            0x3 => SglDescriptorType::LastSegment,
            _ => return Err(NvmeStatus::INVALID_FIELD),
        };
        Ok(SglDescriptor {
            addr,
            length,
            dtype,
            subtype,
        })
    }

    fn from_dptr(dptr1: u64, dptr2: u64) -> Result<Self, NvmeStatus> {
        // NVMe SGL descriptor: 8-byte address, 4-byte length, 3 bytes reserved, 1 byte type.
        let addr = dptr1;
        let length = (dptr2 & 0xffff_ffff) as u32;
        let type_byte = (dptr2 >> 56) as u8;
        let subtype = type_byte >> 4;
        let reserved = (dptr2 >> 32) & 0x00ff_ffff;
        if subtype == 0 && reserved != 0 {
            return Err(NvmeStatus::INVALID_FIELD);
        }
        let dtype = match type_byte & 0x0f {
            0x0 => SglDescriptorType::DataBlock,
            0x2 => SglDescriptorType::Segment,
            0x3 => SglDescriptorType::LastSegment,
            _ => return Err(NvmeStatus::INVALID_FIELD),
        };
        Ok(SglDescriptor {
            addr,
            length,
            dtype,
            subtype,
        })
    }
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

fn sgl_segments(
    memory: &mut dyn MemoryBus,
    dptr1: u64,
    dptr2: u64,
    len: usize,
) -> Result<Vec<(u64, usize)>, NvmeStatus> {
    if len == 0 {
        return Ok(Vec::new());
    }

    // Parse the inline SGL descriptor in the NVMe command's DPTR field.
    let root = SglDescriptor::from_dptr(dptr1, dptr2)?;

    let mut segs = Vec::new();
    let mut remaining = len;

    // Total SGL descriptors "seen" (including those already pushed onto the work stack).
    let mut descriptors_seen: usize = 1;
    // Use a stack so that when we expand a Segment descriptor, the newly pushed child descriptors
    // are processed before any already-enqueued sibling descriptors (preserves in-order traversal
    // even when a Segment descriptor is not the last entry of a segment list).
    let mut stack = Vec::new();
    stack.push(root);

    while let Some(desc) = stack.pop() {
        if remaining == 0 {
            break;
        }

        // Subtype 0 = address; other subtypes are not supported.
        if desc.subtype != 0 {
            return Err(NvmeStatus::INVALID_FIELD);
        }

        match desc.dtype {
            SglDescriptorType::DataBlock => {
                if desc.addr == 0 {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                let desc_len = desc.length as usize;
                if desc_len == 0 {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                let chunk = remaining.min(desc_len);
                segs.push((desc.addr, chunk));
                remaining -= chunk;
            }
            SglDescriptorType::Segment | SglDescriptorType::LastSegment => {
                if desc.addr == 0 {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                // Segment address should be 16-byte aligned (descriptor alignment).
                if desc.addr & 0xf != 0 {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                let seg_bytes = desc.length as usize;
                if seg_bytes == 0 || seg_bytes % 16 != 0 {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                let count = seg_bytes / 16;

                // Enforce a hard cap on total descriptors processed for this command.
                if descriptors_seen
                    .checked_add(count)
                    .is_none_or(|v| v > NVME_MAX_SGL_DESCRIPTORS)
                {
                    return Err(NvmeStatus::INVALID_FIELD);
                }
                descriptors_seen += count;

                // Read the segment descriptor list and push its entries so that descriptor index 0
                // is processed next.
                //
                // Use a stack (LIFO) and therefore iterate in reverse order.
                for idx in (0..count).rev() {
                    let offset = (idx as u64) * 16;
                    let addr = desc
                        .addr
                        .checked_add(offset)
                        .ok_or(NvmeStatus::INVALID_FIELD)?;
                    let mut buf = [0u8; 16];
                    memory.read_physical(addr, &mut buf);
                    let child = SglDescriptor::parse(buf)?;
                    stack.push(child);
                }
            }
        }
    }

    if remaining != 0 {
        return Err(NvmeStatus::INVALID_FIELD);
    }

    Ok(segs)
}

fn dma_segments(
    memory: &mut dyn MemoryBus,
    psdt: u8,
    dptr1: u64,
    dptr2: u64,
    len: usize,
) -> Result<Vec<(u64, usize)>, NvmeStatus> {
    if len > NVME_MAX_DMA_BYTES {
        return Err(NvmeStatus::INVALID_FIELD);
    }
    match psdt {
        0 => prp_segments(memory, dptr1, dptr2, len),
        1 => sgl_segments(memory, dptr1, dptr2, len),
        _ => Err(NvmeStatus::INVALID_FIELD),
    }
}

/// NVMe PCI device model (PCI config space + BAR0 MMIO registers).
pub struct NvmePciDevice {
    config: PciConfigSpace,
    pub controller: NvmeController,
    msi_target: Option<Box<dyn MsiTrigger>>,
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
        Self {
            config,
            controller,
            msi_target: None,
        }
    }

    /// Construct an NVMe PCI device from an [`aero_storage::VirtualDisk`].
    ///
    /// This is a convenience wrapper around [`from_virtual_disk`] that returns an error if the
    /// disk capacity is not a multiple of 512 bytes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new_from_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    /// Like [`NvmePciDevice::try_new_from_virtual_disk`], but accepts any concrete
    /// `aero_storage` disk type.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn try_new_from_virtual_disk(disk: Box<dyn aero_storage::VirtualDisk>) -> DiskResult<Self> {
        Ok(Self::new(from_virtual_disk(disk)?))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn try_new_from_aero_storage<D>(disk: D) -> DiskResult<Self>
    where
        D: aero_storage::VirtualDisk + 'static,
    {
        Self::try_new_from_virtual_disk(Box::new(disk))
    }

    pub fn irq_level(&self) -> bool {
        // If MSI/MSI-X is enabled and the platform has attached an MSI sink, suppress legacy INTx.
        if self.msi_target.is_some() {
            if self
                .config
                .capability::<MsixCapability>()
                .is_some_and(|msix| msix.enabled())
            {
                return false;
            }
            if self
                .config
                .capability::<MsiCapability>()
                .is_some_and(|msi| msi.enabled())
            {
                return false;
            }
        }

        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
        self.controller.intx_level
    }

    /// Returns whether the controller currently has an interrupt pending.
    ///
    /// This reflects the NVMe controller's own interrupt logic (completion queues + INTMS + per-CQ
    /// interrupt enable), but is **not** gated by PCI COMMAND.INTX_DISABLE. Platforms can use this
    /// to deliver MSI even when the guest has disabled legacy INTx.
    pub fn irq_pending(&self) -> bool {
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

    /// Attach or detach an MSI sink used to deliver interrupts when the guest enables MSI/MSI-X.
    pub fn set_msi_target(&mut self, target: Option<Box<dyn MsiTrigger>>) {
        self.msi_target = target;
    }

    /// Process any DMA work that was made pending by MMIO doorbell writes.
    pub fn process(&mut self, memory: &mut dyn MemoryBus) {
        // MSI-X vectors can be raised while masked; the capability records them as pending in the
        // PBA. If the guest later unmasks MSI-X (clears the function mask bit) we must re-drive any
        // pending vectors that are now deliverable.
        if let (Some(target), Some(msix)) = (
            self.msi_target.as_mut(),
            self.config.capability_mut::<MsixCapability>(),
        ) {
            msix.deliver_pending_into(target.as_mut());
        }

        // Only allow the device to DMA when PCI Bus Mastering is enabled (PCI command bit 2).
        //
        // This mirrors the behavior of other PCI DMA devices in the repo (e.g. AHCI/E1000/UHCI),
        // and ensures platforms that drive the NVMe controller via explicit `process()` calls do
        // not accidentally perform guest-memory DMA before the guest enables bus mastering during
        // PCI enumeration.
        let bus_master_enabled = (self.config.command() & (1 << 2)) != 0;
        if !bus_master_enabled {
            return;
        }
        let prev_intx = self.controller.intx_level;
        self.controller.process(memory);
        // NVMe interrupt requests are edge-triggered when delivered via MSI/MSI-X. Use the legacy
        // INTx-derived level (`NvmeController::intx_level`) as an internal "interrupt requested"
        // signal and trigger MSI/MSI-X on a rising edge (empty -> non-empty completion queue).
        //
        // For masked MSI/MSI-X vectors, the PCI capability logic latches a pending bit but does not
        // automatically re-deliver on unmask; re-trigger while the interrupt condition persists so
        // guests can observe delivery after unmask.
        if !self.controller.intx_level {
            return;
        }

        let Some(target) = self.msi_target.as_mut() else {
            return;
        };

        // Prefer MSI-X over MSI when MSI-X is enabled.
        if let Some(msix) = self.config.capability_mut::<MsixCapability>() {
            if msix.enabled() {
                // Single-vector MSI-X: table entry 0, pending bit 0.
                let pending = msix
                    .snapshot_pba()
                    .first()
                    .is_some_and(|word| (word & 1) != 0);
                if !prev_intx || pending {
                    if let Some(msg) = msix.trigger(0) {
                        target.as_mut().trigger_msi(msg);
                    }
                }
                return;
            }
        }

        if let Some(msi) = self.config.capability_mut::<MsiCapability>() {
            if msi.enabled() {
                let pending = msi.pending_bits() != 0;
                if !prev_intx || pending {
                    let _ = msi.trigger(target.as_mut());
                }
            }
        }
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

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        //
        // Also clear MSI/MSI-X enable state so platform resets start from a sane baseline.
        {
            let cfg = self.config_mut();
            cfg.set_command(0);
            cfg.disable_msi_msix();

            // Clear any pending bits latched in the MSI-X Pending Bit Array (PBA) so reset starts
            // from a deterministic interrupt state.
            if let Some(msix) = cfg.capability_mut::<MsixCapability>() {
                let zeros = vec![0u64; msix.snapshot_pba().len()];
                let _ = msix.restore_pba(&zeros);
            }
        }

        // Reset controller register/queue state while preserving the attached disk backend.
        self.controller.reset();
    }
}

impl MmioHandler for NvmePciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);
        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return all_ones(size);
        }

        // MSI-X table/PBA live in BAR0 and must be accessible independently of NVMe controller
        // register semantics (e.g. doorbells). Dispatch them before handing the access to the NVMe
        // BAR0 register model.
        if let Some(msix) = self.config.capability_mut::<MsixCapability>() {
            if msix.table_bir() == 0 {
                let base = u64::from(msix.table_offset());
                let end = base.saturating_add(msix.table_len_bytes() as u64);
                if offset >= base && offset < end {
                    let mut data = [0u8; 8];
                    msix.table_read(offset - base, &mut data[..size]);
                    let mut out = 0u64;
                    for i in 0..size {
                        out |= u64::from(data[i]) << (i * 8);
                    }
                    return out;
                }
            }
            if msix.pba_bir() == 0 {
                let base = u64::from(msix.pba_offset());
                let end = base.saturating_add(msix.pba_len_bytes() as u64);
                if offset >= base && offset < end {
                    let mut data = [0u8; 8];
                    msix.pba_read(offset - base, &mut data[..size]);
                    let mut out = 0u64;
                    for i in 0..size {
                        out |= u64::from(data[i]) << (i * 8);
                    }
                    return out;
                }
            }
        }

        self.controller.mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);
        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return;
        }

        if let Some(msix) = self.config.capability_mut::<MsixCapability>() {
            if msix.table_bir() == 0 {
                let base = u64::from(msix.table_offset());
                let end = base.saturating_add(msix.table_len_bytes() as u64);
                if offset >= base && offset < end {
                    let mut data = [0u8; 8];
                    for i in 0..size {
                        data[i] = ((value >> (i * 8)) & 0xff) as u8;
                    }
                    msix.table_write(offset - base, &data[..size]);
                    if let Some(target) = self.msi_target.as_mut() {
                        msix.deliver_pending_into(target.as_mut());
                    }
                    return;
                }
            }
            if msix.pba_bir() == 0 {
                let base = u64::from(msix.pba_offset());
                let end = base.saturating_add(msix.pba_len_bytes() as u64);
                if offset >= base && offset < end {
                    let mut data = [0u8; 8];
                    for i in 0..size {
                        data[i] = ((value >> (i * 8)) & 0xff) as u8;
                    }
                    msix.pba_write(offset - base, &data[..size]);
                    return;
                }
            }
        }

        self.controller.mmio_write(offset, size, value)
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

impl IoSnapshot for NvmePciDevice {
    const DEVICE_ID: [u8; 4] = *b"NVMP";
    // v1.2: include MSI-X table + PBA state (if present) in addition to PCI config + controller.
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 2);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;
        const TAG_MSIX_TABLE: u16 = 3;
        const TAG_MSIX_PBA: u16 = 4;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());
        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        if let Some(msix) = self.config.capability::<aero_devices::pci::MsixCapability>() {
            w.field_bytes(TAG_MSIX_TABLE, msix.snapshot_table().to_vec());

            let mut pba = Vec::with_capacity(msix.snapshot_pba().len().saturating_mul(8));
            for word in msix.snapshot_pba() {
                pba.extend_from_slice(&word.to_le_bytes());
            }
            w.field_bytes(TAG_MSIX_PBA, pba);
        }

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;
        const TAG_MSIX_TABLE: u16 = 3;
        const TAG_MSIX_PBA: u16 = 4;

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

        if r.bytes(TAG_MSIX_TABLE).is_some() || r.bytes(TAG_MSIX_PBA).is_some() {
            let Some(msix) = self
                .config
                .capability_mut::<aero_devices::pci::MsixCapability>()
            else {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "snapshot contains MSI-X state but device has no MSI-X capability",
                ));
            };

            if let Some(buf) = r.bytes(TAG_MSIX_TABLE) {
                msix.restore_table(buf)?;
            }
            if let Some(buf) = r.bytes(TAG_MSIX_PBA) {
                msix.restore_pba_bytes(buf)?;
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
    use aero_devices::pci::msi::PCI_CAP_ID_MSI;
    use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
    use aero_storage::{
        AeroSparseConfig, AeroSparseDisk, MemBackend, RawDisk, VirtualDisk as StorageVirtualDisk,
        SECTOR_SIZE,
    };
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

    #[test]
    fn pci_device_reset_disables_msi_and_msix_enable_bits() {
        let mut dev = NvmePciDevice::default();
        let msi_off = dev
            .config_mut()
            .find_capability(PCI_CAP_ID_MSI)
            .expect("NVMe device should expose an MSI capability") as u16;
        let msix_off = dev
            .config_mut()
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("NVMe device should expose an MSI-X capability") as u16;

        // Seed the MSI-X PBA with a pending bit so we can verify reset clears it.
        {
            let msix = dev.config_mut().capability_mut::<MsixCapability>().unwrap();
            msix.restore_pba(&[1]).unwrap();
            assert_eq!(msix.snapshot_pba()[0] & 1, 1);
        }

        // Enable MSI.
        {
            let cfg = dev.config_mut();
            let ctrl = cfg.read(msi_off + 0x02, 2) as u16;
            cfg.write(msi_off + 0x02, 2, u32::from(ctrl | 0x0001));
            assert!(cfg.capability::<MsiCapability>().unwrap().enabled());
        }

        // Enable MSI-X and set Function Mask.
        {
            let cfg = dev.config_mut();
            let ctrl = cfg.read(msix_off + 0x02, 2) as u16;
            cfg.write(
                msix_off + 0x02,
                2,
                u32::from(ctrl | (1 << 15) | (1 << 14)),
            );
            let msix = cfg.capability::<MsixCapability>().unwrap();
            assert!(msix.enabled());
            assert!(msix.function_masked());
        }

        // PCI bus/device reset should clear MSI/MSI-X enable bits.
        <NvmePciDevice as PciDevice>::reset(&mut dev);

        assert!(!dev.config_mut().capability::<MsiCapability>().unwrap().enabled());
        let msix = dev.config_mut().capability::<MsixCapability>().unwrap();
        assert!(!msix.enabled());
        assert!(!msix.function_masked());
        assert_eq!(msix.snapshot_pba()[0], 0, "reset should clear MSI-X PBA pending bits");
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

    #[derive(Clone)]
    struct TestSparseDisk {
        inner: Arc<Mutex<AeroSparseDisk<MemBackend>>>,
        flushed: Arc<Mutex<u32>>,
    }

    impl TestSparseDisk {
        fn new(sectors: u64) -> Self {
            let capacity_bytes = sectors * SECTOR_SIZE as u64;
            let disk = AeroSparseDisk::create(
                MemBackend::new(),
                AeroSparseConfig {
                    disk_size_bytes: capacity_bytes,
                    block_size_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            Self {
                inner: Arc::new(Mutex::new(disk)),
                flushed: Arc::new(Mutex::new(0)),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, AeroSparseDisk<MemBackend>> {
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

    impl StorageVirtualDisk for TestSparseDisk {
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
    fn enable_rejects_oversized_admin_queues() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // AQA fields are 0-based; request 129 entries for both SQ and CQ (larger than MQES=128).
        let too_large = (NVME_MAX_QUEUE_ENTRIES as u64) << 16 | (NVME_MAX_QUEUE_ENTRIES as u64);
        ctrl.mmio_write(0x0024, 4, too_large);
        ctrl.mmio_write(0x0028, 8, 0x10000);
        ctrl.mmio_write(0x0030, 8, 0x20000);
        ctrl.mmio_write(0x0014, 4, 1);

        assert_eq!(
            ctrl.mmio_read(0x001c, 4) & 1,
            0,
            "CSTS.RDY must remain clear when AQA exceeds CAP.MQES"
        );
        assert!(ctrl.admin_sq.is_none());
        assert!(ctrl.admin_cq.is_none());
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
    fn mmio_read_size0_returns_zero() {
        let disk = TestDisk::new(1024);
        let mut dev = NvmePciDevice::new(from_virtual_disk(Box::new(disk)).unwrap());

        assert_eq!(MmioHandler::read(&mut dev, 0x0000, 0), 0);
    }

    #[test]
    fn mmio_write_size0_is_noop() {
        let disk = TestDisk::new(1024);
        let mut dev = NvmePciDevice::new(from_virtual_disk(Box::new(disk)).unwrap());
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Prepare a valid enable sequence, but attempt to flip CC.EN using a size-0 write.
        MmioHandler::write(&mut dev, 0x0024, 4, 0x000f_000f); // 16/16 queues
        MmioHandler::write(&mut dev, 0x0028, 8, 0x10000);
        MmioHandler::write(&mut dev, 0x0030, 8, 0x20000);

        assert_eq!(
            MmioHandler::read(&mut dev, 0x001c, 4) & 1,
            0,
            "CSTS.RDY should start cleared"
        );

        // Regression: previously `size=0` was clamped to 1, which would set CC.EN and transition
        // CSTS.RDY to 1.
        MmioHandler::write(&mut dev, 0x0014, 0, 1); // CC (EN bit)

        assert_eq!(
            MmioHandler::read(&mut dev, 0x0014, 4) & 1,
            0,
            "CC.EN must remain unchanged"
        );
        assert_eq!(
            MmioHandler::read(&mut dev, 0x001c, 4) & 1,
            0,
            "CSTS.RDY must remain unchanged"
        );
    }

    #[test]
    fn msix_table_mmio_round_trip() {
        let disk = TestDisk::new(1024);
        let mut dev = NvmePciDevice::new(from_virtual_disk(Box::new(disk)).unwrap());
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        let (table_base, pba_base, table_bir, pba_bir) = {
            let msix = dev
                .config()
                .capability::<MsixCapability>()
                .expect("NVMe device should expose MSI-X capability");
            (
                u64::from(msix.table_offset()),
                u64::from(msix.pba_offset()),
                msix.table_bir(),
                msix.pba_bir(),
            )
        };
        assert_eq!(table_bir, 0, "MSI-X table should be BAR0-backed");
        assert_eq!(pba_bir, 0, "MSI-X PBA should be BAR0-backed");

        // Program the first MSI-X table entry using MMIO accesses and ensure reads observe the
        // written bytes.
        MmioHandler::write(&mut dev, table_base, 8, 0x1122_3344_5566_7788);
        assert_eq!(
            MmioHandler::read(&mut dev, table_base, 8),
            0x1122_3344_5566_7788
        );

        MmioHandler::write(&mut dev, table_base + 8, 4, 0xAABB_CCDD);
        assert_eq!(MmioHandler::read(&mut dev, table_base + 8, 4), 0xAABB_CCDD);

        // PBA is read-only; writes must be ignored.
        MmioHandler::write(&mut dev, pba_base, 8, u64::MAX);
        assert_eq!(MmioHandler::read(&mut dev, pba_base, 8), 0);
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
    fn identify_controller_advertises_oncs_dsm_and_write_zeroes() {
        let disk = TestDisk::new(1024);
        let ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let data = ctrl.identify_controller();
        let oncs = u16::from_le_bytes(data[520..522].try_into().unwrap());
        assert_ne!(oncs & (1 << 2), 0, "DSM (bit 2) should be advertised in ONCS");
        assert_ne!(
            oncs & (1 << 3),
            0,
            "Write Zeroes (bit 3) should be advertised in ONCS"
        );
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
    fn write_zeroes_zeros_range() {
        let disk = TestDisk::new(1024);
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

        let slba = 5u32;
        let sectors = 2u32;
        let nlb = sectors - 1;

        // Write non-zero data.
        let payload: Vec<u8> = (0..sector_size as u32 * sectors)
            .map(|v| (v.wrapping_add(1) & 0xff) as u8)
            .collect();
        mem.write_physical(write_buf, &payload);

        let mut cmd = build_command(0x01); // WRITE
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, write_buf);
        set_cdw10(&mut cmd, slba);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, nlb);
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(0x1008, 4, 1);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & !0x1, 0);

        // Write Zeroes over the same range.
        let mut cmd = build_command(0x08); // WRITE ZEROES
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_cdw10(&mut cmd, slba);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, nlb);
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 2);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & !0x1, 0);

        // Read it back and ensure it's all zeros.
        let mut cmd = build_command(0x02); // READ
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw10(&mut cmd, slba);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, nlb);
        mem.write_physical(io_sq + 2 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 3);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 2 * 16);
        assert_eq!(cqe.cid, 0x12);
        assert_eq!(cqe.status & !0x1, 0);

        let mut out = vec![0u8; sector_size * sectors as usize];
        mem.read_physical(read_buf, &mut out);
        assert_eq!(out, vec![0u8; sector_size * sectors as usize]);
    }

    #[test]
    fn dataset_management_deallocate_completes_and_does_not_corrupt_outside_range() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(2 * 1024 * 1024);
        let sector_size = 512usize;

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;
        let write_buf = 0x60000;
        let read_buf = 0x61000;
        let dsm_ranges = 0x62000;

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

        // Write distinct patterns into 3 consecutive sectors so we can detect corruption.
        let sector0 = vec![0xAA; sector_size];
        let sector1 = vec![0xBB; sector_size];
        let sector2 = vec![0xCC; sector_size];

        let mut payload = Vec::new();
        payload.extend_from_slice(&sector0);
        payload.extend_from_slice(&sector1);
        payload.extend_from_slice(&sector2);
        mem.write_physical(write_buf, &payload);

        let mut cmd = build_command(0x01); // WRITE
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, write_buf);
        set_cdw10(&mut cmd, 0);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, 2); // 3 sectors (nlb=2)
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(0x1008, 4, 1);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & !0x1, 0);

        // DSM Deallocate sector 1 (1 range).
        //
        // DSM range definition layout (16 bytes):
        // - CATTR (u32)
        // - NLB (u32, 0-based)
        // - SLBA (u64)
        let mut range = [0u8; 16];
        range[4..8].copy_from_slice(&0u32.to_le_bytes()); // 1 sector
        range[8..16].copy_from_slice(&1u64.to_le_bytes()); // slba=1
        mem.write_physical(dsm_ranges, &range);

        let mut cmd = build_command(0x09); // DSM
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, dsm_ranges);
        set_cdw10(&mut cmd, 0); // NR=0 => 1 range
        set_cdw11(&mut cmd, 1 << 2); // Deallocate attribute
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 2);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & !0x1, 0);

        // Also accept additional DSM "integral dataset" hint bits alongside Deallocate.
        let mut cmd = build_command(0x09); // DSM
        set_cid(&mut cmd, 0x14);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, dsm_ranges);
        set_cdw10(&mut cmd, 0); // NR=0 => 1 range
        set_cdw11(&mut cmd, (1 << 2) | (1 << 1) | (1 << 0)); // Deallocate + hint bits
        mem.write_physical(io_sq + 2 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 3);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 2 * 16);
        assert_eq!(cqe.cid, 0x14);
        assert_eq!(cqe.status & !0x1, 0);

        // Read back sectors 0 and 2 to ensure they are unchanged.
        let mut cmd = build_command(0x02); // READ sector 0
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw10(&mut cmd, 0);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 3 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 4);
        ctrl.process(&mut mem);

        let mut out0 = vec![0u8; sector_size];
        mem.read_physical(read_buf, &mut out0);
        assert_eq!(out0, sector0);

        let mut cmd = build_command(0x02); // READ sector 2
        set_cid(&mut cmd, 0x13);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw10(&mut cmd, 2);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 4 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 5);
        ctrl.process(&mut mem);

        let mut out2 = vec![0u8; sector_size];
        mem.read_physical(read_buf, &mut out2);
        assert_eq!(out2, sector2);
    }

    #[test]
    fn dataset_management_deallocate_reclaims_sparse_blocks_and_reads_zero() {
        // Use a real AeroSparseDisk so DSM deallocate can reclaim storage by clearing allocation
        // table entries (reads of deallocated blocks return zeros).
        let sectors = 4096u64; // 2 MiB
        let capacity_bytes = sectors * SECTOR_SIZE as u64;
        let disk = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: capacity_bytes,
                block_size_bytes: 1024 * 1024,
            },
        )
        .unwrap();

        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(2 * 1024 * 1024);
        let sector_size = 512usize;

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;
        let write_buf = 0x60000;
        let read_buf = 0x61000;
        let dsm_ranges = 0x62000;

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

        // Write non-zero data at LBA 0 so we can observe it being discarded.
        let payload = vec![0x5A; sector_size];
        mem.write_physical(write_buf, &payload);

        let mut cmd = build_command(0x01); // WRITE
        set_cid(&mut cmd, 0x10);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, write_buf);
        set_cdw10(&mut cmd, 0);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(0x1008, 4, 1);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x10);
        assert_eq!(cqe.status & !0x1, 0);

        // Discard the entire first sparse allocation block (1 MiB / 512B = 2048 sectors).
        let nlb = 2048u32 - 1;
        let mut range = [0u8; 16];
        range[4..8].copy_from_slice(&nlb.to_le_bytes());
        range[8..16].copy_from_slice(&0u64.to_le_bytes()); // slba=0
        mem.write_physical(dsm_ranges, &range);

        let mut cmd = build_command(0x09); // DSM
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, dsm_ranges);
        set_cdw10(&mut cmd, 0); // NR=0 => 1 range
        set_cdw11(&mut cmd, 1 << 2); // Deallocate
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 2);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 16);
        assert_eq!(cqe.cid, 0x11);
        assert_eq!(cqe.status & !0x1, 0);

        // Read the sector back and ensure it is now zero-filled (block deallocated).
        let mut cmd = build_command(0x02); // READ
        set_cid(&mut cmd, 0x12);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw10(&mut cmd, 0);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 2 * 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 3);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq + 2 * 16);
        assert_eq!(cqe.cid, 0x12);
        assert_eq!(cqe.status & !0x1, 0);

        let mut out = vec![0u8; sector_size];
        mem.read_physical(read_buf, &mut out);
        assert_eq!(out, vec![0u8; sector_size]);
    }

    #[test]
    fn create_io_cq_count_is_capped() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Create the maximum allowed number of IO completion queues.
        for qid in 1u16..=(NVME_MAX_IO_QUEUES as u16) {
            let base = 0x10000u64 + (qid as u64) * (PAGE_SIZE as u64);
            let cmd = NvmeCommand {
                opc: 0x05,
                cid: qid,
                nsid: 0,
                psdt: 0,
                prp1: base,
                prp2: 0,
                cdw10: qid as u32, // qsize=1
                cdw11: 0,
                cdw12: 0,
                cdw13: 0,
                cdw14: 0,
                cdw15: 0,
            };
            let (status, _result) = ctrl.cmd_create_io_cq(cmd);
            assert_eq!(status, NvmeStatus::SUCCESS);
        }
        assert_eq!(ctrl.io_cqs.len(), NVME_MAX_IO_QUEUES);

        // One more should be rejected.
        let qid = (NVME_MAX_IO_QUEUES as u16) + 1;
        let cmd = NvmeCommand {
            opc: 0x05,
            cid: qid,
            nsid: 0,
            psdt: 0,
            prp1: 0x9000_0000,
            prp2: 0,
            cdw10: qid as u32,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _result) = ctrl.cmd_create_io_cq(cmd);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);
        assert_eq!(ctrl.io_cqs.len(), NVME_MAX_IO_QUEUES);
    }

    #[test]
    fn create_io_sq_count_is_capped() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Create a CQ needed by IO SQs (CQID=1).
        let cq_cmd = NvmeCommand {
            opc: 0x05,
            cid: 0x200,
            nsid: 0,
            psdt: 0,
            prp1: 0x8000_0000,
            prp2: 0,
            cdw10: 1, // qid=1, qsize=1
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _result) = ctrl.cmd_create_io_cq(cq_cmd);
        assert_eq!(status, NvmeStatus::SUCCESS);

        // Create the maximum allowed number of IO submission queues, all targeting CQID 1.
        for qid in 1u16..=(NVME_MAX_IO_QUEUES as u16) {
            let base = 0x20000u64 + (qid as u64) * (PAGE_SIZE as u64);
            let cmd = NvmeCommand {
                opc: 0x01,
                cid: qid,
                nsid: 0,
                psdt: 0,
                prp1: base,
                prp2: 0,
                cdw10: qid as u32, // qsize=1
                cdw11: 1,          // CQID=1
                cdw12: 0,
                cdw13: 0,
                cdw14: 0,
                cdw15: 0,
            };
            let (status, _result) = ctrl.cmd_create_io_sq(cmd);
            assert_eq!(status, NvmeStatus::SUCCESS);
        }
        assert_eq!(ctrl.io_sqs.len(), NVME_MAX_IO_QUEUES);

        // One more should be rejected.
        let qid = (NVME_MAX_IO_QUEUES as u16) + 1;
        let cmd = NvmeCommand {
            opc: 0x01,
            cid: qid,
            nsid: 0,
            psdt: 0,
            prp1: 0xA000_0000,
            prp2: 0,
            cdw10: qid as u32,
            cdw11: 1,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _result) = ctrl.cmd_create_io_sq(cmd);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);
        assert_eq!(ctrl.io_sqs.len(), NVME_MAX_IO_QUEUES);
    }

    #[test]
    fn create_io_queue_rejects_qsize_overflow_without_panicking() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // QSIZE is 0-based in CDW10[31:16]; 0xFFFF would overflow the u16 + 1 computation in debug
        // builds if not handled defensively.
        let cmd = NvmeCommand {
            opc: 0x05,
            cid: 0x1234,
            nsid: 0,
            psdt: 0,
            prp1: 0x10000,
            prp2: 0,
            cdw10: (0xFFFFu32 << 16) | 1, // qid=1, qsize=65536 (unsupported)
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _result) = ctrl.cmd_create_io_cq(cmd);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);

        let cmd = NvmeCommand {
            opc: 0x01,
            cid: 0x1235,
            nsid: 0,
            psdt: 0,
            prp1: 0x20000,
            prp2: 0,
            cdw10: (0xFFFFu32 << 16) | 1, // qid=1, qsize=65536 (unsupported)
            cdw11: 1,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _result) = ctrl.cmd_create_io_sq(cmd);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);
    }

    #[test]
    fn pending_sq_tail_map_is_capped() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Enable controller so doorbells are accepted.
        ctrl.mmio_write(0x0024, 4, 0x000f_000f);
        ctrl.mmio_write(0x0028, 8, 0x10000);
        ctrl.mmio_write(0x0030, 8, 0x20000);
        ctrl.mmio_write(0x0014, 4, 1);
        assert_eq!(ctrl.csts & 1, 1);

        // Fill the doorbell map to capacity without ever ringing SQ0.
        for qid in 1u16..=(NVME_MAX_PENDING_SQ_TAIL_UPDATES as u16) {
            let offset = 0x1000u64 + (qid as u64) * 8;
            ctrl.mmio_write(offset, 4, 1);
        }
        assert_eq!(ctrl.pending_sq_tail.len(), NVME_MAX_PENDING_SQ_TAIL_UPDATES);
        assert!(!ctrl.pending_sq_tail.contains_key(&0));

        // SQ0 doorbells must still be accepted, but the map size must remain capped.
        ctrl.mmio_write(0x1000, 4, 1);
        assert_eq!(ctrl.pending_sq_tail.len(), NVME_MAX_PENDING_SQ_TAIL_UPDATES);
        assert!(ctrl.pending_sq_tail.contains_key(&0));

        // Existing QIDs can still be updated even when the map is full.
        ctrl.mmio_write(0x1008, 4, 2); // SQ1 tail = 2
        assert_eq!(ctrl.pending_sq_tail.get(&1), Some(&2));

        // New QIDs should be dropped once the cap is reached.
        let before = ctrl.pending_sq_tail.len();
        ctrl.mmio_write(0x3ff8, 4, 3); // SQ1535 tail
        assert_eq!(ctrl.pending_sq_tail.len(), before);
    }

    #[test]
    fn load_state_rejects_excess_io_queues() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let mut state = NvmeControllerState::default();
        let count = NVME_MAX_IO_QUEUES + 1;

        state.io_sqs = (1u16..=(count as u16))
            .map(|qid| NvmeSubmissionQueueState {
                qid,
                base: 0x1000_0000u64 + (qid as u64) * (PAGE_SIZE as u64),
                size: 1,
                head: 0,
                tail: 0,
                cqid: qid,
            })
            .collect();

        state.io_cqs = (1u16..=(count as u16))
            .map(|qid| NvmeCompletionQueueState {
                qid,
                base: 0x2000_0000u64 + (qid as u64) * (PAGE_SIZE as u64),
                size: 1,
                head: 0,
                tail: 0,
                phase: true,
                irq_enabled: true,
            })
            .collect();

        let bytes = state.save_state();
        let err = ctrl.load_state(&bytes).unwrap_err();
        assert_eq!(
            err,
            SnapshotError::InvalidFieldEncoding("nvme io queue count")
        );
    }

    #[test]
    fn load_state_rejects_invalid_queue_sizes() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let state = NvmeControllerState {
            cc: 1,
            csts: 1,
            admin_sq: Some(NvmeSubmissionQueueState {
                qid: 0,
                base: 0x10000,
                size: 0,
                head: 0,
                tail: 0,
                cqid: 0,
            }),
            admin_cq: Some(NvmeCompletionQueueState {
                qid: 0,
                base: 0x20000,
                size: 1,
                head: 0,
                tail: 0,
                phase: true,
                irq_enabled: true,
            }),
            ..Default::default()
        };

        let bytes = state.save_state();
        let err = ctrl.load_state(&bytes).unwrap_err();
        assert_eq!(err, SnapshotError::InvalidFieldEncoding("nvme sq size"));
    }

    #[test]
    fn load_state_rejects_missing_io_cq_for_sq() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let state = NvmeControllerState {
            cc: 1,
            csts: 1,
            admin_sq: Some(NvmeSubmissionQueueState {
                qid: 0,
                base: 0x10000,
                size: 2,
                head: 0,
                tail: 0,
                cqid: 0,
            }),
            admin_cq: Some(NvmeCompletionQueueState {
                qid: 0,
                base: 0x20000,
                size: 2,
                head: 0,
                tail: 0,
                phase: true,
                irq_enabled: true,
            }),
            io_sqs: vec![NvmeSubmissionQueueState {
                qid: 1,
                base: 0x30000,
                size: 2,
                head: 0,
                tail: 0,
                cqid: 1,
            }],
            io_cqs: Vec::new(),
            ..Default::default()
        };

        let bytes = state.save_state();
        let err = ctrl.load_state(&bytes).unwrap_err();
        assert_eq!(err, SnapshotError::InvalidFieldEncoding("nvme sq cqid"));
    }

    #[test]
    fn io_read_rejects_oversized_transfer() {
        // Enough sectors that the LBA range check passes for a large request.
        let disk = TestDisk::new(20_000);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
        let mut mem = TestMem::new(2 * 1024 * 1024);

        let asq = 0x10000;
        let acq = 0x20000;
        let io_cq = 0x40000;
        let io_sq = 0x50000;
        let read_buf = 0x60000;

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

        // Request one sector more than the controller's max transfer (4MiB / 512 = 8192 sectors).
        let sectors = (NVME_MAX_DMA_BYTES / 512) as u32 + 1;
        let nlb = sectors - 1;

        let mut cmd = build_command(0x02);
        set_cid(&mut cmd, 0x20);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw10(&mut cmd, 0);
        set_cdw11(&mut cmd, 0);
        set_cdw12(&mut cmd, nlb);
        mem.write_physical(io_sq, &cmd);
        ctrl.mmio_write(0x1008, 4, 1);
        ctrl.process(&mut mem);

        let cqe = read_cqe(&mut mem, io_cq);
        assert_eq!(cqe.cid, 0x20);
        assert_eq!(
            cqe.status & !0x1,
            NvmeStatus::INVALID_FIELD.encode_without_phase()
        );
    }

    #[test]
    fn create_io_queues_and_rw_roundtrip_sparse_disk() {
        let disk = TestSparseDisk::new(1024);
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

        // READ it back.
        let mut cmd = build_command(0x02);
        set_cid(&mut cmd, 0x11);
        set_nsid(&mut cmd, 1);
        set_prp1(&mut cmd, read_buf);
        set_cdw12(&mut cmd, 0);
        mem.write_physical(io_sq + 64, &cmd);
        ctrl.mmio_write(0x1008, 4, 2); // SQ1 tail = 2
        ctrl.process(&mut mem);

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

    #[test]
    fn admin_delete_io_sq_clears_pending_doorbell_updates() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Create CQ1 + SQ1 directly.
        let cq_cmd = NvmeCommand {
            opc: 0x05,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0x10000,
            prp2: 0,
            cdw10: 1, // qid=1, qsize=1
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_create_io_cq(cq_cmd);
        assert_eq!(status, NvmeStatus::SUCCESS);

        let sq_cmd = NvmeCommand {
            opc: 0x01,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0x20000,
            prp2: 0,
            cdw10: 1, // qid=1, qsize=1
            cdw11: 1, // CQID=1
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_create_io_sq(sq_cmd);
        assert_eq!(status, NvmeStatus::SUCCESS);

        // Simulate a deferred SQ tail doorbell write so the queue has a pending entry.
        ctrl.pending_sq_tail.insert(1, 1);
        assert!(ctrl.pending_sq_tail.contains_key(&1));

        let del_sq = NvmeCommand {
            opc: 0x00,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1, // qid=1
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_sq(del_sq);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert!(!ctrl.io_sqs.contains_key(&1));
        assert!(!ctrl.pending_sq_tail.contains_key(&1));
    }

    #[test]
    fn admin_delete_io_cq_rejects_in_use_and_then_allows_delete() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Create CQ1 + SQ1 directly.
        let cq_cmd = NvmeCommand {
            opc: 0x05,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0x10000,
            prp2: 0,
            cdw10: 1, // qid=1, qsize=1
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_create_io_cq(cq_cmd);
        assert_eq!(status, NvmeStatus::SUCCESS);

        let sq_cmd = NvmeCommand {
            opc: 0x01,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0x20000,
            prp2: 0,
            cdw10: 1, // qid=1, qsize=1
            cdw11: 1, // CQID=1
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_create_io_sq(sq_cmd);
        assert_eq!(status, NvmeStatus::SUCCESS);

        // Cannot delete CQ1 while SQ1 still targets it.
        let del_cq = NvmeCommand {
            opc: 0x04,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_cq(del_cq);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);
        assert!(ctrl.io_cqs.contains_key(&1));

        // Delete SQ1 first, then delete CQ1.
        let del_sq = NvmeCommand {
            opc: 0x00,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_sq(del_sq);
        assert_eq!(status, NvmeStatus::SUCCESS);

        let del_cq = NvmeCommand {
            opc: 0x04,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_cq(del_cq);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert!(!ctrl.io_cqs.contains_key(&1));
    }

    #[test]
    fn admin_delete_io_queues_nonexistent_returns_invalid_qid() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        let del_sq = NvmeCommand {
            opc: 0x00,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_sq(del_sq);
        assert_eq!(status, NvmeStatus::INVALID_QID);

        let del_cq = NvmeCommand {
            opc: 0x04,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 1,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_delete_io_cq(del_cq);
        assert_eq!(status, NvmeStatus::INVALID_QID);
    }

    #[test]
    fn admin_get_set_features_number_of_queues_interrupt_coalescing_and_unsupported() {
        let disk = TestDisk::new(1024);
        let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());

        // Defaults: report max supported IO queues (0-based).
        let get_nq = NvmeCommand {
            opc: 0x0a,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x07,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, result) = ctrl.cmd_get_features(get_nq);
        assert_eq!(status, NvmeStatus::SUCCESS);
        let max = (NVME_MAX_IO_QUEUES as u32) - 1;
        assert_eq!(result, (max << 16) | max);

        // Supported capabilities for Number of Queues (SEL=3) should also report the maximum.
        let get_nq_supported = NvmeCommand {
            opc: 0x0a,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x07 | (3u32 << 8),
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, result) = ctrl.cmd_get_features(get_nq_supported);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert_eq!(result, (max << 16) | max);

        // Set Number of Queues: request 2 SQs (1) + 4 CQs (3) (both 0-based).
        let set_nq = NvmeCommand {
            opc: 0x09,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x07,
            cdw11: (1u32 << 16) | 3u32,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, result) = ctrl.cmd_set_features(set_nq);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert_eq!(result, (1u32 << 16) | 3u32);
        assert_eq!(ctrl.feature_num_io_sqs, 1);
        assert_eq!(ctrl.feature_num_io_cqs, 3);

        // Interrupt Coalescing roundtrip.
        let set_ic = NvmeCommand {
            opc: 0x09,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x08,
            cdw11: 0x1234,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, result) = ctrl.cmd_set_features(set_ic);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert_eq!(result, 0x1234);

        let get_ic = NvmeCommand {
            opc: 0x0a,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x08,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, result) = ctrl.cmd_get_features(get_ic);
        assert_eq!(status, NvmeStatus::SUCCESS);
        assert_eq!(result, 0x1234);

        // Unsupported FID should be rejected.
        let get_unsupported = NvmeCommand {
            opc: 0x0a,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x01,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_get_features(get_unsupported);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);

        let set_unsupported = NvmeCommand {
            opc: 0x09,
            cid: 0,
            nsid: 0,
            psdt: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0x01,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        };
        let (status, _) = ctrl.cmd_set_features(set_unsupported);
        assert_eq!(status, NvmeStatus::INVALID_FIELD);
    }
}
