use aero_devices::pci::{PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices::pci::profile::{HDA_ICH6, SATA_AHCI_ICH9};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::{DenseMemory, GuestMemory, GuestMemoryResult};
use memory::MemoryBus as _;
use std::cell::RefCell;
use std::rc::Rc;

type RecordingWrites = Rc<RefCell<Vec<(u64, Vec<u8>)>>>;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_hda_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = HDA_ICH6.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

fn read_ahci_bar5_base(pc: &mut PcPlatform) -> u64 {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10 + 5 * 4);
    u64::from(bar5 & 0xffff_fff0)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

struct RecordingRam {
    inner: DenseMemory,
    writes: RecordingWrites,
}

impl RecordingRam {
    fn new(size: u64) -> (Self, RecordingWrites) {
        let writes = Rc::new(RefCell::new(Vec::new()));
        let inner = DenseMemory::new(size).unwrap();
        (
            Self {
                inner,
                writes: writes.clone(),
            },
            writes,
        )
    }
}

impl GuestMemory for RecordingRam {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        self.inner.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        self.writes.borrow_mut().push((paddr, src.to_vec()));
        self.inner.write_from(paddr, src)
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        self.inner.get_slice(paddr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        self.inner.get_slice_mut(paddr, len)
    }
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

    // PhysicalMemoryBus issues MMIO reads in chunks up to 8 bytes; ensure the HDA MMIO mapping
    // handles 64-bit reads by splitting into supported access sizes.
    assert_eq!(pc.memory.read_u64(bar0_base + 0x08), 0x0001_0000_0000_0001);
}

#[test]
fn pc_platform_routes_multiple_pci_mmio_bars() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_hda: true,
            enable_ahci: true,
            ..Default::default()
        },
    );

    let hda_bar0_base = read_hda_bar0_base(&mut pc);
    let ahci_bar5_base = read_ahci_bar5_base(&mut pc);
    assert_ne!(hda_bar0_base, 0);
    assert_ne!(ahci_bar5_base, 0);
    assert_ne!(hda_bar0_base, ahci_bar5_base);

    // HDA: bring controller out of reset (GCTL.CRST).
    pc.memory.write_u32(hda_bar0_base + 0x08, 1);
    assert_ne!(pc.memory.read_u32(hda_bar0_base + 0x08) & 1, 0);

    // AHCI: CAP should be readable (not an unmapped all-ones region), and GHC should latch AE.
    assert_ne!(pc.memory.read_u32(ahci_bar5_base), 0xffff_ffff);
    pc.memory.write_u32(ahci_bar5_base + 0x04, 1 << 31);
    assert_ne!(pc.memory.read_u32(ahci_bar5_base + 0x04) & (1 << 31), 0);
}

#[test]
fn pc_platform_gates_hda_mmio_on_pci_command_register() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;
    let bar0_base = read_hda_bar0_base(&mut pc);

    // GCTL should start as 0 (controller held in reset).
    assert_eq!(pc.memory.read_u32(bar0_base + 0x08), 0);

    // Disable PCI memory decoding: MMIO should behave like an unmapped region (reads return 0xFF).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    assert_eq!(pc.memory.read_u32(bar0_base + 0x08), 0xFFFF_FFFF);

    // Writes should be ignored while decoding is disabled.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Re-enable decoding: state should reflect that the write above did not reach the device.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    assert_eq!(pc.memory.read_u32(bar0_base + 0x08), 0);

    // Now writes should take effect.
    pc.memory.write_u32(bar0_base + 0x08, 1);
    assert_ne!(pc.memory.read_u32(bar0_base + 0x08) & 1, 0);
}

#[test]
fn pc_platform_gates_hda_dma_on_pci_bus_master_enable() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;
    let bar0_base = read_hda_bar0_base(&mut pc);

    // Unmask IRQ2 (cascade) and IRQ10 so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(10, false);
    }

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Enable memory decoding but keep bus mastering disabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // Set up CORB/RIRB in guest memory so the controller can DMA a response once enabled.
    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;
    pc.memory.write_u32(corb_base, 0x000f_0000);

    pc.memory.write_u32(bar0_base + 0x40, corb_base as u32); // CORBLBASE
    pc.memory.write_u32(bar0_base + 0x50, rirb_base as u32); // RIRBLBASE
    pc.memory.write_u16(bar0_base + 0x4a, 0x00ff); // CORBRP
    pc.memory.write_u16(bar0_base + 0x58, 0x00ff); // RIRBWP

    // Enable global + controller interrupts (GIE + CIE) and start CORB/RIRB engines.
    pc.memory
        .write_u32(bar0_base + 0x20, 0x8000_0000 | (1 << 30)); // INTCTL
    pc.memory.write_u8(bar0_base + 0x5c, 0x03); // RIRBCTL: RUN + RINTCTL
    pc.memory.write_u8(bar0_base + 0x4c, 0x02); // CORBCTL: RUN
    pc.memory.write_u16(bar0_base + 0x48, 0x0000); // CORBWP

    // With bus mastering disabled, the platform should refuse to process and no DMA should occur.
    pc.process_hda(0);
    assert_eq!(pc.memory.read_u32(rirb_base), 0, "DMA should be gated off");
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Now enable bus mastering and retry; the pending CORB entry should be processed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    pc.process_hda(0);

    assert_eq!(pc.memory.read_u32(rirb_base), 0x1af4_1620);

    pc.poll_pci_intx_lines();
    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ10 should be pending after HDA DMA completion");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 10);
}

#[test]
fn pc_platform_routes_hda_mmio_after_bar0_reprogramming() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;

    let bar0_base = read_hda_bar0_base(&mut pc);
    let new_base = bar0_base + 0x1_0000;
    assert_eq!(new_base % 0x4000, 0);

    // Bring controller out of reset at the original BAR0 base.
    pc.memory.write_u32(bar0_base + 0x08, 1);
    assert_ne!(pc.memory.read_u32(bar0_base + 0x08) & 1, 0);

    // Move BAR0 within the platform's PCI MMIO window.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        new_base as u32,
    );

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u32(bar0_base + 0x08), 0xFFFF_FFFF);

    // New base should decode and preserve controller state.
    assert_ne!(pc.memory.read_u32(new_base + 0x08) & 1, 0);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_intx() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;
    let bar0_base = read_hda_bar0_base(&mut pc);

    // Unmask IRQ2 (cascade) and IRQ10 so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(10, false);
    }

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Allow the device model to DMA from guest memory (CORB/RIRB) while processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Set up CORB/RIRB to raise a CIS interrupt.
    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;
    pc.memory.write_u32(corb_base, 0x000f_0000);

    pc.memory.write_u32(bar0_base + 0x40, corb_base as u32); // CORBLBASE
    pc.memory.write_u32(bar0_base + 0x50, rirb_base as u32); // RIRBLBASE
    pc.memory.write_u16(bar0_base + 0x4a, 0x00ff); // CORBRP
    pc.memory.write_u16(bar0_base + 0x58, 0x00ff); // RIRBWP

    pc.memory
        .write_u32(bar0_base + 0x20, 0x8000_0000 | (1 << 30)); // INTCTL
    pc.memory.write_u8(bar0_base + 0x5c, 0x03); // RIRBCTL
    pc.memory.write_u8(bar0_base + 0x4c, 0x02); // CORBCTL
    pc.memory.write_u16(bar0_base + 0x48, 0x0000); // CORBWP

    pc.process_hda(0);
    assert_eq!(pc.memory.read_u32(rirb_base), 0x1af4_1620);

    // Disable INTx in PCI command register (bit 10), while keeping memory decoding enabled.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0006 | (1 << 10),
    );

    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "INTx should not be delivered when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx; since the HDA interrupt is still pending, it should now be delivered.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ10 should be pending after re-enabling INTx");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 10);
}

#[test]
fn pc_platform_routes_hda_intx_via_pci_intx_router() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;

    // Interrupt Line register should report the router-selected GSI for 00:04.0 INTA#.
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf.bus, bdf.device, bdf.function, 0x3c));
    let int_line = pc.io.read(PCI_CFG_DATA_PORT, 1) as u8;
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

    // Allow HDA CORB/RIRB DMA while processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

#[test]
fn pc_platform_routes_hda_intx_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new_with_hda(2 * 1024 * 1024);
    let bar0_base = read_hda_bar0_base(&mut pc);

    // Switch the platform into APIC mode via IMCR.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route the HDA INTx line to vector 0x60, level-triggered + active-low.
    let vector = 0x60u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let bdf = HDA_ICH6.bdf;
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Allow HDA CORB/RIRB DMA while processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Set up CORB/RIRB and queue one verb so we can generate a CIS interrupt.
    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;
    pc.memory.write_u32(corb_base, 0x000f_0000);

    pc.memory.write_u32(bar0_base + 0x40, corb_base as u32); // CORBLBASE
    pc.memory.write_u32(bar0_base + 0x50, rirb_base as u32); // RIRBLBASE
    pc.memory.write_u16(bar0_base + 0x4a, 0x00ff); // CORBRP
    pc.memory.write_u16(bar0_base + 0x58, 0x00ff); // RIRBWP

    pc.memory
        .write_u32(bar0_base + 0x20, 0x8000_0000 | (1 << 30)); // INTCTL: GIE + CIE
    pc.memory.write_u8(bar0_base + 0x5c, 0x03); // RIRBCTL: RUN + RINTCTL
    pc.memory.write_u8(bar0_base + 0x4c, 0x02); // CORBCTL: RUN
    pc.memory.write_u16(bar0_base + 0x48, 0x0000); // CORBWP

    pc.process_hda(0);
    assert_eq!(pc.memory.read_u32(rirb_base), 0x1af4_1620);

    pc.poll_pci_intx_lines();

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));
    pc.interrupts.borrow_mut().acknowledge(vector as u8);
    pc.interrupts.borrow_mut().eoi(vector as u8);
}

#[test]
fn pc_platform_uses_provided_guest_memory_for_device_dma_writes() {
    let (ram, writes) = RecordingRam::new(2 * 1024 * 1024);
    let mut pc = PcPlatform::new_with_config_and_ram(
        Box::new(ram),
        PcPlatformConfig {
            enable_hda: true,
            ..Default::default()
        },
    );
    let bar0_base = read_hda_bar0_base(&mut pc);

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    let bdf = HDA_ICH6.bdf;
    // Allow the device model to DMA from guest memory (CORB/RIRB).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;

    // Queue a single verb in CORB so processing causes a RIRB write.
    pc.memory.write_u32(corb_base, 0x000f_0000);

    pc.memory.write_u32(bar0_base + 0x40, corb_base as u32); // CORBLBASE
    pc.memory.write_u32(bar0_base + 0x50, rirb_base as u32); // RIRBLBASE
    pc.memory.write_u16(bar0_base + 0x4a, 0x00ff); // CORBRP
    pc.memory.write_u16(bar0_base + 0x58, 0x00ff); // RIRBWP

    pc.memory
        .write_u32(bar0_base + 0x20, 0x8000_0000 | (1 << 30)); // INTCTL
    pc.memory.write_u8(bar0_base + 0x5c, 0x03); // RIRBCTL
    pc.memory.write_u8(bar0_base + 0x4c, 0x02); // CORBCTL
    pc.memory.write_u16(bar0_base + 0x48, 0x0000); // CORBWP

    // Discard any setup writes performed by the test itself. We want to observe the DMA write
    // performed by the device model during processing.
    writes.borrow_mut().clear();

    pc.process_hda(0);

    let expected = 0x1af4_1620u32;
    assert_eq!(pc.memory.read_u32(rirb_base), expected);

    let writes = writes.borrow();
    assert!(
        writes.iter().any(|(paddr, bytes)| {
            *paddr == rirb_base
                && bytes.len() >= 4
                && u32::from_le_bytes(bytes[..4].try_into().expect("checked length")) == expected
        }),
        "expected a DMA write to RIRB to be routed through the provided GuestMemory"
    );
}

#[test]
fn pc_platform_hda_dma_writes_mark_dirty_pages_when_enabled() {
    let mut pc = PcPlatform::new_with_hda_dirty_tracking(2 * 1024 * 1024);
    let bdf = HDA_ICH6.bdf;
    let bar0_base = read_hda_bar0_base(&mut pc);

    // Bring controller out of reset.
    pc.memory.write_u32(bar0_base + 0x08, 1);

    // Allow HDA CORB/RIRB DMA while processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Set up CORB/RIRB in guest memory so the controller can DMA a response.
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

    // Clear dirty tracking for CPU-initiated setup writes. We want to observe only the DMA
    // writes performed by the device model.
    pc.memory.clear_dirty();

    pc.process_hda(0);
    assert_eq!(pc.memory.read_u32(rirb_base), 0x1af4_1620);

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_page = rirb_base / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_page),
        "dirty pages should include the RIRB DMA page (got {dirty:?})"
    );
}
