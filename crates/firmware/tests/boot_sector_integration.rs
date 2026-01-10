mod common;

use firmware::bus::Bus;
use firmware::vm::RealModeVm;

use common::TestMachine;

#[test]
fn boot_sector_exercises_int10_13_15_16_and_reaches_signature() {
    // Two sectors: boot sector + data sector.
    let mut disk = vec![0u8; 2 * 512];
    disk[512] = 0x99;

    let mut m = TestMachine::new().with_disk(disk);
    m.bios.keyboard.push_key(b'Z', 0x2C);
    m.bus.clear_serial();

    let boot: &[u8] = include_bytes!("../../../test_images/boot_sectors/int_sanity.bin");
    assert_eq!(boot.len(), 512, "boot sector must be exactly 512 bytes");
    assert_eq!(
        &boot[510..512],
        &[0x55, 0xAA],
        "boot sector missing 0x55AA signature"
    );

    let mut vm = RealModeVm::new(&mut m.bus, &mut m.bios);
    vm.load(0x7C00, boot);

    vm.run_until(10_000, |vm| vm.bus.read_u16(0x0530) == 0x4B4F)
        .unwrap();

    assert_eq!(m.bus.read_u16(0x0530), 0x4B4F, "missing OK signature");
    assert_eq!(m.bus.serial_output(), b"A", "INT10 output mismatch");
    assert_eq!(m.bus.read_u16(0x0510), 0x2C5A, "INT16 key mismatch");
    assert_eq!(m.bus.read_u8(0x0520), 0x99, "INT13 disk read mismatch");
}
