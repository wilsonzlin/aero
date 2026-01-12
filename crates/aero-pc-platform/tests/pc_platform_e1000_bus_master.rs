use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_net_e1000::ICR_RXT0;
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

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn read_e1000_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
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
fn pc_platform_gates_e1000_dma_on_pci_bus_master_enable() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bdf = NIC_E1000_82540EM.bdf;

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(
        command & 0x2,
        0,
        "BIOS POST should enable memory decoding for E1000"
    );
    assert_eq!(
        command & (1 << 2),
        0,
        "BIOS POST should not enable bus mastering for E1000"
    );

    let bar0_base = read_e1000_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");

    // Unmask IRQ2 (cascade) and IRQ11 so we can observe E1000 INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(11, false);
    }

    // RX ring: 2 descriptors => 1 usable due to head/tail semantics.
    let ring_base = 0x1000u64;
    let buf_addr = 0x2000u64;

    // Descriptor 0: buffer at 0x2000.
    pc.memory.write_physical(ring_base, &buf_addr.to_le_bytes());

    // Sentinel: buffer should remain unchanged while bus mastering is disabled.
    pc.memory
        .write_physical(buf_addr, &[0x5a; aero_net_e1000::MIN_L2_FRAME_LEN + 4]);

    // Program RX ring.
    pc.memory.write_u32(bar0_base + 0x2800, ring_base as u32); // RDBAL
    pc.memory.write_u32(bar0_base + 0x2804, 0); // RDBAH
    pc.memory.write_u32(bar0_base + 0x2808, 2 * 16); // RDLEN
    pc.memory.write_u32(bar0_base + 0x2810, 0); // RDH
    pc.memory.write_u32(bar0_base + 0x2818, 1); // RDT
    pc.memory.write_u32(bar0_base + 0x0100, 1 << 1); // RCTL.EN

    // Enable RX interrupts (RXT0).
    pc.memory.write_u32(bar0_base + 0x00D0, ICR_RXT0); // IMS

    let frame = build_test_frame(b"hi");
    assert!(pc.e1000_enqueue_rx_frame(frame.clone()));

    // With COMMAND.BME=0, the poll method should be a no-op for DMA: no writes to guest buffers and
    // no interrupt should be asserted.
    pc.process_e1000();
    pc.poll_pci_intx_lines();

    let mut buf = vec![0u8; frame.len()];
    pc.memory.read_physical(buf_addr, &mut buf);
    assert_eq!(buf, vec![0x5a; frame.len()]);
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Enable bus mastering and poll again: DMA should now deliver the frame + assert an interrupt.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        (command as u16) | (1 << 2),
    );

    pc.process_e1000();
    pc.poll_pci_intx_lines();

    pc.memory.read_physical(buf_addr, &mut buf);
    assert_eq!(buf, frame);

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ11 should be pending after enabling bus mastering");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 11);
}
