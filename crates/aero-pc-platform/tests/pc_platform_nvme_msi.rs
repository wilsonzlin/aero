use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::{
    profile::NVME_CONTROLLER, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let aligned = offset & !0x3;
    let shift = (offset & 0x3) * 8;
    (read_cfg_u32(pc, bus, device, function, aligned) >> shift) as u8
}

fn read_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let aligned = offset & !0x3;
    let shift = (offset & 0x2) * 8;
    (read_cfg_u32(pc, bus, device, function, aligned) >> shift) as u16
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    assert_eq!(offset & 0x1, 0, "config u16 writes must be 2-byte aligned");
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    // Sub-dword accesses are performed by selecting the containing dword (via 0xCF8) and using the
    // corresponding byte/word offset of the 0xCFC data port.
    let data_port = PCI_CFG_DATA_PORT + u16::from(offset & 0x3);
    pc.io.write(data_port, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

fn find_pci_capability(pc: &mut PcPlatform, bdf: aero_devices::pci::PciBdf, id: u8) -> u8 {
    let mut cap_ptr = read_cfg_u8(pc, bdf.bus, bdf.device, bdf.function, 0x34);
    let mut guard = 0usize;
    while cap_ptr != 0 {
        guard += 1;
        assert!(guard <= 16, "capability list too long or cyclic");
        let cap_id = read_cfg_u8(pc, bdf.bus, bdf.device, bdf.function, cap_ptr);
        if cap_id == id {
            return cap_ptr;
        }
        cap_ptr = read_cfg_u8(
            pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            cap_ptr.wrapping_add(1),
        );
    }
    panic!("missing PCI capability {id:#x}");
}

#[test]
fn pc_platform_nvme_msi_fires_when_intx_disabled() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );
    let bdf = NVME_CONTROLLER.bdf;

    // Switch the platform into APIC mode (MSI delivers to LAPIC).
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program MSI for the NVMe controller.
    let msi_off = find_pci_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msi_off + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;

    let vector: u8 = 0x45;
    // Standard xAPIC physical destination MSI address for destination APIC ID 0.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x04,
        0xfee0_0000,
    );
    if is_64bit {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            0,
        );
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            u16::from(vector),
        );
    } else {
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            u16::from(vector),
        );
    }
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x02,
        ctrl | 0x0001,
    );

    // Enable Memory Space + Bus Mastering + INTx Disable.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

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

    // Completion must be posted.
    let dw3 = pc.memory.read_u32(acq + 12);
    let cid = (dw3 & 0xffff) as u16;
    let status = (dw3 >> 16) as u16;
    assert_eq!(cid, 0x1234);
    assert_eq!(status & 0x1, 1, "phase bit should start asserted");
    assert_eq!(status & !0x1, 0, "status should indicate success");

    // Identify data should be written.
    let vid = pc.memory.read_u16(id_buf);
    assert_eq!(vid, 0x1b36);

    let nvme = pc.nvme.as_ref().expect("NVMe enabled");
    assert!(nvme.borrow().irq_pending());
    assert!(
        !nvme.borrow().irq_level(),
        "INTx_DISABLE should suppress the device's legacy INTx line"
    );

    // Poll interrupt routing; MSI should be injected even though INTx is disabled.
    pc.poll_pci_intx_lines();

    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(vector),
        "MSI should be pending via LAPIC"
    );

    // Legacy INTx GSI must not be asserted when INTx is disabled.
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        !pc.interrupts.borrow().gsi_level(gsi),
        "NVMe INTx GSI should not be asserted when INTx is disabled"
    );

    // Simulate CPU taking the MSI interrupt.
    pc.interrupts.borrow_mut().acknowledge(vector);
    pc.interrupts.borrow_mut().eoi(vector);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // While the completion is still pending (CQ head not advanced), MSI must not be retriggered.
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Clear the completion and ensure the legacy INTx line is still not asserted.
    pc.memory.write_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert!(!pc.interrupts.borrow().gsi_level(gsi));
}

#[test]
fn pc_platform_nvme_msi_masked_interrupt_sets_pending_and_redelivers_after_unmask() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );
    let bdf = NVME_CONTROLLER.bdf;

    // Switch the platform into APIC mode (MSI delivers to LAPIC).
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program MSI for the NVMe controller, but start with the vector masked.
    let msi_off = find_pci_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msi_off + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "NVMe MSI capability should support per-vector masking for pending-bit tests"
    );

    let vector: u8 = 0x45;
    // Standard xAPIC physical destination MSI address for destination APIC ID 0.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x04,
        0xfee0_0000,
    );
    if is_64bit {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            0,
        );
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            u16::from(vector),
        );
        // Mask register for 64-bit MSI lives at +0x10. (Pending bits are device-managed and
        // read-only, so we don't attempt to clear them here.)
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x10,
            1,
        );
    } else {
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            u16::from(vector),
        );
        // Mask register for 32-bit MSI lives at +0x0c. (Pending bits are device-managed and
        // read-only, so we don't attempt to clear them here.)
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            1,
        );
    }
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x02,
        ctrl | 0x0001,
    );

    // Enable Memory Space + Bus Mastering + INTx Disable.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

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

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // First process: NVMe posts a completion and attempts MSI delivery, but the vector is masked so
    // MSI is suppressed and a pending bit is latched inside the device model.
    pc.process_nvme();
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "masked MSI should suppress delivery"
    );
    let pending_off = if is_64bit {
        msi_off + 0x14
    } else {
        msi_off + 0x10
    };
    assert_ne!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, pending_off) & 1,
        0,
        "expected MSI pending bit to be guest-visible via canonical PCI config space reads"
    );

    // Now unmask MSI in the canonical PCI config space.
    if is_64bit {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x10,
            0,
        );
    } else {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            0,
        );
    }

    // Second process: the interrupt condition is still asserted (no new rising edge), so delivery
    // should occur only if the pending bit survived canonical-config mirroring.
    pc.process_nvme();
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(vector),
        "MSI should re-deliver after unmask due to the pending bit"
    );
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, pending_off) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}

#[test]
fn pc_platform_nvme_msi_unprogrammed_address_latches_pending_and_delivers_after_programming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );
    let bdf = NVME_CONTROLLER.bdf;

    // Switch the platform into APIC mode (MSI delivers to LAPIC).
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program MSI for the NVMe controller, but leave the MSI address unprogrammed/invalid.
    let msi_off = find_pci_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msi_off + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "NVMe MSI capability should support per-vector masking for pending-bit tests"
    );

    let vector: u8 = 0x46;
    // Address low dword left as 0: invalid xAPIC MSI address.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x04,
        0,
    );
    if is_64bit {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            0,
        );
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            u16::from(vector),
        );
        // Unmask.
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x10,
            0,
        );
    } else {
        write_cfg_u16(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            u16::from(vector),
        );
        // Unmask.
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x0c,
            0,
        );
    }
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x02,
        ctrl | 0x0001,
    );

    // Enable Memory Space + Bus Mastering + INTx Disable.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

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

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // First process: NVMe posts a completion and attempts MSI delivery, but the MSI address is
    // invalid so delivery is suppressed and a pending bit is latched.
    pc.process_nvme();
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI delivery while MSI address is invalid"
    );

    let pending_off = if is_64bit {
        msi_off + 0x14
    } else {
        msi_off + 0x10
    };
    assert_ne!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, pending_off) & 1,
        0,
        "expected MSI pending bit to latch while MSI address is invalid"
    );

    // Clear the underlying NVMe interrupt condition by consuming the completion queue entry.
    //
    // Admin CQ0 head doorbell lives at BAR0 + 0x1004 (DSTRD=0, QID=0).
    pc.memory.write_u32(bar0_base + 0x1004, 1);
    assert!(
        !pc.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected NVMe interrupt condition to clear after updating CQ head"
    );

    // Second process before MSI address programming must not deliver.
    pc.process_nvme();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_ne!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, pending_off) & 1,
        0,
        "expected MSI pending bit to remain set after clearing the interrupt condition"
    );

    // Now program a valid MSI address; pending delivery must occur even though the NVMe interrupt
    // condition has been cleared.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msi_off + 0x04,
        0xfee0_0000,
    );
    if is_64bit {
        write_cfg_u32(
            &mut pc,
            bdf.bus,
            bdf.device,
            bdf.function,
            msi_off + 0x08,
            0,
        );
    }

    pc.process_nvme();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector));
    pc.interrupts.borrow_mut().acknowledge(vector);
    pc.interrupts.borrow_mut().eoi(vector);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, pending_off) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}
