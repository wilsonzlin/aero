use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciConfigSpace, PciDevice, PciInterruptPin, PciResourceAllocatorConfig,
    PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::MemoryBus as _;
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

// Test-only xHCI-like PCI function (BAR0 MMIO + interrupter 0 runtime registers).
//
// These tests validate the PC platform's PCI MMIO window routing, BAR relocation behavior, PCI
// COMMAND gating (MEM bit), and INTx delivery via the PIC. The MMIO register model is minimal and
// only implements what these tests need.

// Pick a device number that is not used by the built-in PC platform devices or the canonical PCI
// profiles (e.g. RTL8139 is 00:06.0). This prevents future platform expansions from accidentally
// colliding with this test-injected function.
const XHCI_BDF: PciBdf = PciBdf::new(0, 0x1e, 0);
const XHCI_BAR_INDEX: u8 = 0;
// Typical xHCI controllers expose a 64KiB BAR0 MMIO window.
const XHCI_BAR_SIZE: u32 = 0x1_0000;

// Keep the BAR within the platform's default PCI MMIO window (0xE000_0000..0xF000_0000) and away
// from other devices that BIOS POST might have allocated near the start of the window.
const XHCI_BAR_BASE: u64 = 0xE200_0000;
const XHCI_BAR_BASE_RELOC: u64 = XHCI_BAR_BASE + XHCI_BAR_SIZE as u64;

// xHCI-ish register layout constants (subset).
const CAPLENGTH: u8 = 0x20;
const CAP_REG_CAPLENGTH_HCIVERSION: u64 = 0x00;
const CAP_REG_RTSOFF: u64 = 0x18;

const OP_REG_USBCMD: u64 = CAPLENGTH as u64 + 0x00;

// Runtime registers at RTSOFF; interrupter 0 register set at +0x20.
const RTSOFF: u32 = 0x2000;
const RT_IMAN: u64 = RTSOFF as u64 + 0x20;
const RT_ERSTSZ: u64 = RTSOFF as u64 + 0x28;
const RT_ERSTBA: u64 = RTSOFF as u64 + 0x30;
const RT_ERSTBA_HI: u64 = RT_ERSTBA + 4;
const RT_ERDP: u64 = RTSOFF as u64 + 0x38;
const RT_ERDP_HI: u64 = RT_ERDP + 4;

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

fn read_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    let port = PCI_CFG_DATA_PORT + u16::from(offset & 3);
    pc.io.read(port, 2) as u16
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

struct TestXhciPciConfigDevice {
    cfg: PciConfigSpace,
}

impl TestXhciPciConfigDevice {
    fn new() -> Self {
        let mut cfg = PciConfigSpace::new(0x1234, 0x1111);
        cfg.set_class_code(0x0c, 0x03, 0x30, 0); // serial bus / USB / xHCI
        cfg.set_bar_definition(
            XHCI_BAR_INDEX,
            PciBarDefinition::Mmio32 {
                size: XHCI_BAR_SIZE,
                prefetchable: false,
            },
        );
        Self { cfg }
    }
}

impl PciDevice for TestXhciPciConfigDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[derive(Debug, Default)]
struct TestXhciMmio {
    usbcmd: u32,

    iman: u32,
    erstsz: u32,
    erstba: u64,
    erdp: u64,

    // Controller-produced pointer within the event ring segment.
    event_enqueue: u64,
}

impl TestXhciMmio {
    fn irq_level(&self) -> bool {
        (self.iman & (IMAN_IP | IMAN_IE)) == (IMAN_IP | IMAN_IE)
    }

    fn attach_device(&mut self, mem: &mut dyn memory::MemoryBus) {
        // Minimal modeling: treat a device attach as a single Port Status Change Event delivered
        // through interrupter 0.
        if (self.iman & IMAN_IE) == 0 {
            return;
        }
        if self.erstsz == 0 || self.erstba == 0 {
            return;
        }

        // Read the first ERST entry to find the event ring segment base + size.
        let seg_base = mem.read_u64(self.erstba) & !0x3f;
        let seg_size_trbs = mem.read_u32(self.erstba + 8) & 0xffff;
        if seg_base == 0 || seg_size_trbs == 0 {
            return;
        }

        // Place the event at the current enqueue pointer (defaults to the segment base).
        let trb_addr = if self.event_enqueue == 0 {
            seg_base
        } else {
            self.event_enqueue
        };
        // Wrap if we would go past the segment.
        let seg_bytes = u64::from(seg_size_trbs) * 16;
        let seg_end = seg_base.saturating_add(seg_bytes);
        if trb_addr + 16 > seg_end {
            self.event_enqueue = seg_base;
        }
        let trb_addr = if self.event_enqueue == 0 {
            seg_base
        } else {
            self.event_enqueue
        };

        // Port Status Change Event TRB (type 34). We don't currently validate the contents in
        // tests, but writing something stable makes debugging easier.
        mem.write_u32(trb_addr + 0, 0);
        mem.write_u32(trb_addr + 4, 0);
        mem.write_u32(trb_addr + 8, 0);
        mem.write_u32(trb_addr + 12, (34u32 << 10) | 1); // type + cycle bit

        self.event_enqueue = trb_addr + 16;
        self.iman |= IMAN_IP;
    }

    fn clear_interrupt_pending(&mut self) {
        self.iman &= !IMAN_IP;
    }
}

impl MmioHandler for TestXhciMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match (offset, size) {
            (CAP_REG_CAPLENGTH_HCIVERSION, 4) => {
                let hciversion = 0x0100u32;
                u32::from(CAPLENGTH) as u64 | (u64::from(hciversion) << 16)
            }
            (CAP_REG_CAPLENGTH_HCIVERSION, 1) => u64::from(CAPLENGTH),
            (CAP_REG_RTSOFF, 4) => u64::from(RTSOFF),

            (OP_REG_USBCMD, 4) => u64::from(self.usbcmd),

            (RT_IMAN, 4) => u64::from(self.iman),
            (RT_ERSTSZ, 4) => u64::from(self.erstsz),

            (RT_ERSTBA, 8) => self.erstba,
            (RT_ERSTBA, 4) => u64::from(self.erstba as u32),
            (RT_ERSTBA_HI, 4) => u64::from((self.erstba >> 32) as u32),

            (RT_ERDP, 8) => self.erdp,
            (RT_ERDP, 4) => u64::from(self.erdp as u32),
            (RT_ERDP_HI, 4) => u64::from((self.erdp >> 32) as u32),

            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        match (offset, size) {
            (OP_REG_USBCMD, 4) => self.usbcmd = value as u32,

            // IMAN: IP is RW1C, IE is RW.
            (RT_IMAN, 4) => {
                let v = value as u32;

                // Update IE.
                self.iman = (self.iman & !IMAN_IE) | (v & IMAN_IE);

                // W1C IP.
                if (v & IMAN_IP) != 0 {
                    self.clear_interrupt_pending();
                }
            }

            (RT_ERSTSZ, 4) => self.erstsz = value as u32,

            // ERSTBA is a 64-bit register exposed as two dwords.
            (RT_ERSTBA, 8) => self.erstba = value,
            (RT_ERSTBA, 4) => self.erstba = (self.erstba & 0xffff_ffff_0000_0000) | value as u32 as u64,
            (RT_ERSTBA_HI, 4) => {
                self.erstba = (self.erstba & 0x0000_0000_ffff_ffff) | ((value as u64) << 32)
            }

            // ERDP is a 64-bit register exposed as two dwords.
            (RT_ERDP, 8) => {
                self.erdp = value;
                if self.event_enqueue != 0 && self.erdp == self.event_enqueue {
                    self.clear_interrupt_pending();
                }
            }
            (RT_ERDP, 4) => {
                self.erdp = (self.erdp & 0xffff_ffff_0000_0000) | value as u32 as u64;
                if self.event_enqueue != 0 && self.erdp == self.event_enqueue {
                    self.clear_interrupt_pending();
                }
            }
            (RT_ERDP_HI, 4) => {
                self.erdp = (self.erdp & 0x0000_0000_ffff_ffff) | ((value as u64) << 32);
                if self.event_enqueue != 0 && self.erdp == self.event_enqueue {
                    self.clear_interrupt_pending();
                }
            }

            _ => {}
        }
    }
}

fn install_test_xhci(pc: &mut PcPlatform, mmio: Rc<RefCell<TestXhciMmio>>) {
    let mut dev = TestXhciPciConfigDevice::new();
    pc.pci_intx
        .configure_device_intx(XHCI_BDF, Some(PciInterruptPin::IntA), dev.config_mut());
    pc.pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(XHCI_BDF, Box::new(dev));

    pc.register_pci_mmio_bar_handler(XHCI_BDF, XHCI_BAR_INDEX, mmio);
}

#[test]
fn pc_platform_gates_xhci_mmio_on_pci_command_mem_bit() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let mmio = Rc::new(RefCell::new(TestXhciMmio::default()));
    install_test_xhci(&mut pc, mmio);

    // Program BAR0 and enable memory decoding.
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        XHCI_BAR_BASE as u32,
    );
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0002,
    );

    pc.memory.write_u32(XHCI_BAR_BASE + OP_REG_USBCMD, 0xDEAD_BEEF);
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE + OP_REG_USBCMD),
        0xDEAD_BEEF
    );

    // Disable memory decoding: reads float high and writes are ignored.
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0000,
    );
    assert_eq!(pc.memory.read_u32(XHCI_BAR_BASE + OP_REG_USBCMD), 0xFFFF_FFFF);
    pc.memory.write_u32(XHCI_BAR_BASE + OP_REG_USBCMD, 0);

    // Re-enable decoding: the write above must not have reached the device.
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0002,
    );
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE + OP_REG_USBCMD),
        0xDEAD_BEEF
    );
}

#[test]
fn pc_platform_routes_xhci_mmio_after_bar_reprogramming() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let mmio = Rc::new(RefCell::new(TestXhciMmio::default()));
    install_test_xhci(&mut pc, mmio);

    // Program BAR0 and enable memory decoding.
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        XHCI_BAR_BASE as u32,
    );
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0002,
    );

    pc.memory.write_u32(XHCI_BAR_BASE + OP_REG_USBCMD, 0xA5A5_5A5A);
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE + OP_REG_USBCMD),
        0xA5A5_5A5A
    );

    // Move BAR0 within the PCI MMIO window.
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        XHCI_BAR_BASE_RELOC as u32,
    );

    // Old base should no longer decode.
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE + OP_REG_USBCMD),
        0xFFFF_FFFF
    );

    // New base should decode and preserve register state.
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE_RELOC + OP_REG_USBCMD),
        0xA5A5_5A5A
    );
    pc.memory
        .write_u32(XHCI_BAR_BASE_RELOC + OP_REG_USBCMD, 0x1234_5678);
    assert_eq!(
        pc.memory.read_u32(XHCI_BAR_BASE_RELOC + OP_REG_USBCMD),
        0x1234_5678
    );
}

#[test]
fn pc_platform_xhci_intx_asserts_and_clears() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let xhci = Rc::new(RefCell::new(TestXhciMmio::default()));
    install_test_xhci(&mut pc, xhci.clone());

    // Assign BAR0, enable memory decoding.
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        XHCI_BAR_BASE as u32,
    );
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0002,
    );

    // Register the INTx source so `poll_pci_intx_lines` drives the PIC.
    pc.register_pci_intx_source(XHCI_BDF, PciInterruptPin::IntA, {
        let xhci = xhci.clone();
        move |_pc| xhci.borrow().irq_level()
    });

    let expected_irq =
        u8::try_from(pc.pci_intx.gsi_for_intx(XHCI_BDF, PciInterruptPin::IntA)).unwrap();

    // Unmask the routed IRQ (and cascade) so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if expected_irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
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

    pc.memory.write_u32(XHCI_BAR_BASE + RT_ERSTSZ, 1);
    pc.memory.write_u64(XHCI_BAR_BASE + RT_ERSTBA, ERST_BASE);
    pc.memory.write_u64(XHCI_BAR_BASE + RT_ERDP, EVENT_RING_BASE);

    // IMAN.IE=1
    pc.memory.write_u32(XHCI_BAR_BASE + RT_IMAN, u32::from(IMAN_IE));

    // Trigger a port status change (device attach).
    xhci.borrow_mut().attach_device(&mut pc.memory);
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
    let iman = pc.memory.read_u32(XHCI_BAR_BASE + RT_IMAN);
    pc.memory.write_u32(XHCI_BAR_BASE + RT_IMAN, iman | IMAN_IP);
    pc.memory.write_u64(XHCI_BAR_BASE + RT_ERDP, EVENT_RING_BASE + 16);

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_xhci_intx() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let xhci = Rc::new(RefCell::new(TestXhciMmio::default()));
    install_test_xhci(&mut pc, xhci.clone());

    // Assign BAR0, enable memory decoding.
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        XHCI_BAR_BASE as u32,
    );
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        0x0002,
    );

    // Register the INTx source so `poll_pci_intx_lines` drives the PIC.
    pc.register_pci_intx_source(XHCI_BDF, PciInterruptPin::IntA, {
        let xhci = xhci.clone();
        move |_pc| xhci.borrow().irq_level()
    });

    let expected_irq =
        u8::try_from(pc.pci_intx.gsi_for_intx(XHCI_BDF, PciInterruptPin::IntA)).unwrap();

    // Unmask the routed IRQ (and cascade) so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if expected_irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
        interrupts.pic_mut().set_masked(expected_irq, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Program a minimal ERST and enable interrupter 0 so a port status change can assert IRQ.
    const ERST_BASE: u64 = 0x10_000;
    const EVENT_RING_BASE: u64 = 0x11_000;

    pc.memory.write_u64(ERST_BASE, EVENT_RING_BASE);
    pc.memory.write_u32(ERST_BASE + 8, 1);
    pc.memory.write_u32(ERST_BASE + 12, 0);

    pc.memory.write_u32(XHCI_BAR_BASE + RT_ERSTSZ, 1);
    pc.memory.write_u64(XHCI_BAR_BASE + RT_ERSTBA, ERST_BASE);
    pc.memory.write_u64(XHCI_BAR_BASE + RT_ERDP, EVENT_RING_BASE);

    pc.memory.write_u32(XHCI_BAR_BASE + RT_IMAN, u32::from(IMAN_IE));

    // Cause the device to assert IMAN.IP.
    xhci.borrow_mut().attach_device(&mut pc.memory);
    assert!(
        xhci.borrow().irq_level(),
        "test expects the xHCI stub to have a pending interrupt before COMMAND.INTX_DISABLE gating"
    );

    let command = read_cfg_u32(&mut pc, XHCI_BDF.bus, XHCI_BDF.device, XHCI_BDF.function, 0x04) as u16;

    // Disable INTx in PCI command register (bit 10) while leaving memory decode enabled.
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        command | (1 << 10),
    );
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "INTx should be suppressed when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx and ensure the asserted line is delivered.
    write_cfg_u16(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x04,
        command & !(1 << 10),
    );
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
    // Sanity: the test device's BAR definition should surface through config space probing, so the
    // other tests are actually exercising PCI BAR mechanics (not a hardcoded mapping).
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let mmio = Rc::new(RefCell::new(TestXhciMmio::default()));
    install_test_xhci(&mut pc, mmio);

    // BAR size probing: write all-ones and read back the size mask.
    // For a 64KiB BAR, the mask is 0xFFFF_0000 (lower bits are hardwired to 0 per PCI spec).
    write_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
        0xFFFF_FFFF,
    );
    let got = read_cfg_u32(
        &mut pc,
        XHCI_BDF.bus,
        XHCI_BDF.device,
        XHCI_BDF.function,
        0x10,
    );
    assert_eq!(got, !(XHCI_BAR_SIZE - 1) & 0xFFFF_FFF0);
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

    let bdf = USB_XHCI_QEMU.bdf;

    // PCI ID + class code should match the profile.
    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(USB_XHCI_QEMU.vendor_id));
    assert_eq!((id >> 16) & 0xFFFF, u32::from(USB_XHCI_QEMU.device_id));

    let class_rev = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!(class_rev >> 8, USB_XHCI_QEMU.class.as_u32());

    // BIOS POST should allocate BAR0 and enable MEM decoding.
    let command = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04);
    assert_ne!(command & 0x2, 0, "COMMAND.MEM should be enabled by BIOS POST");

    let bar0_raw = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    let alloc_cfg = PciResourceAllocatorConfig::default();
    assert!(
        (alloc_cfg.mmio_base..alloc_cfg.mmio_base + alloc_cfg.mmio_size).contains(&bar0_base),
        "BAR0 base {bar0_base:#x} should land in the platform's PCI MMIO window"
    );

    // Smoke MMIO read/write through the decoded BAR.
    let cap = pc.memory.read_u32(bar0_base);
    assert_ne!(cap, 0xffff_ffff, "BAR0 should route to an MMIO handler");

    // The xHCI capability registers encode CAPLENGTH in the low byte; the operational register
    // block starts at BAR0 + CAPLENGTH. Read/then-write-back USBCMD as an MVP smoke test.
    let caplength = (cap & 0xFF) as u64;
    assert_ne!(caplength, 0, "CAPLENGTH should be non-zero");
    let usbcmd_addr = bar0_base + caplength;
    let before = pc.memory.read_u32(usbcmd_addr);
    pc.memory.write_u32(usbcmd_addr, before);
    let after = pc.memory.read_u32(usbcmd_addr);
    assert_ne!(
        after, 0xffff_ffff,
        "MMIO writes should not deconfigure BAR routing"
    );
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
    let bar0_raw = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_base = u64::from(bar0_raw & 0xffff_fff0);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Ensure we can observe the routed IRQ via the legacy PIC.
    let expected_irq =
        u8::try_from(pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA)).unwrap();
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if expected_irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
        interrupts.pic_mut().set_masked(expected_irq, false);
    }

    // Enable Bus Mastering so the device can exercise a small DMA read on the first RUN edge.
    let command = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | 0x0004,
    );

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

    let bar0_raw = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
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
