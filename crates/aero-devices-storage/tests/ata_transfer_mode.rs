use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::pci::PciDevice as _;
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::pci_ide::{register_piix3_ide_ports, Piix3IdePciDevice, PRIMARY_PORTS};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::IdeDriveState;
use aero_platform::io::IoPortBus;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};

fn identify_word(id: &[u8; SECTOR_SIZE], word: usize) -> u16 {
    let start = word * 2;
    u16::from_le_bytes([id[start], id[start + 1]])
}

#[test]
fn ide_set_features_set_transfer_mode_updates_snapshot_state() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // SET FEATURES / 0x03: set transfer mode to UDMA2 (0x40 | 2).
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x42);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // ack IRQ

    let snap = ide.borrow().snapshot_state();
    match &snap.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 2),
        other => panic!("expected ATA drive state, got {other:?}"),
    }

    // Now select UDMA0.
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x40);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // ack IRQ

    let snap2 = ide.borrow().snapshot_state();
    match &snap2.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 0),
        other => panic!("expected ATA drive state, got {other:?}"),
    }

    // Switch to Multiword DMA mode 2 (0x20 | 2).
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x22);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // ack IRQ

    let snap3 = ide.borrow().snapshot_state();
    match &snap3.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 0x80 | 2),
        other => panic!("expected ATA drive state, got {other:?}"),
    }

    // IDENTIFY DEVICE should reflect the negotiated mode bits (word63/word88) for guests that
    // re-identify after configuring transfer mode.
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
    let data_port = PRIMARY_PORTS.cmd_base + 0;
    let mut id = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 2) {
        let w = io.read(data_port, 2) as u16;
        id[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // clear IRQ

    let w63 = identify_word(&id, 63);
    let w88 = identify_word(&id, 88);
    assert_eq!(w63 & 0xFF00, 1 << (8 + 2), "MWDMA2 should be active");
    assert_eq!(w88 & 0xFF00, 0, "UDMA should be inactive when MWDMA selected");
}

#[test]
fn ide_set_features_set_transfer_mode_rejects_unsupported_mode() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Attempt to set an unsupported transfer mode: UDMA7 (0x40 | 7).
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x47);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);

    // Command should abort and raise an IRQ.
    assert!(ide.borrow().controller.primary_irq_pending());

    let status = io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    let error = io.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8;

    assert_ne!(status & 0x01, 0, "ERR should be set on abort");
    assert_eq!(status & 0x88, 0, "BSY+DRQ should be clear on abort");
    assert_ne!(status & 0x40, 0, "DRDY should be set on abort");
    assert_eq!(error, 0x04, "ABRT should be reported in the error register");

    // The device state should not change.
    let snap = ide.borrow().snapshot_state();
    match &snap.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 2),
        other => panic!("expected ATA drive state, got {other:?}"),
    }
}

#[test]
fn ata_drive_snapshot_roundtrip_preserves_transfer_mode() {
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut drive = AtaDrive::new(Box::new(disk)).unwrap();

    // Switch away from the default (UDMA2) to ensure we actually track the value.
    drive.set_transfer_mode_select(0x40).unwrap(); // UDMA0

    let snap = drive.save_state();

    let disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut restored = AtaDrive::new(Box::new(disk2)).unwrap();
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.snapshot_state().udma_mode, 0);
}

#[test]
fn ata_drive_snapshot_roundtrip_preserves_mwdma_transfer_mode() {
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut drive = AtaDrive::new(Box::new(disk)).unwrap();

    // Switch away from the default (UDMA2) to ensure we track the MWDMA selection.
    drive.set_transfer_mode_select(0x22).unwrap(); // MWDMA2

    let snap = drive.save_state();

    let disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut restored = AtaDrive::new(Box::new(disk2)).unwrap();
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.snapshot_state().udma_mode, 0x80 | 2);
}

#[test]
fn ide_snapshot_roundtrip_preserves_transfer_mode_after_reattach() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Set UDMA0.
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x40);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // ack IRQ

    let snap = ide.borrow().save_state();

    // Restore into a new controller instance (backends are dropped on restore).
    let mut restored = Piix3IdePciDevice::new();
    restored.load_state(&snap).unwrap();

    // Reattach an identical disk backend; the controller should restore the negotiated mode onto
    // the newly-created `AtaDrive`.
    let disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    restored
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk2)).unwrap());

    let state = restored.snapshot_state();
    match &state.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 0),
        other => panic!("expected ATA drive state, got {other:?}"),
    }
}

#[test]
fn ide_snapshot_roundtrip_preserves_mwdma_transfer_mode_after_reattach() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Set MWDMA2 (0x20 | 2).
    io.write(PRIMARY_PORTS.cmd_base + 1, 1, 0x03);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x22);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEF);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1); // ack IRQ

    let snap = ide.borrow().save_state();

    // Restore into a new controller instance (backends are dropped on restore).
    let mut restored = Piix3IdePciDevice::new();
    restored.load_state(&snap).unwrap();

    // Reattach an identical disk backend; the controller should restore the negotiated mode onto
    // the newly-created `AtaDrive`.
    let disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    restored
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk2)).unwrap());

    let state = restored.snapshot_state();
    match &state.primary.drives[0] {
        IdeDriveState::Ata(s) => assert_eq!(s.udma_mode, 0x80 | 2),
        other => panic!("expected ATA drive state, got {other:?}"),
    }
}

#[test]
fn ata_identify_word88_reflects_negotiated_udma_mode() {
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut drive = AtaDrive::new(Box::new(disk)).unwrap();

    // Default is UDMA2 enabled.
    let id = drive.identify_sector();
    let w53 = identify_word(id, 53);
    assert_ne!(w53 & (1 << 2), 0, "word 53 bit 2 should indicate word 88 is valid");
    let w88 = identify_word(id, 88);
    // Advertise at least UDMA2 support.
    assert_ne!(w88 & (1 << 2), 0, "UDMA2 support bit should be set");
    // Active/selected bits should match the negotiated mode.
    assert_eq!(w88 & 0xFF00, 1 << (8 + 2));

    // Switch to UDMA0 and ensure word 88 updates.
    drive.set_transfer_mode_select(0x40).unwrap(); // UDMA0
    let id2 = drive.identify_sector();
    let w88_2 = identify_word(id2, 88);
    assert_eq!(w88_2 & 0xFF00, 1 << (8 + 0));
}

#[test]
fn ata_identify_word63_reflects_negotiated_mwdma_mode() {
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut drive = AtaDrive::new(Box::new(disk)).unwrap();

    // Switch to Multiword DMA mode 2 (0x20 | 2).
    drive.set_transfer_mode_select(0x22).unwrap();

    // Snapshotted UDMA mode encodes MWDMA selections by setting the high bit.
    assert_eq!(drive.snapshot_state().udma_mode, 0x80 | 2);

    let id = drive.identify_sector();
    let w63 = identify_word(id, 63);

    // Advertise mode 2 support and mark it active.
    assert_ne!(w63 & (1 << 2), 0, "MWDMA2 support bit should be set");
    assert_eq!(w63 & 0xFF00, 1 << (8 + 2));

    // UDMA word should have no active bits when MWDMA is selected.
    let w88 = identify_word(id, 88);
    assert_eq!(w88 & 0xFF00, 0);
}
