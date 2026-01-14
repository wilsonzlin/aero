#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn new_minimal_aerogpu_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for unit tests.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

fn enable_bus_mastering(m: &mut Machine) {
    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let cfg = pci_cfg
        .bus_mut()
        .device_config_mut(bdf)
        .expect("AeroGPU PCI function missing");
    cfg.set_command(cfg.command() | (1 << 2)); // COMMAND.BME
}

fn read_mmio_u64(m: &mut Machine, bar0: u64, lo_off: u32, hi_off: u32) -> u64 {
    (u64::from(m.read_physical_u32(bar0 + u64::from(hi_off))) << 32)
        | u64::from(m.read_physical_u32(bar0 + u64::from(lo_off)))
}

#[test]
fn aerogpu_submission_queue_overflow_completes_dropped_fence() {
    // The device-side submission capture queue is bounded. If the host fails to drain it (e.g. GPU
    // worker restart), the device must not deadlock fences waiting on out-of-process completions.
    //
    // Submit MAX+1 fences, drain only the latest MAX submissions (simulating an overflow drop),
    // and ensure the machine still reaches the final fence value without requiring the host to
    // explicitly complete the dropped fence.
    const MAX_PENDING_SUBMISSIONS: usize = 256;
    const SUBMISSION_COUNT: usize = MAX_PENDING_SUBMISSIONS + 1;

    let mut m = new_minimal_aerogpu_machine();
    m.aerogpu_enable_submission_bridge();
    enable_bus_mastering(&mut m);

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be assigned by BIOS");

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Use a minimal non-empty command stream so submissions are captured for the bridge.
    m.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

    // Build a ring containing SUBMISSION_COUNT submit descriptors (head=0, tail=N).
    let entry_count = 512u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header.
    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, SUBMISSION_COUNT as u32); // tail

    // Submit descriptors.
    let desc_base = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    for i in 0..SUBMISSION_COUNT {
        let desc_gpa = desc_base + (i as u64) * u64::from(entry_stride_bytes);
        let fence = (i as u64) + 1;

        m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
        m.write_physical_u32(desc_gpa + 4, 0); // flags
        m.write_physical_u32(desc_gpa + 8, 0); // context_id
        m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
        m.write_physical_u64(desc_gpa + 16, cmd_gpa);
        m.write_physical_u32(desc_gpa + 24, 4); // cmd_size_bytes
        m.write_physical_u32(desc_gpa + 28, 0);
        m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
        m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
        m.write_physical_u32(desc_gpa + 44, 0);
        m.write_physical_u64(desc_gpa + 48, fence);
        m.write_physical_u64(desc_gpa + 56, 0);
    }

    // Program BAR0 registers.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Consume the ring.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    // Ring head advanced fully.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), SUBMISSION_COUNT as u32);

    // The oldest submission (fence=1) is dropped by the bounded submission queue. The device must
    // complete that fence immediately to avoid deadlocking later fences.
    let completed_after_doorbell = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_after_doorbell, 1);

    // Drain submissions: should contain fences 2..=SUBMISSION_COUNT.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), MAX_PENDING_SUBMISSIONS);
    assert_eq!(subs[0].signal_fence, 2);
    assert_eq!(
        subs.last().expect("subs non-empty").signal_fence,
        SUBMISSION_COUNT as u64
    );

    // Simulate the host completing only the fences it observed (fences 2..=SUBMISSION_COUNT).
    // The dropped fence=1 must not block forward progress.
    for sub in subs {
        m.aerogpu_complete_fence(sub.signal_fence);
    }

    let completed = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed, SUBMISSION_COUNT as u64);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), SUBMISSION_COUNT as u64);
}
