#![no_std]

//! Canonical PCI INTx swizzle + PIRQ[A-D] -> GSI routing helpers.
//!
//! Aero models PCI INTx routing using the conventional deterministic swizzle used by QEMU and
//! PC-compatible firmware:
//!
//! ```text
//! pirq = (device + pin_index) & 3
//! ```
//!
//! where:
//! - `device` is the PCI slot device number (0-31)
//! - `pin_index` is 0..3 for INTA..INTD
//!
//! The resulting PIRQ index (0..3 for A..D) is then mapped to a platform Global System Interrupt
//! (GSI) via the caller-provided `pirq_to_gsi` table.
//!
/// Computes the PIRQ index (0 = A, 1 = B, 2 = C, 3 = D) for a device/pin pair.
#[inline]
pub const fn pirq_index(device: u8, pin_index: u8) -> u8 {
    device.wrapping_add(pin_index) & 0x03
}

/// Returns the routed GSI for a device/pin pair.
#[inline]
pub const fn gsi_for_intx(pirq_to_gsi: [u32; 4], device: u8, pin_index: u8) -> u32 {
    pirq_to_gsi[pirq_index(device, pin_index) as usize]
}

/// Returns the value to program into the PCI config-space `Interrupt Line` register (0x3C).
///
/// - `interrupt_pin_cfg` is the PCI config-space `Interrupt Pin` register encoding:
///   0 = no interrupt pin, 1 = INTA#, 2 = INTB#, 3 = INTC#, 4 = INTD#.
/// - When `interrupt_pin_cfg == 0`, the conventional "unknown/unconnected" value 0xFF is returned.
#[inline]
pub fn irq_line_for_intx(pirq_to_gsi: [u32; 4], device: u8, interrupt_pin_cfg: u8) -> u8 {
    if interrupt_pin_cfg == 0 {
        return 0xFF;
    }
    let pin_index = interrupt_pin_cfg.wrapping_sub(1) & 0x03;
    let gsi = gsi_for_intx(pirq_to_gsi, device, pin_index);
    u8::try_from(gsi).unwrap_or(0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pirq_swizzle_matches_qemu_style_policy() {
        // Device 0: no swizzle.
        assert_eq!(pirq_index(0, 0), 0);
        assert_eq!(pirq_index(0, 1), 1);
        assert_eq!(pirq_index(0, 2), 2);
        assert_eq!(pirq_index(0, 3), 3);

        // Device 1: swizzled by one.
        assert_eq!(pirq_index(1, 0), 1);
        assert_eq!(pirq_index(1, 1), 2);
        assert_eq!(pirq_index(1, 2), 3);
        assert_eq!(pirq_index(1, 3), 0);

        // Device 4 wraps around.
        assert_eq!(pirq_index(4, 0), 0);
    }

    #[test]
    fn gsi_and_irq_line_helpers_follow_pirq_map() {
        let map = [10, 11, 12, 13];

        assert_eq!(gsi_for_intx(map, 0, 0), 10);
        assert_eq!(gsi_for_intx(map, 1, 0), 11);
        assert_eq!(gsi_for_intx(map, 2, 3), 11);

        // interrupt_pin_cfg: 1=INTA, 4=INTD.
        assert_eq!(irq_line_for_intx(map, 1, 1), 11);
        assert_eq!(irq_line_for_intx(map, 2, 4), 11);

        // interrupt_pin_cfg=0 => 0xFF sentinel.
        assert_eq!(irq_line_for_intx(map, 2, 0), 0xFF);
    }
}
