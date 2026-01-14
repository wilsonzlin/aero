use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::{
    PciBdf, PciInterruptPin, PciResourceAllocatorConfig, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::MemoryBus as _;

const CAP_REG_RTSOFF: u64 = 0x18;
const IMAN_IP: u32 = 1 << 0;
const IMAN_IE: u32 = 1 << 1;

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;

    let mut interrupts = pc.interrupts.borrow_mut();
    interrupts.ioapic_mmio_write(0x00, redtbl_low);
    interrupts.ioapic_mmio_write(0x10, low);
    interrupts.ioapic_mmio_write(0x00, redtbl_high);
    interrupts.ioapic_mmio_write(0x10, high);
}

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_data_port(offset: u8) -> u16 {
    PCI_CFG_DATA_PORT + u16::from(offset & 3)
}

fn read_cfg_u8(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u8 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.read(cfg_data_port(offset), 1) as u8
}

fn read_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u32 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.read(cfg_data_port(offset), 4)
}

fn read_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u16 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.read(cfg_data_port(offset), 2) as u16
}

fn write_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u16) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.write(cfg_data_port(offset), 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u32) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.write(cfg_data_port(offset), 4, value);
}

fn find_capability(pc: &mut PcPlatform, bdf: PciBdf, cap_id: u8) -> Option<u8> {
    let mut offset = read_cfg_u8(pc, bdf, 0x34);
    let mut seen = [false; 256];
    while offset != 0 {
        let idx = offset as usize;
        if idx >= seen.len() || seen[idx] {
            break;
        }
        seen[idx] = true;
        let id = read_cfg_u8(pc, bdf, offset);
        if id == cap_id {
            return Some(offset);
        }
        offset = read_cfg_u8(pc, bdf, offset.wrapping_add(1));
    }
    None
}

fn read_xhci_bar0_raw(pc: &mut PcPlatform) -> u32 {
    let bdf = USB_XHCI_QEMU.bdf;
    read_cfg_u32(pc, bdf, 0x10)
}

fn read_xhci_bar0_base(pc: &mut PcPlatform) -> u64 {
    u64::from(read_xhci_bar0_raw(pc) & 0xffff_fff0)
}

fn xhci_usbcmd_addr(pc: &mut PcPlatform, bar0_base: u64) -> u64 {
    // CAPLENGTH is the low byte at BAR0+0, and the operational register block starts at
    // BAR0+CAPLENGTH.
    let caplength = u64::from(pc.memory.read_u8(bar0_base));
    bar0_base + caplength
}

fn xhci_interrupter0_runtime_addrs(
    pc: &mut PcPlatform,
    bar0_base: u64,
) -> (u64, u64, u64, u64, u64) {
    let rtsoff = u64::from(pc.memory.read_u32(bar0_base + CAP_REG_RTSOFF));
    let rt_base = bar0_base + rtsoff;

    // Runtime registers: interrupter 0 register set starts at RT_BASE + 0x20.
    let iman = rt_base + 0x20;
    let erstsz = rt_base + 0x28;
    let erstba = rt_base + 0x30;
    let erdp = rt_base + 0x38;
    (rt_base, iman, erstsz, erstba, erdp)
}

#[test]
fn pc_platform_exposes_xhci_as_pci_mmio_and_routes_mmio_through_bar0() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );

    assert!(pc.xhci.is_some(), "xHCI device model should be enabled");

    let bdf = USB_XHCI_QEMU.bdf;

    // PCI ID + class code should match the profile.
    let id = read_cfg_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(USB_XHCI_QEMU.vendor_id));
    assert_eq!((id >> 16) & 0xFFFF, u32::from(USB_XHCI_QEMU.device_id));

    let class_rev = read_cfg_u32(&mut pc, bdf, 0x08);
    assert_eq!(class_rev >> 8, USB_XHCI_QEMU.class.as_u32());

    // BIOS POST should allocate BAR0 and enable MEM decoding.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    assert_ne!(
        command & 0x2,
        0,
        "COMMAND.MEM should be enabled by BIOS POST"
    );

    let bar0_base = read_xhci_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    let alloc_cfg = PciResourceAllocatorConfig::default();
    assert!(
        (alloc_cfg.mmio_base..alloc_cfg.mmio_base + alloc_cfg.mmio_size).contains(&bar0_base),
        "BAR0 base {bar0_base:#x} should land in the platform's PCI MMIO window"
    );

    // Smoke MMIO read/write through the decoded BAR.
    let cap = pc.memory.read_u32(bar0_base);
    assert_ne!(cap, 0xffff_ffff, "BAR0 should route to an MMIO handler");

    // Read/then-write-back USBCMD as a minimal MMIO smoke test.
    let usbcmd_addr = xhci_usbcmd_addr(&mut pc, bar0_base);
    let before = pc.memory.read_u32(usbcmd_addr);
    pc.memory.write_u32(usbcmd_addr, before);
    let after = pc.memory.read_u32(usbcmd_addr);
    assert_ne!(
        after, 0xffff_ffff,
        "MMIO writes should not deconfigure BAR routing"
    );
}

#[test]
fn pc_platform_gates_xhci_mmio_on_pci_command_mem_bit() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );
    let bdf = USB_XHCI_QEMU.bdf;

    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    assert_ne!(
        command & 0x2,
        0,
        "xHCI should have MEM decoding enabled by BIOS POST"
    );

    let bar0_base = read_xhci_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    // Use CRCR (command ring control register) as a scratch register. Unlike USBCMD, CRCR does not
    // have write-sensitive bits like HCRST, so it's safe to write test values.
    //
    // Note: xHCI masks CRCR reserved bits 4..=5 to zero (see `sync_command_ring_from_crcr`), so use
    // a marker value with those bits clear to get stable readback.
    let usbcmd_addr = xhci_usbcmd_addr(&mut pc, bar0_base);
    let crcr_addr = usbcmd_addr + 0x18;
    const CRCR_MARKER: u64 = 0xDEAD_BEEF_F00D_CACE;
    pc.memory.write_u64(crcr_addr, CRCR_MARKER);
    assert_eq!(pc.memory.read_u64(crcr_addr), CRCR_MARKER);

    // Disable memory decoding (COMMAND.MEM bit 1): reads float high and writes are ignored.
    write_cfg_u16(&mut pc, bdf, 0x04, command & !0x2);
    assert_eq!(pc.memory.read_u64(crcr_addr), u64::MAX);
    pc.memory.write_u64(crcr_addr, 0);

    // Re-enable decoding: the write above must not have reached the device.
    write_cfg_u16(&mut pc, bdf, 0x04, command);
    assert_eq!(pc.memory.read_u64(crcr_addr), CRCR_MARKER);
}

#[test]
fn pc_platform_routes_xhci_mmio_after_bar_reprogramming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );
    let bdf = USB_XHCI_QEMU.bdf;

    let bar0_raw = read_xhci_bar0_raw(&mut pc);
    let old_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(old_base, 0);

    let usbcmd_off = u64::from(pc.memory.read_u8(old_base));
    let usbcmd_addr_old = old_base + usbcmd_off;
    let crcr_addr_old = usbcmd_addr_old + 0x18;

    // Write a recognizable value via the initial BAR.
    // Bits 4..=5 are masked to zero by the controller model; use a value with those bits already
    // clear so we can assert stable readback.
    const CRCR_MARKER: u64 = 0xA5A5_5A5A_1234_5648;
    pc.memory.write_u64(crcr_addr_old, CRCR_MARKER);
    assert_eq!(pc.memory.read_u64(crcr_addr_old), CRCR_MARKER);

    // Relocate BAR0 within the PCI MMIO window (alignment == BAR size).
    //
    // Avoid placing the BAR exactly at the end of the window; some memory buses can have subtle
    // off-by-one issues at region boundaries. Instead, move to the next aligned slot after the
    // existing allocation.
    let alloc_cfg = PciResourceAllocatorConfig::default();
    let window_end = alloc_cfg.mmio_base + alloc_cfg.mmio_size;
    let bar_size = u64::from(XhciPciDevice::MMIO_BAR_SIZE);

    let new_base = old_base + bar_size;
    assert!(
        new_base + bar_size <= window_end,
        "test relocation base should fit within PCI MMIO window"
    );
    assert_ne!(new_base, old_base);
    assert_eq!(new_base % bar_size, 0);

    // Preserve BAR flags (low bits).
    let flags = bar0_raw & 0xf;
    write_cfg_u32(&mut pc, bdf, 0x10, (new_base as u32) | flags);
    assert_eq!(read_xhci_bar0_base(&mut pc), new_base);

    let usbcmd_addr_new = new_base + usbcmd_off;
    let crcr_addr_new = usbcmd_addr_new + 0x18;

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u64(crcr_addr_old), u64::MAX);

    // New base should decode and preserve device register state.
    assert_eq!(pc.memory.read_u64(crcr_addr_new), CRCR_MARKER);
    pc.memory.write_u64(crcr_addr_new, 0x1111_2222_3333_4444);
    assert_eq!(pc.memory.read_u64(crcr_addr_new), 0x1111_2222_3333_4444);
}

#[test]
fn pc_platform_xhci_intx_asserts_and_clears() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = read_xhci_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    let (rt_base, rt_iman, rt_erstsz, rt_erstba, rt_erdp) =
        xhci_interrupter0_runtime_addrs(&mut pc, bar0_base);
    assert_ne!(rt_base, bar0_base, "RTSOFF should be non-zero");

    let expected_irq = u8::try_from(pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA)).unwrap();

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe xHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(expected_irq, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Program a minimal ERST and enable interrupter 0.
    const ERST_BASE: u64 = 0x10_000;
    const EVENT_RING_BASE: u64 = 0x11_000;

    // ERST[0]: {segment_base, segment_size_trbs=1}
    pc.memory.write_u64(ERST_BASE, EVENT_RING_BASE);
    pc.memory.write_u32(ERST_BASE + 8, 1);
    pc.memory.write_u32(ERST_BASE + 12, 0);

    pc.memory.write_u32(rt_erstsz, 1);
    pc.memory.write_u64(rt_erstba, ERST_BASE);
    pc.memory.write_u64(rt_erdp, EVENT_RING_BASE);

    // IMAN.IE=1
    pc.memory.write_u32(rt_iman, IMAN_IE);

    // Trigger a port status change (device attach).
    let xhci = pc.xhci.as_ref().unwrap().clone();
    xhci.borrow_mut()
        .trigger_port_status_change_event(&mut pc.memory);

    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("xHCI INTx should be pending after a port status change");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, expected_irq);

    // Consume + EOI so subsequent pending checks aren't affected by PIC latching.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the interrupt: IMAN.IP is RW1C. Also advance ERDP to show the event is consumed.
    let iman = pc.memory.read_u32(rt_iman);
    pc.memory.write_u32(rt_iman, iman | IMAN_IP);
    pc.memory.write_u64(rt_erdp, EVENT_RING_BASE + 16);

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_xhci_intx() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );

    let bdf = USB_XHCI_QEMU.bdf;
    let bar0_base = read_xhci_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    let (_rt_base, rt_iman, rt_erstsz, rt_erstba, rt_erdp) =
        xhci_interrupter0_runtime_addrs(&mut pc, bar0_base);

    let expected_irq = u8::try_from(pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA)).unwrap();

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe xHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(expected_irq, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Program ERST + enable interrupter 0 so a port status change can assert IRQ.
    const ERST_BASE: u64 = 0x10_000;
    const EVENT_RING_BASE: u64 = 0x11_000;

    pc.memory.write_u64(ERST_BASE, EVENT_RING_BASE);
    pc.memory.write_u32(ERST_BASE + 8, 1);
    pc.memory.write_u32(ERST_BASE + 12, 0);

    pc.memory.write_u32(rt_erstsz, 1);
    pc.memory.write_u64(rt_erstba, ERST_BASE);
    pc.memory.write_u64(rt_erdp, EVENT_RING_BASE);

    pc.memory.write_u32(rt_iman, IMAN_IE);

    // Cause the device to assert IMAN.IP.
    let xhci = pc.xhci.as_ref().unwrap().clone();
    xhci.borrow_mut()
        .trigger_port_status_change_event(&mut pc.memory);
    assert!(
        xhci.borrow().irq_level(),
        "test expects the xHCI device to have a pending interrupt before COMMAND.INTX_DISABLE gating"
    );

    let command = read_cfg_u16(&mut pc, bdf, 0x04);

    // Disable INTx in PCI command register (bit 10) while leaving memory decode enabled.
    write_cfg_u16(&mut pc, bdf, 0x04, command | (1 << 10));
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "INTx should be suppressed when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx and ensure the asserted line is delivered.
    write_cfg_u16(&mut pc, bdf, 0x04, command & !(1 << 10));
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts
            .borrow()
            .pic()
            .get_pending_vector()
            .and_then(|v| pc.interrupts.borrow().pic().vector_to_irq(v)),
        Some(expected_irq)
    );
}

#[test]
fn pc_platform_xhci_bar0_size_probe_reports_expected_mask() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_xhci: true,
            ..Default::default()
        },
    );
    let bdf = USB_XHCI_QEMU.bdf;

    let bar0_raw = read_xhci_bar0_raw(&mut pc);

    // BAR size probing: write all-ones and read back the size mask.
    write_cfg_u32(&mut pc, bdf, 0x10, 0xFFFF_FFFF);
    let got = read_cfg_u32(&mut pc, bdf, 0x10);
    assert_eq!(got, 0xFFFF_0000);

    // Restore the original BAR so other reads reflect the programmed base.
    write_cfg_u32(&mut pc, bdf, 0x10, bar0_raw);
    assert_eq!(read_xhci_bar0_raw(&mut pc), bar0_raw);
}

#[test]
fn pc_platform_xhci_run_stop_sets_usbsts_eint_and_triggers_intx() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_xhci: true,
            ..Default::default()
        },
    );

    let bdf = USB_XHCI_QEMU.bdf;

    // Locate the MMIO BAR allocated by BIOS POST.
    let bar0_raw = read_cfg_u32(&mut pc, bdf, 0x10);
    let bar0_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Ensure we can observe the routed IRQ via the legacy PIC.
    let expected_irq = u8::try_from(pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA)).unwrap();
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if expected_irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
        interrupts.pic_mut().set_masked(expected_irq, false);
    }

    // Enable Bus Mastering so the device can exercise a small DMA read on the first RUN edge.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    write_cfg_u16(&mut pc, bdf, 0x04, command | 0x0004);

    // Determine CAPLENGTH so we can find the operational register block.
    let cap = pc.memory.read_u32(bar0_base);
    let caplength = (cap & 0xFF) as u64;
    assert_ne!(caplength, 0);

    let usbcmd_addr = bar0_base + caplength;
    let usbsts_addr = usbcmd_addr + 4;
    let crcr_addr = usbcmd_addr + 0x18;

    // Program CRCR to point at RAM so the DMA read is valid.
    const CRCR_BASE: u64 = 0x10_000;
    pc.memory.write_u32(CRCR_BASE, 0x1234_5678);
    pc.memory.write_u64(crcr_addr, CRCR_BASE);

    // Start the controller (USBCMD.RUN=1).
    pc.memory.write_u32(usbcmd_addr, 1);

    // Advance by 1ms so the device model can process the RUN edge and assert an interrupt.
    pc.tick(1_000_000);

    // The device should report an Event Interrupt in USBSTS and assert legacy INTx.
    let usbsts = pc.memory.read_u32(usbsts_addr);
    assert_ne!(
        usbsts & (1 << 3),
        0,
        "USBSTS.EINT should be set after RUN triggers an event interrupt"
    );

    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("xHCI INTx should be pending after RUN triggers an event interrupt");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, expected_irq);

    // Consume + EOI so we can test the deassert path.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the interrupt (USBSTS is RW1C).
    pc.memory.write_u32(usbsts_addr, 1 << 3);
    assert_eq!(pc.memory.read_u32(usbsts_addr) & (1 << 3), 0);

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_routes_xhci_intx_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_xhci: true,
            ..Default::default()
        },
    );

    let bdf = USB_XHCI_QEMU.bdf;

    let bar0_raw = read_cfg_u32(&mut pc, bdf, 0x10);
    let bar0_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Switch to APIC mode via IMCR.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route the xHCI INTx line to vector 0x60, level-triggered + active-low.
    let vector = 0x60u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    // Determine CAPLENGTH so we can locate the operational register block.
    let cap = pc.memory.read_u32(bar0_base);
    let caplength = (cap & 0xFF) as u64;
    assert_ne!(caplength, 0);

    let usbcmd_addr = bar0_base + caplength;
    let usbsts_addr = usbcmd_addr + 4;

    // Enable Bus Mastering so the xHCI model can run its DMA-on-RUN probe and raise the synthetic
    // event interrupt used by these integration tests.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    write_cfg_u16(&mut pc, bdf, 0x04, command | 0x0004);

    // Start the controller. The PC platform tick loop runs xHCI at a 1ms cadence.
    pc.memory.write_u32(usbcmd_addr, 1);
    pc.tick(1_000_000);

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Acknowledge the interrupt (vector in service).
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the controller IRQ and propagate the deassertion before sending EOI.
    pc.memory.write_u32(usbsts_addr, 1 << 3);
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_xhci_msi_triggers_lapic_vector_and_suppresses_intx() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_xhci: true,
            ..Default::default()
        },
    );
    let bdf = USB_XHCI_QEMU.bdf;

    // Locate the MMIO BAR allocated by BIOS POST.
    let bar0_raw = read_cfg_u32(&mut pc, bdf, 0x10);
    let bar0_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Switch into APIC mode so we can observe MSI delivery via the LAPIC using `InterruptController`.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Also route the legacy INTx line to a higher-priority vector so we can detect accidental INTx
    // delivery when MSI is enabled.
    let intx_vector = 0x60u32;
    let low = intx_vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Program and enable the MSI capability (single-vector).
    let msi_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI)
        .expect("xHCI should expose an MSI capability");

    const MSI_VECTOR: u8 = 0x55;
    write_cfg_u32(&mut pc, bdf, msi_cap + 0x04, 0xfee0_0000);
    write_cfg_u32(&mut pc, bdf, msi_cap + 0x08, 0);
    write_cfg_u16(&mut pc, bdf, msi_cap + 0x0c, u16::from(MSI_VECTOR));
    let ctrl = read_cfg_u16(&mut pc, bdf, msi_cap + 0x02);
    write_cfg_u16(&mut pc, bdf, msi_cap + 0x02, ctrl | 0x0001);

    // Determine CAPLENGTH so we can locate the operational register block.
    let cap = pc.memory.read_u32(bar0_base);
    let caplength = (cap & 0xFF) as u64;
    assert_ne!(caplength, 0);

    let usbcmd_addr = bar0_base + caplength;
    let usbsts_addr = usbcmd_addr + 4;

    // Enable Bus Mastering so the xHCI model can run its DMA-on-RUN probe and raise the synthetic
    // event interrupt used by these integration tests.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    write_cfg_u16(&mut pc, bdf, 0x04, command | 0x0004);

    // Start the controller; the device will assert an interrupt on the first RUN edge.
    pc.memory.write_u32(usbcmd_addr, 1);
    pc.tick(1_000_000);

    // Poll INTx after the tick: if MSI delivery accidentally also asserts legacy INTx, the IOAPIC
    // entry above would inject `intx_vector` and preempt the MSI vector.
    pc.poll_pci_intx_lines();

    // The device should report an Event Interrupt in USBSTS and inject the MSI vector into the
    // LAPIC.
    let usbsts = pc.memory.read_u32(usbsts_addr);
    assert_ne!(usbsts & (1 << 3), 0, "USBSTS.EINT should be set after RUN");
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(MSI_VECTOR),
        "MSI delivery should inject the programmed vector and suppress legacy INTx"
    );

    // Acknowledge + EOI and ensure no further interrupts are pending once the device is cleared.
    pc.interrupts.borrow_mut().acknowledge(MSI_VECTOR);
    pc.memory.write_u32(usbsts_addr, 1 << 3);
    pc.poll_pci_intx_lines();
    pc.interrupts.borrow_mut().eoi(MSI_VECTOR);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Ensure a subsequent tick does not re-trigger MSI without another rising edge.
    pc.tick(1_000_000);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}
