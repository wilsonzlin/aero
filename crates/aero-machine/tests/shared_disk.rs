#![cfg(not(target_arch = "wasm32"))]

use aero_machine::SharedDisk;
use aero_storage::{VirtualDisk, SECTOR_SIZE};
use firmware::bios::BlockDevice as _;

#[test]
fn shared_disk_virtualdisk_writes_are_visible_to_bios_blockdevice_reads() {
    let disk = SharedDisk::from_bytes(vec![0u8; 4 * SECTOR_SIZE]).unwrap();

    // Write a recognizable pattern via the `aero_storage::VirtualDisk` path.
    let mut writer = disk.clone();
    let mut pattern = vec![0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    writer.write_sectors(2, &pattern).unwrap();

    // Read it back via the BIOS INT13 `firmware::bios::BlockDevice` path.
    let mut bios = disk.clone();
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    bios.read_sector(2, &mut sector).unwrap();
    assert_eq!(&sector[..], &pattern[..]);
}
