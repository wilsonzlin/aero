use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::write_u8;
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use aero_storage::{DiskError as StorageDiskError, VirtualDisk};

pub const VIRTIO_DEVICE_TYPE_BLK: u16 = 2;

pub const VIRTIO_BLK_SECTOR_SIZE: u64 = 512;

pub const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBackendError {
    OutOfBounds,
    IoError,
}

impl core::fmt::Display for BlockBackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "out of bounds"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

impl std::error::Error for BlockBackendError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkConfig {
    /// Capacity in 512-byte sectors.
    pub capacity: u64,
    pub size_max: u32,
    pub seg_max: u32,
    pub blk_size: u32,
}

impl VirtioBlkConfig {
    // capacity (8) + size_max (4) + seg_max (4) + geometry (4) + blk_size (4)
    pub const SIZE: usize = 24;

    pub fn read(&self, offset: u64, data: &mut [u8]) {
        let mut cfg = [0u8; Self::SIZE];
        cfg[0..8].copy_from_slice(&self.capacity.to_le_bytes());
        cfg[8..12].copy_from_slice(&self.size_max.to_le_bytes());
        cfg[12..16].copy_from_slice(&self.seg_max.to_le_bytes());
        // geometry is zeroed.
        cfg[20..24].copy_from_slice(&self.blk_size.to_le_bytes());

        // Avoid truncating on 32-bit targets: guest MMIO offsets are `u64` but config space is a
        // small fixed-size array.
        let start: usize = match offset.try_into() {
            Ok(v) => v,
            Err(_) => {
                data.fill(0);
                return;
            }
        };
        if start >= cfg.len() {
            data.fill(0);
            return;
        }
        let end = start
            .checked_add(data.len())
            .unwrap_or(cfg.len())
            .min(cfg.len());
        data[..end - start].copy_from_slice(&cfg[start..end]);
        if end - start < data.len() {
            data[end - start..].fill(0);
        }
    }
}

/// Disk backend trait used by the `aero-virtio` virtio-blk device model.
///
/// # Canonical trait note
///
/// The repo-wide canonical synchronous disk trait is [`aero_storage::VirtualDisk`]. This crate
/// keeps a separate `BlockBackend` trait primarily for virtio-blk device ergonomics, but most
/// call sites should pass a boxed `aero-storage` disk type; an adapter is provided:
///
/// - `impl<T: aero_storage::VirtualDisk> BlockBackend for Box<T>`
///
/// Avoid introducing new backend traits in other crates; prefer adapting from
/// `aero_storage::VirtualDisk` instead.
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait BlockBackend {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), BlockBackendError>;
    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), BlockBackendError>;
    fn blk_size(&self) -> u32 {
        VIRTIO_BLK_SECTOR_SIZE as u32
    }
    fn flush(&mut self) -> Result<(), BlockBackendError> {
        Ok(())
    }
    fn device_id(&self) -> [u8; 20] {
        [0; 20]
    }
}

fn map_storage_error(err: StorageDiskError) -> BlockBackendError {
    match err {
        StorageDiskError::OutOfBounds { .. } => BlockBackendError::OutOfBounds,
        StorageDiskError::UnalignedLength { .. }
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
        | StorageDiskError::Io(_) => BlockBackendError::IoError,
    }
}

/// Allow `aero-storage` virtual disks to be used directly as virtio-blk backends.
///
/// This means platform code can do:
///
/// ```rust,ignore
/// use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
/// use aero_virtio::devices::blk::VirtioBlk;
///
/// let disk = RawDisk::create(MemBackend::new(), (1024 * SECTOR_SIZE) as u64).unwrap();
/// let blk = VirtioBlk::new(Box::new(disk));
/// ```
///
/// The virtio-blk device logic itself still enforces sector-based requests; this adapter is
/// byte-addressed and forwards directly to the underlying [`VirtualDisk`] `read_at`/`write_at`.
impl<T: VirtualDisk + ?Sized> BlockBackend for Box<T> {
    fn len(&self) -> u64 {
        (**self).capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), BlockBackendError> {
        (**self).read_at(offset, dst).map_err(map_storage_error)
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), BlockBackendError> {
        (**self).write_at(offset, src).map_err(map_storage_error)
    }

    fn blk_size(&self) -> u32 {
        VIRTIO_BLK_SECTOR_SIZE as u32
    }

    fn flush(&mut self) -> Result<(), BlockBackendError> {
        (**self).flush().map_err(map_storage_error)
    }
}

#[derive(Debug, Clone)]
pub struct MemDisk {
    data: Vec<u8>,
    id: [u8; 20],
}

impl MemDisk {
    pub fn new(size: usize) -> Self {
        let mut id = [0u8; 20];
        id[..19].copy_from_slice(b"aero-virtio-memdisk");
        Self {
            data: vec![0; size],
            id,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl BlockBackend for MemDisk {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), BlockBackendError> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| BlockBackendError::OutOfBounds)?;
        let end = offset
            .checked_add(dst.len())
            .ok_or(BlockBackendError::OutOfBounds)?;
        let src = self
            .data
            .get(offset..end)
            .ok_or(BlockBackendError::OutOfBounds)?;
        dst.copy_from_slice(src);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), BlockBackendError> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| BlockBackendError::OutOfBounds)?;
        let end = offset
            .checked_add(src.len())
            .ok_or(BlockBackendError::OutOfBounds)?;
        let dst = self
            .data
            .get_mut(offset..end)
            .ok_or(BlockBackendError::OutOfBounds)?;
        dst.copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockBackendError> {
        Ok(())
    }

    fn device_id(&self) -> [u8; 20] {
        self.id
    }
}

pub struct VirtioBlk<B: BlockBackend> {
    backend: B,
    features: u64,
    config: VirtioBlkConfig,
}

impl<B: BlockBackend> VirtioBlk<B> {
    pub fn new(backend: B) -> Self {
        let queue_max_size = 128u16;
        let config = VirtioBlkConfig {
            capacity: backend.len() / VIRTIO_BLK_SECTOR_SIZE,
            // Contract v1: `size_max` is unused and MUST be 0.
            size_max: 0,
            seg_max: u32::from(queue_max_size.saturating_sub(2)),
            blk_size: backend.blk_size(),
        };
        Self {
            backend,
            features: 0,
            config,
        }
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

impl<B: BlockBackend + 'static> VirtioDevice for VirtioBlk<B> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_BLK
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1
            | VIRTIO_F_RING_INDIRECT_DESC
            | VIRTIO_BLK_F_SEG_MAX
            | VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
    }

    fn set_features(&mut self, features: u64) {
        self.features = features;
    }

    fn num_queues(&self) -> u16 {
        1
    }

    fn queue_max_size(&self, _queue: u16) -> u16 {
        128
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        if queue_index != 0 {
            return Err(VirtioDeviceError::Unsupported);
        }

        let descs = chain.descriptors();
        if descs.is_empty() {
            return Ok(false);
        }

        let status_desc = descs[descs.len() - 1];
        let can_write_status = status_desc.is_write_only() && status_desc.len != 0;

        // Read the 16-byte request header.
        let mut hdr = [0u8; 16];
        let mut hdr_written = 0usize;
        let mut d_idx = 0usize;
        let mut d_off = 0usize;
        let mut header_ok = true;
        while hdr_written < hdr.len() {
            if d_idx >= descs.len().saturating_sub(1) {
                header_ok = false;
                break;
            }
            let d = descs[d_idx];
            if d.is_write_only() {
                header_ok = false;
                break;
            }
            let avail = (d.len as usize).saturating_sub(d_off);
            if avail == 0 {
                d_idx += 1;
                d_off = 0;
                continue;
            }
            let take = avail.min(hdr.len() - hdr_written);
            let Some(addr) = d.addr.checked_add(d_off as u64) else {
                header_ok = false;
                break;
            };
            let Ok(src) = mem.get_slice(addr, take) else {
                header_ok = false;
                break;
            };
            hdr[hdr_written..hdr_written + take].copy_from_slice(src);
            hdr_written += take;
            d_off += take;
            if d_off == d.len as usize {
                d_idx += 1;
                d_off = 0;
            }
        }

        let mut status = if header_ok {
            VIRTIO_BLK_S_OK
        } else {
            VIRTIO_BLK_S_IOERR
        };

        // Build data segments (everything between header cursor and status descriptor).
        let mut data_segs = Vec::new();
        while d_idx < descs.len().saturating_sub(1) {
            let d = descs[d_idx];
            let seg_len = (d.len as usize).saturating_sub(d_off);
            if seg_len != 0 {
                data_segs.push((d, d_off, seg_len));
            }
            d_idx += 1;
            d_off = 0;
        }

        if header_ok {
            let typ = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let sector = u64::from_le_bytes(hdr[8..16].try_into().unwrap());

            match typ {
                VIRTIO_BLK_T_IN => {
                    let total_len: u64 = data_segs.iter().map(|(_, _, len)| *len as u64).sum();
                    if data_segs.is_empty() || !total_len.is_multiple_of(VIRTIO_BLK_SECTOR_SIZE) {
                        status = VIRTIO_BLK_S_IOERR;
                    } else if let Some(sector_off) = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE) {
                        if let Some(end_off) = sector_off.checked_add(total_len) {
                            if end_off > self.backend.len() {
                                status = VIRTIO_BLK_S_IOERR;
                            } else {
                                let mut offset = sector_off;
                                for (d, seg_off, seg_len) in &data_segs {
                                    if !d.is_write_only() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    let Some(addr) = d.addr.checked_add(*seg_off as u64) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    let Ok(dst) = mem.get_slice_mut(addr, *seg_len) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    if self.backend.read_at(offset, dst).is_err() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    offset = match offset.checked_add(*seg_len as u64) {
                                        Some(v) => v,
                                        None => {
                                            status = VIRTIO_BLK_S_IOERR;
                                            break;
                                        }
                                    };
                                }
                            }
                        } else {
                            status = VIRTIO_BLK_S_IOERR;
                        }
                    } else {
                        status = VIRTIO_BLK_S_IOERR;
                    }
                }
                VIRTIO_BLK_T_OUT => {
                    let total_len: u64 = data_segs.iter().map(|(_, _, len)| *len as u64).sum();
                    if data_segs.is_empty() || !total_len.is_multiple_of(VIRTIO_BLK_SECTOR_SIZE) {
                        status = VIRTIO_BLK_S_IOERR;
                    } else if let Some(sector_off) = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE) {
                        if let Some(end_off) = sector_off.checked_add(total_len) {
                            if end_off > self.backend.len() {
                                status = VIRTIO_BLK_S_IOERR;
                            } else {
                                let mut offset = sector_off;
                                for (d, seg_off, seg_len) in &data_segs {
                                    if d.is_write_only() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    let Some(addr) = d.addr.checked_add(*seg_off as u64) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    let Ok(src) = mem.get_slice(addr, *seg_len) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    if self.backend.write_at(offset, src).is_err() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    offset = match offset.checked_add(*seg_len as u64) {
                                        Some(v) => v,
                                        None => {
                                            status = VIRTIO_BLK_S_IOERR;
                                            break;
                                        }
                                    };
                                }
                            }
                        } else {
                            status = VIRTIO_BLK_S_IOERR;
                        }
                    } else {
                        status = VIRTIO_BLK_S_IOERR;
                    }
                }
                VIRTIO_BLK_T_FLUSH => {
                    if (self.features & VIRTIO_BLK_F_FLUSH) == 0 {
                        status = VIRTIO_BLK_S_UNSUPP;
                    } else if self.backend.flush().is_err() {
                        status = VIRTIO_BLK_S_IOERR;
                    }
                }
                VIRTIO_BLK_T_GET_ID => {
                    // The driver supplies a single data buffer (write-only) and expects up to 20
                    // bytes back. If the buffer is smaller, we write as much as fits; if larger, we
                    // truncate and still succeed.
                    if data_segs.is_empty() || data_segs.iter().all(|(_, _, len)| *len == 0) {
                        status = VIRTIO_BLK_S_IOERR;
                    } else {
                        let id = self.backend.device_id();
                        let mut remaining: &[u8] = &id;

                        for (d, seg_off, seg_len) in &data_segs {
                            if remaining.is_empty() {
                                break;
                            }
                            if !d.is_write_only() {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            }
                            let write_len = (*seg_len).min(remaining.len());
                            if write_len == 0 {
                                continue;
                            }
                            let Some(addr) = d.addr.checked_add(*seg_off as u64) else {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            };
                            let Ok(dst) = mem.get_slice_mut(addr, write_len) else {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            };
                            dst.copy_from_slice(&remaining[..write_len]);
                            remaining = &remaining[write_len..];
                        }
                    }
                }
                _ => status = VIRTIO_BLK_S_UNSUPP,
            }
        }

        if can_write_status {
            write_u8(mem, status_desc.addr, status).map_err(|_| VirtioDeviceError::IoError)?;
        }

        queue
            // Contract v1: virtio-blk drivers must not depend on used lengths.
            .add_used(mem, chain.head_index(), 0)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        self.config.read(offset, data);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Read-only for now.
    }

    fn reset(&mut self) {
        self.features = 0;
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockBackend, MemDisk, VirtioBlk, VIRTIO_BLK_S_OK, VIRTIO_BLK_T_GET_ID,
        VIRTIO_BLK_T_OUT,
    };
    use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
    use crate::devices::VirtioDevice;
    use crate::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
    use crate::queue::{VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

    fn write_desc(
        mem: &mut GuestRam,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    #[test]
    fn doc_example_open_raw_disk_as_virtio_blk_backend() {
        let disk = RawDisk::create(MemBackend::new(), (1024 * SECTOR_SIZE) as u64).unwrap();
        let mut blk = VirtioBlk::new(Box::new(disk));

        // Sanity-check that the adapter exposes the underlying disk capacity.
        assert_eq!(blk.backend_mut().len(), (1024 * SECTOR_SIZE) as u64);
    }

    #[test]
    fn virtio_blk_get_id_writes_backend_id_and_truncates() {
        let disk = MemDisk::new(4096);
        let expected_id = disk.device_id();
        let mut blk = VirtioBlk::new(disk);

        let desc_table: u64 = 0x1000;
        let avail_ring: u64 = 0x2000;
        let used_ring: u64 = 0x3000;

        let header: u64 = 0x4000;
        let data: u64 = 0x5000;
        let status: u64 = 0x6000;

        let mut mem = GuestRam::new(0x10000);

        // Request header: type + reserved + sector.
        write_u32_le(&mut mem, header, VIRTIO_BLK_T_GET_ID).unwrap();
        write_u32_le(&mut mem, header + 4, 0).unwrap();
        write_u64_le(&mut mem, header + 8, 0).unwrap();

        // Data buffer is larger than 20 bytes to ensure we don't write past the ID length.
        mem.write(data, &[0xccu8; 32]).unwrap();
        mem.write(status, &[0xaau8]).unwrap();

        // Descriptor chain: header (ro) -> data (wo) -> status (wo).
        write_desc(
            &mut mem,
            desc_table,
            0,
            header,
            16,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            data,
            32,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(&mut mem, desc_table, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);

        // Avail ring: one entry pointing at descriptor 0.
        write_u16_le(&mut mem, avail_ring, 0).unwrap(); // flags
        write_u16_le(&mut mem, avail_ring + 2, 1).unwrap(); // idx
        write_u16_le(&mut mem, avail_ring + 4, 0).unwrap(); // ring[0]

        // Used ring initial state.
        write_u16_le(&mut mem, used_ring, 0).unwrap(); // flags
        write_u16_le(&mut mem, used_ring + 2, 0).unwrap(); // idx

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: desc_table,
                avail_addr: avail_ring,
                used_addr: used_ring,
            },
            false,
        )
        .unwrap();

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            crate::queue::PoppedDescriptorChain::Chain(c) => c,
            crate::queue::PoppedDescriptorChain::Invalid { .. } => panic!("invalid chain"),
        };

        blk.process_queue(0, chain, &mut queue, &mut mem)
            .expect("process_queue failed");

        let written = mem.get_slice(data, 20).unwrap();
        assert_eq!(written, &expected_id);
        let untouched = mem.get_slice(data + 20, 12).unwrap();
        assert!(untouched.iter().all(|&b| b == 0xcc));
        assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_OK);
    }

    #[test]
    fn virtio_blk_get_id_rejects_readonly_data_buffer() {
        let disk = MemDisk::new(4096);
        let mut blk = VirtioBlk::new(disk);

        let desc_table: u64 = 0x1000;
        let avail_ring: u64 = 0x2000;
        let used_ring: u64 = 0x3000;

        let header: u64 = 0x4000;
        let data: u64 = 0x5000;
        let status: u64 = 0x6000;

        let mut mem = GuestRam::new(0x10000);

        // Header says GET_ID, but make the data descriptor read-only. Device should fail the
        // request and leave the data bytes unchanged.
        write_u32_le(&mut mem, header, VIRTIO_BLK_T_GET_ID).unwrap();
        write_u32_le(&mut mem, header + 4, 0).unwrap();
        write_u64_le(&mut mem, header + 8, 0).unwrap();

        mem.write(data, &[0xccu8; 20]).unwrap();

        // Status shares the same last descriptor semantics as other request types.
        mem.write(status, &[0xaau8]).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            header,
            16,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        // Data descriptor is read-only (no VIRTQ_DESC_F_WRITE).
        write_desc(&mut mem, desc_table, 1, data, 20, VIRTQ_DESC_F_NEXT, 2);
        write_desc(&mut mem, desc_table, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);

        write_u16_le(&mut mem, avail_ring, 0).unwrap(); // flags
        write_u16_le(&mut mem, avail_ring + 2, 1).unwrap(); // idx
        write_u16_le(&mut mem, avail_ring + 4, 0).unwrap(); // ring[0]

        write_u16_le(&mut mem, used_ring, 0).unwrap(); // flags
        write_u16_le(&mut mem, used_ring + 2, 0).unwrap(); // idx

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: desc_table,
                avail_addr: avail_ring,
                used_addr: used_ring,
            },
            false,
        )
        .unwrap();

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            crate::queue::PoppedDescriptorChain::Chain(c) => c,
            crate::queue::PoppedDescriptorChain::Invalid { .. } => panic!("invalid chain"),
        };

        blk.process_queue(0, chain, &mut queue, &mut mem)
            .expect("process_queue failed");

        assert!(mem.get_slice(data, 20).unwrap().iter().all(|&b| b == 0xcc));
        assert_ne!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_OK);
    }

    #[test]
    fn virtio_blk_get_id_is_not_confused_with_out_opcode() {
        // Regression guard: ensure we didn't accidentally reuse the OUT opcode constant when adding
        // GET_ID support.
        assert_ne!(VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_OUT);
    }
}
