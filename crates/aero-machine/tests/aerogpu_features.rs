use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

#[test]
fn aerogpu_features_lo_hi_match_implemented_capabilities() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal; the PCI/ECAM/MMIO router is what we need.
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    enable_a20(&mut m);

    // Discover BAR0 base via PCI config.
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let cfg = bus
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU device must be present on the PCI bus");

        // Ensure MEM decoding is enabled so BAR0 MMIO accesses route to the device.
        assert!(
            (cfg.command() & 0x2) != 0,
            "expected PCI MEM decoding enabled for AeroGPU (command=0x{:04x})",
            cfg.command()
        );

        cfg.bar_range(0)
            .expect("AeroGPU BAR0 must be assigned by PCI BIOS POST")
            .base
    };
    assert_ne!(bar0_base, 0, "AeroGPU BAR0 base must be non-zero");

    // Sanity: identity regs.
    let magic = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_MAGIC));
    assert_eq!(magic, pci::AEROGPU_MMIO_MAGIC);
    let abi = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_ABI_VERSION));
    assert_eq!(abi, pci::AEROGPU_ABI_VERSION_U32);

    // Feature discovery.
    let features_lo = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FEATURES_LO));
    let features_hi = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FEATURES_HI));
    let features = (u64::from(features_hi) << 32) | u64::from(features_lo);

    // `aero-machine` currently implements:
    // - Scanout (framebuffer DMA-read)
    // - VBlank sequencing / IRQs
    // - Fence-page writeback
    // - Cursor overlay
    // - Error reporting registers
    let expected = pci::AEROGPU_FEATURE_SCANOUT
        | pci::AEROGPU_FEATURE_CURSOR
        | pci::AEROGPU_FEATURE_VBLANK
        | pci::AEROGPU_FEATURE_FENCE_PAGE
        | pci::AEROGPU_FEATURE_ERROR_INFO;

    assert_eq!(
        features, expected,
        "unexpected AeroGPU feature bits: got=0x{features:016x} expected=0x{expected:016x}"
    );

    // Guardrails: transfer is not implemented yet; ensure it is not advertised.
    assert_eq!(features & pci::AEROGPU_FEATURE_TRANSFER, 0);
}
