use aero_devices::pci::PciInterruptPin;
use aero_machine::{ImmediateAeroGpuBackend, Machine, MachineConfig};
use aero_platform::interrupts::InterruptController as PlatformInterruptController;
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_immediate_backend_completes_fence_and_raises_intx() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.aerogpu_set_backend(Box::new(ImmediateAeroGpuBackend::new()))
        .unwrap();

    let bdf = aero_devices::pci::profile::AEROGPU.bdf;

    // Enable PCI memory decoding + bus mastering so the device is allowed to DMA.
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu device missing from PCI bus");
        cfg.set_command(cfg.command() | (1 << 1) | (1 << 2));
        cfg.bar_range(0).map(|r| r.base).unwrap_or(0)
    };
    assert!(
        bar0_base != 0,
        "expected AeroGPU BAR0 to be assigned by BIOS POST"
    );

    // Minimal ring with a single submission that signals fence 1.
    const RING_GPA: u64 = 0x20_0000;
    const FENCE_GPA: u64 = 0x21_0000;

    let ring_size_bytes =
        (ring::AerogpuRingHeader::SIZE_BYTES + ring::AerogpuSubmitDesc::SIZE_BYTES) as u32;

    let mut ring_hdr = [0u8; ring::AerogpuRingHeader::SIZE_BYTES];
    ring_hdr[0..4].copy_from_slice(&ring::AEROGPU_RING_MAGIC.to_le_bytes());
    ring_hdr[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    ring_hdr[8..12].copy_from_slice(&ring_size_bytes.to_le_bytes());
    ring_hdr[12..16].copy_from_slice(&1u32.to_le_bytes()); // entry_count
    ring_hdr[16..20].copy_from_slice(&(ring::AerogpuSubmitDesc::SIZE_BYTES as u32).to_le_bytes()); // stride
    ring_hdr[24..28].copy_from_slice(&0u32.to_le_bytes()); // head
    ring_hdr[28..32].copy_from_slice(&1u32.to_le_bytes()); // tail
    m.write_physical(RING_GPA, &ring_hdr);

    let mut submit = [0u8; ring::AerogpuSubmitDesc::SIZE_BYTES];
    submit[0..4].copy_from_slice(&(ring::AerogpuSubmitDesc::SIZE_BYTES as u32).to_le_bytes());
    submit[12..16].copy_from_slice(&ring::AEROGPU_ENGINE_0.to_le_bytes());
    submit[48..56].copy_from_slice(&1u64.to_le_bytes());
    m.write_physical(
        RING_GPA + ring::AerogpuRingHeader::SIZE_BYTES as u64,
        &submit,
    );

    // Program MMIO registers and ring the doorbell.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        RING_GPA as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (RING_GPA >> 32) as u32,
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
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        FENCE_GPA as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (FENCE_GPA >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    // Let the device make forward progress: consume the ring entry and complete the fence.
    m.process_aerogpu();

    let completed_fence = {
        let lo =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
        let hi =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
        u64::from(lo) | (u64::from(hi) << 32)
    };
    assert_eq!(completed_fence, 1);

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status, pci::AEROGPU_IRQ_FENCE);

    // Fence page in guest RAM should also reflect completion.
    let fence_page = m.read_physical_bytes(FENCE_GPA, ring::AerogpuFencePage::SIZE_BYTES);
    let fence_page = ring::AerogpuFencePage::decode_from_le_bytes(&fence_page).unwrap();
    // `AerogpuFencePage` is `#[repr(packed)]`, so avoid taking references to its fields.
    let fence_magic = fence_page.magic;
    let fence_completed = fence_page.completed_fence;
    assert_eq!(fence_magic, ring::AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(fence_completed, 1);

    // Now verify the machine routes INTx into the platform interrupt controller.
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");

    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected aerogpu INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let expected_vector = if gsi < 8 {
        0x20u8.wrapping_add(gsi as u8)
    } else {
        0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
    };

    // Configure PIC offsets and unmask only the routed IRQ (and cascade if needed).
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        // Cascade.
        ints.pic_mut().set_masked(2, false);
        if let Ok(irq) = u8::try_from(gsi) {
            ints.pic_mut().set_masked(irq, false);
        }
    }

    // Synchronize PCI INTx sources into the platform interrupt controller.
    m.poll_pci_intx_lines();

    let pending = PlatformInterruptController::get_pending(&*interrupts.borrow());
    assert_eq!(pending, Some(expected_vector));
}
