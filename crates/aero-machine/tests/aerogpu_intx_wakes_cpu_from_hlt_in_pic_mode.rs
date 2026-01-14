use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

fn build_real_mode_interrupt_wait_boot_sector(
    vector: u8,
    flag_addr: u16,
    flag_value: u8,
) -> [u8; 512] {
    let mut sector = [0u8; 512];
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
    // imm16 placeholder
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // mov word ptr [ivt_off+2], 0x0000 (segment)
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&(ivt_off + 2).to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

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

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for HLT: {other:?}"),
        }
    }
    panic!("machine did not reach HLT in time");
}

#[test]
fn aerogpu_intx_wakes_cpu_from_hlt_in_pic_mode() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for interrupt delivery.
        enable_vga: false,
        enable_serial: false,
        enable_debugcon: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");

    let bdf = AEROGPU.bdf;
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected AeroGPU INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let irq = u8::try_from(gsi).unwrap();
    let vector = if irq < 8 {
        0x20 + irq
    } else {
        0x28 + (irq - 8)
    };

    let flag_addr = 0x0500u16;
    let flag_value = 0x5Au8;
    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure the PIC for deterministic vectors and unmask only the routed IRQ (and cascade).
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for i in 0..16 {
            ints.pic_mut().set_masked(i, true);
        }
        if irq >= 8 {
            ints.pic_mut().set_masked(2, false);
        }
        ints.pic_mut().set_masked(irq, false);
    }

    // Enable PCI MMIO decode + bus mastering on the canonical AeroGPU function and resolve BAR0.
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU config function should exist when enable_aerogpu=true");
        // COMMAND.MEM | COMMAND.BME
        cfg.set_command((1 << 1) | (1 << 2));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0, "AeroGPU BAR0 should be assigned by BIOS POST");

    // Minimal ring + fence submission that signals fence=42 and requests an IRQ.
    let ring_gpa: u64 = 0x10000;
    let fence_gpa: u64 = 0x20000;

    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header.
    m.write_physical_u32(ring_gpa + 0, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc_gpa + 0, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u64(desc_gpa + 48, 42); // signal_fence

    // Program MMIO registers over BAR0.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Ring doorbell to submit work.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    // Run until the ISR fires (it writes `flag_value` to `flag_addr`).
    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "AeroGPU INTx handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}
