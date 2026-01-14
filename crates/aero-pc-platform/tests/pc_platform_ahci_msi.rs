use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::profile::{AHCI_ABAR_BAR_INDEX, SATA_AHCI_ICH9};
use aero_devices::pci::PciBdf;
use aero_devices_storage::ata::ATA_CMD_IDENTIFY;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::MemoryBus as _;

mod helpers;
use helpers::*;

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

fn find_capability(pc: &mut PcPlatform, bdf: PciBdf, cap_id: u8) -> u16 {
    let mut cap_ptr = pci_cfg_read_u8(pc, bdf, 0x34);
    let mut guard = 0usize;
    while cap_ptr != 0 {
        guard += 1;
        assert!(guard <= 16, "capability list too long or cyclic");
        let id = pci_cfg_read_u8(pc, bdf, u16::from(cap_ptr));
        if id == cap_id {
            return u16::from(cap_ptr);
        }
        cap_ptr = pci_cfg_read_u8(pc, bdf, u16::from(cap_ptr.wrapping_add(1)));
    }
    panic!("PCI capability {cap_id:#x} not found for {bdf:?}");
}

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

    mem_write(pc, ctba, &cfis);
}

#[test]
fn pc_platform_triggers_ahci_msi_when_enabled_even_if_intx_disabled() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            // Keep the platform minimal to avoid unrelated device interrupts affecting the test.
            enable_uhci: false,
            ..Default::default()
        },
    );

    // Switch the platform into APIC mode so we can observe MSI delivery via the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = SATA_AHCI_ICH9.bdf;

    // Enable memory decoding + bus mastering so the controller can DMA and raise interrupts, but
    // disable legacy INTx so the test observes MSI delivery exclusively.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= (1 << 1) | (1 << 2) | (1 << 10); // MEM | BME | INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Program the MSI capability.
    let cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    pci_cfg_write_u32(&mut pc, bdf, cap + 0x04, 0xFEE0_0000); // addr low (dest=0)
    pci_cfg_write_u32(&mut pc, bdf, cap + 0x08, 0); // addr high
    pci_cfg_write_u16(&mut pc, bdf, cap + 0x0C, 0x0045); // vector=0x45
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, cap + 0x02);
    pci_cfg_write_u16(&mut pc, bdf, cap + 0x02, ctrl | 0x0001); // enable

    let abar = pci_read_bar(&mut pc, bdf, AHCI_ABAR_BAR_INDEX);
    assert_eq!(abar.kind, BarKind::Mem32);
    assert_ne!(abar.base, 0);

    // Guest memory layout for command list + table + DMA buffer.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let identify_buf = alloc.alloc_bytes(512, 512);

    // Program AHCI registers (port 0).
    pc.memory.write_u32(abar.base + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        abar.base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Clear any prior port interrupt state and build an IDENTIFY command.
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, 512);

    pc.memory.write_u32(abar.base + PORT_BASE + PORT_REG_CI, 1);
    pc.process_ahci();

    // Convert the controller's asserted interrupt state into an MSI delivery.
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(0x45));

    // Ensure we don't spam MSI while the controller's level remains asserted.
    pc.interrupts.borrow_mut().acknowledge(0x45);
    pc.interrupts.borrow_mut().eoi(0x45);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Sanity check: DMA ran.
    let mut identify = [0u8; 512];
    mem_read(&mut pc, identify_buf, &mut identify);
    assert_eq!(identify[0], 0x40);
}

#[test]
fn pc_platform_ahci_msi_masked_interrupt_sets_pending_and_redelivers_after_unmask() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            // Keep the platform minimal to avoid unrelated device interrupts affecting the test.
            enable_uhci: false,
            ..Default::default()
        },
    );

    // Switch the platform into APIC mode so we can observe MSI delivery via the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = SATA_AHCI_ICH9.bdf;

    // Enable memory decoding + bus mastering so the controller can DMA and raise interrupts, but
    // disable legacy INTx so the test observes MSI delivery exclusively.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= (1 << 1) | (1 << 2) | (1 << 10); // MEM | BME | INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Program the MSI capability with the single vector masked.
    let cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, cap + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "AHCI MSI capability should support per-vector masking"
    );
    let pending_off = if is_64bit { cap + 0x14 } else { cap + 0x10 };

    let vector: u8 = 0x46;
    pci_cfg_write_u32(&mut pc, bdf, cap + 0x04, 0xFEE0_0000); // addr low (dest=0)
    if is_64bit {
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x08, 0); // addr high
        pci_cfg_write_u16(&mut pc, bdf, cap + 0x0C, u16::from(vector)); // data
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x10, 1); // mask
    } else {
        pci_cfg_write_u16(&mut pc, bdf, cap + 0x08, u16::from(vector)); // data
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x0C, 1); // mask
    }
    pci_cfg_write_u16(&mut pc, bdf, cap + 0x02, ctrl | 0x0001); // enable

    let abar = pci_read_bar(&mut pc, bdf, AHCI_ABAR_BAR_INDEX);
    assert_eq!(abar.kind, BarKind::Mem32);
    assert_ne!(abar.base, 0);

    // Guest memory layout for command list + table + DMA buffer.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let identify_buf = alloc.alloc_bytes(512, 512);

    // Program AHCI registers (port 0).
    pc.memory.write_u32(abar.base + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        abar.base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Clear any prior port interrupt state and build an IDENTIFY command.
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, 512);

    pc.memory.write_u32(abar.base + PORT_BASE + PORT_REG_CI, 1);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    pc.process_ahci();

    // Polling INTx should not produce any interrupt (INTx is disabled and MSI is masked).
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_ne!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to be guest-visible while vector is masked"
    );

    // Unmask MSI in canonical PCI config space.
    if is_64bit {
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x10, 0);
    } else {
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x0C, 0);
    }

    // Sync the new MSI mask into the device model via the platform's regular polling path.
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // The controller's interrupt condition is still asserted (status bits uncleared), but MSI
    // delivery is edge-triggered; it should only re-deliver due to the pending bit that was latched
    // while masked.
    pc.process_ahci();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector));
    assert_eq!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}

#[test]
fn pc_platform_ahci_msi_unprogrammed_address_latches_pending_and_delivers_after_programming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            // Keep the platform minimal to avoid unrelated device interrupts affecting the test.
            enable_uhci: false,
            ..Default::default()
        },
    );

    // Switch the platform into APIC mode so we can observe MSI delivery via the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = SATA_AHCI_ICH9.bdf;

    // Enable memory decoding + bus mastering so the controller can DMA and raise interrupts, but
    // disable legacy INTx so the test observes MSI delivery exclusively.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= (1 << 1) | (1 << 2) | (1 << 10); // MEM | BME | INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Program the MSI capability, but leave the message address unprogrammed/invalid.
    let cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI);
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, cap + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "AHCI MSI capability should support per-vector masking"
    );
    let pending_off = if is_64bit { cap + 0x14 } else { cap + 0x10 };

    let vector: u8 = 0x47;
    // Address low dword left as 0: invalid xAPIC MSI address.
    pci_cfg_write_u32(&mut pc, bdf, cap + 0x04, 0);
    if is_64bit {
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x08, 0);
        pci_cfg_write_u16(&mut pc, bdf, cap + 0x0C, u16::from(vector));
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x10, 0); // unmask
    } else {
        pci_cfg_write_u16(&mut pc, bdf, cap + 0x08, u16::from(vector));
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x0C, 0); // unmask
    }
    pci_cfg_write_u16(&mut pc, bdf, cap + 0x02, ctrl | 0x0001); // enable

    let abar = pci_read_bar(&mut pc, bdf, AHCI_ABAR_BAR_INDEX);
    assert_eq!(abar.kind, BarKind::Mem32);
    assert_ne!(abar.base, 0);

    // Guest memory layout for command list + table + DMA buffer.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let identify_buf = alloc.alloc_bytes(512, 512);

    // Program AHCI registers (port 0).
    pc.memory.write_u32(abar.base + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        abar.base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Clear any prior port interrupt state and build an IDENTIFY command.
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut pc, ctba, 0, identify_buf, 512);

    pc.memory.write_u32(abar.base + PORT_BASE + PORT_REG_CI, 1);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // First process: MSI is enabled but unprogrammed, so delivery is blocked and a pending bit is
    // latched instead.
    pc.process_ahci();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_ne!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to latch while MSI address is invalid"
    );

    // Clear the AHCI interrupt condition before completing MSI programming so delivery relies
    // solely on the pending bit.
    pc.memory
        .write_u32(abar.base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    pc.process_ahci();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_ne!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to remain set after clearing the interrupt condition"
    );

    // Now program a valid MSI address; the next processing step should observe the pending bit and
    // deliver without requiring a new rising edge.
    pci_cfg_write_u32(&mut pc, bdf, cap + 0x04, 0xFEE0_0000);
    if is_64bit {
        pci_cfg_write_u32(&mut pc, bdf, cap + 0x08, 0);
    }

    pc.process_ahci();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector));
    pc.interrupts.borrow_mut().acknowledge(vector);
    pc.interrupts.borrow_mut().eoi(vector);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
    assert_eq!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}
