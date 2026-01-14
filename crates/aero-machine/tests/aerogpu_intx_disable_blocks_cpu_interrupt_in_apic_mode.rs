mod aerogpu_intx_helpers;

use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

use aerogpu_intx_helpers::{
    build_real_mode_interrupt_wait_boot_sector, ioapic_default_polarity_low, program_ioapic_entry,
    run_until_halt,
};

#[test]
fn aerogpu_intx_disable_blocks_cpu_interrupt_in_apic_mode() {
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

    const VECTOR: u8 = 0x60;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5Au8;
    let boot = build_real_mode_interrupt_wait_boot_sector(VECTOR, flag_addr, flag_value);

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");

    let bdf = AEROGPU.bdf;
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Switch to APIC mode and route the AeroGPU GSI to `VECTOR`.
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        let polarity_low = ioapic_default_polarity_low(gsi);
        let mut low = u32::from(VECTOR) | (1 << 15); // level-triggered
        if polarity_low {
            low |= 1 << 13; // active-low
        }
        program_ioapic_entry(&mut ints, gsi, low, 0);
    }

    // Enable PCI decoding + bus mastering, but set COMMAND.INTX_DISABLE.
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU config function should exist when enable_aerogpu=true");
        // COMMAND.MEM | COMMAND.BME | COMMAND.INTX_DISABLE
        cfg.set_command((1 << 1) | (1 << 2) | (1 << 10));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0, "AeroGPU BAR0 should be assigned by BIOS POST");

    // Clear the flag byte up-front so the negative assertion is deterministic.
    m.write_physical_u8(u64::from(flag_addr), 0);

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

    // While INTX_DISABLE is set, the device should latch IRQ_STATUS, but the CPU should not observe
    // the interrupt.
    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        assert_ne!(m.read_physical_u8(u64::from(flag_addr)), flag_value);
    }
    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(
        irq_status & pci::AEROGPU_IRQ_FENCE,
        0,
        "expected fence IRQ to latch even while INTx delivery is disabled"
    );

    // Clear INTX_DISABLE; the already-latched IRQ should now deliver and wake the CPU.
    {
        let pci_cfg = m.pci_config_ports().unwrap();
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg.bus_mut().device_config_mut(bdf).unwrap();
        let command = cfg.command();
        cfg.set_command(command & !(1 << 10));
    }

    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "AeroGPU INTx handler did not run after clearing INTX_DISABLE (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}
