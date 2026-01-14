use aero_devices_nvme::{from_virtual_disk, NvmeController};
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
use memory::MemoryBus as _;

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

impl memory::MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, out: &mut [u8]) {
        let start = paddr as usize;
        let end = start + out.len();
        assert!(end <= self.buf.len(), "out-of-bounds DMA read");
        out.copy_from_slice(&self.buf[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, data: &[u8]) {
        let start = paddr as usize;
        let end = start + data.len();
        assert!(end <= self.buf.len(), "out-of-bounds DMA write");
        self.buf[start..end].copy_from_slice(data);
    }
}

fn build_command(opc: u8, psdt: u8) -> [u8; 64] {
    let dw0 = (opc as u32) | ((psdt as u32) << 14);
    let mut cmd = [0u8; 64];
    cmd[0..4].copy_from_slice(&dw0.to_le_bytes());
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

#[derive(Debug)]
struct Cqe {
    cid: u16,
    status: u16,
}

fn read_cqe(mem: &mut TestMem, addr: u64) -> Cqe {
    let mut bytes = [0u8; 16];
    mem.read_physical(addr, &mut bytes);
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    Cqe {
        cid: (dw3 & 0xffff) as u16,
        status: (dw3 >> 16) as u16,
    }
}

#[test]
fn dataset_management_deallocate_accepts_sgl_datablock() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let write_buf = 0x60000;
    let read_buf = 0x61000;
    let dsm_ranges = 0x62001; // intentionally unaligned

    // Enable controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05, 0);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1);
    ctrl.process(&mut mem);

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01, 0);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    ctrl.mmio_write(0x1000, 4, 2);
    ctrl.process(&mut mem);

    // Write distinct patterns into 3 consecutive sectors so we can detect corruption.
    let sector_size = 512usize;
    let sector0 = vec![0xAA; sector_size];
    let sector1 = vec![0xBB; sector_size];
    let sector2 = vec![0xCC; sector_size];

    let mut payload = Vec::new();
    payload.extend_from_slice(&sector0);
    payload.extend_from_slice(&sector1);
    payload.extend_from_slice(&sector2);
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01, 0); // WRITE
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
    let mut range = [0u8; 16];
    range[4..8].copy_from_slice(&0u32.to_le_bytes()); // 1 sector
    range[8..16].copy_from_slice(&1u64.to_le_bytes()); // slba=1
    mem.write_physical(dsm_ranges, &range);

    let mut cmd = build_command(0x09, 1); // DSM, PSDT=SGL
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, dsm_ranges);
    // Inline Data Block SGL descriptor: length=16, type=0x00 (subtype=0).
    set_prp2(&mut cmd, 16);
    set_cdw10(&mut cmd, 0); // NR=0 => 1 range
    set_cdw11(&mut cmd, 1 << 2); // Deallocate attribute
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & !0x1, 0);

    // Read back all 3 sectors and ensure they are unchanged (discard is best-effort and this
    // backend does not reclaim storage).
    let mut cmd = build_command(0x02, 0); // READ
    set_cid(&mut cmd, 0x12);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 2); // 3 sectors
    mem.write_physical(io_sq + 2 * 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 3);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 2 * 16);
    assert_eq!(cqe.cid, 0x12);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; sector_size * 3];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}
