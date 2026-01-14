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

fn write_sgl_desc(mem: &mut TestMem, addr: u64, ptr: u64, len: u32, type_byte: u8) {
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&ptr.to_le_bytes());
    desc[8..12].copy_from_slice(&len.to_le_bytes());
    desc[15] = type_byte;
    mem.write_physical(addr, &desc);
}

#[derive(Debug)]
struct Cqe {
    sqid: u16,
    cid: u16,
    status: u16,
}

fn read_cqe(mem: &mut TestMem, addr: u64) -> Cqe {
    let mut bytes = [0u8; 16];
    mem.read_physical(addr, &mut bytes);
    let dw2 = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    Cqe {
        sqid: (dw2 >> 16) as u16,
        cid: (dw3 & 0xffff) as u16,
        status: (dw3 >> 16) as u16,
    }
}

#[test]
fn create_io_queues_and_rw_roundtrip_sgl_segment_chain() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    // Enable the controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f); // AQA
    ctrl.mmio_write(0x0028, 8, asq); // ASQ
    ctrl.mmio_write(0x0030, 8, acq); // ACQ
    ctrl.mmio_write(0x0014, 4, 1); // CC.EN

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

    // WRITE 1 sector at LBA 0 using SGL segments.
    let sector_size = 512usize;
    let split = 200usize;
    let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();

    let write_buf1 = 0x60000u64;
    let write_buf2 = 0x61000u64;
    mem.write_physical(write_buf1, &payload[..split]);
    mem.write_physical(write_buf2, &payload[split..]);

    // Two-level segment chain:
    // - DPTR is a Segment descriptor -> list1 (2 descriptors)
    // - list1 contains a Data Block + a Last Segment descriptor -> list2 (1 descriptor)
    // - list2 contains a Data Block
    let list1 = 0x70000u64;
    let list2 = 0x71000u64;
    write_sgl_desc(&mut mem, list1, write_buf1, split as u32, 0x00); // Data Block
    write_sgl_desc(&mut mem, list1 + 16, list2, 16, 0x03); // Last Segment
    write_sgl_desc(
        &mut mem,
        list2,
        write_buf2,
        (sector_size - split) as u32,
        0x00,
    );

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1);
    // Root Segment descriptor: length=32 bytes of descriptors, type=Segment (0x02).
    set_prp2(&mut cmd, 32u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0); // slba low
    set_cdw11(&mut cmd, 0); // slba high
    set_cdw12(&mut cmd, 0); // nlb=0 (1 sector)
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x10);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    // READ it back into different buffers using the same SGL structure.
    let read_buf1 = 0x62000u64;
    let read_buf2 = 0x63000u64;
    let list1_r = 0x72000u64;
    let list2_r = 0x73000u64;

    mem.write_physical(read_buf1, &vec![0u8; split]);
    mem.write_physical(read_buf2, &vec![0u8; sector_size - split]);
    write_sgl_desc(&mut mem, list1_r, read_buf1, split as u32, 0x00);
    write_sgl_desc(&mut mem, list1_r + 16, list2_r, 16, 0x03);
    write_sgl_desc(
        &mut mem,
        list2_r,
        read_buf2,
        (sector_size - split) as u32,
        0x00,
    );

    let mut cmd = build_command(0x02, 1); // READ, PSDT=SGL
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1_r);
    set_prp2(&mut cmd, 32u64 | ((0x02u64) << 56));
    set_cdw12(&mut cmd, 0); // nlb=0 (1 sector)
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2); // SQ1 tail = 2
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    let mut out = vec![0u8; sector_size];
    mem.read_physical(read_buf1, &mut out[..split]);
    mem.read_physical(read_buf2, &mut out[split..]);
    assert_eq!(out, payload);
}

#[test]
fn create_io_queues_and_rw_roundtrip_sgl_datablock_inline() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    // Enable the controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f); // AQA
    ctrl.mmio_write(0x0028, 8, asq); // ASQ
    ctrl.mmio_write(0x0030, 8, acq); // ACQ
    ctrl.mmio_write(0x0014, 4, 1); // CC.EN

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

    // WRITE 1 sector at LBA 0 using a single inline Data Block SGL descriptor (no segment list).
    let sector_size = 512usize;
    let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();

    let write_buf = 0x60000u64;
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x30);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    // Inline Data Block descriptor: length=512, type=0x00 (subtype=0, Data Block).
    set_prp2(&mut cmd, sector_size as u64);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x30);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    // READ it back to a different buffer using an inline Data Block descriptor.
    let read_buf = 0x61000u64;
    mem.write_physical(read_buf, &vec![0u8; sector_size]);

    let mut cmd = build_command(0x02, 1); // READ, PSDT=SGL
    set_cid(&mut cmd, 0x31);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_prp2(&mut cmd, sector_size as u64);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x31);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    let mut out = vec![0u8; sector_size];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn create_io_queues_and_rw_roundtrip_sgl_datablock_inline_unaligned() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    // Enable the controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f); // AQA
    ctrl.mmio_write(0x0028, 8, asq); // ASQ
    ctrl.mmio_write(0x0030, 8, acq); // ACQ
    ctrl.mmio_write(0x0014, 4, 1); // CC.EN

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

    // WRITE 1 sector at LBA 0 using an unaligned inline Data Block SGL descriptor.
    let sector_size = 512usize;
    let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();

    let write_buf = 0x60001u64; // intentionally unaligned
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x40);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_prp2(&mut cmd, sector_size as u64);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x40);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    // READ it back into another unaligned buffer.
    let read_buf = 0x61003u64;
    mem.write_physical(read_buf, &vec![0u8; sector_size]);

    let mut cmd = build_command(0x02, 1); // READ, PSDT=SGL
    set_cid(&mut cmd, 0x41);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_prp2(&mut cmd, sector_size as u64);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.sqid, 1);
    assert_eq!(cqe.cid, 0x41);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    let mut out = vec![0u8; sector_size];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn create_io_queues_and_rw_roundtrip_sgl_segment_multiblock_multi_sector() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    // Enable the controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f); // AQA
    ctrl.mmio_write(0x0028, 8, asq); // ASQ
    ctrl.mmio_write(0x0030, 8, acq); // ACQ
    ctrl.mmio_write(0x0014, 4, 1); // CC.EN

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

    // Write 2 sectors using a single Segment descriptor that points to a list of Data Blocks.
    let sector_size = 512usize;
    let total_len = sector_size * 2;
    let payload: Vec<u8> = (0..total_len as u32).map(|v| (v & 0xff) as u8).collect();

    // Split across 3 guest buffers.
    let len1 = 300usize;
    let len2 = 400usize;
    let len3 = total_len - len1 - len2;
    assert!(len3 > 0);

    let buf1 = 0x60000u64;
    let buf2 = 0x61011u64; // intentionally unaligned
    let buf3 = 0x62000u64;
    mem.write_physical(buf1, &payload[..len1]);
    mem.write_physical(buf2, &payload[len1..len1 + len2]);
    mem.write_physical(buf3, &payload[len1 + len2..]);

    let list = 0x70000u64;
    write_sgl_desc(&mut mem, list, buf1, len1 as u32, 0x00);
    write_sgl_desc(&mut mem, list + 16, buf2, len2 as u32, 0x00);
    write_sgl_desc(&mut mem, list + 32, buf3, len3 as u32, 0x00);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x50);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Root Segment descriptor: 3 descriptors = 48 bytes.
    set_prp2(&mut cmd, 48u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 1); // 2 sectors
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x50);
    assert_eq!(cqe.status & !0x1, 0);

    // Read it back into different buffers using another segment list.
    let out1 = 0x63000u64;
    let out2 = 0x64007u64;
    let out3 = 0x65000u64;
    let list_r = 0x71000u64;
    write_sgl_desc(&mut mem, list_r, out1, len1 as u32, 0x00);
    write_sgl_desc(&mut mem, list_r + 16, out2, len2 as u32, 0x00);
    write_sgl_desc(&mut mem, list_r + 32, out3, len3 as u32, 0x00);

    let mut cmd = build_command(0x02, 1); // READ, PSDT=SGL
    set_cid(&mut cmd, 0x51);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list_r);
    set_prp2(&mut cmd, 48u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 1);
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.cid, 0x51);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; total_len];
    mem.read_physical(out1, &mut out[..len1]);
    mem.read_physical(out2, &mut out[len1..len1 + len2]);
    mem.read_physical(out3, &mut out[len1 + len2..]);
    assert_eq!(out, payload);
}

#[test]
fn create_io_queues_and_rw_roundtrip_sgl_last_segment_root() {
    let disk_sectors = 1024u64;
    let capacity_bytes = disk_sectors * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    // Enable the controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f); // AQA
    ctrl.mmio_write(0x0028, 8, asq); // ASQ
    ctrl.mmio_write(0x0030, 8, acq); // ACQ
    ctrl.mmio_write(0x0014, 4, 1); // CC.EN

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

    // Write 1 sector using a root Last Segment descriptor that points to a list of Data Blocks.
    let sector_size = 512usize;
    let split = 123usize;
    let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();

    let buf1 = 0x60000u64;
    let buf2 = 0x61003u64; // intentionally unaligned
    mem.write_physical(buf1, &payload[..split]);
    mem.write_physical(buf2, &payload[split..]);

    let list = 0x70000u64;
    write_sgl_desc(&mut mem, list, buf1, split as u32, 0x00);
    write_sgl_desc(
        &mut mem,
        list + 16,
        buf2,
        (sector_size - split) as u32,
        0x00,
    );

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x60);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Root Last Segment descriptor: 2 descriptors = 32 bytes.
    set_prp2(&mut cmd, 32u64 | ((0x03u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x60);
    assert_eq!(cqe.status & !0x1, 0);

    // Read it back using another root Last Segment descriptor.
    let out1 = 0x62000u64;
    let out2 = 0x63007u64;
    let list_r = 0x71000u64;
    write_sgl_desc(&mut mem, list_r, out1, split as u32, 0x00);
    write_sgl_desc(
        &mut mem,
        list_r + 16,
        out2,
        (sector_size - split) as u32,
        0x00,
    );

    let mut cmd = build_command(0x02, 1); // READ, PSDT=SGL
    set_cid(&mut cmd, 0x61);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list_r);
    set_prp2(&mut cmd, 32u64 | ((0x03u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.cid, 0x61);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; sector_size];
    mem.read_physical(out1, &mut out[..split]);
    mem.read_physical(out2, &mut out[split..]);
    assert_eq!(out, payload);
}
