use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController as _, PlatformInterruptMode};

fn bar0_base_for(
    pci_cfg: &mut aero_devices::pci::PciConfigPorts,
    bdf: aero_devices::pci::PciBdf,
) -> u64 {
    let (lo, hi) = (
        pci_cfg.bus_mut().read_config(bdf, 0x10, 4),
        pci_cfg.bus_mut().read_config(bdf, 0x14, 4),
    );
    (u64::from(hi) << 32) | u64::from(lo & 0xFFFF_FFF0)
}

#[test]
fn machine_virtio_net_msix_config_interrupt_delivers_lapic_vector() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        enable_e1000: false,
        // Keep the machine minimal and deterministic for this integration test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    let bdf = profile::VIRTIO_NET.bdf;
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform enabled");

    let (bar0_base, msix_cap_off) = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bar0_base = bar0_base_for(&mut pci_cfg, bdf);
        assert_ne!(bar0_base, 0, "BAR0 must be assigned by PCI BIOS POST");

        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("virtio-net device missing from PCI bus");

        // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so only MSI-X delivery is
        // observable.
        cfg.set_command(0x0406);

        let msix_cap_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("virtio-net should expose MSI-X capability") as u16;

        // Enable MSI-X (bit 15) and clear function mask (bit 14).
        let ctrl = cfg.read(msix_cap_off + 0x02, 2) as u16;
        cfg.write(
            msix_cap_off + 0x02,
            2,
            u32::from((ctrl & !(1 << 14)) | (1 << 15)),
        );

        (bar0_base, msix_cap_off)
    };

    // Program table entry 0 (vector index 0): destination = BSP (APIC ID 0), vector = 0x55.
    let table_offset = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let table = pci_cfg.bus_mut().read_config(bdf, msix_cap_off + 0x04, 4);
        assert_eq!(table & 0x7, 0, "MSI-X table should live in BAR0 (BIR=0)");
        u64::from(table & !0x7)
    };
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, 0x0055);
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // BAR0 common config base for Aero's virtio-pci contract.
    const COMMON: u64 = 0x0000;

    // Assign MSI-X vector index 0 as the config interrupt vector.
    m.write_physical_u16(bar0_base + COMMON + 0x10, 0); // msix_config_vector
    assert_eq!(
        m.read_physical_u16(bar0_base + COMMON + 0x10),
        0,
        "msix_config_vector should be writable after MSI-X is enabled"
    );

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Trigger a device configuration interrupt directly from the virtio transport.
    let virtio = m.virtio_net().expect("virtio-net should be enabled");
    virtio.borrow_mut().signal_config_interrupt();

    assert_eq!(interrupts.borrow().get_pending(), Some(0x55));
}
