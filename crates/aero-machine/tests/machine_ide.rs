#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::state::RFLAGS_IF;
use aero_devices::pci::{profile, PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::InterruptController;
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use pretty_assertions::{assert_eq, assert_ne};

fn ide_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep the machine minimal and deterministic for this port-level IDE test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    }
}

fn pci_cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000u32
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

#[test]
fn machine_piix3_ide_pio_read_raises_irq14() {
    // Use a deterministic HLT boot sector so `run_slice` is safe to call for device polling.
    let mut boot = [0u8; aero_storage::SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xF4, 0xEB, 0xFD]); // hlt; jmp $-3
    boot[510] = 0x55;
    boot[511] = 0xAA;

    let mut m = Machine::new(ide_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Attach a tiny in-memory ATA disk with "BOOT" at the start of sector 0.
    let mut disk = RawDisk::create(MemBackend::new(), SECTOR_SIZE as u64).unwrap();
    disk.write_at(0, b"BOOT").unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Unmask IRQ14 (and cascade IRQ2) so PIC pending vectors are observable.
    {
        let ints = m.platform_interrupts().unwrap();
        let mut ints = ints.borrow_mut();
        ints.pic_mut().set_masked(2, false); // cascade
        ints.pic_mut().set_masked(14, false);
    }

    // Keep IF=0 so `Machine::run_slice` does not acknowledge/present the interrupt to the CPU
    // (which would clear the PIC pending vector and make it harder to assert on).
    let rflags = m.cpu().rflags();
    m.cpu_mut().set_rflags(rflags & !RFLAGS_IF);

    // Disable PCI I/O decode: IDE legacy ports should read as open bus (all ones).
    let bdf = profile::IDE_PIIX3.bdf;
    m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bdf, 0x04));
    m.io_write(PCI_CFG_DATA_PORT, 2, 0x0000);
    assert_eq!(m.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8, 0xFF);

    // Enable IDE COMMAND.IO | COMMAND.BME.
    m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bdf, 0x04));
    m.io_write(PCI_CFG_DATA_PORT, 2, 0x0005);
    assert_ne!(m.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8, 0xFF);

    // Issue a PIO READ SECTORS (0x20) for LBA 0, count 1, primary master.
    m.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // sector count
    m.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // LBA0
    m.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // LBA1
    m.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // LBA2
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA mode
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    // Poll the machine once so IDE IRQ levels are synchronized into the platform controller.
    // With IF=0, the interrupt should remain pending in the PIC.
    let _ = m.run_slice(1);

    let pending = m.platform_interrupts().unwrap().borrow().get_pending();
    assert_eq!(pending, Some(0x76), "IDE primary should assert ISA IRQ14");

    // Consume the first 4 bytes from the data port and validate content.
    let w0 = m.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = m.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut out = [0u8; 4];
    out[0..2].copy_from_slice(&w0.to_le_bytes());
    out[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(&out, b"BOOT");

    // Sanity: running again should still observe a Halted state (boot sector is `hlt; jmp`).
    if let RunExit::Halted { .. } = m.run_slice(1) {
    } else {
        panic!("expected CPU to remain halted in the boot sector loop");
    }
}

#[test]
fn machine_piix3_ide_secondary_identify_aborts_and_raises_irq15() {
    // Deterministic `hlt; jmp` boot sector so `run_slice` is safe to use as a polling mechanism.
    let mut boot = [0u8; aero_storage::SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xF4, 0xEB, 0xFD]); // hlt; jmp $-3
    boot[510] = 0x55;
    boot[511] = 0xAA;

    let mut m = Machine::new(ide_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // PCI topology: IDE is a PIIX3 multi-function device; function 0 must exist and be marked
    // multi-function so OSes enumerate the IDE function at 00:01.1.
    {
        let bdf = profile::ISA_PIIX3.bdf;
        m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bdf, 0x0C));
        let header = m.io_read(PCI_CFG_DATA_PORT, 4);
        let header_type = ((header >> 16) & 0xFF) as u8;
        assert_ne!(header_type & 0x80, 0, "ISA_PIIX3 must be multi-function");
    }

    // Attach an empty ATAPI CD-ROM device on the secondary master so IDENTIFY DEVICE aborts and
    // generates an interrupt on the secondary channel.
    m.attach_ide_secondary_master_atapi(AtapiCdrom::new(None));

    // Unmask IRQ15 (and cascade IRQ2) so PIC pending vectors are observable.
    {
        let ints = m.platform_interrupts().unwrap();
        let mut ints = ints.borrow_mut();
        ints.pic_mut().set_masked(2, false); // cascade
        ints.pic_mut().set_masked(15, false);
    }

    // Keep IF=0 so `Machine::run_slice` does not acknowledge/present the interrupt to the CPU.
    let rflags = m.cpu().rflags();
    m.cpu_mut().set_rflags(rflags & !RFLAGS_IF);

    // Enable IDE I/O decoding.
    let bdf = profile::IDE_PIIX3.bdf;
    m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bdf, 0x04));
    m.io_write(PCI_CFG_DATA_PORT, 2, 0x0001);

    // IDENTIFY DEVICE (0xEC) on an ATAPI device aborts and raises an interrupt.
    m.io_write(SECONDARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Poll once so the IDE IRQ pending state is synchronized into the platform controller. With
    // IF=0, the PIC should retain the interrupt as pending.
    let _ = m.run_slice(1);

    let pending = m.platform_interrupts().unwrap().borrow().get_pending();
    assert_eq!(pending, Some(0x77), "IDE secondary should assert ISA IRQ15");
}
