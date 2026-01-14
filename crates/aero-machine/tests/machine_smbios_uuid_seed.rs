use aero_machine::{Machine, MachineConfig};
use firmware::smbios::{find_eps, parse_eps_table_info, parse_structures, validate_eps_checksum};
use pretty_assertions::assert_eq;

fn boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
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

    let eps_addr = find_eps(&mut m).expect("SMBIOS EPS not found after BIOS POST");
    let eps = m.read_physical_bytes(eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(validate_eps_checksum(&eps));

    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");
    let table = m.read_physical_bytes(table_info.table_addr, table_info.table_len);

    let structures = parse_structures(&table);
    let type1 = structures
        .iter()
        .find(|s| s.header.ty == 1)
        .expect("Type 1 missing from SMBIOS table");
    let uuid: [u8; 16] = type1.formatted[8..24].try_into().unwrap();
    let expected = expected_smbios_uuid(ram_size_bytes, 1, seed);
    assert_eq!(uuid, expected);
}
