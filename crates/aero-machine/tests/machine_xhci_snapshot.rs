#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::usb::uhci::regs as uhci_regs;
use aero_devices::usb::xhci::{regs as xhci_regs, XhciController};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;

fn minimal_pc_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal/deterministic for snapshot tests.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    }
}

fn uhci_bar4_base(m: &Machine) -> u64 {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let cfg = pci_cfg
        .bus_mut()
        .device_config(USB_UHCI_PIIX3.bdf)
        .expect("UHCI PCI function should exist");
    cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
}

fn xhci_bar0_base(m: &Machine) -> u64 {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let cfg = pci_cfg
        .bus_mut()
        .device_config(USB_XHCI_QEMU.bdf)
        .expect("xHCI PCI function should exist");
    cfg.bar_range(0).map(|range| range.base).unwrap_or(0)
}

#[test]
fn xhci_snapshot_roundtrips_xhci_state_and_uhci_tick_remainder() {
    let mut cfg = minimal_pc_cfg();
    cfg.enable_uhci = true;
    cfg.enable_xhci = true;

    let mut src = Machine::new(cfg.clone()).unwrap();
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    src.io_write(A20_GATE_PORT, 1, 0x02);

    let bar4_base = uhci_bar4_base(&src);
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    src.io_write(
        base + uhci_regs::REG_USBCMD,
        2,
        u32::from(uhci_regs::USBCMD_RS),
    );

    let bar0_base = xhci_bar0_base(&src);
    assert_ne!(
        bar0_base, 0,
        "xHCI BAR0 base should be assigned by BIOS POST"
    );

    let fr0 = src.io_read(base + uhci_regs::REG_FRNUM, 2) as u16;
    let mf0 = (src.read_physical_u32(bar0_base + xhci_regs::REG_MFINDEX) & 0x3fff) as u16;

    // Advance by 1ms so both controllers change guest-visible state (UHCI FRNUM + xHCI MFINDEX).
    src.tick_platform(1_000_000);
    let fr1 = src.io_read(base + uhci_regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr1, fr0.wrapping_add(1) & 0x07ff);
    let mf1 = (src.read_physical_u32(bar0_base + xhci_regs::REG_MFINDEX) & 0x3fff) as u16;
    assert_eq!(mf1, mf0.wrapping_add(8) & 0x3fff);

    // Advance by half a millisecond; neither controller should step yet, but the machine should
    // retain a sub-ms remainder for snapshot restore.
    src.tick_platform(500_000);
    let fr_mid = src.io_read(base + uhci_regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr_mid, fr1);
    let mf_mid = (src.read_physical_u32(bar0_base + xhci_regs::REG_MFINDEX) & 0x3fff) as u16;
    assert_eq!(mf_mid, mf1);

    // Capture xHCI state immediately before snapshot so any controller-local time fields (e.g.
    // MFINDEX) are compared against the on-snapshot image rather than the initial reset state.
    let xhci_before = src
        .xhci()
        .expect("xHCI should exist when enabled")
        .borrow()
        .controller()
        .save_state();

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.io_write(A20_GATE_PORT, 1, 0x02);
    restored.restore_snapshot_bytes(&snap).unwrap();

    let xhci_after = restored
        .xhci()
        .expect("xHCI should exist when enabled")
        .borrow()
        .controller()
        .save_state();
    // xHCI snapshot encoding is not necessarily stable byte-for-byte under a
    // load+save cycle because `load_state()` may sanitize/normalize reserved bits
    // (e.g. pointer alignment) or clamp values to architecturally valid ranges.
    //
    // Compare against the controller's own normalized view of the snapshot bytes.
    let mut normalized = XhciController::new();
    normalized.load_state(&xhci_before).unwrap();
    let xhci_expected = normalized.save_state();
    assert_eq!(xhci_after, xhci_expected);

    let bar4_base_restored = uhci_bar4_base(&restored);
    assert_ne!(
        bar4_base_restored, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base_restored =
        u16::try_from(bar4_base_restored).expect("UHCI BAR4 base should fit in u16");

    let bar0_base_restored = xhci_bar0_base(&restored);
    assert_ne!(
        bar0_base_restored, 0,
        "xHCI BAR0 base should be assigned by BIOS POST"
    );

    let fr_after_restore = restored.io_read(base_restored + uhci_regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr_after_restore, fr1);
    let mf_after_restore =
        (restored.read_physical_u32(bar0_base_restored + xhci_regs::REG_MFINDEX) & 0x3fff) as u16;
    assert_eq!(mf_after_restore, mf1);

    // Advance by the remaining half-millisecond; if the machine's sub-ms tick remainders are
    // snapshotted, this should now advance UHCI FRNUM and xHCI MFINDEX by one 1ms tick.
    restored.tick_platform(500_000);
    let fr_after_tick = restored.io_read(base_restored + uhci_regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr_after_tick, fr1.wrapping_add(1) & 0x07ff);
    let mf_after_tick =
        (restored.read_physical_u32(bar0_base_restored + xhci_regs::REG_MFINDEX) & 0x3fff) as u16;
    assert_eq!(mf_after_tick, mf1.wrapping_add(8) & 0x3fff);
}

#[test]
fn xhci_restore_errors_when_snapshot_contains_xhci_state_but_xhci_disabled() {
    let mut cfg_with_xhci = minimal_pc_cfg();
    cfg_with_xhci.enable_uhci = true;
    cfg_with_xhci.enable_xhci = true;

    let mut src = Machine::new(cfg_with_xhci.clone()).unwrap();
    let snap = src.take_snapshot_full().unwrap();

    let mut cfg_without_xhci = cfg_with_xhci;
    cfg_without_xhci.enable_xhci = false;

    let mut restored = Machine::new(cfg_without_xhci).unwrap();
    let err = restored
        .restore_snapshot_bytes(&snap)
        .expect_err("restoring xHCI state into a machine without xHCI should error");

    assert!(
        matches!(
            err,
            snapshot::SnapshotError::Corrupt(
                "snapshot contains xHCI state but enable_xhci is false"
            )
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn xhci_enabled_restore_without_xhci_payload_leaves_xhci_at_reset_state() {
    let mut cfg_uhci_only = minimal_pc_cfg();
    cfg_uhci_only.enable_uhci = true;
    cfg_uhci_only.enable_xhci = false;

    let mut src = Machine::new(cfg_uhci_only).unwrap();
    let snap = src.take_snapshot_full().unwrap();

    let mut cfg_both = minimal_pc_cfg();
    cfg_both.enable_uhci = true;
    cfg_both.enable_xhci = true;

    let mut restored = Machine::new(cfg_both).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let xhci_state = restored
        .xhci()
        .expect("xHCI should exist when enabled")
        .borrow()
        .controller()
        .save_state();
    let default_state = XhciController::new().save_state();
    assert_eq!(xhci_state, default_state);
}
