use std::sync::{Arc, Mutex};

use aero_devices_nvme::NvmeController;
use aero_storage::{
    DiskError as StorageDiskError, Result as StorageResult, VirtualDisk, SECTOR_SIZE,
};
use memory::MemoryBus;

const NVME_MAX_DMA_BYTES: usize = 4 * 1024 * 1024;

// Completion status encodings (without phase).
const NVME_STATUS_SUCCESS: u16 = 0x0000;
const NVME_STATUS_INVALID_FIELD: u16 = 0x4004;
const NVME_STATUS_LBA_OUT_OF_RANGE: u16 = 0x4300;
const NVME_STATUS_INVALID_NS: u16 = 0x4216;

struct TestMem {
    buf: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { buf: vec![0; size] }
    }

    fn read_at(&self, addr: u64, out: &mut [u8]) {
        let start = addr as usize;
        let end = start + out.len();
        out.copy_from_slice(&self.buf[start..end]);
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, out: &mut [u8]) {
        self.read_at(paddr, out);
    }

    fn write_physical(&mut self, paddr: u64, data: &[u8]) {
        let start = paddr as usize;
        let end = start + data.len();
        self.buf[start..end].copy_from_slice(data);
    }
}

#[derive(Clone)]
struct MemDisk {
    data: Arc<Mutex<Vec<u8>>>,
}

impl MemDisk {
    fn new(sectors: u64) -> Self {
        let mut v = vec![0u8; sectors as usize * SECTOR_SIZE];
        // Default to a non-zero pattern so "zeroing" operations are easy to validate.
        v.fill(0xA5);
        Self {
            data: Arc::new(Mutex::new(v)),
        }
    }

    fn fill(&self, byte: u8) {
        let mut guard = self.data.lock().unwrap();
        guard.fill(byte);
    }

    fn read_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        let guard = self.data.lock().unwrap();
        guard[offset..offset + len].to_vec()
    }
}

impl VirtualDisk for MemDisk {
    fn capacity_bytes(&self) -> u64 {
        let guard = self.data.lock().unwrap();
        guard.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StorageResult<()> {
        let offset = usize::try_from(offset).map_err(|_| StorageDiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(StorageDiskError::OffsetOverflow)?;
        let guard = self.data.lock().unwrap();
        if end > guard.len() {
            return Err(StorageDiskError::OutOfBounds {
                offset: offset as u64,
                len: buf.len(),
                capacity: guard.len() as u64,
            });
        }
        buf.copy_from_slice(&guard[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> StorageResult<()> {
        let offset = usize::try_from(offset).map_err(|_| StorageDiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(StorageDiskError::OffsetOverflow)?;
        let mut guard = self.data.lock().unwrap();
        if end > guard.len() {
            return Err(StorageDiskError::OutOfBounds {
                offset: offset as u64,
                len: buf.len(),
                capacity: guard.len() as u64,
            });
        }
        guard[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> StorageResult<()> {
        Ok(())
    }

    fn discard_range(&mut self, offset: u64, len: u64) -> StorageResult<()> {
        if len == 0 {
            if offset > self.capacity_bytes() {
                return Err(StorageDiskError::OutOfBounds {
                    offset,
                    len: 0,
                    capacity: self.capacity_bytes(),
                });
            }
            return Ok(());
        }

        let end = offset
            .checked_add(len)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        let cap = self.capacity_bytes();
        if end > cap {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: usize::try_from(len).unwrap_or(usize::MAX),
                capacity: cap,
            });
        }

        let offset_usize = usize::try_from(offset).map_err(|_| StorageDiskError::OffsetOverflow)?;
        let len_usize = usize::try_from(len).map_err(|_| StorageDiskError::OffsetOverflow)?;
        let end_usize = offset_usize
            .checked_add(len_usize)
            .ok_or(StorageDiskError::OffsetOverflow)?;

        let mut guard = self.data.lock().unwrap();
        guard[offset_usize..end_usize].fill(0);
        Ok(())
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

fn set_prp2(cmd: &mut [u8; 64], prp2: u64) {
    cmd[32..40].copy_from_slice(&prp2.to_le_bytes());
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

fn read_cqe(mem: &mut TestMem, cq_base: u64, index: u16) -> (u16, u16) {
    let addr = cq_base + index as u64 * 16;
    let mut bytes = [0u8; 16];
    mem.read_physical(addr, &mut bytes);
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let cid = (dw3 & 0xffff) as u16;
    let status = (dw3 >> 16) as u16;
    (cid, status)
}

fn setup_admin_and_io_queue_pair(
    ctrl: &mut NvmeController,
    mem: &mut TestMem,
    asq: u64,
    acq: u64,
    io_cq: u64,
    io_sq: u64,
) {
    // Enable controller with 16-entry admin SQ/CQ.
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
    ctrl.process(mem);

    // Create IO SQ (qid=1, size=16, cqid=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 2);
    ctrl.process(mem);
}

#[test]
fn identify_controller_advertises_write_zeroes_and_dsm() {
    let disk = MemDisk::new(1024);
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let id_buf = 0x30000;

    // Enable controller with a small admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // IDENTIFY (CNS=1 = controller).
    let mut cmd = build_command(0x06);
    set_cid(&mut cmd, 0x1234);
    set_prp1(&mut cmd, id_buf);
    set_cdw10(&mut cmd, 1);
    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, acq, 0);
    assert_eq!(cid, 0x1234);
    assert_eq!(status & !0x1, NVME_STATUS_SUCCESS);

    let mut id = [0u8; 4096];
    mem.read_physical(id_buf, &mut id);
    let oncs = u16::from_le_bytes(id[520..522].try_into().unwrap());
    assert!(
        oncs & (1 << 2) != 0,
        "expected Identify Controller ONCS to advertise Dataset Management support"
    );
    assert!(
        oncs & (1 << 3) != 0,
        "expected Identify Controller ONCS to advertise Write Zeroes support"
    );
}

#[test]
fn identify_namespace_advertises_thin_provisioning_for_dsm() {
    let disk = MemDisk::new(1024);
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let id_buf = 0x30000;

    // Enable controller with a small admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // IDENTIFY (CNS=0 = namespace).
    let mut cmd = build_command(0x06);
    set_cid(&mut cmd, 0x1235);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, id_buf);
    set_cdw10(&mut cmd, 0);
    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, acq, 0);
    assert_eq!(cid, 0x1235);
    assert_eq!(status & !0x1, NVME_STATUS_SUCCESS);

    let mut id = [0u8; 4096];
    mem.read_physical(id_buf, &mut id);

    // Identify Namespace NSFEAT bit0 advertises thin provisioning, which in turn signals to guests
    // that DSM/TRIM (deallocate) is supported.
    assert_ne!(id[24] & 0x1, 0, "expected NSFEAT.THINP to be set");
}

#[test]
fn dsm_without_deallocate_is_noop_success() {
    let disk = MemDisk::new(1024);
    let disk_state = disk.clone();
    disk_state.fill(0x77);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // DSM with "integral dataset" hint bits only (no AD / deallocate). This should be accepted as
    // a no-op for compatibility.
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x40);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 0); // 1 range (ignored by no-op path)
    set_cdw11(&mut cmd, 1 << 0); // IDR hint only
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x40);
    assert_eq!(status & !0x1, NVME_STATUS_SUCCESS);

    // Ensure the disk was not modified.
    assert_eq!(
        disk_state.read_bytes(0, SECTOR_SIZE),
        vec![0x77u8; SECTOR_SIZE]
    );
}

#[test]
fn dsm_rejects_unknown_attributes() {
    let disk = MemDisk::new(1024);
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x43);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 1 << 31); // unknown attribute bit
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x43);
    assert_eq!(status & !0x1, NVME_STATUS_INVALID_FIELD);
}

#[test]
fn dsm_rejects_reserved_cdw10_bits_even_without_deallocate() {
    let disk = MemDisk::new(1024);
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // CDW10 higher bits are reserved; set one and ensure the command is rejected even though we
    // don't request deallocate.
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x44);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 1u32 << 31); // reserved bit + NR=0
    set_cdw11(&mut cmd, 1 << 0); // IDR hint only
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x44);
    assert_eq!(status & !0x1, NVME_STATUS_INVALID_FIELD);
}

#[test]
fn write_zeroes_invalid_nsid_is_rejected() {
    let disk = MemDisk::new(1024);
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    let mut cmd = build_command(0x08);
    set_cid(&mut cmd, 0x45);
    set_nsid(&mut cmd, 2); // only NSID=1 exists
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x45);
    assert_eq!(status & !0x1, NVME_STATUS_INVALID_NS);
}

#[test]
fn write_zeroes_zero_fills_disk() {
    let disk = MemDisk::new(1024);
    let disk_state = disk.clone();
    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // Zero 4 sectors starting at LBA 4 (NLB is 0-based: 4 sectors => NLB=3).
    let mut cmd = build_command(0x08);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 4);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 3);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x10);
    assert_eq!(status & !0x1, NVME_STATUS_SUCCESS);

    // Ensure the zeroed range is all zeros, while neighbouring LBAs remain unchanged.
    let start = 4 * SECTOR_SIZE;
    let len = 4 * SECTOR_SIZE;
    assert_eq!(disk_state.read_bytes(start, len), vec![0u8; len]);

    // LBA 0 should remain in the original non-zero fill pattern.
    let lba0 = disk_state.read_bytes(0, SECTOR_SIZE);
    assert_eq!(lba0, vec![0xA5u8; SECTOR_SIZE]);
}

#[test]
fn dsm_deallocate_zero_fills_disk() {
    let disk = MemDisk::new(1024);
    let disk_state = disk.clone();
    disk_state.fill(0xCC);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let ranges = 0x60000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // Build a single DSM range descriptor (16 bytes):
    // - Context Attributes (u32) = 0
    // - NLB (u32) is 0-based => 2 sectors => NLB=1
    // - SLBA (u64) = 8
    let mut range_desc = [0u8; 16];
    range_desc[0..4].copy_from_slice(&0u32.to_le_bytes());
    range_desc[4..8].copy_from_slice(&1u32.to_le_bytes());
    range_desc[8..16].copy_from_slice(&8u64.to_le_bytes());
    mem.write_physical(ranges, &range_desc);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x20);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, ranges);
    set_prp2(&mut cmd, 0);
    set_cdw10(&mut cmd, 0); // NR=0 => 1 range
    set_cdw11(&mut cmd, 1 << 2); // AD (deallocate)
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x20);
    assert_eq!(status & !0x1, NVME_STATUS_SUCCESS);

    let start = 8 * SECTOR_SIZE;
    let len = 2 * SECTOR_SIZE;
    assert_eq!(disk_state.read_bytes(start, len), vec![0u8; len]);

    // Adjacent sector should remain unchanged.
    assert_eq!(
        disk_state.read_bytes(7 * SECTOR_SIZE, SECTOR_SIZE),
        vec![0xCCu8; SECTOR_SIZE]
    );
}

#[test]
fn write_zeroes_rejects_oversized_request() {
    // Use a disk large enough that the range check passes for an oversized request.
    let disk = MemDisk::new(9000);
    let disk_state = disk.clone();
    disk_state.fill(0x11);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // Request one sector more than the controller's per-command cap.
    let sectors = (NVME_MAX_DMA_BYTES / SECTOR_SIZE) as u32 + 1;
    let nlb = sectors - 1; // 0-based field for WRITE ZEROES.

    let mut cmd = build_command(0x08);
    set_cid(&mut cmd, 0x30);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, nlb);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x30);
    assert_eq!(status & !0x1, NVME_STATUS_INVALID_FIELD);

    // Regression: the controller must not attempt to allocate and write a huge zero buffer.
    assert_eq!(
        disk_state.read_bytes(0, SECTOR_SIZE),
        vec![0x11u8; SECTOR_SIZE]
    );
}

#[test]
fn write_zeroes_out_of_range_is_rejected() {
    let disk = MemDisk::new(16);
    let disk_state = disk.clone();
    disk_state.fill(0x55);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // Request 2 sectors starting at the final LBA (out-of-range).
    let mut cmd = build_command(0x08);
    set_cid(&mut cmd, 0x41);
    set_nsid(&mut cmd, 1);
    set_cdw10(&mut cmd, 15); // slba
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 1); // nlb=1 => 2 sectors
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x41);
    assert_eq!(status & !0x1, NVME_STATUS_LBA_OUT_OF_RANGE);

    // Ensure the disk was not modified.
    assert_eq!(
        disk_state.read_bytes(15 * SECTOR_SIZE, SECTOR_SIZE),
        vec![0x55u8; SECTOR_SIZE]
    );
}

#[test]
fn dsm_deallocate_rejects_oversized_request() {
    // Use a disk large enough that the range check passes for an oversized request.
    let disk = MemDisk::new(9000);
    let disk_state = disk.clone();
    disk_state.fill(0x22);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let ranges = 0x60000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // DSM range: request one sector more than the controller's per-command cap.
    // NLB is 0-based in the range definition.
    let sectors = (NVME_MAX_DMA_BYTES / SECTOR_SIZE) as u32 + 1;
    let nlb = sectors - 1;
    let mut range_desc = [0u8; 16];
    range_desc[0..4].copy_from_slice(&0u32.to_le_bytes()); // cattr
    range_desc[4..8].copy_from_slice(&nlb.to_le_bytes());
    range_desc[8..16].copy_from_slice(&0u64.to_le_bytes()); // slba
    mem.write_physical(ranges, &range_desc);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x31);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, ranges);
    set_cdw10(&mut cmd, 0); // NR=0 => 1 range
    set_cdw11(&mut cmd, 1 << 2); // deallocate
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x31);
    assert_eq!(status & !0x1, NVME_STATUS_INVALID_FIELD);

    assert_eq!(
        disk_state.read_bytes(0, SECTOR_SIZE),
        vec![0x22u8; SECTOR_SIZE]
    );
}

#[test]
fn dsm_deallocate_out_of_range_is_rejected() {
    let disk = MemDisk::new(16);
    let disk_state = disk.clone();
    disk_state.fill(0x66);

    let mut ctrl = NvmeController::try_new_from_aero_storage(disk).unwrap();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let ranges = 0x60000;

    setup_admin_and_io_queue_pair(&mut ctrl, &mut mem, asq, acq, io_cq, io_sq);

    // DSM range descriptor: SLBA=16 (one past end), NLB=0 => 1 sector.
    let mut range_desc = [0u8; 16];
    range_desc[0..4].copy_from_slice(&0u32.to_le_bytes()); // cattr
    range_desc[4..8].copy_from_slice(&0u32.to_le_bytes()); // nlb=0 => 1 sector
    range_desc[8..16].copy_from_slice(&16u64.to_le_bytes()); // slba
    mem.write_physical(ranges, &range_desc);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 0x42);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, ranges);
    set_cdw10(&mut cmd, 0); // NR=0 => 1 range
    set_cdw11(&mut cmd, 1 << 2); // deallocate
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let (cid, status) = read_cqe(&mut mem, io_cq, 0);
    assert_eq!(cid, 0x42);
    assert_eq!(status & !0x1, NVME_STATUS_LBA_OUT_OF_RANGE);

    // Ensure the disk was not modified.
    assert_eq!(
        disk_state.read_bytes(0, SECTOR_SIZE),
        vec![0x66u8; SECTOR_SIZE]
    );
}
