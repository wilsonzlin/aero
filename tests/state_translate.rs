use aero_d3d9::state::tracker::{
    BlendFactor, BlendOp, ColorWriteMask, CompareFunc, CullMode, RasterizerState, StencilOp,
};
use aero_d3d9::state::{
    translate_blend_factor, translate_blend_op, translate_color_write_mask, translate_compare_func,
    translate_cull_and_front_face, translate_stencil_op,
};

#[test]
fn cull_mode_respects_front_face() {
    let mut raster = RasterizerState::default();
    raster.cull_mode = CullMode::CW;
    raster.front_counter_clockwise = false; // front is CW
    let (front_face, cull_mode) = translate_cull_and_front_face(&raster);
    assert_eq!(front_face, wgpu::FrontFace::Cw);
    assert_eq!(cull_mode, Some(wgpu::Face::Front));

    raster.front_counter_clockwise = true; // front is CCW
    let (front_face, cull_mode) = translate_cull_and_front_face(&raster);
    assert_eq!(front_face, wgpu::FrontFace::Ccw);
    assert_eq!(cull_mode, Some(wgpu::Face::Back));
}

#[test]
fn compare_func_maps_correctly() {
    assert_eq!(
        translate_compare_func(CompareFunc::LessEqual),
        wgpu::CompareFunction::LessEqual
    );
    assert_eq!(
        translate_compare_func(CompareFunc::Always),
        wgpu::CompareFunction::Always
    );
}

#[test]
fn stencil_op_maps_correctly() {
    assert_eq!(
        translate_stencil_op(StencilOp::IncrSat),
        wgpu::StencilOperation::IncrementClamp
    );
    assert_eq!(
        translate_stencil_op(StencilOp::Decr),
        wgpu::StencilOperation::DecrementWrap
    );
}

#[test]
fn blend_factor_maps_correctly() {
    assert_eq!(
        translate_blend_factor(BlendFactor::SrcAlpha),
        wgpu::BlendFactor::SrcAlpha
    );
    assert_eq!(
        translate_blend_factor(BlendFactor::InvDestColor),
        wgpu::BlendFactor::OneMinusDst
    );
    assert_eq!(
        translate_blend_factor(BlendFactor::BlendFactor),
        wgpu::BlendFactor::Constant
    );
}

#[test]
fn blend_op_maps_correctly() {
    assert_eq!(
        translate_blend_op(BlendOp::RevSubtract),
        wgpu::BlendOperation::ReverseSubtract
    );
    assert_eq!(translate_blend_op(BlendOp::Max), wgpu::BlendOperation::Max);
}

#[test]
fn color_write_mask_maps_correctly() {
    let mask = ColorWriteMask(0b0101); // R + B
    let translated = translate_color_write_mask(mask);
    assert!(translated.contains(wgpu::ColorWrites::RED));
    assert!(!translated.contains(wgpu::ColorWrites::GREEN));
    assert!(translated.contains(wgpu::ColorWrites::BLUE));
    assert!(!translated.contains(wgpu::ColorWrites::ALPHA));
}
