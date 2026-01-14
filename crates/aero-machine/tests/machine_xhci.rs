#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::{
    MsiCapability, PciBdf, PciDevice, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_devices::usb::xhci::{regs, XhciPciDevice};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterruptMode,
};
use aero_snapshot as snapshot;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use pretty_assertions::{assert_eq, assert_ne};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn find_capability(m: &mut Machine, bdf: PciBdf, cap_id: u8) -> Option<u16> {
    let mut ptr = cfg_read(m, bdf, 0x34, 1) as u8;
    for _ in 0..64 {
        if ptr == 0 {
            return None;
        }
        let id = cfg_read(m, bdf, u16::from(ptr), 1) as u8;
        if id == cap_id {
            return Some(u16::from(ptr));
        }
        ptr = cfg_read(m, bdf, u16::from(ptr) + 1, 1) as u8;
    }
    None
}

#[test]
fn xhci_pci_function_exists_and_capability_registers_are_mmio_readable() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the machine minimal/deterministic for this PCI/MMIO probe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    assert!(m.xhci().is_some(), "xHCI device model should be enabled");

    let (class, command, bar0_base) = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_XHCI_QEMU.bdf)
            .expect("xHCI PCI function should exist");
        (
            cfg.class_code(),
            cfg.command(),
            cfg.bar_range(0).map(|range| range.base).unwrap_or(0),
        )
    };

    assert_eq!(class.class, 0x0c, "base class should be Serial Bus");
    assert_eq!(class.subclass, 0x03, "subclass should be USB");
    assert_eq!(class.prog_if, 0x30, "prog-if should be xHCI");
    assert_ne!(
        command & 0x2,
        0,
        "xHCI MEM decoding should be enabled by BIOS POST"
    );
    assert_ne!(
        bar0_base, 0,
        "xHCI BAR0 base should be assigned by BIOS POST"
    );

    let cap = m.read_physical_u32(bar0_base);
    assert_eq!(cap, regs::CAPLENGTH_HCIVERSION);
}

#[test]
fn xhci_mmio_is_gated_on_pci_command_mem_bit() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bar0_base = m
        .pci_bar_base(USB_XHCI_QEMU.bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    let command_before = cfg_read(&mut m, USB_XHCI_QEMU.bdf, 0x04, 2) as u16;
    assert_ne!(command_before & (1 << 1), 0);

    // Disable memory decoding (COMMAND.MEM bit 1).
    cfg_write(
        &mut m,
        USB_XHCI_QEMU.bdf,
        0x04,
        2,
        u32::from(command_before & !(1 << 1)),
    );

    assert_eq!(
        m.read_physical_u32(bar0_base),
        0xffff_ffff,
        "BAR0 reads should float high when COMMAND.MEM is cleared"
    );

    // Restore memory decoding and ensure BAR0 routes again.
    cfg_write(
        &mut m,
        USB_XHCI_QEMU.bdf,
        0x04,
        2,
        u32::from(command_before),
    );
    assert_eq!(m.read_physical_u32(bar0_base), regs::CAPLENGTH_HCIVERSION);
}

#[test]
fn xhci_run_stop_toggles_usbsts_hchalted_bit() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Enable memory decoding + bus mastering so MMIO behaves like a real PCI device.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let usbsts_before = m.read_physical_u32(bar0_base + regs::REG_USBSTS);
    assert_ne!(
        usbsts_before & regs::USBSTS_HCHALTED,
        0,
        "controller should start halted"
    );

    // Set USBCMD.RUN (bit 0) and observe USBSTS.HCHALTED clear.
    let usbcmd_before = m.read_physical_u32(bar0_base + regs::REG_USBCMD);
    m.write_physical_u32(
        bar0_base + regs::REG_USBCMD,
        usbcmd_before | regs::USBCMD_RUN,
    );

    let usbsts_running = m.read_physical_u32(bar0_base + regs::REG_USBSTS);
    assert_eq!(
        usbsts_running & regs::USBSTS_HCHALTED,
        0,
        "USBSTS.HCHALTED should clear when USBCMD.RUN is set"
    );

    // Clear USBCMD.RUN and observe USBSTS.HCHALTED set again.
    let usbcmd_running = m.read_physical_u32(bar0_base + regs::REG_USBCMD);
    m.write_physical_u32(
        bar0_base + regs::REG_USBCMD,
        usbcmd_running & !regs::USBCMD_RUN,
    );

    let usbsts_after = m.read_physical_u32(bar0_base + regs::REG_USBSTS);
    assert_ne!(
        usbsts_after & regs::USBSTS_HCHALTED,
        0,
        "USBSTS.HCHALTED should set when USBCMD.RUN is cleared"
    );
}

#[test]
fn xhci_tick_platform_advances_mfindex_with_sub_ms_remainder() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Ensure MMIO decode is enabled so MFINDEX reads are valid.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let mfindex_before = m.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;

    // Advance by just under 1ms: should not tick yet, but should accumulate remainder.
    m.tick_platform(999_999);
    let mfindex_mid = m.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_mid, mfindex_before);

    // Advance by 1ns: should cross the 1ms boundary and tick exactly once.
    m.tick_platform(1);
    let mfindex_after = m.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_after, mfindex_before.wrapping_add(8) & 0x3fff);
}

#[test]
fn xhci_command_ring_progress_is_gated_by_pci_bme() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Ensure MMIO decode is enabled.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        0x04,
        2,
        u32::from((cmd | (1 << 1)) & !(1 << 2)),
    );

    // Guest ring memory.
    let cmd_ring = 0x10_000u64; // 64B-aligned
    let erstba = 0x20_000u64; // 64B-aligned
    let event_ring = 0x30_000u64; // 16B-aligned

    // Zero the first event TRB so we can detect when it becomes written.
    m.write_physical(event_ring, &[0u8; TRB_LEN]);

    // ERST entry 0: segment base + size (in TRBs).
    m.write_physical_u64(erstba, event_ring);
    m.write_physical_u32(erstba + 8, 256);
    m.write_physical_u32(erstba + 12, 0);

    // Configure interrupter 0 event ring (MMIO writes do not require DMA).
    m.write_physical_u32(bar0_base + regs::REG_INTR0_ERSTSZ, 1);
    m.write_physical_u64(bar0_base + regs::REG_INTR0_ERSTBA_LO, erstba);
    m.write_physical_u64(bar0_base + regs::REG_INTR0_ERDP_LO, event_ring);
    m.write_physical_u32(bar0_base + regs::REG_INTR0_IMAN, regs::IMAN_IE);

    // Program CRCR + start controller (RUN=1).
    m.write_physical_u64(bar0_base + regs::REG_CRCR_LO, cmd_ring | 1);
    m.write_physical_u32(bar0_base + regs::REG_USBCMD, regs::USBCMD_RUN);

    // Write a single No-Op command TRB (cycle=1) then a cycle-mismatch stop marker.
    let mut trb = Trb::new(0, 0, 0);
    trb.set_trb_type(TrbType::NoOpCommand);
    trb.set_cycle(true);
    m.write_physical(cmd_ring, &trb.to_bytes());

    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.set_cycle(false);
    m.write_physical(cmd_ring + TRB_LEN as u64, &stop.to_bytes());

    // Ring doorbell 0 (Command Ring).
    m.write_physical_u32(bar0_base + u64::from(regs::DBOFF_VALUE), 0);

    // With BME disabled, ticks should not process the command ring or write an event.
    m.tick_platform(10_000_000); // 10ms
    let before = m.read_physical_bytes(event_ring, TRB_LEN);
    assert_eq!(
        before,
        vec![0u8; TRB_LEN],
        "expected event ring to remain untouched while COMMAND.BME=0"
    );

    // Enable bus mastering (BME) and tick again; command completion should be written.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));
    m.tick_platform(10_000_000); // 10ms

    let after = m.read_physical_bytes(event_ring, TRB_LEN);
    let evt = Trb::from_bytes(after.try_into().unwrap());
    assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt.parameter & !0x0f, cmd_ring);
}

#[test]
fn xhci_msi_triggers_lapic_vector_and_suppresses_intx() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Locate and program the MSI capability: dest = BSP (APIC ID 0), vector = 0x65.
    let msi_cap =
        find_capability(&mut m, bdf, PCI_CAP_ID_MSI).expect("xHCI should expose MSI capability");
    let base = msi_cap;

    let vector: u8 = 0x65;
    cfg_write(&mut m, bdf, base + 0x04, 4, 0xfee0_0000);
    cfg_write(&mut m, bdf, base + 0x08, 4, 0);
    cfg_write(&mut m, bdf, base + 0x0c, 2, u32::from(vector));
    cfg_write(&mut m, bdf, base + 0x10, 4, 0); // unmask

    let ctrl = cfg_read(&mut m, bdf, base + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, base + 0x02, 2, u32::from(ctrl | 1)); // MSI enable

    // The machine owns the canonical config space, so tick once to mirror MSI state into the xHCI
    // model before triggering an interrupt.
    m.tick_platform(1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        xhci.borrow().irq_level(),
        false,
        "xHCI INTx should be suppressed while MSI is active"
    );
}

#[test]
fn xhci_msi_masked_interrupt_sets_pending_and_redelivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Locate and program the MSI capability, starting with the vector masked.
    let base =
        find_capability(&mut m, bdf, PCI_CAP_ID_MSI).expect("xHCI should expose MSI capability");
    let vector: u8 = 0x65;
    cfg_write(&mut m, bdf, base + 0x04, 4, 0xfee0_0000);
    cfg_write(&mut m, bdf, base + 0x08, 4, 0);
    cfg_write(&mut m, bdf, base + 0x0c, 2, u32::from(vector));
    cfg_write(&mut m, bdf, base + 0x10, 4, 1); // mask

    let ctrl = cfg_read(&mut m, bdf, base + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, base + 0x02, 2, u32::from(ctrl | 1)); // MSI enable

    // Mirror the canonical MSI state into the xHCI model before raising interrupts.
    m.tick_platform(1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    // MSI is masked; delivery should be suppressed.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "masked MSI should set the pending bit in the device model"
    );

    // Unmask MSI in the canonical config space and tick so the machine mirrors state into the
    // device model. This must not clobber device-managed MSI pending bits.
    cfg_write(&mut m, bdf, base + 0x10, 4, 0); // unmask
    m.tick_platform(1);

    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "canonical PCI config sync must not clear device-managed MSI pending bits"
    );

    // Re-drive the interrupt condition; the device model should re-trigger MSI due to the pending
    // bit even though there's no new rising edge.
    xhci.borrow_mut().raise_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
}

#[test]
fn xhci_msi_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Enable MSI, but leave the message address unprogrammed/invalid.
    let base =
        find_capability(&mut m, bdf, PCI_CAP_ID_MSI).expect("xHCI should expose MSI capability");
    let vector: u8 = 0x67;

    // Address left as 0: invalid xAPIC MSI address.
    cfg_write(&mut m, bdf, base + 0x04, 4, 0);
    cfg_write(&mut m, bdf, base + 0x08, 4, 0);
    cfg_write(&mut m, bdf, base + 0x0c, 2, u32::from(vector));
    cfg_write(&mut m, bdf, base + 0x10, 4, 0); // unmask

    let ctrl = cfg_read(&mut m, bdf, base + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "test requires per-vector masking support"
    );
    let pending_off = if is_64bit { base + 0x14 } else { base + 0x10 };
    cfg_write(&mut m, bdf, base + 0x02, 2, u32::from(ctrl | 1)); // MSI enable

    // Mirror canonical MSI state into the device model.
    m.tick_platform(0);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();
    m.poll_pci_intx_lines();

    // MSI delivery is blocked by the invalid address; the device should latch its pending bit and
    // must not fall back to INTx.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI is active"
    );
    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "unprogrammed MSI address should set the pending bit in the device model"
    );

    // Clear the interrupt condition before completing MSI programming so delivery relies solely on
    // the pending bit.
    xhci.borrow_mut().clear_event_interrupt();
    m.poll_pci_intx_lines();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Now program a valid MSI address; the next service step should observe the pending bit and
    // deliver without requiring a new rising edge.
    cfg_write(&mut m, bdf, base + 0x04, 4, 0xfee0_0000);
    cfg_write(&mut m, bdf, base + 0x08, 4, 0);
    m.tick_platform(0);

    // Ensure canonical PCI config sync does not clear the device-managed pending bit.
    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "canonical PCI config sync must not clear device-managed MSI pending bits"
    );
    assert_ne!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to be guest-visible via canonical PCI config space reads"
    );

    xhci.borrow_mut().clear_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );

    // Sync once more so the canonical PCI config space reflects the pending-bit clear.
    m.tick_platform(0);
    assert_eq!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}

#[test]
fn xhci_msix_triggers_lapic_vector_and_suppresses_intx() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table offset.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let table_bir = (table & 0x7) as u8;
    let table_offset = table & !0x7;
    assert_eq!(table_bir, 0, "xHCI MSI-X table should be in BAR0");

    // Program MSI-X table entry 0: dest = BSP (APIC ID 0), vector = 0x66.
    let vector: u8 = 0x66;
    let entry0 = bar0_base + u64::from(table_offset);
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x04, 0);
    m.write_physical_u32(entry0 + 0x08, u32::from(vector));
    m.write_physical_u32(entry0 + 0x0c, 0); // unmasked

    // Enable MSI-X (control bit 15).
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl | (1 << 15)));

    // The machine owns the canonical config space, so tick once to mirror MSI-X state into the
    // xHCI model before triggering an interrupt.
    m.tick_platform(1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active"
    );
}

#[test]
fn xhci_msix_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!((table & 0x7) as u8, 0, "xHCI MSI-X table should be in BAR0");
    assert_eq!((pba & 0x7) as u8, 0, "xHCI MSI-X PBA should be in BAR0");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0 with an invalid address but a valid vector.
    let vector: u8 = 0x6b;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0);
    m.write_physical_u32(entry0 + 0x04, 0);
    m.write_physical_u32(entry0 + 0x08, u32::from(vector));
    m.write_physical_u32(entry0 + 0x0c, 0); // unmasked

    // Enable MSI-X (control bit 15) and tick once so the runtime xHCI device mirrors it.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl | (1 << 15)));
    m.tick_platform(1);

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    // Delivery is blocked by the invalid table entry address; the device must set the PBA pending
    // bit instead.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set when the table entry address is invalid"
    );

    // Clear the interrupt condition so delivery relies solely on the pending bit.
    xhci.borrow_mut().clear_event_interrupt();
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    // Program a valid MSI-X message address; the xHCI MMIO handler services pending MSI-X vectors
    // on table writes, so delivery should occur without reasserting the interrupt condition.
    m.write_physical_u32(entry0, 0xfee0_0000);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after delivery"
    );
}

#[test]
fn xhci_tick_platform_zero_syncs_msix_enable_state_into_device_model() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let xhci = m.xhci().expect("xHCI should be enabled");

    // Sanity: runtime MSI-X starts disabled.
    assert!(
        xhci.borrow()
            .config()
            .capability::<aero_devices::pci::MsixCapability>()
            .is_some_and(|msix| !msix.enabled()),
        "expected runtime MSI-X to start disabled"
    );

    // Enable MSI-X + Function Mask in canonical PCI config space.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose MSI-X capability in PCI config space");
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // `tick_platform(0)` must still sync the canonical PCI config space into the runtime xHCI
    // device model (without advancing time / ticking devices).
    m.tick_platform(0);

    let (enabled, masked) = {
        let dev = xhci.borrow();
        let msix = dev
            .config()
            .capability::<aero_devices::pci::MsixCapability>()
            .expect("xHCI should have MSI-X capability");
        (msix.enabled(), msix.function_masked())
    };

    assert!(
        enabled,
        "expected MSI-X enable bit to sync via tick_platform(0)"
    );
    assert!(
        masked,
        "expected MSI-X Function Mask bit to sync via tick_platform(0)"
    );
}

#[test]
fn xhci_msix_function_mask_defers_delivery_until_unmasked() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!((table & 0x7) as u8, 0, "xHCI MSI-X table should be in BAR0");
    assert_eq!((pba & 0x7) as u8, 0, "xHCI MSI-X PBA should be in BAR0");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0: dest = BSP (APIC ID 0), vector = 0x67.
    let vector: u8 = 0x67;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x04, 0);
    m.write_physical_u32(entry0 + 0x08, u32::from(vector));
    m.write_physical_u32(entry0 + 0x0c, 0); // unmasked

    // Enable MSI-X (control bit 15) and set Function Mask (bit 14).
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // The machine owns the canonical config space, so tick once to mirror MSI-X state into the xHCI
    // model before triggering an interrupt.
    m.tick_platform(1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    // MSI-X Function Mask should suppress delivery without falling back to INTx. The masked vector
    // must be latched as pending in the MSI-X PBA.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if masked)"
    );

    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear Function Mask and tick the machine so xHCI services interrupts again. The pending vector
    // should be delivered and the PBA bit cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.tick_platform(1_000_000);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after unmask + delivery"
    );
}

#[test]
fn xhci_msix_vector_mask_delivers_pending_after_unmask_even_when_interrupt_cleared() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!((table & 0x7) as u8, 0, "xHCI MSI-X table should be in BAR0");
    assert_eq!((pba & 0x7) as u8, 0, "xHCI MSI-X PBA should be in BAR0");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0 and keep it masked.
    let vector: u8 = 0x68;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x04, 0);
    m.write_physical_u32(entry0 + 0x08, u32::from(vector));
    m.write_physical_u32(entry0 + 0x0c, 1); // entry masked

    // Enable MSI-X (control bit 15) and tick once so the runtime xHCI device mirrors it.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl | (1 << 15)));
    m.tick_platform(1);

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    // Masked entry should suppress delivery but set the MSI-X PBA pending bit.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while entry is masked"
    );

    // Clear the interrupt condition before unmasking. Pending delivery should still occur once the
    // entry becomes unmasked.
    xhci.borrow_mut().clear_event_interrupt();

    m.write_physical_u32(entry0 + 0x0c, 0); // unmask entry

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after unmask + delivery"
    );
}

#[test]
fn xhci_msix_function_mask_delivers_pending_after_unmask_even_when_interrupt_cleared() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the test focused on PCI + xHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = USB_XHCI_QEMU.bdf;

    // Enable BAR0 MMIO decode + bus mastering.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = m.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut m, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!((table & 0x7) as u8, 0, "xHCI MSI-X table should be in BAR0");
    assert_eq!((pba & 0x7) as u8, 0, "xHCI MSI-X PBA should be in BAR0");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0 (unmasked): dest = BSP (APIC ID 0), vector = 0x69.
    let vector: u8 = 0x69;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x04, 0);
    m.write_physical_u32(entry0 + 0x08, u32::from(vector));
    m.write_physical_u32(entry0 + 0x0c, 0);

    // Enable MSI-X (control bit 15) and set Function Mask (bit 14).
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // Tick once so the runtime xHCI device mirrors MSI-X state.
    m.tick_platform(1);

    let xhci = m.xhci().expect("xHCI should be enabled");
    xhci.borrow_mut().raise_event_interrupt();

    // Function mask should suppress delivery but set PBA pending bit.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear the interrupt condition before unmasking. Pending delivery should still occur once the
    // function mask is cleared (mirrored into the runtime device on the next tick).
    xhci.borrow_mut().clear_event_interrupt();

    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.tick_platform(1_000_000);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after function unmask + delivery"
    );
}

#[test]
fn xhci_intx_level_is_routed_and_gated_by_command_intx_disable() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on PCI INTx routing.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = USB_XHCI_QEMU.bdf;
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Ensure INTx is enabled in the PCI command register (bit 10 clear).
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command & !(1 << 10)));

    let xhci = m.xhci().expect("xhci enabled");
    xhci.borrow_mut().raise_event_interrupt();
    assert!(xhci.borrow().irq_level(), "xHCI should assert legacy INTx");

    // Polling should drive the xHCI INTx level into the platform interrupt controller.
    m.poll_pci_intx_lines();
    assert!(interrupts.borrow().gsi_level(gsi));

    // Disable INTx in the guest-visible PCI command register.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command | (1 << 10)));

    m.poll_pci_intx_lines();
    assert!(!interrupts.borrow().gsi_level(gsi));
}

struct LegacyXhciUsbSnapshotSource {
    machine: Machine,
}

impl snapshot::SnapshotSource for LegacyXhciUsbSnapshotSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotSource::snapshot_meta(&mut self.machine)
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::SnapshotSource::cpu_state(&self.machine)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::SnapshotSource::mmu_state(&self.machine)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        let mut devices = snapshot::SnapshotSource::device_states(&self.machine);
        let Some(pos) = devices
            .iter()
            .position(|device| device.id == snapshot::DeviceId::USB)
        else {
            return devices;
        };
        let Some(xhci) = self.machine.xhci() else {
            return devices;
        };

        let version = <XhciPciDevice as IoSnapshot>::DEVICE_VERSION;
        devices[pos] = snapshot::DeviceState {
            id: snapshot::DeviceId::USB,
            version: version.major,
            flags: version.minor,
            data: xhci.borrow().save_state(),
        };
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::SnapshotSource::disk_overlays(&self.machine)
    }

    fn ram_len(&self) -> usize {
        snapshot::SnapshotSource::ram_len(&self.machine)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        snapshot::SnapshotSource::read_ram(&self.machine, offset, buf)
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        snapshot::SnapshotSource::take_dirty_pages(&mut self.machine)
    }
}

struct LegacyXhciControllerUsbSnapshotSource {
    machine: Machine,
}

impl snapshot::SnapshotSource for LegacyXhciControllerUsbSnapshotSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotSource::snapshot_meta(&mut self.machine)
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::SnapshotSource::cpu_state(&self.machine)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::SnapshotSource::mmu_state(&self.machine)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        let mut devices = snapshot::SnapshotSource::device_states(&self.machine);
        let Some(pos) = devices
            .iter()
            .position(|device| device.id == snapshot::DeviceId::USB)
        else {
            return devices;
        };
        let Some(xhci) = self.machine.xhci() else {
            return devices;
        };

        let version = <aero_usb::xhci::XhciController as IoSnapshot>::DEVICE_VERSION;
        devices[pos] = snapshot::DeviceState {
            id: snapshot::DeviceId::USB,
            version: version.major,
            flags: version.minor,
            data: xhci.borrow().controller().save_state(),
        };
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::SnapshotSource::disk_overlays(&self.machine)
    }

    fn ram_len(&self) -> usize {
        snapshot::SnapshotSource::ram_len(&self.machine)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        snapshot::SnapshotSource::read_ram(&self.machine, offset, buf)
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        snapshot::SnapshotSource::take_dirty_pages(&mut self.machine)
    }
}

#[test]
fn xhci_restore_accepts_legacy_deviceid_usb_payload_without_machine_remainder() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = LegacyXhciUsbSnapshotSource {
        machine: Machine::new(cfg.clone()).unwrap(),
    };
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    src.machine.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = src
        .machine
        .pci_bar_base(bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Ensure MMIO decode is enabled so MFINDEX reads are valid.
    let cmd = cfg_read(&mut src.machine, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut src.machine,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2)),
    );

    let mfindex_before = src.machine.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    // Advance by 1.5ms so the xHCI device ticks once and the machine accumulates a sub-ms remainder.
    src.machine.tick_platform(1_500_000);
    let mfindex_snapshot = src.machine.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_snapshot, mfindex_before.wrapping_add(8) & 0x3fff);

    // Ensure our snapshot source really emitted the legacy payload (XHCP), not the canonical `USBC`
    // wrapper.
    let usb_state = snapshot::SnapshotSource::device_states(&src)
        .into_iter()
        .find(|d| d.id == snapshot::DeviceId::USB)
        .expect("USB device state should exist");
    assert_eq!(
        usb_state.data.get(8..12),
        Some(b"XHCP".as_slice()),
        "legacy snapshot should store xHCI PCI snapshot directly under DeviceId::USB"
    );

    let mut bytes = std::io::Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut bytes, &mut src, snapshot::SaveOptions::default()).unwrap();
    let bytes = bytes.into_inner();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&bytes).unwrap();
    restored.io_write(A20_GATE_PORT, 1, 0x02);

    let bar0_base_restored = restored
        .pci_bar_base(bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base_restored, 0);

    let mfindex_after_restore =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_after_restore, mfindex_snapshot);

    // Legacy snapshots did not include the machine's sub-ms tick remainder; we should start from a
    // deterministic default of 0 on restore.
    restored.tick_platform(500_000);
    let mfindex_after_half_ms =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_after_half_ms, mfindex_snapshot);

    restored.tick_platform(500_000);
    let mfindex_after_full_ms =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(
        mfindex_after_full_ms,
        mfindex_snapshot.wrapping_add(8) & 0x3fff
    );
}

#[test]
fn xhci_tick_remainder_roundtrips_through_snapshot_restore() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the machine minimal/deterministic for this timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    src.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    // Ensure MMIO decode is enabled so MFINDEX reads are valid.
    let cmd = cfg_read(&mut src, bdf, 0x04, 2) as u16;
    cfg_write(&mut src, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    let bar0_base = src.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(
        bar0_base, 0,
        "xHCI BAR0 base should be assigned by BIOS POST"
    );

    let before = src.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;

    // Advance by half a millisecond; the machine should not tick the controller yet.
    src.tick_platform(500_000);
    let mid = src.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mid, before);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    restored.io_write(A20_GATE_PORT, 1, 0x02);

    // Ensure MMIO decode is enabled so MFINDEX reads are valid.
    let cmd = cfg_read(&mut restored, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut restored,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2)),
    );

    let bar0_base_restored = restored
        .pci_bar_base(bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(
        bar0_base_restored, 0,
        "xHCI BAR0 base should be assigned by BIOS POST"
    );

    let after_restore = restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(after_restore, before);

    // Advance by the remaining half-millisecond; if the machine's xHCI tick remainder is included
    // in snapshots, this should now advance MFINDEX by 8 microframes.
    restored.tick_platform(500_000);
    let after_tick = restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(after_tick, before.wrapping_add(8) & 0x3fff);
}

#[test]
fn xhci_restore_accepts_legacy_deviceid_usb_controller_payload_without_machine_remainder() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = LegacyXhciControllerUsbSnapshotSource {
        machine: Machine::new(cfg.clone()).unwrap(),
    };
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    src.machine.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = src
        .machine
        .pci_bar_base(bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Ensure MMIO decode is enabled so MFINDEX reads are valid.
    let cmd = cfg_read(&mut src.machine, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut src.machine,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2)),
    );

    let mfindex_before = src.machine.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    // Advance by 1.5ms so the xHCI device ticks once and the machine accumulates a sub-ms remainder.
    src.machine.tick_platform(1_500_000);
    let mfindex_snapshot = src.machine.read_physical_u32(bar0_base + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_snapshot, mfindex_before.wrapping_add(8) & 0x3fff);

    // Ensure our snapshot source really emitted the legacy payload (`XHCI` controller), not the
    // canonical `USBC` wrapper.
    let usb_state = snapshot::SnapshotSource::device_states(&src)
        .into_iter()
        .find(|d| d.id == snapshot::DeviceId::USB)
        .expect("USB device state should exist");
    assert_eq!(
        usb_state.data.get(8..12),
        Some(b"XHCI".as_slice()),
        "legacy snapshot should store xHCI controller snapshot directly under DeviceId::USB"
    );

    let mut bytes = std::io::Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut bytes, &mut src, snapshot::SaveOptions::default()).unwrap();
    let bytes = bytes.into_inner();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&bytes).unwrap();
    restored.io_write(A20_GATE_PORT, 1, 0x02);

    let bar0_base_restored = restored
        .pci_bar_base(bdf, 0)
        .expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base_restored, 0);

    let mfindex_after_restore =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_after_restore, mfindex_snapshot);

    // Legacy snapshots did not include the machine's sub-ms tick remainder; we should start from a
    // deterministic default of 0 on restore.
    restored.tick_platform(500_000);
    let mfindex_after_half_ms =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(mfindex_after_half_ms, mfindex_snapshot);

    restored.tick_platform(500_000);
    let mfindex_after_full_ms =
        restored.read_physical_u32(bar0_base_restored + regs::REG_MFINDEX) & 0x3fff;
    assert_eq!(
        mfindex_after_full_ms,
        mfindex_snapshot.wrapping_add(8) & 0x3fff
    );
}
