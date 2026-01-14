#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::{profile, PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
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

#[test]
fn nvme_msix_delivers_to_lapic_in_apic_mode() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on PCI + NVMe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts.borrow_mut().set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        0x04,
        2,
        u32::from(cmd | (1 << 1) | (1 << 2)),
    );

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected NVMe BAR0 to be assigned during BIOS POST");

    // Enable MSI-X (capability control bit 15).
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl | (1 << 15)));

    // Program MSI-X table entry 0 via guest physical MMIO.
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);

    let vector: u8 = 0x67;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0 + 0x0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
}

