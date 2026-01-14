use aero_devices_storage::atapi::{AtapiCdrom, PacketResult};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};

fn test_unit_ready_packet() -> [u8; 12] {
    [0x00; 12]
}

fn request_sense_packet(alloc_len: u8) -> [u8; 12] {
    let mut pkt = [0u8; 12];
    pkt[0] = 0x03;
    pkt[4] = alloc_len;
    pkt
}

fn read_cd_packet(lba: u32, blocks: u32) -> [u8; 12] {
    let mut pkt = [0u8; 12];
    pkt[0] = 0xBE;
    // Expected sector type: Mode 1 (2048-byte user data).
    pkt[1] = 0x02;
    pkt[2..6].copy_from_slice(&lba.to_be_bytes());
    pkt[6] = ((blocks >> 16) & 0xFF) as u8;
    pkt[7] = ((blocks >> 8) & 0xFF) as u8;
    pkt[8] = (blocks & 0xFF) as u8;
    // Sector type bitmask: user data only.
    pkt[9] = 0x10;
    // No subchannel.
    pkt[10] = 0x00;
    pkt
}

#[test]
fn atapi_read_cd_reads_user_data_sectors() {
    let mut disk = RawDisk::create(MemBackend::new(), 2 * AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    disk.write_at(AtapiCdrom::SECTOR_SIZE as u64, b"HELLO")
        .unwrap();

    let mut cdrom = AtapiCdrom::new_from_virtual_disk(Box::new(disk)).unwrap();

    // Clear the initial "media changed" unit attention.
    let _ = cdrom.handle_packet(&test_unit_ready_packet(), false);
    assert!(matches!(
        cdrom.handle_packet(&test_unit_ready_packet(), false),
        PacketResult::NoDataSuccess
    ));

    match cdrom.handle_packet(&read_cd_packet(1, 1), false) {
        PacketResult::DataIn(buf) => {
            assert_eq!(buf.len(), AtapiCdrom::SECTOR_SIZE);
            assert_eq!(&buf[..5], b"HELLO");
        }
        other => panic!("unexpected READ CD result: {other:?}"),
    }
}

#[test]
fn atapi_read_cd_rejects_subchannel_requests() {
    let disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    let mut cdrom = AtapiCdrom::new_from_virtual_disk(Box::new(disk)).unwrap();

    // Clear the initial "media changed" unit attention.
    let _ = cdrom.handle_packet(&test_unit_ready_packet(), false);
    let _ = cdrom.handle_packet(&test_unit_ready_packet(), false);

    let mut pkt = read_cd_packet(0, 1);
    pkt[10] = 0x01; // request some subchannel bits

    match cdrom.handle_packet(&pkt, false) {
        PacketResult::Error { sense_key, asc, .. } => {
            assert_eq!(sense_key, 0x05);
            assert_eq!(asc, 0x24);
        }
        other => panic!("expected READ CD error, got: {other:?}"),
    }

    // Ensure the sense data is latched for REQUEST SENSE.
    match cdrom.handle_packet(&request_sense_packet(18), false) {
        PacketResult::DataIn(buf) => {
            assert_eq!(buf[2] & 0x0F, 0x05);
            assert_eq!(buf[12], 0x24);
        }
        other => panic!("unexpected REQUEST SENSE result: {other:?}"),
    }
}
