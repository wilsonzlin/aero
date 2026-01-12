#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::NVME_CONTROLLER;
use aero_machine::{Machine, MachineConfig};
use aero_storage::SECTOR_SIZE;
use firmware::bios::BlockDevice as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    m.io_write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    m.io_write(0xCFC, 2, u32::from(value));
}

fn write_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    m.io_write(0xCFC, 4, value);
}

fn build_cmd(opc: u8) -> [u8; 64] {
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

fn set_cdw10(cmd: &mut [u8; 64], cdw10: u32) {
    cmd[40..44].copy_from_slice(&cdw10.to_le_bytes());
}

fn set_cdw11(cmd: &mut [u8; 64], cdw11: u32) {
    cmd[44..48].copy_from_slice(&cdw11.to_le_bytes());
}

fn set_cdw12(cmd: &mut [u8; 64], cdw12: u32) {
    cmd[48..52].copy_from_slice(&cdw12.to_le_bytes());
}

fn read_cqe_cid(m: &mut Machine, cqe: u64) -> u16 {
    m.read_physical_u16(cqe + 12)
}

fn read_cqe_status(m: &mut Machine, cqe: u64) -> u16 {
    m.read_physical_u16(cqe + 14)
}

#[test]
fn machine_shared_bios_disk_is_visible_to_nvme_dma() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep this test focused on storage.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Install a multi-sector disk image with a recognizable marker at a non-zero LBA.
    let lba = 4u64;
    let marker = b"AERO_SHARED_DISK";
    let mut bytes = vec![0u8; 8 * SECTOR_SIZE];
    let start = (lba as usize) * SECTOR_SIZE;
    bytes[start..start + marker.len()].copy_from_slice(marker);
    m.set_disk_image(bytes).unwrap();

    // Read the expected sector via the BIOS `BlockDevice` path.
    let mut bios_disk = m.shared_disk();
    let mut expected = [0u8; SECTOR_SIZE];
    bios_disk.read_sector(lba, &mut expected).unwrap();
    assert_eq!(&expected[..marker.len()], marker);

    // Enable A20 before touching high MMIO addresses.
    m.io_write(0x92, 1, 0x02);

    // Program the NVMe controller and issue a READ command via IO queues.
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base: u64 = 0xE300_0000;

    // BAR0 is mem64.
    write_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x10, bar0_base as u32);
    write_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x14, 0);

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Guest memory: admin queues + IO queues + read buffer (all 4K-aligned).
    let mut next: u64 = 0x10_000;
    let mut alloc = |size: u64, align: u64| -> u64 {
        let mask = align - 1;
        next = (next + mask) & !mask;
        let out = next;
        next += size;
        out
    };

    let asq = alloc(4096, 4096);
    let acq = alloc(4096, 4096);
    let io_cq = alloc(4096, 4096);
    let io_sq = alloc(4096, 4096);
    let read_buf = alloc(4096, 4096);

    // Configure controller (AQA/ASQ/ACQ then CC.EN).
    m.write_physical_u32(bar0_base + 0x24, 0x000f_000f); // 16-entry SQ/CQ
    m.write_physical_u64(bar0_base + 0x28, asq);
    m.write_physical_u64(bar0_base + 0x30, acq);
    m.write_physical_u32(bar0_base + 0x14, 1); // CC.EN

    // --- Admin: Create IO CQ (qid=1, size=16, PC+IEN) ---
    let mut cmd = build_cmd(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    m.write_physical(asq, &cmd);

    // Ring admin SQ tail doorbell to 1 and process.
    m.write_physical_u32(bar0_base + 0x1000, 1);
    m.process_nvme();

    assert_eq!(read_cqe_cid(&mut m, acq), 1);
    assert_eq!(read_cqe_status(&mut m, acq) & 0xFFFE, 0);

    // Consume admin completion and ensure INTx level is not influenced by it.
    m.write_physical_u32(bar0_base + 0x1004, 1); // CQ0 head = 1

    // --- Admin: Create IO SQ (qid=1, size=16, CQID=1) ---
    let mut cmd = build_cmd(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    m.write_physical(asq + 64, &cmd);

    m.write_physical_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    m.process_nvme();

    assert_eq!(read_cqe_cid(&mut m, acq + 16), 2);
    assert_eq!(read_cqe_status(&mut m, acq + 16) & 0xFFFE, 0);
    m.write_physical_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    // --- IO: READ 1 sector at `lba` into read_buf ---
    let mut cmd = build_cmd(0x02);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, lba as u32);
    set_cdw11(&mut cmd, (lba >> 32) as u32);
    set_cdw12(&mut cmd, 0); // NLB=0 => 1 block
    m.write_physical(io_sq, &cmd);

    m.write_physical_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1
    m.process_nvme();

    assert_eq!(read_cqe_cid(&mut m, io_cq), 0x10);
    assert_eq!(read_cqe_status(&mut m, io_cq) & 0xFFFE, 0);
    m.write_physical_u32(bar0_base + 0x100c, 1); // CQ1 head = 1

    let got = m.read_physical_bytes(read_buf, SECTOR_SIZE);
    assert_eq!(&got[..], &expected[..]);
}

