use crate::io::storage::disk::{DiskBackend, DiskResult};
use crate::io::virtio::vio_core::{DescriptorChain, VirtQueue, VirtQueueError, VRING_DESC_F_WRITE};
use memory::GuestMemory;

pub const VIRTIO_BLK_SECTOR_SIZE: u64 = 512;

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

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

    pub fn read(&self, offset: usize, data: &mut [u8]) {
        let mut cfg = [0u8; Self::SIZE];
        cfg[0..8].copy_from_slice(&self.capacity.to_le_bytes());
        cfg[8..12].copy_from_slice(&self.size_max.to_le_bytes());
        cfg[12..16].copy_from_slice(&self.seg_max.to_le_bytes());
        // geometry is zeroed.
        cfg[20..24].copy_from_slice(&self.blk_size.to_le_bytes());

        if offset >= cfg.len() {
            data.fill(0);
            return;
        }

        let end = usize::min(cfg.len(), offset + data.len());
        data[..end - offset].copy_from_slice(&cfg[offset..end]);
        if end - offset < data.len() {
            data[end - offset..].fill(0);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VirtioBlkReq {
    req_type: u32,
    sector: u64,
}

pub struct VirtualDrive {
    backend: Box<dyn DiskBackend>,
}

impl VirtualDrive {
    pub fn new(backend: Box<dyn DiskBackend>) -> Self {
        Self { backend }
    }

    pub fn sector_size(&self) -> u32 {
        self.backend.sector_size()
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.backend
            .total_sectors()
            .saturating_mul(self.backend.sector_size() as u64)
    }

    pub fn capacity_512_sectors(&self) -> u64 {
        self.capacity_bytes() / VIRTIO_BLK_SECTOR_SIZE
    }

    pub fn backend_mut(&mut self) -> &mut dyn DiskBackend {
        &mut *self.backend
    }

    pub fn flush(&mut self) -> DiskResult<()> {
        self.backend.flush()
    }
}

impl core::fmt::Debug for VirtualDrive {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VirtualDrive")
            .field("sector_size", &self.sector_size())
            .field("total_sectors", &self.backend.total_sectors())
            .field("capacity_bytes", &self.capacity_bytes())
            .finish()
    }
}

#[derive(Debug)]
pub struct VirtioBlkDevice {
    pub config: VirtioBlkConfig,
    pub vq: VirtQueue,
    drive: VirtualDrive,
    isr_queue: bool,
}

impl VirtioBlkDevice {
    pub fn new(drive: VirtualDrive, vq: VirtQueue) -> Self {
        let config = VirtioBlkConfig {
            capacity: drive.capacity_512_sectors(),
            size_max: u32::MAX,
            seg_max: u32::from(vq.size.saturating_sub(2)),
            blk_size: drive.sector_size(),
        };
        Self {
            config,
            vq,
            drive,
            isr_queue: false,
        }
    }

    pub fn take_isr(&mut self) -> u8 {
        let isr = if self.isr_queue { 0x1 } else { 0x0 };
        self.isr_queue = false;
        isr
    }

    pub fn process_queue(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.vq.pop_available(mem)? {
            let used_len = self.process_chain(mem, &chain);
            if self.vq.push_used(mem, &chain, used_len)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    fn process_chain(&mut self, mem: &mut impl GuestMemory, chain: &DescriptorChain) -> u32 {
        // Expect at least header + status.
        if chain.descriptors.len() < 2 {
            return 0;
        }

        let status_desc = chain.descriptors[chain.descriptors.len() - 1];
        if status_desc.flags & VRING_DESC_F_WRITE == 0 || status_desc.len < 1 {
            return 0;
        }

        let mut used_len = 1u32;
        let status = match self.read_req(mem, chain) {
            Ok(req) => match req.req_type {
                VIRTIO_BLK_T_IN => match self.do_read(mem, chain, req.sector) {
                    Ok(bytes) => {
                        used_len = bytes.saturating_add(1);
                        VIRTIO_BLK_S_OK
                    }
                    Err(()) => VIRTIO_BLK_S_IOERR,
                },
                VIRTIO_BLK_T_OUT => match self.do_write(mem, chain, req.sector) {
                    Ok(()) => VIRTIO_BLK_S_OK,
                    Err(()) => VIRTIO_BLK_S_IOERR,
                },
                VIRTIO_BLK_T_FLUSH => match self.drive.flush() {
                    Ok(()) => VIRTIO_BLK_S_OK,
                    Err(_) => VIRTIO_BLK_S_IOERR,
                },
                _ => VIRTIO_BLK_S_UNSUPP,
            },
            Err(()) => VIRTIO_BLK_S_IOERR,
        };

        let _ = mem.write_u8_le(status_desc.addr, status);
        used_len
    }

    fn read_req(
        &self,
        mem: &impl GuestMemory,
        chain: &DescriptorChain,
    ) -> Result<VirtioBlkReq, ()> {
        let hdr = chain.descriptors[0];
        if hdr.flags & VRING_DESC_F_WRITE != 0 || hdr.len < 16 {
            return Err(());
        }
        let req_type = mem.read_u32_le(hdr.addr).map_err(|_| ())?;
        let sector = mem.read_u64_le(hdr.addr + 8).map_err(|_| ())?;
        Ok(VirtioBlkReq { req_type, sector })
    }

    fn do_read(
        &mut self,
        mem: &mut impl GuestMemory,
        chain: &DescriptorChain,
        sector: u64,
    ) -> Result<u32, ()> {
        if chain.descriptors.len() < 3 {
            return Err(());
        }
        let disk_sector_size = u64::from(self.drive.sector_size());
        let byte_offset = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE).ok_or(())?;
        if disk_sector_size == 0 || !byte_offset.is_multiple_of(disk_sector_size) {
            return Err(());
        }
        let mut lba = byte_offset / disk_sector_size;

        let mut written: u32 = 0;
        for desc in &chain.descriptors[1..chain.descriptors.len() - 1] {
            if desc.flags & VRING_DESC_F_WRITE == 0 {
                return Err(());
            }
            let len = desc.len as usize;
            if disk_sector_size == 0 || !(len as u64).is_multiple_of(disk_sector_size) {
                return Err(());
            }
            let sectors = (len as u64) / disk_sector_size;

            if let Some(dst) = mem.get_slice_mut(desc.addr, len) {
                self.drive
                    .backend_mut()
                    .read_sectors(lba, dst)
                    .map_err(|_| ())?;
            } else {
                let mut tmp = Vec::new();
                tmp.try_reserve_exact(len).map_err(|_| ())?;
                tmp.resize(len, 0);
                self.drive
                    .backend_mut()
                    .read_sectors(lba, &mut tmp)
                    .map_err(|_| ())?;
                mem.write_from(desc.addr, &tmp).map_err(|_| ())?;
            }
            lba = lba.checked_add(sectors).ok_or(())?;
            written = written.saturating_add(desc.len);
        }

        Ok(written)
    }

    fn do_write(
        &mut self,
        mem: &mut impl GuestMemory,
        chain: &DescriptorChain,
        sector: u64,
    ) -> Result<(), ()> {
        if chain.descriptors.len() < 3 {
            return Err(());
        }
        let disk_sector_size = u64::from(self.drive.sector_size());
        let byte_offset = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE).ok_or(())?;
        if disk_sector_size == 0 || !byte_offset.is_multiple_of(disk_sector_size) {
            return Err(());
        }
        let mut lba = byte_offset / disk_sector_size;

        for desc in &chain.descriptors[1..chain.descriptors.len() - 1] {
            if desc.flags & VRING_DESC_F_WRITE != 0 {
                return Err(());
            }
            let len = desc.len as usize;
            if disk_sector_size == 0 || !(len as u64).is_multiple_of(disk_sector_size) {
                return Err(());
            }
            let sectors = (len as u64) / disk_sector_size;

            if let Some(src) = mem.get_slice(desc.addr, len) {
                self.drive
                    .backend_mut()
                    .write_sectors(lba, src)
                    .map_err(|_| ())?;
            } else {
                let mut tmp = Vec::new();
                tmp.try_reserve_exact(len).map_err(|_| ())?;
                tmp.resize(len, 0);
                mem.read_into(desc.addr, &mut tmp).map_err(|_| ())?;
                self.drive
                    .backend_mut()
                    .write_sectors(lba, &tmp)
                    .map_err(|_| ())?;
            }
            lba = lba.checked_add(sectors).ok_or(())?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::storage::disk::DiskBackend;
    use crate::io::virtio::vio_core::{
        Descriptor, VirtQueue, VRING_AVAIL_F_NO_INTERRUPT, VRING_DESC_F_NEXT,
    };
    use memory::DenseMemory;
    use std::sync::{Arc, Mutex};

    const DESC_TABLE: u64 = 0x1000;
    const AVAIL_RING: u64 = 0x2000;
    const USED_RING: u64 = 0x3000;

    fn write_desc(mem: &mut DenseMemory, index: u16, desc: Descriptor) {
        let base = DESC_TABLE + (index as u64) * 16;
        mem.write_u64_le(base, desc.addr).unwrap();
        mem.write_u32_le(base + 8, desc.len).unwrap();
        mem.write_u16_le(base + 12, desc.flags).unwrap();
        mem.write_u16_le(base + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, flags: u16, head: u16) {
        mem.write_u16_le(AVAIL_RING, flags).unwrap();
        mem.write_u16_le(AVAIL_RING + 2, 1).unwrap(); // idx
        mem.write_u16_le(AVAIL_RING + 4, head).unwrap();
    }

    fn read_used_elem(mem: &DenseMemory, index: u16) -> (u32, u32) {
        let base = USED_RING + 4 + (index as u64) * 8;
        let id = mem.read_u32_le(base).unwrap();
        let len = mem.read_u32_le(base + 4).unwrap();
        (id, len)
    }

    #[derive(Clone)]
    struct SharedDisk {
        inner: Arc<Mutex<State>>,
    }

    #[derive(Default)]
    struct State {
        sector_size: u32,
        data: Vec<u8>,
        flush_count: usize,
    }

    impl SharedDisk {
        fn new(sectors: u64, sector_size: u32) -> Self {
            let len = usize::try_from(sectors * sector_size as u64).unwrap();
            Self {
                inner: Arc::new(Mutex::new(State {
                    sector_size,
                    data: vec![0; len],
                    flush_count: 0,
                })),
            }
        }

        fn write_bytes(&self, offset: usize, data: &[u8]) {
            let mut inner = self.inner.lock().unwrap();
            inner.data[offset..offset + data.len()].copy_from_slice(data);
        }

        fn read_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
            self.inner.lock().unwrap().data[offset..offset + len].to_vec()
        }

        fn flush_count(&self) -> usize {
            self.inner.lock().unwrap().flush_count
        }
    }

    impl DiskBackend for SharedDisk {
        fn sector_size(&self) -> u32 {
            self.inner.lock().unwrap().sector_size
        }

        fn total_sectors(&self) -> u64 {
            let inner = self.inner.lock().unwrap();
            inner.data.len() as u64 / inner.sector_size as u64
        }

        fn read_sectors(
            &mut self,
            lba: u64,
            buf: &mut [u8],
        ) -> Result<(), crate::io::storage::disk::DiskError> {
            let inner = self.inner.lock().unwrap();
            let offset = (lba * inner.sector_size as u64) as usize;
            let end = offset + buf.len();
            if end > inner.data.len() {
                return Err(crate::io::storage::disk::DiskError::OutOfBounds);
            }
            buf.copy_from_slice(&inner.data[offset..end]);
            Ok(())
        }

        fn write_sectors(
            &mut self,
            lba: u64,
            buf: &[u8],
        ) -> Result<(), crate::io::storage::disk::DiskError> {
            let mut inner = self.inner.lock().unwrap();
            let offset = (lba * inner.sector_size as u64) as usize;
            let end = offset + buf.len();
            if end > inner.data.len() {
                return Err(crate::io::storage::disk::DiskError::OutOfBounds);
            }
            inner.data[offset..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), crate::io::storage::disk::DiskError> {
            self.inner.lock().unwrap().flush_count += 1;
            Ok(())
        }
    }

    #[test]
    fn read_request_returns_correct_bytes() {
        let disk = SharedDisk::new(8, 512);
        disk.write_bytes(0, b"abcdefgh");

        let drive = VirtualDrive::new(Box::new(disk.clone()));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let data1_addr = 0x5000;
        let data2_addr = 0x5200;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_IN).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: data1_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            2,
            Descriptor {
                addr: data2_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            3,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        let irq = dev.process_queue(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut out = vec![0u8; 8];
        mem.read_into(data1_addr, &mut out).unwrap();
        assert_eq!(&out, b"abcdefgh");
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);

        assert_eq!(mem.read_u16_le(USED_RING + 2).unwrap(), 1);
        assert_eq!(read_used_elem(&mem, 0), (0, 1025)); // 1024 bytes + status
    }

    #[test]
    fn write_request_persists() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk.clone()));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let data1_addr = 0x5000;
        let data2_addr = 0x5200;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_OUT).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        mem.write_from(data1_addr, b"abcdefgh").unwrap();
        mem.write_from(data2_addr, &vec![0u8; 512]).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: data1_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            2,
            Descriptor {
                addr: data2_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            3,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        let irq = dev.process_queue(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(disk.read_bytes(0, 8), b"abcdefgh");
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
    }

    #[test]
    fn flush_request_calls_backend_flush() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk.clone()));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_FLUSH).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        let irq = dev.process_queue(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(disk.flush_count(), 1);
    }

    #[test]
    fn malformed_chains_return_ioerr_without_panic() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let data_addr = 0x5000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_IN).unwrap();
        // Intentionally provide a too-short header descriptor.

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 8,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: data_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            2,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        dev.process_queue(&mut mem).unwrap();
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_IOERR);
    }

    #[test]
    fn read_request_without_data_returns_ioerr() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_IN).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        dev.process_queue(&mut mem).unwrap();
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_IOERR);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
    }

    #[test]
    fn write_request_with_write_only_data_desc_returns_ioerr() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let data_addr = 0x5000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_OUT).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();
        mem.write_from(data_addr, &vec![0u8; 512]).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        // For OUT requests the data buffers must be read-only; mark it write-only to force IOERR.
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: data_addr,
                len: 512,
                flags: VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                next: 2,
            },
        );
        write_desc(
            &mut mem,
            2,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, 0, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        dev.process_queue(&mut mem).unwrap();
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_IOERR);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
    }

    #[test]
    fn no_interrupt_flag_suppresses_irq() {
        let disk = SharedDisk::new(8, 512);
        let drive = VirtualDrive::new(Box::new(disk));
        let vq = VirtQueue::new(8, DESC_TABLE, AVAIL_RING, USED_RING);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x8000).unwrap();
        let header_addr = 0x4000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_FLUSH).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        write_desc(
            &mut mem,
            0,
            Descriptor {
                addr: header_addr,
                len: 16,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            1,
            Descriptor {
                addr: status_addr,
                len: 1,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, VRING_AVAIL_F_NO_INTERRUPT, 0);
        mem.write_u16_le(USED_RING, 0).unwrap();
        mem.write_u16_le(USED_RING + 2, 0).unwrap();

        let irq = dev.process_queue(&mut mem).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0);
    }
}
