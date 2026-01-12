use aero_devices::pci::profile::SATA_AHCI_ICH9;
use aero_devices::pci::{PciDevice, PciInterruptPin};
use aero_devices_storage::ata::{ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA_EXT};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
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

fn read_ahci_bar5_base(pc: &mut PcPlatform) -> u64 {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x24);
    u64::from(bar5 & 0xffff_fff0)
}

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

const HBA_GHC: u64 = 0x04;

const PORT_BASE: u64 = 0x100;

const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

const PORT_IS_DHRS: u32 = 1 << 0;

fn write_cmd_header(
    pc: &mut PcPlatform,
    clb: u64,
    slot: usize,
    ctba: u64,
    prdtl: u16,
    write: bool,
) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    pc.memory.write_u32(addr, flags);
    pc.memory.write_u32(addr + 4, 0); // PRDBC
    pc.memory.write_u32(addr + 8, ctba as u32);
    pc.memory.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(pc: &mut PcPlatform, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    pc.memory.write_u32(addr, dba as u32);
    pc.memory.write_u32(addr + 4, (dba >> 32) as u32);
    pc.memory.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    pc.memory.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(pc: &mut PcPlatform, ctba: u64, command: u8, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = command;
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    cfis[12] = (count & 0xFF) as u8;
    cfis[13] = (count >> 8) as u8;

    pc.memory.write_physical(ctba, &cfis);
}

#[test]
fn pc_platform_enumerates_ahci_and_assigns_bar5() {
    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    let bdf = SATA_AHCI_ICH9.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(SATA_AHCI_ICH9.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(SATA_AHCI_ICH9.device_id));

    let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!((class >> 8) & 0x00ff_ffff, 0x010601);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    let bar5_base = read_ahci_bar5_base(&mut pc);
    assert_ne!(bar5_base, 0, "BAR5 should be assigned during BIOS POST");
    assert_eq!(bar5_base % 0x2000, 0);

    // Smoke-test that the MMIO route is live (AHCI VS register).
    let vs = pc.memory.read_u32(bar5_base + 0x10);
    assert_eq!(vs, 0x0001_0300);
}

#[test]
fn pc_platform_ahci_mmio_supports_u64_reads_across_dword_registers() {
    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    let bar5_base = read_ahci_bar5_base(&mut pc);

    // HBA.CAP (0x00) + HBA.GHC (0x04) can be read as a single 64-bit value by the platform bus.
    let cap = pc.memory.read_u32(bar5_base) as u64;
    let ghc = pc.memory.read_u32(bar5_base + HBA_GHC) as u64;
    assert_eq!(pc.memory.read_u64(bar5_base), cap | (ghc << 32));
}

#[test]
fn pc_platform_gates_ahci_mmio_on_pci_command_register() {
    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base = read_ahci_bar5_base(&mut pc);

    // Program a known value into GHC with memory decoding enabled.
    pc.memory
        .write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC) & (GHC_AE | GHC_IE), GHC_AE | GHC_IE);

    // Disable PCI memory decoding: MMIO should behave like an unmapped region (reads return 0xFF).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC), 0xFFFF_FFFF);

    // Writes should be ignored while decoding is disabled.
    pc.memory.write_u32(bar5_base + HBA_GHC, 0);

    // Re-enable decoding: state should reflect that the write above did not reach the device.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC) & (GHC_AE | GHC_IE), GHC_AE | GHC_IE);
}

#[test]
fn pc_platform_ahci_mmio_syncs_device_command_before_each_access() {
    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base = read_ahci_bar5_base(&mut pc);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    // Simulate a stale device-side PCI command register copy (the platform maintains a separate
    // guest-facing PCI config space for enumeration).
    let ahci = pc.ahci.as_ref().expect("AHCI is enabled").clone();
    ahci.borrow_mut().config_mut().set_command(0);

    // With COMMAND.MEM disabled in the device model, direct MMIO reads return 0xFFFF_FFFF.
    assert_eq!(ahci.borrow_mut().mmio_read(0x10, 4) as u32, 0xFFFF_FFFF);

    // But through the platform's MMIO bus, accesses should still succeed because the MMIO router
    // syncs the live PCI command register into the device model before dispatch.
    assert_eq!(pc.memory.read_u32(bar5_base + 0x10), 0x0001_0300);

    // The above access should have resynchronized the device model's command register.
    assert_eq!(ahci.borrow_mut().mmio_read(0x10, 4) as u32, 0x0001_0300);

    // Now toggle COMMAND.MEMORY through the guest-facing config space, and ensure MMIO still works
    // immediately after re-enabling decode (without waiting for `process_ahci()` to sync the
    // device model command register).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    pc.process_ahci();
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    assert_eq!(ahci.borrow_mut().mmio_read(0x10, 4) as u32, 0xFFFF_FFFF);
    assert_eq!(pc.memory.read_u32(bar5_base + 0x10), 0x0001_0300);
    assert_eq!(ahci.borrow_mut().mmio_read(0x10, 4) as u32, 0x0001_0300);
}

#[test]
fn pc_platform_gates_ahci_dma_on_pci_bus_master_enable() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;

    // Unmask IRQ2 (cascade) and IRQ12 so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(12, false);
    }

    // Reprogram BAR5 within the platform's PCI MMIO window for determinism.
    let bar5_base: u64 = 0xE100_0000;
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x24, bar5_base as u32);

    // Enable memory decoding but keep bus mastering disabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // Program HBA + port 0 registers.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    pc.memory.write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Issue IDENTIFY DMA.
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    // Ensure the buffer starts cleared so we can detect whether DMA ran.
    pc.memory.write_u32(identify_buf, 0);

    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();
    pc.poll_pci_intx_lines();

    assert_eq!(
        pc.memory.read_u8(identify_buf),
        0,
        "AHCI DMA should be gated off when PCI bus mastering is disabled"
    );
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Now enable bus mastering and re-run processing; the pending command should complete.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    pc.process_ahci();
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ12 should be pending after IDENTIFY DMA completion");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 12);

    // Consume and EOI the interrupt so subsequent assertions about pending vectors are not
    // affected by the edge-triggered PIC latching semantics.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    let mut identify = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(identify_buf, &mut identify);
    assert_eq!(identify[0], 0x40);

    // Clear the interrupt and ensure it deasserts.
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_routes_ahci_mmio_after_bar5_reprogramming() {
    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    let bdf = SATA_AHCI_ICH9.bdf;

    let bar5_base = read_ahci_bar5_base(&mut pc);
    let new_base = bar5_base + 0x20_000;
    assert_eq!(new_base % 0x2000, 0);

    pc.memory
        .write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC) & (GHC_AE | GHC_IE), GHC_AE | GHC_IE);

    // Move BAR5 within the platform's PCI MMIO window.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x24,
        new_base as u32,
    );

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC), 0xFFFF_FFFF);

    // New base should decode and preserve controller state.
    assert_eq!(pc.memory.read_u32(new_base + HBA_GHC) & (GHC_AE | GHC_IE), GHC_AE | GHC_IE);
}

#[test]
fn pc_platform_ahci_dma_and_intx_routing_work() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;

    // Unmask IRQ2 (cascade) and IRQ12 so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(12, false);
    }

    // Reprogram BAR5 within the platform's PCI MMIO window.
    let bar5_base: u64 = 0xE100_0000;
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x24, bar5_base as u32);

    // BAR5 MMIO should be gated by COMMAND.MEMORY (bit 1).
    assert_eq!(
        pc.memory.read_u32(bar5_base + HBA_GHC) & (GHC_AE | GHC_IE),
        GHC_AE
    );
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    assert_eq!(pc.memory.read_u32(bar5_base + HBA_GHC), 0xFFFF_FFFF);

    // Writes should be ignored while decoding is disabled.
    pc.memory.write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    assert_eq!(
        pc.memory.read_u32(bar5_base + HBA_GHC) & (GHC_AE | GHC_IE),
        GHC_AE
    );

    // Enable bus mastering so DMA is permitted (keep memory decoding enabled).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Program HBA + port 0 registers.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    pc.memory
        .write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // IDENTIFY DMA.
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();

    // Route device IRQ line into the platform interrupt controller.
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ12 should be pending after INTx routing");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 12);

    // Consume and EOI the interrupt so subsequent assertions about pending vectors are not
    // affected by the edge-triggered PIC latching semantics.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    let mut identify = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(identify_buf, &mut identify);
    assert_eq!(identify[0], 0x40);

    // Clear the interrupt and ensure it deasserts.
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // READ DMA EXT for LBA 0, 1 sector.
    let read_buf = 0x5000u64;
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    write_prdt(&mut pc, ctba, 0, read_buf, SECTOR_SIZE as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();

    let mut out = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");
    assert_eq!(&out[510..512], &[0x55, 0xAA]);

    // Disable INTx via PCI command bit 10 while keeping interrupts pending: the PIC should not
    // observe an interrupt until it is re-enabled.
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
        Some(12)
    );
}

#[test]
fn pc_platform_ahci_dma_writes_mark_dirty_pages_when_enabled() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut pc = PcPlatform::new_with_ahci_dirty_tracking(2 * 1024 * 1024);
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;

    // Reprogram BAR5 within the platform's PCI MMIO window.
    let bar5_base: u64 = 0xE100_0000;
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x24, bar5_base as u32);

    // Enable bus mastering so DMA is permitted (keep memory decoding enabled).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Program HBA + port 0 registers.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    pc.memory.write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    // Clear dirty tracking for CPU-initiated setup writes. We want to observe only the DMA
    // writes performed by the device model.
    pc.memory.clear_dirty();

    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();

    let mut identify = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(identify_buf, &mut identify);
    assert_eq!(identify[0], 0x40);

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_page = identify_buf / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_page),
        "dirty pages should include IDENTIFY DMA buffer page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_routes_ahci_intx_via_ioapic_in_apic_mode() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    disk.write_sectors(0, &[0u8; SECTOR_SIZE]).unwrap();

    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    // Switch the platform into APIC mode via IMCR (0x22/0x23).
    pc.io.write_u8(0x22, 0x70);
    pc.io.write_u8(0x23, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route the AHCI INTx line to vector 0x60, level-triggered + active-low.
    let vector = 0x60u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let bdf = SATA_AHCI_ICH9.bdf;
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    // Reprogram BAR5 within the platform's PCI MMIO window for determinism.
    let bar5_base: u64 = 0xE100_0000;
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x24, bar5_base as u32);

    // Enable memory decoding + bus mastering so MMIO works and DMA is permitted.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Program HBA + port 0 registers and issue an IDENTIFY DMA command.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    pc.memory.write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    pc.memory.write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();
    pc.poll_pci_intx_lines();

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Acknowledge the interrupt (vector in service).
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the controller IRQ and propagate the deassertion before sending EOI, so we don't
    // immediately retrigger due to the level-triggered line remaining high.
    pc.memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}
