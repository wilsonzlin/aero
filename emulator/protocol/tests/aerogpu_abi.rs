use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, AerogpuBlendFactor, AerogpuBlendOp, AerogpuBlendState,
    AerogpuCmdBindShaders, AerogpuCmdClear, AerogpuCmdCopyBuffer, AerogpuCmdCopyTexture2d,
    AerogpuCmdCreateBuffer, AerogpuCmdCreateInputLayout, AerogpuCmdCreateSampler,
    AerogpuCmdCreateShaderDxbc, AerogpuCmdCreateTexture2d, AerogpuCmdDestroyInputLayout,
    AerogpuCmdDestroyResource, AerogpuCmdDestroySampler, AerogpuCmdDestroyShader, AerogpuCmdDraw,
    AerogpuCmdDrawIndexed, AerogpuCmdExportSharedSurface, AerogpuCmdFlush, AerogpuCmdHdr,
    AerogpuCmdImportSharedSurface, AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdPresentEx,
    AerogpuCmdReleaseSharedSurface, AerogpuCmdResourceDirtyRange, AerogpuCmdSetBlendState,
    AerogpuCmdSetConstantBuffers, AerogpuCmdSetDepthStencilState, AerogpuCmdSetIndexBuffer,
    AerogpuCmdSetInputLayout, AerogpuCmdSetPrimitiveTopology, AerogpuCmdSetRasterizerState,
    AerogpuCmdSetRenderState, AerogpuCmdSetRenderTargets, AerogpuCmdSetSamplerState,
    AerogpuCmdSetSamplers, AerogpuCmdSetScissor, AerogpuCmdSetShaderConstantsF,
    AerogpuCmdSetTexture, AerogpuCmdSetVertexBuffers, AerogpuCmdSetViewport, AerogpuCmdStreamFlags,
    AerogpuCmdStreamHeader, AerogpuCmdUploadResource, AerogpuCompareFunc,
    AerogpuConstantBufferBinding, AerogpuCullMode, AerogpuDepthStencilState, AerogpuFillMode,
    AerogpuIndexFormat, AerogpuInputLayoutBlobHeader, AerogpuInputLayoutElementDxgi,
    AerogpuPrimitiveTopology, AerogpuRasterizerState, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_NONE, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION, AEROGPU_MAX_RENDER_TARGETS,
    AEROGPU_PRESENT_FLAG_NONE, AEROGPU_PRESENT_FLAG_VSYNC,
    AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_NONE, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_SCANOUT, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{
    parse_and_validate_abi_version_u32, AerogpuAbiError, AerogpuFormat, AEROGPU_ABI_MAJOR,
    AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32, AEROGPU_FEATURE_FENCE_PAGE,
    AEROGPU_FEATURE_TRANSFER, AEROGPU_FEATURE_VBLANK, AEROGPU_IRQ_FENCE, AEROGPU_MMIO_MAGIC,
    AEROGPU_MMIO_REG_DOORBELL, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS,
    AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
    AEROGPU_PCI_BAR0_INDEX, AEROGPU_PCI_BAR0_SIZE_BYTES, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER,
    AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE,
    AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, AEROGPU_PCI_VENDOR_ID,
    AEROGPU_RING_CONTROL_ENABLE,
};
use aero_protocol::aerogpu::aerogpu_ring::{
    write_fence_page_completed_fence_le, AerogpuAllocEntry, AerogpuAllocTableHeader,
    AerogpuFencePage, AerogpuRingDecodeError, AerogpuRingHeader, AerogpuSubmitDesc,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC,
    AEROGPU_SUBMIT_FLAG_NO_IRQ, AEROGPU_SUBMIT_FLAG_PRESENT,
};
use aero_protocol::aerogpu::aerogpu_umd_private::{
    AerogpuUmdPrivateV1, AEROGPU_UMDPRIV_FEATURE_CURSOR, AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE,
    AEROGPU_UMDPRIV_FEATURE_SCANOUT, AEROGPU_UMDPRIV_FEATURE_TRANSFER,
    AEROGPU_UMDPRIV_FEATURE_VBLANK, AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE,
    AEROGPU_UMDPRIV_FLAG_HAS_VBLANK, AEROGPU_UMDPRIV_FLAG_IS_LEGACY,
    AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP, AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
    AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI,
    AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO, AEROGPU_UMDPRIV_MMIO_REG_MAGIC,
    AEROGPU_UMDPRIV_STRUCT_VERSION_V1,
};
use aero_protocol::aerogpu::aerogpu_wddm_alloc::{
    AerogpuWddmAllocKind, AerogpuWddmAllocPriv, AerogpuWddmAllocPrivV2,
    AEROGPU_WDDM_ALLOC_ID_KMD_MIN, AEROGPU_WDDM_ALLOC_ID_UMD_MAX,
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER, AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT,
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH, AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE,
    AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED, AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE,
    AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING, AEROGPU_WDDM_ALLOC_PRIV_MAGIC,
    AEROGPU_WDDM_ALLOC_PRIV_VERSION, AEROGPU_WDDM_ALLOC_PRIV_VERSION_2,
};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

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

fn parse_c_define_const_names(header_path: &PathBuf) -> Vec<String> {
    let text = std::fs::read_to_string(header_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", header_path.display());
    });

    let mut names = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if !line.starts_with("#define") {
            continue;
        }
        let rest = line.trim_start_matches("#define").trim_start();
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };

        if !name.starts_with("AEROGPU_") {
            continue;
        }
        if name.starts_with("AEROGPU_PROTOCOL_") {
            continue;
        }
        // Function-like macros are not ABI surface area.
        if name.contains('(') {
            continue;
        }
        // Internal preprocessor helpers used only by the C headers.
        if name.starts_with("AEROGPU_CONCAT") || name == "AEROGPU_STATIC_ASSERT" {
            continue;
        }

        names.push(name.to_string());
    }

    names.sort();
    names.dedup();
    names
}

fn parse_c_enum_const_names(header_path: &PathBuf, enum_name: &str, prefix: &str) -> Vec<String> {
    let text = std::fs::read_to_string(header_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", header_path.display());
    });

    let enum_start = text
        .find(enum_name)
        .unwrap_or_else(|| panic!("missing {enum_name} in {}", header_path.display()));
    let after_start = &text[enum_start..];

    let open_brace = after_start
        .find('{')
        .unwrap_or_else(|| panic!("missing '{{' for {enum_name}"));
    let after_open = &after_start[open_brace + 1..];

    let close = after_open
        .find("};")
        .unwrap_or_else(|| panic!("missing '}};' for {enum_name}"));
    let body = &after_open[..close];

    let mut names = Vec::new();
    let mut idx = 0;
    while let Some(pos) = body[idx..].find(prefix) {
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

fn parse_c_struct_def_names(header_path: &PathBuf) -> Vec<String> {
    let text = std::fs::read_to_string(header_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", header_path.display());
    });

    let mut names = Vec::new();
    let mut idx = 0;
    while let Some(pos) = text[idx..].find("struct ") {
        let start = idx + pos + "struct ".len();
        let after = &text[start..];

        // Parse the identifier.
        let mut end = 0;
        for b in after.as_bytes() {
            if b.is_ascii_alphanumeric() || *b == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        if end == 0 {
            idx = start;
            continue;
        }

        let name = &after[..end];
        // Only consider ABI structs, not arbitrary "struct foo" usages in comments.
        if !name.starts_with("aerogpu_") {
            idx = start + end;
            continue;
        }

        // Skip forward over whitespace and check for a definition (`{`), not a field usage.
        let mut j = start + end;
        while j < text.len() && text.as_bytes()[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < text.len() && text.as_bytes()[j] == b'{' {
            names.push(name.to_string());
        }
        idx = j;
    }

    names.sort();
    names.dedup();
    names
}

fn assert_name_set_eq(mut seen: Vec<String>, mut expected: Vec<String>, what: &str) {
    seen.sort();
    seen.dedup();
    expected.sort();
    expected.dedup();

    if seen == expected {
        return;
    }

    let seen_set: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
    let expected_set: std::collections::BTreeSet<_> = expected.iter().cloned().collect();

    let missing: Vec<String> = expected_set.difference(&seen_set).cloned().collect();
    let extra: Vec<String> = seen_set.difference(&expected_set).cloned().collect();

    panic!("{what} coverage mismatch.\nmissing: {missing:#?}\nextra: {extra:#?}");
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

fn parse_c_cmd_struct_def_names() -> Vec<String> {
    let header_path = repo_root().join("drivers/aerogpu/protocol/aerogpu_cmd.h");
    let text = std::fs::read_to_string(&header_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", header_path.display());
    });

    let mut names = Vec::new();
    let mut idx = 0;
    while let Some(pos) = text[idx..].find("struct aerogpu_") {
        let start = idx + pos + "struct ".len();
        let mut end = start;
        while end < text.len() {
            let b = text.as_bytes()[end];
            if b.is_ascii_alphanumeric() || b == b'_' {
                end += 1;
            } else {
                break;
            }
        }

        let mut after = end;
        while after < text.len() && text.as_bytes()[after].is_ascii_whitespace() {
            after += 1;
        }

        // Only treat `struct name { ... }` as a definition. This excludes usages like:
        // `struct aerogpu_cmd_hdr hdr;`
        if after < text.len() && text.as_bytes()[after] == b'{' {
            names.push(text[start..end].to_string());
        }

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

fn write_u32_le(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn rust_layout_matches_c_headers() {
    let abi = abi_dump();
    let mut struct_sizes_seen: Vec<&'static str> = Vec::new();

    macro_rules! assert_size {
        ($ty:ty, $c_name:literal) => {
            assert_eq!(
                abi.size($c_name),
                core::mem::size_of::<$ty>(),
                "sizeof({})",
                $c_name
            );
            struct_sizes_seen.push($c_name);
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
    assert_off!(
        AerogpuCmdStreamHeader,
        reserved0,
        "aerogpu_cmd_stream_header",
        "reserved0"
    );
    assert_off!(
        AerogpuCmdStreamHeader,
        reserved1,
        "aerogpu_cmd_stream_header",
        "reserved1"
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
    assert_cmd_size!(AerogpuCmdCreateSampler, "aerogpu_cmd_create_sampler");
    assert_cmd_size!(AerogpuCmdDestroySampler, "aerogpu_cmd_destroy_sampler");
    assert_cmd_size!(AerogpuCmdSetSamplers, "aerogpu_cmd_set_samplers");
    assert_size!(
        AerogpuConstantBufferBinding,
        "aerogpu_constant_buffer_binding"
    );
    assert_cmd_size!(
        AerogpuCmdSetConstantBuffers,
        "aerogpu_cmd_set_constant_buffers"
    );
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
    assert_cmd_size!(
        AerogpuCmdReleaseSharedSurface,
        "aerogpu_cmd_release_shared_surface"
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

    let mut cmd_structs_seen: Vec<String> = cmd_structs_seen
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    cmd_structs_seen.sort();
    cmd_structs_seen.dedup();
    assert_eq!(
        cmd_structs_seen, expected_cmd_structs,
        "command packet struct coverage"
    );

    // Coverage guard: every `struct aerogpu_* { ... }` definition in `aerogpu_cmd.h` must have its
    // size validated against the C headers.
    let struct_sizes_seen: std::collections::BTreeSet<String> = struct_sizes_seen
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for c_name in parse_c_cmd_struct_def_names() {
        if !struct_sizes_seen.contains(&c_name) {
            missing.push(c_name);
        }
    }
    assert!(
        missing.is_empty(),
        "command stream struct size coverage: missing {missing:?}"
    );

    // Coverage guard: every `struct aerogpu_*` defined in `aerogpu_cmd.h` must have its size
    // checked against the C headers (not just the packet structs tied to opcodes).
    let expected_cmd_struct_defs =
        parse_c_struct_def_names(&repo_root().join("drivers/aerogpu/protocol/aerogpu_cmd.h"));
    let mut cmd_struct_defs_seen = vec![
        "aerogpu_cmd_stream_header".to_string(),
        "aerogpu_cmd_hdr".to_string(),
        "aerogpu_input_layout_blob_header".to_string(),
        "aerogpu_input_layout_element_dxgi".to_string(),
        "aerogpu_blend_state".to_string(),
        "aerogpu_depth_stencil_state".to_string(),
        "aerogpu_rasterizer_state".to_string(),
        "aerogpu_vertex_buffer_binding".to_string(),
        "aerogpu_constant_buffer_binding".to_string(),
    ];
    cmd_struct_defs_seen.extend(cmd_structs_seen.clone());
    assert_name_set_eq(
        cmd_struct_defs_seen,
        expected_cmd_struct_defs,
        "aerogpu_cmd.h struct definitions",
    );

    // Coverage guard: same for `aerogpu_ring.h`.
    let expected_ring_struct_defs =
        parse_c_struct_def_names(&repo_root().join("drivers/aerogpu/protocol/aerogpu_ring.h"));
    let ring_struct_defs_seen = vec![
        "aerogpu_alloc_table_header".to_string(),
        "aerogpu_alloc_entry".to_string(),
        "aerogpu_submit_desc".to_string(),
        "aerogpu_ring_header".to_string(),
        "aerogpu_fence_page".to_string(),
    ];
    assert_name_set_eq(
        ring_struct_defs_seen,
        expected_ring_struct_defs,
        "aerogpu_ring.h struct definitions",
    );

    // Ring structs.
    assert_size!(AerogpuAllocTableHeader, "aerogpu_alloc_table_header");
    assert_size!(AerogpuAllocEntry, "aerogpu_alloc_entry");
    assert_size!(AerogpuSubmitDesc, "aerogpu_submit_desc");
    assert_size!(AerogpuRingHeader, "aerogpu_ring_header");
    assert_size!(AerogpuFencePage, "aerogpu_fence_page");
    assert_size!(AerogpuUmdPrivateV1, "aerogpu_umd_private_v1");

    assert_off!(
        AerogpuAllocTableHeader,
        magic,
        "aerogpu_alloc_table_header",
        "magic"
    );
    assert_off!(
        AerogpuAllocTableHeader,
        abi_version,
        "aerogpu_alloc_table_header",
        "abi_version"
    );
    assert_off!(
        AerogpuAllocTableHeader,
        size_bytes,
        "aerogpu_alloc_table_header",
        "size_bytes"
    );
    assert_off!(
        AerogpuAllocTableHeader,
        entry_count,
        "aerogpu_alloc_table_header",
        "entry_count"
    );
    assert_off!(
        AerogpuAllocTableHeader,
        entry_stride_bytes,
        "aerogpu_alloc_table_header",
        "entry_stride_bytes"
    );
    assert_off!(
        AerogpuAllocTableHeader,
        reserved0,
        "aerogpu_alloc_table_header",
        "reserved0"
    );

    assert_off!(
        AerogpuAllocEntry,
        alloc_id,
        "aerogpu_alloc_entry",
        "alloc_id"
    );
    assert_off!(AerogpuAllocEntry, flags, "aerogpu_alloc_entry", "flags");
    assert_off!(AerogpuAllocEntry, gpa, "aerogpu_alloc_entry", "gpa");
    assert_off!(
        AerogpuAllocEntry,
        size_bytes,
        "aerogpu_alloc_entry",
        "size_bytes"
    );
    assert_off!(
        AerogpuAllocEntry,
        reserved0,
        "aerogpu_alloc_entry",
        "reserved0"
    );

    assert_off!(
        AerogpuSubmitDesc,
        desc_size_bytes,
        "aerogpu_submit_desc",
        "desc_size_bytes"
    );
    assert_off!(AerogpuSubmitDesc, flags, "aerogpu_submit_desc", "flags");
    assert_off!(
        AerogpuSubmitDesc,
        context_id,
        "aerogpu_submit_desc",
        "context_id"
    );
    assert_off!(
        AerogpuSubmitDesc,
        engine_id,
        "aerogpu_submit_desc",
        "engine_id"
    );
    assert_off!(AerogpuSubmitDesc, cmd_gpa, "aerogpu_submit_desc", "cmd_gpa");
    assert_off!(
        AerogpuSubmitDesc,
        cmd_size_bytes,
        "aerogpu_submit_desc",
        "cmd_size_bytes"
    );
    assert_off!(
        AerogpuSubmitDesc,
        cmd_reserved0,
        "aerogpu_submit_desc",
        "cmd_reserved0"
    );
    assert_off!(
        AerogpuSubmitDesc,
        alloc_table_gpa,
        "aerogpu_submit_desc",
        "alloc_table_gpa"
    );
    assert_off!(
        AerogpuSubmitDesc,
        alloc_table_size_bytes,
        "aerogpu_submit_desc",
        "alloc_table_size_bytes"
    );
    assert_off!(
        AerogpuSubmitDesc,
        alloc_table_reserved0,
        "aerogpu_submit_desc",
        "alloc_table_reserved0"
    );
    assert_off!(
        AerogpuSubmitDesc,
        signal_fence,
        "aerogpu_submit_desc",
        "signal_fence"
    );
    assert_off!(
        AerogpuSubmitDesc,
        reserved0,
        "aerogpu_submit_desc",
        "reserved0"
    );

    assert_off!(AerogpuRingHeader, magic, "aerogpu_ring_header", "magic");
    assert_off!(
        AerogpuRingHeader,
        abi_version,
        "aerogpu_ring_header",
        "abi_version"
    );
    assert_off!(
        AerogpuRingHeader,
        size_bytes,
        "aerogpu_ring_header",
        "size_bytes"
    );
    assert_off!(
        AerogpuRingHeader,
        entry_count,
        "aerogpu_ring_header",
        "entry_count"
    );
    assert_off!(
        AerogpuRingHeader,
        entry_stride_bytes,
        "aerogpu_ring_header",
        "entry_stride_bytes"
    );
    assert_off!(AerogpuRingHeader, flags, "aerogpu_ring_header", "flags");
    assert_off!(AerogpuRingHeader, head, "aerogpu_ring_header", "head");
    assert_off!(AerogpuRingHeader, tail, "aerogpu_ring_header", "tail");
    assert_off!(
        AerogpuRingHeader,
        reserved0,
        "aerogpu_ring_header",
        "reserved0"
    );
    assert_off!(
        AerogpuRingHeader,
        reserved1,
        "aerogpu_ring_header",
        "reserved1"
    );
    assert_off!(
        AerogpuRingHeader,
        reserved2,
        "aerogpu_ring_header",
        "reserved2"
    );

    assert_off!(AerogpuFencePage, magic, "aerogpu_fence_page", "magic");
    assert_off!(
        AerogpuFencePage,
        abi_version,
        "aerogpu_fence_page",
        "abi_version"
    );
    assert_off!(
        AerogpuFencePage,
        completed_fence,
        "aerogpu_fence_page",
        "completed_fence"
    );
    assert_off!(
        AerogpuFencePage,
        reserved0,
        "aerogpu_fence_page",
        "reserved0"
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

    // Fixed-layout packet field offsets (helps catch accidental field reordering even when the
    // overall struct size stays constant).
    let mut cmd_offset_structs_seen: Vec<&'static str> = Vec::new();
    macro_rules! assert_cmd_hdr_off {
        ($ty:ty, $c_ty:literal) => {{
            cmd_offset_structs_seen.push($c_ty);
            assert_off!($ty, hdr, $c_ty, "hdr");
        }};
    }

    assert_cmd_hdr_off!(AerogpuCmdCreateBuffer, "aerogpu_cmd_create_buffer");
    assert_off!(
        AerogpuCmdCreateBuffer,
        buffer_handle,
        "aerogpu_cmd_create_buffer",
        "buffer_handle"
    );
    assert_off!(
        AerogpuCmdCreateBuffer,
        usage_flags,
        "aerogpu_cmd_create_buffer",
        "usage_flags"
    );
    assert_off!(
        AerogpuCmdCreateBuffer,
        size_bytes,
        "aerogpu_cmd_create_buffer",
        "size_bytes"
    );
    assert_off!(
        AerogpuCmdCreateBuffer,
        backing_alloc_id,
        "aerogpu_cmd_create_buffer",
        "backing_alloc_id"
    );
    assert_off!(
        AerogpuCmdCreateBuffer,
        backing_offset_bytes,
        "aerogpu_cmd_create_buffer",
        "backing_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCreateBuffer,
        reserved0,
        "aerogpu_cmd_create_buffer",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdCreateTexture2d, "aerogpu_cmd_create_texture2d");
    assert_off!(
        AerogpuCmdCreateTexture2d,
        texture_handle,
        "aerogpu_cmd_create_texture2d",
        "texture_handle"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        usage_flags,
        "aerogpu_cmd_create_texture2d",
        "usage_flags"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        format,
        "aerogpu_cmd_create_texture2d",
        "format"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        width,
        "aerogpu_cmd_create_texture2d",
        "width"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        height,
        "aerogpu_cmd_create_texture2d",
        "height"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        mip_levels,
        "aerogpu_cmd_create_texture2d",
        "mip_levels"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        array_layers,
        "aerogpu_cmd_create_texture2d",
        "array_layers"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        row_pitch_bytes,
        "aerogpu_cmd_create_texture2d",
        "row_pitch_bytes"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        backing_alloc_id,
        "aerogpu_cmd_create_texture2d",
        "backing_alloc_id"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        backing_offset_bytes,
        "aerogpu_cmd_create_texture2d",
        "backing_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCreateTexture2d,
        reserved0,
        "aerogpu_cmd_create_texture2d",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdDestroyResource, "aerogpu_cmd_destroy_resource");
    assert_off!(
        AerogpuCmdDestroyResource,
        resource_handle,
        "aerogpu_cmd_destroy_resource",
        "resource_handle"
    );
    assert_off!(
        AerogpuCmdDestroyResource,
        reserved0,
        "aerogpu_cmd_destroy_resource",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdResourceDirtyRange,
        "aerogpu_cmd_resource_dirty_range"
    );
    assert_off!(
        AerogpuCmdResourceDirtyRange,
        resource_handle,
        "aerogpu_cmd_resource_dirty_range",
        "resource_handle"
    );
    assert_off!(
        AerogpuCmdResourceDirtyRange,
        reserved0,
        "aerogpu_cmd_resource_dirty_range",
        "reserved0"
    );
    assert_off!(
        AerogpuCmdResourceDirtyRange,
        offset_bytes,
        "aerogpu_cmd_resource_dirty_range",
        "offset_bytes"
    );
    assert_off!(
        AerogpuCmdResourceDirtyRange,
        size_bytes,
        "aerogpu_cmd_resource_dirty_range",
        "size_bytes"
    );

    assert_cmd_hdr_off!(AerogpuCmdUploadResource, "aerogpu_cmd_upload_resource");
    assert_off!(
        AerogpuCmdUploadResource,
        resource_handle,
        "aerogpu_cmd_upload_resource",
        "resource_handle"
    );
    assert_off!(
        AerogpuCmdUploadResource,
        reserved0,
        "aerogpu_cmd_upload_resource",
        "reserved0"
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

    assert_cmd_hdr_off!(AerogpuCmdCopyBuffer, "aerogpu_cmd_copy_buffer");
    assert_off!(
        AerogpuCmdCopyBuffer,
        dst_buffer,
        "aerogpu_cmd_copy_buffer",
        "dst_buffer"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        src_buffer,
        "aerogpu_cmd_copy_buffer",
        "src_buffer"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        dst_offset_bytes,
        "aerogpu_cmd_copy_buffer",
        "dst_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        src_offset_bytes,
        "aerogpu_cmd_copy_buffer",
        "src_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        size_bytes,
        "aerogpu_cmd_copy_buffer",
        "size_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        flags,
        "aerogpu_cmd_copy_buffer",
        "flags"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        reserved0,
        "aerogpu_cmd_copy_buffer",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdCopyTexture2d, "aerogpu_cmd_copy_texture2d");
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_texture,
        "aerogpu_cmd_copy_texture2d",
        "dst_texture"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_texture,
        "aerogpu_cmd_copy_texture2d",
        "src_texture"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_mip_level,
        "aerogpu_cmd_copy_texture2d",
        "dst_mip_level"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_array_layer,
        "aerogpu_cmd_copy_texture2d",
        "dst_array_layer"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_mip_level,
        "aerogpu_cmd_copy_texture2d",
        "src_mip_level"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_array_layer,
        "aerogpu_cmd_copy_texture2d",
        "src_array_layer"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_x,
        "aerogpu_cmd_copy_texture2d",
        "dst_x"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_y,
        "aerogpu_cmd_copy_texture2d",
        "dst_y"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_x,
        "aerogpu_cmd_copy_texture2d",
        "src_x"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_y,
        "aerogpu_cmd_copy_texture2d",
        "src_y"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        width,
        "aerogpu_cmd_copy_texture2d",
        "width"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        height,
        "aerogpu_cmd_copy_texture2d",
        "height"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        flags,
        "aerogpu_cmd_copy_texture2d",
        "flags"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        reserved0,
        "aerogpu_cmd_copy_texture2d",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdCreateShaderDxbc, "aerogpu_cmd_create_shader_dxbc");
    assert_off!(
        AerogpuCmdCreateShaderDxbc,
        shader_handle,
        "aerogpu_cmd_create_shader_dxbc",
        "shader_handle"
    );
    assert_off!(
        AerogpuCmdCreateShaderDxbc,
        stage,
        "aerogpu_cmd_create_shader_dxbc",
        "stage"
    );
    assert_off!(
        AerogpuCmdCreateShaderDxbc,
        dxbc_size_bytes,
        "aerogpu_cmd_create_shader_dxbc",
        "dxbc_size_bytes"
    );
    assert_off!(
        AerogpuCmdCreateShaderDxbc,
        reserved0,
        "aerogpu_cmd_create_shader_dxbc",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdDestroyShader, "aerogpu_cmd_destroy_shader");
    assert_off!(
        AerogpuCmdDestroyShader,
        shader_handle,
        "aerogpu_cmd_destroy_shader",
        "shader_handle"
    );
    assert_off!(
        AerogpuCmdDestroyShader,
        reserved0,
        "aerogpu_cmd_destroy_shader",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdBindShaders, "aerogpu_cmd_bind_shaders");
    assert_off!(AerogpuCmdBindShaders, vs, "aerogpu_cmd_bind_shaders", "vs");
    assert_off!(AerogpuCmdBindShaders, ps, "aerogpu_cmd_bind_shaders", "ps");
    assert_off!(AerogpuCmdBindShaders, cs, "aerogpu_cmd_bind_shaders", "cs");
    assert_off!(
        AerogpuCmdBindShaders,
        reserved0,
        "aerogpu_cmd_bind_shaders",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdSetShaderConstantsF,
        "aerogpu_cmd_set_shader_constants_f"
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
        AerogpuCmdSetShaderConstantsF,
        vec4_count,
        "aerogpu_cmd_set_shader_constants_f",
        "vec4_count"
    );
    assert_off!(
        AerogpuCmdSetShaderConstantsF,
        reserved0,
        "aerogpu_cmd_set_shader_constants_f",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdCreateInputLayout,
        "aerogpu_cmd_create_input_layout"
    );
    assert_off!(
        AerogpuCmdCreateInputLayout,
        input_layout_handle,
        "aerogpu_cmd_create_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdCreateInputLayout,
        blob_size_bytes,
        "aerogpu_cmd_create_input_layout",
        "blob_size_bytes"
    );
    assert_off!(
        AerogpuCmdCreateInputLayout,
        reserved0,
        "aerogpu_cmd_create_input_layout",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdDestroyInputLayout,
        "aerogpu_cmd_destroy_input_layout"
    );
    assert_off!(
        AerogpuCmdDestroyInputLayout,
        input_layout_handle,
        "aerogpu_cmd_destroy_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdDestroyInputLayout,
        reserved0,
        "aerogpu_cmd_destroy_input_layout",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetInputLayout, "aerogpu_cmd_set_input_layout");
    assert_off!(
        AerogpuCmdSetInputLayout,
        input_layout_handle,
        "aerogpu_cmd_set_input_layout",
        "input_layout_handle"
    );
    assert_off!(
        AerogpuCmdSetInputLayout,
        reserved0,
        "aerogpu_cmd_set_input_layout",
        "reserved0"
    );

    assert_off!(AerogpuBlendState, enable, "aerogpu_blend_state", "enable");
    assert_off!(
        AerogpuBlendState,
        src_factor,
        "aerogpu_blend_state",
        "src_factor"
    );
    assert_off!(
        AerogpuBlendState,
        dst_factor,
        "aerogpu_blend_state",
        "dst_factor"
    );
    assert_off!(
        AerogpuBlendState,
        blend_op,
        "aerogpu_blend_state",
        "blend_op"
    );
    assert_off!(
        AerogpuBlendState,
        color_write_mask,
        "aerogpu_blend_state",
        "color_write_mask"
    );
    assert_off!(
        AerogpuBlendState,
        reserved0,
        "aerogpu_blend_state",
        "reserved0"
    );
    assert_off!(
        AerogpuBlendState,
        src_factor_alpha,
        "aerogpu_blend_state",
        "src_factor_alpha"
    );
    assert_off!(
        AerogpuBlendState,
        dst_factor_alpha,
        "aerogpu_blend_state",
        "dst_factor_alpha"
    );
    assert_off!(
        AerogpuBlendState,
        blend_op_alpha,
        "aerogpu_blend_state",
        "blend_op_alpha"
    );
    assert_off!(
        AerogpuBlendState,
        blend_constant_rgba_f32,
        "aerogpu_blend_state",
        "blend_constant_rgba_f32"
    );
    assert_off!(
        AerogpuBlendState,
        sample_mask,
        "aerogpu_blend_state",
        "sample_mask"
    );
    assert_cmd_hdr_off!(AerogpuCmdSetBlendState, "aerogpu_cmd_set_blend_state");
    assert_off!(
        AerogpuCmdSetBlendState,
        state,
        "aerogpu_cmd_set_blend_state",
        "state"
    );

    assert_off!(
        AerogpuDepthStencilState,
        depth_enable,
        "aerogpu_depth_stencil_state",
        "depth_enable"
    );
    assert_off!(
        AerogpuDepthStencilState,
        depth_write_enable,
        "aerogpu_depth_stencil_state",
        "depth_write_enable"
    );
    assert_off!(
        AerogpuDepthStencilState,
        depth_func,
        "aerogpu_depth_stencil_state",
        "depth_func"
    );
    assert_off!(
        AerogpuDepthStencilState,
        stencil_enable,
        "aerogpu_depth_stencil_state",
        "stencil_enable"
    );
    assert_off!(
        AerogpuDepthStencilState,
        stencil_read_mask,
        "aerogpu_depth_stencil_state",
        "stencil_read_mask"
    );
    assert_off!(
        AerogpuDepthStencilState,
        stencil_write_mask,
        "aerogpu_depth_stencil_state",
        "stencil_write_mask"
    );
    assert_off!(
        AerogpuDepthStencilState,
        reserved0,
        "aerogpu_depth_stencil_state",
        "reserved0"
    );
    assert_cmd_hdr_off!(
        AerogpuCmdSetDepthStencilState,
        "aerogpu_cmd_set_depth_stencil_state"
    );
    assert_off!(
        AerogpuCmdSetDepthStencilState,
        state,
        "aerogpu_cmd_set_depth_stencil_state",
        "state"
    );

    assert_off!(
        AerogpuRasterizerState,
        fill_mode,
        "aerogpu_rasterizer_state",
        "fill_mode"
    );
    assert_off!(
        AerogpuRasterizerState,
        cull_mode,
        "aerogpu_rasterizer_state",
        "cull_mode"
    );
    assert_off!(
        AerogpuRasterizerState,
        front_ccw,
        "aerogpu_rasterizer_state",
        "front_ccw"
    );
    assert_off!(
        AerogpuRasterizerState,
        scissor_enable,
        "aerogpu_rasterizer_state",
        "scissor_enable"
    );
    assert_off!(
        AerogpuRasterizerState,
        depth_bias,
        "aerogpu_rasterizer_state",
        "depth_bias"
    );
    assert_off!(
        AerogpuRasterizerState,
        flags,
        "aerogpu_rasterizer_state",
        "flags"
    );
    assert_cmd_hdr_off!(
        AerogpuCmdSetRasterizerState,
        "aerogpu_cmd_set_rasterizer_state"
    );
    assert_off!(
        AerogpuCmdSetRasterizerState,
        state,
        "aerogpu_cmd_set_rasterizer_state",
        "state"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetRenderTargets, "aerogpu_cmd_set_render_targets");
    assert_off!(
        AerogpuCmdSetRenderTargets,
        color_count,
        "aerogpu_cmd_set_render_targets",
        "color_count"
    );
    assert_off!(
        AerogpuCmdSetRenderTargets,
        depth_stencil,
        "aerogpu_cmd_set_render_targets",
        "depth_stencil"
    );
    assert_off!(
        AerogpuCmdSetRenderTargets,
        colors,
        "aerogpu_cmd_set_render_targets",
        "colors"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetViewport, "aerogpu_cmd_set_viewport");
    assert_off!(
        AerogpuCmdSetViewport,
        x_f32,
        "aerogpu_cmd_set_viewport",
        "x_f32"
    );
    assert_off!(
        AerogpuCmdSetViewport,
        y_f32,
        "aerogpu_cmd_set_viewport",
        "y_f32"
    );
    assert_off!(
        AerogpuCmdSetViewport,
        width_f32,
        "aerogpu_cmd_set_viewport",
        "width_f32"
    );
    assert_off!(
        AerogpuCmdSetViewport,
        height_f32,
        "aerogpu_cmd_set_viewport",
        "height_f32"
    );
    assert_off!(
        AerogpuCmdSetViewport,
        min_depth_f32,
        "aerogpu_cmd_set_viewport",
        "min_depth_f32"
    );
    assert_off!(
        AerogpuCmdSetViewport,
        max_depth_f32,
        "aerogpu_cmd_set_viewport",
        "max_depth_f32"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetScissor, "aerogpu_cmd_set_scissor");
    assert_off!(AerogpuCmdSetScissor, x, "aerogpu_cmd_set_scissor", "x");
    assert_off!(AerogpuCmdSetScissor, y, "aerogpu_cmd_set_scissor", "y");
    assert_off!(
        AerogpuCmdSetScissor,
        width,
        "aerogpu_cmd_set_scissor",
        "width"
    );
    assert_off!(
        AerogpuCmdSetScissor,
        height,
        "aerogpu_cmd_set_scissor",
        "height"
    );

    assert_off!(
        AerogpuVertexBufferBinding,
        buffer,
        "aerogpu_vertex_buffer_binding",
        "buffer"
    );
    assert_off!(
        AerogpuVertexBufferBinding,
        stride_bytes,
        "aerogpu_vertex_buffer_binding",
        "stride_bytes"
    );
    assert_off!(
        AerogpuVertexBufferBinding,
        offset_bytes,
        "aerogpu_vertex_buffer_binding",
        "offset_bytes"
    );
    assert_off!(
        AerogpuVertexBufferBinding,
        reserved0,
        "aerogpu_vertex_buffer_binding",
        "reserved0"
    );
    assert_cmd_hdr_off!(AerogpuCmdSetVertexBuffers, "aerogpu_cmd_set_vertex_buffers");
    assert_off!(
        AerogpuCmdSetVertexBuffers,
        start_slot,
        "aerogpu_cmd_set_vertex_buffers",
        "start_slot"
    );
    assert_off!(
        AerogpuCmdSetVertexBuffers,
        buffer_count,
        "aerogpu_cmd_set_vertex_buffers",
        "buffer_count"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetIndexBuffer, "aerogpu_cmd_set_index_buffer");
    assert_off!(
        AerogpuCmdSetIndexBuffer,
        buffer,
        "aerogpu_cmd_set_index_buffer",
        "buffer"
    );
    assert_off!(
        AerogpuCmdSetIndexBuffer,
        format,
        "aerogpu_cmd_set_index_buffer",
        "format"
    );
    assert_off!(
        AerogpuCmdSetIndexBuffer,
        offset_bytes,
        "aerogpu_cmd_set_index_buffer",
        "offset_bytes"
    );
    assert_off!(
        AerogpuCmdSetIndexBuffer,
        reserved0,
        "aerogpu_cmd_set_index_buffer",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdSetPrimitiveTopology,
        "aerogpu_cmd_set_primitive_topology"
    );
    assert_off!(
        AerogpuCmdSetPrimitiveTopology,
        topology,
        "aerogpu_cmd_set_primitive_topology",
        "topology"
    );
    assert_off!(
        AerogpuCmdSetPrimitiveTopology,
        reserved0,
        "aerogpu_cmd_set_primitive_topology",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetTexture, "aerogpu_cmd_set_texture");
    assert_off!(
        AerogpuCmdSetTexture,
        shader_stage,
        "aerogpu_cmd_set_texture",
        "shader_stage"
    );
    assert_off!(
        AerogpuCmdSetTexture,
        slot,
        "aerogpu_cmd_set_texture",
        "slot"
    );
    assert_off!(
        AerogpuCmdSetTexture,
        texture,
        "aerogpu_cmd_set_texture",
        "texture"
    );
    assert_off!(
        AerogpuCmdSetTexture,
        reserved0,
        "aerogpu_cmd_set_texture",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetSamplerState, "aerogpu_cmd_set_sampler_state");
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

    assert_cmd_hdr_off!(AerogpuCmdCreateSampler, "aerogpu_cmd_create_sampler");
    assert_off!(
        AerogpuCmdCreateSampler,
        sampler_handle,
        "aerogpu_cmd_create_sampler",
        "sampler_handle"
    );
    assert_off!(
        AerogpuCmdCreateSampler,
        filter,
        "aerogpu_cmd_create_sampler",
        "filter"
    );
    assert_off!(
        AerogpuCmdCreateSampler,
        address_u,
        "aerogpu_cmd_create_sampler",
        "address_u"
    );
    assert_off!(
        AerogpuCmdCreateSampler,
        address_v,
        "aerogpu_cmd_create_sampler",
        "address_v"
    );
    assert_off!(
        AerogpuCmdCreateSampler,
        address_w,
        "aerogpu_cmd_create_sampler",
        "address_w"
    );

    assert_cmd_hdr_off!(AerogpuCmdDestroySampler, "aerogpu_cmd_destroy_sampler");
    assert_off!(
        AerogpuCmdDestroySampler,
        sampler_handle,
        "aerogpu_cmd_destroy_sampler",
        "sampler_handle"
    );
    assert_off!(
        AerogpuCmdDestroySampler,
        reserved0,
        "aerogpu_cmd_destroy_sampler",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetSamplers, "aerogpu_cmd_set_samplers");
    assert_off!(
        AerogpuCmdSetSamplers,
        shader_stage,
        "aerogpu_cmd_set_samplers",
        "shader_stage"
    );
    assert_off!(
        AerogpuCmdSetSamplers,
        start_slot,
        "aerogpu_cmd_set_samplers",
        "start_slot"
    );
    assert_off!(
        AerogpuCmdSetSamplers,
        sampler_count,
        "aerogpu_cmd_set_samplers",
        "sampler_count"
    );
    assert_off!(
        AerogpuCmdSetSamplers,
        reserved0,
        "aerogpu_cmd_set_samplers",
        "reserved0"
    );

    assert_off!(
        AerogpuConstantBufferBinding,
        buffer,
        "aerogpu_constant_buffer_binding",
        "buffer"
    );
    assert_off!(
        AerogpuConstantBufferBinding,
        offset_bytes,
        "aerogpu_constant_buffer_binding",
        "offset_bytes"
    );
    assert_off!(
        AerogpuConstantBufferBinding,
        size_bytes,
        "aerogpu_constant_buffer_binding",
        "size_bytes"
    );
    assert_off!(
        AerogpuConstantBufferBinding,
        reserved0,
        "aerogpu_constant_buffer_binding",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdSetConstantBuffers,
        "aerogpu_cmd_set_constant_buffers"
    );
    assert_off!(
        AerogpuCmdSetConstantBuffers,
        shader_stage,
        "aerogpu_cmd_set_constant_buffers",
        "shader_stage"
    );
    assert_off!(
        AerogpuCmdSetConstantBuffers,
        start_slot,
        "aerogpu_cmd_set_constant_buffers",
        "start_slot"
    );
    assert_off!(
        AerogpuCmdSetConstantBuffers,
        buffer_count,
        "aerogpu_cmd_set_constant_buffers",
        "buffer_count"
    );
    assert_off!(
        AerogpuCmdSetConstantBuffers,
        reserved0,
        "aerogpu_cmd_set_constant_buffers",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdSetRenderState, "aerogpu_cmd_set_render_state");
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
    assert_off!(
        AerogpuCmdCopyBuffer,
        dst_buffer,
        "aerogpu_cmd_copy_buffer",
        "dst_buffer"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        src_buffer,
        "aerogpu_cmd_copy_buffer",
        "src_buffer"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        dst_offset_bytes,
        "aerogpu_cmd_copy_buffer",
        "dst_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        src_offset_bytes,
        "aerogpu_cmd_copy_buffer",
        "src_offset_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        size_bytes,
        "aerogpu_cmd_copy_buffer",
        "size_bytes"
    );
    assert_off!(
        AerogpuCmdCopyBuffer,
        flags,
        "aerogpu_cmd_copy_buffer",
        "flags"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_texture,
        "aerogpu_cmd_copy_texture2d",
        "dst_texture"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_texture,
        "aerogpu_cmd_copy_texture2d",
        "src_texture"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_mip_level,
        "aerogpu_cmd_copy_texture2d",
        "dst_mip_level"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_array_layer,
        "aerogpu_cmd_copy_texture2d",
        "dst_array_layer"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_mip_level,
        "aerogpu_cmd_copy_texture2d",
        "src_mip_level"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_array_layer,
        "aerogpu_cmd_copy_texture2d",
        "src_array_layer"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_x,
        "aerogpu_cmd_copy_texture2d",
        "dst_x"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        dst_y,
        "aerogpu_cmd_copy_texture2d",
        "dst_y"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_x,
        "aerogpu_cmd_copy_texture2d",
        "src_x"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        src_y,
        "aerogpu_cmd_copy_texture2d",
        "src_y"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        width,
        "aerogpu_cmd_copy_texture2d",
        "width"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        height,
        "aerogpu_cmd_copy_texture2d",
        "height"
    );
    assert_off!(
        AerogpuCmdCopyTexture2d,
        flags,
        "aerogpu_cmd_copy_texture2d",
        "flags"
    );

    assert_cmd_hdr_off!(AerogpuCmdClear, "aerogpu_cmd_clear");
    assert_off!(AerogpuCmdClear, flags, "aerogpu_cmd_clear", "flags");
    assert_off!(
        AerogpuCmdClear,
        color_rgba_f32,
        "aerogpu_cmd_clear",
        "color_rgba_f32"
    );
    assert_off!(AerogpuCmdClear, depth_f32, "aerogpu_cmd_clear", "depth_f32");
    assert_off!(AerogpuCmdClear, stencil, "aerogpu_cmd_clear", "stencil");

    assert_cmd_hdr_off!(AerogpuCmdDraw, "aerogpu_cmd_draw");
    assert_off!(
        AerogpuCmdDraw,
        vertex_count,
        "aerogpu_cmd_draw",
        "vertex_count"
    );
    assert_off!(
        AerogpuCmdDraw,
        instance_count,
        "aerogpu_cmd_draw",
        "instance_count"
    );
    assert_off!(
        AerogpuCmdDraw,
        first_vertex,
        "aerogpu_cmd_draw",
        "first_vertex"
    );
    assert_off!(
        AerogpuCmdDraw,
        first_instance,
        "aerogpu_cmd_draw",
        "first_instance"
    );

    assert_cmd_hdr_off!(AerogpuCmdDrawIndexed, "aerogpu_cmd_draw_indexed");
    assert_off!(
        AerogpuCmdDrawIndexed,
        index_count,
        "aerogpu_cmd_draw_indexed",
        "index_count"
    );
    assert_off!(
        AerogpuCmdDrawIndexed,
        instance_count,
        "aerogpu_cmd_draw_indexed",
        "instance_count"
    );
    assert_off!(
        AerogpuCmdDrawIndexed,
        first_index,
        "aerogpu_cmd_draw_indexed",
        "first_index"
    );
    assert_off!(
        AerogpuCmdDrawIndexed,
        base_vertex,
        "aerogpu_cmd_draw_indexed",
        "base_vertex"
    );
    assert_off!(
        AerogpuCmdDrawIndexed,
        first_instance,
        "aerogpu_cmd_draw_indexed",
        "first_instance"
    );

    assert_cmd_hdr_off!(AerogpuCmdPresent, "aerogpu_cmd_present");
    assert_off!(
        AerogpuCmdPresent,
        scanout_id,
        "aerogpu_cmd_present",
        "scanout_id"
    );
    assert_off!(AerogpuCmdPresent, flags, "aerogpu_cmd_present", "flags");

    assert_cmd_hdr_off!(AerogpuCmdPresentEx, "aerogpu_cmd_present_ex");
    assert_off!(
        AerogpuCmdPresentEx,
        scanout_id,
        "aerogpu_cmd_present_ex",
        "scanout_id"
    );
    assert_off!(
        AerogpuCmdPresentEx,
        flags,
        "aerogpu_cmd_present_ex",
        "flags"
    );
    assert_off!(
        AerogpuCmdPresentEx,
        d3d9_present_flags,
        "aerogpu_cmd_present_ex",
        "d3d9_present_flags"
    );
    assert_off!(
        AerogpuCmdPresentEx,
        reserved0,
        "aerogpu_cmd_present_ex",
        "reserved0"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdExportSharedSurface,
        "aerogpu_cmd_export_shared_surface"
    );
    assert_off!(
        AerogpuCmdExportSharedSurface,
        resource_handle,
        "aerogpu_cmd_export_shared_surface",
        "resource_handle"
    );
    assert_off!(
        AerogpuCmdExportSharedSurface,
        reserved0,
        "aerogpu_cmd_export_shared_surface",
        "reserved0"
    );
    assert_off!(
        AerogpuCmdExportSharedSurface,
        share_token,
        "aerogpu_cmd_export_shared_surface",
        "share_token"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdImportSharedSurface,
        "aerogpu_cmd_import_shared_surface"
    );
    assert_off!(
        AerogpuCmdImportSharedSurface,
        out_resource_handle,
        "aerogpu_cmd_import_shared_surface",
        "out_resource_handle"
    );
    assert_off!(
        AerogpuCmdImportSharedSurface,
        reserved0,
        "aerogpu_cmd_import_shared_surface",
        "reserved0"
    );
    assert_off!(
        AerogpuCmdImportSharedSurface,
        share_token,
        "aerogpu_cmd_import_shared_surface",
        "share_token"
    );

    assert_cmd_hdr_off!(
        AerogpuCmdReleaseSharedSurface,
        "aerogpu_cmd_release_shared_surface"
    );
    assert_off!(
        AerogpuCmdReleaseSharedSurface,
        share_token,
        "aerogpu_cmd_release_shared_surface",
        "share_token"
    );
    assert_off!(
        AerogpuCmdReleaseSharedSurface,
        reserved0,
        "aerogpu_cmd_release_shared_surface",
        "reserved0"
    );

    assert_cmd_hdr_off!(AerogpuCmdFlush, "aerogpu_cmd_flush");
    assert_off!(AerogpuCmdFlush, reserved0, "aerogpu_cmd_flush", "reserved0");
    assert_off!(AerogpuCmdFlush, reserved1, "aerogpu_cmd_flush", "reserved1");

    let mut cmd_offset_structs_seen: Vec<String> = cmd_offset_structs_seen
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    cmd_offset_structs_seen.sort();
    cmd_offset_structs_seen.dedup();
    assert_eq!(
        cmd_offset_structs_seen, expected_cmd_structs,
        "command packet offset coverage"
    );

    // WDDM allocation private-data contract (stable across x86/x64).
    assert_size!(AerogpuWddmAllocPriv, "aerogpu_wddm_alloc_priv");
    assert_off!(
        AerogpuWddmAllocPriv,
        magic,
        "aerogpu_wddm_alloc_priv",
        "magic"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        version,
        "aerogpu_wddm_alloc_priv",
        "version"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        alloc_id,
        "aerogpu_wddm_alloc_priv",
        "alloc_id"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        flags,
        "aerogpu_wddm_alloc_priv",
        "flags"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        share_token,
        "aerogpu_wddm_alloc_priv",
        "share_token"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        size_bytes,
        "aerogpu_wddm_alloc_priv",
        "size_bytes"
    );
    assert_off!(
        AerogpuWddmAllocPriv,
        reserved0,
        "aerogpu_wddm_alloc_priv",
        "reserved0"
    );

    assert_size!(AerogpuWddmAllocPrivV2, "aerogpu_wddm_alloc_priv_v2");
    assert_off!(
        AerogpuWddmAllocPrivV2,
        magic,
        "aerogpu_wddm_alloc_priv_v2",
        "magic"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        version,
        "aerogpu_wddm_alloc_priv_v2",
        "version"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        alloc_id,
        "aerogpu_wddm_alloc_priv_v2",
        "alloc_id"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        flags,
        "aerogpu_wddm_alloc_priv_v2",
        "flags"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        share_token,
        "aerogpu_wddm_alloc_priv_v2",
        "share_token"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        size_bytes,
        "aerogpu_wddm_alloc_priv_v2",
        "size_bytes"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        reserved0,
        "aerogpu_wddm_alloc_priv_v2",
        "reserved0"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        kind,
        "aerogpu_wddm_alloc_priv_v2",
        "kind"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        width,
        "aerogpu_wddm_alloc_priv_v2",
        "width"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        height,
        "aerogpu_wddm_alloc_priv_v2",
        "height"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        format,
        "aerogpu_wddm_alloc_priv_v2",
        "format"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        row_pitch_bytes,
        "aerogpu_wddm_alloc_priv_v2",
        "row_pitch_bytes"
    );
    assert_off!(
        AerogpuWddmAllocPrivV2,
        reserved1,
        "aerogpu_wddm_alloc_priv_v2",
        "reserved1"
    );

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
    assert_eq!(abi.size("aerogpu_escape_map_shared_handle_inout"), 32);

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
    assert_eq!(
        abi.offset("aerogpu_escape_query_vblank_out", "vblank_interrupt_type"),
        52
    );
    assert_eq!(
        abi.offset("aerogpu_escape_map_shared_handle_inout", "shared_handle"),
        16
    );
    assert_eq!(
        abi.offset("aerogpu_escape_map_shared_handle_inout", "debug_token"),
        24
    );
    assert_eq!(
        abi.offset("aerogpu_escape_map_shared_handle_inout", "share_token"),
        24
    );
    assert_eq!(
        abi.offset("aerogpu_escape_map_shared_handle_inout", "reserved0"),
        28
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
        reserved0,
        "aerogpu_umd_private_v1",
        "reserved0"
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
    assert_off!(
        AerogpuUmdPrivateV1,
        reserved1,
        "aerogpu_umd_private_v1",
        "reserved1"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        reserved2,
        "aerogpu_umd_private_v1",
        "reserved2"
    );
    assert_off!(
        AerogpuUmdPrivateV1,
        reserved3,
        "aerogpu_umd_private_v1",
        "reserved3"
    );

    // Constants / enum numeric values.
    //
    // Coverage is header-driven: if a constant or enum member is added/removed in the C headers,
    // this test must fail until the Rust mirror is updated accordingly.
    let pci_header_path = repo_root().join("drivers/aerogpu/protocol/aerogpu_pci.h");
    let ring_header_path = repo_root().join("drivers/aerogpu/protocol/aerogpu_ring.h");
    let cmd_header_path = repo_root().join("drivers/aerogpu/protocol/aerogpu_cmd.h");

    let expected_pci_consts = {
        let mut names = parse_c_define_const_names(&pci_header_path);
        names.extend(parse_c_enum_const_names(
            &pci_header_path,
            "enum aerogpu_format",
            "AEROGPU_FORMAT_",
        ));
        names
    };

    let expected_ring_consts = {
        let mut names = parse_c_define_const_names(&ring_header_path);
        names.extend(parse_c_enum_const_names(
            &ring_header_path,
            "enum aerogpu_submit_flags",
            "AEROGPU_SUBMIT_FLAG_",
        ));
        names.extend(parse_c_enum_const_names(
            &ring_header_path,
            "enum aerogpu_engine_id",
            "AEROGPU_ENGINE_",
        ));
        names.extend(parse_c_enum_const_names(
            &ring_header_path,
            "enum aerogpu_alloc_flags",
            "AEROGPU_ALLOC_FLAG_",
        ));
        names
    };

    let expected_cmd_consts = {
        let mut names = parse_c_define_const_names(&cmd_header_path);
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_cmd_stream_flags",
            "AEROGPU_CMD_STREAM_FLAG_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_cmd_opcode",
            "AEROGPU_CMD_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_shader_stage",
            "AEROGPU_SHADER_STAGE_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_index_format",
            "AEROGPU_INDEX_FORMAT_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_primitive_topology",
            "AEROGPU_TOPOLOGY_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_resource_usage_flags",
            "AEROGPU_RESOURCE_USAGE_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_copy_flags",
            "AEROGPU_COPY_FLAG_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_blend_factor",
            "AEROGPU_BLEND_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_blend_op",
            "AEROGPU_BLEND_OP_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_compare_func",
            "AEROGPU_COMPARE_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_fill_mode",
            "AEROGPU_FILL_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_cull_mode",
            "AEROGPU_CULL_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_clear_flags",
            "AEROGPU_CLEAR_",
        ));
        names.extend(parse_c_enum_const_names(
            &cmd_header_path,
            "enum aerogpu_present_flags",
            "AEROGPU_PRESENT_FLAG_",
        ));
        names
    };

    let mut pci_consts_seen: Vec<String> = Vec::new();
    let mut ring_consts_seen: Vec<String> = Vec::new();
    let mut cmd_consts_seen: Vec<String> = Vec::new();

    let check_const = |seen: &mut Vec<String>, name: &'static str, value: u64| {
        seen.push(name.to_string());
        assert_eq!(abi.konst(name), value, "constant value for {name}");
    };

    // aerogpu_pci.h
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_ABI_MAJOR",
        AEROGPU_ABI_MAJOR as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_ABI_MINOR",
        AEROGPU_ABI_MINOR as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_ABI_VERSION_U32",
        AEROGPU_ABI_VERSION_U32 as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_VENDOR_ID",
        AEROGPU_PCI_VENDOR_ID as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_DEVICE_ID",
        AEROGPU_PCI_DEVICE_ID as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_SUBSYSTEM_VENDOR_ID",
        AEROGPU_PCI_SUBSYSTEM_VENDOR_ID as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_SUBSYSTEM_ID",
        AEROGPU_PCI_SUBSYSTEM_ID as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER",
        AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE",
        AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_PROG_IF",
        AEROGPU_PCI_PROG_IF as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_BAR0_INDEX",
        AEROGPU_PCI_BAR0_INDEX as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_PCI_BAR0_SIZE_BYTES",
        AEROGPU_PCI_BAR0_SIZE_BYTES as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_MAGIC",
        pci::AEROGPU_MMIO_REG_MAGIC as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_ABI_VERSION",
        pci::AEROGPU_MMIO_REG_ABI_VERSION as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_FEATURES_LO",
        pci::AEROGPU_MMIO_REG_FEATURES_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_FEATURES_HI",
        pci::AEROGPU_MMIO_REG_FEATURES_HI as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_MAGIC",
        AEROGPU_MMIO_MAGIC as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FEATURE_FENCE_PAGE",
        AEROGPU_FEATURE_FENCE_PAGE,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FEATURE_CURSOR",
        pci::AEROGPU_FEATURE_CURSOR,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FEATURE_SCANOUT",
        pci::AEROGPU_FEATURE_SCANOUT,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FEATURE_VBLANK",
        AEROGPU_FEATURE_VBLANK,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FEATURE_TRANSFER",
        AEROGPU_FEATURE_TRANSFER,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_RING_GPA_LO",
        pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_RING_GPA_HI",
        pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_RING_SIZE_BYTES",
        pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_RING_CONTROL",
        pci::AEROGPU_MMIO_REG_RING_CONTROL as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_RING_CONTROL_ENABLE",
        AEROGPU_RING_CONTROL_ENABLE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_RING_CONTROL_RESET",
        pci::AEROGPU_RING_CONTROL_RESET as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_FENCE_GPA_LO",
        pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_FENCE_GPA_HI",
        pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_COMPLETED_FENCE_LO",
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_COMPLETED_FENCE_HI",
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_DOORBELL",
        AEROGPU_MMIO_REG_DOORBELL as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_IRQ_STATUS",
        pci::AEROGPU_MMIO_REG_IRQ_STATUS as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_IRQ_ENABLE",
        pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_IRQ_ACK",
        pci::AEROGPU_MMIO_REG_IRQ_ACK as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_IRQ_FENCE",
        AEROGPU_IRQ_FENCE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_IRQ_SCANOUT_VBLANK",
        pci::AEROGPU_IRQ_SCANOUT_VBLANK as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_IRQ_ERROR",
        pci::AEROGPU_IRQ_ERROR as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_ENABLE",
        pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_WIDTH",
        pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_HEIGHT",
        pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_FORMAT",
        pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES",
        pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO",
        pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI",
        pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO",
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI",
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO",
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI",
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS",
        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_ENABLE",
        pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_X",
        pci::AEROGPU_MMIO_REG_CURSOR_X as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_Y",
        pci::AEROGPU_MMIO_REG_CURSOR_Y as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_HOT_X",
        pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_HOT_Y",
        pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_WIDTH",
        pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_HEIGHT",
        pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_FORMAT",
        pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO",
        pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI",
        pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES",
        pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64,
    );

    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_INVALID",
        AerogpuFormat::Invalid as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B8G8R8A8_UNORM",
        AerogpuFormat::B8G8R8A8Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B8G8R8X8_UNORM",
        AerogpuFormat::B8G8R8X8Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_R8G8B8A8_UNORM",
        AerogpuFormat::R8G8B8A8Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_R8G8B8X8_UNORM",
        AerogpuFormat::R8G8B8X8Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B5G6R5_UNORM",
        AerogpuFormat::B5G6R5Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B5G5R5A1_UNORM",
        AerogpuFormat::B5G5R5A1Unorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB",
        AerogpuFormat::B8G8R8A8UnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB",
        AerogpuFormat::B8G8R8X8UnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB",
        AerogpuFormat::R8G8B8A8UnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB",
        AerogpuFormat::R8G8B8X8UnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_D24_UNORM_S8_UINT",
        AerogpuFormat::D24UnormS8Uint as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_D32_FLOAT",
        AerogpuFormat::D32Float as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC1_RGBA_UNORM",
        AerogpuFormat::BC1RgbaUnorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB",
        AerogpuFormat::BC1RgbaUnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC2_RGBA_UNORM",
        AerogpuFormat::BC2RgbaUnorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB",
        AerogpuFormat::BC2RgbaUnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC3_RGBA_UNORM",
        AerogpuFormat::BC3RgbaUnorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB",
        AerogpuFormat::BC3RgbaUnormSrgb as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC7_RGBA_UNORM",
        AerogpuFormat::BC7RgbaUnorm as u64,
    );
    check_const(
        &mut pci_consts_seen,
        "AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB",
        AerogpuFormat::BC7RgbaUnormSrgb as u64,
    );

    assert_name_set_eq(
        pci_consts_seen,
        expected_pci_consts,
        "aerogpu_pci.h constants",
    );

    // aerogpu_ring.h
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_ALLOC_TABLE_MAGIC",
        AEROGPU_ALLOC_TABLE_MAGIC as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_RING_MAGIC",
        AEROGPU_RING_MAGIC as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_FENCE_PAGE_MAGIC",
        AEROGPU_FENCE_PAGE_MAGIC as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_SUBMIT_FLAG_NONE",
        ring::AEROGPU_SUBMIT_FLAG_NONE as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_SUBMIT_FLAG_PRESENT",
        AEROGPU_SUBMIT_FLAG_PRESENT as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_SUBMIT_FLAG_NO_IRQ",
        AEROGPU_SUBMIT_FLAG_NO_IRQ as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_ENGINE_0",
        ring::AEROGPU_ENGINE_0 as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_ALLOC_FLAG_NONE",
        ring::AEROGPU_ALLOC_FLAG_NONE as u64,
    );
    check_const(
        &mut ring_consts_seen,
        "AEROGPU_ALLOC_FLAG_READONLY",
        ring::AEROGPU_ALLOC_FLAG_READONLY as u64,
    );

    assert_name_set_eq(
        ring_consts_seen,
        expected_ring_consts,
        "aerogpu_ring.h constants",
    );

    // aerogpu_cmd.h
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CMD_STREAM_MAGIC",
        AEROGPU_CMD_STREAM_MAGIC as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_INPUT_LAYOUT_BLOB_MAGIC",
        AEROGPU_INPUT_LAYOUT_BLOB_MAGIC as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_INPUT_LAYOUT_BLOB_VERSION",
        AEROGPU_INPUT_LAYOUT_BLOB_VERSION as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_MAX_RENDER_TARGETS",
        AEROGPU_MAX_RENDER_TARGETS as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CMD_STREAM_FLAG_NONE",
        AerogpuCmdStreamFlags::None as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_NONE",
        AEROGPU_RESOURCE_USAGE_NONE as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER",
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_INDEX_BUFFER",
        AEROGPU_RESOURCE_USAGE_INDEX_BUFFER as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER",
        AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_TEXTURE",
        AEROGPU_RESOURCE_USAGE_TEXTURE as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_RENDER_TARGET",
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL",
        AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_RESOURCE_USAGE_SCANOUT",
        AEROGPU_RESOURCE_USAGE_SCANOUT as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COPY_FLAG_NONE",
        AEROGPU_COPY_FLAG_NONE as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COPY_FLAG_WRITEBACK_DST",
        AEROGPU_COPY_FLAG_WRITEBACK_DST as u64,
    );
    for c_name in parse_c_cmd_opcode_const_names() {
        cmd_consts_seen.push(c_name.clone());

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

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CLEAR_COLOR",
        AEROGPU_CLEAR_COLOR as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CLEAR_DEPTH",
        AEROGPU_CLEAR_DEPTH as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CLEAR_STENCIL",
        AEROGPU_CLEAR_STENCIL as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_PRESENT_FLAG_NONE",
        AEROGPU_PRESENT_FLAG_NONE as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_PRESENT_FLAG_VSYNC",
        AEROGPU_PRESENT_FLAG_VSYNC as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_ZERO",
        AerogpuBlendFactor::Zero as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_ONE",
        AerogpuBlendFactor::One as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_SRC_ALPHA",
        AerogpuBlendFactor::SrcAlpha as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_INV_SRC_ALPHA",
        AerogpuBlendFactor::InvSrcAlpha as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_DEST_ALPHA",
        AerogpuBlendFactor::DestAlpha as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_INV_DEST_ALPHA",
        AerogpuBlendFactor::InvDestAlpha as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_CONSTANT",
        AerogpuBlendFactor::Constant as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_INV_CONSTANT",
        AerogpuBlendFactor::InvConstant as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_OP_ADD",
        AerogpuBlendOp::Add as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_OP_SUBTRACT",
        AerogpuBlendOp::Subtract as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_OP_REV_SUBTRACT",
        AerogpuBlendOp::RevSubtract as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_OP_MIN",
        AerogpuBlendOp::Min as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_BLEND_OP_MAX",
        AerogpuBlendOp::Max as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_NEVER",
        AerogpuCompareFunc::Never as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_LESS",
        AerogpuCompareFunc::Less as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_EQUAL",
        AerogpuCompareFunc::Equal as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_LESS_EQUAL",
        AerogpuCompareFunc::LessEqual as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_GREATER",
        AerogpuCompareFunc::Greater as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_NOT_EQUAL",
        AerogpuCompareFunc::NotEqual as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_GREATER_EQUAL",
        AerogpuCompareFunc::GreaterEqual as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_COMPARE_ALWAYS",
        AerogpuCompareFunc::Always as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_FILL_SOLID",
        AerogpuFillMode::Solid as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_FILL_WIREFRAME",
        AerogpuFillMode::Wireframe as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CULL_NONE",
        AerogpuCullMode::None as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CULL_FRONT",
        AerogpuCullMode::Front as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_CULL_BACK",
        AerogpuCullMode::Back as u64,
    );
    assert_eq!(
        abi.konst("AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE"),
        AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE as u64
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_SHADER_STAGE_VERTEX",
        AerogpuShaderStage::Vertex as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_SHADER_STAGE_PIXEL",
        AerogpuShaderStage::Pixel as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_SHADER_STAGE_COMPUTE",
        AerogpuShaderStage::Compute as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_INDEX_FORMAT_UINT16",
        AerogpuIndexFormat::Uint16 as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_INDEX_FORMAT_UINT32",
        AerogpuIndexFormat::Uint32 as u64,
    );

    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_POINTLIST",
        AerogpuPrimitiveTopology::PointList as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_LINELIST",
        AerogpuPrimitiveTopology::LineList as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_LINESTRIP",
        AerogpuPrimitiveTopology::LineStrip as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_TRIANGLELIST",
        AerogpuPrimitiveTopology::TriangleList as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_TRIANGLESTRIP",
        AerogpuPrimitiveTopology::TriangleStrip as u64,
    );
    check_const(
        &mut cmd_consts_seen,
        "AEROGPU_TOPOLOGY_TRIANGLEFAN",
        AerogpuPrimitiveTopology::TriangleFan as u64,
    );

    assert_name_set_eq(
        cmd_consts_seen,
        expected_cmd_consts,
        "aerogpu_cmd.h constants",
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
        abi.konst("AEROGPU_UMDPRIV_MMIO_REG_MAGIC"),
        AEROGPU_UMDPRIV_MMIO_REG_MAGIC as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION"),
        AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO"),
        AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI"),
        AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE"),
        AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_CURSOR"),
        AEROGPU_UMDPRIV_FEATURE_CURSOR
    );
    assert_eq!(
        abi.konst("AEROGPU_UMDPRIV_FEATURE_SCANOUT"),
        AEROGPU_UMDPRIV_FEATURE_SCANOUT
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

    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_MAGIC"),
        AEROGPU_WDDM_ALLOC_PRIV_MAGIC as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION"),
        AEROGPU_WDDM_ALLOC_PRIV_VERSION as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_VERSION_2"),
        AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_ID_UMD_MAX"),
        AEROGPU_WDDM_ALLOC_ID_UMD_MAX as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_ID_KMD_MIN"),
        AEROGPU_WDDM_ALLOC_ID_KMD_MIN as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE"),
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED"),
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE"),
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING"),
        AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER"),
        AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH"),
        AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT"),
        AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_KIND_UNKNOWN"),
        AerogpuWddmAllocKind::Unknown as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_KIND_BUFFER"),
        AerogpuWddmAllocKind::Buffer as u64
    );
    assert_eq!(
        abi.konst("AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D"),
        AerogpuWddmAllocKind::Texture2d as u64
    );

    assert_eq!(abi.konst("AEROGPU_ESCAPE_VERSION"), 1);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE"), 1);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2"), 7);
    assert_eq!(abi.konst("AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE"), 8);

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
    assert_eq!(
        abi.konst("AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID"),
        2
    );
}

#[test]
fn cmd_hdr_rejects_bad_size_bytes() {
    let mut buf = [0u8; AerogpuCmdHdr::SIZE_BYTES];

    // Too small (must be >= sizeof(aerogpu_cmd_hdr)).
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuCmdHdr, size_bytes),
        4,
    );
    let err = decode_cmd_hdr_le(&buf)
        .err()
        .expect("expected decode error");
    assert!(matches!(
        err,
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDecodeError::BadSizeBytes { found: 4 }
    ));

    // Not 4-byte aligned.
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuCmdHdr, size_bytes),
        10,
    );
    let err = decode_cmd_hdr_le(&buf)
        .err()
        .expect("expected decode error");
    assert!(matches!(
        err,
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDecodeError::SizeNotAligned { found: 10 }
    ));

    // Unknown opcode is OK as long as the size is valid.
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuCmdHdr, opcode),
        0xFFFF_FFFF,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuCmdHdr, size_bytes),
        AerogpuCmdHdr::SIZE_BYTES as u32,
    );
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
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuSubmitDesc, desc_size_bytes),
        128,
    );

    let desc = AerogpuSubmitDesc::decode_from_le_bytes(&buf).unwrap();
    desc.validate_prefix().unwrap();
}

#[test]
fn submit_desc_size_rejects_too_small() {
    let mut buf = vec![0u8; AerogpuSubmitDesc::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuSubmitDesc, desc_size_bytes),
        32,
    );

    let desc = AerogpuSubmitDesc::decode_from_le_bytes(&buf).unwrap();
    let err = desc.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadSizeField { found: 32 }
    ));
}

#[test]
fn ring_header_accepts_unknown_minor_and_extended_stride() {
    let header_size_bytes = AerogpuRingHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, magic),
        AEROGPU_RING_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, abi_version),
        (AEROGPU_ABI_MAJOR << 16) | 999u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, size_bytes),
        header_size_bytes + 8u32 * 128u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_count),
        8,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_stride_bytes),
        128,
    );

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    hdr.validate_prefix().unwrap();
}

#[test]
fn ring_header_rejects_non_power_of_two_entry_count() {
    let header_size_bytes = AerogpuRingHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, magic),
        AEROGPU_RING_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, abi_version),
        AEROGPU_ABI_VERSION_U32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, size_bytes),
        header_size_bytes + 3u32 * 64u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_count),
        3,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_stride_bytes),
        64,
    );

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadEntryCount { found: 3 }
    ));
}

#[test]
fn ring_header_rejects_stride_too_small() {
    let header_size_bytes = AerogpuRingHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, magic),
        AEROGPU_RING_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, abi_version),
        AEROGPU_ABI_VERSION_U32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, size_bytes),
        header_size_bytes + 8u32 * 32u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_count),
        8,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_stride_bytes),
        32,
    );

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadStrideField { found: 32 }
    ));
}

#[test]
fn ring_header_rejects_size_too_small_for_layout() {
    let header_size_bytes = AerogpuRingHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuRingHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, magic),
        AEROGPU_RING_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, abi_version),
        AEROGPU_ABI_VERSION_U32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, size_bytes),
        header_size_bytes,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_count),
        8,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuRingHeader, entry_stride_bytes),
        64,
    );

    let hdr = AerogpuRingHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadSizeField { found: 64 }
    ));
}

#[test]
fn alloc_table_header_accepts_unknown_minor_and_extended_stride() {
    let header_size_bytes = AerogpuAllocTableHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, magic),
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, abi_version),
        (AEROGPU_ABI_MAJOR << 16) | 999u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, size_bytes),
        header_size_bytes + 2u32 * 64u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_count),
        2,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_stride_bytes),
        64,
    );

    let hdr = AerogpuAllocTableHeader::decode_from_le_bytes(&buf).unwrap();
    hdr.validate_prefix().unwrap();
}

#[test]
fn alloc_table_header_rejects_stride_too_small() {
    let header_size_bytes = AerogpuAllocTableHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, magic),
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, abi_version),
        AEROGPU_ABI_VERSION_U32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, size_bytes),
        header_size_bytes + 2u32 * 16u32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_count),
        2,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_stride_bytes),
        16,
    );

    let hdr = AerogpuAllocTableHeader::decode_from_le_bytes(&buf).unwrap();
    let err = hdr.validate_prefix().unwrap_err();
    assert!(matches!(
        err,
        AerogpuRingDecodeError::BadStrideField { found: 16 }
    ));
}

#[test]
fn alloc_table_header_rejects_size_too_small_for_layout() {
    let header_size_bytes = AerogpuAllocTableHeader::SIZE_BYTES as u32;
    let mut buf = vec![0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, magic),
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, abi_version),
        AEROGPU_ABI_VERSION_U32,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, size_bytes),
        header_size_bytes,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_count),
        2,
    );
    write_u32_le(
        &mut buf,
        core::mem::offset_of!(AerogpuAllocTableHeader, entry_stride_bytes),
        32,
    );

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
    let off = core::mem::offset_of!(AerogpuFencePage, completed_fence);
    assert_eq!(&page[off..off + 8], &0x0102_0304_0506_0708u64.to_le_bytes());
}
