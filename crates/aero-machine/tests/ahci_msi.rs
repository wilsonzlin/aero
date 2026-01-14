#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::profile::{AHCI_ABAR_BAR_INDEX, SATA_AHCI_ICH9};
use aero_devices::pci::{MsiCapability, PciBdf, PciDevice, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::ATA_CMD_IDENTIFY;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterruptMode,
};
use pretty_assertions::{assert_eq, assert_ne};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn find_capability(m: &mut Machine, bdf: PciBdf, cap_id: u8) -> Option<u16> {
    let mut ptr = cfg_read(m, bdf, 0x34, 1) as u8;
    for _ in 0..64 {
        if ptr == 0 {
            return None;
        }
        let id = cfg_read(m, bdf, u16::from(ptr), 1) as u8;
        if id == cap_id {
            return Some(u16::from(ptr));
        }
        ptr = cfg_read(m, bdf, u16::from(ptr) + 1, 1) as u8;
    }
    None
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

fn write_cmd_header(m: &mut Machine, clb: u64, slot: usize, ctba: u64, prdtl: u16, write: bool) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    m.write_physical_u32(addr, flags);
    m.write_physical_u32(addr + 4, 0); // PRDBC
    m.write_physical_u32(addr + 8, ctba as u32);
    m.write_physical_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(m: &mut Machine, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    m.write_physical_u32(addr, dba as u32);
    m.write_physical_u32(addr + 4, (dba >> 32) as u32);
    m.write_physical_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    m.write_physical_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(m: &mut Machine, ctba: u64, command: u8, lba: u64, count: u16) {
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

    m.write_physical(ctba, &cfis);
}

#[test]
fn ahci_msi_masked_interrupt_sets_pending_and_redelivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the test focused on PCI + AHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = SATA_AHCI_ICH9.bdf;

    // Enable PCI memory decoding + bus mastering + INTx disable.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2) | (1 << 10)),
    );

    // Program MSI, starting with the vector masked.
    let base = find_capability(&mut m, bdf, PCI_CAP_ID_MSI).expect("AHCI should expose MSI");
    let ctrl = cfg_read(&mut m, bdf, base + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(per_vector_masking, "expected per-vector masking support");
    let pending_off = if is_64bit { base + 0x14 } else { base + 0x10 };

    let vector: u8 = 0x46;
    cfg_write(&mut m, bdf, base + 0x04, 4, 0xfee0_0000);
    if is_64bit {
        cfg_write(&mut m, bdf, base + 0x08, 4, 0);
        cfg_write(&mut m, bdf, base + 0x0c, 2, u32::from(vector));
        cfg_write(&mut m, bdf, base + 0x10, 4, 1); // mask
    } else {
        cfg_write(&mut m, bdf, base + 0x08, 2, u32::from(vector));
        cfg_write(&mut m, bdf, base + 0x0c, 4, 1); // mask
    }
    cfg_write(&mut m, bdf, base + 0x02, 2, u32::from(ctrl | 1)); // MSI enable

    let abar = m
        .pci_bar_base(bdf, AHCI_ABAR_BAR_INDEX)
        .expect("AHCI BAR5 should exist");
    assert_ne!(abar, 0);

    // Guest memory layout for command list + table + DMA buffer.
    // Use small fixed addresses inside the 2MiB RAM window.
    let clb = 0x10000u64;
    let fb = 0x11000u64;
    let ctba = 0x12000u64;
    let identify_buf = 0x13000u64;

    // Program AHCI registers (port 0).
    m.write_physical_u32(abar + HBA_GHC, GHC_IE | GHC_AE);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Clear any prior port interrupt state and build an IDENTIFY command.
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    write_cmd_header(&mut m, clb, 0, ctba, 1, false);
    write_cfis(&mut m, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut m, ctba, 0, identify_buf, aero_storage::SECTOR_SIZE as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CI, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // First process: MSI is masked, so delivery is suppressed but a pending bit is latched.
    m.process_ahci();
    m.poll_pci_intx_lines();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_ne!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to be guest-visible via canonical PCI config space reads"
    );

    // Now unmask MSI in canonical PCI config space.
    if is_64bit {
        cfg_write(&mut m, bdf, base + 0x10, 4, 0);
    } else {
        cfg_write(&mut m, bdf, base + 0x0c, 4, 0);
    }

    // The machine mirrors PCI config state into the device model while polling INTx sources. This
    // must not clobber the device-managed MSI pending bit.
    m.poll_pci_intx_lines();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Second process: interrupt condition is still asserted, so delivery should occur due to the
    // pending bit even without a new rising edge.
    m.process_ahci();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}

#[test]
fn ahci_msi_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the test focused on PCI + AHCI.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = SATA_AHCI_ICH9.bdf;
    let ahci = m.ahci().expect("AHCI should be enabled");

    // Enable PCI memory decoding + bus mastering + INTx disable.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2) | (1 << 10)),
    );

    // Enable MSI and program the vector, but leave the message address unprogrammed/invalid.
    let base = find_capability(&mut m, bdf, PCI_CAP_ID_MSI).expect("AHCI should expose MSI");
    let ctrl = cfg_read(&mut m, bdf, base + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(per_vector_masking, "expected per-vector masking support");

    let vector: u8 = 0x47;
    // Address low dword left as 0: invalid LAPIC MSI address.
    cfg_write(&mut m, bdf, base + 0x04, 4, 0);
    if is_64bit {
        cfg_write(&mut m, bdf, base + 0x08, 4, 0);
        cfg_write(&mut m, bdf, base + 0x0c, 2, u32::from(vector));
        cfg_write(&mut m, bdf, base + 0x10, 4, 0); // unmasked
    } else {
        cfg_write(&mut m, bdf, base + 0x08, 2, u32::from(vector));
        cfg_write(&mut m, bdf, base + 0x0c, 4, 0); // unmasked
    }
    cfg_write(&mut m, bdf, base + 0x02, 2, u32::from(ctrl | 1)); // MSI enable

    let abar = m
        .pci_bar_base(bdf, AHCI_ABAR_BAR_INDEX)
        .expect("AHCI BAR5 should exist");
    assert_ne!(abar, 0);

    // Guest memory layout for command list + table + DMA buffer.
    let clb = 0x10000u64;
    let fb = 0x11000u64;
    let ctba = 0x12000u64;
    let identify_buf = 0x13000u64;

    // Program AHCI registers (port 0).
    m.write_physical_u32(abar + HBA_GHC, GHC_IE | GHC_AE);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Clear any prior port interrupt state and build an IDENTIFY command.
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    write_cmd_header(&mut m, clb, 0, ctba, 1, false);
    write_cfis(&mut m, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut m, ctba, 0, identify_buf, aero_storage::SECTOR_SIZE as u32);
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_CI, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // First process: MSI is enabled but unprogrammed, so delivery is blocked and a pending bit is
    // latched instead.
    m.process_ahci();
    m.poll_pci_intx_lines();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    assert!(
        ahci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "MSI pending bit should latch in the device model when message address is invalid"
    );
    let pending_off = if is_64bit { base + 0x14 } else { base + 0x10 };
    assert_ne!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to be guest-visible via canonical PCI config space reads"
    );

    // Clear the interrupt condition before completing MSI programming so delivery relies solely on
    // the pending bit.
    m.write_physical_u32(abar + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    m.process_ahci();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Now program a valid MSI address; the next AHCI processing step should observe the pending
    // bit and deliver without requiring a new rising edge.
    cfg_write(&mut m, bdf, base + 0x04, 4, 0xfee0_0000);
    if is_64bit {
        cfg_write(&mut m, bdf, base + 0x08, 4, 0);
    }
    m.poll_pci_intx_lines();
    m.process_ahci();

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    assert_eq!(
        cfg_read(&mut m, bdf, pending_off, 4) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}
