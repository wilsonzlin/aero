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

    /// Returns the MSI delivery mode field (data bits 8..10).
    pub fn delivery_mode(self) -> u8 {
        ((self.data >> 8) & 0x7) as u8
    }

    pub fn destination_id(self) -> u8 {
        ((self.address >> 12) & 0xff) as u8
    }

    /// Returns `true` if the MSI address selects xAPIC logical destination mode (address bit 2).
    pub fn destination_is_logical(self) -> bool {
        ((self.address >> 2) & 0x1) != 0
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
        let dest_is_logical = message.destination_is_logical();
        let delivery_mode = message.delivery_mode();

        // MSI encodes the same delivery mode field used by the APIC ICR.
        //
        // We currently only model "fixed" interrupt injection through `LocalApic::inject_fixed_interrupt`.
        // For forward compatibility with devices/guests that program other modes (e.g. Lowest Priority),
        // treat any non-fixed mode as Fixed for now.
        let _ = delivery_mode;

        if dest_is_logical {
            // Minimal xAPIC "logical destination" decoding:
            // - Destination ID 0xFF is treated as a broadcast to all LAPICs.
            // - Otherwise, `dest` is treated as a simple 8-bit mask where bit `n` targets LAPIC APIC ID `n`.
            if dest == 0xFF {
                self.inject_fixed_broadcast(vector);
                return;
            }

            for cpu in 0..self.cpu_count() {
                let lapic = self.lapic(cpu);
                let apic_id = lapic.apic_id();
                if apic_id < 8 && (dest & (1u8 << apic_id)) != 0 {
                    lapic.inject_fixed_interrupt(vector);
                }
            }
            return;
        }

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

    fn enable_lapic_svr(ints: &PlatformInterrupts) {
        for cpu_index in 0..ints.cpu_count() {
            let apic_id = u8::try_from(cpu_index).expect("cpu_count should fit in u8");
            enable_lapic_svr_for_apic(ints, apic_id);
        }
    }

    fn msi_message(dest_id: u8, vector: u8) -> MsiMessage {
        msi_message_with(dest_id, vector, false, 0)
    }

    fn msi_message_logical(dest_mask: u8, vector: u8) -> MsiMessage {
        msi_message_with(dest_mask, vector, true, 0)
    }

    fn msi_message_with(
        dest_id: u8,
        vector: u8,
        dest_is_logical: bool,
        delivery_mode: u8,
    ) -> MsiMessage {
        let mut address = 0xFEE0_0000u64 | ((dest_id as u64) << 12);
        if dest_is_logical {
            address |= 1u64 << 2;
        }
        let data = (vector as u16) | ((u16::from(delivery_mode & 0x7)) << 8);
        MsiMessage { address, data }
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

    #[test]
    fn apic_mode_logical_destination_msi_delivers_to_matching_lapics() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr(&ints);

        // Logical destination mask bit1 -> APIC ID 1.
        ints.trigger_msi(msi_message_logical(0b10, 0x66));

        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), Some(0x66));
    }

    #[test]
    fn apic_mode_non_fixed_delivery_mode_is_treated_as_fixed() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr(&ints);

        // Delivery Mode = Lowest Priority (0b001). We currently treat it as Fixed.
        let bsp_id = ints.lapic_apic_id();
        ints.trigger_msi(msi_message_with(bsp_id, 0x77, false, 0b001));

        assert_eq!(ints.get_pending_for_apic(bsp_id), Some(0x77));
    }
}
