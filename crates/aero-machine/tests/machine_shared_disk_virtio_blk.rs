#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::{self, VIRTIO_BLK};
use aero_devices::{a20_gate::A20_GATE_PORT, pci::PCI_CFG_ADDR_PORT, pci::PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_storage::SECTOR_SIZE;
use aero_virtio::devices::blk::{VIRTIO_BLK_S_OK, VIRTIO_BLK_T_IN};
use aero_virtio::pci::{
    VIRTIO_F_VERSION_1, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use firmware::bios::BlockDevice as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(
        PCI_CFG_DATA_PORT + u16::from(offset & 3),
        2,
        u32::from(value),
    );
}

fn write_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT + u16::from(offset & 3), 4, value);
}

fn write_desc(
    m: &mut Machine,
    desc_addr: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = desc_addr + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

fn submit_avail(m: &mut Machine, avail_addr: u64, pos: u16, head: u16) {
    let ring_off = 4 + u64::from(pos) * 2;
    m.write_physical_u16(avail_addr + ring_off, head);
    m.write_physical_u16(avail_addr + 2, pos + 1);
}

#[test]
fn machine_shared_bios_disk_is_visible_to_virtio_blk_dma() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_virtio_blk: true,
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
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Program the virtio-blk controller and issue a READ request.
    let bdf = VIRTIO_BLK.bdf;
    let bar0_base: u64 = 0xE200_0000;

    // BAR0 is mem64.
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        bar0_base as u32,
    );
    write_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x14, 0);

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Allocate virtqueue + request buffers.
    let mut next: u64 = 0x10_000;
    let mut alloc = |size: u64, align: u64| -> u64 {
        let mask = align - 1;
        next = (next + mask) & !mask;
        let out = next;
        next += size;
        out
    };

    let queue_size: u16 = 128;
    let desc_addr = alloc(u64::from(queue_size) * 16, 16);
    let avail_addr = alloc(4 + u64::from(queue_size) * 2, 2);
    let used_addr = alloc(4 + u64::from(queue_size) * 8, 4);

    let req_hdr = alloc(16, 16);
    let data_buf = alloc(SECTOR_SIZE as u64, SECTOR_SIZE as u64);
    let status_buf = alloc(1, 1);

    // Modern virtio-pci common config lives at BAR0 + 0x0000.
    const COMMON_BASE: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY_BASE: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;

    // status = ACKNOWLEDGE | DRIVER
    m.write_physical_u8(bar0_base + COMMON_BASE + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        bar0_base + COMMON_BASE + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // driver_features (low then high 32 bits).
    m.write_physical_u32(bar0_base + COMMON_BASE + 0x08, 0); // driver_feature_select=0
    m.write_physical_u32(bar0_base + COMMON_BASE + 0x0c, 0); // low bits
    m.write_physical_u32(bar0_base + COMMON_BASE + 0x08, 1); // driver_feature_select=1
    m.write_physical_u32(
        bar0_base + COMMON_BASE + 0x0c,
        (VIRTIO_F_VERSION_1 >> 32) as u32,
    );

    // status |= FEATURES_OK (triggers negotiation).
    m.write_physical_u8(
        bar0_base + COMMON_BASE + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );

    // Configure queue 0.
    m.write_physical_u16(bar0_base + COMMON_BASE + 0x16, 0); // queue_select
    m.write_physical_u64(bar0_base + COMMON_BASE + 0x20, desc_addr);
    m.write_physical_u64(bar0_base + COMMON_BASE + 0x28, avail_addr);
    m.write_physical_u64(bar0_base + COMMON_BASE + 0x30, used_addr);
    m.write_physical_u16(bar0_base + COMMON_BASE + 0x1c, 1); // queue_enable

    // status |= DRIVER_OK.
    m.write_physical_u8(
        bar0_base + COMMON_BASE + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // --- Request: READ sector `lba` (IN) ---
    // virtio-blk request header: type=u32, reserved=u32, sector=u64
    m.write_physical_u32(req_hdr, VIRTIO_BLK_T_IN);
    m.write_physical_u32(req_hdr + 4, 0);
    m.write_physical_u64(req_hdr + 8, lba);

    write_desc(&mut m, desc_addr, 0, req_hdr, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut m,
        desc_addr,
        1,
        data_buf,
        SECTOR_SIZE as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        2,
    );
    write_desc(&mut m, desc_addr, 2, status_buf, 1, VIRTQ_DESC_F_WRITE, 0);

    m.write_physical_u8(status_buf, 0xFF);
    submit_avail(&mut m, avail_addr, 0, 0);

    // Notify queue 0 (offset encodes queue index in modern transport).
    m.write_physical_u32(bar0_base + NOTIFY_BASE, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used_addr + 2), 1);
    assert_eq!(m.read_physical_u8(status_buf), VIRTIO_BLK_S_OK);

    let got = m.read_physical_bytes(data_buf, SECTOR_SIZE);
    assert_eq!(&got[..], &expected[..]);
}
