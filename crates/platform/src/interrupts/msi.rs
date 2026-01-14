use super::router::PlatformInterrupts;
use aero_interrupts::apic::LocalApic;
use std::cell::RefCell;
use std::rc::Rc;

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

        // xAPIC "physical destination" decoding.
        // - Destination ID 0xFF broadcasts to all LAPICs.
        // - Otherwise, deliver to the LAPIC whose physical APIC ID matches `dest`.
        // - If no LAPIC matches, drop the MSI.
        if dest == 0xFF {
            self.inject_fixed_broadcast(vector);
        } else {
            self.inject_fixed_for_apic(dest, vector);
        }
    }
}

// Allow shared `PlatformInterrupts` handles (`Rc<RefCell<...>>`) to be used as MSI sinks by device
// models without requiring platform-specific wrapper types.
impl MsiTrigger for Rc<RefCell<PlatformInterrupts>> {
    fn trigger_msi(&mut self, message: MsiMessage) {
        self.borrow_mut().trigger_msi(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupts::PlatformInterruptMode;

    fn enable_lapic_svr_for_apic(ints: &PlatformInterrupts, apic_id: u8) {
        // The LAPIC model drops injected interrupts while the software enable bit is cleared.
        // Keep this explicit in tests so behaviour doesn't depend on constructor defaults.
        ints.lapic_mmio_write_for_apic(apic_id, 0xF0, &(0x1FFu32).to_le_bytes());
    }

    fn msi_message(dest_id: u8, vector: u8) -> MsiMessage {
        MsiMessage {
            // xAPIC physical destination mode encodes the APIC ID in bits 12..19.
            address: 0xFEE0_0000u64 | ((dest_id as u64) << 12),
            data: vector as u16,
        }
    }

    #[test]
    fn apic_system_delivers_msi_to_lapic0() {
        let mut sys = ApicSystem::new_single_cpu();
        sys.trigger_msi(MsiMessage {
            address: 0xFEE0_0000,
            data: 0x0044,
        });
        assert_eq!(sys.lapic0().get_pending_vector(), Some(0x44));
    }

    // NOTE: `PlatformInterrupts` implements xAPIC "physical destination" MSI decoding.
    // In particular, MSI delivery must correctly target non-BSP LAPICs in SMP guests.

    #[test]
    fn platform_interrupts_msi_delivers_when_destination_matches_bsp() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr_for_apic(&ints, 0);

        assert_eq!(ints.get_pending_for_apic(0), None);

        let bsp_id = ints.lapic_apic_id();
        ints.trigger_msi(msi_message(bsp_id, 0x44));

        assert_eq!(ints.get_pending_for_apic(bsp_id), Some(0x44));
        assert_eq!(ints.get_pending_for_apic(0), Some(0x44));
    }

    #[test]
    fn platform_interrupts_msi_delivers_broadcast_to_bsp() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr_for_apic(&ints, 0);

        assert_eq!(ints.get_pending_for_apic(0), None);

        ints.trigger_msi(msi_message(0xFF, 0x45));

        assert_eq!(ints.get_pending_for_apic(0), Some(0x45));
    }

    #[test]
    fn platform_interrupts_msi_drops_unmatched_physical_destination() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr_for_apic(&ints, 0);

        assert_eq!(ints.get_pending_for_apic(0), None);

        let bsp_id = ints.lapic_apic_id();
        let other_dest = if bsp_id == 0 { 1 } else { 0 };
        assert_ne!(other_dest, bsp_id);
        assert_ne!(other_dest, 0xFF);

        ints.trigger_msi(msi_message(other_dest, 0x46));

        assert_eq!(ints.get_pending_for_apic(0), None);
    }

    #[test]
    fn platform_interrupts_msi_delivers_to_matching_apic_id() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr_for_apic(&ints, 0);
        enable_lapic_svr_for_apic(&ints, 1);

        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), None);

        ints.trigger_msi(msi_message(1, 0x44));

        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), Some(0x44));
    }

    #[test]
    fn platform_interrupts_msi_broadcast_delivers_to_all_lapics() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr_for_apic(&ints, 0);
        enable_lapic_svr_for_apic(&ints, 1);

        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), None);

        ints.trigger_msi(msi_message(0xFF, 0x44));

        assert_eq!(ints.get_pending_for_apic(0), Some(0x44));
        assert_eq!(ints.get_pending_for_apic(1), Some(0x44));
    }
}
