use crate::pci::PciBus;

/// ACPI name of the PCI root device.
///
/// The DSDT must expose this as `\_SB.PCI0` for Windows to attach the PCI bus driver.
pub const ACPI_PCI_ROOT_NAME: &str = "PCI0";

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PciPrtEntry {
    /// PCI device address encoded as (device << 16) | function.
    pub address: u32,
    /// Interrupt pin, 0..3 for INTA..INTD.
    pub pin: u8,
    /// Global System Interrupt (GSI) to route this pin to.
    pub gsi: u32,
}

/// Build a simple `_PRT` routing table for bus 0.
///
/// This uses a typical "PCI swizzle" mapping into IOAPIC GSIs 16..19:
/// `gsi = 16 + ((device + pin) % 4)`.
pub fn build_prt_bus0(bus: &PciBus) -> Vec<PciPrtEntry> {
    let mut prt = Vec::new();
    for addr in bus.iter_device_addrs() {
        if addr.bus != 0 {
            continue;
        }
        // Host bridge doesn't participate in INTx routing.
        if addr.device == 0 && addr.function == 0 {
            continue;
        }
        for pin in 0u8..4u8 {
            let gsi = 16 + ((u32::from(addr.device) + u32::from(pin)) % 4);
            prt.push(PciPrtEntry {
                address: (u32::from(addr.device) << 16) | u32::from(addr.function),
                pin,
                gsi,
            });
        }
    }
    prt
}

/// Generate a minimal DSDT in ASL form.
///
/// This is currently meant for testing and for wiring the ACPI namespace to the PCI topology.
/// In a full firmware implementation this should be compiled to AML.
pub fn dsdt_asl(bus: &PciBus) -> String {
    let prt = build_prt_bus0(bus);

    let mut out = String::new();
    out.push_str("DefinitionBlock (\"\", \"DSDT\", 2, \"AERO\", \"AEROPCI\", 0x00000001)\n");
    out.push_str("{\n");
    out.push_str("    Scope (\\_SB)\n");
    out.push_str("    {\n");
    out.push_str("        Device (");
    out.push_str(ACPI_PCI_ROOT_NAME);
    out.push_str(")\n");
    out.push_str("        {\n");
    out.push_str("            Name (_HID, EisaId (\"PNP0A03\"))\n");
    out.push_str("            Name (_UID, Zero)\n");
    out.push_str("            Name (_PRT, Package ()\n");
    out.push_str("            {\n");
    for entry in prt {
        out.push_str("                Package () { 0x");
        out.push_str(&format!("{:08X}", entry.address));
        out.push_str(", ");
        out.push_str(&format!("{}, ", entry.pin));
        out.push_str("Zero, ");
        out.push_str(&format!("{}", entry.gsi));
        out.push_str(" },\n");
    }
    out.push_str("            })\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::PciPlatform;

    #[test]
    fn dsdt_contains_pci0_and_prt_entries_for_present_devices() {
        let bus = PciPlatform::build_bus();
        let asl = dsdt_asl(&bus);
        assert!(asl.contains("Device (PCI0)"));
        assert!(asl.contains("Name (_PRT"));

        // ISA bridge at 00:1f.0 should appear in _PRT (device<<16).
        assert!(asl.contains("0x001F0000"));
    }
}
