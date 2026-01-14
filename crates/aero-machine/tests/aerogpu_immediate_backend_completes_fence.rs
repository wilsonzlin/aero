use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

#[test]
fn aerogpu_immediate_backend_completes_fence() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for this unit test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .expect("machine should construct");

    // Start with the null backend: submissions are consumed, but fences must not progress.
    m.aerogpu_set_backend_null();

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bdf = aero_devices::pci::profile::AEROGPU.bdf;

    // Enable MMIO decoding + bus mastering so the device is allowed to DMA and raise IRQs.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu device missing from PCI bus");
        let command = cfg.command();
        cfg.set_command(command | (1 << 1) | (1 << 2));
    }

    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("aerogpu device missing from PCI bus");
        cfg.bar_range(0).expect("missing aerogpu BAR0").base
    };
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Guest memory layout:
    // - ring header + 1 entry at 0x1000
    // - fence page at 0x2000
    let ring_gpa = 0x1000u64;
    let fence_gpa = 0x2000u64;

    let entry_count = 1u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Write the ring header (one pending entry: head=0, tail=1).
    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Write one submission descriptor with a signal fence.
    let fence_value = 7u64;
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32);
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, 0); // engine_id
    m.write_physical_u64(desc_gpa + 16, 0); // cmd_gpa
    m.write_physical_u32(desc_gpa + 24, 0); // cmd_size_bytes
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u64(desc_gpa + 48, fence_value); // signal_fence

    // Program the AeroGPU MMIO registers.
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
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Doorbell: consume the ring entry and enqueue backend work.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    // Tick the device so backend fence completions are observed.
    m.process_aerogpu();

    // The ring head should have advanced to tail.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // With the null backend installed, the completed fence must not advance.
    let completed_lo =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
    let completed_hi =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
    let completed = u64::from(completed_lo) | (u64::from(completed_hi) << 32);
    assert_eq!(completed, 0);

    // And the fence IRQ status bit should not be raised.
    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);

    // Fence page is still written (to keep it initialized/coherent), but must report
    // `COMPLETED_FENCE=0` while the null backend is installed.
    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);

    // Reset the machine. The null backend should remain installed across reset, so submissions
    // should still be consumed without advancing fences.
    m.reset();

    // Enable MMIO decoding + bus mastering again (PCI COMMAND resets to 0).
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu device missing from PCI bus");
        let command = cfg.command();
        cfg.set_command(command | (1 << 1) | (1 << 2));
    }

    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("aerogpu device missing from PCI bus");
        cfg.bar_range(0).expect("missing aerogpu BAR0").base
    };
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Rewrite the ring header + descriptor in case firmware used the low-memory region during
    // reset/POST.
    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32);
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, 0); // engine_id
    m.write_physical_u64(desc_gpa + 16, 0); // cmd_gpa
    m.write_physical_u32(desc_gpa + 24, 0); // cmd_size_bytes
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u64(desc_gpa + 48, fence_value); // signal_fence

    // Program the AeroGPU MMIO registers again (device state resets on `Machine::reset`).
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
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);
    let completed_lo =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
    let completed_hi =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
    let completed = u64::from(completed_lo) | (u64::from(completed_hi) << 32);
    assert_eq!(completed, 0);

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);

    // Switch to the immediate backend and re-submit the same fence. This must complete.
    m.aerogpu_set_backend_immediate();

    // Reset the ring header to re-run the single entry (head=0, tail=1).
    m.write_physical_u32(ring_gpa + 24, 0);
    m.write_physical_u32(ring_gpa + 28, 1);
    m.write_physical_u64(desc_gpa + 48, fence_value); // signal_fence

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let completed_lo =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
    let completed_hi =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
    let completed = u64::from(completed_lo) | (u64::from(completed_hi) << 32);
    assert_eq!(completed, fence_value);

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);

    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), fence_value);

    // Reset again: the immediate backend should remain installed across reset.
    m.reset();

    // Re-enable PCI decoding + bus mastering after reset and re-program the BAR0 device registers.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu device missing from PCI bus");
        let command = cfg.command();
        cfg.set_command(command | (1 << 1) | (1 << 2));
    }
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("aerogpu device missing from PCI bus");
        cfg.bar_range(0).expect("missing aerogpu BAR0").base
    };
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Re-write the ring header/descriptor.
    m.write_physical_u32(ring_gpa + 24, 0);
    m.write_physical_u32(ring_gpa + 28, 1);
    m.write_physical_u64(desc_gpa + 48, fence_value); // signal_fence

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
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let completed_lo =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
    let completed_hi =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
    let completed = u64::from(completed_lo) | (u64::from(completed_hi) << 32);
    assert_eq!(completed, fence_value);
}
