use aero_devices::pci::profile::NVME_CONTROLLER;
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

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

#[test]
fn pc_platform_enumerates_nvme_and_assigns_bar0() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(NVME_CONTROLLER.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(NVME_CONTROLLER.device_id));

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    // BAR0 is a 64-bit MMIO BAR.
    let bar0_lo = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
    assert_eq!(bar0_lo & 0x7, 0x4);

    let bar0_base = read_nvme_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");
    assert_eq!(bar0_base % 0x4000, 0);
}

#[test]
fn pc_platform_nvme_admin_identify_produces_completion_and_intx() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0006,
    );

    // Unmask IRQ2 (cascade) and the routed NVMe INTx IRQ (device 3 INTA# -> PIRQD -> GSI/IRQ13).
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(13, false);
    }

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    // Configure + enable controller.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    pc.memory.write_u64(bar0_base + 0x0028, asq); // ASQ
    pc.memory.write_u64(bar0_base + 0x0030, acq); // ACQ
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN

    // Admin IDENTIFY (controller) command in SQ0 entry 0.
    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    pc.memory.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    pc.memory.write_u32(bar0_base + 0x1000, 1);

    pc.process_nvme();

    // Completion entry must be posted to ACQ[0].
    let mut cqe = [0u8; 16];
    pc.memory.read_physical(acq, &mut cqe);
    let dw3 = u32::from_le_bytes(cqe[12..16].try_into().unwrap());
    let cid = (dw3 & 0xffff) as u16;
    let status = (dw3 >> 16) as u16;
    assert_eq!(cid, 0x1234);
    assert_eq!(status & 0x1, 1, "phase bit should start asserted");
    assert_eq!(status & !0x1, 0, "status should indicate success");

    // Identify data should be written.
    let vid = pc.memory.read_u16(id_buf);
    assert_eq!(vid, 0x1b36);

    // Device should assert its INTx line.
    assert!(pc
        .nvme
        .as_ref()
        .expect("nvme should be enabled")
        .borrow()
        .irq_level());

    // Route INTx into the platform interrupt controller.
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("NVMe INTx should be pending via IRQ13");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 13);
}

#[test]
fn pc_platform_gates_nvme_dma_on_pci_bus_master_enable() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Program PIC offsets and unmask IRQ2 (cascade) + IRQ13 (device 3 INTA# -> PIRQD -> IRQ13).
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(13, false);
    }

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    // Enable memory decoding but keep bus mastering disabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // Configure + enable controller.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    pc.memory.write_u64(bar0_base + 0x0028, asq); // ASQ
    pc.memory.write_u64(bar0_base + 0x0030, acq); // ACQ
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN

    // Admin IDENTIFY (controller) command in SQ0 entry 0.
    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    pc.memory.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    pc.memory.write_u32(bar0_base + 0x1000, 1);

    // Clear the identify buffer so we can detect whether DMA ran.
    pc.memory.write_u16(id_buf, 0);

    // With bus mastering disabled, the platform should refuse to process and DMA must not occur.
    pc.process_nvme();
    pc.poll_pci_intx_lines();

    let mut cqe = [0u8; 16];
    pc.memory.read_physical(acq, &mut cqe);
    let dw3 = u32::from_le_bytes(cqe[12..16].try_into().unwrap());
    assert_eq!(dw3, 0, "completion queue entry should remain empty");
    assert_eq!(pc.memory.read_u16(id_buf), 0, "identify data should not be written");
    assert!(
        !pc.nvme
            .as_ref()
            .expect("nvme should be enabled")
            .borrow()
            .irq_level(),
        "NVMe device should not assert INTx before DMA processing runs"
    );
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Now enable bus mastering and retry processing; the pending command should complete.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    pc.process_nvme();

    // Completion entry must be posted to ACQ[0].
    pc.memory.read_physical(acq, &mut cqe);
    let dw3 = u32::from_le_bytes(cqe[12..16].try_into().unwrap());
    let cid = (dw3 & 0xffff) as u16;
    let status = (dw3 >> 16) as u16;
    assert_eq!(cid, 0x1234);
    assert_eq!(status & 0x1, 1, "phase bit should start asserted");
    assert_eq!(status & !0x1, 0, "status should indicate success");

    // Identify data should now be written.
    let vid = pc.memory.read_u16(id_buf);
    assert_eq!(vid, 0x1b36);

    // Device should assert its INTx line.
    assert!(pc
        .nvme
        .as_ref()
        .expect("nvme should be enabled")
        .borrow()
        .irq_level());

    // Route INTx into the platform interrupt controller.
    pc.poll_pci_intx_lines();
    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("NVMe INTx should be pending via IRQ13");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 13);
}
