//! Regression test: `PcPlatform::reset()` must not collapse an SMP LAPIC topology.
//!
//! The platform interrupt controller supports a configurable number of LAPICs so firmware can
//! publish SMP-capable ACPI/SMBIOS tables. A reset must preserve that configuration.
#![cfg(not(target_arch = "wasm32"))]

use aero_pc_platform::{PcPlatform, PcPlatformConfig};

#[test]
fn pc_platform_reset_preserves_lapic_count_and_ids_in_smp_mode() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            cpu_count: 4,
            // Keep the platform minimal; this test only cares about LAPIC topology across reset.
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_ahci: false,
            enable_nvme: false,
            enable_ide: false,
            enable_e1000: false,
            enable_virtio_blk: false,
            enable_hda: false,
            ..Default::default()
        },
    );

    assert_eq!(pc.interrupts.borrow().cpu_count(), 4);
    for cpu in 0..4usize {
        assert_eq!(pc.interrupts.borrow().lapic(cpu).apic_id(), cpu as u8);
    }

    pc.reset();

    assert_eq!(pc.interrupts.borrow().cpu_count(), 4);
    for cpu in 0..4usize {
        assert_eq!(pc.interrupts.borrow().lapic(cpu).apic_id(), cpu as u8);
    }
}
