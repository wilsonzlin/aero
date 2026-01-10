use aero_d3d9::vertex::*;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

fn decl(elements: Vec<VertexElement>) -> VertexDeclaration {
    VertexDeclaration { elements }
}

#[test]
fn translate_single_stream_position_color_uv() {
    let decl = decl(vec![
        VertexElement::new(
            0,
            0,
            DeclType::Float3,
            DeclMethod::Default,
            DeclUsage::Position,
            0,
        ),
        VertexElement::new(
            0,
            12,
            DeclType::D3dColor,
            DeclMethod::Default,
            DeclUsage::Color,
            0,
        ),
        VertexElement::new(
            0,
            16,
            DeclType::Float2,
            DeclMethod::Default,
            DeclUsage::TexCoord,
            0,
        ),
    ]);

    let mut strides = BTreeMap::new();
    strides.insert(0, 24);

    let freq = StreamsFreqState::default();
    let caps = WebGpuVertexCaps {
        vertex_attribute_16bit: true,
    };

    let translated =
        translate_vertex_declaration(&decl, &strides, &freq, caps, &StandardLocationMap).unwrap();

    assert_eq!(translated.buffers.len(), 1);
    assert_eq!(translated.stream_to_buffer_slot.get(&0), Some(&0));
    assert_eq!(translated.conversions.len(), 1);
    assert!(translated.conversions.contains_key(&0));
    assert_eq!(translated.instancing.draw_instances(), 1);

    let b0 = &translated.buffers[0];
    assert_eq!(b0.array_stride, 24);
    assert_eq!(b0.step_mode, wgpu::VertexStepMode::Vertex);
    assert_eq!(
        b0.attributes,
        vec![
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 12,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 16,
                shader_location: 8,
            },
        ]
    );
}

#[test]
fn translate_two_stream_layout() {
    let decl = decl(vec![
        VertexElement::new(
            0,
            0,
            DeclType::Float3,
            DeclMethod::Default,
            DeclUsage::Position,
            0,
        ),
        VertexElement::new(
            1,
            0,
            DeclType::Float2,
            DeclMethod::Default,
            DeclUsage::TexCoord,
            0,
        ),
        VertexElement::new(
            1,
            8,
            DeclType::D3dColor,
            DeclMethod::Default,
            DeclUsage::Color,
            0,
        ),
    ]);

    let mut strides = BTreeMap::new();
    strides.insert(0, 12);
    strides.insert(1, 12);

    let freq = StreamsFreqState::default();
    let caps = WebGpuVertexCaps {
        vertex_attribute_16bit: true,
    };

    let translated =
        translate_vertex_declaration(&decl, &strides, &freq, caps, &StandardLocationMap).unwrap();

    assert_eq!(translated.buffers.len(), 2);
    assert_eq!(translated.stream_to_buffer_slot.get(&0), Some(&0));
    assert_eq!(translated.stream_to_buffer_slot.get(&1), Some(&1));
    assert_eq!(translated.conversions.len(), 1);
    assert!(translated.conversions.contains_key(&1));

    let b0 = &translated.buffers[0];
    assert_eq!(b0.array_stride, 12);
    assert_eq!(
        b0.attributes,
        vec![wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        }]
    );

    let b1 = &translated.buffers[1];
    assert_eq!(b1.array_stride, 12);
    assert_eq!(
        b1.attributes,
        vec![
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 8,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 8,
                shader_location: 6,
            },
        ]
    );
}

#[test]
fn translate_instanced_color_stream() {
    let decl = decl(vec![
        VertexElement::new(
            0,
            0,
            DeclType::Float3,
            DeclMethod::Default,
            DeclUsage::Position,
            0,
        ),
        VertexElement::new(
            1,
            0,
            DeclType::D3dColor,
            DeclMethod::Default,
            DeclUsage::Color,
            0,
        ),
    ]);

    let mut strides = BTreeMap::new();
    strides.insert(0, 12);
    strides.insert(1, 4);

    let mut freq = StreamsFreqState::default();
    freq.set(0, 0x4000_0000 | 2).unwrap(); // INDEXEDDATA (draw 2 instances)
    freq.set(1, 0x8000_0000 | 1).unwrap(); // INSTANCEDATA divisor=1

    let caps = WebGpuVertexCaps {
        vertex_attribute_16bit: true,
    };

    let translated =
        translate_vertex_declaration(&decl, &strides, &freq, caps, &StandardLocationMap).unwrap();

    assert_eq!(translated.instancing.draw_instances(), 2);
    assert_eq!(translated.instancing.stream_step(0), StreamStep::Vertex);
    assert_eq!(
        translated.instancing.stream_step(1),
        StreamStep::Instance { divisor: 1 }
    );

    assert_eq!(
        translated.buffers[0].step_mode,
        wgpu::VertexStepMode::Vertex
    );
    assert_eq!(
        translated.buffers[1].step_mode,
        wgpu::VertexStepMode::Instance
    );
    assert_eq!(translated.conversions.len(), 1);
    assert!(translated.conversions.contains_key(&1));
}

#[test]
fn dec3n_requires_conversion_and_decodes_correctly() {
    let decl = decl(vec![VertexElement::new(
        0,
        0,
        DeclType::Dec3N,
        DeclMethod::Default,
        DeclUsage::Normal,
        0,
    )]);

    let mut strides = BTreeMap::new();
    strides.insert(0, 4);

    let freq = StreamsFreqState::default();
    let caps = WebGpuVertexCaps {
        vertex_attribute_16bit: true,
    };

    let translated =
        translate_vertex_declaration(&decl, &strides, &freq, caps, &StandardLocationMap).unwrap();

    assert_eq!(translated.buffers[0].array_stride, 12);
    assert_eq!(translated.conversions.len(), 1);
    let plan = translated.conversions.get(&0).unwrap();
    assert_eq!(plan.src_stride, 4);
    assert_eq!(plan.dst_stride, 12);
    assert_eq!(plan.elements.len(), 1);

    // Packed DEC3N: x=511 (1.0), y=0 (0.0), z=-512 (-1.0).
    let packed: u32 = 0x2000_01ff;
    let src = packed.to_le_bytes();
    let dst = plan.convert_vertices(&src, 1).unwrap();
    let x = f32::from_le_bytes(dst[0..4].try_into().unwrap());
    let y = f32::from_le_bytes(dst[4..8].try_into().unwrap());
    let z = f32::from_le_bytes(dst[8..12].try_into().unwrap());
    assert_eq!((x, y, z), (1.0, 0.0, -1.0));
}

#[test]
fn half_promotes_to_f32_when_f16_vertex_attributes_unsupported() {
    let decl = decl(vec![VertexElement::new(
        0,
        0,
        DeclType::Float16_2,
        DeclMethod::Default,
        DeclUsage::TexCoord,
        0,
    )]);

    let mut strides = BTreeMap::new();
    strides.insert(0, 4);

    let freq = StreamsFreqState::default();
    let caps = WebGpuVertexCaps {
        vertex_attribute_16bit: false,
    };

    let translated =
        translate_vertex_declaration(&decl, &strides, &freq, caps, &StandardLocationMap).unwrap();

    let b0 = &translated.buffers[0];
    assert_eq!(b0.array_stride, 8);
    assert_eq!(b0.attributes[0].format, wgpu::VertexFormat::Float32x2);

    let plan = translated.conversions.get(&0).unwrap();
    let src = [
        half::f16::from_f32(0.5).to_bits(),
        half::f16::from_f32(1.0).to_bits(),
    ]
    .into_iter()
    .flat_map(|v| v.to_le_bytes())
    .collect::<Vec<_>>();
    let dst = plan.convert_vertices(&src, 1).unwrap();
    let x = f32::from_le_bytes(dst[0..4].try_into().unwrap());
    let y = f32::from_le_bytes(dst[4..8].try_into().unwrap());
    assert_eq!((x, y), (0.5, 1.0));
}

#[test]
fn fvf_decode_produces_declaration_compatible_with_fixed_function_locations() {
    // XYZ | NORMAL | DIFFUSE | TEX1 (default texcoord size=2)
    let fvf = Fvf(0x002 | 0x010 | 0x040 | 0x0100);
    let layout = fvf.decode().unwrap();
    assert_eq!(layout.stride, 36);
    assert_eq!(layout.pretransformed, false);

    let mut strides = BTreeMap::new();
    strides.insert(0, layout.stride);

    let translated = translate_vertex_declaration(
        &layout.declaration,
        &strides,
        &StreamsFreqState::default(),
        WebGpuVertexCaps {
            vertex_attribute_16bit: true,
        },
        &FixedFunctionLocationMap,
    )
    .unwrap();

    assert_eq!(translated.conversions.len(), 1);
    assert!(translated.conversions.contains_key(&0));

    let attrs = &translated.buffers[0].attributes;
    assert_eq!(
        attrs,
        &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 24,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 28,
                shader_location: 5,
            },
        ]
    );
}

#[test]
fn fvf_texcoord_size_bits_follow_d3d9_encoding() {
    // XYZ | TEX1, default size for TEX0 is 2 components when the size bits are 0.
    let base = 0x002 | 0x0100;

    let fvf2 = Fvf(base);
    let layout2 = fvf2.decode().unwrap();
    assert_eq!(layout2.declaration.elements.len(), 2);
    assert_eq!(layout2.declaration.elements[1].ty, DeclType::Float2);

    // Bits=3 => 1 component (`D3DFVF_TEXCOORDSIZE1(0)`).
    let fvf1 = Fvf(base | (3 << 16));
    let layout1 = fvf1.decode().unwrap();
    assert_eq!(layout1.declaration.elements[1].ty, DeclType::Float1);

    // Bits=1 => 3 components (`D3DFVF_TEXCOORDSIZE3(0)`).
    let fvf3 = Fvf(base | (1 << 16));
    let layout3 = fvf3.decode().unwrap();
    assert_eq!(layout3.declaration.elements[1].ty, DeclType::Float3);

    // Bits=2 => 4 components (`D3DFVF_TEXCOORDSIZE4(0)`).
    let fvf4 = Fvf(base | (2 << 16));
    let layout4 = fvf4.decode().unwrap();
    assert_eq!(layout4.declaration.elements[1].ty, DeclType::Float4);
}

#[test]
fn expand_instance_data_emulates_divisor() {
    let stride = 4;
    let divisor = 2;
    let draw_instances = 5;
    let src = [1u32.to_le_bytes(), 2u32.to_le_bytes(), 3u32.to_le_bytes()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let expanded = expand_instance_data(&src, stride, divisor, draw_instances).unwrap();
    let words = expanded
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(words, vec![1, 1, 2, 2, 3]);
}

#[test]
fn parses_serialized_d3d_vertex_elements() {
    fn push_elem(
        out: &mut Vec<u8>,
        stream: u16,
        offset: u16,
        ty: u8,
        method: u8,
        usage: u8,
        usage_index: u8,
    ) {
        out.extend_from_slice(&stream.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.push(ty);
        out.push(method);
        out.push(usage);
        out.push(usage_index);
    }

    let mut bytes = Vec::new();
    push_elem(
        &mut bytes,
        0,
        0,
        DeclType::Float3 as u8,
        DeclMethod::Default as u8,
        DeclUsage::Position as u8,
        0,
    );
    push_elem(
        &mut bytes,
        0,
        12,
        DeclType::D3dColor as u8,
        DeclMethod::Default as u8,
        DeclUsage::Color as u8,
        0,
    );
    let end = D3dVertexElement9::end();
    push_elem(
        &mut bytes,
        end.stream,
        end.offset,
        end.ty,
        end.method,
        end.usage,
        end.usage_index,
    );

    let decl = VertexDeclaration::from_d3d_bytes(&bytes).unwrap();
    assert_eq!(decl.elements.len(), 2);
    assert_eq!(decl.elements[0].stream, 0);
    assert_eq!(decl.elements[0].offset, 0);
    assert_eq!(decl.elements[0].ty, DeclType::Float3);
    assert_eq!(decl.elements[0].usage, DeclUsage::Position);
    assert_eq!(decl.elements[1].offset, 12);
    assert_eq!(decl.elements[1].ty, DeclType::D3dColor);
    assert_eq!(decl.elements[1].usage, DeclUsage::Color);
}

#[test]
fn vertex_decl_bytes_require_end_marker() {
    let bytes = [0u8; 8];
    let err = VertexDeclaration::from_d3d_bytes(&bytes).unwrap_err();
    assert!(matches!(err, VertexInputError::VertexDeclMissingEndMarker));
}

#[test]
fn vertex_decl_bytes_len_must_be_multiple_of_8() {
    let bytes = [0u8; 7];
    let err = VertexDeclaration::from_d3d_bytes(&bytes).unwrap_err();
    assert!(matches!(
        err,
        VertexInputError::VertexDeclBytesNotMultipleOf8 { .. }
    ));
}
