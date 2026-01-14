use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::pci::{profile, PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController, InterruptInput, PlatformInterruptMode, IMCR_INDEX, IMCR_SELECT_PORT,
};
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
use firmware::bios::PCIE_ECAM_BASE;
use pretty_assertions::assert_eq;

fn mmio_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn disable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 controls A20, bit0 is a reset pulse.
    m.io_write(A20_GATE_PORT, 1, 0x00);
}

fn program_ioapic_entry(m: &mut Machine, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    m.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_low);
    m.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, low);
    m.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_high);
    m.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn build_real_mode_imcr_interrupt_wait_boot_sector(
    vector: u8,
    flag_addr: u16,
    flag_value: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov ss, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
    i += 2;
    // mov sp, 0x7c00
    sector[i..i + 3].copy_from_slice(&[0xBC, 0x00, 0x7C]);
    i += 3;

    let ivt_off = (vector as u16) * 4;

    // mov word ptr [ivt_off], handler_offset (patched later)
    // C7 06 <addr16> <imm16>
    let patch_off = i + 4;
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&ivt_off.to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]); // placeholder
    i += 6;

    // mov word ptr [ivt_off+2], 0x0000 (segment)
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&(ivt_off + 2).to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // Program IMCR to route interrupts through the APIC/IOAPIC path:
    //   out 0x22, 0x70; out 0x23, 0x01
    // mov dx, 0x22
    let imcr_select_port = IMCR_SELECT_PORT.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBA, imcr_select_port[0], imcr_select_port[1]]);
    i += 3;
    // mov al, 0x70
    sector[i..i + 2].copy_from_slice(&[0xB0, IMCR_INDEX]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // inc dx
    sector[i] = 0x42;
    i += 1;
    // mov al, 0x01
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x01]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // sti
    sector[i] = 0xFB;
    i += 1;

    // hlt; jmp short $-3 (busy wait at HLT)
    sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);
    i += 3;

    // Handler lives directly after the loop, still within the boot sector (loaded at 0x7C00).
    let handler_addr = 0x7C00u16 + (i as u16);
    sector[patch_off..patch_off + 2].copy_from_slice(&handler_addr.to_le_bytes());

    // mov byte ptr [flag_addr], flag_value
    sector[i..i + 2].copy_from_slice(&[0xC6, 0x06]);
    i += 2;
    sector[i..i + 2].copy_from_slice(&flag_addr.to_le_bytes());
    i += 2;
    sector[i] = flag_value;
    i += 1;
    // iret
    sector[i] = 0xCF;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn pci_cfg_ports_and_ecam_match_for_host_bridge() {
    let mut m = Machine::new(mmio_machine_config()).unwrap();
    enable_a20(&mut m);

    // PCI host bridge lives at 00:00.0. Read vendor/device ID via cfg ports.
    m.io_write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000);
    let id_ports = m.io_read(PCI_CFG_DATA_PORT, 4);

    // Read the same register via ECAM MMIO.
    let id_ecam = m.read_physical_u32(PCIE_ECAM_BASE);

    assert_eq!(id_ecam, id_ports);
}

#[test]
fn imcr_port_switch_to_apic_mode_delivers_ioapic_interrupt_programmed_via_mmio() {
    let vector = 0x60u8;
    let flag_addr = 0x0504u16;
    let flag_value = 0xA5u8;
    let gsi = 10u32;

    let boot = build_real_mode_imcr_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(mmio_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    enable_a20(&mut m);

    // Route GSI10 -> vector 0x60, edge-triggered, active-low (typical PCI INTx wiring).
    let low = u32::from(vector) | (1 << 13);
    program_ioapic_entry(&mut m, gsi, low, 0);

    // Assert the line while still in legacy PIC mode; the guest switches to APIC mode via IMCR.
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .raise_irq(InterruptInput::Gsi(gsi));

    for _ in 0..50 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IOAPIC interrupt was not delivered after IMCR switch (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn imcr_port_switch_to_apic_mode_delivers_ide_irq14_programmed_via_mmio() {
    let vector = 0x60u8;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA6u8;

    let boot = build_real_mode_imcr_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = mmio_machine_config();
    cfg.enable_ide = true;
    cfg.enable_vga = false;

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    enable_a20(&mut m);

    // Attach a disk to IDE primary master so ATA IDENTIFY has a target.
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route ISA IRQ14 (GSI14) -> vector 0x60, edge-triggered, active-high (ISA wiring).
    program_ioapic_entry(&mut m, 14, u32::from(vector), 0);

    // Ensure PCI command enables I/O decode for the IDE function.
    cfg_write(&mut m, profile::IDE_PIIX3.bdf, 0x04, 2, 0x0001);

    // Issue ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Verify that IDENTIFY data is reachable via the data port (0x1F0).
    let word0 = m.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x0040);

    for _ in 0..50 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ14 was not delivered after IMCR switch to APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn imcr_port_switch_to_apic_mode_delivers_ide_irq15_programmed_via_mmio() {
    let vector = 0x61u8;
    let flag_addr = 0x0501u16;
    let flag_value = 0x5Au8;

    let boot = build_real_mode_imcr_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = mmio_machine_config();
    cfg.enable_ide = true;
    cfg.enable_vga = false;

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    enable_a20(&mut m);

    // Attach an ATAPI device to IDE secondary master so IDENTIFY PACKET can raise IRQ15.
    m.attach_ide_secondary_master_atapi(AtapiCdrom::new(None));

    // Route ISA IRQ15 (GSI15) -> vector 0x61, edge-triggered, active-high (ISA wiring).
    program_ioapic_entry(&mut m, 15, u32::from(vector), 0);

    // Ensure PCI command enables I/O decode for the IDE function.
    cfg_write(&mut m, profile::IDE_PIIX3.bdf, 0x04, 2, 0x0001);

    // ATAPI IDENTIFY PACKET DEVICE (0xA1) via secondary legacy ports.
    m.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0); // select secondary master
    m.io_write(SECONDARY_PORTS.cmd_base + 7, 1, 0xA1);

    // Verify that IDENTIFY data is reachable via the data port (0x170).
    let word0 = m.io_read(SECONDARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x8581);

    for _ in 0..50 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ15 was not delivered after IMCR switch to APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn ioapic_mmio_supports_partial_and_unaligned_accesses() {
    let vector = 0x63u8;
    let gsi = 10u32;

    let mut m = Machine::new(mmio_machine_config()).unwrap();
    enable_a20(&mut m);

    // Enable APIC mode delivery so IOAPIC routes into the LAPIC.
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Route GSI10 -> vector with typical active-low polarity. Keep it edge-triggered.
    let low = u32::from(vector) | (1 << 13);
    let high = 0u32;

    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;

    // Program IOREGSEL with a 1-byte write.
    m.write_physical_u8(IOAPIC_MMIO_BASE, redtbl_low as u8);
    // Program IOWIN using two 16-bit writes (partial word updates).
    m.write_physical_u16(IOAPIC_MMIO_BASE + 0x10, low as u16);
    m.write_physical_u16(IOAPIC_MMIO_BASE + 0x12, (low >> 16) as u16);

    // Read back the low dword via the aligned 32-bit register window.
    m.write_physical_u8(IOAPIC_MMIO_BASE, redtbl_low as u8);
    assert_eq!(m.read_physical_u32(IOAPIC_MMIO_BASE + 0x10), low);

    // Program the high dword similarly.
    m.write_physical_u8(IOAPIC_MMIO_BASE, redtbl_high as u8);
    m.write_physical_u16(IOAPIC_MMIO_BASE + 0x10, high as u16);
    m.write_physical_u16(IOAPIC_MMIO_BASE + 0x12, (high >> 16) as u16);
    m.write_physical_u8(IOAPIC_MMIO_BASE, redtbl_high as u8);
    assert_eq!(m.read_physical_u32(IOAPIC_MMIO_BASE + 0x10), high);

    // Now assert the input line and ensure the vector becomes pending.
    let interrupts = m.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);
    interrupts.borrow_mut().raise_irq(InterruptInput::Gsi(gsi));
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
}

#[test]
fn a20_gate_masks_bit20_for_physical_accesses() {
    // Sanity check: with A20 disabled, physical addresses are masked with `!(1<<20)` which
    // aliases addresses separated by 1MiB.
    //
    // Many PC MMIO base addresses rely on bit20, so tests that touch MMIO should explicitly
    // enable A20 first.
    let mut m = Machine::new(mmio_machine_config()).unwrap();
    disable_a20(&mut m);

    let lo = 0x0000_1234u64;
    let hi = lo | (1u64 << 20);

    // A20 disabled: hi aliases lo.
    m.write_physical_u8(lo, 0x11);
    m.write_physical_u8(hi, 0x22);
    assert_eq!(m.read_physical_u8(lo), 0x22);
    assert_eq!(m.read_physical_u8(hi), 0x22);

    enable_a20(&mut m);

    // A20 enabled: the same addresses become distinct.
    m.write_physical_u8(lo, 0x33);
    m.write_physical_u8(hi, 0x44);
    assert_eq!(m.read_physical_u8(lo), 0x33);
    assert_eq!(m.read_physical_u8(hi), 0x44);
}

#[test]
fn pci_bar_routing_respects_command_bits_for_e1000() {
    let mut cfg = mmio_machine_config();
    cfg.enable_e1000 = true;
    let mut m = Machine::new(cfg).unwrap();
    enable_a20(&mut m);

    let bdf = profile::NIC_E1000_82540EM.bdf;

    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    assert!(
        (command & 0x3) == 0x3,
        "bios_post should enable IO+MEM decoding (command=0x{command:04x})"
    );

    let bar0 = cfg_read(&mut m, bdf, 0x10, 4);
    let bar1 = cfg_read(&mut m, bdf, 0x14, 4);
    let mmio_base = u64::from(bar0 & !0xF);
    let io_base = u16::try_from(bar1 & !0x3).expect("IO BAR must fit in u16");
    assert_ne!(mmio_base, 0);
    assert_ne!(io_base, 0);

    // With decoding enabled, accesses should route to the E1000 model (not return all-ones).
    assert_ne!(m.io_read(io_base, 4), 0xFFFF_FFFF);
    assert_ne!(m.read_physical_u32(mmio_base), 0xFFFF_FFFF);

    // Disable I/O BAR decoding (COMMAND.IO = 0). Port reads should fall back to all-ones.
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command & !0x1));
    assert_eq!(m.io_read(io_base, 4), 0xFFFF_FFFF);
    // MMIO remains enabled.
    assert_ne!(m.read_physical_u32(mmio_base), 0xFFFF_FFFF);

    // Disable MMIO BAR decoding (COMMAND.MEM = 0) while re-enabling I/O.
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command & !0x2));
    assert_ne!(m.io_read(io_base, 4), 0xFFFF_FFFF);
    assert_eq!(m.read_physical_u32(mmio_base), 0xFFFF_FFFF);
}

#[test]
fn hpet_mmio_writes_can_raise_ioapic_interrupt_in_apic_mode() {
    let vector = 0x61u8;

    let mut m = Machine::new(mmio_machine_config()).unwrap();
    enable_a20(&mut m);

    // Enable APIC mode delivery so IOAPIC routes into the LAPIC.
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Route HPET timer0 default GSI2 -> vector 0x61, level-triggered, unmasked.
    let gsi = 2u32;
    let low = u32::from(vector) | (1 << 15); // level-triggered, active-high
    program_ioapic_entry(&mut m, gsi, low, 0);

    // Program HPET timer0 via guest-visible MMIO.
    //
    // Configure Timer0: route=2, level-triggered, interrupt enabled.
    let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
    m.write_physical_u64(HPET_MMIO_BASE + 0x100, timer0_cfg);
    // Arm comparator at 0 so it's immediately pending once HPET is enabled.
    m.write_physical_u64(HPET_MMIO_BASE + 0x108, 0);
    // Enable HPET (general config).
    m.write_physical_u64(HPET_MMIO_BASE + 0x010, 1);

    let interrupts = m.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
}

#[test]
fn pci_bar_reprogramming_relocates_io_and_mmio_routing_for_e1000() {
    let mut cfg = mmio_machine_config();
    cfg.enable_e1000 = true;
    let mut m = Machine::new(cfg).unwrap();
    enable_a20(&mut m);

    let bdf = profile::NIC_E1000_82540EM.bdf;

    let bar0 = cfg_read(&mut m, bdf, 0x10, 4);
    let bar1 = cfg_read(&mut m, bdf, 0x14, 4);
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    assert!(
        (command & 0x3) == 0x3,
        "expected IO+MEM decoding enabled before BAR relocation"
    );

    let mmio_base = u64::from(bar0 & !0xF);
    let io_base = u16::try_from(bar1 & !0x3).expect("IO BAR must fit in u16");
    assert_ne!(mmio_base, 0);
    assert_ne!(io_base, 0);

    // Sanity: the original BAR bases should route to the device.
    assert_ne!(m.read_physical_u32(mmio_base), 0xFFFF_FFFF);
    assert_ne!(m.io_read(io_base, 4), 0xFFFF_FFFF);

    // Compute new bases by shifting each BAR by its own size (preserves alignment).
    let bar0_size = profile::NIC_E1000_82540EM
        .build_config_space()
        .bar_definition(0)
        .expect("E1000 BAR0 definition")
        .size();
    let bar1_size = profile::NIC_E1000_82540EM
        .build_config_space()
        .bar_definition(1)
        .expect("E1000 BAR1 definition")
        .size();

    let new_mmio_base = mmio_base + bar0_size;
    let new_io_base_u64 = u64::from(io_base) + bar1_size;
    let new_io_base = u16::try_from(new_io_base_u64).expect("relocated IO BAR must fit in u16");

    // Preserve BAR flag bits.
    let bar0_flags = bar0 & 0xF;
    let bar1_flags = bar1 & 0x3;

    cfg_write(&mut m, bdf, 0x10, 4, (new_mmio_base as u32) | bar0_flags);
    cfg_write(
        &mut m,
        bdf,
        0x14,
        4,
        (u32::from(new_io_base) & !0x3) | bar1_flags,
    );

    // Old addresses should now miss the BAR decoders (all-ones reads).
    assert_eq!(m.read_physical_u32(mmio_base), 0xFFFF_FFFF);
    assert_eq!(m.io_read(io_base, 4), 0xFFFF_FFFF);

    // New addresses should route again.
    assert_ne!(m.read_physical_u32(new_mmio_base), 0xFFFF_FFFF);
    assert_ne!(m.io_read(new_io_base, 4), 0xFFFF_FFFF);
}

#[test]
fn lapic_mmio_timer_can_fire_in_apic_mode() {
    let vector = 0x40u8;

    let mut m = Machine::new(mmio_machine_config()).unwrap();
    enable_a20(&mut m);

    // Ensure LAPIC delivery is active for get_pending().
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Program LAPIC timer via the MMIO window:
    // - divide config: 0xB => divisor 1 (fast ticks)
    // - LVT timer: vector 0x40 (one-shot, unmasked)
    // - initial count: 10
    m.write_lapic_u32(0, 0x3E0, 0xBu32);
    m.write_lapic_u32(0, 0x320, u32::from(vector));
    m.write_lapic_u32(0, 0x380, 10u32);

    let interrupts = m.platform_interrupts().unwrap();
    interrupts.borrow().tick(9);
    assert_eq!(interrupts.borrow().get_pending(), None);
    interrupts.borrow().tick(1);
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
}
