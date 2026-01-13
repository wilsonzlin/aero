use aero_devices::pci::profile::CANONICAL_IO_DEVICES;
use aero_devices::pci::PciBdf;
use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_pci_stub_does_not_collide_with_canonical_aerogpu_bdf() {
    // This test exists to guard the Windows driver binding contract documented in:
    // - docs/abi/aerogpu-pci-identity.md
    // - docs/pci-device-compatibility.md
    //
    // `00:07.0` is reserved for AeroGPU (A3A0:0001). Ensure no non-AeroGPU identity ever occupies
    // this BDF (which would break the Windows driver binding contract).
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the machine minimal for the contract check.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let m = Machine::new(cfg).unwrap();
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    // Canonical AeroGPU BDF.
    let aerogpu_bdf = PciBdf::new(0, 0x07, 0);
    let aerogpu_vendor = bus.read_config(aerogpu_bdf, 0x00, 2) as u16;
    let aerogpu_present = aerogpu_vendor != 0xFFFF;
    // `00:07.0` is a *reserved* BDF: if any device is present there, it must be the canonical
    // AeroGPU identity (`A3A0:0001`).
    if aerogpu_present {
        let aerogpu_device = bus.read_config(aerogpu_bdf, 0x02, 2) as u16;
        assert_eq!(aerogpu_vendor, 0xA3A0);
        assert_eq!(aerogpu_device, 0x0001);
    }

    // Historically the canonical machine exposed a Bochs/QEMU-style VGA PCI stub
    // (`1234:1111`, see `aero_devices::pci::profile::VGA_TRANSITIONAL_STUB`) used only for VBE LFB
    // routing. The canonical machine no longer relies on this, and now expects this BDF to be
    // empty.
    let vga_bdf = aero_devices::pci::profile::VGA_TRANSITIONAL_STUB.bdf;
    let vga_vendor = bus.read_config(vga_bdf, 0x00, 2) as u16;
    // Guardrail: ensure no canonical paravirtual device profile uses this BDF (even though it is
    // currently expected to be empty).
    for profile in CANONICAL_IO_DEVICES {
        assert!(
            profile.bdf != vga_bdf,
            "VGA PCI stub BDF {vga_bdf:?} collides with canonical device profile `{}` at {:?}",
            profile.name,
            profile.bdf
        );
    }

    assert_eq!(
        vga_vendor, 0xFFFF,
        "expected {vga_bdf:?} to be empty; transitional VGA PCI stub (1234:1111) was removed"
    );
}

#[test]
fn aerogpu_is_exposed_at_canonical_bdf_without_transitional_vga_stub_when_enabled() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // AeroGPU and the legacy VGA/VBE device are mutually exclusive.
        enable_vga: false,
        // Keep the machine minimal for the contract check.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let m = Machine::new(cfg).unwrap();
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    let aerogpu_bdf = aero_devices::pci::profile::AEROGPU.bdf;
    let aerogpu_vendor = bus.read_config(aerogpu_bdf, 0x00, 2) as u16;
    let aerogpu_device = bus.read_config(aerogpu_bdf, 0x02, 2) as u16;
    assert_eq!(aerogpu_vendor, 0xA3A0);
    assert_eq!(aerogpu_device, 0x0001);

    // Transitional VGA stub must be absent when AeroGPU is enabled.
    let vga_bdf = aero_devices::pci::profile::VGA_TRANSITIONAL_STUB.bdf;
    let vga_vendor = bus.read_config(vga_bdf, 0x00, 2) as u16;
    assert_eq!(
        vga_vendor, 0xFFFF,
        "expected transitional VGA PCI stub at {vga_bdf:?} to be absent when enable_aerogpu=true"
    );
}
