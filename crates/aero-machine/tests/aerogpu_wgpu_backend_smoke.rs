#![cfg(all(feature = "aerogpu-wgpu-backend", not(target_arch = "wasm32")))]

use std::time::{Duration, Instant};

use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn require_webgpu() -> bool {
    let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

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

fn mmio_read_u64_pair(m: &mut Machine, lo: u64, hi: u64) -> u64 {
    (u64::from(m.read_physical_u32(hi)) << 32) | u64::from(m.read_physical_u32(lo))
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= cmd::AerogpuCmdHdr::SIZE_BYTES as u32);
    assert!(size_bytes.is_multiple_of(4));
    out[start + 4..start + 8].copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header
    push_u32(&mut out, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, pci::AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, cmd::AerogpuCmdStreamFlags::None as u32); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    out
}

#[test]
fn aerogpu_wgpu_backend_smoke_executes_acmd_and_updates_scanout() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    if let Err(err) = m.aerogpu_set_backend_wgpu() {
        if err.contains("wgpu adapter not found") || err.contains("request_device failed") {
            skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_wgpu_backend_smoke_executes_acmd_and_updates_scanout"
                ),
                &err,
            );
            return;
        }
        panic!("failed to initialize AeroGPU wgpu backend: {err}");
    }

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);

    // BAR0 base (assigned by `bios_post`).
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= (1 << 1) | (1 << 2); // COMMAND.MSE | COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Command buffer: create RT texture, bind it, clear green, present scanout0.
    let (width, height) = (4u32, 4u32);
    let stream = build_stream(|out| {
        emit_packet(out, cmd::AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 1); // texture_handle
            push_u32(
                out,
                cmd::AEROGPU_RESOURCE_USAGE_TEXTURE
                    | cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                    | cmd::AEROGPU_RESOURCE_USAGE_SCANOUT,
            ); // usage_flags
            push_u32(out, pci::AerogpuFormat::R8G8B8A8Unorm as u32); // format
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, cmd::AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, 1); // colors[0]
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, cmd::AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, cmd::AEROGPU_CLEAR_COLOR);
            push_u32(out, 0.0f32.to_bits()); // r
            push_u32(out, 1.0f32.to_bits()); // g
            push_u32(out, 0.0f32.to_bits()); // b
            push_u32(out, 1.0f32.to_bits()); // a
            push_u32(out, 1.0f32.to_bits()); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, cmd::AerogpuCmdOpcode::Present as u32, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });
    m.write_physical(cmd_gpa, &stream);

    // Build a minimal valid ring containing a single submit desc (head=0, tail=1).
    let entry_count = 8u32;
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
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;

    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);

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

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Doorbell.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    let start = Instant::now();
    let mut completed = 0u64;
    let mut page_fence = 0u64;
    loop {
        m.process_aerogpu();

        completed = mmio_read_u64_pair(
            &mut m,
            bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO),
            bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI),
        );
        page_fence = m.read_physical_u64(fence_gpa + 8);

        if completed >= signal_fence && page_fence == signal_fence {
            break;
        }

        if start.elapsed() > Duration::from_secs(5) {
            panic!(
                "timed out waiting for fence completion (completed={completed} fence_page={page_fence})"
            );
        }
    }

    assert_eq!(completed, signal_fence);
    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        m.read_physical_u32(fence_gpa + 4),
        pci::AEROGPU_ABI_VERSION_U32
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);

    // IRQ status latched and ACK clears it.
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, pci::AEROGPU_IRQ_FENCE);

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ACK),
        pci::AEROGPU_IRQ_FENCE,
    );
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);

    // Host-visible scanout output should contain the clear color.
    m.display_present();
    assert_eq!(m.display_resolution(), (width, height));

    let fb = m.display_framebuffer();
    assert_eq!(fb.len(), (width * height) as usize);

    let green = u32::from_le_bytes([0, 255, 0, 255]);
    assert!(fb.iter().any(|&px| px == green));
}
