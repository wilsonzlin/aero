use crate::bus::Bus;
use crate::realmode::RealModeCpu;

#[derive(Debug, Clone, Copy)]
pub struct BiosConfig {
    pub ram_size: u32,
    pub acpi_base: u32,
    pub hpet_base: u32,
}

/// Minimal INT 15h services used by legacy platform integration tests.
#[derive(Debug)]
pub struct LegacyBios {
    _config: BiosConfig,
}

impl LegacyBios {
    pub fn new(config: BiosConfig) -> Self {
        Self { _config: config }
    }

    pub fn handle_interrupt(&mut self, vector: u8, bus: &mut dyn Bus, cpu: &mut RealModeCpu) {
        match vector {
            0x15 => self.int15(bus, cpu),
            _ => {
                cpu.set_cf();
            }
        }
    }

    fn int15(&mut self, bus: &mut dyn Bus, cpu: &mut RealModeCpu) {
        match cpu.ax() {
            0x2400 => {
                // Disable A20.
                bus.set_a20_enabled(false);
                cpu.clear_cf();
            }
            0x2401 => {
                // Enable A20.
                bus.set_a20_enabled(true);
                cpu.clear_cf();
            }
            0x2402 => {
                // Query A20 status.
                cpu.set_al(if bus.a20_enabled() { 1 } else { 0 });
                cpu.clear_cf();
            }
            0x2403 => {
                // Query A20 support. Return all commonly available methods:
                // - KBC (bit0)
                // - Fast A20 (port 0x92) (bit1)
                // - BIOS INT 15h interface itself (bit2)
                cpu.set_bx(0x0007);
                cpu.clear_cf();
            }
            _ => {
                cpu.set_cf();
            }
        }
    }
}
