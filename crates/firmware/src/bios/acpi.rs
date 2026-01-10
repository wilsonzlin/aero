use super::{BiosBus, ACPI_TABLE_BASE, ACPI_TABLE_SIZE, EBDA_BASE};

#[derive(Debug, Clone, Copy)]
pub struct AcpiPlacement {
    /// Physical base address reserved for ACPI tables.
    pub tables_base: u64,
    /// Size of the reserved ACPI table area in bytes.
    pub tables_size: usize,
    /// Suggested physical address for the RSDP (must be in EBDA or 0xE0000-0xFFFFF).
    pub rsdp_addr: u64,
}

pub trait AcpiBuilder {
    /// Build ACPI tables and place them into the reserved regions.
    ///
    /// Returns the physical address of the RSDP if one was written.
    fn build(&mut self, bus: &mut dyn BiosBus, placement: AcpiPlacement) -> Option<u64>;
}

/// Placeholder ACPI builder.
///
/// A full implementation is expected to be added in a dedicated ACPI task. The
/// BIOS integration (placement + call-out) is provided here so the builder can
/// be swapped in without changing POST logic.
#[derive(Debug, Default)]
pub struct StubAcpiBuilder;

impl AcpiBuilder for StubAcpiBuilder {
    fn build(&mut self, bus: &mut dyn BiosBus, placement: AcpiPlacement) -> Option<u64> {
        // Clear the ACPI table area (it's RAM, but reserved for ACPI).
        for i in 0..placement.tables_size as u64 {
            bus.write_u8(placement.tables_base + i, 0);
        }

        // Minimal ACPI 1.0 RSDP (20 bytes). Enough for guests that just scan for the signature.
        let rsdp_addr = placement.rsdp_addr;
        let mut rsdp = [0u8; 20];
        rsdp[0..8].copy_from_slice(b"RSD PTR ");
        rsdp[9..15].copy_from_slice(b"Aero  "); // OEMID
        rsdp[15] = 0; // revision 0 (ACPI 1.0)

        // RSDT address (unused in this placeholder).
        rsdp[16..20].copy_from_slice(&(placement.tables_base as u32).to_le_bytes());

        // Checksum: sum of all bytes should be 0 mod 256.
        let checksum = (0u8).wrapping_sub(rsdp.iter().copied().fold(0u8, u8::wrapping_add));
        rsdp[8] = checksum;

        bus.write_physical(rsdp_addr, &rsdp);
        Some(rsdp_addr)
    }
}

pub fn default_placement() -> AcpiPlacement {
    AcpiPlacement {
        tables_base: ACPI_TABLE_BASE,
        tables_size: ACPI_TABLE_SIZE,
        rsdp_addr: EBDA_BASE + 0x100,
    }
}
