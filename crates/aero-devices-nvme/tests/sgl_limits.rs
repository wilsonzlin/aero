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
fn sgl_descriptor_count_is_capped() {
    let disk = RawDisk::create(MemBackend::new(), 1024 * SECTOR_SIZE as u64).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;

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

    // WRITE 1 sector using an SGL segment descriptor with an absurdly large descriptor list length.
    // This should be rejected by the per-command descriptor count cap before the device attempts
    // to walk the segment list.
    let segment_list_addr = 0x70000u64;
    // 16k descriptors (each 16 bytes) would already fill the cap; exceeding it should fail.
    let too_many_desc_bytes = 16u64 * 1024 * 16;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, segment_list_addr);
    set_prp2(&mut cmd, too_many_desc_bytes | ((0x02u64) << 56)); // Segment
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x10);
    // INVALID_FIELD: SCT=0, SC=0x2, DNR=1 -> 0x4004 (phase bit masked out).
    assert_eq!(
        cqe.status & !0x1,
        0x4004,
        "expected INVALID_FIELD status"
    );
}
