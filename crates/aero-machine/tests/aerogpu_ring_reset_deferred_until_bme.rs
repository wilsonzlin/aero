use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

#[test]
fn aerogpu_ring_reset_updates_ring_head_once_dma_is_enabled() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal and deterministic for this unit test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);

    // BAR0 base (assigned by `bios_post`).
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;

    // Build a minimal valid ring (head=0, tail=5).
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 5); // tail

    // Pre-fill the fence page with a sentinel so we can detect whether DMA wrote it.
    m.write_physical_u32(fence_gpa, 0xDEAD_BEEF);

    // Program BAR0 ring + fence registers.
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

    // Sanity: Bus mastering is disabled by default after BIOS POST.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    assert_eq!(command & (1 << 2), 0, "expected COMMAND.BME=0");

    // Request a ring reset while DMA is disabled. The device should record the reset immediately,
    // but must defer DMA writes to guest memory (ring head + fence page) until BME is enabled.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE | pci::AEROGPU_RING_CONTROL_RESET,
    );
    m.process_aerogpu();

    // With COMMAND.BME=0, head is unchanged and the fence page was not written.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 0);
    assert_eq!(m.read_physical_u32(fence_gpa), 0xDEAD_BEEF);

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let command = command | (1 << 2);
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));
    m.process_aerogpu();

    // Ring reset DMA now runs: head := tail, and the fence page is re-initialized.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 5);
    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);
}
