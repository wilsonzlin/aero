use crate::pci::{PciBarKind, PciBarRange, PciBus, PciResourceAllocator, PciResourceError};

/// A minimal PCI "POST" routine that assigns BARs and enables decoding.
///
/// This is a firmware helper, not a full PCI BIOS implementation. It exists so that early boot
/// payloads can see a coherent PCI resource map without relying on the guest OS to assign BARs.
pub fn bios_post(
    pci: &mut PciBus,
    allocator: &mut PciResourceAllocator,
) -> Result<(), PciResourceError> {
    bios_post_with_extra_reservations(pci, allocator, core::iter::empty::<PciBarRange>())
}

/// Variant of [`bios_post`] that allows the caller to reserve fixed MMIO/I/O regions inside the
/// allocator window that are not represented by a PCI function.
///
/// This is useful for "hybrid" platform layouts where a fixed-function device (like a legacy VGA
/// framebuffer) lives inside the ACPI-reported PCI MMIO window but is routed directly by the
/// platform instead of being exposed as a PCI device with a BAR.
pub fn bios_post_with_extra_reservations<I>(
    pci: &mut PciBus,
    allocator: &mut PciResourceAllocator,
    extra_reserved: I,
) -> Result<(), PciResourceError>
where
    I: IntoIterator<Item = PciBarRange>,
{
    pci.reset_with_extra_reservations(allocator, extra_reserved)?;

    let addrs = pci.iter_device_addrs().collect::<Vec<_>>();
    for addr in addrs {
        let Some(cfg) = pci.device_config(addr) else {
            continue;
        };
        let mut command: u16 = 0;
        for bar in 0u8..6u8 {
            let Some(def) = cfg.bar_definition(bar) else {
                continue;
            };
            match def.kind() {
                PciBarKind::Io => command |= 0x1,
                PciBarKind::Mmio32 | PciBarKind::Mmio64 => command |= 0x2,
            }
        }
        if command != 0 {
            pci.write_config(addr, 0x04, 2, u32::from(command));
        }
    }

    Ok(())
}
