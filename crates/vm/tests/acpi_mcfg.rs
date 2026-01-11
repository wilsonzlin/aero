use firmware::bios::{
    Bios, BiosConfig, PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_START_BUS,
};
use machine::InMemoryDisk;
use vm::Vm;

fn boot_sector_with(bytes: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let len = bytes.len().min(510);
    sector[..len].copy_from_slice(&bytes[..len]);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn bios_publishes_mcfg_for_pcie_ecam() {
    const MEM_SIZE: usize = 16 * 1024 * 1024;

    let cfg = BiosConfig {
        memory_size_bytes: MEM_SIZE as u64,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(MEM_SIZE, bios, disk);
    vm.reset();

    let rsdp_addr = vm.bios.rsdp_addr().expect("RSDP should be present");
    let rsdp = vm.mem.read_bytes(rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");

    let xsdt_addr = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
    assert_ne!(xsdt_addr, 0, "XSDT address should be non-zero");

    let xsdt_len =
        u32::from_le_bytes(vm.mem.read_bytes(xsdt_addr + 4, 4).try_into().unwrap()) as usize;
    let xsdt = vm.mem.read_bytes(xsdt_addr, xsdt_len);
    assert_eq!(&xsdt[0..4], b"XSDT");

    let mut mcfg_addr = None;
    for chunk in xsdt[36..].chunks_exact(8) {
        let addr = u64::from_le_bytes(chunk.try_into().unwrap());
        let sig = vm.mem.read_bytes(addr, 4);
        if &sig == b"MCFG" {
            mcfg_addr = Some(addr);
            break;
        }
    }
    let mcfg_addr = mcfg_addr.expect("MCFG should be listed in XSDT");

    let mcfg_len =
        u32::from_le_bytes(vm.mem.read_bytes(mcfg_addr + 4, 4).try_into().unwrap()) as usize;
    let mcfg = vm.mem.read_bytes(mcfg_addr, mcfg_len);
    assert_eq!(&mcfg[0..4], b"MCFG");
    assert!(
        mcfg.len() >= 44 + 16,
        "MCFG should contain at least one allocation entry"
    );

    let base = u64::from_le_bytes(mcfg[44..52].try_into().unwrap());
    let segment = u16::from_le_bytes(mcfg[52..54].try_into().unwrap());
    let start_bus = mcfg[54];
    let end_bus = mcfg[55];

    assert_eq!(base, PCIE_ECAM_BASE);
    assert_eq!(segment, PCIE_ECAM_SEGMENT);
    assert_eq!(start_bus, PCIE_ECAM_START_BUS);
    assert_eq!(end_bus, PCIE_ECAM_END_BUS);
}
