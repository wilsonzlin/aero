use std::sync::{Arc, Mutex};

use aero_devices_nvme::{DiskBackend, NvmeController};
use aero_io_snapshot::io::state::IoSnapshot;
use memory::MemoryBus;

#[derive(Clone)]
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
struct SharedDisk {
    sector_size: u32,
    data: Arc<Mutex<Vec<u8>>>,
    flush_count: Arc<Mutex<u32>>,
}

impl SharedDisk {
    fn new(sectors: u64) -> Self {
        let sector_size = 512u32;
        Self {
            sector_size,
            data: Arc::new(Mutex::new(vec![
                0u8;
                sectors as usize * sector_size as usize
            ])),
            flush_count: Arc::new(Mutex::new(0)),
        }
    }
}

impl DiskBackend for SharedDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        (self.data.lock().unwrap().len() as u64) / (self.sector_size as u64)
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> aero_devices_nvme::DiskResult<()> {
        let sector_size = self.sector_size as usize;
        if buffer.len() % sector_size != 0 {
            return Err(aero_devices_nvme::DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba
            .checked_add(sectors)
            .ok_or(aero_devices_nvme::DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors(),
            })?;
        let capacity = self.total_sectors();
        if end_lba > capacity {
            return Err(aero_devices_nvme::DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: capacity,
            });
        }
        let offset = lba as usize * sector_size;
        let end = offset + buffer.len();
        let data = self.data.lock().unwrap();
        buffer.copy_from_slice(&data[offset..end]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> aero_devices_nvme::DiskResult<()> {
        let sector_size = self.sector_size as usize;
        if buffer.len() % sector_size != 0 {
            return Err(aero_devices_nvme::DiskError::UnalignedBuffer {
                len: buffer.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buffer.len() / sector_size) as u64;
        let end_lba = lba
            .checked_add(sectors)
            .ok_or(aero_devices_nvme::DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors(),
            })?;
        let capacity = self.total_sectors();
        if end_lba > capacity {
            return Err(aero_devices_nvme::DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: capacity,
            });
        }
        let offset = lba as usize * sector_size;
        let end = offset + buffer.len();
        let mut data = self.data.lock().unwrap();
        data[offset..end].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&mut self) -> aero_devices_nvme::DiskResult<()> {
        *self.flush_count.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct CqEntry {
    cid: u16,
    status: u16,
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
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    CqEntry {
        cid: (dw3 & 0xffff) as u16,
        status: (dw3 >> 16) as u16,
    }
}

#[test]
fn snapshot_restore_preserves_pending_completion_and_disk_contents() {
    let disk = SharedDisk::new(1024);
    let mut ctrl = NvmeController::new(Box::new(disk.clone()));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let write_buf = 0x60000;
    let read_buf = 0x61000;

    ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem);
    ctrl.mmio_write(0x0028, 8, asq, &mut mem);
    ctrl.mmio_write(0x0030, 8, acq, &mut mem);
    ctrl.mmio_write(0x0014, 4, 1, &mut mem);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq + 0 * 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 1, &mut mem); // SQ0 tail = 1

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 1 * 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 2, &mut mem); // SQ0 tail = 2

    // Consume admin CQ completions so INTx level reflects IO CQ only.
    ctrl.mmio_write(0x1004, 4, 2, &mut mem);

    // WRITE 1 sector at LBA 0 (completion left pending in the IO CQ).
    let payload: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq + 0 * 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 1, &mut mem); // SQ1 tail = 1

    assert!(ctrl.intx_level);

    let snap = ctrl.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmeController::new(Box::new(disk.clone()));
    let mut mem2 = mem_snap;
    restored.load_state(&snap).unwrap();

    // Pending completion should keep INTx asserted.
    assert!(restored.intx_level);

    let cqe = read_cqe(&mut mem2, io_cq);
    assert_eq!(cqe.cid, 0x10);
    assert_eq!(cqe.status & 0x1, 1); // phase
    assert_eq!(cqe.status & !0x1, 0); // success

    // Consume completion and ensure INTx deasserts.
    restored.mmio_write(0x100c, 4, 1, &mut mem2); // CQ1 head = 1
    assert!(!restored.intx_level);

    // READ it back after restore.
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem2.write_physical(io_sq + 1 * 64, &cmd);
    restored.mmio_write(0x1008, 4, 2, &mut mem2); // SQ1 tail = 2

    let cqe = read_cqe(&mut mem2, io_cq + 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    mem2.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn snapshot_restore_preserves_cq_phase_across_wrap() {
    let disk = SharedDisk::new(1024);
    let mut ctrl = NvmeController::new(Box::new(disk.clone()));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    ctrl.mmio_write(0x0024, 4, 0x000f_000f, &mut mem);
    ctrl.mmio_write(0x0028, 8, asq, &mut mem);
    ctrl.mmio_write(0x0030, 8, acq, &mut mem);
    ctrl.mmio_write(0x0014, 4, 1, &mut mem);

    // Create IO CQ (qid=1, size=2, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (1u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq + 0 * 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 1, &mut mem);

    // Create IO SQ (qid=1, size=2, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (1u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 1 * 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 2, &mut mem);

    // Consume admin CQ completions (2 entries).
    ctrl.mmio_write(0x1004, 4, 2, &mut mem);

    let sq_tail_db = 0x1008;
    let cq_head_db = 0x100c;

    // 1) FLUSH at SQ slot 0, CQ slot 0, phase=1.
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    mem.write_physical(io_sq + 0 * 64, &cmd);
    ctrl.mmio_write(sq_tail_db, 4, 1, &mut mem);
    assert!(ctrl.intx_level);

    ctrl.mmio_write(cq_head_db, 4, 1, &mut mem);
    assert!(!ctrl.intx_level);

    // 2) FLUSH at SQ slot 1, CQ slot 1, phase=1 (tail wraps and toggles phase for the *next* CQE).
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    mem.write_physical(io_sq + 1 * 64, &cmd);
    ctrl.mmio_write(sq_tail_db, 4, 0, &mut mem);
    assert!(ctrl.intx_level);

    // Snapshot while CQ tail has wrapped (phase has toggled) but CQE#2 is still pending.
    let snap = ctrl.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmeController::new(Box::new(disk.clone()));
    let mut mem2 = mem_snap;
    restored.load_state(&snap).unwrap();

    assert!(restored.intx_level);

    let cqe = read_cqe(&mut mem2, io_cq + 1 * 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    // Consume CQE#2 (head wraps to 0).
    restored.mmio_write(cq_head_db, 4, 0, &mut mem2);
    assert!(!restored.intx_level);

    // 3) Next FLUSH should reuse CQ slot 0 with phase=0 (because the tail wrapped after CQE#2).
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x12);
    set_nsid(&mut cmd, 1);
    mem2.write_physical(io_sq + 0 * 64, &cmd);
    restored.mmio_write(sq_tail_db, 4, 1, &mut mem2);

    let cqe = read_cqe(&mut mem2, io_cq + 0 * 16);
    assert_eq!(cqe.cid, 0x12);
    assert_eq!(cqe.status & 0x1, 0);
    assert_eq!(cqe.status & !0x1, 0);
}
