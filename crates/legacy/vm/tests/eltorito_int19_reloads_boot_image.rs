use firmware::bios::{Bios, BiosConfig, InMemoryDisk};
use vm::{CpuExit, Vm};

fn build_eltorito_no_emulation_iso(boot_image_2048: &[u8; 2048]) -> Vec<u8> {
    const SECTOR: usize = 2048;
    const PVD_LBA: usize = 16;
    const BOOT_RECORD_LBA: usize = 17;
    const TERMINATOR_LBA: usize = 18;
    const BOOT_CATALOG_LBA: u32 = 20;
    const BOOT_IMAGE_LBA: u32 = 21;

    let total_sectors = 24usize;
    let mut iso = vec![0u8; total_sectors * SECTOR];

    // Primary Volume Descriptor (minimal; enough for the BIOS to recognize ISO9660).
    {
        let off = PVD_LBA * SECTOR;
        iso[off] = 1; // type 1: primary volume descriptor
        iso[off + 1..off + 6].copy_from_slice(b"CD001");
        iso[off + 6] = 1; // version
    }

    // El Torito Boot Record Volume Descriptor.
    {
        let off = BOOT_RECORD_LBA * SECTOR;
        iso[off] = 0; // type 0: boot record
        iso[off + 1..off + 6].copy_from_slice(b"CD001");
        iso[off + 6] = 1; // version

        // The El Torito boot system ID field is space-padded to 32 bytes. The firmware parser
        // matches the full padded field, so fill the remainder with spaces.
        let id = b"EL TORITO SPECIFICATION";
        iso[off + 7..off + 7 + 32].fill(b' ');
        iso[off + 7..off + 7 + id.len()].copy_from_slice(id);

        iso[off + 71..off + 75].copy_from_slice(&BOOT_CATALOG_LBA.to_le_bytes());
    }

    // Volume Descriptor Set Terminator.
    {
        let off = TERMINATOR_LBA * SECTOR;
        iso[off] = 255;
        iso[off + 1..off + 6].copy_from_slice(b"CD001");
        iso[off + 6] = 1;
    }

    // Boot Catalog.
    {
        let mut catalog = [0u8; 2048];

        // Validation entry.
        let mut validation = [0u8; 32];
        validation[0] = 0x01; // header id
        validation[1] = 0x00; // platform id: x86
        validation[30] = 0x55;
        validation[31] = 0xAA;
        // checksum word so that the sum of all 16-bit words in the entry is 0.
        let mut sum: u16 = 0;
        for i in 0..16usize {
            sum = sum.wrapping_add(u16::from_le_bytes([validation[i * 2], validation[i * 2 + 1]]));
        }
        let checksum = 0u16.wrapping_sub(sum);
        validation[28..30].copy_from_slice(&checksum.to_le_bytes());
        catalog[0..32].copy_from_slice(&validation);

        // Initial/Default entry: no emulation, load at 0x07C0:0000, 4x512 bytes (2048 bytes).
        let mut initial = [0u8; 32];
        initial[0] = 0x88; // bootable
        initial[1] = 0x00; // no emulation
        initial[2..4].copy_from_slice(&0x07C0u16.to_le_bytes()); // load segment
        initial[6..8].copy_from_slice(&4u16.to_le_bytes()); // sector count (512-byte units)
        initial[8..12].copy_from_slice(&BOOT_IMAGE_LBA.to_le_bytes()); // boot image LBA (2048-byte units)
        catalog[32..64].copy_from_slice(&initial);

        let off = (BOOT_CATALOG_LBA as usize) * SECTOR;
        iso[off..off + 2048].copy_from_slice(&catalog);
    }

    // Boot image.
    {
        let off = (BOOT_IMAGE_LBA as usize) * SECTOR;
        iso[off..off + 2048].copy_from_slice(boot_image_2048);
    }

    iso
}

fn build_boot_image_calls_int19_and_halts() -> [u8; 2048] {
    let mut code: Vec<u8> = Vec::new();

    // xor ax, ax
    code.extend_from_slice(&[0x31, 0xC0]);
    // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD8]);

    // mov al, [0x0500]
    code.extend_from_slice(&[0xA0, 0x00, 0x05]);
    // cmp al, 0
    code.extend_from_slice(&[0x3C, 0x00]);
    // jne second (patch later)
    code.extend_from_slice(&[0x75, 0x00]);
    let jne_disp_off = code.len() - 1;

    // First boot:
    // mov byte [0x0500], 1
    code.extend_from_slice(&[0xC6, 0x06, 0x00, 0x05, 0x01]);
    // mov ax, 0x0E41 ('A')
    code.extend_from_slice(&[0xB8, b'A', 0x0E]);
    // int 0x10
    code.extend_from_slice(&[0xCD, 0x10]);
    // int 0x19 (reboot / bootstrap)
    code.extend_from_slice(&[0xCD, 0x19]);
    // hlt (should never reach if INT 19 is implemented correctly)
    code.push(0xF4);

    let second_label = code.len();

    // Second boot:
    // mov ax, 0x0E42 ('B')
    code.extend_from_slice(&[0xB8, b'B', 0x0E]);
    // int 0x10
    code.extend_from_slice(&[0xCD, 0x10]);
    // hlt
    code.push(0xF4);

    let next_ip = jne_disp_off + 1;
    let disp = (second_label as isize - next_ip as isize) as i8;
    code[jne_disp_off] = disp as u8;

    assert!(code.len() <= 2048, "boot image too large: {} bytes", code.len());

    let mut img = [0u8; 2048];
    img[..code.len()].copy_from_slice(&code);
    img
}

#[test]
fn int19_reloads_eltorito_boot_image() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0, // CD-ROM boot drive
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);

    let boot_img = build_boot_image_calls_int19_and_halts();
    let iso_bytes = build_eltorito_no_emulation_iso(&boot_img);
    let disk = InMemoryDisk::new(iso_bytes);

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Sanity check: El Torito default load segment 0x07C0.
    assert_eq!(vm.cpu.segments.cs.selector, 0x07C0);
    assert_eq!(vm.cpu.rip(), 0x0000);

    let mut saw_int19 = false;
    let mut halted = false;
    for _ in 0..10_000 {
        match vm.step() {
            CpuExit::BiosInterrupt(0x19) => saw_int19 = true,
            CpuExit::Halt => {
                halted = true;
                break;
            }
            _ => {}
        }
    }

    assert!(halted, "VM did not halt (likely failed to return to boot image)");
    assert!(saw_int19, "boot image never invoked INT 19h");
    assert_eq!(vm.bios.tty_output(), b"AB");
}
