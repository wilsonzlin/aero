use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::pci::{PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_usb::ehci::regs::*;
use aero_usb::ehci::DEFAULT_PORT_COUNT;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use memory::MemoryBus as _;

// Keep the relocated BAR within the platform's default PCI MMIO window
// (0xE000_0000..0xF000_0000) and away from other devices that BIOS POST might have allocated near
// the start of the window.
const EHCI_BAR0_RELOC_BASE: u64 = 0xE200_0000;

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((bdf.bus as u32) << 16)
        | ((bdf.device as u32) << 11)
        | ((bdf.function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u32 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u16 {
    let shift = (offset & 2) * 8;
    (read_cfg_u32(pc, bdf, offset) >> shift) as u16
}

fn write_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u16) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    // PCI config writes to 0xCFC always write a dword; the platform will apply byte enables based
    // on the port access size (2 here).
    pc.io.write(
        PCI_CFG_DATA_PORT + u16::from(offset & 2),
        2,
        u32::from(value),
    );
}

fn write_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u32) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_bar0_base(pc: &mut PcPlatform, bdf: PciBdf) -> u64 {
    let bar0 = read_cfg_u32(pc, bdf, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

fn new_pc() -> PcPlatform {
    PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            // Keep the test focused: avoid allocating other large MMIO devices by default.
            enable_ahci: false,
            enable_uhci: false,
            enable_ehci: true,
            // Avoid pulling in unrelated virtio MSI-X paths; tests use INTx.
            enable_virtio_msix: false,
            ..Default::default()
        },
    )
}

#[test]
fn pc_platform_enumerates_ehci_and_assigns_bar0_and_probe_mask() {
    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_size = USB_EHCI_ICH9.bars[0].size;
    let bar0_size_u32 = u32::try_from(bar0_size).expect("EHCI BAR0 size should fit in u32");

    let id = read_cfg_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xffff, u32::from(USB_EHCI_ICH9.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(USB_EHCI_ICH9.device_id));

    let class_rev = read_cfg_u32(&mut pc, bdf, 0x08);
    assert_eq!(class_rev >> 8, USB_EHCI_ICH9.class.as_u32());

    // BIOS POST should allocate BAR0 and enable MEM decoding.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    assert_ne!(
        command & 0x2,
        0,
        "COMMAND.MEM should be enabled by BIOS POST"
    );

    let bar0_orig = read_cfg_u32(&mut pc, bdf, 0x10);
    let bar0_base = u64::from(bar0_orig & 0xffff_fff0);
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 should be assigned during BIOS POST"
    );
    assert_eq!(
        bar0_base % bar0_size,
        0,
        "EHCI BAR0 should be aligned to its configured size"
    );

    write_cfg_u32(&mut pc, bdf, 0x10, 0xffff_ffff);
    let bar0_probe = read_cfg_u32(&mut pc, bdf, 0x10);
    let expected_probe = !(bar0_size_u32 - 1) | (bar0_orig & 0xF);
    assert_eq!(
        bar0_probe, expected_probe,
        "BAR0 probe should return {bar0_size:#x} size mask"
    );
    write_cfg_u32(&mut pc, bdf, 0x10, bar0_orig);
}

#[test]
fn pc_platform_routes_ehci_mmio_capability_registers() {
    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(bar0_base, 0);

    let caplength = pc.memory.read_u8(bar0_base + REG_CAPLENGTH_HCIVERSION);
    assert_eq!(caplength, CAPLENGTH);

    let hciversion = pc.memory.read_u16(bar0_base + REG_CAPLENGTH_HCIVERSION + 2);
    assert_eq!(hciversion, HCIVERSION);

    let hcsparams = pc.memory.read_u32(bar0_base + REG_HCSPARAMS);
    let n_ports = hcsparams & 0x0f;
    assert_eq!(n_ports, DEFAULT_PORT_COUNT as u32);
}

#[test]
fn pc_platform_ehci_bar0_size_probe_reports_expected_mask() {
    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_size = USB_EHCI_ICH9.bars[0].size;
    let bar0_size_u32 = u32::try_from(bar0_size).expect("EHCI BAR0 size should fit in u32");
    let bar0_orig = read_cfg_u32(&mut pc, bdf, 0x10);

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    write_cfg_u32(&mut pc, bdf, 0x10, 0xffff_ffff);
    let got = read_cfg_u32(&mut pc, bdf, 0x10);

    let expected = !(bar0_size_u32 - 1) | (bar0_orig & 0xF);
    assert_eq!(got, expected);
}

#[test]
fn pc_platform_gates_ehci_mmio_on_pci_command_mem_bit() {
    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 should be allocated during BIOS POST"
    );

    // Enable MEM decoding so BAR0 MMIO is routed (BIOS POST may already do this).
    let cmd = read_cfg_u16(&mut pc, bdf, 0x04) | 0x0002;
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);

    // Note: USBCMD has reserved bits and a reset bit (HCRESET, bit 1). Use only bits that are
    // defined as writable by the controller model.
    let usbcmd_addr = bar0_base + REG_USBCMD;
    let usbcmd_val = (USBCMD_RS | USBCMD_PSE | USBCMD_ASE) & USBCMD_WRITE_MASK;
    pc.memory.write_u32(usbcmd_addr, usbcmd_val);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), usbcmd_val);

    // Disable memory decoding: reads float high and writes are ignored.
    write_cfg_u16(&mut pc, bdf, 0x04, cmd & !0x0002);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), 0xffff_ffff);
    pc.memory.write_u32(usbcmd_addr, 0);

    // Re-enable decoding: the write above must not have reached the device.
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), usbcmd_val);
}

#[test]
fn pc_platform_routes_ehci_mmio_after_bar0_reprogramming() {
    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_size = USB_EHCI_ICH9.bars[0].size;

    // Enable MEM decoding so BAR0 MMIO is routed.
    let cmd = read_cfg_u16(&mut pc, bdf, 0x04) | 0x0002;
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 should be allocated during BIOS POST"
    );

    let initial = USBCMD_PSE & USBCMD_WRITE_MASK;
    pc.memory.write_u32(bar0_base + REG_USBCMD, initial);
    assert_eq!(pc.memory.read_u32(bar0_base + REG_USBCMD), initial);

    // Move BAR0 within the PCI MMIO window.
    let new_base = if bar0_base == EHCI_BAR0_RELOC_BASE {
        EHCI_BAR0_RELOC_BASE + bar0_size
    } else {
        EHCI_BAR0_RELOC_BASE
    };
    write_cfg_u32(&mut pc, bdf, 0x10, new_base as u32);

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u32(bar0_base + REG_USBCMD), 0xffff_ffff);

    // New base should decode and preserve register state.
    assert_eq!(pc.memory.read_u32(new_base + REG_USBCMD), initial);
    let next = (USBCMD_RS | USBCMD_ASE) & USBCMD_WRITE_MASK;
    pc.memory.write_u32(new_base + REG_USBCMD, next);
    assert_eq!(pc.memory.read_u32(new_base + REG_USBCMD), next);
}

struct FixedInDevice {
    sent: bool,
    report: Vec<u8>,
}

impl FixedInDevice {
    fn new(report: Vec<u8>) -> Self {
        Self {
            sent: false,
            report,
        }
    }
}

impl UsbDeviceModel for FixedInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        if self.sent {
            None
        } else {
            self.sent = true;
            Some(self.report.clone())
        }
    }
}

#[test]
fn pc_platform_ehci_async_schedule_in_transfer_dmas_and_asserts_intx() {
    // EHCI schedule element alignment / pointer encoding.
    const LINK_TERMINATE: u32 = 1 << 0;
    const LINK_TYPE_QH: u32 = 0b01 << 1;

    // QH/qTD offsets (subset) used by the async schedule engine.
    const QH_HORIZ: u64 = 0x00;
    const QH_EPCHAR: u64 = 0x04;
    const QH_CUR_QTD: u64 = 0x0c;
    const QH_NEXT_QTD: u64 = 0x10;
    const QH_ALT_NEXT_QTD: u64 = 0x14;

    const QTD_NEXT: u64 = 0x00;
    const QTD_ALT_NEXT: u64 = 0x04;
    const QTD_TOKEN: u64 = 0x08;
    const QTD_BUF0: u64 = 0x0c;

    // qTD token bits.
    const QTD_STS_ACTIVE: u32 = 1 << 7;
    const QTD_PID_IN: u32 = 0b01 << 8;
    const QTD_IOC: u32 = 1 << 15;
    const QTD_TOTAL_BYTES_SHIFT: u32 = 16;

    const QH_ADDR: u64 = 0x1000;
    const QTD_ADDR: u64 = 0x2000;
    const BUF_ADDR: u64 = 0x3000;

    let mut pc = new_pc();
    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(bar0_base, 0);

    // Unmask the routed IRQ (and cascade) so we can observe EHCI INTx through the legacy PIC.
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("EHCI INTx should route to a PIC IRQ in legacy mode");
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable PCI memory decoding + Bus Mastering so the controller can DMA schedule structures.
    let command = read_cfg_u16(&mut pc, bdf, 0x04);
    // Also clear PCI `INTX_DISABLE` (bit 10) so we can observe INTx from the controller.
    write_cfg_u16(&mut pc, bdf, 0x04, (command | 0x0006) & !0x0400);

    // Attach a deterministic device model.
    {
        let ehci = pc.ehci.as_ref().expect("EHCI should be enabled").clone();
        let mut dev = ehci.borrow_mut();
        dev.controller_mut().hub_mut().attach(
            0,
            Box::new(FixedInDevice::new(vec![0xde, 0xad, 0xbe, 0xef])),
        );
    }

    // Route all ports to EHCI (clear PORT_OWNER) and reset the port so it becomes enabled.
    pc.memory
        .write_u32(bar0_base + REG_CONFIGFLAG, CONFIGFLAG_CF);
    // Preserve PORTSC.PP (port power) while asserting reset.
    pc.memory
        .write_u32(bar0_base + reg_portsc(0), PORTSC_PP | PORTSC_PR);
    pc.tick(50_000_000);

    // Program a minimal async schedule: one QH with one IN qTD that writes 4 bytes.
    pc.memory
        .write_u32(QH_ADDR + QH_HORIZ, (QH_ADDR as u32) | LINK_TYPE_QH);
    // Device address 0, endpoint 1, high-speed, MPS=64.
    const SPEED_HIGH: u32 = 2;
    let ep_char = 0 | (1u32 << 8) | (SPEED_HIGH << 12) | (64u32 << 16);
    pc.memory.write_u32(QH_ADDR + QH_EPCHAR, ep_char);
    pc.memory.write_u32(QH_ADDR + QH_CUR_QTD, 0);
    pc.memory.write_u32(QH_ADDR + QH_NEXT_QTD, QTD_ADDR as u32);
    pc.memory
        .write_u32(QH_ADDR + QH_ALT_NEXT_QTD, LINK_TERMINATE);

    pc.memory.write_u32(QTD_ADDR + QTD_NEXT, LINK_TERMINATE);
    pc.memory.write_u32(QTD_ADDR + QTD_ALT_NEXT, LINK_TERMINATE);
    pc.memory.write_u32(QTD_ADDR + QTD_BUF0, BUF_ADDR as u32);
    pc.memory.write_physical(BUF_ADDR, &[0u8; 4]);

    let token = QTD_STS_ACTIVE | QTD_IOC | QTD_PID_IN | (4u32 << QTD_TOTAL_BYTES_SHIFT);
    pc.memory.write_u32(QTD_ADDR + QTD_TOKEN, token);

    // Clear any stale W1C bits.
    pc.memory.write_u32(bar0_base + REG_USBSTS, USBSTS_W1C_MASK);
    pc.memory
        .write_u32(bar0_base + REG_ASYNCLISTADDR, QH_ADDR as u32);
    pc.memory.write_u32(bar0_base + REG_USBINTR, USBINTR_USBINT);
    pc.memory
        .write_u32(bar0_base + REG_USBCMD, USBCMD_RS | USBCMD_ASE);

    pc.tick(1_000_000);

    let token_after = pc.memory.read_u32(QTD_ADDR + QTD_TOKEN);
    assert_eq!(
        token_after & QTD_STS_ACTIVE,
        0,
        "qTD should complete (Active cleared)"
    );

    let mut buf = [0u8; 4];
    pc.memory.read_physical(BUF_ADDR, &mut buf);
    assert_eq!(buf, [0xde, 0xad, 0xbe, 0xef]);

    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("EHCI IRQ should be pending after schedule completion");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to a PIC IRQ");
    assert_eq!(pending_irq, irq);
}
