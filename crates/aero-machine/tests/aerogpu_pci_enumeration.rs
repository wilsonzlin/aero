use aero_devices::pci::PciBarKind;
use aero_machine::{Machine, MachineConfig};
use aero_pc_constants::{PCI_MMIO_BASE, PCI_MMIO_SIZE};
use aero_protocol::aerogpu::aerogpu_pci as pci;

#[test]
fn aerogpu_enumerates_at_canonical_bdf_with_bar0_in_pci_mmio_window() {
    let mut cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep this test focused on PCI topology.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };
    // Ensure no extra PCI devices are enabled by accident.
    cfg.enable_ahci = false;
    cfg.enable_nvme = false;
    cfg.enable_ide = false;
    cfg.enable_virtio_blk = false;
    cfg.enable_uhci = false;
    cfg.enable_e1000 = false;
    cfg.enable_virtio_net = false;

    let mut m = Machine::new(cfg).unwrap();

    let bdf = aero_devices::pci::profile::AEROGPU.bdf;
    let (id, class, bar0) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let cfg = bus
            .device_config(bdf)
            .expect("AeroGPU PCI function missing from canonical machine");

        let id = cfg.vendor_device_id();
        let class = cfg.class_code();
        let bar0 = cfg
            .bar_range(0)
            .expect("AeroGPU BAR0 should be assigned by PCI BIOS POST");
        (id, class, bar0)
    };

    assert_eq!(id.vendor_id, 0xA3A0, "AeroGPU vendor ID drifted");
    assert_eq!(id.device_id, 0x0001, "AeroGPU device ID drifted");

    assert_eq!(class.class, 0x03, "AeroGPU base class drifted");
    assert_eq!(class.subclass, 0x00, "AeroGPU subclass drifted");
    assert_eq!(class.prog_if, 0x00, "AeroGPU programming interface drifted");
    assert_eq!(bar0.kind, PciBarKind::Mmio32);
    assert_ne!(bar0.base, 0, "AeroGPU BAR0 base must be non-zero");
    assert_eq!(
        bar0.size,
        aero_devices::pci::profile::AEROGPU_BAR0_SIZE,
        "AeroGPU BAR0 size drifted"
    );
    assert_eq!(bar0.base & (bar0.size - 1), 0, "BAR0 base must be aligned");

    let window_end = PCI_MMIO_BASE + PCI_MMIO_SIZE;
    assert!(
        bar0.base >= PCI_MMIO_BASE && bar0.end_exclusive() <= window_end,
        "AeroGPU BAR0 (0x{:x}..0x{:x}) must lie within PCI MMIO window (0x{:x}..0x{:x})",
        bar0.base,
        bar0.end_exclusive(),
        PCI_MMIO_BASE,
        window_end
    );

    // Sanity-check that BAR0 is actually wired into the PCI MMIO router by reading the MMIO magic
    // constant from the BAR0 register space.
    let magic = m.read_physical_u32(bar0.base + u64::from(pci::AEROGPU_MMIO_REG_MAGIC));
    assert_eq!(magic, pci::AEROGPU_MMIO_MAGIC);
}
