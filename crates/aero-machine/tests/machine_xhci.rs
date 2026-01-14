#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices::usb::xhci::regs;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterruptMode,
};
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
    cfg_write(&mut m, bdf, base + 0x14, 4, 0); // clear pending

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
