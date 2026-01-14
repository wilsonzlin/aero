use aero_devices::pci::{profile, PciVendorDeviceId};
use aero_machine::{Machine, MachineConfig};

#[test]
fn machine_exposes_aerogpu_pci_identity_at_canonical_bdf() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal for the identity check.
        enable_vga: false,
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

    let dev = bus
        .device_config(profile::AEROGPU.bdf)
        .expect("expected AeroGPU PCI function to be present");
    assert_eq!(
        dev.vendor_device_id(),
        PciVendorDeviceId {
            vendor_id: profile::AEROGPU.vendor_id,
            device_id: profile::AEROGPU.device_id,
        }
    );
}
