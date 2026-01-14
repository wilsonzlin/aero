use aero_devices_nvme::NvmeController;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};

const NVME_REG_CC: u64 = 0x0014;
const NVME_REG_CSTS: u64 = 0x001c;
const NVME_REG_AQA: u64 = 0x0024;
const NVME_REG_ASQ: u64 = 0x0028;
const NVME_REG_ACQ: u64 = 0x0030;

const CC_EN: u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

fn make_controller() -> NvmeController {
    let capacity_bytes = 8u64 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity_bytes).unwrap();
    NvmeController::try_new_from_aero_storage(disk).unwrap()
}

fn program_minimal_admin_queues(ctrl: &mut NvmeController) {
    // Provide a valid admin queue pair so CC.EN attempts reach the page size validation logic.
    ctrl.mmio_write(NVME_REG_AQA, 4, 0x000f_000f); // 16 entries for SQ/CQ (encoded as N-1)
    ctrl.mmio_write(NVME_REG_ASQ, 8, 0x10000);
    ctrl.mmio_write(NVME_REG_ACQ, 8, 0x20000);
}

#[test]
fn nvme_enable_rejects_unsupported_page_size() {
    let mut ctrl = make_controller();
    program_minimal_admin_queues(&mut ctrl);

    // MPS=15 => 2^(12 + 15) = 128MiB pages. This device only advertises 4KiB pages (MPS=0).
    let cc = CC_EN | (15u32 << 7);
    ctrl.mmio_write(NVME_REG_CC, 4, u64::from(cc));

    let csts = ctrl.mmio_read(NVME_REG_CSTS, 4) as u32;
    assert_eq!(csts & CSTS_RDY, 0);
    assert_ne!(csts & CSTS_CFS, 0);
}

#[test]
fn nvme_enable_with_large_aqa_does_not_panic() {
    let mut ctrl = make_controller();
    program_minimal_admin_queues(&mut ctrl);

    // Regression: avoid debug-build overflow when decoding (u16 field + 1).
    ctrl.mmio_write(NVME_REG_AQA, 4, 0xffff_ffff);

    // Enable with default page size (MPS=0 => 4KiB).
    ctrl.mmio_write(NVME_REG_CC, 4, u64::from(CC_EN));

    // Queue sizes above CAP.MQES should be rejected; the key property here is "no panic".
    let csts = ctrl.mmio_read(NVME_REG_CSTS, 4) as u32;
    assert_eq!(csts & CSTS_RDY, 0);
}
