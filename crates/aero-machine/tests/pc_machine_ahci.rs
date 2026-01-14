#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(pc: &mut PcMachine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.bus.platform.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.bus
        .platform
        .io
        .write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcMachine, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.bus.platform.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.bus.platform.io.write(PCI_CFG_DATA_PORT, 4, value);
}

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

    pc.cpu.state.set_stack_ptr(0x7000);
    pc.cpu.state.set_rip(entry_ip);
    pc.cpu.state.set_rflags(0x202); // IF=1
    pc.cpu.state.halted = false;
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

fn write_cmd_header(
    mem: &mut dyn memory::MemoryBus,
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
    mem.write_u32(addr, flags);
    mem.write_u32(addr + 4, 0); // PRDBC
    mem.write_u32(addr + 8, ctba as u32);
    mem.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(mem: &mut dyn memory::MemoryBus, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    mem.write_u32(addr, dba as u32);
    mem.write_u32(addr + 4, (dba >> 32) as u32);
    mem.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    mem.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(mem: &mut dyn memory::MemoryBus, ctba: u64, command: u8, lba: u64, count: u16) {
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

    mem.write_physical(ctba, &cfis);
}

#[test]
fn pc_machine_processes_ahci_and_can_wake_a_halted_cpu_via_intx() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    let mut pc = PcMachine::new(RAM_SIZE);

    // Attach a small disk to AHCI port 0.
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();
    pc.bus
        .platform
        .attach_ahci_disk_port0(Box::new(disk))
        .unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;
    let gsi = pc
        .bus
        .platform
        .pci_intx
        .gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected AHCI INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let irq = u8::try_from(gsi).unwrap();
    let vector = if irq < 8 {
        0x20 + irq
    } else {
        0x28 + (irq - 8)
    };
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5A_u8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut pc, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Enable delivery through the legacy PIC.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        if irq >= 8 {
            // Unmask cascade when routing to the slave PIC.
            ints.pic_mut().set_masked(2, false);
        }
        ints.pic_mut().set_masked(irq, false);
    }

    // Program the AHCI controller.
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the platform's PCI MMIO window (deterministic address).
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Sanity-check that MMIO is live (AHCI VS register).
    let vs = pc.bus.platform.memory.read_u32(bar5_base + 0x10);
    assert_eq!(vs, 0x0001_0300);

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

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

    // IDENTIFY DMA.
    write_cmd_header(&mut pc.bus.platform.memory, clb, 0, ctba, 1, false);
    write_cfis(&mut pc.bus.platform.memory, ctba, 0xEC, 0, 0); // ATA IDENTIFY
    write_prdt(
        &mut pc.bus.platform.memory,
        ctba,
        0,
        identify_buf,
        SECTOR_SIZE as u32,
    );
    pc.bus
        .platform
        .memory
        .write_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            // DMA should have filled the IDENTIFY buffer before raising the interrupt.
            let mut identify = [0u8; SECTOR_SIZE];
            pc.bus
                .platform
                .memory
                .read_physical(identify_buf, &mut identify);
            assert_eq!(identify[0], 0x40);

            // Clear the controller interrupt so it doesn't remain asserted across the test suite.
            pc.bus
                .platform
                .memory
                .write_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
            return;
        }
    }

    panic!(
        "AHCI INTx interrupt handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}
