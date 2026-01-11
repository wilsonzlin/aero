use firmware::bios::{build_bios_rom, Bios, BiosConfig, BIOS_SIZE, EBDA_BASE};
use machine::{CpuState, InMemoryDisk, MemoryAccess, PhysicalMemory};

fn boot_sector(pattern: u8) -> [u8; 512] {
    let mut sector = [pattern; 512];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

fn read_bytes(mem: &PhysicalMemory, paddr: u64, len: usize) -> Vec<u8> {
    mem.read_bytes(paddr, len)
}

fn scan_region_for_smbios(mem: &PhysicalMemory, base: u64, len: u64) -> Option<u64> {
    for off in (0..len).step_by(16) {
        let addr = base + off;
        if mem.read_u8(addr) == b'_'
            && mem.read_u8(addr + 1) == b'S'
            && mem.read_u8(addr + 2) == b'M'
            && mem.read_u8(addr + 3) == b'_'
        {
            return Some(addr);
        }
    }
    None
}

fn find_smbios_eps(mem: &PhysicalMemory) -> Option<u64> {
    // SMBIOS spec: search the first KiB of EBDA first, then scan 0xF0000-0xFFFFF
    // on 16-byte boundaries.
    let ebda_seg = mem.read_u16(0x040E);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;
        if let Some(addr) = scan_region_for_smbios(mem, ebda_base, 1024) {
            return Some(addr);
        }
    }
    scan_region_for_smbios(mem, 0xF0000, 0x10000)
}

#[derive(Debug)]
struct ParsedStructure {
    ty: u8,
}

fn parse_smbios_table(table: &[u8]) -> Vec<ParsedStructure> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < table.len() {
        let ty = table[i];
        let len = table[i + 1] as usize;
        let mut j = i + len;

        // Skip strings.
        loop {
            if j + 1 >= table.len() {
                panic!("unterminated string-set");
            }
            if table[j] == 0 && table[j + 1] == 0 {
                j += 2;
                break;
            }
            j += 1;
        }

        out.push(ParsedStructure { ty });
        i = j;
        if ty == 127 {
            break;
        }
    }
    out
}

#[test]
fn bios_post_loads_boot_sector_and_publishes_acpi_and_smbios() {
    let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0xAA));
    let mut mem = PhysicalMemory::new(16 * 1024 * 1024);
    let mut cpu = CpuState::default();

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        ..BiosConfig::default()
    });

    bios.post(&mut cpu, &mut mem, &mut disk);

    // BIOS must have loaded the boot sector.
    let loaded = read_bytes(&mem, 0x7C00, 512);
    assert_eq!(loaded[..510], vec![0xAA; 510]);
    assert_eq!(loaded[510], 0x55);
    assert_eq!(loaded[511], 0xAA);

    // ACPI RSDP should be written during POST when enabled.
    let rsdp_addr = bios.rsdp_addr().expect("RSDP should be built");
    assert_eq!(rsdp_addr, EBDA_BASE + 0x100);
    let rsdp = read_bytes(&mem, rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));

    // SMBIOS EPS should be discoverable by spec search rules.
    let eps_addr = find_smbios_eps(&mem).expect("SMBIOS EPS not found after BIOS POST");
    assert!(eps_addr >= EBDA_BASE && eps_addr < EBDA_BASE + 1024);

    let eps = read_bytes(&mem, eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(checksum_ok(&eps));
    assert_eq!(&eps[0x10..0x15], b"_DMI_");
    assert!(checksum_ok(&eps[0x10..]));

    let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
    let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]) as u64;
    let table = read_bytes(&mem, table_addr, table_len);
    let structures = parse_smbios_table(&table);

    assert!(structures.iter().any(|s| s.ty == 0), "missing Type 0");
    assert!(structures.iter().any(|s| s.ty == 1), "missing Type 1");
    assert!(structures.iter().any(|s| s.ty == 4), "missing Type 4");
    assert!(structures.iter().any(|s| s.ty == 16), "missing Type 16");
    assert!(structures.iter().any(|s| s.ty == 17), "missing Type 17");
    assert!(structures.iter().any(|s| s.ty == 19), "missing Type 19");
    assert!(structures.iter().any(|s| s.ty == 127), "missing Type 127");
}

#[test]
fn bios_rom_contains_a_valid_reset_vector_jump() {
    let rom = build_bios_rom();
    assert_eq!(rom.len(), BIOS_SIZE);
    assert_eq!(&rom[0xFFF0..0xFFF5], &[0xEA, 0x00, 0xE0, 0x00, 0xF0]);
    assert_eq!(&rom[0xFFFE..0x10000], &[0x55, 0xAA]);
}
