use aero_devices::pci::{
    profile::NVME_CONTROLLER, PciDevice as _, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_devices_nvme::NvmeController;
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
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
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(PCI_CFG_DATA_PORT, 4)
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

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

fn build_command(opc: u8) -> [u8; 64] {
    let mut cmd = [0u8; 64];
    cmd[0] = opc;
    cmd
}

fn set_cid(cmd: &mut [u8; 64], cid: u16) {
    cmd[2..4].copy_from_slice(&cid.to_le_bytes());
}

fn set_nsid(cmd: &mut [u8; 64], nsid: u32) {
    cmd[4..8].copy_from_slice(&nsid.to_le_bytes());
}

fn set_prp1(cmd: &mut [u8; 64], prp1: u64) {
    cmd[24..32].copy_from_slice(&prp1.to_le_bytes());
}

fn set_prp2(cmd: &mut [u8; 64], prp2: u64) {
    cmd[32..40].copy_from_slice(&prp2.to_le_bytes());
}

fn set_cdw10(cmd: &mut [u8; 64], val: u32) {
    cmd[40..44].copy_from_slice(&val.to_le_bytes());
}

fn set_cdw11(cmd: &mut [u8; 64], val: u32) {
    cmd[44..48].copy_from_slice(&val.to_le_bytes());
}

fn set_cdw12(cmd: &mut [u8; 64], val: u32) {
    cmd[48..52].copy_from_slice(&val.to_le_bytes());
}

#[derive(Debug)]
struct CqEntry {
    cid: u16,
    status: u16,
}

fn read_cqe(pc: &mut PcPlatform, addr: u64) -> CqEntry {
    let mut bytes = [0u8; 16];
    pc.memory.read_physical(addr, &mut bytes);
    let dw3 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    CqEntry {
        cid: (dw3 & 0xffff) as u16,
        status: (dw3 >> 16) as u16,
    }
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
fn pc_platform_nvme_mmio_is_gated_by_pci_command_mem() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    let cap_lo = pc.memory.read_u32(bar0_base);
    assert_ne!(
        cap_lo, 0xffff_ffff,
        "CAP should be readable when COMMAND.MEM is enabled"
    );
    assert_ne!(
        pc.nvme
            .as_ref()
            .expect("nvme should be enabled")
            .borrow()
            .config()
            .command()
            & 0x2,
        0,
        "platform MMIO handler should sync COMMAND.MEM into the NVMe device model"
    );

    // Disable Memory Space Enable (COMMAND.MEM = bit 1).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);

    let cap_lo_disabled = pc.memory.read_u32(bar0_base);
    assert_eq!(
        cap_lo_disabled, 0xffff_ffff,
        "BAR0 reads should return all-ones when COMMAND.MEM=0"
    );

    // Writes while decoding is disabled must not change device state.
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN = 1 (would normally be observable via CC)

    // Re-enable memory decoding.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    let cap_lo_reenabled = pc.memory.read_u32(bar0_base);
    assert_ne!(
        cap_lo_reenabled, 0xffff_ffff,
        "BAR0 should decode again when COMMAND.MEM is re-enabled"
    );
    let dev_command = pc
        .nvme
        .as_ref()
        .expect("nvme should be enabled")
        .borrow()
        .config()
        .command();
    assert_eq!(dev_command, 0x0002);

    let cc = pc.memory.read_u32(bar0_base + 0x0014);
    assert_eq!(
        cc, 0,
        "writes while COMMAND.MEM=0 should not reach the NVMe controller"
    );

    // With decoding enabled again, MMIO writes should take effect.
    pc.memory.write_u32(bar0_base + 0x0014, 1);
    let cc_after = pc.memory.read_u32(bar0_base + 0x0014);
    assert_eq!(cc_after & 1, 1);
}

#[test]
fn pc_platform_nvme_mmio_syncs_device_command_before_each_access() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    let nvme = pc.nvme.as_ref().expect("NVMe enabled").clone();

    // Simulate a stale device-side PCI command register copy.
    nvme.borrow_mut().config_mut().set_command(0);

    // With COMMAND.MEM disabled in the device model, direct MMIO reads return all-ones.
    assert_eq!(
        memory::MmioHandler::read(&mut *nvme.borrow_mut(), 0x0000, 4) as u32,
        0xFFFF_FFFF
    );

    // Through the platform MMIO bus, the access should still succeed because the MMIO router
    // mirrors the live PCI command register into the device model before dispatch.
    let cap_platform = pc.memory.read_u32(bar0_base);
    assert_ne!(cap_platform, 0xFFFF_FFFF);

    // And the device model should now observe a synced command register.
    assert_eq!(
        memory::MmioHandler::read(&mut *nvme.borrow_mut(), 0x0000, 4) as u32,
        cap_platform
    );
}

#[test]
fn pc_platform_nvme_bar0_relocation_is_honored_by_mmio_routing() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Ensure the platform routes MMIO.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    let old_base = read_nvme_bar0_base(&mut pc);
    assert_ne!(old_base, 0, "BAR0 should be assigned during BIOS POST");

    // Touch state at the old base so we can ensure it survives BAR reprogramming.
    // INTMS is write-1-to-set.
    pc.memory.write_u32(old_base + 0x000c, 0x1234_5678);
    assert_eq!(pc.memory.read_u32(old_base + 0x000c), 0x1234_5678);

    // Pick a new aligned base within the platform's PCI MMIO window.
    let bar_len = NvmeController::bar0_len();
    let new_base = old_base + (bar_len * 16);
    assert_eq!(
        new_base % bar_len,
        0,
        "new BAR0 base must be aligned to its size"
    );
    assert_ne!(new_base, old_base);

    // Program the new BAR0 base (64-bit BAR: low dword then high dword).
    let bar0_flags = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10) & 0xF;
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        (new_base as u32) | bar0_flags,
    );
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x14,
        (new_base >> 32) as u32,
    );
    assert_eq!(read_nvme_bar0_base(&mut pc), new_base);

    // Old base must no longer decode.
    let cap_old = pc.memory.read_u32(old_base);
    assert_eq!(cap_old, 0xffff_ffff, "old BAR0 base should not route");

    // New base should decode.
    let cap_new = pc.memory.read_u32(new_base);
    assert_ne!(cap_new, 0xffff_ffff, "new BAR0 base should route");

    // Controller state must be preserved when BAR0 moves.
    assert_eq!(pc.memory.read_u32(new_base + 0x000c), 0x1234_5678);

    // Writes at the new base should take effect.
    pc.memory.write_u32(new_base + 0x0024, 0x000f_000f); // AQA
    let aqa = pc.memory.read_u32(new_base + 0x0024);
    assert_eq!(aqa, 0x000f_000f);

    // Writes at the old base should be ignored.
    pc.memory.write_u32(old_base + 0x0024, 0x0001_0001);
    let aqa_after = pc.memory.read_u32(new_base + 0x0024);
    assert_eq!(aqa_after, 0x000f_000f);
}

#[test]
fn pc_platform_nvme_admin_identify_produces_completion_and_intx() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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
fn pc_platform_routes_nvme_intx_via_ioapic_in_apic_mode() {
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

    // Switch the platform into APIC mode via IMCR.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route the NVMe INTx line to vector 0x61, level-triggered + active-low.
    let vector = 0x61u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

    assert!(
        pc.nvme
            .as_ref()
            .expect("nvme should be enabled")
            .borrow()
            .irq_level(),
        "NVMe should assert INTx when a completion is pending"
    );

    pc.poll_pci_intx_lines();

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Simulate CPU taking the interrupt.
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Consume the completion by advancing CQ0 head, which should deassert INTx.
    pc.memory.write_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    pc.poll_pci_intx_lines();
    assert!(
        !pc.nvme
            .as_ref()
            .expect("nvme should be enabled")
            .borrow()
            .irq_level()
    );

    // End-of-interrupt should *not* cause a redelivery now that the line is deasserted.
    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_nvme_intx() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

    // Consume and EOI the interrupt so subsequent assertions about pending vectors are not
    // affected by the edge-triggered PIC latching semantics.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Disable INTx via PCI command bit 10 while the device still has a completion pending.
    // The PIC should not observe an interrupt until it is re-enabled.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0006 | (1 << 10),
    );
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Re-enable INTx and ensure the asserted line is delivered.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts
            .borrow()
            .pic()
            .get_pending_vector()
            .and_then(|v| pc.interrupts.borrow().pic().vector_to_irq(v)),
        Some(13)
    );
}

#[test]
fn pc_platform_resyncs_nvme_pci_command_before_polling_intx_level() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

    // Process the admin queue so a completion is posted and INTx becomes asserted.
    pc.process_nvme();
    let nvme = pc.nvme.as_ref().expect("NVMe should be enabled").clone();
    assert!(
        nvme.borrow().irq_level(),
        "NVMe should assert INTx when a completion is pending"
    );

    // Disable INTx in PCI command register (bit 10), then call `process_nvme` so the NVMe model's
    // internal PCI config copy observes the new command register value.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        0x0006 | (1 << 10),
    );
    pc.process_nvme();
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe model should suppress its IRQ when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx in guest-facing PCI config space without calling `process_nvme`. This leaves
    // the device model with a stale copy of the PCI command register (INTx still disabled).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe model should still see stale INTx disable bit until the platform resyncs PCI config"
    );

    // Polling INTx lines must resync PCI command state before querying the device model so the
    // cleared INTx disable bit takes effect immediately.
    pc.poll_pci_intx_lines();
    assert!(
        nvme.borrow().irq_level(),
        "poll_pci_intx_lines should resync PCI command and expose the pending NVMe interrupt"
    );
    assert_eq!(
        pc.interrupts
            .borrow()
            .pic()
            .get_pending_vector()
            .and_then(|v| pc.interrupts.borrow().pic().vector_to_irq(v)),
        Some(13)
    );
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
    assert_eq!(
        pc.memory.read_u16(id_buf),
        0,
        "identify data should not be written"
    );
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

#[test]
fn pc_platform_nvme_dma_writes_mark_dirty_pages_when_enabled() {
    let mut pc = PcPlatform::new_with_nvme_dirty_tracking(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

    // Clear dirty tracking for CPU-initiated setup writes. We want to observe only the DMA writes
    // performed by the device model.
    pc.memory.clear_dirty();

    // Ring SQ0 tail doorbell and process DMA.
    pc.memory.write_u32(bar0_base + 0x1000, 1);
    pc.process_nvme();

    // Identify data should be written.
    let vid = pc.memory.read_u16(id_buf);
    assert_eq!(vid, 0x1b36);

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_identify_page = id_buf / page_size;
    let expected_cq_page = acq / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_identify_page),
        "dirty pages should include IDENTIFY DMA buffer page (got {dirty:?})"
    );
    assert!(
        dirty.contains(&expected_cq_page),
        "dirty pages should include completion queue page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_new_with_nvme_disk_reflects_backend_capacity_in_identify_namespace() {
    const DISK_SECTORS: u64 = 2048;
    let disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    let mut pc = PcPlatform::new_with_nvme_disk(2 * 1024 * 1024, Box::new(disk));
    let bdf = NVME_CONTROLLER.bdf;

    // Enable Memory Space + Bus Mastering so the platform allows DMA processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Admin IDENTIFY (namespace) command in SQ0 entry 0.
    let mut cmd = build_command(0x06);
    set_cid(&mut cmd, 0x5678);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, id_buf);
    set_cdw10(&mut cmd, 0x00); // CNS=0 (namespace)
    pc.memory.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    pc.memory.write_u32(bar0_base + 0x1000, 1);

    pc.process_nvme();

    let cqe = read_cqe(&mut pc, acq);
    assert_eq!(cqe.cid, 0x5678);
    assert_eq!(cqe.status & !0x1, 0);

    let nsze = pc.memory.read_u64(id_buf);
    assert_eq!(nsze, DISK_SECTORS);
}

#[test]
fn pc_platform_new_with_nvme_disk_uses_backend_for_read_and_write_io() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const DISK_SECTORS: u64 = 16;

    // Seed a disk sector with a recognizable marker so a READ command proves the provided backend
    // is actually being used by the NVMe device model.
    let mut disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    disk.write_at(0, &[9, 8, 7, 6]).unwrap();

    let mut pc = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk));
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    // Allow bus mastering for NVMe DMA (queues + data buffers).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;
    let write_buf = 0x60000u64;
    let read_buf = 0x61000u64;

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so IO CQ entries start at offset 0 and INTx reflects I/O only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    // READ 1 sector at LBA 0 (should observe the seeded marker).
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x20);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0); // slba low
    set_cdw11(&mut cmd, 0); // slba high
    set_cdw12(&mut cmd, 0); // nlb = 0 (1 sector)
    pc.memory.write_physical(io_sq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq);
    assert_eq!(cqe.cid, 0x20);
    assert_eq!(cqe.status & !0x1, 0);
    let mut first4 = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut first4);
    assert_eq!(first4, [9, 8, 7, 6]);

    // WRITE a full sector, then READ it back.
    let payload: Vec<u8> = (0..SECTOR_SIZE).map(|v| (v & 0xff) as u8).collect();
    pc.memory.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x21);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 2); // SQ1 tail = 2
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 16);
    assert_eq!(cqe.cid, 0x21);
    assert_eq!(cqe.status & !0x1, 0);

    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x22);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq + 128, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 3); // SQ1 tail = 3
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 32);
    assert_eq!(cqe.cid, 0x22);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);

    // READ beyond the injected disk capacity should return an error status.
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x23);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, DISK_SECTORS as u32); // OOB: valid LBAs are 0..DISK_SECTORS-1
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq + 192, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 4); // SQ1 tail = 4
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 48);
    assert_eq!(cqe.cid, 0x23);
    assert_ne!(
        cqe.status & !0x1,
        0,
        "OOB read should return a non-success NVMe status"
    );

    // WRITE beyond the injected disk capacity should return an error status.
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x24);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, DISK_SECTORS as u32); // OOB: valid LBAs are 0..DISK_SECTORS-1
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq + 256, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 5); // SQ1 tail = 5
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 64);
    assert_eq!(cqe.cid, 0x24);
    assert_ne!(
        cqe.status & !0x1,
        0,
        "OOB write should return a non-success NVMe status"
    );
}

#[test]
fn pc_platform_nvme_prp2_cross_page_write_and_read_roundtrip() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const DISK_SECTORS: u64 = 128;
    const SECTORS: u32 = 10; // 5120 bytes, spans PRP1+PRP2
    const PAGE_SIZE: usize = 4096;

    let disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    let mut pc = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk));

    let bdf = NVME_CONTROLLER.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;

    let write_page0 = 0x70000u64;
    let write_page1 = write_page0 + 0x1000;
    let read_page0 = 0x72000u64;
    let read_page1 = read_page0 + 0x1000;

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so INTx reflects the I/O queue only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    let len = (SECTORS as usize) * SECTOR_SIZE;
    let payload: Vec<u8> = (0..len).map(|v| (v & 0xff) as u8).collect();
    assert!(len > PAGE_SIZE);
    assert!(len <= PAGE_SIZE + SECTOR_SIZE * 4);

    pc.memory
        .write_physical(write_page0, &payload[..PAGE_SIZE]);
    pc.memory
        .write_physical(write_page1, &payload[PAGE_SIZE..]);

    // WRITE spanning PRP1+PRP2.
    let cid_write: u16 = 0x40;
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, cid_write);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_page0);
    set_prp2(&mut cmd, write_page1);
    set_cdw10(&mut cmd, 0); // LBA low
    set_cdw11(&mut cmd, 0); // LBA high
    set_cdw12(&mut cmd, SECTORS - 1); // nlb
    pc.memory.write_physical(io_sq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq);
    assert_eq!(cqe.cid, cid_write);
    assert_eq!(cqe.status & !0x1, 0);

    // READ spanning PRP1+PRP2.
    pc.memory.write_physical(read_page0, &vec![0u8; PAGE_SIZE]);
    pc.memory.write_physical(read_page1, &vec![0u8; PAGE_SIZE]);

    let cid_read: u16 = 0x41;
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, cid_read);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_page0);
    set_prp2(&mut cmd, read_page1);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, SECTORS - 1);
    pc.memory.write_physical(io_sq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 2); // SQ1 tail = 2
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 16);
    assert_eq!(cqe.cid, cid_read);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    pc.memory.read_physical(read_page0, &mut out[..PAGE_SIZE]);
    pc.memory.read_physical(read_page1, &mut out[PAGE_SIZE..]);
    assert_eq!(out, payload);
}

#[test]
fn pc_platform_nvme_prp_list_multi_page_write_and_read_roundtrip() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const DISK_SECTORS: u64 = 128;
    const SECTORS: u32 = 20; // 10240 bytes, spans PRP list
    const PAGE_SIZE: usize = 4096;

    let disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    let mut pc = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk));

    let bdf = NVME_CONTROLLER.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;

    let write_page0 = 0x74000u64;
    let write_page1 = write_page0 + 0x1000;
    let write_page2 = write_page1 + 0x1000;
    let write_prp_list = write_page2 + 0x1000;

    let read_page0 = 0x78000u64;
    let read_page1 = read_page0 + 0x1000;
    let read_page2 = read_page1 + 0x1000;
    let read_prp_list = read_page2 + 0x1000;

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so INTx reflects the I/O queue only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    let len = (SECTORS as usize) * SECTOR_SIZE;
    let payload: Vec<u8> = (0..len).map(|v| (v & 0xff) as u8).collect();
    assert!(len > 2 * PAGE_SIZE);

    // Build the PRP list entries pointing at the remaining pages.
    pc.memory.write_u64(write_prp_list, write_page1);
    pc.memory.write_u64(write_prp_list + 8, write_page2);

    pc.memory
        .write_physical(write_page0, &payload[..PAGE_SIZE]);
    pc.memory
        .write_physical(write_page1, &payload[PAGE_SIZE..PAGE_SIZE * 2]);
    pc.memory
        .write_physical(write_page2, &payload[PAGE_SIZE * 2..]);

    // WRITE spanning PRP list.
    let cid_write: u16 = 0x50;
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, cid_write);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_page0);
    set_prp2(&mut cmd, write_prp_list);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, SECTORS - 1);
    pc.memory.write_physical(io_sq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq);
    assert_eq!(cqe.cid, cid_write);
    assert_eq!(cqe.status & !0x1, 0);

    // READ spanning PRP list.
    pc.memory.write_u64(read_prp_list, read_page1);
    pc.memory.write_u64(read_prp_list + 8, read_page2);

    pc.memory.write_physical(read_page0, &vec![0u8; PAGE_SIZE]);
    pc.memory.write_physical(read_page1, &vec![0u8; PAGE_SIZE]);
    pc.memory.write_physical(read_page2, &vec![0u8; PAGE_SIZE]);

    let cid_read: u16 = 0x51;
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, cid_read);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_page0);
    set_prp2(&mut cmd, read_prp_list);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, SECTORS - 1);
    pc.memory.write_physical(io_sq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 2); // SQ1 tail = 2
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 16);
    assert_eq!(cqe.cid, cid_read);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    pc.memory.read_physical(read_page0, &mut out[..PAGE_SIZE]);
    pc.memory
        .read_physical(read_page1, &mut out[PAGE_SIZE..PAGE_SIZE * 2]);
    pc.memory.read_physical(read_page2, &mut out[PAGE_SIZE * 2..]);
    assert_eq!(out, payload);
}

#[test]
fn pc_platform_nvme_snapshot_restore_processes_pending_io_without_renotify() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const DISK_SECTORS: u64 = 16;
    const SEED: [u8; 4] = [9, 8, 7, 6];

    let mut disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    disk.write_at(0, &SEED).unwrap();
    let mut pc = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk));

    let bdf = NVME_CONTROLLER.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;
    let read_buf = 0x61000u64;

    // Ensure guest buffers are deterministic.
    pc.memory.write_physical(io_cq, &[0u8; 64]);
    pc.memory.write_physical(io_sq, &[0u8; 64]);
    pc.memory.write_physical(read_buf, &[0u8; SECTOR_SIZE]);

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so INTx reflects the I/O queue only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    // Post a READ to IO SQ1, but snapshot before the platform processes it.
    let cid: u16 = 0x33;
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, cid);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq, &cmd);

    // Ring doorbell but do not call `process_nvme()` yet.
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1

    let mut first4 = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut first4);
    assert_eq!(first4, [0, 0, 0, 0]);
    assert_eq!(read_cqe(&mut pc, io_cq).cid, 0);

    let dev_snap = pc.nvme.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; RAM_SIZE];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform, re-attaching the host disk backend at construction time.
    let mut disk2 = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    disk2.write_at(0, &SEED).unwrap();
    let mut restored = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk2));
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .nvme
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Without re-notifying, the platform should still process the pending SQ entry.
    restored.process_nvme();

    let cqe = read_cqe(&mut restored, io_cq);
    assert_eq!(cqe.cid, cid);
    assert_eq!(cqe.status & !0x1, 0);

    restored.memory.read_physical(read_buf, &mut first4);
    assert_eq!(first4, SEED);

    // Calling `process_nvme()` again without a new doorbell must not duplicate the completion.
    assert_eq!(read_cqe(&mut restored, io_cq + 16).cid, 0);
    restored.process_nvme();
    assert_eq!(read_cqe(&mut restored, io_cq + 16).cid, 0);
}

#[test]
fn pc_platform_nvme_snapshot_restore_processes_pending_write_without_renotify() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const DISK_SECTORS: u64 = 16;

    let disk = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    let mut pc = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk));
    let bdf = NVME_CONTROLLER.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_nvme_bar0_base(&mut pc);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;
    let write_buf = 0x60000u64;
    let read_buf = 0x61000u64;

    // Ensure guest buffers are deterministic.
    pc.memory.write_physical(io_cq, &[0u8; 64]);
    pc.memory.write_physical(io_sq, &[0u8; 64]);
    pc.memory.write_physical(write_buf, &[0u8; SECTOR_SIZE]);
    pc.memory.write_physical(read_buf, &[0u8; SECTOR_SIZE]);

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so INTx reflects the I/O queue only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    // Post a WRITE to IO SQ1, but snapshot before the platform processes it.
    let payload: Vec<u8> = (0..SECTOR_SIZE).map(|v| (v & 0xff) as u8).collect();
    pc.memory.write_physical(write_buf, &payload);

    let cid: u16 = 0x44;
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, cid);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0); // nlb=0 (1 sector)
    pc.memory.write_physical(io_sq, &cmd);

    // Ring doorbell but do not call `process_nvme()` yet.
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1

    // Sanity-check preconditions: no completion and read buffer still zeroed.
    let mut first4 = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut first4);
    assert_eq!(first4, [0, 0, 0, 0]);
    assert_eq!(read_cqe(&mut pc, io_cq).cid, 0);

    let dev_snap = pc.nvme.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; RAM_SIZE];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform, re-attaching the host disk backend at construction time.
    let disk2 = RawDisk::create(MemBackend::new(), DISK_SECTORS * SECTOR_SIZE as u64)
        .expect("failed to allocate in-memory NVMe disk");
    let mut restored = PcPlatform::new_with_nvme_disk(RAM_SIZE, Box::new(disk2));
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .nvme
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Without re-notifying, the platform should still process the pending SQ entry.
    restored.process_nvme();

    let cqe = read_cqe(&mut restored, io_cq);
    assert_eq!(cqe.cid, cid);
    assert_eq!(cqe.status & !0x1, 0);

    // Calling `process_nvme()` again without a new doorbell must not duplicate the completion.
    assert_eq!(read_cqe(&mut restored, io_cq + 16).cid, 0);
    restored.process_nvme();
    assert_eq!(read_cqe(&mut restored, io_cq + 16).cid, 0);

    // READ back and ensure the disk observed the write.
    restored.memory.write_physical(read_buf, &[0u8; SECTOR_SIZE]);
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, cid.wrapping_add(1));
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    restored.memory.write_physical(io_sq + 64, &cmd);
    restored.memory.write_u32(bar0_base + 0x1008, 2); // SQ1 tail = 2
    restored.process_nvme();

    let cqe = read_cqe(&mut restored, io_cq + 16);
    assert_eq!(cqe.cid, cid.wrapping_add(1));
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    restored.memory.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);
}

#[test]
fn pc_platform_nvme_bar0_rw_flush_roundtrip() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    // Allow bus mastering for NVMe DMA (queues + data buffers).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let io_cq = 0x40000u64;
    let io_sq = 0x50000u64;
    let write_buf = 0x60000u64;
    let read_buf = 0x61000u64;

    // Admin SQ/CQ setup and enable.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // 16/16 queues
    pc.memory.write_u64(bar0_base + 0x0028, asq);
    pc.memory.write_u64(bar0_base + 0x0030, acq);
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    // Create IO CQ (qid=1, size=16, PC+IEN).
    let mut cmd = build_command(0x05);
    set_cid(&mut cmd, 1);
    set_prp1(&mut cmd, io_cq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 0x3);
    pc.memory.write_physical(asq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 1); // SQ0 tail = 1
    pc.process_nvme();

    // Create IO SQ (qid=1, size=16, CQID=1).
    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 2);
    set_prp1(&mut cmd, io_sq);
    set_cdw10(&mut cmd, (15u32 << 16) | 1);
    set_cdw11(&mut cmd, 1);
    pc.memory.write_physical(asq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1000, 2); // SQ0 tail = 2
    pc.process_nvme();

    // Consume admin CQ completions so INTx reflects the I/O queue only.
    pc.memory.write_u32(bar0_base + 0x1004, 2); // CQ0 head = 2

    // WRITE 1 sector at LBA 0.
    let payload: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    pc.memory.write_physical(write_buf, &payload);

    let mut cmd = build_command(0x01);
    set_cid(&mut cmd, 0x10);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, write_buf);
    set_cdw10(&mut cmd, 0); // slba low
    set_cdw11(&mut cmd, 0); // slba high
    set_cdw12(&mut cmd, 0); // nlb = 0 (1 sector)
    pc.memory.write_physical(io_sq, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 1); // SQ1 tail = 1
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq);
    assert_eq!(cqe.cid, 0x10);
    assert_eq!(cqe.status & !0x1, 0);

    // READ it back.
    let mut cmd = build_command(0x02);
    set_cid(&mut cmd, 0x11);
    set_nsid(&mut cmd, 1);
    set_prp1(&mut cmd, read_buf);
    set_cdw10(&mut cmd, 0);
    set_cdw11(&mut cmd, 0);
    set_cdw12(&mut cmd, 0);
    pc.memory.write_physical(io_sq + 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 2); // SQ1 tail = 2
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 16);
    assert_eq!(cqe.cid, 0x11);
    assert_eq!(cqe.status & !0x1, 0);

    let mut out = vec![0u8; payload.len()];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(out, payload);

    // FLUSH.
    let mut cmd = build_command(0x00);
    set_cid(&mut cmd, 0x12);
    set_nsid(&mut cmd, 1);
    pc.memory.write_physical(io_sq + 2 * 64, &cmd);
    pc.memory.write_u32(bar0_base + 0x1008, 3); // SQ1 tail = 3
    pc.process_nvme();

    let cqe = read_cqe(&mut pc, io_cq + 2 * 16);
    assert_eq!(cqe.cid, 0x12);
    assert_eq!(cqe.status & !0x1, 0);
}
