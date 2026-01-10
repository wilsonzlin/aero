use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendState, AerogpuCmdBindShaders, AerogpuCmdClear, AerogpuCmdCreateBuffer, AerogpuCmdCreateShaderDxbc,
    AerogpuCmdCreateTexture2d, AerogpuCmdDestroyResource, AerogpuCmdDestroyShader, AerogpuCmdDraw,
    AerogpuCmdDrawIndexed, AerogpuCmdHdr, AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdResourceDirtyRange,
    AerogpuCmdSetBlendState, AerogpuCmdSetDepthStencilState, AerogpuCmdSetIndexBuffer, AerogpuCmdSetRasterizerState,
    AerogpuCmdSetRenderTargets, AerogpuCmdSetScissor, AerogpuCmdSetVertexBuffers, AerogpuCmdSetViewport,
    AerogpuCmdStreamHeader, AerogpuDepthStencilState, AerogpuRasterizerState, AerogpuVertexBufferBinding,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::{
    parse_and_validate_abi_version_u32, AerogpuAbiError, AerogpuFormat, AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR,
    AEROGPU_ABI_VERSION_U32, AEROGPU_FEATURE_FENCE_PAGE, AEROGPU_IRQ_FENCE, AEROGPU_MMIO_MAGIC, AEROGPU_MMIO_REG_DOORBELL,
    AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_VENDOR_ID, AEROGPU_RING_CONTROL_ENABLE,
};
use aero_protocol::aerogpu::aerogpu_ring::{
    write_fence_page_completed_fence_le, AerogpuAllocEntry, AerogpuAllocTableHeader, AerogpuFencePage, AerogpuRingHeader,
    AerogpuSubmitDesc, AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC,
    AEROGPU_SUBMIT_FLAG_NO_IRQ, AEROGPU_SUBMIT_FLAG_PRESENT,
};

#[derive(Debug, Default)]
struct AbiDump {
    sizes: HashMap<String, usize>,
    offsets: HashMap<String, usize>,
    consts: HashMap<String, u64>,
}

impl AbiDump {
    fn size(&self, ty: &str) -> usize {
        *self.sizes.get(ty).unwrap_or_else(|| panic!("missing SIZE for {ty}"))
    }

    fn offset(&self, ty: &str, field: &str) -> usize {
        let key = format!("{ty}.{field}");
        *self
            .offsets
            .get(&key)
            .unwrap_or_else(|| panic!("missing OFF for {key}"))
    }

    fn konst(&self, name: &str) -> u64 {
        *self
            .consts
            .get(name)
            .unwrap_or_else(|| panic!("missing CONST for {name}"))
    }
}

fn compile_and_run_c_abi_dump() -> AbiDump {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.join("../..");
    let c_src = crate_dir.join("tests/aerogpu_abi_dump.c");

    let mut out_path = std::env::temp_dir().join(format!("aerogpu_abi_dump_{}", std::process::id()));
    if cfg!(windows) {
        out_path.set_extension("exe");
    }

    let status = Command::new("cc")
        .arg("-I")
        .arg(&repo_root)
        .arg("-std=c11")
        .arg("-o")
        .arg(&out_path)
        .arg(&c_src)
        .status()
        .expect("failed to spawn C compiler");
    assert!(status.success(), "C compiler failed with status {status}");

    let output = Command::new(&out_path)
        .output()
        .expect("failed to run compiled C ABI dump helper");
    assert!(
        output.status.success(),
        "C ABI dump helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    parse_c_abi_dump_output(String::from_utf8(output.stdout).expect("C ABI dump output was not UTF-8"))
}

fn parse_c_abi_dump_output(text: String) -> AbiDump {
    let mut dump = AbiDump::default();

    for (line_no, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        let tag = parts[0];
        match tag {
            "SIZE" => {
                assert_eq!(parts.len(), 3, "bad SIZE line @{}: {line}", line_no + 1);
                dump.sizes.insert(parts[1].to_string(), parts[2].parse().unwrap());
            }
            "OFF" => {
                assert_eq!(parts.len(), 4, "bad OFF line @{}: {line}", line_no + 1);
                dump.offsets
                    .insert(format!("{}.{}", parts[1], parts[2]), parts[3].parse().unwrap());
            }
            "CONST" => {
                assert_eq!(parts.len(), 3, "bad CONST line @{}: {line}", line_no + 1);
                dump.consts.insert(parts[1].to_string(), parts[2].parse().unwrap());
            }
            other => panic!("unknown ABI dump tag {other} in line @{}: {line}", line_no + 1),
        }
    }

    dump
}

fn abi_dump() -> &'static AbiDump {
    static ABI: OnceLock<AbiDump> = OnceLock::new();
    ABI.get_or_init(compile_and_run_c_abi_dump)
}

#[test]
fn rust_layout_matches_c_headers() {
    let abi = abi_dump();

    macro_rules! assert_size {
        ($ty:ty, $c_name:literal) => {
            assert_eq!(abi.size($c_name), core::mem::size_of::<$ty>(), "sizeof({})", $c_name);
        };
    }

    macro_rules! assert_off {
        ($ty:ty, $field:tt, $c_ty:literal, $c_field:literal) => {
            assert_eq!(
                abi.offset($c_ty, $c_field),
                core::mem::offset_of!($ty, $field),
                "offsetof({}.{})",
                $c_ty,
                $c_field
            );
        };
    }

    // Command stream.
    assert_size!(AerogpuCmdStreamHeader, "aerogpu_cmd_stream_header");
    assert_size!(AerogpuCmdHdr, "aerogpu_cmd_hdr");
    assert_off!(AerogpuCmdStreamHeader, magic, "aerogpu_cmd_stream_header", "magic");
    assert_off!(AerogpuCmdStreamHeader, abi_version, "aerogpu_cmd_stream_header", "abi_version");
    assert_off!(AerogpuCmdStreamHeader, size_bytes, "aerogpu_cmd_stream_header", "size_bytes");
    assert_off!(AerogpuCmdStreamHeader, flags, "aerogpu_cmd_stream_header", "flags");
    assert_off!(AerogpuCmdHdr, opcode, "aerogpu_cmd_hdr", "opcode");
    assert_off!(AerogpuCmdHdr, size_bytes, "aerogpu_cmd_hdr", "size_bytes");

    // Command packet sizes.
    assert_size!(AerogpuCmdCreateBuffer, "aerogpu_cmd_create_buffer");
    assert_size!(AerogpuCmdCreateTexture2d, "aerogpu_cmd_create_texture2d");
    assert_size!(AerogpuCmdDestroyResource, "aerogpu_cmd_destroy_resource");
    assert_size!(AerogpuCmdResourceDirtyRange, "aerogpu_cmd_resource_dirty_range");
    assert_size!(AerogpuCmdCreateShaderDxbc, "aerogpu_cmd_create_shader_dxbc");
    assert_size!(AerogpuCmdDestroyShader, "aerogpu_cmd_destroy_shader");
    assert_size!(AerogpuCmdBindShaders, "aerogpu_cmd_bind_shaders");
    assert_size!(AerogpuBlendState, "aerogpu_blend_state");
    assert_size!(AerogpuCmdSetBlendState, "aerogpu_cmd_set_blend_state");
    assert_size!(AerogpuDepthStencilState, "aerogpu_depth_stencil_state");
    assert_size!(AerogpuCmdSetDepthStencilState, "aerogpu_cmd_set_depth_stencil_state");
    assert_size!(AerogpuRasterizerState, "aerogpu_rasterizer_state");
    assert_size!(AerogpuCmdSetRasterizerState, "aerogpu_cmd_set_rasterizer_state");
    assert_size!(AerogpuCmdSetRenderTargets, "aerogpu_cmd_set_render_targets");
    assert_size!(AerogpuCmdSetViewport, "aerogpu_cmd_set_viewport");
    assert_size!(AerogpuCmdSetScissor, "aerogpu_cmd_set_scissor");
    assert_size!(AerogpuVertexBufferBinding, "aerogpu_vertex_buffer_binding");
    assert_size!(AerogpuCmdSetVertexBuffers, "aerogpu_cmd_set_vertex_buffers");
    assert_size!(AerogpuCmdSetIndexBuffer, "aerogpu_cmd_set_index_buffer");
    assert_size!(AerogpuCmdClear, "aerogpu_cmd_clear");
    assert_size!(AerogpuCmdDraw, "aerogpu_cmd_draw");
    assert_size!(AerogpuCmdDrawIndexed, "aerogpu_cmd_draw_indexed");
    assert_size!(AerogpuCmdPresent, "aerogpu_cmd_present");

    // Ring structs.
    assert_size!(AerogpuAllocTableHeader, "aerogpu_alloc_table_header");
    assert_size!(AerogpuAllocEntry, "aerogpu_alloc_entry");
    assert_size!(AerogpuSubmitDesc, "aerogpu_submit_desc");
    assert_size!(AerogpuRingHeader, "aerogpu_ring_header");
    assert_size!(AerogpuFencePage, "aerogpu_fence_page");

    assert_off!(AerogpuSubmitDesc, cmd_gpa, "aerogpu_submit_desc", "cmd_gpa");
    assert_off!(AerogpuSubmitDesc, alloc_table_gpa, "aerogpu_submit_desc", "alloc_table_gpa");
    assert_off!(AerogpuSubmitDesc, signal_fence, "aerogpu_submit_desc", "signal_fence");
    assert_off!(AerogpuRingHeader, head, "aerogpu_ring_header", "head");
    assert_off!(AerogpuRingHeader, tail, "aerogpu_ring_header", "tail");
    assert_off!(AerogpuFencePage, completed_fence, "aerogpu_fence_page", "completed_fence");

    // Constants / enum numeric values.
    assert_eq!(abi.konst("AEROGPU_ABI_MAJOR"), AEROGPU_ABI_MAJOR as u64);
    assert_eq!(abi.konst("AEROGPU_ABI_MINOR"), AEROGPU_ABI_MINOR as u64);
    assert_eq!(abi.konst("AEROGPU_ABI_VERSION_U32"), AEROGPU_ABI_VERSION_U32 as u64);
    assert_eq!(abi.konst("AEROGPU_PCI_VENDOR_ID"), AEROGPU_PCI_VENDOR_ID as u64);
    assert_eq!(abi.konst("AEROGPU_PCI_DEVICE_ID"), AEROGPU_PCI_DEVICE_ID as u64);

    assert_eq!(abi.konst("AEROGPU_MMIO_MAGIC"), AEROGPU_MMIO_MAGIC as u64);
    assert_eq!(abi.konst("AEROGPU_MMIO_REG_DOORBELL"), AEROGPU_MMIO_REG_DOORBELL as u64);
    assert_eq!(abi.konst("AEROGPU_FEATURE_FENCE_PAGE"), AEROGPU_FEATURE_FENCE_PAGE as u64);
    assert_eq!(abi.konst("AEROGPU_RING_CONTROL_ENABLE"), AEROGPU_RING_CONTROL_ENABLE as u64);
    assert_eq!(abi.konst("AEROGPU_IRQ_FENCE"), AEROGPU_IRQ_FENCE as u64);

    assert_eq!(abi.konst("AEROGPU_CMD_STREAM_MAGIC"), AEROGPU_CMD_STREAM_MAGIC as u64);
    assert_eq!(abi.konst("AEROGPU_ALLOC_TABLE_MAGIC"), AEROGPU_ALLOC_TABLE_MAGIC as u64);
    assert_eq!(abi.konst("AEROGPU_RING_MAGIC"), AEROGPU_RING_MAGIC as u64);
    assert_eq!(abi.konst("AEROGPU_FENCE_PAGE_MAGIC"), AEROGPU_FENCE_PAGE_MAGIC as u64);

    assert_eq!(abi.konst("AEROGPU_CMD_NOP"), AerogpuCmdOpcode::Nop as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_DEBUG_MARKER"), AerogpuCmdOpcode::DebugMarker as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_CREATE_BUFFER"), AerogpuCmdOpcode::CreateBuffer as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_CREATE_TEXTURE2D"), AerogpuCmdOpcode::CreateTexture2d as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_DESTROY_RESOURCE"), AerogpuCmdOpcode::DestroyResource as u64);
    assert_eq!(
        abi.konst("AEROGPU_CMD_RESOURCE_DIRTY_RANGE"),
        AerogpuCmdOpcode::ResourceDirtyRange as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_CREATE_SHADER_DXBC"),
        AerogpuCmdOpcode::CreateShaderDxbc as u64
    );
    assert_eq!(abi.konst("AEROGPU_CMD_DESTROY_SHADER"), AerogpuCmdOpcode::DestroyShader as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_BIND_SHADERS"), AerogpuCmdOpcode::BindShaders as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_SET_BLEND_STATE"), AerogpuCmdOpcode::SetBlendState as u64);
    assert_eq!(
        abi.konst("AEROGPU_CMD_SET_DEPTH_STENCIL_STATE"),
        AerogpuCmdOpcode::SetDepthStencilState as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_SET_RASTERIZER_STATE"),
        AerogpuCmdOpcode::SetRasterizerState as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_SET_RENDER_TARGETS"),
        AerogpuCmdOpcode::SetRenderTargets as u64
    );
    assert_eq!(abi.konst("AEROGPU_CMD_SET_VIEWPORT"), AerogpuCmdOpcode::SetViewport as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_SET_SCISSOR"), AerogpuCmdOpcode::SetScissor as u64);
    assert_eq!(
        abi.konst("AEROGPU_CMD_SET_VERTEX_BUFFERS"),
        AerogpuCmdOpcode::SetVertexBuffers as u64
    );
    assert_eq!(abi.konst("AEROGPU_CMD_SET_INDEX_BUFFER"), AerogpuCmdOpcode::SetIndexBuffer as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_CLEAR"), AerogpuCmdOpcode::Clear as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_DRAW"), AerogpuCmdOpcode::Draw as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_DRAW_INDEXED"), AerogpuCmdOpcode::DrawIndexed as u64);
    assert_eq!(abi.konst("AEROGPU_CMD_PRESENT"), AerogpuCmdOpcode::Present as u64);

    assert_eq!(
        abi.konst("AEROGPU_FORMAT_B8G8R8A8_UNORM"),
        AerogpuFormat::B8G8R8A8Unorm as u64
    );
    assert_eq!(abi.konst("AEROGPU_FORMAT_D32_FLOAT"), AerogpuFormat::D32Float as u64);

    assert_eq!(abi.konst("AEROGPU_SUBMIT_FLAG_PRESENT"), AEROGPU_SUBMIT_FLAG_PRESENT as u64);
    assert_eq!(abi.konst("AEROGPU_SUBMIT_FLAG_NO_IRQ"), AEROGPU_SUBMIT_FLAG_NO_IRQ as u64);
}

#[test]
fn abi_version_rejects_unknown_major() {
    let version_u32 = ((AEROGPU_ABI_MAJOR + 1) << 16) | (AEROGPU_ABI_MINOR);
    let err = parse_and_validate_abi_version_u32(version_u32).unwrap_err();
    assert!(matches!(err, AerogpuAbiError::UnsupportedMajor { .. }));
}

#[test]
fn abi_version_accepts_unknown_minor() {
    let version_u32 = (AEROGPU_ABI_MAJOR << 16) | 999u32;
    let parsed = parse_and_validate_abi_version_u32(version_u32).expect("minor versions are backwards compatible");
    assert_eq!(parsed.major, AEROGPU_ABI_MAJOR as u16);
    assert_eq!(parsed.minor, 999);
}

#[test]
fn fence_page_write_updates_expected_bytes() {
    let mut page = [0u8; AerogpuFencePage::SIZE_BYTES];
    write_fence_page_completed_fence_le(&mut page, 0x0102_0304_0506_0708).unwrap();
    assert_eq!(&page[8..16], &0x0102_0304_0506_0708u64.to_le_bytes());
}

