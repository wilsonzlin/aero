use std::sync::Arc;

use firmware::bios::{
    build_bios_rom, Bios, BiosConfig, FirmwareMemory, InMemoryDisk, BIOS_SIZE, EBDA_BASE,
};
use firmware::smbios::{
    find_eps, parse_eps_table_info, parse_structure_types, validate_eps_checksum,
};
use memory::{DenseMemory, MapError, PhysicalMemoryBus};

struct TestMemory {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

impl TestMemory {
    fn new(size: u64) -> Self {
        let ram = DenseMemory::new(size).expect("guest RAM allocation failed");
        Self {
            a20_enabled: false,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl firmware::bios::A20Gate for TestMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for TestMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                let already_mapped = self
                    .inner
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == base && r.data.len() == len);
                if !already_mapped {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})");
            }
        }
    }
}

impl memory::MemoryBus for TestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20_enabled {
            self.inner.read_physical(paddr, buf);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20_enabled {
            self.inner.write_physical(paddr, buf);
            return;
        }

        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

fn boot_sector(pattern: u8) -> [u8; 512] {
    let mut sector = [pattern; 512];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

fn read_bytes(mem: &mut impl memory::MemoryBus, paddr: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    mem.read_physical(paddr, &mut out);
    out
}

#[test]
fn bios_post_loads_boot_sector_and_publishes_acpi_and_smbios() {
    let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0xAA));
    let mut mem = TestMemory::new(16 * 1024 * 1024);
    let mut cpu = Default::default();

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        ..BiosConfig::default()
    });

    bios.post(&mut cpu, &mut mem, &mut disk, None);

    // BIOS must have loaded the boot sector.
    let loaded = read_bytes(&mut mem, 0x7C00, 512);
    assert_eq!(loaded[..510], vec![0xAA; 510]);
    assert_eq!(loaded[510], 0x55);
    assert_eq!(loaded[511], 0xAA);

    // ACPI RSDP should be written during POST when enabled.
    let rsdp_addr = bios.rsdp_addr().expect("RSDP should be built");
    assert_eq!(rsdp_addr, EBDA_BASE + 0x100);
    let rsdp = read_bytes(&mut mem, rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));

    // SMBIOS EPS should be discoverable by spec search rules.
    let eps_addr = find_eps(&mut mem).expect("SMBIOS EPS not found after BIOS POST");
    assert!((EBDA_BASE..EBDA_BASE + 1024).contains(&eps_addr));

    let eps = read_bytes(&mut mem, eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(validate_eps_checksum(&eps));

    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");
    let table = read_bytes(&mut mem, table_info.table_addr, table_info.table_len);
    let types = parse_structure_types(&table);

    assert_eq!(types.last().copied(), Some(127), "missing Type 127");
    assert!(types.contains(&0), "missing Type 0");
    assert!(types.contains(&1), "missing Type 1");
    assert!(types.contains(&4), "missing Type 4");
    assert!(types.contains(&16), "missing Type 16");
    assert!(types.contains(&17), "missing Type 17");
    assert!(types.contains(&19), "missing Type 19");
}

#[test]
fn bios_rom_contains_a_valid_reset_vector_jump() {
    let rom = build_bios_rom();
    assert_eq!(rom.len(), BIOS_SIZE);
    assert_eq!(&rom[0xFFF0..0xFFF5], &[0xEA, 0x00, 0xE0, 0x00, 0xF0]);
    assert_eq!(&rom[0xFFFE..0x10000], &[0x55, 0xAA]);
}
