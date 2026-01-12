#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{PlatformInterruptMode, PlatformInterrupts};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};

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

fn read_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_read(PCI_CFG_DATA_PORT, 4)
}

fn write_ivt_entry(m: &mut Machine, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    m.write_physical_u16(base, offset);
    m.write_physical_u16(base + 2, segment);
}

fn install_real_mode_handler(m: &mut Machine, handler_addr: u64, flag_addr: u16, value: u8) {
    let flag_addr_bytes = flag_addr.to_le_bytes();

    // mov byte ptr [imm16], imm8
    // iret
    let code = [
        0xC6,
        0x06,
        flag_addr_bytes[0],
        flag_addr_bytes[1],
        value,
        0xCF,
    ];
    m.write_physical(handler_addr, &code);
}

fn install_hlt_loop(m: &mut Machine, code_base: u64) {
    // hlt; jmp short $-3 (back to hlt)
    let code = [0xF4u8, 0xEB, 0xFD];
    m.write_physical(code_base, &code);
}

fn setup_real_mode_cpu(m: &mut Machine, entry_ip: u64) {
    let cpu = m.cpu_mut();

    // Real-mode segments: base = selector<<4, limit = 0xFFFF.
    for seg in [
        &mut cpu.segments.cs,
        &mut cpu.segments.ds,
        &mut cpu.segments.es,
        &mut cpu.segments.ss,
        &mut cpu.segments.fs,
        &mut cpu.segments.gs,
    ] {
        seg.selector = 0;
        seg.base = 0;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    cpu.set_stack_ptr(0x7000);
    cpu.set_rip(entry_ip);
    cpu.set_rflags(0x202); // IF=1
    cpu.halted = false;
}

fn program_ioapic_redirection_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn machine_ide_identify_pio_raises_irq14_and_wakes_halted_cpu() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE + ISA IRQ wiring.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IRQ14 into a real-mode handler that writes a flag byte.
    let vector = 0x2E_u8; // PIC slave base 0x28 + (IRQ14-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA5_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ14 delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ14.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    // Ensure PCI command enables I/O space decode for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0); // select primary master
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Verify that IDENTIFY data is reachable via the data port (0x1F0).
    let word0 = m.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x0040);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ14 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_pio_is_gated_by_pci_command_io_enable() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on PCI COMMAND.IO gating for legacy IDE ports.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IRQ14 into a real-mode handler that writes a flag byte.
    let vector = 0x2E_u8; // PIC slave base 0x28 + (IRQ14-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xB6_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so any interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ14 delivery through the legacy PIC (but keep the IDE PCI function I/O disabled).
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    let bdf = IDE_PIIX3.bdf;

    // Explicitly disable PCI I/O decode for the IDE function.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);

    // Attempt ATA IDENTIFY DEVICE with I/O decode disabled: writes should be ignored and reads
    // should return all-ones (open bus).
    m.io_write(0x1F6, 1, 0xA0);
    m.io_write(0x1F7, 1, 0xEC);
    let word0 = m.io_read(0x1F0, 2) as u16;
    assert_eq!(word0, 0xFFFF);

    for _ in 0..5 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "IRQ14 should not be delivered while PCI COMMAND.IO=0"
        );
    }

    // Re-enable PCI I/O decode and re-issue IDENTIFY: it should now complete and deliver IRQ14.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    m.io_write(0x1F6, 1, 0xA0);
    m.io_write(0x1F7, 1, 0xEC);
    let word0 = m.io_read(0x1F0, 2) as u16;
    assert_eq!(word0, 0x0040);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ14 interrupt handler did not run after re-enabling PCI I/O decode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_irq14_is_delivered_via_ioapic_in_apic_mode() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const VECTOR: u8 = 0x60;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE + IOAPIC/LAPIC routing.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IOAPIC vector into a real-mode handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA6_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and program the IOAPIC redirection entry for GSI14 -> VECTOR.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // ISA IRQ14 is wired to GSI14 in our platform model (no ACPI override).
        //
        // Configure as edge-triggered, active-high, unmasked, fixed delivery, physical dest=0.
        let low = u32::from(VECTOR);
        program_ioapic_redirection_entry(&mut ints, 14, low, 0);
    }

    // Ensure PCI command enables I/O space decode for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0); // select primary master
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Verify that IDENTIFY data is reachable via the data port (0x1F0).
    let word0 = m.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x0040);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ14 interrupt handler did not run in APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_irq15_is_delivered_via_ioapic_in_apic_mode() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const VECTOR: u8 = 0x61;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE secondary channel + IOAPIC/LAPIC routing.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach an ATAPI device to IDE secondary master so IDENTIFY PACKET can raise IRQ15.
    m.attach_ide_secondary_master_atapi(AtapiCdrom::new(None));

    // Route IOAPIC vector into a real-mode handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5B_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and program the IOAPIC redirection entry for GSI15 -> VECTOR.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // ISA IRQ15 is wired to GSI15 in our platform model (no ACPI override).
        let low = u32::from(VECTOR);
        program_ioapic_redirection_entry(&mut ints, 15, low, 0);
    }

    // Enable PCI command I/O space decode for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // ATAPI IDENTIFY PACKET DEVICE (0xA1) via secondary legacy ports.
    m.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0); // select secondary master
    m.io_write(SECONDARY_PORTS.cmd_base + 7, 1, 0xA1);

    // Verify that IDENTIFY data is reachable via the data port (0x170).
    let word0 = m.io_read(SECONDARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x8581);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ15 interrupt handler did not run in APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_primary_dma_read_wakes_halted_cpu_via_ioapic_in_apic_mode() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const VECTOR: u8 = 0x62;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE Bus Master DMA + IOAPIC/LAPIC delivery.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master and seed it with a known prefix.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..8].copy_from_slice(b"APICDMA!");
    disk.write_sectors(0, &sector0).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IOAPIC vector into a real-mode handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xD1_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and program IOAPIC redirection entry for GSI14 -> VECTOR.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_redirection_entry(&mut ints, 14, u32::from(VECTOR), 0);
    }

    let bdf = IDE_PIIX3.bdf;

    // Read BAR4 so the test is resilient to future default base changes.
    let bar4_raw = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected IDE BMIDE BAR4 to be programmed");

    // Prepare a single-entry PRD table (512 bytes, end-of-table).
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;
    m.write_physical_u32(prd_addr, data_buf as u32);
    m.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    m.write_physical_u16(prd_addr + 6, 0x8000);

    // Clear the destination buffer to a sentinel value first.
    m.write_physical(data_buf, &[0u8; 8]);

    // Enable PCI I/O decode + bus mastering for IDE.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    // Program BMIDE: PRDT base + start DMA in the "device -> memory" direction (bit3=1).
    m.io_write(bm_base + 4, 4, prd_addr as u32);
    m.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    m.io_write(bm_base, 1, 0x09);

    // Issue ATA READ DMA (0xC8) for LBA 0, count 1, primary master.
    m.io_write(0x1F2, 1, 1);
    m.io_write(0x1F3, 1, 0);
    m.io_write(0x1F4, 1, 0);
    m.io_write(0x1F5, 1, 0);
    m.io_write(0x1F6, 1, 0xE0);
    m.io_write(0x1F7, 1, 0xC8);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            let prefix = m.read_physical_bytes(data_buf, 8);
            assert_eq!(prefix.as_slice(), b"APICDMA!");

            let bm_status = m.io_read(bm_base + 2, 1) as u8;
            assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
            return;
        }
    }

    panic!(
        "IDE primary DMA READ did not deliver IOAPIC interrupt (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_bmide_bar4_routing_tracks_pci_bar_reprogramming() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on PCI I/O BAR routing.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a disk so the Bus Master status register exposes DMA capability bits.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Enable I/O decoding.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Reprogram BAR4 to a new base within the machine's PCI I/O window.
    let new_base: u16 = 0x1800;
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x20, // BAR4
        u32::from(new_base),
    );

    // Primary channel Bus Master status register is at BAR4+2.
    let status = m.io_read(new_base + 2, 1) as u8;
    assert_eq!(status, 0x20);
}

#[test]
fn machine_ide_secondary_identify_packet_raises_irq15_and_wakes_halted_cpu() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE secondary channel + ISA IRQ15 wiring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Attach an ATAPI device to the canonical Win7 location: IDE secondary master.
    m.attach_ide_secondary_master_atapi(AtapiCdrom::new(None));

    // Route IRQ15 into a real-mode handler that writes a flag byte.
    let vector = 0x2F_u8; // PIC slave base 0x28 + (IRQ15-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5A_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ15 delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ15.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(15, false);
    }

    // Ensure PCI command enables I/O space decode for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // ATAPI IDENTIFY PACKET DEVICE (0xA1) via secondary legacy ports.
    m.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0); // select secondary master
    m.io_write(SECONDARY_PORTS.cmd_base + 7, 1, 0xA1);

    // Verify that IDENTIFY data is reachable via the data port (0x170).
    let word0 = m.io_read(SECONDARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x8581);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ15 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_primary_dma_read_fills_memory_and_wakes_halted_cpu_via_irq14() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE Bus Master DMA + ISA IRQ wiring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master and seed it with a known prefix.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..8].copy_from_slice(b"DMA-BOOT");
    disk.write_sectors(0, &sector0).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IRQ14 into a real-mode handler that writes a flag byte.
    let vector = 0x2E_u8; // PIC slave base 0x28 + (IRQ14-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x3C_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ14 delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ14.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    let bdf = IDE_PIIX3.bdf;

    // Read BAR4 so the test is resilient to future default base changes.
    let bar4_raw = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected IDE BMIDE BAR4 to be programmed");

    // Prepare a single-entry PRD table (512 bytes, end-of-table).
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;
    m.write_physical_u32(prd_addr, data_buf as u32);
    m.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    m.write_physical_u16(prd_addr + 6, 0x8000);

    // Clear the destination buffer to a sentinel value first.
    m.write_physical(data_buf, &[0u8; 8]);

    // Enable only PCI I/O decoding for IDE (not bus mastering yet). This should allow guests to
    // program the BMIDE registers but prevent DMA progress.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Program BMIDE: PRDT base + start DMA in the "device -> memory" direction (bit3=1).
    m.io_write(bm_base + 4, 4, prd_addr as u32);
    m.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    m.io_write(bm_base, 1, 0x09);

    // Issue ATA READ DMA (0xC8) for LBA 0, count 1, primary master.
    m.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    m.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    m.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    m.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // With bus mastering disabled, the DMA transfer must not complete.
    for _ in 0..3 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(data_buf),
            b'D',
            "DMA should not run until PCI COMMAND.BME is enabled"
        );
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "IRQ14 should not fire until DMA completes"
        );
    }

    // Enable bus mastering: DMA should now complete and raise IRQ14.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            let prefix = m.read_physical_bytes(data_buf, 8);
            assert_eq!(prefix.as_slice(), b"DMA-BOOT");

            let bm_status = m.io_read(bm_base + 2, 1) as u8;
            assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
            return;
        }
    }

    panic!(
        "IDE primary DMA IRQ14 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_nien_masks_irq14_until_cleared() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE IRQ masking semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk to IDE primary master.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IRQ14 into a real-mode handler that writes a flag byte.
    let vector = 0x2E_u8; // PIC slave base 0x28 + (IRQ14-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xC7_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Configure the legacy PIC and unmask only cascade + IRQ14.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    // Enable PCI I/O decode for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Set nIEN (Device Control bit1) to mask interrupt output.
    m.io_write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // Issue ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Run a few slices; the interrupt is latched internally but must not be delivered while nIEN=1.
    for _ in 0..5 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "IRQ14 should be masked while nIEN=1"
        );
    }

    // Clear nIEN; the pending IRQ should now be delivered and wake the CPU.
    m.io_write(PRIMARY_PORTS.ctrl_base, 1, 0x00);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IDE IRQ14 interrupt handler did not run after clearing nIEN (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_primary_dma_write_updates_disk_and_wakes_halted_cpu_via_irq14() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE Bus Master DMA write + ISA IRQ wiring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a shared disk so we can verify the DMA commit wrote to the backend.
    let mut img = vec![0u8; 4 * SECTOR_SIZE];
    img[0..4].copy_from_slice(b"BOOT");
    let disk = aero_machine::SharedDisk::from_bytes(img).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk.clone()))
        .unwrap();

    // Route IRQ14 into a real-mode handler that writes a flag byte.
    let vector = 0x2E_u8; // PIC slave base 0x28 + (IRQ14-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x99_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ14 delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ14.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    let bdf = IDE_PIIX3.bdf;

    // Read BAR4 so the test is resilient to future default base changes.
    let bar4_raw = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected IDE BMIDE BAR4 to be programmed");

    // Prepare a single-entry PRD table (512 bytes, end-of-table).
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;
    m.write_physical_u32(prd_addr, data_buf as u32);
    m.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    m.write_physical_u16(prd_addr + 6, 0x8000);

    // Fill the guest buffer with a pattern to be written to LBA 1.
    let mut pattern = vec![0u8; SECTOR_SIZE];
    pattern[0..8].copy_from_slice(b"DMA-WRIT");
    for (i, b) in pattern.iter_mut().enumerate().skip(8) {
        *b = (i as u8).wrapping_mul(3).wrapping_add(0x5D);
    }
    m.write_physical(data_buf, &pattern);

    // Enable only PCI I/O decoding for IDE: should allow register programming, but DMA must not run
    // until COMMAND.BME is enabled.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Program BMIDE (PRDT base + start bit; direction=FromMemory for ATA writes).
    m.io_write(bm_base + 4, 4, prd_addr as u32);
    m.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    m.io_write(bm_base, 1, 0x01); // start, direction=0 (from memory)

    // Issue ATA WRITE DMA (0xCA) for LBA 1, count 1, primary master.
    m.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    m.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    m.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    m.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA);

    // With bus mastering disabled, the write must not commit.
    for _ in 0..3 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "IRQ14 should not fire until DMA completes"
        );

        let mut out = vec![0u8; SECTOR_SIZE];
        let mut disk_view = disk.clone();
        disk_view.read_sectors(1, &mut out).unwrap();
        assert_ne!(
            out.as_slice(),
            pattern.as_slice(),
            "DMA write should not commit until PCI COMMAND.BME is enabled"
        );
    }

    // Enable bus mastering: DMA should now complete and raise IRQ14.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            let mut out = vec![0u8; SECTOR_SIZE];
            let mut disk_view = disk.clone();
            disk_view.read_sectors(1, &mut out).unwrap();
            assert_eq!(out.as_slice(), pattern.as_slice());

            let bm_status = m.io_read(bm_base + 2, 1) as u8;
            assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
            assert_eq!(
                bm_status & 0x03,
                0,
                "BMIDE status should not show active/error"
            );
            return;
        }
    }

    panic!(
        "IDE primary DMA write IRQ14 handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ide_altstatus_does_not_clear_irq_pending_but_status_does() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE taskfile interrupt latching semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a small disk so the primary channel responds.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Enable PCI I/O decoding for the IDE function.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Issue ATA IDENTIFY DEVICE (0xEC).
    m.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
    m.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // The command should latch an interrupt condition.
    assert!(
        m.ide()
            .expect("ide enabled")
            .borrow()
            .controller
            .primary_irq_pending(),
        "expected primary IRQ to be pending after IDENTIFY"
    );

    // Reading alternate status must *not* clear the IRQ latch.
    let _ = m.io_read(PRIMARY_PORTS.ctrl_base, 1);
    assert!(
        m.ide()
            .expect("ide enabled")
            .borrow()
            .controller
            .primary_irq_pending(),
        "ALTSTATUS read should not clear the IRQ latch"
    );

    // Reading STATUS must clear the IRQ latch.
    let _ = m.io_read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(
        !m.ide()
            .expect("ide enabled")
            .borrow()
            .controller
            .primary_irq_pending(),
        "STATUS read should clear the IRQ latch"
    );
}

fn send_atapi_packet(m: &mut Machine, base: u16, features: u8, pkt: &[u8; 12], byte_count: u16) {
    m.io_write(base + 1, 1, u32::from(features));
    m.io_write(base + 4, 1, u32::from(byte_count & 0xFF));
    m.io_write(base + 5, 1, u32::from(byte_count >> 8));
    m.io_write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        m.io_write(base, 2, u32::from(w));
    }
}

#[test]
fn machine_ide_secondary_atapi_read10_dma_fills_memory_and_wakes_halted_cpu_via_irq15() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on ATAPI install-media style DMA on the secondary channel.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Build a tiny ISO image as a generic VirtualDisk (2048-byte sectors).
    let iso_sector = AtapiCdrom::SECTOR_SIZE as u64;
    let iso_capacity = 2 * iso_sector;
    let mut iso = RawDisk::create(MemBackend::new(), iso_capacity).unwrap();
    iso.write_at(iso_sector, b"WORLD").unwrap();
    m.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    // Route IRQ15 into a real-mode handler that writes a flag byte.
    let vector = 0x2F_u8; // PIC slave base 0x28 + (IRQ15-8)
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x42_u8;

    install_real_mode_handler(&mut m, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut m, code_base);
    write_ivt_entry(&mut m, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut m, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(m.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ15 delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ15.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(15, false);
    }

    let bdf = IDE_PIIX3.bdf;

    // Read BAR4 so the test is resilient to future default base changes.
    let bar4_raw = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected IDE BMIDE BAR4 to be programmed");

    // Destination buffer for one 2048-byte ISO sector.
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;

    // PRD entry: 2048 bytes, end-of-table.
    m.write_physical_u32(prd_addr, data_buf as u32);
    m.write_physical_u16(prd_addr + 4, AtapiCdrom::SECTOR_SIZE as u16);
    m.write_physical_u16(prd_addr + 6, 0x8000);

    // Clear destination buffer to ensure DMA actually fills it.
    m.write_physical(data_buf, &[0u8; 8]);

    // Enable only PCI I/O decode for IDE so we can program registers, but keep bus mastering off
    // to assert BME gating later.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Program BMIDE secondary channel: PRDT base + start DMA in the "device -> memory" direction.
    m.io_write(bm_base + 0x0C, 4, prd_addr as u32);
    m.io_write(bm_base + 0x0A, 1, 0x06); // clear error/irq bits
    m.io_write(bm_base + 0x08, 1, 0x09); // start + direction=to-memory

    // Clear the initial UNIT ATTENTION that real ATAPI devices report after media insertion.
    // The first command after insertion typically fails with "medium changed"; consume that here
    // so the subsequent READ(10) succeeds.
    let mut tur = [0u8; 12];
    tur[0] = 0x00; // TEST UNIT READY
    m.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    send_atapi_packet(&mut m, SECONDARY_PORTS.cmd_base, 0x00, &tur, 0);
    let _ = m.io_read(SECONDARY_PORTS.cmd_base + 7, 1); // clear IRQ

    // Send ATAPI READ(10) for LBA 1, blocks=1, using DMA (features bit0=1).
    let mut pkt = [0u8; 12];
    pkt[0] = 0x28; // READ(10)
    pkt[2..6].copy_from_slice(&1u32.to_be_bytes()); // LBA=1
    pkt[7..9].copy_from_slice(&1u16.to_be_bytes()); // blocks=1

    // Select secondary master and issue PACKET.
    m.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    send_atapi_packet(
        &mut m,
        SECONDARY_PORTS.cmd_base,
        0x01, // DMA requested
        &pkt,
        AtapiCdrom::SECTOR_SIZE as u16,
    );

    // Clear the "packet request" IRQ so we only observe the DMA completion interrupt.
    let _ = m.io_read(SECONDARY_PORTS.cmd_base + 7, 1);

    // With bus mastering disabled, the DMA transfer must not complete.
    for _ in 0..3 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(data_buf),
            b'W',
            "ATAPI DMA should not run until PCI COMMAND.BME is enabled"
        );
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "IRQ15 should not fire until DMA completes"
        );
    }

    // Enable bus mastering: DMA should now complete and raise IRQ15.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            let out = m.read_physical_bytes(data_buf, 5);
            assert_eq!(out.as_slice(), b"WORLD");

            let st = m.io_read(bm_base + 0x0A, 1) as u8;
            assert_ne!(st & 0x04, 0, "BMIDE secondary status IRQ bit should be set");
            assert_eq!(st & 0x02, 0, "BMIDE secondary status should not show error");
            return;
        }
    }

    panic!(
        "ATAPI DMA IRQ15 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}
