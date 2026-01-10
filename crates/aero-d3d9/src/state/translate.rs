use crate::state::topology::{translate_primitive_topology, PrimitiveTopologyTranslation};
use crate::state::tracker::{
    BlendFactor, BlendOp, BlendState, ColorWriteMask, CompareFunc, CullMode, DepthStencilState,
    PipelineKey, RasterizerState, ScissorRect, StateTracker, StencilOp, Viewport,
};

#[derive(Clone, Debug)]
pub struct TranslatedPipelineState {
    pub primitive: wgpu::PrimitiveState,
    pub depth_stencil: Option<wgpu::DepthStencilState>,
    pub targets: Vec<Option<wgpu::ColorTargetState>>,
    pub multisample: wgpu::MultisampleState,
    pub topology_translation: PrimitiveTopologyTranslation,
}

#[derive(Clone, Debug)]
pub struct DynamicRenderState {
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,
    pub blend_constant: wgpu::Color,
    pub stencil_reference: u32,
}

pub fn translate_pipeline_state(
    tracker: &StateTracker,
) -> Option<(PipelineKey, TranslatedPipelineState, DynamicRenderState)> {
    let key = tracker.pipeline_key()?;

    let topology_translation = translate_primitive_topology(tracker.primitive_type);
    let primitive = translate_rasterizer_state(&tracker.rasterizer, topology_translation.topology);
    let depth_stencil = translate_depth_stencil_state(
        &tracker.depth_stencil,
        tracker.render_targets.depth_stencil_format,
    );

    let blend = translate_blend_state(&tracker.blend);
    let blend_constant = translate_blend_constant(&tracker.blend);

    let targets = tracker
        .render_targets
        .color_formats
        .iter()
        .enumerate()
        .map(|(i, &format)| {
            let format =
                translate_texture_format_srgb(format, tracker.render_targets.srgb_write_enable);
            let mask = tracker
                .render_targets
                .color_write_masks
                .get(i)
                .copied()
                .unwrap_or(ColorWriteMask::RGBA);
            Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: translate_color_write_mask(mask),
            })
        })
        .collect::<Vec<_>>();

    let translated = TranslatedPipelineState {
        primitive,
        depth_stencil,
        targets,
        multisample: wgpu::MultisampleState::default(),
        topology_translation,
    };

    let dynamic = DynamicRenderState {
        viewport: tracker.viewport,
        scissor: tracker.scissor,
        blend_constant,
        stencil_reference: tracker.depth_stencil.stencil_ref as u32,
    };

    Some((key, translated, dynamic))
}

pub fn translate_cull_and_front_face(
    raster: &RasterizerState,
) -> (wgpu::FrontFace, Option<wgpu::Face>) {
    let front_face = if raster.front_counter_clockwise {
        wgpu::FrontFace::Ccw
    } else {
        wgpu::FrontFace::Cw
    };

    let cull = match raster.cull_mode {
        CullMode::None => None,
        CullMode::CW => Some(match front_face {
            // D3D9 `CW` means "cull clockwise triangles".
            wgpu::FrontFace::Cw => wgpu::Face::Front,
            wgpu::FrontFace::Ccw => wgpu::Face::Back,
        }),
        CullMode::CCW => Some(match front_face {
            wgpu::FrontFace::Cw => wgpu::Face::Back,
            wgpu::FrontFace::Ccw => wgpu::Face::Front,
        }),
    };

    (front_face, cull)
}

pub fn translate_rasterizer_state(
    raster: &RasterizerState,
    topology: wgpu::PrimitiveTopology,
) -> wgpu::PrimitiveState {
    let (front_face, cull_mode) = translate_cull_and_front_face(raster);

    wgpu::PrimitiveState {
        topology,
        strip_index_format: None,
        front_face,
        cull_mode,
        // Wireframe support is optional in the requirements. We translate it but
        // callers must ensure the device was created with the corresponding
        // `POLYGON_MODE_LINE` feature, otherwise pipeline creation will fail.
        polygon_mode: if raster.fill_wireframe {
            wgpu::PolygonMode::Line
        } else {
            wgpu::PolygonMode::Fill
        },
        unclipped_depth: false,
        conservative: false,
    }
}

pub fn translate_depth_stencil_state(
    ds: &DepthStencilState,
    depth_stencil_format: Option<wgpu::TextureFormat>,
) -> Option<wgpu::DepthStencilState> {
    let format = depth_stencil_format?;
    let needs_depth_stencil = ds.depth_enable || ds.depth_write_enable || ds.stencil_enable;
    if !needs_depth_stencil {
        return None;
    }

    let depth_compare = if ds.depth_enable {
        translate_compare_func(ds.depth_func)
    } else {
        wgpu::CompareFunction::Always
    };
    let depth_write_enabled = ds.depth_enable && ds.depth_write_enable;

    let stencil = if ds.stencil_enable {
        let face = wgpu::StencilFaceState {
            compare: translate_compare_func(ds.stencil_func),
            fail_op: translate_stencil_op(ds.stencil_fail),
            depth_fail_op: translate_stencil_op(ds.stencil_zfail),
            pass_op: translate_stencil_op(ds.stencil_pass),
        };
        wgpu::StencilState {
            front: face,
            back: face,
            read_mask: ds.stencil_read_mask as u32,
            write_mask: ds.stencil_write_mask as u32,
        }
    } else {
        wgpu::StencilState {
            front: wgpu::StencilFaceState::IGNORE,
            back: wgpu::StencilFaceState::IGNORE,
            read_mask: 0xFF,
            write_mask: 0,
        }
    };

    Some(wgpu::DepthStencilState {
        format,
        depth_write_enabled,
        depth_compare,
        stencil,
        bias: wgpu::DepthBiasState::default(),
    })
}

fn translate_blend_state(blend: &BlendState) -> Option<wgpu::BlendState> {
    if !blend.alpha_blend_enable {
        return None;
    }

    let color = wgpu::BlendComponent {
        src_factor: translate_blend_factor(blend.src_blend),
        dst_factor: translate_blend_factor(blend.dst_blend),
        operation: translate_blend_op(blend.blend_op),
    };

    let alpha = if blend.separate_alpha_blend_enable {
        wgpu::BlendComponent {
            src_factor: translate_blend_factor(blend.src_blend_alpha),
            dst_factor: translate_blend_factor(blend.dst_blend_alpha),
            operation: translate_blend_op(blend.blend_op_alpha),
        }
    } else {
        color
    };

    Some(wgpu::BlendState { color, alpha })
}

fn translate_blend_constant(blend: &BlendState) -> wgpu::Color {
    let a = ((blend.blend_factor >> 24) & 0xFF) as f64 / 255.0;
    let r = ((blend.blend_factor >> 16) & 0xFF) as f64 / 255.0;
    let g = ((blend.blend_factor >> 8) & 0xFF) as f64 / 255.0;
    let b = (blend.blend_factor & 0xFF) as f64 / 255.0;
    wgpu::Color { r, g, b, a }
}

pub fn translate_compare_func(func: CompareFunc) -> wgpu::CompareFunction {
    match func {
        CompareFunc::Never => wgpu::CompareFunction::Never,
        CompareFunc::Less => wgpu::CompareFunction::Less,
        CompareFunc::Equal => wgpu::CompareFunction::Equal,
        CompareFunc::LessEqual => wgpu::CompareFunction::LessEqual,
        CompareFunc::Greater => wgpu::CompareFunction::Greater,
        CompareFunc::NotEqual => wgpu::CompareFunction::NotEqual,
        CompareFunc::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        CompareFunc::Always => wgpu::CompareFunction::Always,
    }
}

pub fn translate_stencil_op(op: StencilOp) -> wgpu::StencilOperation {
    match op {
        StencilOp::Keep => wgpu::StencilOperation::Keep,
        StencilOp::Zero => wgpu::StencilOperation::Zero,
        StencilOp::Replace => wgpu::StencilOperation::Replace,
        StencilOp::IncrSat => wgpu::StencilOperation::IncrementClamp,
        StencilOp::DecrSat => wgpu::StencilOperation::DecrementClamp,
        StencilOp::Invert => wgpu::StencilOperation::Invert,
        StencilOp::Incr => wgpu::StencilOperation::IncrementWrap,
        StencilOp::Decr => wgpu::StencilOperation::DecrementWrap,
    }
}

pub fn translate_blend_factor(factor: BlendFactor) -> wgpu::BlendFactor {
    match factor {
        BlendFactor::Zero => wgpu::BlendFactor::Zero,
        BlendFactor::One => wgpu::BlendFactor::One,
        BlendFactor::SrcColor => wgpu::BlendFactor::Src,
        BlendFactor::InvSrcColor => wgpu::BlendFactor::OneMinusSrc,
        BlendFactor::SrcAlpha => wgpu::BlendFactor::SrcAlpha,
        BlendFactor::InvSrcAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
        BlendFactor::DestAlpha => wgpu::BlendFactor::DstAlpha,
        BlendFactor::InvDestAlpha => wgpu::BlendFactor::OneMinusDstAlpha,
        BlendFactor::DestColor => wgpu::BlendFactor::Dst,
        BlendFactor::InvDestColor => wgpu::BlendFactor::OneMinusDst,
        BlendFactor::SrcAlphaSat => wgpu::BlendFactor::SrcAlphaSaturated,
        BlendFactor::BlendFactor => wgpu::BlendFactor::Constant,
        BlendFactor::InvBlendFactor => wgpu::BlendFactor::OneMinusConstant,
    }
}

pub fn translate_blend_op(op: BlendOp) -> wgpu::BlendOperation {
    match op {
        BlendOp::Add => wgpu::BlendOperation::Add,
        BlendOp::Subtract => wgpu::BlendOperation::Subtract,
        BlendOp::RevSubtract => wgpu::BlendOperation::ReverseSubtract,
        BlendOp::Min => wgpu::BlendOperation::Min,
        BlendOp::Max => wgpu::BlendOperation::Max,
    }
}

pub fn translate_color_write_mask(mask: ColorWriteMask) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if mask.0 & 0b0001 != 0 {
        out |= wgpu::ColorWrites::RED;
    }
    if mask.0 & 0b0010 != 0 {
        out |= wgpu::ColorWrites::GREEN;
    }
    if mask.0 & 0b0100 != 0 {
        out |= wgpu::ColorWrites::BLUE;
    }
    if mask.0 & 0b1000 != 0 {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

/// Approximate D3D9 `D3DRS_SRGBWRITEENABLE` via selecting an sRGB render target format.
///
/// Note: In WebGPU, the underlying texture *must* be created with the chosen
/// format. This helper only converts formats where an sRGB variant exists.
pub fn translate_texture_format_srgb(
    format: wgpu::TextureFormat,
    srgb_write_enable: bool,
) -> wgpu::TextureFormat {
    if !srgb_write_enable {
        return format;
    }

    match format {
        wgpu::TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8UnormSrgb,
        _ => format,
    }
}
