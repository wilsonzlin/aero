use crate::io::virtio::core::{
    DescChain, GuestMemory, VirtQueue, VirtQueueError, VIRTQ_DESC_F_WRITE,
};
use crate::storage::VirtualDrive;

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

#[derive(Debug, Clone, Copy)]
struct VirtioBlkReqHeader {
    req_type: u32,
    sector: u64,
}

pub trait VirtioInterrupt {
    fn trigger(&mut self);
}

#[derive(Debug)]
pub struct VirtioBlkDevice {
    cfg: VirtioBlkConfig,
    drive: VirtualDrive,
    vq: VirtQueue,
}

impl VirtioBlkDevice {
    pub fn new(drive: VirtualDrive, vq: VirtQueue) -> Self {
        let cfg = VirtioBlkConfig {
            capacity: drive.capacity_bytes() / VIRTIO_BLK_SECTOR_SIZE,
            size_max: u32::MAX,
            seg_max: u32::from(vq.size().saturating_sub(2)),
            blk_size: drive.sector_size(),
        };
        Self { cfg, drive, vq }
    }

    pub fn config(&self) -> VirtioBlkConfig {
        self.cfg
    }

    pub fn virtqueue_mut(&mut self) -> &mut VirtQueue {
        &mut self.vq
    }

    pub fn process_queue(
        &mut self,
        mem: &mut dyn GuestMemory,
        irq: &mut dyn VirtioInterrupt,
    ) -> Result<usize, VirtQueueError> {
        let mut processed = 0usize;
        while let Some(chain) = self.vq.pop_available(mem)? {
            let used_len = self.process_chain(mem, &chain);
            // Even for malformed requests, the driver expects an entry in the used ring.
            self.vq.push_used(mem, chain.head_index, used_len)?;
            processed += 1;
        }

        if processed > 0 && self.vq.should_notify(mem)? {
            irq.trigger();
        }
        Ok(processed)
    }

    fn process_chain(&mut self, mem: &mut dyn GuestMemory, chain: &DescChain) -> u32 {
        // Virtio-blk requests always end with a 1-byte status field.
        let status_desc = chain.descs.last();
        let Some(status_desc) = status_desc else {
            return 0;
        };

        // If the status descriptor isn't writable, there's no well-defined way to signal failure.
        if status_desc.flags & VIRTQ_DESC_F_WRITE == 0 || status_desc.len < 1 {
            return 0;
        }

        let mut used_len = 1u32; // status byte

        let header = match self.read_header(mem, chain) {
            Ok(h) => h,
            Err(()) => {
                let _ = mem.write_u8_le(status_desc.addr, VIRTIO_BLK_S_IOERR);
                return used_len;
            }
        };

        let status = match header.req_type {
            VIRTIO_BLK_T_IN => match self.do_read(mem, chain, header.sector) {
                Ok(bytes) => {
                    used_len = bytes.saturating_add(1);
                    VIRTIO_BLK_S_OK
                }
                Err(()) => VIRTIO_BLK_S_IOERR,
            },
            VIRTIO_BLK_T_OUT => match self.do_write(mem, chain, header.sector) {
                Ok(()) => VIRTIO_BLK_S_OK,
                Err(()) => VIRTIO_BLK_S_IOERR,
            },
            VIRTIO_BLK_T_FLUSH => match self.drive.flush() {
                Ok(()) => VIRTIO_BLK_S_OK,
                Err(_) => VIRTIO_BLK_S_IOERR,
            },
            _ => VIRTIO_BLK_S_UNSUPP,
        };

        let _ = mem.write_u8_le(status_desc.addr, status);
        used_len
    }

    fn read_header(
        &self,
        mem: &dyn GuestMemory,
        chain: &DescChain,
    ) -> Result<VirtioBlkReqHeader, ()> {
        if chain.descs.len() < 2 {
            return Err(());
        }
        let header_desc = chain.descs[0];
        if header_desc.len < 16 {
            return Err(());
        }

        let req_type = mem.read_u32_le(header_desc.addr).map_err(|_| ())?;
        // reserved at +4 ignored
        let sector = mem.read_u64_le(header_desc.addr + 8).map_err(|_| ())?;
        Ok(VirtioBlkReqHeader { req_type, sector })
    }

    fn do_read(
        &mut self,
        mem: &mut dyn GuestMemory,
        chain: &DescChain,
        sector: u64,
    ) -> Result<u32, ()> {
        // header + status must exist; data descs are [1..len-1]
        if chain.descs.len() < 3 {
            return Err(());
        }

        let mut disk_offset = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE).ok_or(())?;

        let mut transferred: u32 = 0;
        for desc in &chain.descs[1..chain.descs.len() - 1] {
            if desc.flags & VIRTQ_DESC_F_WRITE == 0 {
                return Err(());
            }
            let len = desc.len as usize;
            if let Some(buf) = mem.get_slice_mut(desc.addr, len) {
                self.drive.read_at(disk_offset, buf).map_err(|_| ())?;
            } else {
                let mut tmp = vec![0u8; len];
                self.drive.read_at(disk_offset, &mut tmp).map_err(|_| ())?;
                mem.write_from(desc.addr, &tmp).map_err(|_| ())?;
            }
            disk_offset = disk_offset.checked_add(desc.len as u64).ok_or(())?;
            transferred = transferred.saturating_add(desc.len);
        }
        Ok(transferred)
    }

    fn do_write(
        &mut self,
        mem: &dyn GuestMemory,
        chain: &DescChain,
        sector: u64,
    ) -> Result<(), ()> {
        if chain.descs.len() < 3 {
            return Err(());
        }

        let mut disk_offset = sector.checked_mul(VIRTIO_BLK_SECTOR_SIZE).ok_or(())?;

        for desc in &chain.descs[1..chain.descs.len() - 1] {
            if desc.flags & VIRTQ_DESC_F_WRITE != 0 {
                return Err(());
            }
            let len = desc.len as usize;
            if let Some(buf) = mem.get_slice(desc.addr, len) {
                self.drive.write_at(disk_offset, buf).map_err(|_| ())?;
            } else {
                let mut tmp = vec![0u8; len];
                mem.read_into(desc.addr, &mut tmp).map_err(|_| ())?;
                self.drive.write_at(disk_offset, &tmp).map_err(|_| ())?;
            }
            disk_offset = disk_offset.checked_add(desc.len as u64).ok_or(())?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::virtio::core::{DenseMemory, GuestMemory, VirtQueue, VIRTQ_DESC_F_NEXT};
    use crate::storage::DiskBackend;
    use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
    use std::io;
    use std::sync::{Arc, Mutex};

    const DESC_ADDR: u64 = 0x1000;
    const AVAIL_ADDR: u64 = 0x2000;
    const USED_ADDR: u64 = 0x3000;

    fn write_desc(
        mem: &mut dyn GuestMemory,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = DESC_ADDR + (index as u64) * 16;
        mem.write_u64_le(base, addr).unwrap();
        mem.write_u32_le(base + 8, len).unwrap();
        mem.write_u16_le(base + 12, flags).unwrap();
        mem.write_u16_le(base + 14, next).unwrap();
    }

    fn write_avail(mem: &mut dyn GuestMemory, idx: u16, head: u16) {
        mem.write_u16_le(AVAIL_ADDR, 0).unwrap(); // flags
        mem.write_u16_le(AVAIL_ADDR + 2, idx).unwrap();
        mem.write_u16_le(AVAIL_ADDR + 4, head).unwrap();
    }

    fn read_used_elem(mem: &dyn GuestMemory, ring_index: u16) -> (u32, u32) {
        let base = USED_ADDR + 4 + (ring_index as u64) * 8;
        let id = mem.read_u32_le(base).unwrap();
        let len = mem.read_u32_le(base + 4).unwrap();
        (id, len)
    }

    #[derive(Clone)]
    struct SharedMemBackend {
        inner: Arc<Mutex<MemState>>,
    }

    struct MemState {
        data: Vec<u8>,
        flush_count: usize,
    }

    impl SharedMemBackend {
        fn new(size: usize) -> Self {
            Self {
                inner: Arc::new(Mutex::new(MemState {
                    data: vec![0; size],
                    flush_count: 0,
                })),
            }
        }

        fn set_bytes(&self, offset: usize, bytes: &[u8]) {
            let mut inner = self.inner.lock().unwrap();
            inner.data[offset..offset + bytes.len()].copy_from_slice(bytes);
        }

        fn bytes(&self, offset: usize, len: usize) -> Vec<u8> {
            let inner = self.inner.lock().unwrap();
            inner.data[offset..offset + len].to_vec()
        }

        fn flush_count(&self) -> usize {
            self.inner.lock().unwrap().flush_count
        }
    }

    impl DiskBackend for SharedMemBackend {
        fn len(&self) -> u64 {
            self.inner.lock().unwrap().data.len() as u64
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            let offset = offset as usize;
            let inner = self.inner.lock().unwrap();
            let end = offset
                .checked_add(buf.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
            if end > inner.data.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read OOB"));
            }
            buf.copy_from_slice(&inner.data[offset..end]);
            Ok(())
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
            let offset = offset as usize;
            let mut inner = self.inner.lock().unwrap();
            let end = offset
                .checked_add(buf.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
            if end > inner.data.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "write OOB"));
            }
            inner.data[offset..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.lock().unwrap().flush_count += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestIrq {
        count: usize,
    }

    impl VirtioInterrupt for TestIrq {
        fn trigger(&mut self) {
            self.count += 1;
        }
    }

    #[derive(Clone)]
    struct SharedAeroDisk {
        inner: Arc<Mutex<RawDisk<MemBackend>>>,
        flush_count: Arc<Mutex<usize>>,
    }

    impl SharedAeroDisk {
        fn new(capacity_bytes: u64) -> Self {
            let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
            Self {
                inner: Arc::new(Mutex::new(disk)),
                flush_count: Arc::new(Mutex::new(0)),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, RawDisk<MemBackend>> {
            self.inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn bytes(&self, offset: u64, len: usize) -> Vec<u8> {
            let mut disk = self.lock();
            let mut out = vec![0u8; len];
            disk.read_at(offset, &mut out).unwrap();
            out
        }

        fn flush_count(&self) -> usize {
            *self
                .flush_count
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl VirtualDisk for SharedAeroDisk {
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
                .flush_count
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
            self.lock().flush()
        }
    }

    #[test]
    fn read_request_returns_correct_bytes() {
        let backend = SharedMemBackend::new(4096);
        backend.set_bytes(0, b"abcdefgh");

        let drive = VirtualDrive::new(512, Box::new(backend.clone()));
        let vq = VirtQueue::new(8, DESC_ADDR, AVAIL_ADDR, USED_ADDR);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x10000).unwrap();
        let header_addr = 0x4000;
        let data1_addr = 0x5000;
        let data2_addr = 0x5010;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_IN).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap(); // sector 0

        write_desc(&mut mem, 0, header_addr, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(
            &mut mem,
            1,
            data1_addr,
            3,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(
            &mut mem,
            2,
            data2_addr,
            5,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            3,
        );
        write_desc(&mut mem, 3, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

        write_avail(&mut mem, 1, 0);

        let mut irq = TestIrq::default();
        dev.process_queue(&mut mem, &mut irq).unwrap();

        let out = [
            mem.read_u8_le(data1_addr).unwrap(),
            mem.read_u8_le(data1_addr + 1).unwrap(),
            mem.read_u8_le(data1_addr + 2).unwrap(),
            mem.read_u8_le(data2_addr).unwrap(),
            mem.read_u8_le(data2_addr + 1).unwrap(),
            mem.read_u8_le(data2_addr + 2).unwrap(),
            mem.read_u8_le(data2_addr + 3).unwrap(),
            mem.read_u8_le(data2_addr + 4).unwrap(),
        ];
        assert_eq!(&out, b"abcdefgh");
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(mem.read_u16_le(USED_ADDR + 2).unwrap(), 1);
        assert_eq!(read_used_elem(&mem, 0), (0, 9)); // 8 bytes + status
        assert_eq!(irq.count, 1);
    }

    #[test]
    fn write_request_persists() {
        let backend = SharedMemBackend::new(4096);
        let drive = VirtualDrive::new(512, Box::new(backend.clone()));
        let vq = VirtQueue::new(8, DESC_ADDR, AVAIL_ADDR, USED_ADDR);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x10000).unwrap();
        let header_addr = 0x4000;
        let data1_addr = 0x5000;
        let data2_addr = 0x5010;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_OUT).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap(); // sector 0

        mem.write_from(data1_addr, b"abc").unwrap();
        mem.write_from(data2_addr, b"defgh").unwrap();

        write_desc(&mut mem, 0, header_addr, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(&mut mem, 1, data1_addr, 3, VIRTQ_DESC_F_NEXT, 2);
        write_desc(&mut mem, 2, data2_addr, 5, VIRTQ_DESC_F_NEXT, 3);
        write_desc(&mut mem, 3, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);
        write_avail(&mut mem, 1, 0);

        let mut irq = TestIrq::default();
        dev.process_queue(&mut mem, &mut irq).unwrap();

        assert_eq!(backend.bytes(0, 8), b"abcdefgh");
        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
        assert_eq!(irq.count, 1);
    }

    #[test]
    fn flush_request_calls_backend_flush() {
        let backend = SharedMemBackend::new(4096);
        let drive = VirtualDrive::new(512, Box::new(backend.clone()));
        let vq = VirtQueue::new(8, DESC_ADDR, AVAIL_ADDR, USED_ADDR);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x10000).unwrap();
        let header_addr = 0x4000;
        let status_addr = 0x6000;

        mem.write_u32_le(header_addr, VIRTIO_BLK_T_FLUSH).unwrap();
        mem.write_u32_le(header_addr + 4, 0).unwrap();
        mem.write_u64_le(header_addr + 8, 0).unwrap();

        write_desc(&mut mem, 0, header_addr, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(&mut mem, 1, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);
        write_avail(&mut mem, 1, 0);

        let mut irq = TestIrq::default();
        dev.process_queue(&mut mem, &mut irq).unwrap();

        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(backend.flush_count(), 1);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
        assert_eq!(irq.count, 1);
    }

    #[test]
    fn aero_storage_adapter_supports_virtio_blk_read_write() {
        let disk = SharedAeroDisk::new(8 * SECTOR_SIZE as u64);
        let drive = VirtualDrive::new_from_aero_storage(disk.clone());
        let vq = VirtQueue::new(16, DESC_ADDR, AVAIL_ADDR, USED_ADDR);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x10000).unwrap();

        let write_header_addr = 0x4000;
        let write_data1_addr = 0x5000;
        let write_data2_addr = 0x5200;
        let write_status_addr = 0x6000;

        let read_header_addr = 0x4100;
        let read_data1_addr = 0x7000;
        let read_data2_addr = 0x7200;
        let read_status_addr = 0x8000;

        let flush_header_addr = 0x4200;
        let flush_status_addr = 0x9000;

        let payload: Vec<u8> = (0..(2 * SECTOR_SIZE)).map(|i| (i & 0xff) as u8).collect();

        // 1) WRITE two sectors at LBA 0 via two data descriptors.
        mem.write_u32_le(write_header_addr, VIRTIO_BLK_T_OUT)
            .unwrap();
        mem.write_u32_le(write_header_addr + 4, 0).unwrap();
        mem.write_u64_le(write_header_addr + 8, 0).unwrap(); // sector 0
        mem.write_from(write_data1_addr, &payload[..SECTOR_SIZE])
            .unwrap();
        mem.write_from(write_data2_addr, &payload[SECTOR_SIZE..])
            .unwrap();

        // 2) READ the same two sectors back.
        mem.write_u32_le(read_header_addr, VIRTIO_BLK_T_IN).unwrap();
        mem.write_u32_le(read_header_addr + 4, 0).unwrap();
        mem.write_u64_le(read_header_addr + 8, 0).unwrap(); // sector 0

        // 3) FLUSH.
        mem.write_u32_le(flush_header_addr, VIRTIO_BLK_T_FLUSH)
            .unwrap();
        mem.write_u32_le(flush_header_addr + 4, 0).unwrap();
        mem.write_u64_le(flush_header_addr + 8, 0).unwrap();

        // WRITE chain.
        write_desc(&mut mem, 0, write_header_addr, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(
            &mut mem,
            1,
            write_data1_addr,
            SECTOR_SIZE as u32,
            VIRTQ_DESC_F_NEXT,
            2,
        );
        write_desc(
            &mut mem,
            2,
            write_data2_addr,
            SECTOR_SIZE as u32,
            VIRTQ_DESC_F_NEXT,
            3,
        );
        write_desc(&mut mem, 3, write_status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

        // READ chain.
        write_desc(&mut mem, 4, read_header_addr, 16, VIRTQ_DESC_F_NEXT, 5);
        write_desc(
            &mut mem,
            5,
            read_data1_addr,
            SECTOR_SIZE as u32,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            6,
        );
        write_desc(
            &mut mem,
            6,
            read_data2_addr,
            SECTOR_SIZE as u32,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            7,
        );
        write_desc(&mut mem, 7, read_status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

        // FLUSH chain.
        write_desc(&mut mem, 8, flush_header_addr, 16, VIRTQ_DESC_F_NEXT, 9);
        write_desc(&mut mem, 9, flush_status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

        // Three requests in the available ring: WRITE, READ, FLUSH.
        mem.write_u16_le(AVAIL_ADDR, 0).unwrap(); // flags
        mem.write_u16_le(AVAIL_ADDR + 2, 3).unwrap(); // idx
        mem.write_u16_le(AVAIL_ADDR + 4, 0).unwrap(); // ring[0]
        mem.write_u16_le(AVAIL_ADDR + 6, 4).unwrap(); // ring[1]
        mem.write_u16_le(AVAIL_ADDR + 8, 8).unwrap(); // ring[2]

        let mut irq = TestIrq::default();
        assert_eq!(dev.process_queue(&mut mem, &mut irq).unwrap(), 3);

        let mut out = vec![0u8; payload.len()];
        mem.read_into(read_data1_addr, &mut out[..SECTOR_SIZE])
            .unwrap();
        mem.read_into(read_data2_addr, &mut out[SECTOR_SIZE..])
            .unwrap();
        assert_eq!(out, payload);
        assert_eq!(disk.bytes(0, payload.len()), payload);
        assert_eq!(mem.read_u8_le(write_status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(mem.read_u8_le(read_status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(mem.read_u8_le(flush_status_addr).unwrap(), VIRTIO_BLK_S_OK);
        assert_eq!(disk.flush_count(), 1);
        assert_eq!(mem.read_u16_le(USED_ADDR + 2).unwrap(), 3);
        assert_eq!(read_used_elem(&mem, 0), (0, 1));
        assert_eq!(read_used_elem(&mem, 1), (4, (payload.len() as u32) + 1));
        assert_eq!(read_used_elem(&mem, 2), (8, 1));
        assert_eq!(irq.count, 1);
    }

    #[test]
    fn malformed_chains_return_ioerr_without_panic() {
        let backend = SharedMemBackend::new(4096);
        let drive = VirtualDrive::new(512, Box::new(backend));
        let vq = VirtQueue::new(8, DESC_ADDR, AVAIL_ADDR, USED_ADDR);
        let mut dev = VirtioBlkDevice::new(drive, vq);

        let mut mem = DenseMemory::new(0x10000).unwrap();
        let header_addr = 0x4000;
        let data_addr = 0x5000;
        let status_addr = 0x6000;

        // Header descriptor too small (< 16 bytes).
        mem.write_u32_le(header_addr, VIRTIO_BLK_T_IN).unwrap();

        write_desc(&mut mem, 0, header_addr, 8, VIRTQ_DESC_F_NEXT, 1);
        write_desc(
            &mut mem,
            1,
            data_addr,
            8,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(&mut mem, 2, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);
        write_avail(&mut mem, 1, 0);

        let mut irq = TestIrq::default();
        dev.process_queue(&mut mem, &mut irq).unwrap();

        assert_eq!(mem.read_u8_le(status_addr).unwrap(), VIRTIO_BLK_S_IOERR);
        assert_eq!(irq.count, 1);
    }

    // `rw_roundtrip_works_with_aero_storage_disk_via_adapter` was replaced by
    // `aero_storage_adapter_supports_virtio_blk_read_write`, which covers scatter-gather,
    // read-back, and flush in a single virtqueue processing pass.
}
