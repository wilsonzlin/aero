#![cfg(all(feature = "aerogpu-wgpu-backend", not(target_arch = "wasm32")))]

use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding,
        AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{self as pci, AerogpuFormat},
    aerogpu_ring as ring,
    cmd_writer::AerogpuCmdWriter,
};

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for &w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn assemble_vs_passthrough_pos() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(
        0x0001,
        &[
            enc_dst(/*oPos*/ 4, /*reg*/ 0, /*mask*/ 0xF),
            enc_src(/*v0*/ 1, /*reg*/ 0, /*swizzle*/ 0xE4),
        ],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(
        0x0001,
        &[
            enc_dst(/*oC0*/ 8, /*reg*/ 0, /*mask*/ 0xF),
            enc_src(/*c0*/ 2, /*reg*/ 0, /*swizzle*/ 0xE4),
        ],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn d3d9_vertex_decl_pos4() -> Vec<u8> {
    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut decl = Vec::new();
    decl.extend_from_slice(&0u16.to_le_bytes()); // stream
    decl.extend_from_slice(&0u16.to_le_bytes()); // offset
    decl.push(3); // type = FLOAT4
    decl.push(0); // method
    decl.push(0); // usage = POSITION
    decl.push(0); // usage_index

    decl.extend_from_slice(&0x00FFu16.to_le_bytes()); // stream = 0xFF
    decl.extend_from_slice(&0u16.to_le_bytes()); // offset
    decl.push(17); // type = UNUSED
    decl.push(0); // method
    decl.push(0); // usage
    decl.push(0); // usage_index

    assert_eq!(decl.len(), 16);
    decl
}

fn make_cmd_stream(width: u32, height: u32) -> Vec<u8> {
    // Resource handles are arbitrary stable integers.
    const RT: u32 = 1;
    const VB: u32 = 2;
    const VS: u32 = 3;
    const PS: u32 = 4;
    const IL: u32 = 5;

    let mut vb_bytes = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces. Use clockwise winding.
    let verts = [
        (-0.8f32, -0.2f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
        (0.8f32, -0.2f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        vb_bytes.extend_from_slice(&x.to_le_bytes());
        vb_bytes.extend_from_slice(&y.to_le_bytes());
        vb_bytes.extend_from_slice(&z.to_le_bytes());
        vb_bytes.extend_from_slice(&w.to_le_bytes());
    }

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = d3d9_vertex_decl_pos4();

    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        RT,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        1,
        1,
        width * 4,
        0,
        0,
    );
    w.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_bytes.len() as u64,
        0,
        0,
    );
    w.upload_resource(VB, 0, &vb_bytes);

    w.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &vs_bytes);
    w.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_bytes);
    w.bind_shaders(VS, PS, 0);

    w.create_input_layout(IL, &vertex_decl);
    w.set_input_layout(IL);

    let binding = AerogpuVertexBufferBinding {
        buffer: VB,
        stride_bytes: 16,
        offset_bytes: 0,
        reserved0: 0,
    };
    w.set_vertex_buffers(0, &[binding]);
    w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

    w.set_render_targets(&[RT], 0);
    w.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    w.set_scissor(0, 0, width as i32, height as i32);

    w.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    // c0 = solid green.
    w.set_shader_constants_f(AerogpuShaderStage::Pixel, 0, &[0.0, 1.0, 0.0, 1.0]);
    w.draw(3, 1, 0, 0);
    w.present(0, 0);
    w.finish()
}

fn require_webgpu() -> bool {
    std::env::var("AERO_REQUIRE_WEBGPU").as_deref() == Ok("1")
}

fn is_missing_webgpu_adapter(err: &str) -> bool {
    err.contains("wgpu adapter not found") || err.contains("request_device failed")
}

#[test]
fn aerogpu_wgpu_backend_triangle_end_to_end() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for this integration test.
        enable_vga: false,
        enable_serial: false,
        enable_debugcon: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .expect("machine should construct");

    if let Err(err) = m.aerogpu_set_backend_wgpu() {
        if is_missing_webgpu_adapter(&err) && !require_webgpu() {
            eprintln!("skipping: {err}");
            return;
        }
        panic!("failed to create AeroGPU wgpu backend: {err}");
    }

    // Enable MMIO decoding + bus mastering so the device is allowed to DMA and raise IRQs.
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bdf = aero_devices::pci::profile::AEROGPU.bdf;
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu device missing from PCI bus");
        let command = cfg.command();
        cfg.set_command(command | (1 << 1) | (1 << 2));
    }

    let bar0_base = m
        .aerogpu_bar0_base()
        .expect("aerogpu BAR0 should be assigned");
    assert_ne!(bar0_base, 0, "BAR0 should be assigned by BIOS POST");

    // Guest memory layout:
    // - ring header + 1 entry at 0x1000
    // - fence page at 0x2000
    // - cmd stream at 0x3000
    let ring_gpa = 0x1000u64;
    let fence_gpa = 0x2000u64;
    let cmd_gpa = 0x3000u64;

    let width = 64u32;
    let height = 64u32;
    let cmd_stream = make_cmd_stream(width, height);
    m.write_physical(cmd_gpa, &cmd_stream);

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
    let fence_value = 1u64;
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32);
    m.write_physical_u32(desc_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT);
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, 0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, cmd_stream.len() as u32);
    m.write_physical_u32(desc_gpa + 28, 0); // cmd_reserved0
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0); // alloc_table_reserved0
    m.write_physical_u64(desc_gpa + 48, fence_value);
    m.write_physical_u64(desc_gpa + 56, 0); // reserved0

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

    // Doorbell: consume the ring entry and execute the command stream.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);

    // Tick the device until the fence completes.
    let mut completed = 0u64;
    for _ in 0..16 {
        m.process_aerogpu();
        let completed_lo =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO));
        let completed_hi =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI));
        completed = u64::from(completed_lo) | (u64::from(completed_hi) << 32);
        if completed == fence_value {
            break;
        }
    }
    assert_eq!(completed, fence_value, "submission fence should complete");

    let (out_w, out_h, rgba) = m
        .aerogpu_backend_read_scanout_rgba8(0)
        .expect("backend scanout 0 should be present");
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    // Verify background clear and triangle draw. The triangle is intentionally biased toward the
    // top of the framebuffer; if Y is flipped the "inside" probe will be black.
    assert_eq!(px(32, 2), [0, 0, 0, 255], "top probe should be background");
    assert_eq!(
        px(32, 48),
        [0, 0, 0, 255],
        "bottom probe should be background"
    );
    assert_eq!(
        px(32, 16),
        [0, 255, 0, 255],
        "center-top probe should be inside the triangle"
    );
}
