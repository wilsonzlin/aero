use aero_devices::pci::profile::{ISA_PIIX3, USB_UHCI_PIIX3};
use aero_pc_platform::PcPlatform;
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::DeviceId;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::*;
use aero_usb::uhci::UhciController;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 4, value);
}

fn read_uhci_bar4_raw(pc: &mut PcPlatform) -> u32 {
    let bdf = USB_UHCI_PIIX3.bdf;
    read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x20)
}

fn read_uhci_bar4_base(pc: &mut PcPlatform) -> u16 {
    (read_uhci_bar4_raw(pc) & 0xffff_fffc) as u16
}

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn pc_platform_exposes_piix3_multifunction_isa_bridge_for_uhci() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Ensure function 0 exists and is marked multi-function so OSes will probe function 2.
    let isa_hdr = read_cfg_u32(
        &mut pc,
        ISA_PIIX3.bdf.bus,
        ISA_PIIX3.bdf.device,
        ISA_PIIX3.bdf.function,
        0x0c,
    );
    let header_type = ((isa_hdr >> 16) & 0xff) as u8;
    assert_ne!(
        header_type & 0x80,
        0,
        "PIIX3 function 0 should be multi-function"
    );
}

#[test]
fn pc_platform_enumerates_uhci_and_assigns_bar4() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(USB_UHCI_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(USB_UHCI_PIIX3.device_id));

    // Class code should be 0x0c0300 (serial bus / USB / UHCI).
    let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!((class >> 8) & 0x00ff_ffff, 0x000c_0300);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    assert_ne!(command & 0x1, 0, "BIOS POST should enable I/O decoding");

    let bar4_raw = read_uhci_bar4_raw(&mut pc);
    let bar4_base = read_uhci_bar4_base(&mut pc);
    assert_ne!(bar4_base, 0, "UHCI BAR4 should be assigned during BIOS POST");
    assert_eq!(bar4_base as u32 % 0x20, 0, "UHCI BAR4 must be 0x20-aligned");
    assert_ne!(bar4_raw & 0x1, 0, "UHCI BAR4 must be an I/O BAR (bit0=1)");

    // BAR size probing should report 0x20 bytes.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x20,
        0xffff_ffff,
    );
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20),
        0xffff_ffe1
    );

    // Restore the original BAR value so subsequent tests can read it back normally.
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20, bar4_raw);
    assert_eq!(read_uhci_bar4_raw(&mut pc), bar4_raw);

    // Smoke test: SOFMOD defaults to 64 and should be writable via the programmed BAR.
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 64);
    pc.io.write(bar4_base + REG_SOFMOD, 1, 12);
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 12);
}

#[test]
fn pc_platform_routes_uhci_io_through_bar4() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // USBCMD defaults to MAXP=1 (64-byte packets).
    let usbcmd = pc.io.read(bar4_base + REG_USBCMD, 2) as u16;
    assert_ne!(usbcmd & USBCMD_MAXP, 0);

    // FRNUM should be readable/writable (masked to 11 bits).
    pc.io.write(bar4_base + REG_FRNUM, 2, 0x1234);
    let frnum = pc.io.read(bar4_base + REG_FRNUM, 2) as u16;
    assert_eq!(frnum, 0x1234 & 0x07ff);

    // Out-of-range reads behave like open bus.
    assert_eq!(pc.io.read(bar4_base + 0x40, 4), 0xffff_ffff);
}

#[test]
fn pc_platform_uhci_tick_advances_frnum_deterministically() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Start the controller running.
    pc.io
        .write(bar4_base + REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));

    // FRNUM starts at 0 and should advance by 1 per millisecond tick.
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 0);

    // Two half-ms ticks should add up to one UHCI frame.
    pc.tick(500_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 0);
    pc.tick(500_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 1);

    // Large deltas should advance multiple frames deterministically.
    pc.tick(10_000_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 11);
}

#[test]
fn uhci_snapshot_roundtrip_restores_regs_and_port_state() {
    struct ZeroMem;

    impl aero_usb::MemoryBus for ZeroMem {
        fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
            buf.fill(0);
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
    }

    let mut ctrl = UhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    ctrl.io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    ctrl.io_write(REG_FRNUM, 2, 0x0100);

    let mut mem = ZeroMem;
    ctrl.tick_1ms(&mut mem);

    let expected_frnum = ctrl.regs().frnum;
    let expected_portsc0 = ctrl.hub().read_portsc(0);

    let state = device_state_from_io_snapshot(DeviceId::USB, &ctrl);
    assert_eq!(state.id, DeviceId::USB);

    // Restore into a new controller with the same host-side topology (device attached to port 0).
    let mut restored = UhciController::new();
    restored
        .hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    restored.hub_mut().force_enable_for_tests(0);

    apply_io_snapshot_to_device(&state, &mut restored).unwrap();

    assert_eq!(restored.regs().frnum, expected_frnum);
    assert_eq!(restored.hub().read_portsc(0), expected_portsc0);
}

#[test]
fn pc_platform_uhci_dma_writes_mark_dirty_pages_when_enabled() {
    let mut pc = PcPlatform::new_with_dirty_tracking(2 * 1024 * 1024);
    let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
    let bdf = USB_UHCI_PIIX3.bdf;

    // Enable Bus Mastering so UHCI DMA reaches guest memory.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    // Propagate the updated PCI command register into the UHCI model; the platform maintains
    // a separate canonical config space for enumeration.
    pc.tick(0);

    const FRAME_LIST_BASE: u64 = 0x3000;
    const TD_ADDR: u64 = 0x4000;
    const LINK_PTR_TERMINATE: u32 = 1;
    const TD_STATUS_ACTIVE: u32 = 1 << 23;
    const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;

    // Frame list entry 0 points at our test TD.
    pc.memory
        .write_physical(FRAME_LIST_BASE, &(TD_ADDR as u32).to_le_bytes());

    // TD layout: link, status/control, token, buffer.
    pc.memory
        .write_physical(TD_ADDR, &LINK_PTR_TERMINATE.to_le_bytes());
    pc.memory
        .write_physical(TD_ADDR + 4, &TD_STATUS_ACTIVE.to_le_bytes());

    // Token: PID=IN (0x69), dev_addr=1 (no device attached), endpoint=0, max_len_field=0x7ff.
    let token = 0x69u32 | (1u32 << 8) | (0x7ffu32 << 21);
    pc.memory.write_physical(TD_ADDR + 8, &token.to_le_bytes());
    pc.memory.write_physical(TD_ADDR + 12, &0u32.to_le_bytes());

    {
        // Program controller registers directly; I/O routing is tested separately.
        let mut dev = uhci.borrow_mut();
        dev.controller_mut()
            .io_write(REG_FLBASEADD, 4, FRAME_LIST_BASE as u32);
        dev.controller_mut().io_write(REG_FRNUM, 2, 0);
        dev.controller_mut()
            .io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    }

    // Clear dirty tracking for CPU-initiated setup writes; we want to observe only the writes the
    // UHCI scheduler performs when completing the TD.
    pc.memory.clear_dirty();

    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_eq!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should be completed (active bit cleared)"
    );
    assert_ne!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should record a CRC/timeout error when no device is attached"
    );

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_page = TD_ADDR / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_page),
        "dirty pages should include TD page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_gates_uhci_dma_on_pci_bus_master_enable() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
    let bdf = USB_UHCI_PIIX3.bdf;

    const FRAME_LIST_BASE: u64 = 0x3000;
    const TD_ADDR: u64 = 0x4000;
    const LINK_PTR_TERMINATE: u32 = 1;
    const TD_STATUS_ACTIVE: u32 = 1 << 23;
    const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;

    // Frame list entry 0 points at our test TD.
    pc.memory
        .write_physical(FRAME_LIST_BASE, &(TD_ADDR as u32).to_le_bytes());

    // TD layout: link, status/control, token, buffer.
    pc.memory
        .write_physical(TD_ADDR, &LINK_PTR_TERMINATE.to_le_bytes());
    pc.memory
        .write_physical(TD_ADDR + 4, &TD_STATUS_ACTIVE.to_le_bytes());

    // Token: PID=IN (0x69), dev_addr=1 (no device attached), endpoint=0, max_len_field=0x7ff.
    let token = 0x69u32 | (1u32 << 8) | (0x7ffu32 << 21);
    pc.memory.write_physical(TD_ADDR + 8, &token.to_le_bytes());
    pc.memory.write_physical(TD_ADDR + 12, &0u32.to_le_bytes());

    // Disable Bus Mastering (but keep I/O decoding enabled) so UHCI DMA is gated off.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    let command_no_bme = command & !(1 << 2);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command_no_bme,
    );
    // Propagate the updated PCI command register into the UHCI model.
    pc.tick(0);

    // Program controller registers directly; I/O routing is tested elsewhere.
    {
        let mut dev = uhci.borrow_mut();
        dev.controller_mut()
            .io_write(REG_FLBASEADD, 4, FRAME_LIST_BASE as u32);
        dev.controller_mut().io_write(REG_FRNUM, 2, 0);
        dev.controller_mut()
            .io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    }

    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_ne!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should remain active when DMA is gated off"
    );
    assert_eq!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should not be updated when DMA is gated off"
    );

    // Reset FRNUM so the next tick uses frame list entry 0 again.
    uhci.borrow_mut().controller_mut().io_write(REG_FRNUM, 2, 0);

    // Enable bus mastering and retry; the pending TD should now be processed.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command_no_bme | (1 << 2),
    );
    pc.tick(0);
    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_eq!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should complete once bus mastering is enabled"
    );
    assert_ne!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should record a CRC/timeout error when no device is attached"
    );
}

#[test]
fn pc_platform_routes_uhci_intx_via_pic_in_legacy_mode() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Unmask IRQ2 (cascade) + IRQ11 so we can observe the UHCI interrupt through the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(11, false);
    }

    // Enable IOC interrupts in the UHCI controller.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));

    // Force a USBINT status bit so the controller asserts its IRQ line.
    {
        let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
        uhci.borrow_mut()
            .controller_mut()
            .set_usbsts_bits(USBSTS_USBINT);
    }

    pc.poll_pci_intx_lines();

    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ11 should be pending after UHCI asserts INTx");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 11);

    // Consume + EOI the interrupt so we can observe deassertion cleanly.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the status bit (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_routes_uhci_intx_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Switch the platform into APIC mode via IMCR (0x22/0x23).
    pc.io.write_u8(0x22, 0x70);
    pc.io.write_u8(0x23, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route GSI11 (PIIX3 UHCI 00:01.2 INTA#) to vector 0x60, level-triggered + active-low.
    let vector = 0x60u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    {
        let mut ints = pc.interrupts.borrow_mut();
        program_ioapic_entry(&mut ints, 11, low, 0);
    }

    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));
    {
        let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
        uhci.borrow_mut()
            .controller_mut()
            .set_usbsts_bits(USBSTS_USBINT);
    }

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Acknowledge the interrupt (vector in service).
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the controller IRQ and propagate the deassertion before sending EOI, so we don't
    // immediately retrigger due to the level-triggered line remaining high.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}
