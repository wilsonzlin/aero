#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::ATA_CMD_WRITE_DMA_EXT;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use firmware::bios::BlockDevice as _;

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
fn machine_processes_ahci_and_can_wake_a_halted_cpu_via_intx() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on AHCI + PCI INTx wiring.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Attach a small disk to AHCI port 0.
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();
    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;
    let irq = {
        let router = m.pci_intx_router().expect("pc platform enabled");
        let gsi = router.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        assert!(
            gsi < 16,
            "expected AHCI INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
        );
        u8::try_from(gsi).unwrap()
    };
    let vector = if irq < 8 {
        0x20 + irq
    } else {
        0x28 + (irq - 8)
    };
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

    // Enable delivery through the legacy PIC.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        if irq >= 8 {
            // Unmask cascade when routing to the slave PIC.
            ints.pic_mut().set_masked(2, false);
        }
        ints.pic_mut().set_masked(irq, false);
    }

    // Program the AHCI controller.
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the machine's PCI MMIO window (deterministic address).
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );

    // Sanity-check COMMAND.MEM gating: with MEM disabled, MMIO reads should return all-ones.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0004);
    let vs_disabled = m.read_physical_u32(bar5_base + 0x10);
    assert_eq!(vs_disabled, 0xFFFF_FFFF);

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Sanity-check that MMIO is live (AHCI VS register).
    let vs = m.read_physical_u32(bar5_base + 0x10);
    assert_eq!(vs, 0x0001_0300);

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    m.write_physical_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    m.write_physical_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // IDENTIFY DMA.
    write_cmd_header(&mut m, clb, 0, ctba, 1, false);
    write_cfis(&mut m, ctba, 0xEC, 0, 0); // ATA IDENTIFY
    write_prdt(&mut m, ctba, 0, identify_buf, SECTOR_SIZE as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            // DMA should have filled the IDENTIFY buffer before raising the interrupt.
            let identify = m.read_physical_bytes(identify_buf, SECTOR_SIZE);
            assert_eq!(identify[0], 0x40);

            // Clear the controller interrupt so it doesn't remain asserted across the test suite.
            m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
            return;
        }
    }

    panic!(
        "AHCI INTx interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_ahci_writes_are_visible_to_bios_disk_reads() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on disk sharing between BIOS and AHCI.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Ensure the disk is large enough for LBA 1 writes. `set_disk_image` updates the shared backend
    // and re-attaches it to AHCI so ATA IDENTIFY geometry stays coherent.
    m.set_disk_image(vec![0u8; 4 * SECTOR_SIZE]).unwrap();

    // Program the AHCI controller.
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the machine's PCI MMIO window (deterministic address).
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    // Basic port programming.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let write_buf = 0x4000u64;

    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    m.write_physical_u32(bar5_base + HBA_GHC, GHC_AE);
    m.write_physical_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Guest buffer -> disk write payload.
    let mut pattern = vec![0u8; SECTOR_SIZE];
    pattern[0..8].copy_from_slice(b"AHCI-BIO");
    for (i, b) in pattern.iter_mut().enumerate().skip(8) {
        *b = (i as u8).wrapping_mul(7).wrapping_add(0x3D);
    }
    m.write_physical(write_buf, &pattern);

    // WRITE DMA EXT to LBA 1.
    write_cmd_header(&mut m, clb, 0, ctba, 1, true);
    write_cfis(&mut m, ctba, ATA_CMD_WRITE_DMA_EXT, 1, 1);
    write_prdt(&mut m, ctba, 0, write_buf, SECTOR_SIZE as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Run the controller until the command completes.
    for _ in 0..16 {
        m.process_ahci();
        if m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI) == 0 {
            break;
        }
    }
    assert_eq!(m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI), 0);

    // Read back via the BIOS `BlockDevice` view of the disk.
    let mut bios_disk = m.shared_disk();
    let mut sector = [0u8; 512];
    bios_disk.read_sector(1, &mut sector).unwrap();
    assert_eq!(&sector[..], &pattern[..]);
}

#[test]
fn machine_exposes_ich9_ahci_at_canonical_bdf_and_bar5_mmio_works() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 for deterministic MMIO access behaviour.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let bdf = SATA_AHCI_ICH9.bdf;

    // Read vendor/device ID via PCI config ports (guest-visible).
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bdf.bus, bdf.device, bdf.function, 0x00),
    );
    let id = m.io_read(PCI_CFG_DATA_PORT, 4);
    assert_eq!(
        id,
        (u32::from(SATA_AHCI_ICH9.device_id) << 16) | u32::from(SATA_AHCI_ICH9.vendor_id)
    );

    // Interrupt Line should match the active PCI INTx router configuration.
    let expected_gsi = {
        let router = m.pci_intx_router().expect("pc platform enabled");
        let gsi = router.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        gsi
    };
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bdf.bus, bdf.device, bdf.function, 0x3C),
    );
    let irq_line = m.io_read(PCI_CFG_DATA_PORT, 1) as u8;
    assert_eq!(irq_line, u8::try_from(expected_gsi).unwrap());

    // BAR5 should be assigned by firmware POST and routed through the PCI MMIO window.
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bdf.bus, bdf.device, bdf.function, AHCI_ABAR_CFG_OFFSET),
    );
    let bar5_reg = m.io_read(PCI_CFG_DATA_PORT, 4) as u64;
    let bar5_base = bar5_reg & !0xFu64;
    assert!(bar5_base != 0, "expected AHCI BAR5 to be assigned");

    // Enable memory decoding (COMMAND.MEM bit 1) before accessing the ABAR.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // Smoke-test BAR5 MMIO dispatch by reading CAP/PI.
    let cap = m.read_physical_u32(bar5_base);
    let pi = m.read_physical_u32(bar5_base + 0x0C);

    assert_eq!(cap, 0x8000_1F00);
    assert_eq!(pi, 0x0000_0001);
}

#[test]
fn machine_gates_ahci_dma_on_pci_bus_master_enable() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep the machine minimal for deterministic polling.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Attach a small disk to AHCI port 0.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    // Program the AHCI controller.
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the machine's PCI MMIO window (deterministic address).
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );

    // Enable memory decoding but keep bus mastering disabled.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // Basic port programming.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    m.write_physical_u32(bar5_base + HBA_GHC, GHC_AE);
    m.write_physical_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // Issue IDENTIFY DMA (slot 0).
    write_cmd_header(&mut m, clb, 0, ctba, 1, false);
    write_cfis(&mut m, ctba, 0xEC, 0, 0);
    write_prdt(&mut m, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    // Ensure the buffer starts cleared so we can detect whether DMA ran.
    m.write_physical_u32(identify_buf, 0);

    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Without Bus Master Enable, `process_ahci()` must not perform DMA or complete the command.
    for _ in 0..8 {
        m.process_ahci();
    }
    assert_eq!(
        m.read_physical_u8(identify_buf),
        0,
        "AHCI DMA should be gated off when PCI bus mastering is disabled"
    );
    assert_eq!(
        m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI),
        1,
        "AHCI command issue bits should remain set when PCI bus mastering is disabled"
    );

    // Now enable bus mastering and re-run processing; the pending command should complete.
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    for _ in 0..32 {
        m.process_ahci();
        if m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI) == 0 {
            break;
        }
    }
    assert_eq!(m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI), 0);

    let identify = m.read_physical_bytes(identify_buf, SECTOR_SIZE);
    assert_eq!(identify[0], 0x40);
}
