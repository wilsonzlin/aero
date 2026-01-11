use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, AerogpuBlendFactor, AerogpuBlendOp, AerogpuBlendState, AerogpuCmdBindShaders,
    AerogpuCmdClear, AerogpuCmdCopyBuffer, AerogpuCmdCopyTexture2d, AerogpuCmdCreateBuffer,
    AerogpuCmdCreateInputLayout, AerogpuCmdCreateShaderDxbc, AerogpuCmdCreateTexture2d,
    AerogpuCmdDestroyInputLayout, AerogpuCmdDestroyResource, AerogpuCmdDestroyShader, AerogpuCmdDraw,
    AerogpuCmdDrawIndexed, AerogpuCmdExportSharedSurface, AerogpuCmdFlush, AerogpuCmdHdr,
    AerogpuCmdImportSharedSurface, AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdPresentEx,
    AerogpuCmdResourceDirtyRange, AerogpuCmdSetBlendState, AerogpuCmdSetDepthStencilState,
    AerogpuCmdSetIndexBuffer, AerogpuCmdSetInputLayout, AerogpuCmdSetPrimitiveTopology,
    AerogpuCmdSetRasterizerState, AerogpuCmdSetRenderState, AerogpuCmdSetRenderTargets,
    AerogpuCmdSetSamplerState, AerogpuCmdSetScissor, AerogpuCmdSetShaderConstantsF,
    AerogpuCmdSetTexture, AerogpuCmdSetVertexBuffers, AerogpuCmdSetViewport, AerogpuCmdStreamFlags,
    AerogpuCmdStreamHeader, AerogpuCmdUploadResource, AerogpuCompareFunc, AerogpuCullMode,
    AerogpuDepthStencilState, AerogpuFillMode, AerogpuIndexFormat, AerogpuInputLayoutBlobHeader,
    AerogpuInputLayoutElementDxgi, AerogpuPrimitiveTopology, AerogpuRasterizerState, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_NONE, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION, AEROGPU_MAX_RENDER_TARGETS,
    AEROGPU_PRESENT_FLAG_NONE, AEROGPU_PRESENT_FLAG_VSYNC, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_NONE, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{
    parse_and_validate_abi_version_u32, AerogpuAbiError, AerogpuFormat, AEROGPU_ABI_MAJOR,
    AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32, AEROGPU_FEATURE_FENCE_PAGE, AEROGPU_FEATURE_TRANSFER,
    AEROGPU_FEATURE_VBLANK, AEROGPU_IRQ_FENCE, AEROGPU_MMIO_MAGIC, AEROGPU_MMIO_REG_DOORBELL,
    AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
    AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
    AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
    AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, AEROGPU_PCI_VENDOR_ID,
    AEROGPU_PCI_BAR0_INDEX, AEROGPU_PCI_BAR0_SIZE_BYTES, AEROGPU_RING_CONTROL_ENABLE,
};
use aero_protocol::aerogpu::aerogpu_ring::{
    write_fence_page_completed_fence_le, AerogpuAllocEntry, AerogpuAllocTableHeader,
    AerogpuFencePage, AerogpuRingDecodeError, AerogpuRingHeader, AerogpuSubmitDesc,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC,
    AEROGPU_SUBMIT_FLAG_NO_IRQ, AEROGPU_SUBMIT_FLAG_PRESENT,
};
use aero_protocol::aerogpu::aerogpu_umd_private::{
    AerogpuUmdPrivateV1, AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE, AEROGPU_UMDPRIV_FEATURE_TRANSFER,
    AEROGPU_UMDPRIV_FEATURE_VBLANK, AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE,
    AEROGPU_UMDPRIV_FLAG_HAS_VBLANK, AEROGPU_UMDPRIV_FLAG_IS_LEGACY,
    AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP, AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
    AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
};

#[derive(Debug, Default)]
struct AbiDump {
    sizes: HashMap<String, usize>,
    offsets: HashMap<String, usize>,
    consts: HashMap<String, u64>,
}

impl AbiDump {
    fn size(&self, ty: &str) -> usize {
        *self
            .sizes
            .get(ty)
            .unwrap_or_else(|| panic!("missing SIZE for {ty}"))
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

    let mut out_path =
        std::env::temp_dir().join(format!("aerogpu_abi_dump_{}", std::process::id()));
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

    parse_c_abi_dump_output(
        String::from_utf8(output.stdout).expect("C ABI dump output was not UTF-8"),
    )
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
                dump.sizes
                    .insert(parts[1].to_string(), parts[2].parse().unwrap());
            }
            "OFF" => {
                assert_eq!(parts.len(), 4, "bad OFF line @{}: {line}", line_no + 1);
                dump.offsets.insert(
                    format!("{}.{}", parts[1], parts[2]),
                    parts[3].parse().unwrap(),
                );
            }
            "CONST" => {
                assert_eq!(parts.len(), 3, "bad CONST line @{}: {line}", line_no + 1);
                dump.consts
                    .insert(parts[1].to_string(), parts[2].parse().unwrap());
            }
            other => panic!(
                "unknown ABI dump tag {other} in line @{}: {line}",
                line_no + 1
            ),
        }
    }

    dump
}

fn abi_dump() -> &'static AbiDump {
    static ABI: OnceLock<AbiDump> = OnceLock::new();
    ABI.get_or_init(compile_and_run_c_abi_dump)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_c_cmd_opcode_const_names() -> Vec<String> {
    let header_path = repo_root().join("drivers/aerogpu/protocol/aerogpu_cmd.h");
    let text = std::fs::read_to_string(&header_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", header_path.display());
    });

    let enum_start = text
        .find("enum aerogpu_cmd_opcode")
        .expect("missing enum aerogpu_cmd_opcode in aerogpu_cmd.h");
    let after_start = &text[enum_start..];

    let open_brace = after_start
        .find('{')
        .expect("missing '{' for enum aerogpu_cmd_opcode");
    let after_open = &after_start[open_brace + 1..];

    let close = after_open
        .find("};")
        .expect("missing '};' for enum aerogpu_cmd_opcode");
    let body = &after_open[..close];

    let mut names = Vec::new();
    let mut idx = 0;
    while let Some(pos) = body[idx..].find("AEROGPU_CMD_") {
        let start = idx + pos;
        let mut end = start;
        while end < body.len() {
            let b = body.as_bytes()[end];
            if b.is_ascii_alphanumeric() || b == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        names.push(body[start..end].to_string());
        idx = end;
    }

    names.sort();
    names.dedup();
    names
}

fn upper_snake_to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[test]
fn rust_layout_matches_c_headers() {
    let abi = abi_dump();

    macro_rules! assert_size {
        ($ty:ty, $c_name:literal) => {
            assert_eq!(
                abi.size($c_name),
                core::mem::size_of::<$ty>(),
                "sizeof({})",
                $c_name
            );
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
    assert_off!(
        AerogpuCmdStreamHeader,
        magic,
        "aerogpu_cmd_stream_header",
        "magic"
    );
    assert_off!(
        AerogpuCmdStreamHeader,
        abi_version,
        "aerogpu_cmd_stream_header",
        "abi_version"
    );
    assert_off!(
        AerogpuCmdStreamHeader,
        size_bytes,
        "aerogpu_cmd_stream_header",
        "size_bytes"
    );
    assert_off!(
        AerogpuCmdStreamHeader,
        flags,
        "aerogpu_cmd_stream_header",
        "flags"
    );
    assert_off!(AerogpuCmdHdr, opcode, "aerogpu_cmd_hdr", "opcode");
    assert_off!(AerogpuCmdHdr, size_bytes, "aerogpu_cmd_hdr", "size_bytes");

    assert_off!(
        AerogpuInputLayoutBlobHeader,
        magic,
        "aerogpu_input_layout_blob_header",
        "magic"
    );
    assert_off!(
        AerogpuInputLayoutBlobHeader,
        version,
        "aerogpu_input_layout_blob_header",
        "version"
    );
    assert_off!(
        AerogpuInputLayoutBlobHeader,
        element_count,
        "aerogpu_input_layout_blob_header",
        "element_count"
    );
    assert_off!(
        AerogpuInputLayoutBlobHeader,
        reserved0,
        "aerogpu_input_layout_blob_header",
        "reserved0"
    );

    assert_off!(
        AerogpuInputLayoutElementDxgi,
        semantic_name_hash,
        "aerogpu_input_layout_element_dxgi",
        "semantic_name_hash"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        semantic_index,
        "aerogpu_input_layout_element_dxgi",
        "semantic_index"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        dxgi_format,
        "aerogpu_input_layout_element_dxgi",
        "dxgi_format"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        input_slot,
        "aerogpu_input_layout_element_dxgi",
        "input_slot"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        aligned_byte_offset,
        "aerogpu_input_layout_element_dxgi",
        "aligned_byte_offset"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        input_slot_class,
        "aerogpu_input_layout_element_dxgi",
        "input_slot_class"
    );
    assert_off!(
        AerogpuInputLayoutElementDxgi,
        instance_data_step_rate,
        "aerogpu_input_layout_element_dxgi",
        "instance_data_step_rate"
    );

    // Command packet sizes.
    let mut cmd_structs_seen: Vec<&'static str> = Vec::new();
    macro_rules! assert_cmd_size {
        ($ty:ty, $c_name:literal) => {{
            assert_size!($ty, $c_name);
            cmd_structs_seen.push($c_name);
        }};
    }

    assert_cmd_size!(AerogpuCmdCreateBuffer, "aerogpu_cmd_create_buffer");
    assert_cmd_size!(AerogpuCmdCreateTexture2d, "aerogpu_cmd_create_texture2d");
    assert_cmd_size!(AerogpuCmdDestroyResource, "aerogpu_cmd_destroy_resource");
    assert_cmd_size!(
        AerogpuCmdResourceDirtyRange,
        "aerogpu_cmd_resource_dirty_range"
    );
    assert_cmd_size!(AerogpuCmdUploadResource, "aerogpu_cmd_upload_resource");
    assert_cmd_size!(AerogpuCmdCopyBuffer, "aerogpu_cmd_copy_buffer");
    assert_cmd_size!(AerogpuCmdCopyTexture2d, "aerogpu_cmd_copy_texture2d");
    assert_cmd_size!(AerogpuCmdCreateShaderDxbc, "aerogpu_cmd_create_shader_dxbc");
    assert_cmd_size!(AerogpuCmdDestroyShader, "aerogpu_cmd_destroy_shader");
    assert_cmd_size!(AerogpuCmdBindShaders, "aerogpu_cmd_bind_shaders");
    assert_cmd_size!(
        AerogpuCmdSetShaderConstantsF,
        "aerogpu_cmd_set_shader_constants_f"
    );
    assert_size!(
        AerogpuInputLayoutBlobHeader,
        "aerogpu_input_layout_blob_header"
    );
    assert_size!(
        AerogpuInputLayoutElementDxgi,
        "aerogpu_input_layout_element_dxgi"
    );
    assert_cmd_size!(
        AerogpuCmdCreateInputLayout,
        "aerogpu_cmd_create_input_layout"
    );
    assert_cmd_size!(
        AerogpuCmdDestroyInputLayout,
        "aerogpu_cmd_destroy_input_layout"
    );
    assert_cmd_size!(AerogpuCmdSetInputLayout, "aerogpu_cmd_set_input_layout");
    assert_size!(AerogpuBlendState, "aerogpu_blend_state");
    assert_cmd_size!(AerogpuCmdSetBlendState, "aerogpu_cmd_set_blend_state");
    assert_size!(AerogpuDepthStencilState, "aerogpu_depth_stencil_state");
    assert_cmd_size!(
        AerogpuCmdSetDepthStencilState,
        "aerogpu_cmd_set_depth_stencil_state"
    );
    assert_size!(AerogpuRasterizerState, "aerogpu_rasterizer_state");
    assert_cmd_size!(
        AerogpuCmdSetRasterizerState,
        "aerogpu_cmd_set_rasterizer_state"
    );
    assert_cmd_size!(AerogpuCmdSetRenderTargets, "aerogpu_cmd_set_render_targets");
    assert_cmd_size!(AerogpuCmdSetViewport, "aerogpu_cmd_set_viewport");
    assert_cmd_size!(AerogpuCmdSetScissor, "aerogpu_cmd_set_scissor");
    assert_size!(AerogpuVertexBufferBinding, "aerogpu_vertex_buffer_binding");
    assert_cmd_size!(AerogpuCmdSetVertexBuffers, "aerogpu_cmd_set_vertex_buffers");
    assert_cmd_size!(AerogpuCmdSetIndexBuffer, "aerogpu_cmd_set_index_buffer");
    assert_cmd_size!(
        AerogpuCmdSetPrimitiveTopology,
        "aerogpu_cmd_set_primitive_topology"
    );
    assert_cmd_size!(AerogpuCmdSetTexture, "aerogpu_cmd_set_texture");
    assert_cmd_size!(AerogpuCmdSetSamplerState, "aerogpu_cmd_set_sampler_state");
    assert_cmd_size!(AerogpuCmdSetRenderState, "aerogpu_cmd_set_render_state");
    assert_cmd_size!(AerogpuCmdClear, "aerogpu_cmd_clear");
    assert_cmd_size!(AerogpuCmdDraw, "aerogpu_cmd_draw");
    assert_cmd_size!(AerogpuCmdDrawIndexed, "aerogpu_cmd_draw_indexed");
    assert_cmd_size!(AerogpuCmdPresent, "aerogpu_cmd_present");
    assert_cmd_size!(AerogpuCmdPresentEx, "aerogpu_cmd_present_ex");
    assert_cmd_size!(
        AerogpuCmdExportSharedSurface,
        "aerogpu_cmd_export_shared_surface"
    );
    assert_cmd_size!(
        AerogpuCmdImportSharedSurface,
        "aerogpu_cmd_import_shared_surface"
    );
    assert_cmd_size!(AerogpuCmdFlush, "aerogpu_cmd_flush");

    // Coverage guard: every opcode (except NOP/DEBUG_MARKER) must have a corresponding
    // `aerogpu_cmd_*` packet struct whose size is validated against the C headers.
    let mut expected_cmd_structs = Vec::new();
    for c_name in parse_c_cmd_opcode_const_names() {
        if c_name == "AEROGPU_CMD_NOP" || c_name == "AEROGPU_CMD_DEBUG_MARKER" {
            continue;
        }
        let suffix = c_name
            .strip_prefix("AEROGPU_CMD_")
            .expect("opcode constant missing AEROGPU_CMD_ prefix");
        expected_cmd_structs.push(format!("aerogpu_cmd_{}", suffix.to_ascii_lowercase()));
    }
    expected_cmd_structs.sort();
    expected_cmd_structs.dedup();

    let mut cmd_structs_seen: Vec<String> = cmd_structs_seen.iter().map(|name| (*name).to_string()).collect();
    cmd_structs_seen.sort();
    cmd_structs_seen.dedup();
    assert_eq!(cmd_structs_seen, expected_cmd_structs, "command packet struct coverage");

    // Ring structs.
    assert_size!(AerogpuAllocTableHeader, "aerogpu_alloc_table_header");
    assert_size!(AerogpuAllocEntry, "aerogpu_alloc_entry");
    assert_size!(AerogpuSubmitDesc, "aerogpu_submit_desc");
    assert_size!(AerogpuRingHeader, "aerogpu_ring_header");
    assert_size!(AerogpuFencePage, "aerogpu_fence_page");
    assert_size!(AerogpuUmdPrivateV1, "aerogpu_umd_private_v1");

    assert_off!(AerogpuSubmitDesc, cmd_gpa, "aerogpu_submit_desc", "cmd_gpa");
    assert_off!(
        AerogpuSubmitDesc,
        alloc_table_gpa,
        "aerogpu_submit_desc",
        "alloc_table_gpa"
    );
    assert_off!(
        AerogpuSubmitDesc,
        signal_fence,
        "aerogpu_submit_desc",
        "signal_fence"
    );
    assert_off!(AerogpuRingHeader, head, "aerogpu_ring_header", "head");
    assert_off!(AerogpuRingHeader, tail, "aerogpu_ring_header", "tail");
    assert_off!(
        AerogpuFencePage,
        completed_fence,
        "aerogpu_fence_page",
        "completed_fence"
    );

    // Variable-length packets (must remain stable for parsing).
    assert_off!(
        AerogpuCmdCreateShaderDxbc,
        dxbc_size_bytes,
        "aerogpu_cmd_create_shader_dxbc",
        "dxbc_size_bytes"
    );
    assert_off!(
        AerogpuCmdSetShaderConstantsF,
        vec4_count,
        "aerogpu_cmd_set_shader_constants_f",
        "vec4_count"
    );
    assert_off!(
        AerogpuCmdCreateInputLayout,
        blob_size_bytes,
        "aerogpu_cmd_create_input_layout",
        "blob_size_bytes"
    );
    assert_off!(
        AerogpuCmdSetVertexBuffers,
        buffer_count,
        "aerogpu_cmd_set_vertex_buffers",
        "buffer_count"
    );
    assert_off!(
        AerogpuCmdUploadResource,
        offset_bytes,
        "aerogpu_cmd_upload_resource",
        "offset_bytes"
    );
    assert_off!(
        AerogpuCmdUploadResource,
        size_bytes,
        "aerogpu_cmd_upload_resource",
        "size_bytes"
    );
    assert_off!(
        AerogpuInputLayoutBlobHeader,
        element_count,
        "aerogpu_input_layout_blob_header",
        "element_count"
    );

    // Fixed-layout packet fields (helps catch accidental field reordering).
    assert_off!(
        AerogpuCmdUploadResource,
        resource_handle,
        "aerogpu_cmd_upload_resource",
        "resource_handle"
    );
    assert_off!(
        AerogpuCmdSetShaderConstantsF,
        stage,
        "aerogpu_cmd_set_shader_constants_f",
        "stage"
    );
    assert_off!(
        AerogpuCmdSetShaderConstantsF,
        start_register,
        "aerogpu_cmd_set_shader_constants_f",
        "start_register"
    );
    assert_off!(
        AerogpuCmdCreateInputLayout,
        input_layout_handle,
        "aerogpu_cmd_create_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdDestroyInputLayout,
        input_layout_handle,
        "aerogpu_cmd_destroy_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdSetInputLayout,
        input_layout_handle,
        "aerogpu_cmd_set_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdSetPrimitiveTopology,
        topology,
        "aerogpu_cmd_set_primitive_topology",
        "topology"
    );
    assert_off!(
        AerogpuCmdSetTexture,
        shader_stage,
        "aerogpu_cmd_set_texture",
        "shader_stage"
    );
    assert_off!(AerogpuCmdSetTexture, slot, "aerogpu_cmd_set_texture", "slot");
    assert_off!(AerogpuCmdSetTexture, texture, "aerogpu_cmd_set_texture", "texture");
    assert_off!(
        AerogpuCmdSetSamplerState,
        shader_stage,
        "aerogpu_cmd_set_sampler_state",
        "shader_stage"
    );
    assert_off!(
        AerogpuCmdSetSamplerState,
        slot,
        "aerogpu_cmd_set_sampler_state",
        "slot"
    );
    assert_off!(
        AerogpuCmdSetSamplerState,
        state,
        "aerogpu_cmd_set_sampler_state",
        "state"
    );
    assert_off!(
        AerogpuCmdSetSamplerState,
        value,
        "aerogpu_cmd_set_sampler_state",
        "value"
    );
    assert_off!(
        AerogpuCmdSetRenderState,
        state,
        "aerogpu_cmd_set_render_state",
        "state"
    );
    assert_off!(
        AerogpuCmdSetRenderState,
        value,
        "aerogpu_cmd_set_render_state",
        "value"
    );

    // WDDM allocation private-data contract (stable across x86/x64).
    assert_eq!(abi.size("aerogpu_wddm_alloc_priv"), 40);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "magic"), 0);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "version"), 4);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "alloc_id"), 8);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "flags"), 12);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "share_token"), 16);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "size_bytes"), 24);
    assert_eq!(abi.offset("aerogpu_wddm_alloc_priv", "reserved0"), 32);

    // Escape ABI (driver-private; should remain stable across x86/x64).
    assert_eq!(abi.size("aerogpu_escape_header"), 16);
    assert_eq!(abi.size("aerogpu_escape_query_device_out"), 24);
    assert_eq!(abi.size("aerogpu_escape_query_device_v2_out"), 48);
    assert_eq!(abi.size("aerogpu_escape_query_fence_out"), 32);
    assert_eq!(
        abi.size("aerogpu_escape_dump_ring_inout"),
        40 + (32 * 24),
        "sizeof(aerogpu_escape_dump_ring_inout)"
    );
    assert_eq!(
        abi.size("aerogpu_escape_dump_ring_v2_inout"),
        52 + (32 * 40),
        "sizeof(aerogpu_escape_dump_ring_v2_inout)"
    );
    assert_eq!(abi.size("aerogpu_escape_selftest_inout"), 32);
    assert_eq!(abi.size("aerogpu_escape_query_vblank_out"), 56);

    assert_eq!(abi.offset("aerogpu_escape_header", "version"), 0);
    assert_eq!(abi.offset("aerogpu_escape_header", "op"), 4);
    assert_eq!(abi.offset("aerogpu_escape_header", "size"), 8);
    assert_eq!(abi.offset("aerogpu_escape_header", "reserved0"), 12);

    assert_eq!(
        abi.offset("aerogpu_escape_query_device_v2_out", "detected_mmio_magic"),
        16
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_device_v2_out", "abi_version_u32"),
        20
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_device_v2_out", "features_lo"),
        24
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_device_v2_out", "features_hi"),
        32
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_device_v2_out", "reserved0"),
        40
    );

    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "vidpn_source_id"),
        16
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "irq_enable"),
        20
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "irq_status"),
        24
    );
    assert_eq!(abi.offset("aerogpu_escape_query_vblank_out", "flags"), 28);
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "vblank_seq"),
        32
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "last_vblank_time_ns"),
        40
    );
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "vblank_period_ns"),
        48
    );

    // UMD-private discovery blob (UMDRIVERPRIVATE).
    assert_off!(
        AerogpuUmdPrivateV1,
        size_bytes,
        "aerogpu_umd_private_v1",
        "size_bytes"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        struct_version,
        "aerogpu_umd_private_v1",
        "struct_version"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        device_mmio_magic,
        "aerogpu_umd_private_v1",
        "device_mmio_magic"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        device_abi_version_u32,
        "aerogpu_umd_private_v1",
        "device_abi_version_u32"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        device_features,
        "aerogpu_umd_private_v1",
        "device_features"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        flags,
        "aerogpu_umd_private_v1",
        "flags"
    );

    // Constants / enum numeric values.
    assert_eq!(abi.konst("AEROGPU_ABI_MAJOR"), AEROGPU_ABI_MAJOR as u64);
    assert_eq!(abi.konst("AEROGPU_ABI_MINOR"), AEROGPU_ABI_MINOR as u64);
    assert_eq!(
        abi.konst("AEROGPU_ABI_VERSION_U32"),
        AEROGPU_ABI_VERSION_U32 as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_VENDOR_ID"),
        AEROGPU_PCI_VENDOR_ID as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_DEVICE_ID"),
        AEROGPU_PCI_DEVICE_ID as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_SUBSYSTEM_VENDOR_ID"),
        AEROGPU_PCI_SUBSYSTEM_VENDOR_ID as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_SUBSYSTEM_ID"),
        AEROGPU_PCI_SUBSYSTEM_ID as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER"),
        AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE"),
        AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE as u64
    );
    assert_eq!(abi.konst("AEROGPU_PCI_PROG_IF"), AEROGPU_PCI_PROG_IF as u64);
    assert_eq!(
        abi.konst("AEROGPU_PCI_BAR0_INDEX"),
        AEROGPU_PCI_BAR0_INDEX as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PCI_BAR0_SIZE_BYTES"),
        AEROGPU_PCI_BAR0_SIZE_BYTES as u64
    );

    assert_eq!(abi.konst("AEROGPU_MMIO_MAGIC"), AEROGPU_MMIO_MAGIC as u64);
    assert_eq!(
        abi.konst("AEROGPU_MMIO_REG_DOORBELL"),
        AEROGPU_MMIO_REG_DOORBELL as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_FEATURE_FENCE_PAGE"),
        AEROGPU_FEATURE_FENCE_PAGE
    );
    assert_eq!(abi.konst("AEROGPU_FEATURE_VBLANK"), AEROGPU_FEATURE_VBLANK);
    assert_eq!(
        abi.konst("AEROGPU_FEATURE_TRANSFER"),
        AEROGPU_FEATURE_TRANSFER
    );
    assert_eq!(
        abi.konst("AEROGPU_RING_CONTROL_ENABLE"),
        AEROGPU_RING_CONTROL_ENABLE as u64
    );
    assert_eq!(abi.konst("AEROGPU_IRQ_FENCE"), AEROGPU_IRQ_FENCE as u64);
    assert_eq!(
        abi.konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO"),
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO"),
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS"),
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_CMD_STREAM_MAGIC"),
        AEROGPU_CMD_STREAM_MAGIC as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_STREAM_FLAG_NONE"),
        AerogpuCmdStreamFlags::None as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_ALLOC_TABLE_MAGIC"),
        AEROGPU_ALLOC_TABLE_MAGIC as u64
    );
    assert_eq!(abi.konst("AEROGPU_RING_MAGIC"), AEROGPU_RING_MAGIC as u64);
    assert_eq!(
        abi.konst("AEROGPU_FENCE_PAGE_MAGIC"),
        AEROGPU_FENCE_PAGE_MAGIC as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_NONE"),
        AEROGPU_RESOURCE_USAGE_NONE as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_COPY_BUFFER"),
        AerogpuCmdOpcode::CopyBuffer as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_CMD_COPY_TEXTURE2D"),
        AerogpuCmdOpcode::CopyTexture2d as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER"),
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_INDEX_BUFFER"),
        AEROGPU_RESOURCE_USAGE_INDEX_BUFFER as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER"),
        AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_TEXTURE"),
        AEROGPU_RESOURCE_USAGE_TEXTURE as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_RENDER_TARGET"),
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL"),
        AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_RESOURCE_USAGE_SCANOUT"),
        AEROGPU_RESOURCE_USAGE_SCANOUT as u64
    );

    assert_eq!(abi.konst("AEROGPU_COPY_FLAG_NONE"), AEROGPU_COPY_FLAG_NONE as u64);
    assert_eq!(
        abi.konst("AEROGPU_COPY_FLAG_WRITEBACK_DST"),
        AEROGPU_COPY_FLAG_WRITEBACK_DST as u64
    );

    assert_eq!(abi.konst("AEROGPU_MAX_RENDER_TARGETS"), AEROGPU_MAX_RENDER_TARGETS as u64);
    for c_name in parse_c_cmd_opcode_const_names() {
        let expected_rust = upper_snake_to_pascal_case(
            c_name
                .strip_prefix("AEROGPU_CMD_")
                .expect("opcode constant missing AEROGPU_CMD_ prefix"),
        );
        let value_u32: u32 = abi
            .konst(&c_name)
            .try_into()
            .expect("opcode did not fit in u32");

        let opcode = AerogpuCmdOpcode::from_u32(value_u32)
            .unwrap_or_else(|| panic!("missing Rust opcode binding for {c_name} ({value_u32:#x})"));
        assert_eq!(
            format!("{opcode:?}"),
            expected_rust,
            "opcode name for {c_name}"
        );
        assert_eq!(opcode as u32, value_u32, "opcode value for {c_name}");
    }

    assert_eq!(abi.konst("AEROGPU_CLEAR_COLOR"), AEROGPU_CLEAR_COLOR as u64);
    assert_eq!(abi.konst("AEROGPU_CLEAR_DEPTH"), AEROGPU_CLEAR_DEPTH as u64);
    assert_eq!(
        abi.konst("AEROGPU_CLEAR_STENCIL"),
        AEROGPU_CLEAR_STENCIL as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_PRESENT_FLAG_NONE"),
        AEROGPU_PRESENT_FLAG_NONE as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_PRESENT_FLAG_VSYNC"),
        AEROGPU_PRESENT_FLAG_VSYNC as u64
    );

    assert_eq!(abi.konst("AEROGPU_BLEND_ZERO"), AerogpuBlendFactor::Zero as u64);
    assert_eq!(abi.konst("AEROGPU_BLEND_ONE"), AerogpuBlendFactor::One as u64);
    assert_eq!(abi.konst("AEROGPU_BLEND_SRC_ALPHA"), AerogpuBlendFactor::SrcAlpha as u64);
    assert_eq!(
        abi.konst("AEROGPU_BLEND_INV_SRC_ALPHA"),
        AerogpuBlendFactor::InvSrcAlpha as u64
    );
    assert_eq!(abi.konst("AEROGPU_BLEND_DEST_ALPHA"), AerogpuBlendFactor::DestAlpha as u64);
    assert_eq!(
        abi.konst("AEROGPU_BLEND_INV_DEST_ALPHA"),
        AerogpuBlendFactor::InvDestAlpha as u64
    );

    assert_eq!(abi.konst("AEROGPU_BLEND_OP_ADD"), AerogpuBlendOp::Add as u64);
    assert_eq!(abi.konst("AEROGPU_BLEND_OP_SUBTRACT"), AerogpuBlendOp::Subtract as u64);
    assert_eq!(
        abi.konst("AEROGPU_BLEND_OP_REV_SUBTRACT"),
        AerogpuBlendOp::RevSubtract as u64
    );
    assert_eq!(abi.konst("AEROGPU_BLEND_OP_MIN"), AerogpuBlendOp::Min as u64);
    assert_eq!(abi.konst("AEROGPU_BLEND_OP_MAX"), AerogpuBlendOp::Max as u64);

    assert_eq!(abi.konst("AEROGPU_COMPARE_NEVER"), AerogpuCompareFunc::Never as u64);
    assert_eq!(abi.konst("AEROGPU_COMPARE_LESS"), AerogpuCompareFunc::Less as u64);
    assert_eq!(abi.konst("AEROGPU_COMPARE_EQUAL"), AerogpuCompareFunc::Equal as u64);
    assert_eq!(
        abi.konst("AEROGPU_COMPARE_LESS_EQUAL"),
        AerogpuCompareFunc::LessEqual as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_COMPARE_GREATER"),
        AerogpuCompareFunc::Greater as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_COMPARE_NOT_EQUAL"),
        AerogpuCompareFunc::NotEqual as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_COMPARE_GREATER_EQUAL"),
        AerogpuCompareFunc::GreaterEqual as u64
    );
    assert_eq!(abi.konst("AEROGPU_COMPARE_ALWAYS"), AerogpuCompareFunc::Always as u64);

    assert_eq!(abi.konst("AEROGPU_FILL_SOLID"), AerogpuFillMode::Solid as u64);
    assert_eq!(abi.konst("AEROGPU_FILL_WIREFRAME"), AerogpuFillMode::Wireframe as u64);

    assert_eq!(abi.konst("AEROGPU_CULL_NONE"), AerogpuCullMode::None as u64);
    assert_eq!(abi.konst("AEROGPU_CULL_FRONT"), AerogpuCullMode::Front as u64);
    assert_eq!(abi.konst("AEROGPU_CULL_BACK"), AerogpuCullMode::Back as u64);

    assert_eq!(
        abi.konst("AEROGPU_INPUT_LAYOUT_BLOB_MAGIC"),
        AEROGPU_INPUT_LAYOUT_BLOB_MAGIC as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_INPUT_LAYOUT_BLOB_VERSION"),
        AEROGPU_INPUT_LAYOUT_BLOB_VERSION as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_SHADER_STAGE_VERTEX"),
        AerogpuShaderStage::Vertex as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_SHADER_STAGE_PIXEL"),
        AerogpuShaderStage::Pixel as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_SHADER_STAGE_COMPUTE"),
        AerogpuShaderStage::Compute as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_INDEX_FORMAT_UINT16"),
        AerogpuIndexFormat::Uint16 as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_INDEX_FORMAT_UINT32"),
        AerogpuIndexFormat::Uint32 as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_POINTLIST"),
        AerogpuPrimitiveTopology::PointList as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_LINELIST"),
        AerogpuPrimitiveTopology::LineList as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_LINESTRIP"),
        AerogpuPrimitiveTopology::LineStrip as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_TRIANGLELIST"),
        AerogpuPrimitiveTopology::TriangleList as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_TRIANGLESTRIP"),
        AerogpuPrimitiveTopology::TriangleStrip as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_TOPOLOGY_TRIANGLEFAN"),
        AerogpuPrimitiveTopology::TriangleFan as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_FORMAT_B8G8R8A8_UNORM"),
        AerogpuFormat::B8G8R8A8Unorm as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_FORMAT_D32_FLOAT"),
        AerogpuFormat::D32Float as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_SUBMIT_FLAG_PRESENT"),
        AEROGPU_SUBMIT_FLAG_PRESENT as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_SUBMIT_FLAG_NO_IRQ"),
        AEROGPU_SUBMIT_FLAG_NO_IRQ as u64
    );

    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_STRUCT_VERSION_V1"),
        AEROGPU_UMDPRIV_STRUCT_VERSION_V1 as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP"),
        AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU"),
        AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE"),
        AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_VBLANK"),
        AEROGPU_UMDPRIV_FEATURE_VBLANK
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_TRANSFER"),
        AEROGPU_UMDPRIV_FEATURE_TRANSFER
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FLAG_IS_LEGACY"),
        AEROGPU_UMDPRIV_FLAG_IS_LEGACY as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FLAG_HAS_VBLANK"),
        AEROGPU_UMDPRIV_FLAG_HAS_VBLANK as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE"),
        AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE as u64
    );

    assert_eq!(abi.konst("AEROGPU_WDDM_ALLOC_PRIV_MAGIC"), 0x414c_4c4f);
    assert_eq!(abi.konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION"), 1);
    assert_eq!(abi.konst("AEROGPU_WDDM_ALLOC_ID_UMD_MAX"), 0x7fff_ffff);
    assert_eq!(abi.konst("AEROGPU_WDDM_ALLOC_ID_KMD_MIN"), 0x8000_0000);
    assert_eq!(abi.konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED"), 1);

    assert_eq!(abi.konst("AEROGPU_ESCAPE_VERSION"), 1);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE"), 1);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2"), 7);

    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_FENCE"), 2);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_DUMP_RING"), 3);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_SELFTEST"), 4);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_VBLANK"), 5);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_DUMP_RING_V2"), 6);

    assert_eq!(abi.konst("AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN"), 0);
    assert_eq!(abi.konst("AEROGPU_DBGCTL_RING_FORMAT_LEGACY"), 1);
    assert_eq!(abi.konst("AEROGPU_DBGCTL_RING_FORMAT_AGPU"), 2);

    assert_eq!(
        abi.konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID"),
        1u64 << 31
    );
    assert_eq!(
        abi.konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED"),
        1
    );
}

#[test]
fn cmd_hdr_rejects_bad_size_bytes() {
    let mut buf = [0u8; AerogpuCmdHdr::SIZE_BYTES];

    // Too small (must be >= sizeof(aerogpu_cmd_hdr)).
    buf[4..8].copy_from_slice(&4u32.to_le_bytes());
    let err = decode_cmd_hdr_le(&buf)
        .err()
        .expect("expected decode error");
    assert!(matches!(
        err,
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDecodeError::BadSizeBytes { found: 4 }
    ));

    // Not 4-byte aligned.
    buf[4..8].copy_from_slice(&10u32.to_le_bytes());
    let err = decode_cmd_hdr_le(&buf)
        .err()
        .expect("expected decode error");
    assert!(matches!(
        err,
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDecodeError::SizeNotAligned { found: 10 }
    ));

    // Unknown opcode is OK as long as the size is valid.
    buf[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    buf[4..8].copy_from_slice(&(AerogpuCmdHdr::SIZE_BYTES as u32).to_le_bytes());
    let hdr = decode_cmd_hdr_le(&buf).expect("unknown opcodes should be decodable");
    let opcode = hdr.opcode;
    assert_eq!(opcode, 0xFFFF_FFFF);
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
    let parsed = parse_and_validate_abi_version_u32(version_u32)
        .expect("minor versions are backwards compatible");
    assert_eq!(parsed.major, AEROGPU_ABI_MAJOR as u16);
    assert_eq!(parsed.minor, 999);
}

#[test]
fn submit_desc_size_accepts_extensions() {
    let mut buf = vec![0u8; 128];
    buf[0..4].copy_from_slice(&(128u32).to_le_bytes());

    let desc = AerogpuSubmitDesc::decode_from_le_bytes(&buf).unwrap();
    desc.validate_prefix().unwrap();
}

#[test]
fn submit_desc_size_rejects_too_small() {
    let mut buf = vec![0u8; AerogpuSubmitDesc::SIZE_BYTES];
    buf[0..4].copy_from_slice(&(32u32).to_le_bytes());

    let desc = AerogpuSubmitDesc::decode_from_le_bytes(&buf).unwrap();
    let err = desc.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadSizeField { found: 32 }
    ));
}

#[test]
fn ring_header_accepts_unknown_minor_and_extended_stride() {
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_RING_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&((AEROGPU_ABI_MAJOR << 16) | 999u32).to_le_bytes());
    buf[8..12].copy_from_slice(&(64u32 + 8u32 * 128u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(8u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(128u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    hdr.validate_prefix().unwrap();
}

#[test]
fn ring_header_rejects_non_power_of_two_entry_count() {
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_RING_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    buf[8..12].copy_from_slice(&(64u32 + 3u32 * 64u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(3u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(64u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadEntryCount { found: 3 }
    ));
}

#[test]
fn ring_header_rejects_stride_too_small() {
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_RING_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    buf[8..12].copy_from_slice(&(64u32 + 8u32 * 32u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(8u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(32u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadStrideField { found: 32 }
    ));
}

#[test]
fn ring_header_rejects_size_too_small_for_layout() {
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_RING_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    buf[8..12].copy_from_slice(&(64u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(8u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(64u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadSizeField { found: 64 }
    ));
}

#[test]
fn alloc_table_header_accepts_unknown_minor_and_extended_stride() {
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&((AEROGPU_ABI_MAJOR << 16) | 999u32).to_le_bytes());
    buf[8..12].copy_from_slice(&(24u32 + 2u32 * 64u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(2u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(64u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuAllocTableHeader::decode_from_le_bytes(&buf).unwrap();
    hdr.validate_prefix().unwrap();
}

#[test]
fn alloc_table_header_rejects_stride_too_small() {
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    buf[8..12].copy_from_slice(&(24u32 + 2u32 * 16u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(2u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(16u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuAllocTableHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadStrideField { found: 16 }
    ));
}

#[test]
fn alloc_table_header_rejects_size_too_small_for_layout() {
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    buf[0..4].copy_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    buf[8..12].copy_from_slice(&(24u32).to_le_bytes()); // size_bytes
    buf[12..16].copy_from_slice(&(2u32).to_le_bytes()); // entry_count
    buf[16..20].copy_from_slice(&(32u32).to_le_bytes()); // entry_stride_bytes

    let hdr = AerogpuAllocTableHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadSizeField { found: 24 }
    ));
}

#[test]
fn fence_page_write_updates_expected_bytes() {
    let mut page = [0u8; AerogpuFencePage::SIZE_BYTES];
    write_fence_page_completed_fence_le(&mut page, 0x0102_0304_0506_0708).unwrap();
    assert_eq!(&page[8..16], &0x0102_0304_0506_0708u64.to_le_bytes());
}
