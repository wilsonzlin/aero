use aero_devices::pci::PciBdf;
use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_pci_stub_does_not_collide_with_canonical_aerogpu_bdf() {
    // This test exists to guard the Windows driver binding contract documented in:
    // - docs/abi/aerogpu-pci-identity.md
    // - docs/pci-device-compatibility.md
    //
    // `00:07.0` is reserved for AeroGPU (A3A0:0001). The canonical machine may optionally expose
    // a transitional VGA/VBE PCI function for boot display, but it must not occupy `00:07.0` with
    // non-AeroGPU IDs.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
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
    if aerogpu_vendor != 0xFFFF {
        let aerogpu_device = bus.read_config(aerogpu_bdf, 0x02, 2) as u16;
        assert_eq!(aerogpu_vendor, 0xA3A0);
        assert_eq!(aerogpu_device, 0x0001);
    }

    // Transitional VGA/VBE PCI stub used by `aero_gpu_vga` for LFB routing.
    let vga_bdf = PciBdf::new(0, 0x0c, 0);
    let vga_vendor = bus.read_config(vga_bdf, 0x00, 2) as u16;
    if vga_vendor == 0xFFFF {
        panic!(
            "expected VGA PCI stub at {vga_bdf:?} when enable_vga=true and enable_pc_platform=true"
        );
    }
    let vga_device = bus.read_config(vga_bdf, 0x02, 2) as u16;
    assert_eq!(vga_vendor, 0x1234);
    assert_eq!(vga_device, 0x1111);
}
