use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCompareFunc, AerogpuCullMode, AerogpuFillMode,
};

#[test]
fn pipeline_state_enums_from_u32_decodes_known_values() {
    assert_eq!(
        AerogpuBlendFactor::from_u32(0),
        Some(AerogpuBlendFactor::Zero)
    );
    assert_eq!(
        AerogpuBlendFactor::from_u32(2),
        Some(AerogpuBlendFactor::SrcAlpha)
    );
    assert_eq!(
        AerogpuBlendFactor::from_u32(7),
        Some(AerogpuBlendFactor::InvConstant)
    );
    assert_eq!(AerogpuBlendFactor::from_u32(99), None);

    assert_eq!(AerogpuBlendOp::from_u32(0), Some(AerogpuBlendOp::Add));
    assert_eq!(AerogpuBlendOp::from_u32(4), Some(AerogpuBlendOp::Max));
    assert_eq!(AerogpuBlendOp::from_u32(99), None);

    assert_eq!(
        AerogpuCompareFunc::from_u32(1),
        Some(AerogpuCompareFunc::Less)
    );
    assert_eq!(
        AerogpuCompareFunc::from_u32(2),
        Some(AerogpuCompareFunc::Equal)
    );
    assert_eq!(
        AerogpuCompareFunc::from_u32(7),
        Some(AerogpuCompareFunc::Always)
    );
    assert_eq!(AerogpuCompareFunc::from_u32(99), None);

    assert_eq!(AerogpuFillMode::from_u32(0), Some(AerogpuFillMode::Solid));
    assert_eq!(
        AerogpuFillMode::from_u32(1),
        Some(AerogpuFillMode::Wireframe)
    );
    assert_eq!(AerogpuFillMode::from_u32(99), None);

    assert_eq!(AerogpuCullMode::from_u32(0), Some(AerogpuCullMode::None));
    assert_eq!(AerogpuCullMode::from_u32(2), Some(AerogpuCullMode::Back));
    assert_eq!(AerogpuCullMode::from_u32(99), None);
}
