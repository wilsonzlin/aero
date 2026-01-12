#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::atapi::AtapiCdrom;
use aero_machine::{Machine, MachineConfig, RunExit};
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
    m.io_write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    m.io_read(0xCFC, 4)
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
    m.io_write(0x1F6, 1, 0xA0); // select primary master
    m.io_write(0x1F7, 1, 0xEC);

    // Verify that IDENTIFY data is reachable via the data port (0x1F0).
    let word0 = m.io_read(0x1F0, 2) as u16;
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
    m.io_write(0x176, 1, 0xA0); // select secondary master
    m.io_write(0x177, 1, 0xA1);

    // Verify that IDENTIFY data is reachable via the data port (0x170).
    let word0 = m.io_read(0x170, 2) as u16;
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
    m.io_write(0x1F2, 1, 1);
    m.io_write(0x1F3, 1, 0);
    m.io_write(0x1F4, 1, 0);
    m.io_write(0x1F5, 1, 0);
    m.io_write(0x1F6, 1, 0xE0);
    m.io_write(0x1F7, 1, 0xC8);

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
    m.io_write(0x3F6, 1, 0x02);

    // Issue ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    m.io_write(0x1F6, 1, 0xA0);
    m.io_write(0x1F7, 1, 0xEC);

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
    m.io_write(0x3F6, 1, 0x00);

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
