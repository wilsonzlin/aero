use aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE;
use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_pc_platform::{PcPlatform, ResetEvent};

#[test]
fn pc_platform_reset_restores_deterministic_power_on_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Capture an initial piece of PCI state so we can verify it's restored deterministically.
    let uhci_bar4_addr = 0x8000_0000
        | ((USB_UHCI_PIIX3.bdf.bus as u32) << 16)
        | ((USB_UHCI_PIIX3.bdf.device as u32) << 11)
        | ((USB_UHCI_PIIX3.bdf.function as u32) << 8)
        | 0x20;
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    let uhci_bar4_before = pc.io.read(0xCFC, 4);

    // Mutate some state:
    // - Enable A20.
    pc.io.write_u8(0x92, 0x02);
    assert!(pc.chipset.a20().enabled());

    // - Touch the PCI config address latch (0xCF8).
    pc.io.write(0xCF8, 4, 0x8000_0000);
    assert_eq!(pc.io.read(0xCF8, 4), 0x8000_0000);

    // - Relocate UHCI BAR4 to a different base (to ensure PCI resources are reset deterministically).
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    pc.io.write(0xCFC, 4, 0xD000);
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    let uhci_bar4_after = pc.io.read(0xCFC, 4);
    assert_ne!(uhci_bar4_after, uhci_bar4_before);

    // - Queue a reset event.
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);

    // Now reset back to baseline.
    pc.reset();

    // A20 must be disabled.
    assert!(!pc.chipset.a20().enabled());

    // Reset should clear any pending reset events.
    assert!(pc.take_reset_events().is_empty());

    // PCI config address latch should be cleared.
    assert_eq!(pc.io.read(0xCF8, 4), 0);

    // UHCI BAR4 should be restored to its initial BIOS-assigned value.
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    assert_eq!(pc.io.read(0xCFC, 4), uhci_bar4_before);
}
