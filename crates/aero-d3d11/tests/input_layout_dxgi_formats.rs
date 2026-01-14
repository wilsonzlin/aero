use aero_d3d11::input_layout::{
    dxgi_format_info, fnv1a_32, map_layout_to_shader_locations_compact, DxgiFormatComponentType,
    InputLayoutBinding, InputLayoutDesc, InputLayoutError, VsInputSignatureElement,
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
    D3D11_APPEND_ALIGNED_ELEMENT,
};

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_ilay(elements: &[IlayElem]) -> Vec<u8> {
    let mut blob = Vec::new();
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut blob, elements.len() as u32);
    push_u32(&mut blob, 0); // reserved0

    for e in elements {
        push_u32(&mut blob, e.semantic_name_hash);
        push_u32(&mut blob, e.semantic_index);
        push_u32(&mut blob, e.dxgi_format);
        push_u32(&mut blob, e.input_slot);
        push_u32(&mut blob, e.aligned_byte_offset);
        push_u32(&mut blob, e.input_slot_class);
        push_u32(&mut blob, e.instance_data_step_rate);
    }

    blob
}

#[derive(Clone, Copy)]
struct IlayElem {
    semantic_name_hash: u32,
    semantic_index: u32,
    dxgi_format: u32,
    input_slot: u32,
    aligned_byte_offset: u32,
    input_slot_class: u32,
    instance_data_step_rate: u32,
}

#[test]
fn maps_new_dxgi_formats_to_wgpu_vertex_formats() {
    struct Case {
        name: &'static str,
        dxgi_format: u32,
        expect: wgpu::VertexFormat,
        stride: u32,
    }

    let tex_hash = fnv1a_32(b"TEXCOORD");
    let cases = [
        Case {
            name: "R32G32B32A32_UINT",
            dxgi_format: 3,
            expect: wgpu::VertexFormat::Uint32x4,
            stride: 16,
        },
        Case {
            name: "R32G32B32A32_SINT",
            dxgi_format: 4,
            expect: wgpu::VertexFormat::Sint32x4,
            stride: 16,
        },
        Case {
            name: "R16G16B16A16_UNORM",
            dxgi_format: 11,
            expect: wgpu::VertexFormat::Unorm16x4,
            stride: 8,
        },
        Case {
            name: "R32G32B32_UINT",
            dxgi_format: 7,
            expect: wgpu::VertexFormat::Uint32x3,
            stride: 12,
        },
        Case {
            name: "R32G32B32_SINT",
            dxgi_format: 8,
            expect: wgpu::VertexFormat::Sint32x3,
            stride: 12,
        },
        Case {
            name: "R32G32_UINT",
            dxgi_format: 17,
            expect: wgpu::VertexFormat::Uint32x2,
            stride: 8,
        },
        Case {
            name: "R32G32_SINT",
            dxgi_format: 18,
            expect: wgpu::VertexFormat::Sint32x2,
            stride: 8,
        },
        Case {
            name: "R16G16_UNORM",
            dxgi_format: 35,
            expect: wgpu::VertexFormat::Unorm16x2,
            stride: 4,
        },
        Case {
            name: "R16G16_SNORM",
            dxgi_format: 37,
            expect: wgpu::VertexFormat::Snorm16x2,
            stride: 4,
        },
        Case {
            name: "R10G10B10A2_UNORM",
            dxgi_format: 24,
            expect: wgpu::VertexFormat::Unorm10_10_10_2,
            stride: 4,
        },
        Case {
            name: "R8G8B8A8_UINT",
            dxgi_format: 30,
            expect: wgpu::VertexFormat::Uint8x4,
            stride: 4,
        },
        Case {
            name: "R8G8B8A8_SNORM",
            dxgi_format: 31,
            expect: wgpu::VertexFormat::Snorm8x4,
            stride: 4,
        },
        Case {
            name: "R8G8B8A8_SINT",
            dxgi_format: 32,
            expect: wgpu::VertexFormat::Sint8x4,
            stride: 4,
        },
        Case {
            name: "R8G8_UNORM",
            dxgi_format: 49,
            expect: wgpu::VertexFormat::Unorm8x2,
            stride: 4,
        },
        Case {
            name: "R8G8_UINT",
            dxgi_format: 50,
            expect: wgpu::VertexFormat::Uint8x2,
            stride: 4,
        },
        Case {
            name: "R8G8_SINT",
            dxgi_format: 52,
            expect: wgpu::VertexFormat::Sint8x2,
            stride: 4,
        },
    ];

    for case in cases {
        let blob = build_ilay(&[IlayElem {
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            dxgi_format: case.dxgi_format,
            input_slot: 0,
            aligned_byte_offset: 0,
            input_slot_class: 0,
            instance_data_step_rate: 0,
        }]);
        let layout = InputLayoutDesc::parse(&blob).expect("ilay parse");
        let strides = [case.stride];
        let binding = InputLayoutBinding::new(&layout, &strides);
        let signature = [VsInputSignatureElement {
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];
        let mapped = map_layout_to_shader_locations_compact(&binding, &signature)
            .unwrap_or_else(|e| panic!("mapping failed for {}: {e}", case.name));
        assert_eq!(mapped.buffers.len(), 1);
        assert_eq!(mapped.buffers[0].attributes.len(), 1);
        assert_eq!(mapped.buffers[0].attributes[0].format, case.expect);
    }
}

#[test]
fn dxgi_format_info_includes_component_metadata() {
    struct Case {
        dxgi_format: u32,
        wgpu_format: wgpu::VertexFormat,
        size_bytes: u32,
        align_bytes: u32,
        component_type: DxgiFormatComponentType,
        component_count: u32,
    }

    let cases = [
        Case {
            dxgi_format: 3, // R32G32B32A32_UINT
            wgpu_format: wgpu::VertexFormat::Uint32x4,
            size_bytes: 16,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U32,
            component_count: 4,
        },
        Case {
            dxgi_format: 4, // R32G32B32A32_SINT
            wgpu_format: wgpu::VertexFormat::Sint32x4,
            size_bytes: 16,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I32,
            component_count: 4,
        },
        Case {
            dxgi_format: 7, // R32G32B32_UINT
            wgpu_format: wgpu::VertexFormat::Uint32x3,
            size_bytes: 12,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U32,
            component_count: 3,
        },
        Case {
            dxgi_format: 8, // R32G32B32_SINT
            wgpu_format: wgpu::VertexFormat::Sint32x3,
            size_bytes: 12,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I32,
            component_count: 3,
        },
        Case {
            dxgi_format: 11, // R16G16B16A16_UNORM
            wgpu_format: wgpu::VertexFormat::Unorm16x4,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Unorm16,
            component_count: 4,
        },
        Case {
            dxgi_format: 12, // R16G16B16A16_UINT
            wgpu_format: wgpu::VertexFormat::Uint16x4,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U16,
            component_count: 4,
        },
        Case {
            dxgi_format: 13, // R16G16B16A16_SNORM
            wgpu_format: wgpu::VertexFormat::Snorm16x4,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Snorm16,
            component_count: 4,
        },
        Case {
            dxgi_format: 14, // R16G16B16A16_SINT
            wgpu_format: wgpu::VertexFormat::Sint16x4,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I16,
            component_count: 4,
        },
        Case {
            dxgi_format: 24, // R10G10B10A2_UNORM
            wgpu_format: wgpu::VertexFormat::Unorm10_10_10_2,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Unorm10_10_10_2,
            component_count: 4,
        },
        Case {
            dxgi_format: 17, // R32G32_UINT
            wgpu_format: wgpu::VertexFormat::Uint32x2,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U32,
            component_count: 2,
        },
        Case {
            dxgi_format: 18, // R32G32_SINT
            wgpu_format: wgpu::VertexFormat::Sint32x2,
            size_bytes: 8,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I32,
            component_count: 2,
        },
        Case {
            dxgi_format: 29, // R8G8B8A8_UNORM_SRGB
            wgpu_format: wgpu::VertexFormat::Unorm8x4,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Unorm8,
            component_count: 4,
        },
        Case {
            dxgi_format: 30, // R8G8B8A8_UINT
            wgpu_format: wgpu::VertexFormat::Uint8x4,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U8,
            component_count: 4,
        },
        Case {
            dxgi_format: 31, // R8G8B8A8_SNORM
            wgpu_format: wgpu::VertexFormat::Snorm8x4,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Snorm8,
            component_count: 4,
        },
        Case {
            dxgi_format: 32, // R8G8B8A8_SINT
            wgpu_format: wgpu::VertexFormat::Sint8x4,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I8,
            component_count: 4,
        },
        Case {
            dxgi_format: 49, // R8G8_UNORM
            wgpu_format: wgpu::VertexFormat::Unorm8x2,
            size_bytes: 2,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Unorm8,
            component_count: 2,
        },
        Case {
            dxgi_format: 50, // R8G8_UINT
            wgpu_format: wgpu::VertexFormat::Uint8x2,
            size_bytes: 2,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::U8,
            component_count: 2,
        },
        Case {
            dxgi_format: 51, // R8G8_SNORM
            wgpu_format: wgpu::VertexFormat::Snorm8x2,
            size_bytes: 2,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Snorm8,
            component_count: 2,
        },
        Case {
            dxgi_format: 52, // R8G8_SINT
            wgpu_format: wgpu::VertexFormat::Sint8x2,
            size_bytes: 2,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::I8,
            component_count: 2,
        },
        Case {
            dxgi_format: 91, // B8G8R8A8_UNORM_SRGB
            wgpu_format: wgpu::VertexFormat::Unorm8x4,
            size_bytes: 4,
            align_bytes: 4,
            component_type: DxgiFormatComponentType::Unorm8,
            component_count: 4,
        },
    ];

    for case in cases {
        let info = dxgi_format_info(case.dxgi_format).expect("dxgi_format_info");
        assert_eq!(info.wgpu_vertex_format, case.wgpu_format);
        assert_eq!(info.size_bytes, case.size_bytes);
        assert_eq!(info.align_bytes, case.align_bytes);
        assert_eq!(info.component_type, case.component_type);
        assert_eq!(info.component_count, case.component_count);
    }
}

#[test]
fn append_aligned_element_uses_4b_alignment_even_for_2b_formats() {
    // D3D11_APPEND_ALIGNED_ELEMENT aligns to 4 bytes regardless of element format size.
    let tex_hash = fnv1a_32(b"TEXCOORD");
    let pos_hash = fnv1a_32(b"POSITION");

    let blob = build_ilay(&[
        IlayElem {
            // TEXCOORD0: R8G8_UNORM @ offset 0 (2 bytes)
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            dxgi_format: 49, // DXGI_FORMAT_R8G8_UNORM
            input_slot: 0,
            aligned_byte_offset: 0,
            input_slot_class: 0,
            instance_data_step_rate: 0,
        },
        IlayElem {
            // POSITION0: R32_FLOAT @ append (should land at offset 4, not 2)
            semantic_name_hash: pos_hash,
            semantic_index: 0,
            dxgi_format: 41, // DXGI_FORMAT_R32_FLOAT
            input_slot: 0,
            aligned_byte_offset: D3D11_APPEND_ALIGNED_ELEMENT,
            input_slot_class: 0,
            instance_data_step_rate: 0,
        },
    ]);

    let layout = InputLayoutDesc::parse(&blob).expect("ilay parse");
    let strides = [8u32]; // 2 + pad2 + 4
    let binding = InputLayoutBinding::new(&layout, &strides);
    let signature = [
        VsInputSignatureElement {
            semantic_name_hash: pos_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        },
        VsInputSignatureElement {
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            input_register: 1,
            mask: 0xF,
            shader_location: 1,
        },
    ];

    let mapped =
        map_layout_to_shader_locations_compact(&binding, &signature).expect("mapping should work");
    let attrs = &mapped.buffers[0].attributes;
    assert_eq!(attrs.len(), 2);
    let mut offsets = attrs
        .iter()
        .map(|a| (a.shader_location, a.offset))
        .collect::<Vec<_>>();
    offsets.sort();
    assert_eq!(offsets, vec![(0, 4), (1, 0)]);
}

#[test]
fn rejects_misaligned_explicit_offset_for_2b_format() {
    let tex_hash = fnv1a_32(b"TEXCOORD");
    let blob = build_ilay(&[IlayElem {
        semantic_name_hash: tex_hash,
        semantic_index: 0,
        dxgi_format: 49, // DXGI_FORMAT_R8G8_UNORM
        input_slot: 0,
        aligned_byte_offset: 2, // not 4-byte aligned
        input_slot_class: 0,
        instance_data_step_rate: 0,
    }]);

    let layout = InputLayoutDesc::parse(&blob).expect("ilay parse");
    let strides = [4u32];
    let binding = InputLayoutBinding::new(&layout, &strides);
    let signature = [VsInputSignatureElement {
        semantic_name_hash: tex_hash,
        semantic_index: 0,
        input_register: 0,
        mask: 0xF,
        shader_location: 0,
    }];

    let err = map_layout_to_shader_locations_compact(&binding, &signature).unwrap_err();
    assert!(matches!(
        err,
        InputLayoutError::MisalignedOffset {
            slot: 0,
            offset: 2,
            alignment: 4
        }
    ));
}

#[test]
fn compact_slot_mapping_still_works_with_new_formats() {
    let tex_hash = fnv1a_32(b"TEXCOORD");
    let pos_hash = fnv1a_32(b"POSITION");

    let blob = build_ilay(&[
        IlayElem {
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            dxgi_format: 49, // R8G8_UNORM
            input_slot: 0,
            aligned_byte_offset: 0,
            input_slot_class: 0,
            instance_data_step_rate: 0,
        },
        IlayElem {
            semantic_name_hash: pos_hash,
            semantic_index: 0,
            dxgi_format: 35, // R16G16_UNORM
            input_slot: 31,
            aligned_byte_offset: 0,
            input_slot_class: 0,
            instance_data_step_rate: 0,
        },
    ]);

    let layout = InputLayoutDesc::parse(&blob).expect("ilay parse");
    let mut strides = vec![0u32; 32];
    strides[0] = 4;
    strides[31] = 4;
    let binding = InputLayoutBinding::new(&layout, &strides);
    let signature = [
        VsInputSignatureElement {
            semantic_name_hash: tex_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        },
        VsInputSignatureElement {
            semantic_name_hash: pos_hash,
            semantic_index: 0,
            input_register: 1,
            mask: 0xF,
            shader_location: 1,
        },
    ];

    let mapped =
        map_layout_to_shader_locations_compact(&binding, &signature).expect("compact mapping");
    assert_eq!(mapped.buffers.len(), 2);
    assert_eq!(mapped.d3d_slot_to_wgpu_slot.get(&0), Some(&0));
    assert_eq!(mapped.d3d_slot_to_wgpu_slot.get(&31), Some(&1));
}
