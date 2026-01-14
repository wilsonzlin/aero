use aero_devices::pci::PciDevice;
use aero_devices_nvme::NvmePciDevice;
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

#[derive(Debug)]
struct CqEntry {
    dw0: u32,
    cid: u16,
    status: u16,
}

fn read_cqe(mem: &mut TestMem, addr: u64) -> CqEntry {
    let mut bytes = [0u8; 16];
    mem.read_physical(addr, &mut bytes);
    let dw0 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    CqEntry {
        dw0,
        cid: (dw3 & 0xffff) as u16,
        status: (dw3 >> 16) as u16,
    }
}

fn assert_success_status(status: u16) {
    assert_eq!(status & !0x1, 0, "expected success status, got {status:#x}");
}

fn build_command(opc: u8) -> [u8; 64] {
    let mut cmd = [0u8; 64];
    cmd[0] = opc;
    cmd
}

fn set_cid(cmd: &mut [u8; 64], cid: u16) {
    cmd[2..4].copy_from_slice(&cid.to_le_bytes());
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

fn submit_admin(
    dev: &mut NvmePciDevice,
    mem: &mut TestMem,
    asq: u64,
    acq: u64,
    next_sq_slot: &mut u16,
    next_cq_slot: &mut u16,
    cmd: [u8; 64],
) -> CqEntry {
    let slot = *next_sq_slot as u64;
    mem.write_physical(asq + slot * 64, &cmd);
    *next_sq_slot += 1;
    dev.controller
        .mmio_write(0x1000, 4, u64::from(*next_sq_slot));
    dev.process(mem);

    let cqe_addr = acq + u64::from(*next_cq_slot) * 16;
    let cqe = read_cqe(mem, cqe_addr);
    *next_cq_slot += 1;
    cqe
}

fn enable_controller(dev: &mut NvmePciDevice, asq: u64, acq: u64) {
    // Enable MMIO decoding + bus mastering so the NVMe wrapper allows DMA in `process()`.
    dev.config_mut().set_command(0x0006); // MEM + BME

    // AQA: 0-based sizes; use 16-entry admin SQ/CQ.
    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1); // CC.EN
}

#[test]
fn admin_delete_io_sq_cq_allows_recreate() {
    let mut dev = NvmePciDevice::default();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    enable_controller(&mut dev, asq, acq);

    let mut sq_slot = 0u16;
    let mut cq_slot = 0u16;

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 1);
    assert_success_status(cqe.status);

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 2);
    assert_success_status(cqe.status);

    // Delete IO SQ 1.
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 3);
    set_cdw10(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 3);
    assert_success_status(cqe.status);

    // Delete IO CQ 1.
    let mut cmd = build_command(0x04);
    set_cid(&mut cmd, 4);
    set_cdw10(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 4);
    assert_success_status(cqe.status);

    // Recreate them with the same QID.
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 5);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 5);
    assert_success_status(cqe.status);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 6);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_eq!(cqe.cid, 6);
    assert_success_status(cqe.status);
}

#[test]
fn admin_get_set_features_roundtrip() {
    let mut dev = NvmePciDevice::default();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;

    enable_controller(&mut dev, asq, acq);

    let mut sq_slot = 0u16;
    let mut cq_slot = 0u16;

    // Defaults.
    let mut cmd = build_command(0x0a); // GET FEATURES
    set_cid(&mut cmd, 1);
    set_cdw10(&mut cmd, 0x07); // Number of Queues
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0x00ff_00ff);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 2);
    set_cdw10(&mut cmd, 0x08); // Interrupt Coalescing
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 3);
    set_cdw10(&mut cmd, 0x06); // Volatile Write Cache
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0);

    // SET then GET: Interrupt Coalescing.
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 4);
    set_cdw10(&mut cmd, 0x08);
    set_cdw11(&mut cmd, 0x1234);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0x1234);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 5);
    set_cdw10(&mut cmd, 0x08);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0x1234);

    // SET then GET: Number of Queues (request 4x SQ/CQ => 0-based value 3).
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 6);
    set_cdw10(&mut cmd, 0x07);
    set_cdw11(&mut cmd, (3u32 << 16) | 3u32);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, (3u32 << 16) | 3u32);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 7);
    set_cdw10(&mut cmd, 0x07);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, (3u32 << 16) | 3u32);

    // SET then GET: VWC enable.
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 8);
    set_cdw10(&mut cmd, 0x06);
    set_cdw11(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 1);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 9);
    set_cdw10(&mut cmd, 0x06);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 1);
}

#[test]
fn snapshot_restore_preserves_features_and_queue_existence() {
    let mut dev = NvmePciDevice::default();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    enable_controller(&mut dev, asq, acq);

    let mut sq_slot = 0u16;
    let mut cq_slot = 0u16;

    // Program some feature values.
    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 1);
    set_cdw10(&mut cmd, 0x08);
    set_cdw11(&mut cmd, 0xbeef);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 2);
    set_cdw10(&mut cmd, 0x06);
    set_cdw11(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);

    let mut cmd = build_command(0x09);
    set_cid(&mut cmd, 3);
    set_cdw10(&mut cmd, 0x07);
    set_cdw11(&mut cmd, (7u32 << 16) | 7u32); // request 8x queues
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);

    // Create IO CQ+SQ.
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 4);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 5);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    let cqe = submit_admin(
        &mut dev,
        &mut mem,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);

    let snap = dev.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmePciDevice::default();
    restored.load_state(&snap).unwrap();
    let mut mem2 = mem_snap;

    // After restore, issue GET FEATURES to confirm values survived.
    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 0x10);
    set_cdw10(&mut cmd, 0x08);
    let cqe = submit_admin(
        &mut restored,
        &mut mem2,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 0xbeef);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 0x11);
    set_cdw10(&mut cmd, 0x06);
    let cqe = submit_admin(
        &mut restored,
        &mut mem2,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, 1);

    let mut cmd = build_command(0x0a);
    set_cid(&mut cmd, 0x12);
    set_cdw10(&mut cmd, 0x07);
    let cqe = submit_admin(
        &mut restored,
        &mut mem2,
        asq,
        acq,
        &mut sq_slot,
        &mut cq_slot,
        cmd,
    );
    assert_success_status(cqe.status);
    assert_eq!(cqe.dw0, (7u32 << 16) | 7u32);

    // Verify IO SQ/CQ still exist by submitting a flush into SQ1 and observing a completion in CQ1.
    let mut io_cmd = build_command(0x00); // FLUSH
    set_cid(&mut io_cmd, 0x99);
    mem2.write_physical(io_sq, &io_cmd);
    restored.controller.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    restored.process(&mut mem2);

    let cqe = read_cqe(&mut mem2, io_cq);
    assert_eq!(cqe.cid, 0x99);
    assert_success_status(cqe.status);
}
