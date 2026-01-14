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
fn aerogpu_bme_toggle_gates_ring_dma_and_scanout_reads() {
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

    // Enable PCI Memory decoding and Bus Mastering (DMA).
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= (1 << 1) | (1 << 2);
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // ---------------------------------------------------------------------
    // 1) Program a tiny WDDM scanout in guest RAM and verify BME gates reads.
    // ---------------------------------------------------------------------
    let fb_gpa = 0x0040_0000u64;
    // Pixel (0,0): B,G,R,X = AA,BB,CC,00 -> displayed as 0xFFAA_BBCC.
    m.write_physical_u8(fb_gpa, 0xAA);
    m.write_physical_u8(fb_gpa + 1, 0xBB);
    m.write_physical_u8(fb_gpa + 2, 0xCC);
    m.write_physical_u8(fb_gpa + 3, 0x00);

    let width = 1u32;
    let height = 1u32;
    let pitch = width * 4;

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), width);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), height);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // ---------------------------------------------------------------------
    // 2) Ring processing: ensure disabling BME in PCI config halts DMA/fence
    //    completion until BME is restored.
    // ---------------------------------------------------------------------

    // Guest allocations.
    let ring_gpa = 0x0010_0000u64;
    let fence_gpa = 0x0020_0000u64;
    let cmd_gpa = 0x0030_0000u64;

    m.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

    // Build a minimal valid ring containing two submit descs.
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header (head=0, tail=1).
    m.write_physical_u32(ring_gpa + 0, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence0 = 0x1111_2222_3333_4444u64;

    m.write_physical_u32(desc0_gpa + 0, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc0_gpa + 4, 0); // flags
    m.write_physical_u32(desc0_gpa + 8, 0); // context_id
    m.write_physical_u32(desc0_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc0_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc0_gpa + 24, 4); // cmd_size_bytes
    m.write_physical_u32(desc0_gpa + 28, 0);
    m.write_physical_u64(desc0_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc0_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc0_gpa + 44, 0);
    m.write_physical_u64(desc0_gpa + 48, signal_fence0);
    m.write_physical_u64(desc0_gpa + 56, 0);

    // Program BAR0 ring + fence registers.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO), ring_gpa as u32);
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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO), fence_gpa as u32);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Process the first submission (BME is enabled).
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence0);

    // Submit desc in slot 1 (tail=2).
    let desc1_gpa = desc0_gpa + u64::from(entry_stride_bytes);
    let signal_fence1 = signal_fence0.wrapping_add(1);

    m.write_physical_u32(desc1_gpa + 0, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc1_gpa + 4, 0); // flags
    m.write_physical_u32(desc1_gpa + 8, 0); // context_id
    m.write_physical_u32(desc1_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc1_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc1_gpa + 24, 4); // cmd_size_bytes
    m.write_physical_u32(desc1_gpa + 28, 0);
    m.write_physical_u64(desc1_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc1_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc1_gpa + 44, 0);
    m.write_physical_u64(desc1_gpa + 48, signal_fence1);
    m.write_physical_u64(desc1_gpa + 56, 0);

    m.write_physical_u32(ring_gpa + 28, 2); // tail

    // Ring the doorbell while bus mastering is enabled, then immediately disable BME in the
    // canonical PCI config space. The tick path must observe the new COMMAND.BME value without
    // relying on an MMIO-synchronized internal config image.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    command &= !(1 << 2);
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    m.process_aerogpu();
    assert_eq!(
        m.read_physical_u32(ring_gpa + 24),
        1,
        "ring head should not advance while COMMAND.BME=0"
    );
    assert_eq!(
        m.read_physical_u64(fence_gpa + 8),
        signal_fence0,
        "fence page should not advance while COMMAND.BME=0"
    );

    // Scanout reads must also be gated by BME.
    m.display_present();
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // Re-enable BME and verify processing + scanout resume.
    command |= 1 << 2;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    m.process_aerogpu();
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 2);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence1);

    m.display_present();
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);
}

