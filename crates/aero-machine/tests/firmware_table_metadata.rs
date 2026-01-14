use aero_machine::{Machine, MachineConfig};

fn boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn exposes_acpi_rsdp_and_smbios_eps_addresses() {
    // ACPI table publication is only enabled when the PC platform is wired in.
    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_acpi: true,
        ..Default::default()
    })
    .unwrap();
    m.set_disk_image(boot_sector().to_vec()).unwrap();
    m.reset();

    let rsdp_addr = m
        .acpi_rsdp_addr()
        .expect("expected an RSDP address after firmware POST with ACPI enabled");
    let rsdp_sig = m.read_physical_bytes(rsdp_addr, 8);
    assert_eq!(&rsdp_sig[..], b"RSD PTR ");

    let eps_addr = m
        .smbios_eps_addr()
        .expect("expected SMBIOS EPS address after firmware POST") as u64;
    let eps_sig = m.read_physical_bytes(eps_addr, 4);
    assert_eq!(&eps_sig[..], b"_SM_");
}
