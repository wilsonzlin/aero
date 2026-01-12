#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::{NIC_E1000_82540EM, SATA_AHCI_ICH9};
use aero_devices::pci::PciInterruptPin;
use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_net_e1000::ICR_TXDW;
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, InterruptInput, PlatformInterruptMode,
};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::MemoryBus as _;

fn write_u16_le(pc: &mut PcMachine, paddr: u64, value: u16) {
    pc.bus
        .platform
        .memory
        .write_physical(paddr, &value.to_le_bytes());
}

fn write_ivt_entry(pc: &mut PcMachine, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    write_u16_le(pc, base, offset);
    write_u16_le(pc, base + 2, segment);
}

fn install_real_mode_handler(pc: &mut PcMachine, handler_addr: u64, flag_addr: u16, value: u8) {
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
    pc.bus.platform.memory.write_physical(handler_addr, &code);
}

fn install_hlt_loop(pc: &mut PcMachine, code_base: u64) {
    // hlt; jmp short $-3 (back to hlt)
    let code = [0xF4u8, 0xEB, 0xFD];
    pc.bus.platform.memory.write_physical(code_base, &code);
}

fn setup_real_mode_cpu(pc: &mut PcMachine, entry_ip: u64) {
    pc.cpu = aero_cpu_core::CpuCore::new(aero_cpu_core::state::CpuMode::Real);

    // Real-mode segments: base = selector<<4, limit = 0xFFFF.
    for seg in [
        &mut pc.cpu.state.segments.cs,
        &mut pc.cpu.state.segments.ds,
        &mut pc.cpu.state.segments.es,
        &mut pc.cpu.state.segments.ss,
        &mut pc.cpu.state.segments.fs,
        &mut pc.cpu.state.segments.gs,
    ] {
        seg.selector = 0;
        seg.base = 0;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    pc.cpu.state.set_stack_ptr(0x8000);
    pc.cpu.state.set_rip(entry_ip);
    pc.cpu.state.set_rflags(0x202); // IF=1
    pc.cpu.state.halted = false;
}

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

// Minimal AHCI constants/helpers for interrupt delivery tests.
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

const ATA_CMD_IDENTIFY: u8 = 0xEC;

fn write_cmd_header(pc: &mut PcMachine, clb: u64, slot: usize, ctba: u64, prdtl: u16) {
    let cfl = 5u32;
    let flags = cfl | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    pc.bus.platform.memory.write_u32(addr, flags);
    pc.bus.platform.memory.write_u32(addr + 4, 0); // PRDBC
    pc.bus.platform.memory.write_u32(addr + 8, ctba as u32);
    pc.bus
        .platform
        .memory
        .write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(pc: &mut PcMachine, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    pc.bus.platform.memory.write_u32(addr, dba as u32);
    pc.bus
        .platform
        .memory
        .write_u32(addr + 4, (dba >> 32) as u32);
    pc.bus.platform.memory.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    pc.bus
        .platform
        .memory
        .write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(pc: &mut PcMachine, ctba: u64, command: u8, lba: u64, count: u16) {
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

    pc.bus.platform.memory.write_physical(ctba, &cfis);
}

#[test]
fn pc_machine_delivers_pic_interrupt_to_real_mode_ivt_handler() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let handler_addr = 0x1000u64;
    let code_base = 0x2000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5Au8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Run until the CPU executes HLT.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Configure the PIC and raise IRQ1.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    // Run until the handler writes the flag byte.
    for _ in 0..10 {
        let _ = pc.run_slice(128);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode PIC interrupt handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_does_not_ack_pic_interrupt_when_if0() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let code_base = 0x2000u64;

    install_hlt_loop(&mut pc, code_base);
    setup_real_mode_cpu(&mut pc, code_base);
    pc.cpu.state.set_rflags(0); // IF=0

    // Run until the CPU executes HLT.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Configure the PIC and raise IRQ1.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    // Sanity: the interrupt controller sees the pending vector.
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        Some(vector)
    );

    // While halted with IF=0, the machine should not acknowledge or enqueue the interrupt.
    assert_eq!(pc.run_slice(16), RunExit::Halted { executed: 0 });
    assert!(pc.cpu.pending.external_interrupts.is_empty());
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        Some(vector)
    );
}

#[test]
fn pc_machine_e1000_intx_is_synced_but_not_acknowledged_when_if0() {
    let mut pc = PcMachine::new_with_e1000(2 * 1024 * 1024, None);

    let bdf = NIC_E1000_82540EM.bdf;
    let gsi = pc
        .bus
        .platform
        .pci_intx
        .gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected E1000 INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let expected_vector = if gsi < 8 {
        0x20u8.wrapping_add(gsi as u8)
    } else {
        0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
    };

    // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
        ints.pic_mut().set_masked(2, false);
        if let Ok(irq) = u8::try_from(gsi) {
            if irq < 16 {
                ints.pic_mut().set_masked(irq, false);
            }
        }
    }

    // Park the CPU at a NOP sled and simulate HLT with IF=0.
    const ENTRY_IP: u64 = 0x2000;
    pc.bus.platform.memory.write_physical(ENTRY_IP, &[0x90; 32]);
    setup_real_mode_cpu(&mut pc, ENTRY_IP);
    pc.cpu.state.set_rflags(0); // IF=0
    pc.cpu.state.halted = true;

    // Assert E1000 INTx level by enabling + setting a cause bit. This sets `irq_level()` in the
    // device model, but does not automatically drive the platform IRQ line until we poll/sync.
    let e1000 = pc.bus.platform.e1000().expect("e1000 enabled");
    {
        let mut dev = e1000.borrow_mut();
        dev.mmio_write_u32_reg(0x00D0, ICR_TXDW); // IMS
        dev.mmio_write_u32_reg(0x00C8, ICR_TXDW); // ICS
        assert!(dev.irq_level());
    }

    // Prior to running a slice, the INTx level has not been synced into the platform interrupt
    // controller yet.
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        None
    );

    // With IF=0, `run_slice` must not acknowledge the interrupt or enqueue a vector, but it should
    // still sync PCI INTx sources so the PIC sees the asserted line.
    let exit = pc.run_slice(5);
    assert_eq!(exit, RunExit::Halted { executed: 0 });
    assert!(pc.cpu.pending.external_interrupts.is_empty());
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        Some(expected_vector)
    );
}

#[test]
fn pc_machine_e1000_intx_is_synced_but_not_acknowledged_during_interrupt_shadow() {
    let mut pc = PcMachine::new_with_e1000(2 * 1024 * 1024, None);

    let bdf = NIC_E1000_82540EM.bdf;
    let gsi = pc
        .bus
        .platform
        .pci_intx
        .gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected E1000 INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let expected_vector = if gsi < 8 {
        0x20u8.wrapping_add(gsi as u8)
    } else {
        0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
    };

    // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
        ints.pic_mut().set_masked(2, false);
        if let Ok(irq) = u8::try_from(gsi) {
            if irq < 16 {
                ints.pic_mut().set_masked(irq, false);
            }
        }
    }

    // Assert E1000 INTx level by enabling + setting a cause bit. This sets `irq_level()` in the
    // device model, but does not automatically drive the platform IRQ line until we poll/sync.
    let e1000 = pc.bus.platform.e1000().expect("e1000 enabled");
    {
        let mut dev = e1000.borrow_mut();
        dev.mmio_write_u32_reg(0x00D0, ICR_TXDW); // IMS
        dev.mmio_write_u32_reg(0x00C8, ICR_TXDW); // ICS
        assert!(dev.irq_level());
    }

    // Prior to running a slice, the INTx level has not been synced into the platform interrupt
    // controller yet.
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        None
    );

    // Run a single instruction while simulating an `STI` interrupt shadow.
    const ENTRY_IP: u64 = 0x2000;
    pc.bus.platform.memory.write_physical(ENTRY_IP, &[0x90; 32]);
    setup_real_mode_cpu(&mut pc, ENTRY_IP);
    pc.cpu.pending.inhibit_interrupts_for_one_instruction();

    let exit = pc.run_slice(1);
    assert_eq!(exit, RunExit::Completed { executed: 1 });
    assert!(pc.cpu.pending.external_interrupts.is_empty());

    // Even though the interrupt shadow blocked delivery, the machine should still have synced the
    // asserted INTx level into the PIC so it remains pending until it can be acknowledged.
    assert_eq!(
        PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
        Some(expected_vector)
    );
}

#[test]
fn pc_machine_e1000_intx_asserted_via_bar1_io_wakes_hlt_in_same_slice() {
    let mut pc = PcMachine::new_with_e1000(2 * 1024 * 1024, None);

    let bdf = NIC_E1000_82540EM.bdf;
    let gsi = pc
        .bus
        .platform
        .pci_intx
        .gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected E1000 INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let expected_vector = if gsi < 8 {
        0x20u8.wrapping_add(gsi as u8)
    } else {
        0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
    };

    // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
        ints.pic_mut().set_masked(2, false);
        if let Ok(irq) = u8::try_from(gsi) {
            if irq < 16 {
                ints.pic_mut().set_masked(irq, false);
            }
        }
    }

    // Resolve the E1000 BAR1 I/O port base assigned by BIOS POST.
    let bar1_base = {
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .device_config(bdf)
            .and_then(|cfg| cfg.bar_range(1))
            .expect("missing E1000 BAR1")
            .base
    };
    let ioaddr_port = u16::try_from(bar1_base).expect("E1000 BAR1 should fit in u16 I/O space");
    let iodata_port = ioaddr_port.wrapping_add(4);

    // Install a real-mode ISR for the expected vector that clears the interrupt by reading ICR.
    //
    // Handler:
    //   mov byte ptr [0x2000], 0xAA
    //   ; clear interrupt by reading ICR via BAR1
    //   mov dx, ioaddr_port
    //   mov eax, 0x00C0 (ICR)
    //   out dx, eax
    //   mov dx, iodata_port
    //   in eax, dx
    //   iret
    const HANDLER_IP: u16 = 0x1100;
    let mut handler = Vec::new();
    handler.extend_from_slice(&[0xC6, 0x06, 0x00, 0x20, 0xAA]); // mov byte ptr [0x2000], 0xAA
    handler.extend_from_slice(&[0xBA, (ioaddr_port & 0xFF) as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
    handler.extend_from_slice(&[0x66, 0xB8]);
    handler.extend_from_slice(&0x00C0u32.to_le_bytes()); // mov eax, ICR
    handler.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
    handler.extend_from_slice(&[0xBA, (iodata_port & 0xFF) as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
    handler.extend_from_slice(&[0x66, 0xED]); // in eax, dx
    handler.push(0xCF); // iret
    pc.bus
        .platform
        .memory
        .write_physical(u64::from(HANDLER_IP), &handler);
    write_ivt_entry(&mut pc, expected_vector, 0x0000, HANDLER_IP);

    // Guest program:
    //   ; IMS = ICR_TXDW
    //   mov dx, ioaddr_port
    //   mov eax, 0x00D0 (IMS)
    //   out dx, eax
    //   mov dx, iodata_port
    //   mov eax, ICR_TXDW
    //   out dx, eax
    //
    //   ; ICS = ICR_TXDW (assert INTx)
    //   mov dx, ioaddr_port
    //   mov eax, 0x00C8 (ICS)
    //   out dx, eax
    //   mov dx, iodata_port
    //   mov eax, ICR_TXDW
    //   out dx, eax
    //
    //   hlt
    //   hlt
    const ENTRY_IP: u16 = 0x1000;
    let mut code = Vec::new();
    // IOADDR = IMS
    code.extend_from_slice(&[0xBA, (ioaddr_port & 0xFF) as u8, (ioaddr_port >> 8) as u8]);
    code.extend_from_slice(&[0x66, 0xB8]);
    code.extend_from_slice(&0x00D0u32.to_le_bytes());
    code.extend_from_slice(&[0x66, 0xEF]);
    // IODATA = ICR_TXDW
    code.extend_from_slice(&[0xBA, (iodata_port & 0xFF) as u8, (iodata_port >> 8) as u8]);
    code.extend_from_slice(&[0x66, 0xB8]);
    code.extend_from_slice(&ICR_TXDW.to_le_bytes());
    code.extend_from_slice(&[0x66, 0xEF]);
    // IOADDR = ICS
    code.extend_from_slice(&[0xBA, (ioaddr_port & 0xFF) as u8, (ioaddr_port >> 8) as u8]);
    code.extend_from_slice(&[0x66, 0xB8]);
    code.extend_from_slice(&0x00C8u32.to_le_bytes());
    code.extend_from_slice(&[0x66, 0xEF]);
    // IODATA = ICR_TXDW
    code.extend_from_slice(&[0xBA, (iodata_port & 0xFF) as u8, (iodata_port >> 8) as u8]);
    code.extend_from_slice(&[0x66, 0xB8]);
    code.extend_from_slice(&ICR_TXDW.to_le_bytes());
    code.extend_from_slice(&[0x66, 0xEF]);
    // HLT (twice so we can observe wakeup + re-halt deterministically).
    code.extend_from_slice(&[0xF4, 0xF4]);

    pc.bus
        .platform
        .memory
        .write_physical(u64::from(ENTRY_IP), &code);
    pc.bus.platform.memory.write_u8(0x2000, 0);

    setup_real_mode_cpu(&mut pc, u64::from(ENTRY_IP));

    // One slice should be sufficient: the guest asserts INTx, executes HLT, and the machine
    // should sync + deliver the interrupt within the same `run_slice` call, running the ISR.
    let _ = pc.run_slice(100);
    assert_eq!(pc.bus.platform.memory.read_u8(0x2000), 0xAA);
}

#[test]
fn pc_machine_delivers_ioapic_interrupt_to_real_mode_ivt_handler() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    let vector = 0x60u8;
    let gsi = 10u32;
    let handler_addr = 0x1100u64;
    let code_base = 0x2100u64;
    let flag_addr = 0x0501u16;
    let flag_value = 0xA5u8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and route GSI10 to `vector` (level-triggered, active-low).
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = u32::from(vector) | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut ints, gsi, low, 0);
        ints.raise_irq(InterruptInput::Gsi(gsi));
    }

    for _ in 0..10 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode IOAPIC interrupt handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_delivers_e1000_pci_intx_via_legacy_pic() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // Create the PC machine, but swap in a platform that includes the E1000 device.
    let mut pc = PcMachine::new(RAM_SIZE);
    pc.bus =
        aero_pc_platform::PcCpuBus::new(aero_pc_platform::PcPlatform::new_with_e1000(RAM_SIZE));

    // E1000 device 00:05.0 INTA# => (pin+device)%4 = (0+5)%4 = 1 => PIRQB => GSI11 (default config).
    // With PIC offsets 0x20/0x28: IRQ11 => vector 0x2B.
    let vector = 0x2B_u8;

    let handler_addr = 0x1200u64;
    let code_base = 0x2200u64;
    let flag_addr = 0x0502u16;
    let flag_value = 0xC3u8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Stop in HLT so the interrupt must be delivered to wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ11 delivery through the legacy PIC.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ11.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(11, false);
    }

    // Locate BAR0 for the E1000 MMIO window (assigned during BIOS POST).
    let bar0_base = {
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("E1000 config function must exist");
        assert_ne!(
            cfg.command() & 0x2,
            0,
            "E1000 MMIO decoding must be enabled"
        );
        cfg.bar_range(0).expect("E1000 BAR0 must exist").base
    };
    assert_ne!(
        bar0_base, 0,
        "E1000 BAR0 should be assigned during BIOS POST"
    );

    // Trigger an interrupt inside the E1000 model by enabling IMS and setting ICS.
    //
    // We use ICR_TXDW (bit 0) as an arbitrary interrupt cause; this avoids having to program
    // descriptor rings. Writing to ICS sets the corresponding bit in ICR and asserts INTx when
    // unmasked in IMS.
    const REG_ICS: u64 = 0x00C8;
    const REG_IMS: u64 = 0x00D0;
    const ICR_TXDW: u32 = 1 << 0;
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_IMS, ICR_TXDW);
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_ICS, ICR_TXDW);

    // Run until the handler executes.
    for _ in 0..10 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode E1000 INTx handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_delivers_e1000_pci_intx_after_tx_dma_sets_txdw() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const MIN_L2_FRAME_LEN: usize = 14;

    // Create the PC machine, but swap in a platform that includes the E1000 device.
    let mut pc = PcMachine::new(RAM_SIZE);
    pc.bus =
        aero_pc_platform::PcCpuBus::new(aero_pc_platform::PcPlatform::new_with_e1000(RAM_SIZE));

    // E1000 device 00:05.0 INTA# => (pin+device)%4 = (0+5)%4 = 1 => PIRQB => GSI11 (default config).
    // With PIC offsets 0x20/0x28: IRQ11 => vector 0x2B.
    let vector = 0x2B_u8;

    let handler_addr = 0x1300u64;
    let code_base = 0x2300u64;
    let flag_addr = 0x0503u16;
    let flag_value = 0x5Bu8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Stop in HLT so the interrupt must be delivered to wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ11 delivery through the legacy PIC.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ11.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(11, false);
    }

    // Locate BAR0 for the E1000 MMIO window (assigned during BIOS POST) and enable bus mastering
    // so TX DMA can run in `PcPlatform::process_e1000`.
    let bar0_base = {
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let cfg = bus
            .device_config_mut(bdf)
            .expect("E1000 config function must exist");
        cfg.set_command(cfg.command() | (1 << 2));
        assert_ne!(
            cfg.command() & 0x2,
            0,
            "E1000 MMIO decoding must be enabled"
        );
        cfg.bar_range(0).expect("E1000 BAR0 must exist").base
    };
    assert_ne!(
        bar0_base, 0,
        "E1000 BAR0 should be assigned during BIOS POST"
    );

    // Program a minimal TX ring with one descriptor (ring size must be >= 2 for head!=tail).
    let tx_desc_base = 0x10_000u64;
    let tx_buf_addr = 0x11_000u64;

    let frame = vec![0x11u8; MIN_L2_FRAME_LEN];
    pc.bus.platform.memory.write_physical(tx_buf_addr, &frame);

    // Legacy TX descriptor: buffer_addr + length + cmd(EOP|RS).
    let mut desc0 = [0u8; 16];
    desc0[0..8].copy_from_slice(&tx_buf_addr.to_le_bytes());
    desc0[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
    desc0[11] = (1 << 0) | (1 << 3); // EOP|RS
    pc.bus.platform.memory.write_physical(tx_desc_base, &desc0);
    pc.bus
        .platform
        .memory
        .write_physical(tx_desc_base + 16, &[0u8; 16]);

    // Enable TXDW interrupt cause.
    const REG_IMS: u64 = 0x00D0;
    const ICR_TXDW: u32 = 1 << 0;
    pc.bus
        .platform
        .memory
        .write_u32(bar0_base + REG_IMS, ICR_TXDW);

    // Program E1000 TX registers over MMIO (BAR0).
    const REG_TCTL: u64 = 0x0400;
    const REG_TDBAL: u64 = 0x3800;
    const REG_TDBAH: u64 = 0x3804;
    const REG_TDLEN: u64 = 0x3808;
    const REG_TDH: u64 = 0x3810;
    const REG_TDT: u64 = 0x3818;
    const TCTL_EN: u32 = 1 << 1;

    let mem = &mut pc.bus.platform.memory;
    mem.write_u32(bar0_base + REG_TDBAL, tx_desc_base as u32);
    mem.write_u32(bar0_base + REG_TDBAH, (tx_desc_base >> 32) as u32);
    mem.write_u32(bar0_base + REG_TDLEN, 32);
    mem.write_u32(bar0_base + REG_TDH, 0);
    mem.write_u32(bar0_base + REG_TDT, 0);
    mem.write_u32(bar0_base + REG_TCTL, TCTL_EN);

    // Doorbell: publish descriptor 0 (TDH=0, TDT=1).
    mem.write_u32(bar0_base + REG_TDT, 1);

    // Run a single slice. Correct ordering in `PcMachine::run_slice` must ensure:
    // - E1000 DMA runs first (processing the TX descriptor and setting ICR.TXDW),
    // - then PCI INTx lines are polled/latched into the PIC,
    // - then the interrupt is delivered to wake the CPU and execute the handler.
    let _ = pc.run_slice(512);

    assert_eq!(
        pc.bus.platform.memory.read_u8(u64::from(flag_addr)),
        flag_value,
        "expected E1000 TXDW DMA interrupt to be delivered within the same run_slice call"
    );
}

#[test]
fn pc_machine_delivers_ahci_pci_intx_via_legacy_pic() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcMachine::new(RAM_SIZE);
    pc.bus
        .platform
        .attach_ahci_disk_port0(Box::new(disk))
        .unwrap();

    // AHCI device 00:02.0 INTA# => (pin+device)%4 = (0+2)%4 = 2 => PIRQC => GSI12 (default config).
    // With PIC offsets 0x20/0x28: IRQ12 => vector 0x2C.
    let vector = 0x2C_u8;

    let handler_addr = 0x1300u64;
    let code_base = 0x2300u64;
    let flag_addr = 0x0503u16;
    let flag_value = 0x5Au8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Stop in HLT so the interrupt must be delivered to wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Enable IRQ12 delivery through the legacy PIC.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        // Unmask cascade + IRQ12.
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(12, false);
    }

    let bdf = SATA_AHCI_ICH9.bdf;

    // Locate BAR5 for the AHCI MMIO window (assigned during BIOS POST).
    let bar5_base = {
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("AHCI config function must exist");
        assert_ne!(cfg.command() & 0x2, 0, "AHCI MMIO decoding must be enabled");
        cfg.bar_range(5).expect("AHCI BAR5 must exist").base
    };
    assert_ne!(
        bar5_base, 0,
        "AHCI BAR5 should be assigned during BIOS POST"
    );

    // Enable PCI bus mastering so AHCI DMA is permitted.
    {
        let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus
            .device_config(bdf)
            .expect("AHCI config function must exist")
            .command();
        bus.write_config(bdf, 0x04, 2, u32::from(command | (1 << 2)));
    }

    // Program HBA + port 0 registers.
    let clb = 0x3000u64;
    let fb = 0x4000u64;
    let ctba = 0x5000u64;
    let identify_buf = 0x6000u64;

    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.bus.platform.memory.write_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Issue an IDENTIFY command; PcMachine's run loop calls `PcPlatform::process_ahci()` so the
    // device will complete the command and assert INTx.
    write_cmd_header(&mut pc, clb, 0, ctba, 1);
    write_cfis(&mut pc, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(
        &mut pc,
        ctba,
        0,
        identify_buf,
        SECTOR_SIZE.try_into().unwrap(),
    );
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Run until the handler executes.
    for _ in 0..10 {
        let _ = pc.run_slice(512);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            // Validate that DMA wrote the IDENTIFY sector.
            assert_eq!(pc.bus.platform.memory.read_u16(identify_buf), 0x0040);

            // Clear the interrupt so it deasserts (avoid holding IRQ12 asserted indefinitely).
            pc.bus
                .platform
                .memory
                .write_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
            return;
        }
    }

    panic!(
        "real-mode AHCI INTx handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}
