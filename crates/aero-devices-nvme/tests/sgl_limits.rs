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

fn write_sgl_desc(mem: &mut TestMem, addr: u64, ptr: u64, len: u32, type_byte: u8) {
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&ptr.to_le_bytes());
    desc[8..12].copy_from_slice(&len.to_le_bytes());
    desc[15] = type_byte;
    mem.write_physical(addr, &desc);
}

fn setup_ctrl_with_io_queues() -> (NvmeController, TestMem, u64, u64) {
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

    (ctrl, mem, io_cq, io_sq)
}

#[test]
fn sgl_descriptor_count_is_capped() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

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

#[test]
fn sgl_descriptor_count_cap_applies_to_nested_segments() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    // Root Segment list containing a single Segment descriptor which itself would exceed the
    // descriptor cap if expanded.
    let list1 = 0x70000u64;
    let list2 = 0x71000u64;
    // Expand count such that `descriptors_seen + count > NVME_MAX_SGL_DESCRIPTORS`.
    let too_many_desc_bytes = 16u32 * (16 * 1024 - 1);
    write_sgl_desc(&mut mem, list1, list2, too_many_desc_bytes, 0x02);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_non_address_subtype() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let buf = 0x60000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x20);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, buf);
    // Data Block descriptor with subtype=1 (keyed) is not supported.
    set_prp2(&mut cmd, 512u64 | ((0x10u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x20);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_segment_descriptor_requires_alignment() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    // Misaligned segment list pointer.
    let list = 0x70008u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x21);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Segment
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x21);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_allows_segment_descriptor_not_last_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list1 = 0x70000u64;
    let list2 = 0x71000u64;
    let buf1 = 0x60000u64;
    let buf2 = 0x61000u64;

    let sector_size = 512usize;
    let split = 200usize;
    let payload: Vec<u8> = (0..sector_size as u32).map(|v| (v & 0xff) as u8).collect();
    mem.write_physical(buf1, &payload[..split]);
    mem.write_physical(buf2, &payload[split..]);

    // Root points to list1 (2 descriptors).
    // List1: [ Segment -> list2, Data Block (second half) ]
    // List2: [ Data Block (first half) ]
    //
    // This is a valid SGL layout per spec and ensures segment lists can contain Segment
    // descriptors in the middle (not only as the last entry) without reordering bytes.
    write_sgl_desc(&mut mem, list2, buf1, split as u32, 0x00);
    write_sgl_desc(&mut mem, list1, list2, 16, 0x02); // Segment
    write_sgl_desc(
        &mut mem,
        list1 + 16,
        buf2,
        (sector_size - split) as u32,
        0x00,
    );

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x22);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1);
    set_prp2(&mut cmd, 32u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x22);
    assert_eq!(cqe.status & !0x1, 0);

    // Read back with PRP to verify ordering is preserved.
    let read_buf = 0x62000u64;
    mem.write_physical(read_buf, &vec![0u8; sector_size]);

    let mut cmd = build_command(0x02, 0); // READ, PSDT=PRP
    set_cid(&mut cmd, 0x23);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq + 64, &cmd);
    ctrl.mmio_write(0x1008, 4, 2);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq + 16);
    assert_eq!(cqe.cid, 0x23);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; sector_size];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn sgl_datablock_rejects_zero_length() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let buf = 0x60000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x23);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, buf);
    // Data Block descriptor with length=0 is invalid.
    set_prp2(&mut cmd, 0);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x23);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_datablock_rejects_null_address() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x24);
    set_nsid(&mut cmd, 1);
    // Data Block descriptor with addr=0 is invalid.
    set_prp1(&mut cmd, 0);
    set_prp2(&mut cmd, 512);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x24);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_inline_datablock_descriptor_too_short_for_transfer() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    // WRITE 1 sector but provide an inline Data Block descriptor that is smaller than 512 bytes.
    let buf = 0x60000u64;
    mem.write_physical(buf, &[0xAAu8; 100]);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x30);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, buf);
    set_prp2(&mut cmd, 100); // Data Block length = 100 bytes
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0); // nlb=0 (1 sector)
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x30);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_segment_list_too_short_for_transfer() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    // WRITE 1 sector but provide only 400 bytes worth of Data Block descriptors.
    let buf1 = 0x60000u64;
    let buf2 = 0x61000u64;
    mem.write_physical(buf1, &[0xBBu8; 200]);
    mem.write_physical(buf2, &[0xCCu8; 200]);

    let list = 0x70000u64;
    write_sgl_desc(&mut mem, list, buf1, 200, 0x00);
    write_sgl_desc(&mut mem, list + 16, buf2, 200, 0x00);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x31);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 32u64 | ((0x02u64) << 56)); // Segment (2 descriptors)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0); // nlb=0 (1 sector)
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x31);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_inline_descriptor_with_nonzero_reserved_bytes() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let buf = 0x60000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x32);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, buf);
    // Inline Data Block descriptor with reserved bits (DW12..DW13 bits 23:0) set.
    // Layout: dptr2[31:0]=len, dptr2[55:32]=reserved, dptr2[63:56]=type.
    set_prp2(&mut cmd, 512u64 | (0x12u64 << 32));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x32);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_datablock_descriptor_with_nonzero_reserved_bytes_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    let buf = 0x60000u64;

    // Data Block descriptor with reserved bytes set.
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&buf.to_le_bytes());
    desc[8..12].copy_from_slice(&512u32.to_le_bytes());
    desc[12] = 0xAA; // reserved
    desc[15] = 0x00; // Data Block, subtype=0
    mem.write_physical(list, &desc);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x33);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x33);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_segment_descriptor_with_nonzero_reserved_bytes_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    let child_list = 0x71000u64;

    // Segment descriptor with reserved bytes set.
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&child_list.to_le_bytes());
    desc[8..12].copy_from_slice(&16u32.to_le_bytes());
    desc[12] = 0xAA; // reserved
    desc[15] = 0x02; // Segment, subtype=0
    mem.write_physical(list, &desc);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3A);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3A);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_nested_segment_length_not_multiple_of_16() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list1 = 0x70000u64;
    let list2 = 0x71000u64;

    // Root -> list1 containing a Segment descriptor with an invalid length.
    write_sgl_desc(&mut mem, list1, list2, 20, 0x02); // Segment, length not multiple of 16

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3E);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3E);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_nested_segment_descriptor_requires_alignment() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list1 = 0x70000u64;
    let list2 = 0x71008u64; // misaligned

    write_sgl_desc(&mut mem, list1, list2, 16, 0x02); // Segment

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3F);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list1);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3F);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_nested_datablock_null_address() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    // Data Block descriptor with addr=0 is invalid.
    write_sgl_desc(&mut mem, list, 0, 512, 0x00);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x40);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x40);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_inline_descriptor_with_unknown_type() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let buf = 0x60000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x34);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, buf);
    // Unknown descriptor type (low nibble=0x4). Should be rejected.
    set_prp2(&mut cmd, 512u64 | ((0x04u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x34);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_segment_length_not_multiple_of_16() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x35);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Segment length must be a multiple of 16 bytes.
    set_prp2(&mut cmd, 20u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x35);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_segment_length_zero() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x36);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Segment length 0 is invalid.
    set_prp2(&mut cmd, (0x02u64) << 56);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x36);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_last_segment_length_not_multiple_of_16() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x37);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Segment length must be a multiple of 16 bytes.
    set_prp2(&mut cmd, 20u64 | ((0x03u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x37);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_last_segment_length_zero() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x38);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Segment length 0 is invalid.
    set_prp2(&mut cmd, (0x03u64) << 56);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x38);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_root_last_segment_descriptor_requires_alignment() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    // Misaligned segment list pointer.
    let list = 0x70008u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x39);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x03u64) << 56)); // Last Segment
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x39);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_segment_descriptor_with_nonzero_reserved_bits() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3B);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    // Reserved bits (DW12..DW13 bits 23:0) must be zero for address-based SGLs.
    // Layout: dptr2[31:0]=len, dptr2[55:32]=reserved, dptr2[63:56]=type.
    set_prp2(&mut cmd, 16u64 | (0x12u64 << 32) | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3B);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_last_segment_descriptor_with_nonzero_reserved_bits() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3C);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | (0x12u64 << 32) | ((0x03u64) << 56));
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3C);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_segment_list_address_overflow() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    // Root points to a list with a single Segment descriptor that would overflow `u64` when the
    // controller attempts to read the second 16-byte descriptor.
    //
    // Use a 16-byte aligned address near `u64::MAX` so the alignment check passes.
    let near_max_aligned = u64::MAX - 0x0F;
    write_sgl_desc(&mut mem, list, near_max_aligned, 32, 0x02); // Segment, 2 descriptors

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x3D);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x3D);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_unknown_descriptor_type_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    let buf = 0x60000u64;

    // Segment list containing an unknown descriptor type should be rejected.
    write_sgl_desc(&mut mem, list, buf, 512, 0x04);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x41);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x41);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_non_address_subtype_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    let buf = 0x60000u64;

    // Segment list containing a Data Block descriptor with a non-address subtype (subtype=1) should
    // be rejected.
    write_sgl_desc(&mut mem, list, buf, 512, 0x10);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x42);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x42);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_datablock_zero_length_in_segment_list() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;
    let buf = 0x60000u64;

    // Data Block descriptor with length=0 is invalid (even inside a segment list).
    write_sgl_desc(&mut mem, list, buf, 0, 0x00);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x43);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x43);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_nested_segment_null_address() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let list = 0x70000u64;

    // Segment descriptor with addr=0 is invalid.
    write_sgl_desc(&mut mem, list, 0, 16, 0x02);

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x44);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, list);
    set_prp2(&mut cmd, 16u64 | ((0x02u64) << 56)); // Root Segment (1 descriptor)
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x44);
    assert_eq!(cqe.status & !0x1, 0x4004);
}

#[test]
fn sgl_rejects_root_last_segment_null_address() {
    let (mut ctrl, mut mem, io_cq, io_sq) = setup_ctrl_with_io_queues();

    let mut cmd = build_command(0x01, 1); // WRITE, PSDT=SGL
    set_cid(&mut cmd, 0x45);
    set_nsid(&mut cmd, 1);
    // Root Last Segment descriptor with addr=0 is invalid.
    set_prp1(&mut cmd, 0);
    set_prp2(&mut cmd, 16u64 | ((0x03u64) << 56)); // Last Segment
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    ctrl.mmio_write(0x1008, 4, 1);
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, io_cq);
    assert_eq!(cqe.cid, 0x45);
    assert_eq!(cqe.status & !0x1, 0x4004);
}
