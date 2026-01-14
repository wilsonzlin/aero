use aero_devices::pci::PciBarKind;
use aero_machine::{Machine, MachineConfig};
use aero_pc_constants::{PCI_MMIO_BASE, PCI_MMIO_SIZE};
use aero_protocol::aerogpu::aerogpu_pci as pci;

#[test]
fn aerogpu_enumerates_at_canonical_bdf_with_bars_in_pci_mmio_window() {
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
    let (id, class, command, bar0, bar1, bar0_reg, bar1_reg) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let cfg = bus
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing from canonical machine");

        let id = cfg.vendor_device_id();
        let class = cfg.class_code();
        let command = cfg.command();
        let bar0_reg = cfg.read(0x10, 4);
        let bar1_reg = cfg.read(0x14, 4);
        let bar0 = cfg
            .bar_range(0)
            .expect("AeroGPU BAR0 should be assigned by PCI BIOS POST");
        let bar1 = cfg
            .bar_range(1)
            .expect("AeroGPU BAR1 should be assigned by PCI BIOS POST");
        (id, class, command, bar0, bar1, bar0_reg, bar1_reg)
    };

    assert_eq!(id.vendor_id, 0xA3A0, "AeroGPU vendor ID drifted");
    assert_eq!(id.device_id, 0x0001, "AeroGPU device ID drifted");

    assert_eq!(class.class, 0x03, "AeroGPU base class drifted");
    assert_eq!(class.subclass, 0x00, "AeroGPU subclass drifted");
    assert_eq!(class.prog_if, 0x00, "AeroGPU programming interface drifted");

    // PCI BIOS POST should have enabled memory decoding (AeroGPU exposes only MMIO BARs).
    assert_eq!(command & 0x1, 0, "AeroGPU should not enable PCI I/O decode");
    assert_ne!(
        command & 0x2,
        0,
        "AeroGPU should enable PCI memory decode during BIOS POST"
    );

    assert_eq!(bar0.kind, PciBarKind::Mmio32);
    assert_ne!(bar0.base, 0, "AeroGPU BAR0 base must be non-zero");
    assert_eq!(
        bar0.size,
        aero_devices::pci::profile::AEROGPU_BAR0_SIZE,
        "AeroGPU BAR0 size drifted"
    );
    assert_eq!(bar0.base & (bar0.size - 1), 0, "BAR0 base must be aligned");
    assert_eq!(
        u64::from(bar0_reg & 0xFFFF_FFF0),
        bar0.base,
        "BAR0 base must match config-space BAR register"
    );
    assert_eq!(
        bar0_reg & 0x1,
        0,
        "BAR0 must be a memory BAR (bit0=0), got 0x{bar0_reg:08x}"
    );
    assert_eq!(
        bar0_reg & (0b11 << 1),
        0,
        "BAR0 must be a 32-bit BAR (bits2:1=00), got 0x{bar0_reg:08x}"
    );
    assert_eq!(
        bar0_reg & (1 << 3),
        0,
        "BAR0 must be non-prefetchable (bit3=0), got 0x{bar0_reg:08x}"
    );

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

    // BAR1 (VRAM aperture) should also be reachable via the same PCI MMIO window.
    assert_eq!(bar1.kind, PciBarKind::Mmio32);
    assert_ne!(bar1.base, 0, "AeroGPU BAR1 base must be non-zero");
    assert_eq!(
        bar1.size,
        aero_devices::pci::profile::AEROGPU_VRAM_SIZE,
        "AeroGPU BAR1 size drifted"
    );
    assert_eq!(bar1.base & (bar1.size - 1), 0, "BAR1 base must be aligned");
    assert_eq!(
        u64::from(bar1_reg & 0xFFFF_FFF0),
        bar1.base,
        "BAR1 base must match config-space BAR register"
    );
    assert_eq!(
        bar1_reg & 0x1,
        0,
        "BAR1 must be a memory BAR (bit0=0), got 0x{bar1_reg:08x}"
    );
    assert_eq!(
        bar1_reg & (0b11 << 1),
        0,
        "BAR1 must be a 32-bit BAR (bits2:1=00), got 0x{bar1_reg:08x}"
    );
    assert_ne!(
        bar1_reg & (1 << 3),
        0,
        "BAR1 must be prefetchable (bit3=1), got 0x{bar1_reg:08x}"
    );
    assert!(
        bar1.base >= PCI_MMIO_BASE && bar1.end_exclusive() <= window_end,
        "AeroGPU BAR1 (0x{:x}..0x{:x}) must lie within PCI MMIO window (0x{:x}..0x{:x})",
        bar1.base,
        bar1.end_exclusive(),
        PCI_MMIO_BASE,
        window_end
    );

    assert!(
        bar0.end_exclusive() <= bar1.base || bar1.end_exclusive() <= bar0.base,
        "AeroGPU BARs must not overlap (BAR0: 0x{:x}..0x{:x}, BAR1: 0x{:x}..0x{:x})",
        bar0.base,
        bar0.end_exclusive(),
        bar1.base,
        bar1.end_exclusive()
    );

    // Probe that BAR1 is wired into the MMIO router by performing a small write/read roundtrip at
    // two offsets:
    // - in the VBE LFB region (after the reserved VGA planar storage),
    // - near the end of the BAR, to catch partial routing bugs.
    let mut probe_roundtrip = |addr: u64| {
        let orig = m.read_physical_u32(addr);
        let value = orig ^ 0xA5A5_5A5A;
        m.write_physical_u32(addr, value);
        assert_eq!(m.read_physical_u32(addr), value);
    };
    probe_roundtrip(bar1.base + u64::from(pci::AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES));
    // WASM builds cap the BAR1 VRAM backing store at 32MiB to avoid ballooning the browser heap.
    // The guest-visible BAR size is still 64MiB (device contract), but reads/writes beyond the
    // allocation are treated as zero/ignored.
    #[cfg(target_arch = "wasm32")]
    let max_vram_backing_bytes = bar1.size.min(32 * 1024 * 1024u64);
    #[cfg(not(target_arch = "wasm32"))]
    let max_vram_backing_bytes = bar1.size;
    probe_roundtrip(bar1.base + max_vram_backing_bytes - 0x1000);
}
