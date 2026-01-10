use firmware::bus::TestBus;
use firmware::devices::Devices;
use firmware::legacy_bios::{BiosConfig, Disk, LegacyBios};

pub const DEFAULT_RAM_SIZE: usize = 64 * 1024 * 1024;
pub const DEFAULT_ACPI_BASE: u32 = 0x000E_0000;
pub const DEFAULT_HPET_BASE: u64 = 0xFED0_0000;

pub struct TestMachine {
    pub bus: TestBus,
    pub bios: LegacyBios,
}

impl TestMachine {
    pub fn new() -> Self {
        let devices = Devices::new(DEFAULT_HPET_BASE);
        let mut bus = TestBus::new(DEFAULT_RAM_SIZE, devices);
        let bios = LegacyBios::new(BiosConfig {
            ram_size: DEFAULT_RAM_SIZE as u64,
            acpi_base: DEFAULT_ACPI_BASE,
            hpet_base: DEFAULT_HPET_BASE,
        });
        bios.post(&mut bus);
        Self { bus, bios }
    }

    pub fn with_disk(mut self, data: Vec<u8>) -> Self {
        self.bios.disk0 = Disk::from_bytes(data);
        self
    }
}
