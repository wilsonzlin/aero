use std::sync::{Arc, Mutex};

use aero_devices::pci::PciDevice;
use aero_devices_nvme::{AeroStorageDiskAdapter, NvmePciDevice};
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
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
    inner: Arc<Mutex<RawDisk<MemBackend>>>,
}

impl SharedDisk {
    fn new(sectors: u64) -> Self {
        let capacity_bytes = sectors * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
        Self {
            inner: Arc::new(Mutex::new(disk)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RawDisk<MemBackend>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl VirtualDisk for SharedDisk {
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
        self.lock().flush()
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
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Program some PCI config-space state so the snapshot exercises `PciConfigSpaceState`.
    dev.config_mut().write(0x04, 2, 0x0006); // memory + bus master
    dev.config_mut().write(0x10, 4, 0xfebf_0000);
    dev.config_mut().write(0x14, 4, 0);
    // Leave BAR0 in probe mode so the snapshot must preserve the BAR probe flag too.
    dev.config_mut().write(0x10, 4, 0xffff_ffff);
    dev.config_mut().write(0x3c, 1, 0x55); // interrupt line
    let pci_state_before = dev.config().snapshot_state();

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let write_buf = 0x60000;
    let read_buf = 0x61000;

    // Configure + enable controller.
    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq, &cmd);
    dev.controller.mmio_write(0x1000, 4, 1); // SQ0 tail = 1
    dev.process(&mut mem);

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    dev.controller.mmio_write(0x1000, 4, 2); // SQ0 tail = 2
    dev.process(&mut mem);

    // Consume admin CQ completions so INTx level reflects IO CQ only.
    dev.controller.mmio_write(0x1004, 4, 2);

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
    mem.write_physical(io_sq, &cmd);
    dev.controller.mmio_write(0x1008, 4, 1); // SQ1 tail = 1
    dev.process(&mut mem);

    assert!(dev.irq_level());

    let snap = dev.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem2 = mem_snap;
    restored.load_state(&snap).unwrap();

    assert_eq!(
        restored.config().snapshot_state(),
        pci_state_before,
        "PCI config-space state should survive NVMe device snapshot/restore"
    );

    // Pending completion should keep INTx asserted.
    assert!(restored.irq_level());

    let cqe = read_cqe(&mut mem2, io_cq);
    assert_eq!(cqe.cid, 0x10);
    assert_eq!(cqe.status & 0x1, 1); // phase
    assert_eq!(cqe.status & !0x1, 0); // success

    // Consume completion and ensure INTx deasserts.
    restored.controller.mmio_write(0x100c, 4, 1); // CQ1 head = 1
    assert!(!restored.irq_level());

    // READ it back after restore.
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem2.write_physical(io_sq + 64, &cmd);
    restored.controller.mmio_write(0x1008, 4, 2); // SQ1 tail = 2
    restored.process(&mut mem2);

    let cqe = read_cqe(&mut mem2, io_cq + 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    mem2.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn snapshot_restore_replays_unprocessed_admin_sq_doorbell() {
    // NVMe MMIO doorbells defer DMA processing into `NvmeController::process()`. A snapshot can be
    // taken between a doorbell write and the next processing step; ensure restore can still make
    // forward progress (i.e. we don't lose the doorbell).
    let disk = SharedDisk::new(1024);
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable bus mastering so the controller is permitted to DMA when `process()` runs.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let asq = 0x10000;
    let acq = 0x20000;
    let id_buf = 0x30000;

    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1);

    // Admin IDENTIFY controller (CNS=1).
    let mut cmd = build_command(0x06);
    set_cid(&mut cmd, 0x1234);
    set_prp1(&mut cmd, id_buf);
    set_cdw10(&mut cmd, 0x01);
    mem.write_physical(asq, &cmd);

    // Ring the SQ0 tail doorbell but do not call `process()` yet.
    dev.controller.mmio_write(0x1000, 4, 1);
    assert!(
        !dev.irq_level(),
        "no completion should be pending before processing the admin SQ"
    );

    let snap = dev.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    restored.load_state(&snap).unwrap();

    let mut mem2 = mem_snap;
    assert!(
        !restored.irq_level(),
        "restored device should not assert INTx until the deferred SQ processing runs"
    );

    restored.process(&mut mem2);
    assert!(
        restored.irq_level(),
        "processing should post a completion and assert INTx"
    );

    let cqe = read_cqe(&mut mem2, acq);
    assert_eq!(cqe.cid, 0x1234);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    let mut vid_bytes = [0u8; 2];
    mem2.read_physical(id_buf, &mut vid_bytes);
    let vid = u16::from_le_bytes(vid_bytes);
    assert_eq!(vid, 0x1b36);
}

#[test]
fn snapshot_restore_replays_unprocessed_io_sq_doorbell_and_persists_write() {
    // Similar to `snapshot_restore_replays_unprocessed_admin_sq_doorbell`, but for an IO SQ.
    // This exercises that `NvmeController::load_state()` reconstructs `pending_sq_tail` for IO
    // queues (not just the admin queue).
    let disk = SharedDisk::new(1024);
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable bus mastering so the controller is permitted to DMA when `process()` runs.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let write_buf = 0x60000;

    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq, &cmd);
    dev.controller.mmio_write(0x1000, 4, 1);
    dev.process(&mut mem);

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    dev.controller.mmio_write(0x1000, 4, 2);
    dev.process(&mut mem);

    // Consume admin completions so INTx level is not influenced by them.
    dev.controller.mmio_write(0x1004, 4, 2);
    assert!(
        !dev.irq_level(),
        "after consuming admin completions, no interrupts should be pending"
    );

    // Post an IO WRITE into SQ1 entry 0.
    let payload: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);

    // Ring SQ1 tail doorbell but *do not* call `process()` yet.
    dev.controller.mmio_write(0x1008, 4, 1);
    assert!(
        !dev.irq_level(),
        "no completion should be pending before processing the IO SQ"
    );

    let snap = dev.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    restored.load_state(&snap).unwrap();

    let mut mem2 = mem_snap;
    restored.process(&mut mem2);

    assert!(
        restored.irq_level(),
        "processing should post an IO completion"
    );

    let cqe = read_cqe(&mut mem2, io_cq);
    assert_eq!(cqe.cid, 0x10);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    let mut disk_read = disk.clone();
    let mut out = vec![0u8; 512];
    disk_read.read_at(0, &mut out).unwrap();
    assert_eq!(out, payload);
}

#[test]
fn snapshot_restore_preserves_cq_phase_across_wrap() {
    let disk = SharedDisk::new(1024);
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable bus mastering so the controller is permitted to DMA when `process()` runs.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;

    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1);

    // Create IO CQ (qid=1, size=2, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (1u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq, &cmd);
    dev.controller.mmio_write(0x1000, 4, 1);
    dev.process(&mut mem);

    // Create IO SQ (qid=1, size=2, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (1u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    dev.controller.mmio_write(0x1000, 4, 2);
    dev.process(&mut mem);

    // Consume admin CQ completions (2 entries).
    dev.controller.mmio_write(0x1004, 4, 2);

    let sq_tail_db = 0x1008;
    let cq_head_db = 0x100c;

    // 1) FLUSH at SQ slot 0, CQ slot 0, phase=1.
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    mem.write_physical(io_sq, &cmd);
    dev.controller.mmio_write(sq_tail_db, 4, 1);
    dev.process(&mut mem);
    assert!(dev.irq_level());

    dev.controller.mmio_write(cq_head_db, 4, 1);
    assert!(!dev.irq_level());

    // 2) FLUSH at SQ slot 1, CQ slot 1, phase=1 (tail wraps and toggles phase for the *next* CQE).
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    mem.write_physical(io_sq + 64, &cmd);
    dev.controller.mmio_write(sq_tail_db, 4, 0);
    dev.process(&mut mem);
    assert!(dev.irq_level());

    // Snapshot while CQ tail has wrapped (phase has toggled) but CQE#2 is still pending.
    let snap = dev.save_state();
    let mem_snap = mem.clone();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem2 = mem_snap;
    restored.load_state(&snap).unwrap();

    assert!(restored.irq_level());

    let cqe = read_cqe(&mut mem2, io_cq + 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & 0x1, 1);
    assert_eq!(cqe.status & !0x1, 0);

    // Consume CQE#2 (head wraps to 0).
    restored.controller.mmio_write(cq_head_db, 4, 0);
    assert!(!restored.irq_level());

    // 3) Next FLUSH should reuse CQ slot 0 with phase=0 (because the tail wrapped after CQE#2).
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x12);
    set_nsid(&mut cmd, 1);
    mem2.write_physical(io_sq, &cmd);
    restored.controller.mmio_write(sq_tail_db, 4, 1);
    restored.process(&mut mem2);

    let cqe = read_cqe(&mut mem2, io_cq);
    assert_eq!(cqe.cid, 0x12);
    assert_eq!(cqe.status & 0x1, 0);
    assert_eq!(cqe.status & !0x1, 0);
}

#[test]
fn snapshot_restore_accepts_legacy_nvmp_1_0_pci_payload() {
    // The `NvmePciDevice` snapshot format was historically `NVMP 1.0` with a bespoke PCI payload.
    // Keep a regression test to ensure we never break restore for existing snapshots.
    let disk = SharedDisk::new(1024);
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));

    // Program some config-space state so the legacy PCI payload is non-trivial.
    dev.config_mut().write(0x04, 4, (0x1234u32 << 16) | 0x0006); // status + command
    dev.config_mut().write(0x10, 4, 0xfebf_0000);
    dev.config_mut().write(0x14, 4, 0);
    dev.config_mut().write(0x10, 4, 0xffff_ffff); // BAR probe mode
    dev.config_mut().write(0x3c, 1, 0x5a); // interrupt line

    let expected_pci_state = dev.config().snapshot_state();

    // Serialize a legacy NVMP 1.0 snapshot.
    let bar0 = expected_pci_state.bar_base[0];
    let bar0_probe = expected_pci_state.bar_probe[0];
    let command = dev.config().command();
    let status = 0x1234u16;
    let interrupt_line = 0x5au8;

    let mut w = SnapshotWriter::new(*b"NVMP", SnapshotVersion::new(1, 0));
    let pci = Encoder::new()
        .u64(bar0)
        .bool(bar0_probe)
        .u16(command)
        .u16(status)
        .u8(interrupt_line)
        .finish();
    w.field_bytes(1, pci);
    w.field_bytes(2, dev.controller.save_state());
    let legacy = w.finish();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    restored.load_state(&legacy).unwrap();

    assert_eq!(
        restored.config().snapshot_state(),
        expected_pci_state,
        "legacy NVMP 1.0 PCI payload should restore into PciConfigSpaceState deterministically"
    );
}

#[test]
fn snapshot_restore_preserves_pci_interrupt_disable_masking() {
    // PCI command bit 10 disables legacy INTx signalling. Ensure the NVMe wrapper:
    // - gates `irq_level()` when the bit is set, and
    // - preserves the bit across snapshot/restore.
    let disk = SharedDisk::new(1024);
    let mut dev = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable bus mastering so the controller is permitted to DMA and raise INTx for the pending IO
    // completion we generate below.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let asq = 0x10000;
    let acq = 0x20000;
    let io_cq = 0x40000;
    let io_sq = 0x50000;
    let write_buf = 0x60000;

    dev.controller.mmio_write(0x0024, 4, 0x000f_000f);
    dev.controller.mmio_write(0x0028, 8, asq);
    dev.controller.mmio_write(0x0030, 8, acq);
    dev.controller.mmio_write(0x0014, 4, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    mem.write_physical(asq, &cmd);
    dev.controller.mmio_write(0x1000, 4, 1);
    dev.process(&mut mem);

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    mem.write_physical(asq + 64, &cmd);
    dev.controller.mmio_write(0x1000, 4, 2);
    dev.process(&mut mem);

    // Consume admin CQ completions.
    dev.controller.mmio_write(0x1004, 4, 2);

    // Post a write to generate a pending IO completion (asserts controller INTx).
    let payload: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    mem.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    mem.write_physical(io_sq, &cmd);
    dev.controller.mmio_write(0x1008, 4, 1);
    dev.process(&mut mem);

    assert!(
        dev.controller.intx_level,
        "controller should have a pending interrupt"
    );
    assert!(
        dev.irq_level(),
        "with PCI Interrupt Disable clear, wrapper should expose INTx"
    );

    // Set PCI command bit 10 (Interrupt Disable) and ensure the wrapper masks the IRQ.
    let mut cmd_reg = dev.config().command();
    cmd_reg |= 1 << 10;
    dev.config_mut().write(0x04, 2, cmd_reg.into());
    assert!(
        !dev.irq_level(),
        "PCI command Interrupt Disable bit should mask legacy INTx level"
    );

    let snap = dev.save_state();

    let mut restored = NvmePciDevice::new(Box::new(AeroStorageDiskAdapter::new(Box::new(
        disk.clone(),
    ))));
    restored.load_state(&snap).unwrap();

    assert!(
        restored.controller.intx_level,
        "restored controller should still have a pending interrupt"
    );
    assert!(
        !restored.irq_level(),
        "restored wrapper must keep PCI Interrupt Disable masking"
    );

    // Clear the bit and verify the pending completion becomes visible again.
    let cleared = restored.config().command() & !(1 << 10);
    restored.config_mut().write(0x04, 2, cleared.into());
    assert!(
        restored.irq_level(),
        "clearing PCI Interrupt Disable should re-expose the pending INTx level"
    );
}
