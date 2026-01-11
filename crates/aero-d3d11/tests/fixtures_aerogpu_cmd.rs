use std::fs;

use aero_d3d11::input_layout::{
    fnv1a_32, map_layout_to_shader_locations, InputLayoutBinding, InputLayoutDesc,
    VsInputSignatureElement, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_d3d11::{parse_signatures, DxbcFile, ShaderStage, Sm4Program};
use aero_gpu::{parse_cmd_stream, AeroGpuCmd, AEROGPU_CMD_STREAM_MAGIC};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn parses_ilay_pos3_color_fixture_and_maps_to_vs_signature() {
    let ilay_bytes = load_fixture("ilay_pos3_color.bin");
    let layout = InputLayoutDesc::parse(&ilay_bytes).expect("ilay_pos3_color should parse");

    assert_eq!(layout.header.magic, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    assert_eq!(layout.header.version, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    assert_eq!(layout.header.element_count, 2);
    assert_eq!(layout.elements.len(), 2);

    let pos_hash = fnv1a_32(b"POSITION");
    let color_hash = fnv1a_32(b"COLOR");

    assert_eq!(layout.elements[0].semantic_name_hash, pos_hash);
    assert_eq!(layout.elements[0].semantic_index, 0);
    assert_eq!(layout.elements[0].dxgi_format, 6); // DXGI_FORMAT_R32G32B32_FLOAT
    assert_eq!(layout.elements[0].input_slot, 0);
    assert_eq!(layout.elements[0].aligned_byte_offset, 0);

    assert_eq!(layout.elements[1].semantic_name_hash, color_hash);
    assert_eq!(layout.elements[1].semantic_index, 0);
    assert_eq!(layout.elements[1].dxgi_format, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    assert_eq!(layout.elements[1].input_slot, 0);
    assert_eq!(layout.elements[1].aligned_byte_offset, 12);

    // Build a VS signature from the real DXBC fixture and validate that the ILAY blob maps to it.
    let vs_dxbc_bytes = load_fixture("vs_passthrough.dxbc");
    let dxbc = DxbcFile::parse(&vs_dxbc_bytes).expect("vs_passthrough should parse as DXBC");
    let signatures = parse_signatures(&dxbc).expect("signature parse should succeed");
    let isgn = signatures.isgn.expect("vs_passthrough should include ISGN");

    let mut vs_signature = Vec::new();
    for p in &isgn.parameters {
        vs_signature.push(VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
        });
    }

    let strides = [28u32]; // float3 position + float4 color
    let binding = InputLayoutBinding::new(&layout, &strides);
    let mapped =
        map_layout_to_shader_locations(&binding, &vs_signature).expect("ILAY mapping should work");

    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].array_stride, 28);
    assert_eq!(mapped[0].attributes.len(), 2);

    assert_eq!(mapped[0].attributes[0].shader_location, 0);
    assert_eq!(mapped[0].attributes[0].offset, 0);
    assert_eq!(
        mapped[0].attributes[0].format,
        wgpu::VertexFormat::Float32x3
    );

    assert_eq!(mapped[0].attributes[1].shader_location, 1);
    assert_eq!(mapped[0].attributes[1].offset, 12);
    assert_eq!(
        mapped[0].attributes[1].format,
        wgpu::VertexFormat::Float32x4
    );
}

#[test]
fn parses_ilay_pos3_tex2_fixture() {
    let ilay_bytes = load_fixture("ilay_pos3_tex2.bin");
    let layout = InputLayoutDesc::parse(&ilay_bytes).expect("ilay_pos3_tex2 should parse");

    assert_eq!(layout.header.magic, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    assert_eq!(layout.header.version, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    assert_eq!(layout.header.element_count, 2);
    assert_eq!(layout.elements.len(), 2);

    let pos_hash = fnv1a_32(b"POSITION");
    let uv_hash = fnv1a_32(b"TEXCOORD");

    assert_eq!(layout.elements[0].semantic_name_hash, pos_hash);
    assert_eq!(layout.elements[0].dxgi_format, 6); // R32G32B32_FLOAT
    assert_eq!(layout.elements[0].aligned_byte_offset, 0);

    assert_eq!(layout.elements[1].semantic_name_hash, uv_hash);
    assert_eq!(layout.elements[1].dxgi_format, 16); // R32G32_FLOAT
    assert_eq!(layout.elements[1].aligned_byte_offset, 12);

    // Validate that the TEXCOORD variant maps correctly with a synthetic VS signature.
    let signature = [
        VsInputSignatureElement {
            semantic_name_hash: pos_hash,
            semantic_index: 0,
            input_register: 0,
        },
        VsInputSignatureElement {
            semantic_name_hash: uv_hash,
            semantic_index: 0,
            input_register: 1,
        },
    ];
    let strides = [20u32]; // float3 position + float2 texcoord
    let binding = InputLayoutBinding::new(&layout, &strides);
    let mapped = map_layout_to_shader_locations(&binding, &signature)
        .expect("ILAY pos3+tex2 mapping should work");
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].array_stride, 20);
    assert_eq!(mapped[0].attributes.len(), 2);
    assert_eq!(mapped[0].attributes[0].shader_location, 0);
    assert_eq!(
        mapped[0].attributes[0].format,
        wgpu::VertexFormat::Float32x3
    );
    assert_eq!(mapped[0].attributes[1].shader_location, 1);
    assert_eq!(
        mapped[0].attributes[1].format,
        wgpu::VertexFormat::Float32x2
    );
}

#[test]
fn parses_aerogpu_cmd_triangle_sm4_fixture() {
    let stream_bytes = load_fixture("cmd_triangle_sm4.bin");
    let parsed = parse_cmd_stream(&stream_bytes).expect("cmd_triangle_sm4 should parse");

    // `AeroGpuCmdStreamHeader` is `repr(C, packed)` (ABI mirror), so copy out fields before
    // asserting to avoid taking references to packed fields.
    let magic = parsed.header.magic;
    let size_bytes = parsed.header.size_bytes;
    assert_eq!(magic, AEROGPU_CMD_STREAM_MAGIC);
    assert_eq!(size_bytes as usize, stream_bytes.len());

    // This fixture is intentionally tiny and stable; assert a fixed command count.
    assert_eq!(parsed.cmds.len(), 18);

    let vs_dxbc_bytes = load_fixture("vs_passthrough.dxbc");
    let ps_dxbc_bytes = load_fixture("ps_passthrough.dxbc");
    let ilay_bytes = load_fixture("ilay_pos3_color.bin");

    // Spot-check the key packets.
    assert!(matches!(
        &parsed.cmds[0],
        AeroGpuCmd::CreateBuffer {
            buffer_handle: 1,
            ..
        }
    ));
    assert!(matches!(
        &parsed.cmds[1],
        AeroGpuCmd::UploadResource {
            resource_handle: 1,
            offset_bytes: 0,
            ..
        }
    ));
    assert!(matches!(
        &parsed.cmds[2],
        AeroGpuCmd::CreateBuffer {
            buffer_handle: 2,
            ..
        }
    ));
    assert!(matches!(
        &parsed.cmds[3],
        AeroGpuCmd::UploadResource {
            resource_handle: 2,
            offset_bytes: 0,
            ..
        }
    ));

    match &parsed.cmds[5] {
        AeroGpuCmd::CreateShaderDxbc {
            shader_handle,
            stage,
            dxbc_bytes,
            ..
        } => {
            assert_eq!(*shader_handle, 10);
            assert_eq!(*stage, 0);
            assert_eq!(*dxbc_bytes, vs_dxbc_bytes.as_slice());
            let prog = Sm4Program::parse_from_dxbc_bytes(dxbc_bytes).unwrap();
            assert_eq!(prog.stage, ShaderStage::Vertex);
        }
        other => panic!("unexpected cmd[5]: {other:?}"),
    }

    match &parsed.cmds[6] {
        AeroGpuCmd::CreateShaderDxbc {
            shader_handle,
            stage,
            dxbc_bytes,
            ..
        } => {
            assert_eq!(*shader_handle, 11);
            assert_eq!(*stage, 1);
            assert_eq!(*dxbc_bytes, ps_dxbc_bytes.as_slice());
            let prog = Sm4Program::parse_from_dxbc_bytes(dxbc_bytes).unwrap();
            assert_eq!(prog.stage, ShaderStage::Pixel);
        }
        other => panic!("unexpected cmd[6]: {other:?}"),
    }

    match &parsed.cmds[7] {
        AeroGpuCmd::CreateInputLayout {
            input_layout_handle,
            blob_bytes,
            ..
        } => {
            assert_eq!(*input_layout_handle, 20);
            assert_eq!(*blob_bytes, ilay_bytes.as_slice());
        }
        other => panic!("unexpected cmd[7]: {other:?}"),
    }
}
