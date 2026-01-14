#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{profile, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use pretty_assertions::{assert_eq, assert_ne};

// AHCI ABAR register offsets (HBA + port 0).
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

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 4, value);
}

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
fn snapshot_restore_roundtrips_ahci_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on AHCI + PCI INTx snapshot restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ahci = vm.ahci().expect("ahci enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = profile::SATA_AHCI_ICH9.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let gsi_u8 = u8::try_from(gsi).expect("gsi must fit in ISA IRQ range for legacy PIC");
        assert!(
            gsi_u8 < 16,
            "test assumes AHCI routes to a legacy PIC IRQ (0-15); got GSI {gsi}"
        );
        let vector = if gsi_u8 < 8 {
            0x20u8.wrapping_add(gsi_u8)
        } else {
            0x28u8.wrapping_add(gsi_u8.wrapping_sub(8))
        };

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false); // unmask cascade
        ints.pic_mut().set_masked(gsi_u8, false); // unmask routed IRQ (GSI 10-13)

        (gsi, vector)
    };

    // Program PCI config state for the AHCI controller (BAR5 + command).
    let bdf = profile::SATA_AHCI_ICH9.bdf;
    let abar: u64 = 0xE200_0000;
    // BAR5: ABAR.
    write_cfg_u32(
        &mut vm,
        bdf.bus,
        bdf.device,
        bdf.function,
        profile::AHCI_ABAR_CFG_OFFSET,
        abar as u32,
    );
    // COMMAND: memory decoding + bus mastering.
    write_cfg_u16(&mut vm, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Program the AHCI controller and issue an IDENTIFY DMA command so that:
    // - the controller has guest-visible state changes,
    // - PxIS is pending + enabled, and
    // - the AHCI device model asserts its legacy INTx line.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_CLB, clb as u32);
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_FB, fb as u32);
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    vm.write_physical_u32(abar + HBA_GHC, GHC_AE | GHC_IE);
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    write_cmd_header(&mut vm, clb, 0, ctba, 1, false);
    write_cfis(&mut vm, ctba, 0xEC, 0, 0); // ATA IDENTIFY
    write_prdt(
        &mut vm,
        ctba,
        0,
        identify_buf,
        aero_storage::SECTOR_SIZE as u32,
    );
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_CI, 1);

    for _ in 0..16 {
        vm.process_ahci();
        if vm.read_physical_u32(abar + PORT_BASE + PORT_REG_CI) == 0 {
            break;
        }
    }

    assert_eq!(
        vm.read_physical_u32(abar + PORT_BASE + PORT_REG_CI),
        0,
        "IDENTIFY DMA did not complete"
    );
    assert!(
        ahci.borrow().intx_level(),
        "AHCI should assert INTx after DMA completion"
    );

    // Ensure we have *not* synchronized PCI INTx levels into the platform interrupts yet. This is
    // the behavior we care about: a machine snapshot can capture the device state while the
    // interrupt sink is still desynchronized.
    assert!(!interrupts.borrow().gsi_level(gsi));
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_ahci_state = { ahci.borrow().save_state() };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    vm.write_physical_u32(abar + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    assert!(
        !ahci.borrow().intx_level(),
        "clearing PxIS should deassert AHCI INTx"
    );

    let mutated_ahci_state = { ahci.borrow().save_state() };
    assert_ne!(
        mutated_ahci_state, expected_ahci_state,
        "AHCI state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the AHCI instance (host wiring/backends live outside snapshots).
    let ahci_after = vm.ahci().expect("ahci still enabled");
    assert!(
        Rc::ptr_eq(&ahci, &ahci_after),
        "restore must not replace the AHCI instance"
    );

    let restored_ahci_state = { ahci_after.borrow().save_state() };
    assert_eq!(restored_ahci_state, expected_ahci_state);
    assert!(ahci_after.borrow().intx_level());

    // After restore, the AHCI's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}
