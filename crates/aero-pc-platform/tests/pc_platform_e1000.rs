use aero_cpu_core::mem::CpuBus as _;
use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_net_e1000::{ICR_TXDW, MIN_L2_FRAME_LEN};
use aero_pc_platform::{PcCpuBus, PcPlatform};
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

fn read_e1000_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

fn read_e1000_bar1_base(pc: &mut PcPlatform) -> u32 {
    let bdf = NIC_E1000_82540EM.bdf;
    let bar1 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    bar1 & 0xffff_fffc
}

fn write_u64_le(pc: &mut PcPlatform, addr: u64, v: u64) {
    pc.memory.write_physical(addr, &v.to_le_bytes());
}

/// Minimal legacy TX descriptor layout (16 bytes).
fn write_tx_desc(pc: &mut PcPlatform, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(pc, addr, buf_addr);
    pc.memory.write_physical(addr + 8, &len.to_le_bytes());
    pc.memory.write_physical(addr + 10, &[0u8]); // cso
    pc.memory.write_physical(addr + 11, &[cmd]);
    pc.memory.write_physical(addr + 12, &[status]);
    pc.memory.write_physical(addr + 13, &[0u8]); // css
    pc.memory.write_physical(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn pc_platform_enumerates_e1000_and_assigns_bars() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(NIC_E1000_82540EM.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(NIC_E1000_82540EM.device_id));

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x1, 0, "BIOS POST should enable IO decoding");
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    let bar0_base = read_e1000_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");
    assert_eq!(bar0_base % 0x20_000u64, 0);

    let bar1_base = read_e1000_bar1_base(&mut pc);
    assert_ne!(bar1_base, 0, "BAR1 should be assigned during BIOS POST");
    assert_eq!(bar1_base % 0x40, 0);
}

#[test]
fn pc_platform_routes_e1000_mmio_through_bar0() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bar0_base = read_e1000_bar0_base(&mut pc);

    // STATUS register at offset 0x08 should return a real value, not all-ones.
    let status = pc.memory.read_u32(bar0_base + 0x08);
    assert_ne!(status, 0xFFFF_FFFF);

    // Ensure the MMIO router supports 64-bit reads by splitting into 32-bit operations.
    assert_eq!(pc.memory.read_u64(bar0_base + 0x08), u64::from(status));
}

#[test]
fn pc_platform_gates_e1000_mmio_on_pci_command_register() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0_base = read_e1000_bar0_base(&mut pc);

    // IMS should start cleared.
    assert_eq!(pc.memory.read_u32(bar0_base + 0x00D0), 0);

    // Disable PCI memory decoding but keep IO decoding enabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    assert_eq!(pc.memory.read_u32(bar0_base + 0x0008), 0xFFFF_FFFF);

    // Writes should be ignored while decoding is disabled.
    pc.memory.write_u32(bar0_base + 0x00D0, 0x1234_5678);

    // Re-enable memory decoding: IMS should still be 0.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0003);
    assert_eq!(pc.memory.read_u32(bar0_base + 0x00D0), 0);
}

#[test]
fn pc_platform_routes_e1000_mmio_after_bar0_reprogramming() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;

    let bar0_base = read_e1000_bar0_base(&mut pc);
    let new_base = bar0_base + 0x20_000;
    assert_eq!(new_base % 0x20_000, 0);

    pc.memory.write_u32(bar0_base + 0x00D0, 0xABCD_EF01);
    assert_eq!(pc.memory.read_u32(bar0_base + 0x00D0), 0xABCD_EF01);

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
    assert_eq!(pc.memory.read_u32(bar0_base + 0x00D0), 0xFFFF_FFFF);

    // New base should decode and preserve register state.
    assert_eq!(pc.memory.read_u32(new_base + 0x00D0), 0xABCD_EF01);
}

#[test]
fn pc_platform_routes_e1000_io_through_bar1() {
    let pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(pc);
    let bar1_base = read_e1000_bar1_base(&mut bus.platform);
    let base = u16::try_from(bar1_base).expect("BAR1 should fit in 16-bit I/O port space");
    let iodata = base.checked_add(4).unwrap();

    // IOADDR/ IODATA should map to MMIO registers. Read STATUS (0x08) via I/O.
    bus.io_write(base, 4, 0x08).unwrap();
    let status = bus.io_read(iodata, 4).unwrap() as u32;
    assert_ne!(status, 0xFFFF_FFFF);
}

#[test]
fn pc_platform_gates_e1000_io_on_pci_command_register() {
    let pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(pc);

    let bdf = NIC_E1000_82540EM.bdf;
    let bar1_base = read_e1000_bar1_base(&mut bus.platform);
    let base = u16::try_from(bar1_base).expect("BAR1 should fit in 16-bit I/O port space");
    let iodata = base.checked_add(4).unwrap();

    // Should decode when IO is enabled.
    bus.io_write(base, 4, 0x08).unwrap();
    assert_ne!(bus.io_read(iodata, 4).unwrap() as u32, 0xFFFF_FFFF);

    // Disable PCI I/O decoding but keep memory decoding enabled.
    write_cfg_u16(
        &mut bus.platform,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0002,
    );

    // Should no longer decode through the BAR1 window.
    bus.io_write(base, 4, 0x08).unwrap();
    assert_eq!(bus.io_read(iodata, 4).unwrap() as u32, 0xFFFF_FFFF);

    // Re-enable I/O decoding.
    write_cfg_u16(
        &mut bus.platform,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0003,
    );
    bus.io_write(base, 4, 0x08).unwrap();
    assert_ne!(bus.io_read(iodata, 4).unwrap() as u32, 0xFFFF_FFFF);
}

#[test]
fn pc_platform_routes_e1000_io_after_bar1_reprogramming() {
    let pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(pc);

    let bdf = NIC_E1000_82540EM.bdf;
    let bar1_base = read_e1000_bar1_base(&mut bus.platform);
    let new_base = bar1_base + 0x40;
    assert_eq!(new_base % 0x40, 0);

    write_cfg_u32(
        &mut bus.platform,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x14,
        new_base,
    );

    // Old base should no longer decode.
    let old = u16::try_from(bar1_base).unwrap();
    let old_iodata = old.checked_add(4).unwrap();
    bus.io_write(old, 4, 0x08).unwrap();
    assert_eq!(bus.io_read(old_iodata, 4).unwrap() as u32, 0xFFFF_FFFF);

    // New base should decode.
    let new = u16::try_from(new_base).unwrap();
    let new_iodata = new.checked_add(4).unwrap();
    bus.io_write(new, 4, 0x08).unwrap();
    assert_ne!(bus.io_read(new_iodata, 4).unwrap() as u32, 0xFFFF_FFFF);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_e1000_intx() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0_base = read_e1000_bar0_base(&mut pc);

    // Unmask IRQ2 (cascade) and IRQ11 so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(11, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Enable TXDW interrupt and set the cause.
    pc.memory.write_u32(bar0_base + 0x00D0, ICR_TXDW); // IMS
    pc.memory.write_u32(bar0_base + 0x00C8, ICR_TXDW); // ICS

    // Disable INTx in PCI command register (bit 10), while keeping IO+MEM decoding enabled.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0003 | (1 << 10),
    );
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "INTx should not be delivered when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx; since the interrupt cause is still pending, it should now be delivered.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0003);
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ11 should be pending after re-enabling INTx");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 11);
}

#[test]
fn pc_platform_routes_e1000_io_bar() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let io_base = read_e1000_bar1_base(&mut pc);
    assert_ne!(io_base, 0);
    assert_eq!(io_base % 0x40, 0);
    let io_base = u16::try_from(io_base).expect("BAR1 base should fit in u16 for IoPortBus");

    let bar0_base = read_e1000_bar0_base(&mut pc);
    let mmio_status = pc.memory.read_u32(bar0_base + 0x08);
    assert_ne!(mmio_status, 0xFFFF_FFFF);

    // IOADDR (offset 0x00) selects the target MMIO register.
    pc.io.write(io_base, 4, 0x0000_0008);

    // IODATA (offset 0x04) reads from the selected MMIO register (STATUS).
    let io_status = pc.io.read(io_base + 0x04, 4);
    assert_eq!(io_status, mmio_status);
}

#[test]
fn pc_platform_defers_e1000_dma_until_process_e1000() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0_base = read_e1000_bar0_base(&mut pc);

    // Enable IO + MEM decoding and Bus Mastering.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0007);

    // Configure TX ring: 4 descriptors at 0x1000.
    pc.memory.write_u32(bar0_base + 0x00D0, ICR_TXDW); // IMS
    pc.memory.write_u32(bar0_base + 0x3800, 0x1000); // TDBAL
    pc.memory.write_u32(bar0_base + 0x3804, 0); // TDBAH
    pc.memory.write_u32(bar0_base + 0x3808, 4 * 16); // TDLEN
    pc.memory.write_u32(bar0_base + 0x3810, 0); // TDH
    pc.memory.write_u32(bar0_base + 0x3818, 0); // TDT
    pc.memory.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN

    // Guest TX: descriptor 0 points at packet buffer 0x2000.
    let pkt_out = build_test_frame(b"guest->host");
    assert_eq!(pkt_out.len(), MIN_L2_FRAME_LEN + b"guest->host".len());
    pc.memory.write_physical(0x2000, &pkt_out);
    write_tx_desc(
        &mut pc,
        0x1000,
        0x2000,
        pkt_out.len() as u16,
        0b0000_1001, // EOP|RS
        0,
    );

    // Update tail via MMIO. The platform MMIO handler does not provide a memory reference, so the
    // device must defer DMA until `process_e1000()` is called.
    pc.memory.write_u32(bar0_base + 0x3818, 1); // TDT = 1

    assert!(pc.e1000_pop_tx_frame().is_none());

    let mut status = [0u8; 1];
    pc.memory.read_physical(0x1000 + 12, &mut status);
    assert_eq!(status[0] & 0x01, 0, "DD should not be set before process_e1000()");

    pc.process_e1000();

    assert_eq!(pc.e1000_pop_tx_frame().as_deref(), Some(pkt_out.as_slice()));
    assert!(pc.e1000.as_ref().unwrap().borrow().irq_level());

    pc.memory.read_physical(0x1000 + 12, &mut status);
    assert_ne!(status[0] & 0x01, 0, "DD should be set after process_e1000()");
}
