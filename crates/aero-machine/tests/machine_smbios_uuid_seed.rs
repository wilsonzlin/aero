use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
}

fn scan_region_for_smbios(m: &mut Machine, base: u64, len: u64) -> Option<u64> {
    for off in (0..len).step_by(16) {
        let addr = base + off;
        if m.read_physical_u8(addr) == b'_'
            && m.read_physical_u8(addr + 1) == b'S'
            && m.read_physical_u8(addr + 2) == b'M'
            && m.read_physical_u8(addr + 3) == b'_'
        {
            return Some(addr);
        }
    }
    None
}

fn find_smbios_eps(m: &mut Machine) -> Option<u64> {
    // SMBIOS spec: search the first KiB of EBDA first, then scan 0xF0000-0xFFFFF on 16-byte
    // boundaries.
    let ebda_seg = m.read_physical_u16(0x040E);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;
        if let Some(addr) = scan_region_for_smbios(m, ebda_base, 1024) {
            return Some(addr);
        }
    }
    scan_region_for_smbios(m, 0xF0000, 0x10000)
}

fn find_type1_uuid(table: &[u8]) -> Option<[u8; 16]> {
    let mut i = 0usize;
    while i < table.len() {
        let ty = table.get(i).copied()?;
        let len = table.get(i + 1).copied()? as usize;
        let formatted = table.get(i..i + len)?;

        if ty == 1 {
            // SMBIOS Type 1 UUID lives at offset 8 and is 16 bytes long.
            return formatted.get(8..24)?.try_into().ok();
        }

        // Skip strings.
        let mut j = i + len;
        loop {
            if j + 1 >= table.len() {
                return None;
            }
            if table[j] == 0 && table[j + 1] == 0 {
                j += 2;
                break;
            }
            j += 1;
        }

        i = j;
        if ty == 127 {
            break;
        }
    }
    None
}

fn fnv1a_128(bytes: &[u8]) -> u128 {
    const OFFSET_BASIS: u128 = 0x6c62272e07bb014262b821756295c58d;
    const FNV_PRIME: u128 = 0x0000000001000000000000000000013B;

    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u128;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn expected_smbios_uuid(ram_bytes: u64, cpu_count: u8, uuid_seed: u64) -> [u8; 16] {
    let mut data = Vec::new();
    data.extend_from_slice(b"AeroSMBIOS");
    data.extend_from_slice(&ram_bytes.to_le_bytes());
    data.push(cpu_count);
    data.extend_from_slice(&uuid_seed.to_le_bytes());

    let mut uuid = fnv1a_128(&data).to_be_bytes();
    // RFC 4122 variant + version bits.
    uuid[6] = (uuid[6] & 0x0F) | 0x40;
    uuid[8] = (uuid[8] & 0x3F) | 0x80;

    // SMBIOS stores the first three UUID fields little-endian.
    uuid[0..4].reverse();
    uuid[4..6].reverse();
    uuid[6..8].reverse();

    uuid
}

#[test]
fn smbios_system_uuid_uses_machine_config_seed() {
    let ram_size_bytes = 16 * 1024 * 1024;
    let seed = 0x0123_4567_89ab_cdefu64;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes,
        smbios_uuid_seed: seed,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot_sector().to_vec()).unwrap();
    m.reset();

    let eps_addr = find_smbios_eps(&mut m).expect("SMBIOS EPS not found after BIOS POST");
    let eps = m.read_physical_bytes(eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(checksum_ok(&eps));
    assert_eq!(&eps[0x10..0x15], b"_DMI_");
    assert!(checksum_ok(&eps[0x10..]));

    let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
    let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]) as u64;
    let table = m.read_physical_bytes(table_addr, table_len);

    let uuid = find_type1_uuid(&table).expect("Type 1 UUID missing from SMBIOS table");
    let expected = expected_smbios_uuid(ram_size_bytes, 1, seed);
    assert_eq!(uuid, expected);
}
