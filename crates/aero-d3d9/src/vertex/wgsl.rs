use crate::vertex::declaration::{DeclUsage, VertexDeclaration, VertexElement, VertexInputError};
use crate::vertex::format_map::{map_element_format, WebGpuVertexCaps};
use crate::vertex::location_map::VertexLocationMap;
use std::borrow::Cow;
use wgpu::VertexFormat;

/// WGSL type information for a vertex input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WgslTypeInfo {
    pub ty: &'static str,
    pub requires_f16: bool,
}

/// WGSL vertex input field derived from a D3D declaration element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgslVertexInputField {
    pub usage: DeclUsage,
    pub usage_index: u8,
    pub location: u32,
    pub name: Cow<'static, str>,
    pub ty: WgslTypeInfo,
}

/// Build a list of WGSL vertex input fields for a D3D9 vertex declaration.
///
/// The output is sorted by `location` for stable struct generation.
pub fn wgsl_vertex_input_fields(
    decl: &VertexDeclaration,
    caps: WebGpuVertexCaps,
    location_map: &dyn VertexLocationMap,
) -> Result<Vec<WgslVertexInputField>, VertexInputError> {
    let mut fields = Vec::new();
    for e in &decl.elements {
        let mapped = map_element_format(e.ty, caps)?;
        fields.push(wgsl_field_for_element(e, mapped.format, location_map)?);
    }

    fields.sort_by_key(|f| f.location);
    Ok(fields)
}

fn wgsl_field_for_element(
    e: &VertexElement,
    vertex_format: VertexFormat,
    location_map: &dyn VertexLocationMap,
) -> Result<WgslVertexInputField, VertexInputError> {
    let location = location_map.location_for(e.usage, e.usage_index)?;
    let ty = wgsl_type_for_vertex_format(vertex_format).ok_or(VertexInputError::UnsupportedDeclType {
        ty: e.ty,
    })?;
    Ok(WgslVertexInputField {
        usage: e.usage,
        usage_index: e.usage_index,
        location,
        name: semantic_name(e.usage, e.usage_index),
        ty,
    })
}

/// Convert a WebGPU vertex format into the WGSL type required for a vertex stage input.
pub fn wgsl_type_for_vertex_format(fmt: VertexFormat) -> Option<WgslTypeInfo> {
    let (ty, requires_f16) = match fmt {
        VertexFormat::Float32 => ("f32", false),
        VertexFormat::Float32x2 => ("vec2<f32>", false),
        VertexFormat::Float32x3 => ("vec3<f32>", false),
        VertexFormat::Float32x4 => ("vec4<f32>", false),
        VertexFormat::Float16x2 => ("vec2<f16>", true),
        VertexFormat::Float16x4 => ("vec4<f16>", true),

        VertexFormat::Unorm8x4 => ("vec4<f32>", false),
        VertexFormat::Snorm16x2 => ("vec2<f32>", false),
        VertexFormat::Snorm16x4 => ("vec4<f32>", false),
        VertexFormat::Unorm16x2 => ("vec2<f32>", false),
        VertexFormat::Unorm16x4 => ("vec4<f32>", false),

        VertexFormat::Uint8x4 => ("vec4<u32>", false),
        VertexFormat::Sint16x2 => ("vec2<i32>", false),
        VertexFormat::Sint16x4 => ("vec4<i32>", false),
        _ => return None,
    };

    Some(WgslTypeInfo { ty, requires_f16 })
}

fn semantic_name(usage: DeclUsage, usage_index: u8) -> Cow<'static, str> {
    // Keep names deterministic and stable, but avoid baking in D3D-specific naming beyond
    // debug friendliness. The shader translator may choose to rename fields.
    match usage {
        DeclUsage::Position => Cow::Owned(format!("position{usage_index}")),
        DeclUsage::Normal => Cow::Owned(format!("normal{usage_index}")),
        DeclUsage::Tangent => Cow::Owned(format!("tangent{usage_index}")),
        DeclUsage::Binormal => Cow::Owned(format!("binormal{usage_index}")),
        DeclUsage::BlendWeight => Cow::Owned(format!("blend_weight{usage_index}")),
        DeclUsage::BlendIndices => Cow::Owned(format!("blend_indices{usage_index}")),
        DeclUsage::Color => Cow::Owned(format!("color{usage_index}")),
        DeclUsage::TexCoord => Cow::Owned(format!("texcoord{usage_index}")),
        DeclUsage::PSize => Cow::Owned(format!("psize{usage_index}")),
        DeclUsage::Fog => Cow::Owned(format!("fog{usage_index}")),
        DeclUsage::Depth => Cow::Owned(format!("depth{usage_index}")),
        DeclUsage::Sample => Cow::Owned(format!("sample{usage_index}")),
        DeclUsage::TessFactor => Cow::Owned(format!("tess_factor{usage_index}")),
        DeclUsage::PositionT => Cow::Owned(format!("positiont{usage_index}")),
    }
}
