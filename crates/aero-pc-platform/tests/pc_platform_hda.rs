use aero_devices::pci::profile::HDA_ICH6;
use aero_pc_platform::PcPlatform;
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

fn read_hda_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = HDA_ICH6.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

#[test]
fn pc_platform_enumerates_hda_and_assigns_bar0() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(HDA_ICH6.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(HDA_ICH6.device_id));

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    let bar0_base = read_hda_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");
    assert_eq!(bar0_base % (0x4000u64), 0);
}

#[test]
fn pc_platform_routes_hda_mmio_through_bar0() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bar0_base = read_hda_bar0_base(&mut pc);

    // GCTL.CRST (offset 0x08) brings the controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);
    let gctl = pc.memory.read_u32(bar0_base + 0x08);
    assert_ne!(gctl & 1, 0);
}

#[test]
fn pc_platform_routes_hda_intx_via_pci_intx_router() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;

    // Interrupt Line register should report the router-selected GSI for 00:04.0 INTA#.
    pc.io
        .write(0xCF8, 4, cfg_addr(bdf.bus, bdf.device, bdf.function, 0x3c));
    let int_line = pc.io.read(0xCFC, 1) as u8;
    assert_eq!(
        int_line, 10,
        "default PciIntxRouterConfig routes INTA# for device 4 to GSI10"
    );

    let bar0_base = read_hda_bar0_base(&mut pc);

    // Unmask IRQ2 (cascade) and IRQ10 so we can observe the asserted INTx line via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(10, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Set up CORB/RIRB in guest memory so the controller can raise a CIS interrupt on response.
    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;

    // Command: root (NID 0), GET_PARAMETER 0xF00 payload 0 => codec vendor id.
    pc.memory.write_u32(corb_base, 0x000f_0000);

    pc.memory.write_u32(bar0_base + 0x40, corb_base as u32); // CORBLBASE
    pc.memory.write_u32(bar0_base + 0x50, rirb_base as u32); // RIRBLBASE

    // First command/response at entry 0.
    pc.memory.write_u16(bar0_base + 0x4a, 0x00ff); // CORBRP
    pc.memory.write_u16(bar0_base + 0x58, 0x00ff); // RIRBWP

    // Enable global + controller interrupts (GIE + CIE).
    pc.memory
        .write_u32(bar0_base + 0x20, 0x8000_0000 | (1 << 30)); // INTCTL
                                                               // Enable CORB/RIRB DMA engines and response interrupts.
    pc.memory.write_u8(bar0_base + 0x5c, 0x03); // RIRBCTL: RUN + RINTCTL
    pc.memory.write_u8(bar0_base + 0x4c, 0x02); // CORBCTL: RUN

    // Publish the command by advancing CORBWP.
    pc.memory.write_u16(bar0_base + 0x48, 0x0000);

    // Poll device model to process CORB and generate the interrupt.
    pc.process_hda(0);
    let resp = pc.memory.read_u32(rirb_base);
    assert_eq!(resp, 0x1af4_1620);

    // Propagate the HDA IRQ line into the platform interrupt controller via INTx routing.
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ10 should be pending after INTx routing");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 10);
}
