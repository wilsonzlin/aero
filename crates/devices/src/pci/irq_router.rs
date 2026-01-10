use std::collections::HashMap;

use super::{PciBdf, PciConfigSpace, PciInterruptPin};
use crate::apic::IoApic;
use crate::pic8259::DualPic8259;

/// A sink that accepts level changes for a platform Global System Interrupt (GSI).
///
/// The IOAPIC input pins are typically addressed by their GSI number.
pub trait GsiLevelSink {
    fn set_gsi_level(&mut self, gsi: u32, level: bool);
}

/// A sink that accepts level changes for a legacy PIC IRQ input (0-15).
pub trait PicIrqLevelSink {
    fn set_irq_level(&mut self, irq: u8, level: bool);
}

impl GsiLevelSink for IoApic {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.set_irq_level(gsi, level);
    }
}

impl PicIrqLevelSink for DualPic8259 {
    fn set_irq_level(&mut self, irq: u8, level: bool) {
        if level {
            self.raise_irq(irq);
        } else {
            self.lower_irq(irq);
        }
    }
}

/// A helper sink that fans out GSI level changes to both an IOAPIC and the legacy PIC.
///
/// This is useful for supporting both APIC and legacy PIC mode during early bring-up.
/// If the routed GSI is not in the ISA range (0-15), mirroring to the PIC is skipped.
pub struct IoApicPicMirrorSink<'a> {
    ioapic: &'a mut dyn GsiLevelSink,
    pic: &'a mut dyn PicIrqLevelSink,
}

impl<'a> IoApicPicMirrorSink<'a> {
    pub fn new(ioapic: &'a mut dyn GsiLevelSink, pic: &'a mut dyn PicIrqLevelSink) -> Self {
        Self { ioapic, pic }
    }
}

impl GsiLevelSink for IoApicPicMirrorSink<'_> {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.ioapic.set_gsi_level(gsi, level);
        if let Ok(irq) = u8::try_from(gsi) {
            if irq < 16 {
                self.pic.set_irq_level(irq, level);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciIntxRouterConfig {
    /// Mapping of PIRQ[A-D] to GSIs.
    pub pirq_to_gsi: [u32; 4],
}

impl Default for PciIntxRouterConfig {
    fn default() -> Self {
        // Match a typical PC-compatible setup where PCI INTx ends up on IRQ/GSI 10-13.
        Self {
            pirq_to_gsi: [10, 11, 12, 13],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct IntxSource {
    bdf: PciBdf,
    pin: PciInterruptPin,
}

/// Routes PCI INTx pins (INTA#-INTD#) to platform interrupts.
///
/// The routing follows a deterministic swizzle:
/// `PIRQ = (pin + device) mod 4`, where `pin` is 0 for INTA#, 1 for INTB#, etc.
///
/// Each PIRQ[A-D] is then mapped to a platform GSI. Multiple devices can share a PIRQ and/or GSI,
/// so the router maintains level-triggered semantics by reference-counting assertions.
pub struct PciIntxRouter {
    cfg: PciIntxRouterConfig,
    source_level: HashMap<IntxSource, bool>,
    gsi_assert_count: HashMap<u32, u32>,
}

impl PciIntxRouter {
    pub fn new(cfg: PciIntxRouterConfig) -> Self {
        Self {
            cfg,
            source_level: HashMap::new(),
            gsi_assert_count: HashMap::new(),
        }
    }

    /// Computes the PIRQ index (0 = A, 1 = B, 2 = C, 3 = D) for a device/pin pair.
    pub fn pirq_index(&self, bdf: PciBdf, pin: PciInterruptPin) -> usize {
        (pin.index() + bdf.device as usize) & 3
    }

    /// Returns the routed GSI for a device/pin pair.
    pub fn gsi_for_intx(&self, bdf: PciBdf, pin: PciInterruptPin) -> u32 {
        let pirq = self.pirq_index(bdf, pin);
        self.cfg.pirq_to_gsi[pirq]
    }

    /// Updates a device's config-space `Interrupt Line` and `Interrupt Pin` registers.
    ///
    /// This should be called during PCI enumeration so the guest can discover the routing.
    pub fn configure_device_intx(
        &self,
        bdf: PciBdf,
        pin: Option<PciInterruptPin>,
        config: &mut PciConfigSpace,
    ) {
        match pin {
            Some(pin) => {
                let gsi = self.gsi_for_intx(bdf, pin);
                let line = u8::try_from(gsi).unwrap_or(0xFF);
                config.set_interrupt_pin(pin.to_config_u8());
                config.set_interrupt_line(line);
            }
            None => {
                config.set_interrupt_pin(0);
                config.set_interrupt_line(0xFF);
            }
        }
    }

    /// Sets the asserted level of a PCI function's INTx pin.
    ///
    /// The router aggregates multiple sources mapped onto the same GSI so the line remains
    /// asserted until all devices deassert (level-triggered semantics).
    pub fn set_intx_level(
        &mut self,
        bdf: PciBdf,
        pin: PciInterruptPin,
        level: bool,
        sink: &mut dyn GsiLevelSink,
    ) {
        let src = IntxSource { bdf, pin };
        let prev = self.source_level.insert(src, level).unwrap_or(false);
        if prev == level {
            return;
        }

        let gsi = self.gsi_for_intx(bdf, pin);
        let count = self.gsi_assert_count.entry(gsi).or_insert(0);

        if level {
            *count += 1;
            if *count == 1 {
                sink.set_gsi_level(gsi, true);
            }
        } else {
            debug_assert!(*count > 0, "INTx deassert would underflow assert count");
            if *count > 0 {
                *count -= 1;
                if *count == 0 {
                    sink.set_gsi_level(gsi, false);
                }
            }
        }
    }

    pub fn assert_intx(&mut self, bdf: PciBdf, pin: PciInterruptPin, sink: &mut dyn GsiLevelSink) {
        self.set_intx_level(bdf, pin, true, sink);
    }

    pub fn deassert_intx(
        &mut self,
        bdf: PciBdf,
        pin: PciInterruptPin,
        sink: &mut dyn GsiLevelSink,
    ) {
        self.set_intx_level(bdf, pin, false, sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockSink {
        events: Vec<(u32, bool)>,
    }

    impl GsiLevelSink for MockSink {
        fn set_gsi_level(&mut self, gsi: u32, level: bool) {
            self.events.push((gsi, level));
        }
    }

    #[test]
    fn routing_returns_expected_gsi() {
        let router = PciIntxRouter::new(PciIntxRouterConfig::default());

        // Device 0: no swizzle.
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA),
            10
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntB),
            11
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntC),
            12
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntD),
            13
        );

        // Device 1: swizzled by one.
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntA),
            11
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntB),
            12
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntC),
            13
        );
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntD),
            10
        );

        // Device 4 wraps back to the same PIRQ.
        assert_eq!(
            router.gsi_for_intx(PciBdf::new(0, 4, 0), PciInterruptPin::IntA),
            10
        );
    }

    #[test]
    fn shared_line_aggregation_keeps_line_asserted_until_all_deassert() {
        let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
        let mut sink = MockSink::default();

        let dev0 = PciBdf::new(0, 0, 0);
        let dev4 = PciBdf::new(0, 4, 0); // Same swizzle as dev0 (device mod 4).

        router.assert_intx(dev0, PciInterruptPin::IntA, &mut sink);
        router.assert_intx(dev4, PciInterruptPin::IntA, &mut sink);

        // Only the first assertion should transition the line high.
        assert_eq!(sink.events, vec![(10, true)]);

        router.deassert_intx(dev0, PciInterruptPin::IntA, &mut sink);
        assert_eq!(sink.events, vec![(10, true)]);

        router.deassert_intx(dev4, PciInterruptPin::IntA, &mut sink);
        assert_eq!(sink.events, vec![(10, true), (10, false)]);
    }

    #[test]
    fn configure_device_updates_interrupt_line_and_pin_registers() {
        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);

        router.configure_device_intx(PciBdf::new(0, 1, 0), Some(PciInterruptPin::IntA), &mut cfg);

        assert_eq!(cfg.interrupt_pin(), 1);
        assert_eq!(cfg.interrupt_line(), 11);
    }
}
