use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuSamplerAddressMode, AerogpuSamplerFilter};

#[test]
fn sampler_enums_from_u32_decodes_known_values() {
    assert_eq!(
        AerogpuSamplerFilter::from_u32(0),
        Some(AerogpuSamplerFilter::Nearest)
    );
    assert_eq!(
        AerogpuSamplerFilter::from_u32(1),
        Some(AerogpuSamplerFilter::Linear)
    );
    assert_eq!(AerogpuSamplerFilter::from_u32(99), None);

    assert_eq!(
        AerogpuSamplerAddressMode::from_u32(0),
        Some(AerogpuSamplerAddressMode::ClampToEdge)
    );
    assert_eq!(
        AerogpuSamplerAddressMode::from_u32(1),
        Some(AerogpuSamplerAddressMode::Repeat)
    );
    assert_eq!(
        AerogpuSamplerAddressMode::from_u32(2),
        Some(AerogpuSamplerAddressMode::MirrorRepeat)
    );
    assert_eq!(AerogpuSamplerAddressMode::from_u32(99), None);
}
