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

fn set_prp1(cmd: &mut [u8; 64], prp1: u64) {
    cmd[24..32].copy_from_slice(&prp1.to_le_bytes());
}

fn set_prp2(cmd: &mut [u8; 64], prp2: u64) {
    cmd[32..40].copy_from_slice(&prp2.to_le_bytes());
}

fn set_cdw10(cmd: &mut [u8; 64], val: u32) {
    cmd[40..44].copy_from_slice(&val.to_le_bytes());
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
fn identify_controller_supports_sgl_data_pointer() {
    let disk = RawDisk::create(MemBackend::new(), 1024 * SECTOR_SIZE as u64).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    // Enable controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // IDENTIFY Controller (CNS=1) with PSDT=SGL and a single Data Block descriptor.
    let mut cmd = build_command(0x06, 1);
    set_cid(&mut cmd, 0x1234);
    set_prp1(&mut cmd, id_buf);
    // DPTR2: length (low 32) + type (high byte). Type=0x00 (Data Block, subtype=0).
    set_prp2(&mut cmd, 4096u64 | ((0x00u64) << 56));
    set_cdw10(&mut cmd, 0x01);

    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, acq);
    assert_eq!(cqe.sqid, 0);
    assert_eq!(cqe.cid, 0x1234);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    // Verify the identify payload was written.
    assert_eq!(mem.read_u16(id_buf), 0x1b36);
    // SGLS at offset 536 advertises Data Block + Segment + Last Segment support.
    assert_eq!(mem.read_u32(id_buf + 536), 0xD);
}

#[test]
fn identify_controller_supports_sgl_segment_list() {
    let disk = RawDisk::create(MemBackend::new(), 1024 * SECTOR_SIZE as u64).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000u64;
    let acq = 0x20000u64;

    // Enable controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // Split the 4096-byte Identify payload across multiple SGL Data Block descriptors, including
    // an unaligned buffer.
    let buf1 = 0x30000u64;
    let buf2 = 0x31007u64;
    let buf3 = 0x32000u64;

    let len1 = 300usize;
    let len2 = 400usize;
    let len3 = 4096usize - len1 - len2;

    let list = 0x34000u64;
    write_sgl_desc(&mut mem, list, buf1, len1 as u32, 0x00);
    write_sgl_desc(&mut mem, list + 16, buf2, len2 as u32, 0x00);
    write_sgl_desc(&mut mem, list + 32, buf3, len3 as u32, 0x00);

    let mut cmd = build_command(0x06, 1); // IDENTIFY, PSDT=SGL
    set_cid(&mut cmd, 0x1235);
    set_prp1(&mut cmd, list);
    // Root Segment descriptor: 3 descriptors = 48 bytes.
    set_prp2(&mut cmd, 48u64 | ((0x02u64) << 56));
    set_cdw10(&mut cmd, 0x01); // CNS=1 (Identify Controller)

    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, acq);
    assert_eq!(cqe.sqid, 0);
    assert_eq!(cqe.cid, 0x1235);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    // Reconstruct the payload from the scattered buffers.
    let mut out = vec![0u8; 4096];
    mem.read_physical(buf1, &mut out[..len1]);
    mem.read_physical(buf2, &mut out[len1..len1 + len2]);
    mem.read_physical(buf3, &mut out[len1 + len2..]);

    assert_eq!(u16::from_le_bytes(out[0..2].try_into().unwrap()), 0x1b36);
    assert_eq!(
        u32::from_le_bytes(out[536..540].try_into().unwrap()),
        0xD,
        "SGLS should advertise Data Block + Segment + Last Segment"
    );
}

#[test]
fn identify_controller_supports_sgl_last_segment_root() {
    let disk = RawDisk::create(MemBackend::new(), 1024 * SECTOR_SIZE as u64).unwrap();
    let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000u64;
    let acq = 0x20000u64;

    // Enable controller with a 16-entry admin queue pair.
    ctrl.mmio_write(0x0024, 4, 0x000f_000f);
    ctrl.mmio_write(0x0028, 8, asq);
    ctrl.mmio_write(0x0030, 8, acq);
    ctrl.mmio_write(0x0014, 4, 1);

    // Two Data Blocks split across 2 buffers; use a root Last Segment descriptor.
    let buf1 = 0x30000u64;
    let buf2 = 0x31005u64; // intentionally unaligned

    let len1 = 777usize;
    let len2 = 4096usize - len1;

    let list = 0x34000u64;
    write_sgl_desc(&mut mem, list, buf1, len1 as u32, 0x00);
    write_sgl_desc(&mut mem, list + 16, buf2, len2 as u32, 0x00);

    let mut cmd = build_command(0x06, 1); // IDENTIFY, PSDT=SGL
    set_cid(&mut cmd, 0x1236);
    set_prp1(&mut cmd, list);
    // Root Last Segment descriptor: 2 descriptors = 32 bytes.
    set_prp2(&mut cmd, 32u64 | ((0x03u64) << 56));
    set_cdw10(&mut cmd, 0x01); // CNS=1 (Identify Controller)

    mem.write_physical(asq, &cmd);
    ctrl.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
    ctrl.process(&mut mem);

    let cqe = read_cqe(&mut mem, acq);
    assert_eq!(cqe.sqid, 0);
    assert_eq!(cqe.cid, 0x1236);
    assert_eq!(cqe.status & !0x1, 0, "expected success status");

    let mut out = vec![0u8; 4096];
    mem.read_physical(buf1, &mut out[..len1]);
    mem.read_physical(buf2, &mut out[len1..]);

    assert_eq!(u16::from_le_bytes(out[0..2].try_into().unwrap()), 0x1b36);
    assert_eq!(u32::from_le_bytes(out[536..540].try_into().unwrap()), 0xD);
}
