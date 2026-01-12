use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use aero_devices::pci::PciDevice as _;
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::IsoBackend;
use aero_devices_storage::pci_ide::{
    register_piix3_ide_ports, Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::IoPortBus;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

#[derive(Debug)]
struct MemIso {
    sector_count: u32,
    data: Vec<u8>,
}

impl MemIso {
    fn new(sectors: u32) -> Self {
        Self {
            sector_count: sectors,
            data: vec![0u8; sectors as usize * 2048],
        }
    }
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(2048) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned buffer length",
            ));
        }
        let start = lba as usize * 2048;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "overflow"))?;
        if end > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "OOB"));
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

fn send_atapi_packet(io: &mut IoPortBus, base: u16, features: u8, pkt: &[u8; 12], byte_count: u16) {
    io.write(base + 1, 1, features as u32);
    io.write(base + 4, 1, (byte_count & 0xFF) as u32);
    io.write(base + 5, 1, (byte_count >> 8) as u32);
    io.write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        io.write(base, 2, w as u32);
    }
}

fn clear_atapi_unit_attention(io: &mut IoPortBus, base: u16) {
    // TEST UNIT READY.
    let tur = [0u8; 12];
    send_atapi_packet(io, base, 0, &tur, 0);
    let _ = io.read(base + 7, 1);

    // REQUEST SENSE (18 bytes).
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(io, base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = io.read(base, 2);
    }
}

#[test]
fn ide_atapi_pio_read10_snapshot_roundtrip_mid_data_phase() {
    let mut iso = MemIso::new(2);
    // Fill LBA 1 with a deterministic pattern.
    for i in 0..2048usize {
        iso.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }
    let expected = iso.data[2048..2048 + 2048].to_vec();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    clear_atapi_unit_attention(&mut ioports, SECONDARY_PORTS.cmd_base);

    // READ(10) for LBA=1, blocks=1.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &read10, 2048);

    // Read some data, then snapshot mid-transfer.
    let prefix_words = 128usize;
    let mut out = vec![0u8; 2048];
    for i in 0..prefix_words {
        let w = ioports.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    let pci_before = ide.borrow().config().snapshot_state();
    let snap = ide.borrow().save_state();

    let mut iso2 = MemIso::new(2);
    for i in 0..2048usize {
        iso2.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }

    let restored = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    restored
        .borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso2),
        )));
    restored.borrow_mut().load_state(&snap).unwrap();
    assert_eq!(restored.borrow().config().snapshot_state(), pci_before);

    let mut ioports2 = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports2, restored.clone());

    for i in prefix_words..(2048 / 2) {
        let w = ioports2.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(out, expected);
}

#[test]
fn ide_ata_dma_snapshot_roundtrip_preserves_irq_and_status_bits() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let expected: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    disk.write_sectors(0, &expected).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);

    // PRD table at 0x1000, one 512-byte segment to 0x2000.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x2000u64;
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 512);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction=read) and complete DMA.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    // Snapshot when DMA is idle but the interrupt is still pending.
    assert!(ide.borrow().controller.primary_irq_pending());
    let pci_before = ide.borrow().config().snapshot_state();
    let snap = ide.borrow().save_state();

    // Restore into a fresh controller with the same disk contents.
    let mut disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    disk2.write_sectors(0, &expected).unwrap();

    let restored = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    restored
        .borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk2)).unwrap());
    restored.borrow_mut().load_state(&snap).unwrap();
    assert_eq!(restored.borrow().config().snapshot_state(), pci_before);

    let mut ioports2 = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports2, restored.clone());

    // Interrupt line + Bus Master status should still reflect completion.
    assert!(restored.borrow().controller.primary_irq_pending());
    let bm_base = restored.borrow().bus_master_base();
    let bm_status = ioports2.read(bm_base + 2, 1) as u8;
    assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
    assert_eq!(
        bm_status & 0x01,
        0,
        "BMIDE status active bit should be clear"
    );

    // ATA status should report DRDY and not be busy/DRQ.
    let st = ioports2.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA completion");
    assert_eq!(
        st & 0x88,
        0,
        "BSY and DRQ should be clear after DMA completion"
    );

    // Reading STATUS clears the pending IRQ.
    assert!(!restored.borrow().controller.primary_irq_pending());
}
