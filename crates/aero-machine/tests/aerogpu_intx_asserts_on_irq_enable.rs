use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

#[test]
fn aerogpu_intx_asserts_on_irq_enable() {
    // Minimal machine with the PC platform interrupt controller and AeroGPU enabled.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for interrupt assertions.
        enable_vga: false,
        enable_serial: false,
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
    let pci_intx = m
        .pci_intx_router()
        .expect("pc platform should provide PCI INTx router");
    let interrupts = m
        .platform_interrupts()
        .expect("pc platform should provide PlatformInterrupts");

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
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should provide PCI config ports");
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

    // Submit descriptor in slot 0. Only `desc_size_bytes`, `flags`, and `signal_fence` are required
    // for this interrupt routing test.
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
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    // DMA + IRQ status update (fence completion).
    m.process_aerogpu();

    // Until the machine polls PCI INTx sources, the asserted line should not be visible to the PIC.
    assert_eq!(interrupts.borrow().get_pending(), None);

    m.poll_pci_intx_lines();
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(vector),
        "expected AeroGPU INTx to pend PIC vector {vector:#x}"
    );
}
