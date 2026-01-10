use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::write_u8;
use crate::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use crate::memory::GuestMemory;

pub const VIRTIO_DEVICE_TYPE_BLK: u16 = 2;

pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

pub trait BlockBackend {
    fn len(&self) -> u64;
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), ()>;
    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), ()>;
    fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }
    fn device_id(&self) -> [u8; 20] {
        [0; 20]
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

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), ()> {
        let end = offset as usize + dst.len();
        dst.copy_from_slice(&self.data[offset as usize..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), ()> {
        let end = offset as usize + src.len();
        self.data[offset as usize..end].copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }

    fn device_id(&self) -> [u8; 20] {
        self.id
    }
}

pub struct VirtioBlk<B: BlockBackend> {
    backend: B,
    features: u64,
}

impl<B: BlockBackend> VirtioBlk<B> {
    pub fn new(backend: B) -> Self {
        Self { backend, features: 0 }
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn capacity_sectors(&self) -> u64 {
        self.backend.len() / 512
    }
}

impl<B: BlockBackend + 'static> VirtioDevice for VirtioBlk<B> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_BLK
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_RING_EVENT_IDX | VIRTIO_BLK_F_FLUSH
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
        if descs.len() < 2 {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        let status_desc = descs[descs.len() - 1];
        if !status_desc.is_write_only() || status_desc.len == 0 {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        // Read the 16-byte request header.
        let mut hdr = [0u8; 16];
        let mut hdr_written = 0usize;
        let mut d_idx = 0usize;
        let mut d_off = 0usize;
        while hdr_written < hdr.len() {
            if d_idx >= descs.len() - 1 {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
            let d = descs[d_idx];
            if d.is_write_only() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
            let avail = d.len as usize - d_off;
            let take = avail.min(hdr.len() - hdr_written);
            let src = mem
                .get_slice(d.addr + d_off as u64, take)
                .map_err(|_| VirtioDeviceError::IoError)?;
            hdr[hdr_written..hdr_written + take].copy_from_slice(src);
            hdr_written += take;
            d_off += take;
            if d_off == d.len as usize {
                d_idx += 1;
                d_off = 0;
            }
        }

        let typ = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let sector = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
        let mut status = VIRTIO_BLK_S_OK;

        // Build data segments (everything between header cursor and status descriptor).
        let mut data_segs = Vec::new();
        while d_idx < descs.len() - 1 {
            let d = descs[d_idx];
            let seg_len = d.len as usize - d_off;
            if seg_len != 0 {
                data_segs.push((d, d_off, seg_len));
            }
            d_idx += 1;
            d_off = 0;
        }

        let mut written_bytes: u32 = 0;
        match typ {
            VIRTIO_BLK_T_IN => {
                let mut offset = sector
                    .checked_mul(512)
                    .ok_or(VirtioDeviceError::IoError)?;
                for (d, seg_off, seg_len) in &data_segs {
                    if !d.is_write_only() {
                        return Err(VirtioDeviceError::BadDescriptorChain);
                    }
                    let dst = mem
                        .get_slice_mut(d.addr + *seg_off as u64, *seg_len)
                        .map_err(|_| VirtioDeviceError::IoError)?;
                    if self.backend.read_at(offset, dst).is_err() {
                        status = VIRTIO_BLK_S_IOERR;
                        break;
                    }
                    offset += *seg_len as u64;
                    written_bytes = written_bytes.saturating_add(*seg_len as u32);
                }
            }
            VIRTIO_BLK_T_OUT => {
                let mut offset = sector
                    .checked_mul(512)
                    .ok_or(VirtioDeviceError::IoError)?;
                for (d, seg_off, seg_len) in &data_segs {
                    if d.is_write_only() {
                        return Err(VirtioDeviceError::BadDescriptorChain);
                    }
                    let src = mem
                        .get_slice(d.addr + *seg_off as u64, *seg_len)
                        .map_err(|_| VirtioDeviceError::IoError)?;
                    if self.backend.write_at(offset, src).is_err() {
                        status = VIRTIO_BLK_S_IOERR;
                        break;
                    }
                    offset += *seg_len as u64;
                }
                written_bytes = 0;
            }
            VIRTIO_BLK_T_FLUSH => {
                if (self.features & VIRTIO_BLK_F_FLUSH) == 0 {
                    status = VIRTIO_BLK_S_UNSUPP;
                } else if self.backend.flush().is_err() {
                    status = VIRTIO_BLK_S_IOERR;
                }
                written_bytes = 0;
            }
            VIRTIO_BLK_T_GET_ID => {
                let id = self.backend.device_id();
                let mut copied = 0usize;
                for (d, seg_off, seg_len) in &data_segs {
                    if !d.is_write_only() {
                        return Err(VirtioDeviceError::BadDescriptorChain);
                    }
                    let take = (*seg_len).min(id.len() - copied);
                    if take == 0 {
                        break;
                    }
                    let dst = mem
                        .get_slice_mut(d.addr + *seg_off as u64, take)
                        .map_err(|_| VirtioDeviceError::IoError)?;
                    dst.copy_from_slice(&id[copied..copied + take]);
                    copied += take;
                    written_bytes = written_bytes.saturating_add(take as u32);
                }
            }
            _ => {
                status = VIRTIO_BLK_S_UNSUPP;
                written_bytes = 0;
            }
        }

        // Status byte.
        write_u8(mem, status_desc.addr, status).map_err(|_| VirtioDeviceError::IoError)?;
        written_bytes = written_bytes.saturating_add(1);

        let need_irq = queue
            .add_used(mem, chain.head_index(), written_bytes)
            .map_err(|_| VirtioDeviceError::IoError)?;
        Ok(need_irq)
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // virtio-blk device config: first field is `capacity` in 512-byte sectors.
        data.fill(0);
        let cap = self.capacity_sectors().to_le_bytes();
        let start = offset as usize;
        if start < cap.len() {
            let end = (start + data.len()).min(cap.len());
            data[..end - start].copy_from_slice(&cap[start..end]);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Read-only for now.
    }

    fn reset(&mut self) {
        self.features = 0;
    }
}
