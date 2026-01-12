use aero_devices::acpi_pm::PM1_CNT_SCI_EN;
use aero_devices::clock::Clock;
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::pci::PCI_CFG_DATA_PORT;
use aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::{PcPlatform, ResetEvent, PCIE_ECAM_BASE};
use memory::MemoryBus as _;

#[test]
fn pc_platform_wires_canonical_ports_mmio_and_reset_a20() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // A20 masking (starts disabled).
    pc.memory.write_u8(0x0, 0xAA);
    assert_eq!(pc.memory.read_u8(0x1_00000), 0xAA);

    // Enable A20 via port 0x92.
    pc.io.write_u8(0x92, 0x02);
    pc.memory.write_u8(0x1_00000, 0xBB);
    assert_eq!(pc.memory.read_u8(0x0), 0xAA);
    assert_eq!(pc.memory.read_u8(0x1_00000), 0xBB);

    // ACPI enable handshake toggles PM1_CNT.SCI_EN.
    pc.io.write_u8(0xB2, 0xA0);
    let pm1_cnt = pc.io.read(0x0404, 2) as u16;
    assert_ne!(pm1_cnt & PM1_CNT_SCI_EN, 0);

    // PCI config mechanism #1 ports (host bridge vendor/device ID).
    pc.io.write(0xCF8, 4, 0x8000_0000);
    let id = pc.io.read(PCI_CFG_DATA_PORT, 4);
    assert_eq!(id & 0xFFFF, 0x8086);

    // PCIe ECAM window should expose the same config space as 0xCF8/0xCFC.
    let id_ecam = pc.memory.read_u32(PCIE_ECAM_BASE);
    assert_eq!(id_ecam & 0xFFFF, 0x8086);

    // MMIO smoke: LAPIC ID, IOAPIC select, HPET capabilities.
    let _lapic_id = pc.memory.read_u32(0xFEE0_0020);
    pc.memory.write_u32(0xFEC0_0000, 0x01);
    let _ioapic_ver = pc.memory.read_u32(0xFEC0_0010);

    // HPET only becomes uniquely addressable once A20 is enabled (it differs from the IOAPIC base
    // by bit20). We enabled A20 above, so this should hit the HPET mapping.
    let hpet_caps = pc.memory.read_u64(HPET_MMIO_BASE);
    assert_ne!(hpet_caps, 0);

    // Reset control port 0xCF9 generates a reset event.
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);

    // i8042 reset command (0xFE) also surfaces as a platform reset event.
    pc.io.write_u8(0x64, 0xFE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
}

#[test]
fn pc_platform_exposes_snapshot_devices_via_accessors() {
    let pc = PcPlatform::new(2 * 1024 * 1024);

    // The returned handles must be usable for snapshot adapters (borrow + `IoSnapshot`).
    let _pit_state = pc.pit().borrow().save_state();
    let _rtc_state = pc.rtc().borrow().save_state();
    let _hpet_state = pc.hpet().borrow().save_state();

    // The manual clock is shared across devices; cloning the handle should keep pointing at the
    // platform timebase.
    let clock = pc.clock();
    clock.advance_ns(123);
    assert_eq!(pc.clock().now_ns(), 123);
}
