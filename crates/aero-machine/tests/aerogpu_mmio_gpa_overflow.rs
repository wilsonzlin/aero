#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;
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

#[test]
fn aerogpu_ring_reset_with_overflowing_ring_gpa_is_nonpanicking_and_records_oob_error() {
    let mut m = new_minimal_aerogpu_machine();
    enable_bus_mastering(&mut m);

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be assigned by BIOS");

    // Program a ring GPA that will overflow when the device tries to touch the ring header fields
    // during ring reset (e.g. `ring_gpa + RING_TAIL_OFFSET`).
    let ring_gpa = u64::MAX - 1;
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );

    // Trigger ring reset. This must not panic in debug builds even with a pathological GPA.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_RESET,
    );
    m.process_aerogpu();

    let error_code = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE));
    let error_fence =
        (u64::from(m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI)))
            << 32)
            | u64::from(
                m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO)),
            );
    let error_count = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT));
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(error_code, pci::AerogpuErrorCode::Oob as u32);
    assert_eq!(error_fence, 0);
    assert_eq!(error_count, 1);
    assert_ne!(irq_status & pci::AEROGPU_IRQ_ERROR, 0);
}

#[test]
fn aerogpu_doorbell_with_overflowing_ring_gpa_records_oob_error() {
    let mut m = new_minimal_aerogpu_machine();
    enable_bus_mastering(&mut m);

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be assigned by BIOS");

    // Program a ring GPA that will overflow when the device tries to read the ring header / compute
    // descriptor addresses during doorbell processing.
    let ring_gpa = u64::MAX - 1;
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
        0x1000,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let error_code = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE));
    let error_fence =
        (u64::from(m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI)))
            << 32)
            | u64::from(
                m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO)),
            );
    let error_count = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT));
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(error_code, pci::AerogpuErrorCode::Oob as u32);
    assert_eq!(error_fence, 0);
    assert_eq!(error_count, 1);
    assert_ne!(irq_status & pci::AEROGPU_IRQ_ERROR, 0);
}

#[test]
fn aerogpu_fence_page_write_with_overflowing_gpa_is_nonpanicking() {
    let mut m = new_minimal_aerogpu_machine();
    enable_bus_mastering(&mut m);

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be assigned by BIOS");

    // Program a fence page GPA that would overflow when writing fields (e.g. `gpa + 8`).
    let fence_gpa = u64::MAX - 4;
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Trigger ring reset, which writes the fence page when configured.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_RESET,
    );
    m.process_aerogpu();

    let error_code = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE));
    let error_count = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT));
    assert_eq!(error_code, pci::AerogpuErrorCode::None as u32);
    assert_eq!(error_count, 0);
}
