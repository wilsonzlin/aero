use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_storage::SECTOR_SIZE;
use std::cell::RefCell;
use std::collections::BTreeMap;

// Canonical disk-controller wrapper snapshot (`DSKC`) lives in `io::storage::dskc` but is
// re-exported here for backward-compatible imports.
pub use super::dskc::DiskControllersSnapshot;

// Snapshots may be loaded from untrusted sources (e.g. downloaded files). Keep decoding bounded so
// corrupted snapshots cannot force pathological allocations.
const MAX_DISK_STRING_BYTES: usize = 64 * 1024;
const MAX_REMOTE_CHUNK_SIZE_BYTES: u32 = 64 * 1024 * 1024;
const MAX_OVERLAY_BLOCK_SIZE_BYTES: u32 = 64 * 1024 * 1024;

// ----------------------------------------
// Disk layer state (host-side)
// ----------------------------------------

/// Snapshot-layer disk backend interface.
///
/// This trait exists to keep `aero-io-snapshot` self-contained (snapshots can be decoded without
/// depending on any particular disk implementation). It is **not** intended to be a general
/// purpose disk abstraction for device/controller code.
///
/// New synchronous disk code in this repo should prefer [`aero_storage::VirtualDisk`] and adapt to
/// this snapshot trait using [`AeroStorageDiskBackend`] when needed.
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait DiskBackend {
    fn read_at(&self, offset: u64, buf: &mut [u8]);
    fn write_at(&mut self, offset: u64, data: &[u8]);
    fn flush(&mut self);
}

/// Adapter to use a real [`aero_storage::VirtualDisk`] as a snapshot [`DiskBackend`].
///
/// `aero_storage` uses `&mut self` for reads as well (to support internal caching),
/// so we wrap the disk in a [`RefCell`] to provide interior mutability.
///
/// ## wasm32 note
///
/// This adapter intentionally accepts `!Send` disks so wasm32 backends that wrap JS objects (for
/// example OPFS backends) can participate in snapshot flows without unsafe `Send` shims.
pub struct AeroStorageDiskBackend(pub RefCell<Box<dyn aero_storage::VirtualDisk>>);

impl AeroStorageDiskBackend {
    pub fn new(disk: Box<dyn aero_storage::VirtualDisk>) -> Self {
        Self(RefCell::new(disk))
    }
}

impl DiskBackend for AeroStorageDiskBackend {
    fn read_at(&self, offset: u64, buf: &mut [u8]) {
        let Ok(mut disk) = self.0.try_borrow_mut() else {
            debug_assert!(false, "aero-storage disk backend already mutably borrowed");
            return;
        };
        let _ = disk.read_at(offset, buf);
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) {
        let disk = self.0.get_mut();
        let _ = disk.write_at(offset, data);
    }

    fn flush(&mut self) {
        let disk = self.0.get_mut();
        let _ = disk.flush();
    }
}

/// Convenience helper to attach an [`aero_storage::VirtualDisk`] to an existing
/// [`DiskLayerState`].
///
/// This intentionally accepts `!Send` disks (see [`AeroStorageDiskBackend`]) so wasm32 backends
/// don't need unsafe `Send` shims just to participate in snapshot flows.
pub fn attach_aero_storage_disk(
    state: &mut DiskLayerState,
    disk: Box<dyn aero_storage::VirtualDisk>,
) {
    state.attach_backend(Box::new(AeroStorageDiskBackend::new(disk)));
}

/// Local disk backend identity (browser-backed disks).
///
/// Snapshot state MUST remain stable across page reloads and must never include
/// signed URLs, auth tokens, or other secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalDiskBackendKind {
    /// OPFS-backed disk file (key is a stable OPFS path / filename).
    Opfs,
    /// IndexedDB-backed disk (key is a stable disk id / primary key).
    Idb,
    /// Unknown/other backend. Only used for forward/backward-compat decoding.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskOverlayState {
    /// OPFS filename for the overlay image (e.g. `<diskId>.overlay.aerospar`).
    pub file_name: String,
    /// Virtual disk size for the overlay (bytes).
    pub disk_size_bytes: u64,
    /// Overlay block size (bytes). Must be a multiple of 512.
    pub block_size_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCacheState {
    /// OPFS filename for the cache image. Cached bytes are stored in OPFS and
    /// must not be inlined in the snapshot.
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDiskBackendState {
    pub kind: LocalDiskBackendKind,
    /// Stable backend key/path.
    pub key: String,
    /// Optional local overlay (COW).
    pub overlay: Option<DiskOverlayState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDiskValidator {
    Etag(String),
    LastModified(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDiskBaseState {
    /// Stable image identifier (e.g. `win7-sp1-x64` or a UUID for private images).
    pub image_id: String,
    /// Stable image version identifier (e.g. `sha256-...`).
    pub version: String,
    /// Delivery scheme (`range`, `chunked`, ...).
    pub delivery_type: String,
    /// Expected validator for the remote base (etag/last-modified). Used to bind
    /// OPFS cache files to a specific immutable base.
    pub expected_validator: Option<RemoteDiskValidator>,
    /// Chunk size (bytes) used for aligned remote reads and local caching.
    pub chunk_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDiskBackendState {
    pub base: RemoteDiskBaseState,
    /// Local write overlay (OPFS).
    pub overlay: DiskOverlayState,
    /// Local read cache binding (OPFS).
    pub cache: DiskCacheState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskBackendState {
    Local(LocalDiskBackendState),
    Remote(RemoteDiskBackendState),
}

impl DiskBackendState {
    fn encode_string(mut e: Encoder, s: &str) -> Encoder {
        e = e.u32(s.len() as u32);
        e.bytes(s.as_bytes())
    }

    fn decode_string(d: &mut Decoder<'_>) -> SnapshotResult<String> {
        let len = d.u32()? as usize;
        if len > MAX_DISK_STRING_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding("string too long"));
        }
        let bytes = d.bytes(len)?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| SnapshotError::InvalidFieldEncoding("string utf8"))
    }

    fn encode_overlay(mut e: Encoder, overlay: &DiskOverlayState) -> Encoder {
        e = Self::encode_string(e, &overlay.file_name);
        e = e.u64(overlay.disk_size_bytes);
        e.u32(overlay.block_size_bytes)
    }

    fn decode_overlay(d: &mut Decoder<'_>) -> SnapshotResult<DiskOverlayState> {
        let file_name = Self::decode_string(d)?;
        let disk_size_bytes = d.u64()?;
        let block_size_bytes = d.u32()?;
        if disk_size_bytes == 0 {
            return Err(SnapshotError::InvalidFieldEncoding("overlay disk_size"));
        }
        if disk_size_bytes % (SECTOR_SIZE as u64) != 0 {
            return Err(SnapshotError::InvalidFieldEncoding(
                "overlay disk_size not multiple of 512",
            ));
        }
        if block_size_bytes == 0 {
            return Err(SnapshotError::InvalidFieldEncoding("overlay block_size"));
        }
        if !block_size_bytes.is_multiple_of(SECTOR_SIZE as u32) {
            return Err(SnapshotError::InvalidFieldEncoding(
                "overlay block_size not multiple of 512",
            ));
        }
        if !block_size_bytes.is_power_of_two() {
            return Err(SnapshotError::InvalidFieldEncoding(
                "overlay block_size power_of_two",
            ));
        }
        if block_size_bytes > MAX_OVERLAY_BLOCK_SIZE_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "overlay block_size too large",
            ));
        }
        Ok(DiskOverlayState {
            file_name,
            disk_size_bytes,
            block_size_bytes,
        })
    }

    fn encode_cache(e: Encoder, cache: &DiskCacheState) -> Encoder {
        Self::encode_string(e, &cache.file_name)
    }

    fn decode_cache(d: &mut Decoder<'_>) -> SnapshotResult<DiskCacheState> {
        let file_name = Self::decode_string(d)?;
        Ok(DiskCacheState { file_name })
    }

    pub fn encode(&self) -> Vec<u8> {
        // v1 backend descriptor payload:
        // u8 kind (0=local, 1=remote)
        //
        // local:
        //  u8 backend_kind (0=opfs, 1=idb, 2=other)
        //  string key
        //  u8 overlay_present
        //   [overlay]
        //
        // remote:
        //  string image_id
        //  string version
        //  string delivery_type
        //  u8 validator_kind (0=none, 1=etag, 2=last_modified)
        //   [string validator_value]
        //  u32 chunk_size
        //  overlay
        //  cache
        let mut e = Encoder::new();
        match self {
            DiskBackendState::Local(local) => {
                e = e.u8(0);
                let kind = match local.kind {
                    LocalDiskBackendKind::Opfs => 0,
                    LocalDiskBackendKind::Idb => 1,
                    LocalDiskBackendKind::Other => 2,
                };
                e = e.u8(kind);
                e = Self::encode_string(e, &local.key);
                match &local.overlay {
                    Some(overlay) => {
                        e = e.u8(1);
                        e = Self::encode_overlay(e, overlay);
                    }
                    None => {
                        e = e.u8(0);
                    }
                }
                e.finish()
            }
            DiskBackendState::Remote(remote) => {
                e = e.u8(1);
                e = Self::encode_string(e, &remote.base.image_id);
                e = Self::encode_string(e, &remote.base.version);
                e = Self::encode_string(e, &remote.base.delivery_type);
                match &remote.base.expected_validator {
                    None => {
                        e = e.u8(0);
                    }
                    Some(RemoteDiskValidator::Etag(v)) => {
                        e = e.u8(1);
                        e = Self::encode_string(e, v);
                    }
                    Some(RemoteDiskValidator::LastModified(v)) => {
                        e = e.u8(2);
                        e = Self::encode_string(e, v);
                    }
                }
                e = e.u32(remote.base.chunk_size);
                e = Self::encode_overlay(e, &remote.overlay);
                e = Self::encode_cache(e, &remote.cache);
                e.finish()
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> SnapshotResult<Self> {
        let mut d = Decoder::new(bytes);
        let kind = d.u8()?;
        let out = match kind {
            0 => {
                let backend_kind = d.u8()?;
                let kind = match backend_kind {
                    0 => LocalDiskBackendKind::Opfs,
                    1 => LocalDiskBackendKind::Idb,
                    _ => LocalDiskBackendKind::Other,
                };
                let key = Self::decode_string(&mut d)?;
                let overlay = match d.u8()? {
                    0 => None,
                    1 => Some(Self::decode_overlay(&mut d)?),
                    _ => return Err(SnapshotError::InvalidFieldEncoding("overlay_present")),
                };
                DiskBackendState::Local(LocalDiskBackendState { kind, key, overlay })
            }
            1 => {
                let image_id = Self::decode_string(&mut d)?;
                let version = Self::decode_string(&mut d)?;
                let delivery_type = Self::decode_string(&mut d)?;
                let expected_validator = match d.u8()? {
                    0 => None,
                    1 => Some(RemoteDiskValidator::Etag(Self::decode_string(&mut d)?)),
                    2 => Some(RemoteDiskValidator::LastModified(Self::decode_string(
                        &mut d,
                    )?)),
                    _ => return Err(SnapshotError::InvalidFieldEncoding("validator_kind")),
                };
                let chunk_size = d.u32()?;
                if chunk_size == 0 {
                    return Err(SnapshotError::InvalidFieldEncoding("chunk_size"));
                }
                if !u64::from(chunk_size).is_multiple_of(SECTOR_SIZE as u64) {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "chunk_size not multiple of 512",
                    ));
                }
                if chunk_size > MAX_REMOTE_CHUNK_SIZE_BYTES {
                    return Err(SnapshotError::InvalidFieldEncoding("chunk_size too large"));
                }
                let overlay = Self::decode_overlay(&mut d)?;
                let cache = Self::decode_cache(&mut d)?;
                DiskBackendState::Remote(RemoteDiskBackendState {
                    base: RemoteDiskBaseState {
                        image_id,
                        version,
                        delivery_type,
                        expected_validator,
                        chunk_size,
                    },
                    overlay,
                    cache,
                })
            }
            _ => return Err(SnapshotError::InvalidFieldEncoding("backend kind")),
        };
        d.finish()?;
        Ok(out)
    }
}

/// Host-side disk state that can be snapshotted independently of the underlying backing store.
///
/// The actual disk contents are assumed to live in an external backend (OPFS/IndexedDB/etc).
/// Dirty write-back state is flushed before snapshot at the coordinator layer.
pub struct DiskLayerState {
    pub backend: DiskBackendState,
    pub sector_size: usize,
    pub size_bytes: u64,
    attached_backend: Option<Box<dyn DiskBackend>>,
}

impl std::fmt::Debug for DiskLayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskLayerState")
            .field("backend", &self.backend)
            .field("sector_size", &self.sector_size)
            .field("size_bytes", &self.size_bytes)
            .field("backend_attached", &self.attached_backend.is_some())
            .finish()
    }
}

impl PartialEq for DiskLayerState {
    fn eq(&self, other: &Self) -> bool {
        self.backend == other.backend
            && self.sector_size == other.sector_size
            && self.size_bytes == other.size_bytes
    }
}

impl Eq for DiskLayerState {}

impl DiskLayerState {
    pub fn new(backend: DiskBackendState, size_bytes: u64, sector_size: usize) -> Self {
        Self {
            backend,
            sector_size,
            size_bytes,
            attached_backend: None,
        }
    }

    pub fn attach_backend(&mut self, backend: Box<dyn DiskBackend>) {
        self.attached_backend = Some(backend);
    }

    pub fn read_sector(&mut self, lba: u64) -> Vec<u8> {
        let mut out = vec![0u8; self.sector_size];
        let sector_size = self.sector_size as u64;
        let offset = match lba.checked_mul(sector_size) {
            Some(off) => off,
            None => return out,
        };
        let Some(end) = offset.checked_add(sector_size) else {
            return out;
        };
        if end > self.size_bytes {
            return out;
        }
        if let Some(backend) = &self.attached_backend {
            backend.read_at(offset, &mut out);
        }
        out
    }

    pub fn write_sector(&mut self, lba: u64, data: &[u8]) {
        assert_eq!(data.len(), self.sector_size);
        let sector_size = self.sector_size as u64;
        let offset = match lba.checked_mul(sector_size) {
            Some(off) => off,
            None => return,
        };
        let Some(end) = offset.checked_add(sector_size) else {
            return;
        };
        if end > self.size_bytes {
            return;
        }
        if let Some(backend) = self.attached_backend.as_mut() {
            backend.write_at(offset, data);
        }
    }

    pub fn flush(&mut self) {
        if let Some(backend) = self.attached_backend.as_mut() {
            backend.flush();
        }
    }
}

impl IoSnapshot for DiskLayerState {
    const DEVICE_ID: [u8; 4] = *b"DSK0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_BACKEND_KEY: u16 = 1;
        const TAG_SECTOR_SIZE: u16 = 2;
        const TAG_SIZE_BYTES: u16 = 3;
        const TAG_BACKEND_STATE: u16 = 8;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        if let DiskBackendState::Local(local) = &self.backend {
            w.field_bytes(TAG_BACKEND_KEY, local.key.as_bytes().to_vec());
        }
        w.field_bytes(TAG_BACKEND_STATE, self.backend.encode());
        w.field_u32(TAG_SECTOR_SIZE, self.sector_size as u32);
        w.field_u64(TAG_SIZE_BYTES, self.size_bytes);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_BACKEND_KEY: u16 = 1;
        const TAG_SECTOR_SIZE: u16 = 2;
        const TAG_SIZE_BYTES: u16 = 3;
        const TAG_BACKEND_STATE: u16 = 8;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_BACKEND_STATE) {
            self.backend = DiskBackendState::decode(buf)?;
        } else if let Some(key) = r.bytes(TAG_BACKEND_KEY) {
            if key.len() > MAX_DISK_STRING_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("backend_key too long"));
            }
            // Backward-compat: legacy snapshots only stored a backend key string.
            let key = std::str::from_utf8(key)
                .map(|s| s.to_string())
                .map_err(|_| SnapshotError::InvalidFieldEncoding("backend_key utf8"))?;
            self.backend = DiskBackendState::Local(LocalDiskBackendState {
                kind: LocalDiskBackendKind::Other,
                key,
                overlay: None,
            });
        }
        if let Some(sector) = r.u32(TAG_SECTOR_SIZE)? {
            self.sector_size = sector as usize;
        }
        if let Some(size) = r.u64(TAG_SIZE_BYTES)? {
            self.size_bytes = size;
        }

        match self.sector_size {
            SECTOR_SIZE | 4096 => {}
            _ => return Err(SnapshotError::InvalidFieldEncoding("sector_size")),
        }
        if self.size_bytes == 0 {
            return Err(SnapshotError::InvalidFieldEncoding("disk_size"));
        }
        if !self.size_bytes.is_multiple_of(self.sector_size as u64) {
            return Err(SnapshotError::InvalidFieldEncoding(
                "disk_size not multiple of sector_size",
            ));
        }

        match &self.backend {
            DiskBackendState::Local(local) => {
                if let Some(overlay) = &local.overlay {
                    if overlay.disk_size_bytes != self.size_bytes {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "overlay disk_size mismatch",
                        ));
                    }
                }
            }
            DiskBackendState::Remote(remote) => {
                if remote.overlay.disk_size_bytes != self.size_bytes {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "overlay disk_size mismatch",
                    ));
                }
            }
        }

        self.attached_backend = None;
        Ok(())
    }
}

// ----------------------------------------
// PCI IDE / ATA / ATAPI controller state
// ----------------------------------------

// Snapshots may be loaded from untrusted sources. The IDE controller can expose
// multi-sector PIO and ATAPI transfers; cap any inlined data buffers to avoid
// pathological allocations.
pub const MAX_IDE_DATA_BUFFER_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PciConfigSpaceState {
    pub regs: [u8; 256],

    // BAR decode/probe state used by config reads.
    pub bar0: u32,
    pub bar1: u32,
    pub bar2: u32,
    pub bar3: u32,
    pub bar4: u32,
    pub bar0_probe: bool,
    pub bar1_probe: bool,
    pub bar2_probe: bool,
    pub bar3_probe: bool,
    pub bar4_probe: bool,

    // Latched I/O decode bases (may differ from BARs during size probes).
    pub bus_master_base: u16,
}

impl Default for PciConfigSpaceState {
    fn default() -> Self {
        Self {
            regs: [0u8; 256],
            bar0: 0,
            bar1: 0,
            bar2: 0,
            bar3: 0,
            bar4: 0,
            bar0_probe: false,
            bar1_probe: false,
            bar2_probe: false,
            bar3_probe: false,
            bar4_probe: false,
            bus_master_base: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdePortMapState {
    pub cmd_base: u16,
    pub ctrl_base: u16,
    pub irq: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdeTaskFileState {
    pub features: u8,
    pub sector_count: u8,
    pub lba0: u8,
    pub lba1: u8,
    pub lba2: u8,
    pub device: u8,

    pub hob_features: u8,
    pub hob_sector_count: u8,
    pub hob_lba0: u8,
    pub hob_lba1: u8,
    pub hob_lba2: u8,

    pub pending_features_high: bool,
    pub pending_sector_count_high: bool,
    pub pending_lba0_high: bool,
    pub pending_lba1_high: bool,
    pub pending_lba2_high: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdeDataMode {
    #[default]
    None = 0,
    PioIn = 1,
    PioOut = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeTransferKind {
    AtaPioRead = 1,
    AtaPioWrite = 2,
    Identify = 3,
    AtapiPacket = 4,
    AtapiPioIn = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeDmaDirection {
    ToMemory = 0,
    FromMemory = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdeDmaCommitState {
    AtaWrite { lba: u64, sectors: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeDmaRequestState {
    pub direction: IdeDmaDirection,
    pub buffer: Vec<u8>,
    pub commit: Option<IdeDmaCommitState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdeBusMasterChannelState {
    pub cmd: u8,
    pub status: u8,
    pub prd_addr: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeAtaDeviceState {
    /// Negotiated DMA transfer mode for an IDE ATA device.
    ///
    /// This is a compact, backward-compatible encoding used by the IDE controller snapshot:
    /// - `0..=6`: Ultra DMA mode number (UDMA enabled).
    /// - `0x80 | n`: Multiword DMA mode number `n` (UDMA disabled).
    /// - `0xFF`: legacy sentinel for "UDMA disabled" with unknown MWDMA mode (accepted on restore).
    pub udma_mode: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeAtapiDeviceState {
    pub tray_open: bool,
    pub media_changed: bool,
    pub media_present: bool,
    pub sense_key: u8,
    pub asc: u8,
    pub ascq: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IdeDriveState {
    #[default]
    None,
    Ata(IdeAtaDeviceState),
    Atapi(IdeAtapiDeviceState),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdePioWriteState {
    pub lba: u64,
    pub sectors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdeChannelState {
    pub ports: IdePortMapState,

    pub tf: IdeTaskFileState,
    pub status: u8,
    pub error: u8,
    pub control: u8,

    pub irq_pending: bool,

    pub data_mode: IdeDataMode,
    pub transfer_kind: Option<IdeTransferKind>,
    pub data: Vec<u8>,
    pub data_index: u32,

    pub pending_dma: Option<IdeDmaRequestState>,
    pub pio_write: Option<IdePioWriteState>,

    pub bus_master: IdeBusMasterChannelState,

    pub drives: [IdeDriveState; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdeControllerState {
    pub pci: PciConfigSpaceState,
    pub primary: IdeChannelState,
    pub secondary: IdeChannelState,
}

impl PciConfigSpaceState {
    fn encode(&self) -> Vec<u8> {
        Encoder::new()
            .bytes(&self.regs)
            .u32(self.bar0)
            .u32(self.bar1)
            .u32(self.bar2)
            .u32(self.bar3)
            .u32(self.bar4)
            .bool(self.bar0_probe)
            .bool(self.bar1_probe)
            .bool(self.bar2_probe)
            .bool(self.bar3_probe)
            .bool(self.bar4_probe)
            .u16(self.bus_master_base)
            .finish()
    }

    fn decode(bytes: &[u8]) -> SnapshotResult<Self> {
        let mut d = Decoder::new(bytes);
        let regs_bytes = d.bytes(256)?;
        let mut regs = [0u8; 256];
        regs.copy_from_slice(regs_bytes);
        let bar0 = d.u32()?;
        let bar1 = d.u32()?;
        let bar2 = d.u32()?;
        let bar3 = d.u32()?;
        let bar4 = d.u32()?;
        let bar0_probe = d.bool()?;
        let bar1_probe = d.bool()?;
        let bar2_probe = d.bool()?;
        let bar3_probe = d.bool()?;
        let bar4_probe = d.bool()?;
        let bus_master_base = d.u16()?;
        d.finish()?;
        Ok(Self {
            regs,
            bar0,
            bar1,
            bar2,
            bar3,
            bar4,
            bar0_probe,
            bar1_probe,
            bar2_probe,
            bar3_probe,
            bar4_probe,
            bus_master_base,
        })
    }
}

impl IdeTaskFileState {
    fn encode(&self, mut e: Encoder) -> Encoder {
        e = e
            .u8(self.features)
            .u8(self.sector_count)
            .u8(self.lba0)
            .u8(self.lba1)
            .u8(self.lba2)
            .u8(self.device)
            .u8(self.hob_features)
            .u8(self.hob_sector_count)
            .u8(self.hob_lba0)
            .u8(self.hob_lba1)
            .u8(self.hob_lba2)
            .bool(self.pending_features_high)
            .bool(self.pending_sector_count_high)
            .bool(self.pending_lba0_high)
            .bool(self.pending_lba1_high)
            .bool(self.pending_lba2_high);
        e
    }

    fn decode(d: &mut Decoder<'_>) -> SnapshotResult<Self> {
        Ok(Self {
            features: d.u8()?,
            sector_count: d.u8()?,
            lba0: d.u8()?,
            lba1: d.u8()?,
            lba2: d.u8()?,
            device: d.u8()?,
            hob_features: d.u8()?,
            hob_sector_count: d.u8()?,
            hob_lba0: d.u8()?,
            hob_lba1: d.u8()?,
            hob_lba2: d.u8()?,
            pending_features_high: d.bool()?,
            pending_sector_count_high: d.bool()?,
            pending_lba0_high: d.bool()?,
            pending_lba1_high: d.bool()?,
            pending_lba2_high: d.bool()?,
        })
    }
}

impl IdeChannelState {
    fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new()
            .u16(self.ports.cmd_base)
            .u16(self.ports.ctrl_base)
            .u8(self.ports.irq);

        e = self.tf.encode(e);

        e = e
            .u8(self.status)
            .u8(self.error)
            .u8(self.control)
            .bool(self.irq_pending);

        let data_mode = self.data_mode as u8;
        let transfer_kind = self.transfer_kind.map(|k| k as u8).unwrap_or(0);

        e = e
            .u8(data_mode)
            .u8(transfer_kind)
            .u32(self.data_index)
            .u32(self.data.len() as u32)
            .bytes(&self.data);

        match &self.pio_write {
            None => {
                e = e.u8(0);
            }
            Some(pw) => {
                e = e.u8(1).u64(pw.lba).u64(pw.sectors);
            }
        }

        match &self.pending_dma {
            None => {
                e = e.u8(0);
            }
            Some(req) => {
                e = e.u8(1);
                let dir = req.direction as u8;
                e = e.u8(dir).u32(req.buffer.len() as u32).bytes(&req.buffer);
                match &req.commit {
                    None => {
                        e = e.u8(0);
                    }
                    Some(IdeDmaCommitState::AtaWrite { lba, sectors }) => {
                        e = e.u8(1).u64(*lba).u64(*sectors);
                    }
                }
            }
        }

        e = e
            .u8(self.bus_master.cmd)
            .u8(self.bus_master.status)
            .u32(self.bus_master.prd_addr);

        for drive in &self.drives {
            match drive {
                IdeDriveState::None => {
                    e = e.u8(0);
                }
                IdeDriveState::Ata(ata) => {
                    e = e.u8(1).u8(ata.udma_mode);
                }
                IdeDriveState::Atapi(atapi) => {
                    e = e
                        .u8(2)
                        .bool(atapi.tray_open)
                        .bool(atapi.media_changed)
                        .bool(atapi.media_present)
                        .u8(atapi.sense_key)
                        .u8(atapi.asc)
                        .u8(atapi.ascq);
                }
            }
        }

        e.finish()
    }

    fn decode(bytes: &[u8]) -> SnapshotResult<Self> {
        let mut d = Decoder::new(bytes);

        let ports = IdePortMapState {
            cmd_base: d.u16()?,
            ctrl_base: d.u16()?,
            irq: d.u8()?,
        };

        let tf = IdeTaskFileState::decode(&mut d)?;

        let status = d.u8()?;
        let error = d.u8()?;
        let control = d.u8()?;
        let irq_pending = d.bool()?;

        let data_mode_raw = d.u8()?;
        let data_mode = match data_mode_raw {
            0 => IdeDataMode::None,
            1 => IdeDataMode::PioIn,
            2 => IdeDataMode::PioOut,
            _ => return Err(SnapshotError::InvalidFieldEncoding("ide data_mode")),
        };

        let transfer_raw = d.u8()?;
        let transfer_kind = match transfer_raw {
            0 => None,
            1 => Some(IdeTransferKind::AtaPioRead),
            2 => Some(IdeTransferKind::AtaPioWrite),
            3 => Some(IdeTransferKind::Identify),
            4 => Some(IdeTransferKind::AtapiPacket),
            5 => Some(IdeTransferKind::AtapiPioIn),
            _ => return Err(SnapshotError::InvalidFieldEncoding("ide transfer_kind")),
        };

        let data_index = d.u32()?;
        let data_len = d.u32()? as usize;
        if data_len > MAX_IDE_DATA_BUFFER_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "ide pio buffer too large",
            ));
        }
        if matches!(transfer_kind, Some(IdeTransferKind::AtapiPacket)) && data_len < 12 {
            return Err(SnapshotError::InvalidFieldEncoding(
                "ide atapi packet buffer too small",
            ));
        }
        if data_index as usize > data_len {
            return Err(SnapshotError::InvalidFieldEncoding("ide pio data_index"));
        }
        let data = d.bytes_vec(data_len)?;

        let pio_write = match d.u8()? {
            0 => None,
            1 => Some(IdePioWriteState {
                lba: d.u64()?,
                sectors: d.u64()?,
            }),
            _ => return Err(SnapshotError::InvalidFieldEncoding("ide pio_write present")),
        };

        let pending_dma = match d.u8()? {
            0 => None,
            1 => {
                let dir_raw = d.u8()?;
                let direction = match dir_raw {
                    0 => IdeDmaDirection::ToMemory,
                    1 => IdeDmaDirection::FromMemory,
                    _ => return Err(SnapshotError::InvalidFieldEncoding("ide dma direction")),
                };
                let len = d.u32()? as usize;
                if len > MAX_IDE_DATA_BUFFER_BYTES {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "ide dma buffer too large",
                    ));
                }
                let buffer = d.bytes_vec(len)?;
                let commit = match d.u8()? {
                    0 => None,
                    1 => Some(IdeDmaCommitState::AtaWrite {
                        lba: d.u64()?,
                        sectors: d.u64()?,
                    }),
                    _ => return Err(SnapshotError::InvalidFieldEncoding("ide dma commit kind")),
                };
                Some(IdeDmaRequestState {
                    direction,
                    buffer,
                    commit,
                })
            }
            _ => {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "ide pending_dma present",
                ))
            }
        };

        let bus_master = IdeBusMasterChannelState {
            cmd: d.u8()?,
            status: d.u8()?,
            prd_addr: d.u32()?,
        };

        let mut drives: [IdeDriveState; 2] = core::array::from_fn(|_| IdeDriveState::None);
        for slot in drives.iter_mut() {
            let kind = d.u8()?;
            *slot = match kind {
                0 => IdeDriveState::None,
                1 => IdeDriveState::Ata(IdeAtaDeviceState { udma_mode: d.u8()? }),
                2 => IdeDriveState::Atapi(IdeAtapiDeviceState {
                    tray_open: d.bool()?,
                    media_changed: d.bool()?,
                    media_present: d.bool()?,
                    sense_key: d.u8()?,
                    asc: d.u8()?,
                    ascq: d.u8()?,
                }),
                _ => return Err(SnapshotError::InvalidFieldEncoding("ide drive kind")),
            };
        }

        d.finish()?;

        Ok(Self {
            ports,
            tf,
            status,
            error,
            control,
            irq_pending,
            data_mode,
            transfer_kind,
            data,
            data_index,
            pending_dma,
            pio_write,
            bus_master,
            drives,
        })
    }
}

impl IoSnapshot for IdeControllerState {
    const DEVICE_ID: [u8; 4] = *b"IDE0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(2, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_PRIMARY: u16 = 2;
        const TAG_SECONDARY: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_PCI, self.pci.encode());
        w.field_bytes(TAG_PRIMARY, self.primary.encode());
        w.field_bytes(TAG_SECONDARY, self.secondary.encode());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_PRIMARY: u16 = 2;
        const TAG_SECONDARY: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_PCI) {
            self.pci = PciConfigSpaceState::decode(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_PRIMARY) {
            self.primary = IdeChannelState::decode(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_SECONDARY) {
            self.secondary = IdeChannelState::decode(buf)?;
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// Number of Queues feature (FID=0x07): 0-based queue counts.
    ///
    /// The NVMe spec defines these as 0-based (i.e. 0 means 1 queue). We keep the encoded values
    /// here (not the +1 decoded counts) so the state matches what GET/SET FEATURES expose to the
    /// guest.
    pub feature_num_io_sqs: u16,
    pub feature_num_io_cqs: u16,
    /// Interrupt Coalescing feature (FID=0x08): raw 16-bit value (THR/TIME fields).
    pub feature_interrupt_coalescing: u16,
    /// Volatile Write Cache feature (FID=0x06): enabled flag (CDW11 bit 0).
    pub feature_volatile_write_cache: bool,
    pub admin_sq: Option<NvmeSubmissionQueueState>,
    pub admin_cq: Option<NvmeCompletionQueueState>,
    pub io_sqs: Vec<NvmeSubmissionQueueState>,
    pub io_cqs: Vec<NvmeCompletionQueueState>,
    pub intx_level: bool,
    pub in_flight: Vec<NvmeInFlightCommandState>,
}

impl IoSnapshot for NvmeControllerState {
    const DEVICE_ID: [u8; 4] = *b"NVME";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 2);

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
        const TAG_FEATURES: u16 = 10;

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

        let features = Encoder::new()
            .u16(self.feature_num_io_sqs)
            .u16(self.feature_num_io_cqs)
            .u16(self.feature_interrupt_coalescing)
            .bool(self.feature_volatile_write_cache)
            .finish();
        w.field_bytes(TAG_FEATURES, features);

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
        const TAG_FEATURES: u16 = 10;

        const MAX_IO_QUEUES: usize = 4096;
        const MAX_IN_FLIGHT_COMMANDS: usize = 262_144;

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

        // Reset feature state to a deterministic baseline before applying snapshot fields.
        self.feature_num_io_sqs = 0;
        self.feature_num_io_cqs = 0;
        self.feature_interrupt_coalescing = 0;
        self.feature_volatile_write_cache = false;

        // Feature state (NVME 1.2+). For older snapshots these remain at their default values.
        if let Some(buf) = r.bytes(TAG_FEATURES) {
            let mut d = Decoder::new(buf);
            self.feature_num_io_sqs = d.u16()?;
            self.feature_num_io_cqs = d.u16()?;
            self.feature_interrupt_coalescing = d.u16()?;
            self.feature_volatile_write_cache = d.bool()?;
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
            if count > MAX_IO_QUEUES {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io queue count"));
            }
            self.io_sqs
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
            self.io_cqs
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
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
            if count > MAX_IO_QUEUES {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io sq count"));
            }
            self.io_sqs
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
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
            if count > MAX_IO_QUEUES {
                return Err(SnapshotError::InvalidFieldEncoding("nvme io cq count"));
            }
            self.io_cqs
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
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
            if count > MAX_IN_FLIGHT_COMMANDS {
                return Err(SnapshotError::InvalidFieldEncoding("nvme in_flight count"));
            }
            self.in_flight
                .try_reserve_exact(count)
                .map_err(|_| SnapshotError::OutOfMemory)?;
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

        fn validate_sq(sq: &NvmeSubmissionQueueState) -> SnapshotResult<()> {
            if sq.size == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme sq size"));
            }
            if sq.head >= sq.size || sq.tail >= sq.size {
                return Err(SnapshotError::InvalidFieldEncoding("nvme sq head/tail"));
            }
            Ok(())
        }

        fn validate_cq(cq: &NvmeCompletionQueueState) -> SnapshotResult<()> {
            if cq.size == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("nvme cq size"));
            }
            if cq.head >= cq.size || cq.tail >= cq.size {
                return Err(SnapshotError::InvalidFieldEncoding("nvme cq head/tail"));
            }
            Ok(())
        }

        if let Some(ref sq) = self.admin_sq {
            validate_sq(sq)?;
        }
        if let Some(ref cq) = self.admin_cq {
            validate_cq(cq)?;
        }
        for sq in &self.io_sqs {
            validate_sq(sq)?;
        }
        for cq in &self.io_cqs {
            validate_cq(cq)?;
        }

        Ok(())
    }
}

// ----------------------------------------
// AHCI controller
// ----------------------------------------

/// AHCI HBA (global) register state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AhciHbaState {
    pub cap: u32,
    pub ghc: u32,
    pub cap2: u32,
    pub bohc: u32,
    pub vs: u32,
}

/// AHCI per-port register state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AhciPortState {
    pub clb: u64,
    pub fb: u64,
    pub is: u32,
    pub ie: u32,
    pub cmd: u32,
    pub tfd: u32,
    pub sig: u32,
    pub ssts: u32,
    pub sctl: u32,
    pub serr: u32,
    pub sact: u32,
    pub ci: u32,
}

/// Serializable AHCI controller state.
///
/// This captures guest-visible register state for the controller's implemented ports.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AhciControllerState {
    pub hba: AhciHbaState,
    pub ports: Vec<AhciPortState>,
}

impl IoSnapshot for AhciControllerState {
    const DEVICE_ID: [u8; 4] = *b"AHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_HBA: u16 = 1;
        const TAG_PORTS: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let hba = Encoder::new()
            .u32(self.hba.cap)
            .u32(self.hba.ghc)
            .u32(self.hba.cap2)
            .u32(self.hba.bohc)
            .u32(self.hba.vs)
            .finish();
        w.field_bytes(TAG_HBA, hba);

        let mut ports = Encoder::new().u32(self.ports.len() as u32);
        for p in &self.ports {
            ports = ports
                .u64(p.clb)
                .u64(p.fb)
                .u32(p.is)
                .u32(p.ie)
                .u32(p.cmd)
                .u32(p.tfd)
                .u32(p.sig)
                .u32(p.ssts)
                .u32(p.sctl)
                .u32(p.serr)
                .u32(p.sact)
                .u32(p.ci);
        }
        w.field_bytes(TAG_PORTS, ports.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_HBA: u16 = 1;
        const TAG_PORTS: u16 = 2;

        const MAX_PORTS: usize = 32;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_HBA) {
            let mut d = Decoder::new(buf);
            self.hba.cap = d.u32()?;
            self.hba.ghc = d.u32()?;
            self.hba.cap2 = d.u32()?;
            self.hba.bohc = d.u32()?;
            self.hba.vs = d.u32()?;
            d.finish()?;
        }

        self.ports.clear();
        if let Some(buf) = r.bytes(TAG_PORTS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_PORTS {
                return Err(SnapshotError::InvalidFieldEncoding("ahci port count"));
            }
            self.ports.reserve(count);
            for _ in 0..count {
                self.ports.push(AhciPortState {
                    clb: d.u64()?,
                    fb: d.u64()?,
                    is: d.u32()?,
                    ie: d.u32()?,
                    cmd: d.u32()?,
                    tfd: d.u32()?,
                    sig: d.u32()?,
                    ssts: d.u32()?,
                    sctl: d.u32()?,
                    serr: d.u32()?,
                    sact: d.u32()?,
                    ci: d.u32()?,
                });
            }
            d.finish()?;
        }

        Ok(())
    }
}
