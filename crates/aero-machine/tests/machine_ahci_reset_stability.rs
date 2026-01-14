#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};

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

const HBA_GHC: u64 = 0x04;
const HBA_VS: u64 = 0x10;

const GHC_IE: u32 = 1 << 1;

#[test]
fn machine_ahci_mmio_and_device_rc_identity_remain_stable_across_reset() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for a deterministic reset sequence.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ahci_before = m.ahci().expect("AHCI controller should be enabled");
    let ptr_before = Rc::as_ptr(&ahci_before);

    // Enable A20 before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Program BAR5 and enable memory decoding + bus mastering.
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Sanity-check that MMIO is live (AHCI VS register).
    assert_eq!(m.read_physical_u32(bar5_base + HBA_VS), 0x0001_0300);

    // Mutate a register bit that should be cleared by device reset. This is used to detect stale
    // MMIO handler wiring: if `Machine::reset()` were to swap out the `Rc<AhciPciDevice>` without
    // rebuilding the MMIO router, MMIO would keep targeting the *old* device instance and this
    // bit would remain set after reset.
    //
    // Note: the AHCI Enable bit (AE, bit 31) may remain set across reset depending on BIOS/device
    // policy, so we focus on the Interrupt Enable bit (IE, bit 1).
    let ghc0 = m.read_physical_u32(bar5_base + HBA_GHC);
    m.write_physical_u32(bar5_base + HBA_GHC, ghc0 | GHC_IE);
    let ghc_before = m.read_physical_u32(bar5_base + HBA_GHC);
    assert_eq!(ghc_before & GHC_IE, GHC_IE);

    // Reset the machine and ensure the AHCI `Rc` identity is stable (important because the PCI
    // MMIO router is mapped via `map_mmio_once` and retains captured handler instances).
    m.reset();

    let ahci_after = m.ahci().expect("AHCI controller should still be enabled");
    let ptr_after = Rc::as_ptr(&ahci_after);
    assert_eq!(
        ptr_before, ptr_after,
        "AHCI Rc identity changed across reset; persistent MMIO handlers may point at stale devices"
    );

    // Re-enable A20 and re-program PCI config state (reset clears guest-visible config space).
    m.io_write(A20_GATE_PORT, 1, 0x02);
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // The GHC IE bit should be cleared after reset.
    let ghc_after = m.read_physical_u32(bar5_base + HBA_GHC);
    assert_eq!(ghc_after & GHC_IE, 0);

    // MMIO should still be live and routed to the post-reset device state.
    assert_eq!(m.read_physical_u32(bar5_base + HBA_VS), 0x0001_0300);
}
