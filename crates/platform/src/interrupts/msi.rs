use super::router::PlatformInterrupts;
use aero_interrupts::apic::LocalApic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsiMessage {
    pub address: u64,
    pub data: u16,
}

impl MsiMessage {
    pub fn vector(self) -> u8 {
        (self.data & 0x00ff) as u8
    }

    pub fn destination_id(self) -> u8 {
        ((self.address >> 12) & 0xff) as u8
    }
}

pub trait MsiTrigger {
    fn trigger_msi(&mut self, message: MsiMessage);
}

pub struct ApicSystem {
    lapics: Vec<LocalApic>,
}

impl std::fmt::Debug for ApicSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApicSystem")
            .field("lapics", &self.lapics.len())
            .finish_non_exhaustive()
    }
}

impl ApicSystem {
    pub fn new_single_cpu() -> Self {
        // The LAPIC resets with SVR[8]=0 (software disabled). Our device model drops injected
        // interrupts when the LAPIC is disabled, so enable it here to make `ApicSystem` usable
        // as a simple MSI sink in tests.
        let lapic = LocalApic::new(0);
        lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
        Self {
            lapics: vec![lapic],
        }
    }

    pub fn lapic0(&self) -> &LocalApic {
        &self.lapics[0]
    }

    pub fn lapic0_mut(&mut self) -> &mut LocalApic {
        &mut self.lapics[0]
    }
}

impl MsiTrigger for ApicSystem {
    fn trigger_msi(&mut self, message: MsiMessage) {
        let vector = message.vector();
        self.lapic0_mut().inject_fixed_interrupt(vector);
    }
}

impl MsiTrigger for PlatformInterrupts {
    fn trigger_msi(&mut self, message: MsiMessage) {
        let vector = message.vector();
        let dest = message.destination_id();

        // xAPIC "physical destination" decoding (single-CPU for now).
        // - If the destination matches the bootstrap processor's APIC ID, inject.
        // - Treat destination ID 0xFF as a broadcast (single-CPU => BSP).
        //
        // Multi-CPU destination decoding can be layered on later.
        if dest == self.lapic_apic_id() || dest == 0xFF {
            self.lapic_inject_fixed(vector);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apic_system_delivers_msi_to_lapic0() {
        let mut sys = ApicSystem::new_single_cpu();
        sys.trigger_msi(MsiMessage {
            address: 0xFEE0_0000,
            data: 0x0044,
        });
        assert_eq!(sys.lapic0().get_pending_vector(), Some(0x44));
    }
}
