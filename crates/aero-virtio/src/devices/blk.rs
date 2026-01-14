use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::write_u8;
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use aero_storage::{DiskError, VirtualDisk, SECTOR_SIZE};

pub const VIRTIO_DEVICE_TYPE_BLK: u16 = 2;

pub const VIRTIO_BLK_SECTOR_SIZE: u64 = SECTOR_SIZE as u64;

pub const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
pub const VIRTIO_BLK_F_DISCARD: u64 = 1 << 13;
pub const VIRTIO_BLK_F_WRITE_ZEROES: u64 = 1 << 14;

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;
pub const VIRTIO_BLK_T_DISCARD: u32 = 11;
pub const VIRTIO_BLK_T_WRITE_ZEROES: u32 = 13;

// `struct virtio_blk_discard_write_zeroes::flags` (virtio spec).
pub const VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP: u32 = 1 << 0;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// DoS guard: maximum number of descriptors processed for a single virtio-blk request.
///
/// This includes the request header and status descriptors.
pub const VIRTIO_BLK_MAX_REQUEST_DESCRIPTORS: usize = 1024;

/// DoS guard: maximum total data payload bytes for a single virtio-blk request.
///
/// This matches the 4MiB cap used by Aero's NVMe model (`NVME_MAX_DMA_BYTES`).
pub const VIRTIO_BLK_MAX_REQUEST_DATA_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum number of 512-byte sectors that may be affected by a single request.
pub const VIRTIO_BLK_MAX_REQUEST_SECTORS: u64 =
    VIRTIO_BLK_MAX_REQUEST_DATA_BYTES / VIRTIO_BLK_SECTOR_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkConfig {
    /// Capacity in 512-byte sectors.
    pub capacity: u64,
    pub size_max: u32,
    pub seg_max: u32,
    pub blk_size: u32,
}

impl VirtioBlkConfig {
    // Linux `struct virtio_blk_config` layout (virtio spec):
    // capacity (8) + size_max (4) + seg_max (4) + geometry (4) + blk_size (4) +
    // topology (8) + writeback (1) + unused0 (3) +
    // max_discard_sectors (4) + max_discard_seg (4) + discard_sector_alignment (4) +
    // max_write_zeroes_sectors (4) + max_write_zeroes_seg (4) + write_zeroes_may_unmap (1) +
    // unused1 (3)
    pub const SIZE: usize = 60;

    pub fn read(&self, offset: u64, data: &mut [u8]) {
        let mut cfg = [0u8; Self::SIZE];
        cfg[0..8].copy_from_slice(&self.capacity.to_le_bytes());
        cfg[8..12].copy_from_slice(&self.size_max.to_le_bytes());
        cfg[12..16].copy_from_slice(&self.seg_max.to_le_bytes());
        // geometry is zeroed.
        cfg[20..24].copy_from_slice(&self.blk_size.to_le_bytes());
        // topology + writeback are left as zero.

        // Discard / write zeroes limits. These are safe upper bounds for our current best-effort
        // implementation; they mainly exist so in-guest drivers can enable the operations when the
        // corresponding feature bits are negotiated.
        let max_sectors = u32::try_from(VIRTIO_BLK_MAX_REQUEST_SECTORS).unwrap_or(u32::MAX);
        cfg[36..40].copy_from_slice(&max_sectors.to_le_bytes()); // max_discard_sectors
        cfg[40..44].copy_from_slice(&self.seg_max.to_le_bytes()); // max_discard_seg
        let align_sectors = (u64::from(self.blk_size) / VIRTIO_BLK_SECTOR_SIZE).max(1);
        let align_sectors_u32 = u32::try_from(align_sectors).unwrap_or(1);
        cfg[44..48].copy_from_slice(&align_sectors_u32.to_le_bytes()); // discard_sector_alignment
        cfg[48..52].copy_from_slice(&max_sectors.to_le_bytes()); // max_write_zeroes_sectors
        cfg[52..56].copy_from_slice(&self.seg_max.to_le_bytes()); // max_write_zeroes_seg
                                                                  // write_zeroes_may_unmap: allow `WRITE_ZEROES` to deallocate underlying storage ("unmap")
                                                                  // while preserving guest-visible read-after-write semantics (reads must return zero).
        cfg[56] = 1;

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

#[derive(Debug, Clone)]
pub struct MemDisk {
    data: Vec<u8>,
}

impl MemDisk {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl VirtualDisk for MemDisk {
    fn capacity_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        let capacity = self.capacity_bytes();
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + buf.len();
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        let capacity = self.capacity_bytes();
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + buf.len();
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        Ok(())
    }
}

const DEFAULT_DEVICE_ID: [u8; 20] = *b"aero-virtio-blk-id!!";

pub struct VirtioBlk {
    disk: Box<dyn VirtualDisk>,
    device_id: [u8; 20],
    features: u64,
    config: VirtioBlkConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiscardWriteZeroesSegment {
    sector: u64,
    num_sectors: u32,
    flags: u32,
}

fn parse_discard_write_zeroes_segments(
    mem: &dyn GuestMemory,
    data_segs: &[(crate::queue::Descriptor, usize, usize)],
    max_segs: u32,
) -> Result<Vec<DiscardWriteZeroesSegment>, ()> {
    for (d, _off, _len) in data_segs.iter() {
        if d.is_write_only() {
            return Err(());
        }
    }

    let total_len: u64 = data_segs.iter().map(|(_, _, len)| *len as u64).sum();
    if total_len == 0 || !total_len.is_multiple_of(16) {
        return Err(());
    }
    let seg_count_u64 = total_len / 16;
    let seg_count_u32 = u32::try_from(seg_count_u64).map_err(|_| ())?;
    if seg_count_u32 > max_segs {
        return Err(());
    }
    let seg_count = seg_count_u32 as usize;

    // Stream the segment table across the descriptor list. We intentionally avoid allocating a
    // contiguous `Vec<u8>` based on guest-provided lengths.
    let mut segs = Vec::with_capacity(seg_count);
    let mut d_idx = 0usize;
    let mut d_off = 0usize;
    for _ in 0..seg_count {
        let mut buf = [0u8; 16];
        let mut written = 0usize;
        while written < buf.len() {
            if d_idx >= data_segs.len() {
                return Err(());
            }
            let (d, seg_off, seg_len) = data_segs[d_idx];
            let avail = seg_len.saturating_sub(d_off);
            if avail == 0 {
                d_idx += 1;
                d_off = 0;
                continue;
            }
            let take = avail.min(buf.len() - written);
            let Some(addr) = d.addr.checked_add((seg_off + d_off) as u64) else {
                return Err(());
            };
            let Ok(()) = mem.read(addr, &mut buf[written..written + take]) else {
                return Err(());
            };
            written += take;
            d_off += take;
            if d_off == seg_len {
                d_idx += 1;
                d_off = 0;
            }
        }

        let sector = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let num_sectors = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let flags = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        segs.push(DiscardWriteZeroesSegment {
            sector,
            num_sectors,
            flags,
        });
    }

    Ok(segs)
}

impl VirtioBlk {
    pub fn new(disk: Box<dyn VirtualDisk>) -> Self {
        let queue_max_size = 128u16;
        let config = VirtioBlkConfig {
            capacity: disk.capacity_bytes() / VIRTIO_BLK_SECTOR_SIZE,
            // Contract v1: `size_max` is unused and MUST be 0.
            size_max: 0,
            seg_max: u32::from(queue_max_size.saturating_sub(2)),
            // Virtio requests are still in 512-byte sectors.
            blk_size: VIRTIO_BLK_SECTOR_SIZE as u32,
        };
        Self {
            disk,
            device_id: DEFAULT_DEVICE_ID,
            features: 0,
            config,
        }
    }

    pub fn disk_mut(&mut self) -> &mut dyn VirtualDisk {
        &mut *self.disk
    }

    pub fn device_id(&self) -> [u8; 20] {
        self.device_id
    }
}

impl VirtioDevice for VirtioBlk {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_BLK
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1
            | VIRTIO_F_RING_INDIRECT_DESC
            | VIRTIO_BLK_F_SEG_MAX
            | VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
            | VIRTIO_BLK_F_DISCARD
            | VIRTIO_BLK_F_WRITE_ZEROES
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

        // DoS guard: avoid unbounded per-request work/allocations on pathological descriptor chains
        // (especially when indirect descriptors are enabled).
        if descs.len() > VIRTIO_BLK_MAX_REQUEST_DESCRIPTORS {
            let status = VIRTIO_BLK_S_IOERR;
            if can_write_status {
                // Best-effort: if the status buffer is invalid/out-of-bounds, still advance the
                // used ring so the guest can reclaim the descriptor chain.
                let _ = write_u8(mem, status_desc.addr, status);
            }
            return queue
                // Contract v1: virtio-blk drivers must not depend on used lengths.
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError);
        }

        // If the status descriptor is invalid, treat the whole request as invalid. We still
        // advance the used ring so the guest can reclaim the descriptors, but we avoid touching
        // any other guest buffers or backend state.
        if !can_write_status {
            return queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError);
        }

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
            let Ok(()) = mem.read(addr, &mut hdr[hdr_written..hdr_written + take]) else {
                header_ok = false;
                break;
            };
            hdr_written += take;
            d_off += take;
            if d_off == d.len as usize {
                d_idx += 1;
                d_off = 0;
            }
        }

        if !header_ok {
            // Header is malformed (short, out-of-bounds, or wrong direction). Fail the request
            // without scanning the remaining descriptors so malformed chains can't force extra
            // work beyond the header read.
            if can_write_status {
                let _ = write_u8(mem, status_desc.addr, VIRTIO_BLK_S_IOERR);
            }
            return queue
                // Contract v1: virtio-blk drivers must not depend on used lengths.
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError);
        }

        // Build data segments (everything between header cursor and status descriptor), enforcing
        // a per-request payload cap to avoid unbounded allocations or work.
        let mut data_segs: Vec<(crate::queue::Descriptor, usize, usize)> = Vec::new();
        let mut total_data_len: u64 = 0;
        let mut data_len_ok = true;
        let data_end_idx = descs.len().saturating_sub(1);
        let mut seg_idx = d_idx;
        let mut seg_off = d_off;
        while seg_idx < data_end_idx {
            let d = descs[seg_idx];
            let d_len = d.len as usize;
            if seg_off > d_len {
                data_len_ok = false;
                break;
            }
            let seg_len = d_len - seg_off;
            if seg_len != 0 {
                let seg_len_u64 = u64::try_from(seg_len).unwrap_or(u64::MAX);
                total_data_len = match total_data_len.checked_add(seg_len_u64) {
                    Some(v) => v,
                    None => {
                        data_len_ok = false;
                        break;
                    }
                };
                if total_data_len > VIRTIO_BLK_MAX_REQUEST_DATA_BYTES {
                    data_len_ok = false;
                    break;
                }
                data_segs.push((d, seg_off, seg_len));
            }
            seg_idx += 1;
            seg_off = 0;
        }

        let mut status = VIRTIO_BLK_S_IOERR;
        if data_len_ok {
            status = VIRTIO_BLK_S_OK;

            let typ = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let sector = u64::from_le_bytes(hdr[8..16].try_into().unwrap());

            match typ {
                VIRTIO_BLK_T_IN => {
                    if data_segs.is_empty()
                        || !total_data_len.is_multiple_of(VIRTIO_BLK_SECTOR_SIZE)
                    {
                        status = VIRTIO_BLK_S_IOERR;
                    } else if let Some(sector_off) = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE) {
                        if let Some(end_off) = sector_off.checked_add(total_data_len) {
                            if end_off > self.disk.capacity_bytes() {
                                status = VIRTIO_BLK_S_IOERR;
                            } else {
                                let mut offset = sector_off;
                                // Chunked I/O buffer so we don't need to borrow a `&mut [u8]`
                                // directly into guest memory.
                                let mut scratch = vec![0u8; 64 * 1024];
                                for (d, seg_off, seg_len) in &data_segs {
                                    if !d.is_write_only() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    let Some(addr) = d.addr.checked_add(*seg_off as u64) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    let mut remaining = *seg_len;
                                    let mut cur_addr = addr;
                                    while remaining != 0 {
                                        let take = remaining.min(scratch.len());
                                        if self.disk.read_at(offset, &mut scratch[..take]).is_err()
                                            || mem.write(cur_addr, &scratch[..take]).is_err()
                                        {
                                            status = VIRTIO_BLK_S_IOERR;
                                            break;
                                        }
                                        offset = match offset.checked_add(take as u64) {
                                            Some(v) => v,
                                            None => {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break;
                                            }
                                        };
                                        cur_addr = match cur_addr.checked_add(take as u64) {
                                            Some(v) => v,
                                            None => {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break;
                                            }
                                        };
                                        remaining = remaining.saturating_sub(take);
                                    }
                                    if status != VIRTIO_BLK_S_OK {
                                        break;
                                    }
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
                    if data_segs.is_empty()
                        || !total_data_len.is_multiple_of(VIRTIO_BLK_SECTOR_SIZE)
                    {
                        status = VIRTIO_BLK_S_IOERR;
                    } else if let Some(sector_off) = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE) {
                        if let Some(end_off) = sector_off.checked_add(total_data_len) {
                            if end_off > self.disk.capacity_bytes() {
                                status = VIRTIO_BLK_S_IOERR;
                            } else {
                                let mut offset = sector_off;
                                let mut scratch = vec![0u8; 64 * 1024];
                                for (d, seg_off, seg_len) in &data_segs {
                                    if d.is_write_only() {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                    let Some(addr) = d.addr.checked_add(*seg_off as u64) else {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    };
                                    let mut remaining = *seg_len;
                                    let mut cur_addr = addr;
                                    while remaining != 0 {
                                        let take = remaining.min(scratch.len());
                                        if mem.read(cur_addr, &mut scratch[..take]).is_err()
                                            || self.disk.write_at(offset, &scratch[..take]).is_err()
                                        {
                                            status = VIRTIO_BLK_S_IOERR;
                                            break;
                                        }
                                        offset = match offset.checked_add(take as u64) {
                                            Some(v) => v,
                                            None => {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break;
                                            }
                                        };
                                        cur_addr = match cur_addr.checked_add(take as u64) {
                                            Some(v) => v,
                                            None => {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break;
                                            }
                                        };
                                        remaining = remaining.saturating_sub(take);
                                    }
                                    if status != VIRTIO_BLK_S_OK {
                                        break;
                                    }
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
                    } else if self.disk.flush().is_err() {
                        status = VIRTIO_BLK_S_IOERR;
                    }
                }
                VIRTIO_BLK_T_GET_ID => {
                    // The driver supplies a data buffer (write-only) and expects up to 20 bytes
                    // back. If the buffer is smaller, we write as much as fits; if larger, we
                    // truncate and still succeed.
                    if data_segs.is_empty() {
                        status = VIRTIO_BLK_S_IOERR;
                    } else {
                        let mut remaining: &[u8] = &self.device_id;
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
                            if mem.write(addr, &remaining[..write_len]).is_err() {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            };
                            remaining = &remaining[write_len..];
                        }
                    }
                }
                VIRTIO_BLK_T_DISCARD => {
                    if (self.features & VIRTIO_BLK_F_DISCARD) == 0 {
                        status = VIRTIO_BLK_S_UNSUPP;
                    } else {
                        let segs = match parse_discard_write_zeroes_segments(
                            mem,
                            &data_segs,
                            self.config.seg_max,
                        ) {
                            Ok(v) => v,
                            Err(_) => {
                                status = VIRTIO_BLK_S_IOERR;
                                Vec::new()
                            }
                        };

                        if status == VIRTIO_BLK_S_OK {
                            let blk_size =
                                u64::from(self.config.blk_size).max(VIRTIO_BLK_SECTOR_SIZE);
                            let align_sectors = (blk_size / VIRTIO_BLK_SECTOR_SIZE).max(1);
                            let disk_len = self.disk.capacity_bytes();

                            // Validate all segments up-front so a rejected request cannot partially
                            // modify backend state.
                            let mut validated: Vec<(u64, u64)> = Vec::with_capacity(segs.len());
                            let mut total_sectors: u64 = 0;
                            for seg in &segs {
                                let num_sectors = u64::from(seg.num_sectors);
                                total_sectors = match total_sectors.checked_add(num_sectors) {
                                    Some(v) => v,
                                    None => {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                };
                                if total_sectors > VIRTIO_BLK_MAX_REQUEST_SECTORS {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }
                                if seg.sector % align_sectors != 0
                                    || num_sectors % align_sectors != 0
                                {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }

                                let Some(byte_off) = seg.sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE)
                                else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                let Some(byte_len) =
                                    num_sectors.checked_mul(VIRTIO_BLK_SECTOR_SIZE)
                                else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                let Some(end_off) = byte_off.checked_add(byte_len) else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                if end_off > disk_len {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }

                                validated.push((byte_off, byte_len));
                            }
                            // Best-effort: discard is advisory. Prefer disk hole-punching, but
                            // emulate by writing zeros when the disk doesn't reclaim (common for raw
                            // disks).
                            if status == VIRTIO_BLK_S_OK {
                                // Buffer used for chunked zero writes. Use a block-size-aligned
                                // chunk so backends that care about write alignment are not
                                // penalized.
                                let blk_usize = usize::try_from(blk_size).unwrap_or(SECTOR_SIZE);
                                let max_chunk = 64 * 1024usize;
                                let mut chunk_size = if blk_usize > max_chunk {
                                    blk_usize
                                } else {
                                    (max_chunk / blk_usize).saturating_mul(blk_usize)
                                };
                                chunk_size = chunk_size.max(blk_usize).max(SECTOR_SIZE);
                                let zero_buf = vec![0u8; chunk_size];
                                let mut read_buf = vec![0u8; chunk_size];

                                for (byte_off, byte_len) in validated {
                                    if byte_len == 0 {
                                        continue;
                                    }

                                    let discard_result =
                                        self.disk.discard_range(byte_off, byte_len);
                                    let mut needs_zero_fallback = discard_result.is_err();

                                    if !needs_zero_fallback {
                                        // If `discard_range` was a no-op, ensure guest-visible
                                        // semantics by scanning the discarded range and writing
                                        // zeros only for chunks that still contain non-zero bytes.
                                        //
                                        // This preserves hole-punching on sparse backends: if a
                                        // chunk already reads as zero (e.g. fully deallocated
                                        // sparse block), we skip the explicit zero write and avoid
                                        // re-allocating the block.
                                        let mut scan_off = byte_off;
                                        let mut scan_remaining = byte_len;
                                        while scan_remaining != 0 {
                                            let take =
                                                scan_remaining.min(read_buf.len() as u64) as usize;
                                            match self.disk.read_at(scan_off, &mut read_buf[..take])
                                            {
                                                Ok(()) => {
                                                    if read_buf[..take].iter().any(|b| *b != 0)
                                                        && self
                                                            .disk
                                                            .write_at(scan_off, &zero_buf[..take])
                                                            .is_err()
                                                    {
                                                        status = VIRTIO_BLK_S_IOERR;
                                                        break;
                                                    }
                                                }
                                                Err(_) => {
                                                    needs_zero_fallback = true;
                                                    break;
                                                }
                                            }
                                            scan_off = match scan_off.checked_add(take as u64) {
                                                Some(v) => v,
                                                None => {
                                                    needs_zero_fallback = true;
                                                    break;
                                                }
                                            };
                                            scan_remaining =
                                                scan_remaining.saturating_sub(take as u64);
                                        }
                                    }

                                    if needs_zero_fallback && status == VIRTIO_BLK_S_OK {
                                        let mut remaining = byte_len;
                                        let mut cur_off = byte_off;
                                        while remaining != 0 {
                                            let take =
                                                remaining.min(zero_buf.len() as u64) as usize;
                                            if self
                                                .disk
                                                .write_at(cur_off, &zero_buf[..take])
                                                .is_err()
                                            {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break;
                                            }
                                            cur_off = match cur_off.checked_add(take as u64) {
                                                Some(v) => v,
                                                None => {
                                                    status = VIRTIO_BLK_S_IOERR;
                                                    break;
                                                }
                                            };
                                            remaining = remaining.saturating_sub(take as u64);
                                        }
                                    }

                                    if status != VIRTIO_BLK_S_OK {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                VIRTIO_BLK_T_WRITE_ZEROES => {
                    if (self.features & VIRTIO_BLK_F_WRITE_ZEROES) == 0 {
                        status = VIRTIO_BLK_S_UNSUPP;
                    } else {
                        let segs = match parse_discard_write_zeroes_segments(
                            mem,
                            &data_segs,
                            self.config.seg_max,
                        ) {
                            Ok(v) => v,
                            Err(_) => {
                                status = VIRTIO_BLK_S_IOERR;
                                Vec::new()
                            }
                        };

                        if status == VIRTIO_BLK_S_OK {
                            let blk_size =
                                u64::from(self.config.blk_size).max(VIRTIO_BLK_SECTOR_SIZE);
                            let align_sectors = (blk_size / VIRTIO_BLK_SECTOR_SIZE).max(1);
                            let disk_len = self.disk.capacity_bytes();

                            // Validate all segments up-front so a rejected request cannot partially
                            // modify disk state.
                            let mut validated: Vec<(u64, u64, u32)> =
                                Vec::with_capacity(segs.len());
                            let mut total_sectors: u64 = 0;
                            for seg in &segs {
                                let num_sectors = u64::from(seg.num_sectors);
                                total_sectors = match total_sectors.checked_add(num_sectors) {
                                    Some(v) => v,
                                    None => {
                                        status = VIRTIO_BLK_S_IOERR;
                                        break;
                                    }
                                };
                                if total_sectors > VIRTIO_BLK_MAX_REQUEST_SECTORS {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }
                                if seg.sector % align_sectors != 0
                                    || num_sectors % align_sectors != 0
                                {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }
                                let Some(byte_off) = seg.sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE)
                                else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                let Some(byte_len) =
                                    num_sectors.checked_mul(VIRTIO_BLK_SECTOR_SIZE)
                                else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                let Some(end_off) = byte_off.checked_add(byte_len) else {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                };
                                if end_off > disk_len {
                                    status = VIRTIO_BLK_S_IOERR;
                                    break;
                                }
                                validated.push((byte_off, byte_len, seg.flags));
                            }

                            if status == VIRTIO_BLK_S_OK {
                                // Buffer used for chunked zero writes. Use a block-size-aligned chunk
                                // so backends that care about write alignment are not penalized.
                                let blk_usize = usize::try_from(blk_size).unwrap_or(SECTOR_SIZE);
                                let max_chunk = 64 * 1024usize;
                                let mut chunk_size = if blk_usize > max_chunk {
                                    blk_usize
                                } else {
                                    (max_chunk / blk_usize).saturating_mul(blk_usize)
                                };
                                chunk_size = chunk_size.max(blk_usize).max(SECTOR_SIZE);
                                let zero_buf = vec![0u8; chunk_size];
                                let needs_read_buf = validated.iter().any(|(_, _, flags)| {
                                    (flags & VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP) != 0
                                });
                                let mut read_buf = if needs_read_buf {
                                    vec![0u8; chunk_size]
                                } else {
                                    Vec::new()
                                };

                                'seg_loop: for (byte_off, byte_len, flags) in validated {
                                    if byte_len == 0 {
                                        continue;
                                    }

                                    // If the driver requests UNMAP, treat WRITE_ZEROES as a best-effort
                                    // discard (hole punch) and fall back to explicit zero writes only
                                    // if needed to enforce read-after-write semantics.
                                    if (flags & VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP) != 0 {
                                        let mut needs_zero_fallback =
                                            self.disk.discard_range(byte_off, byte_len).is_err();

                                        if !needs_zero_fallback {
                                            // If `discard_range` was a no-op, ensure guest-visible
                                            // semantics by scanning the discarded range and writing
                                            // zeros only for chunks that still contain non-zero bytes.
                                            //
                                            // This preserves hole-punching on sparse backends: if a
                                            // chunk already reads as zero (e.g. fully deallocated
                                            // sparse block), we skip the explicit zero write and avoid
                                            // re-allocating the block.
                                            let mut scan_off = byte_off;
                                            let mut scan_remaining = byte_len;
                                            while scan_remaining != 0 {
                                                let take = scan_remaining.min(read_buf.len() as u64)
                                                    as usize;
                                                match self
                                                    .disk
                                                    .read_at(scan_off, &mut read_buf[..take])
                                                {
                                                    Ok(()) => {
                                                        if read_buf[..take].iter().any(|&b| b != 0)
                                                            && self
                                                                .disk
                                                                .write_at(
                                                                    scan_off,
                                                                    &zero_buf[..take],
                                                                )
                                                                .is_err()
                                                        {
                                                            status = VIRTIO_BLK_S_IOERR;
                                                            break 'seg_loop;
                                                        }
                                                    }
                                                    Err(_) => {
                                                        needs_zero_fallback = true;
                                                        break;
                                                    }
                                                }
                                                scan_off = match scan_off.checked_add(take as u64) {
                                                    Some(v) => v,
                                                    None => {
                                                        needs_zero_fallback = true;
                                                        break;
                                                    }
                                                };
                                                scan_remaining =
                                                    scan_remaining.saturating_sub(take as u64);
                                            }
                                        }

                                        if needs_zero_fallback && status == VIRTIO_BLK_S_OK {
                                            let mut cur_off = byte_off;
                                            let mut remaining = byte_len;
                                            while remaining != 0 {
                                                let take =
                                                    remaining.min(zero_buf.len() as u64) as usize;
                                                if self
                                                    .disk
                                                    .write_at(cur_off, &zero_buf[..take])
                                                    .is_err()
                                                {
                                                    status = VIRTIO_BLK_S_IOERR;
                                                    break 'seg_loop;
                                                }
                                                cur_off = match cur_off.checked_add(take as u64) {
                                                    Some(v) => v,
                                                    None => {
                                                        status = VIRTIO_BLK_S_IOERR;
                                                        break 'seg_loop;
                                                    }
                                                };
                                                remaining = remaining.saturating_sub(take as u64);
                                            }
                                        }
                                    } else {
                                        let mut cur_off = byte_off;
                                        let mut remaining = byte_len;
                                        while remaining != 0 {
                                            let take =
                                                remaining.min(zero_buf.len() as u64) as usize;
                                            if self
                                                .disk
                                                .write_at(cur_off, &zero_buf[..take])
                                                .is_err()
                                            {
                                                status = VIRTIO_BLK_S_IOERR;
                                                break 'seg_loop;
                                            }
                                            cur_off = match cur_off.checked_add(take as u64) {
                                                Some(v) => v,
                                                None => {
                                                    status = VIRTIO_BLK_S_IOERR;
                                                    break 'seg_loop;
                                                }
                                            };
                                            remaining = remaining.saturating_sub(take as u64);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => status = VIRTIO_BLK_S_UNSUPP,
            }
        }

        // Best-effort: if the status buffer is invalid/out-of-bounds, still advance the used ring
        // so the guest can reclaim the descriptor chain.
        if can_write_status {
            let _ = write_u8(mem, status_desc.addr, status);
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
    use super::{MemDisk, VirtioBlk, VIRTIO_BLK_S_OK, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_OUT};
    use crate::devices::VirtioDevice;
    use crate::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
    use crate::queue::{VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
    use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};

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

        // Sanity-check that virtio-blk sees the underlying disk capacity.
        assert_eq!(blk.disk_mut().capacity_bytes(), (1024 * SECTOR_SIZE) as u64);
    }

    #[test]
    fn virtio_blk_get_id_writes_backend_id_and_truncates() {
        let disk = Box::new(MemDisk::new(4096));
        let mut blk = VirtioBlk::new(disk);
        let expected_id = blk.device_id();

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
        write_desc(&mut mem, desc_table, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
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
        let disk = Box::new(MemDisk::new(4096));
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

        write_desc(&mut mem, desc_table, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
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
