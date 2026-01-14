use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

#[test]
fn aerogpu_format_from_u32_decodes_known_values() {
    assert_eq!(AerogpuFormat::from_u32(0), Some(AerogpuFormat::Invalid));
    assert_eq!(
        AerogpuFormat::from_u32(3),
        Some(AerogpuFormat::R8G8B8A8Unorm)
    );
    assert_eq!(AerogpuFormat::from_u32(33), Some(AerogpuFormat::D32Float));
    assert_eq!(
        AerogpuFormat::from_u32(71),
        Some(AerogpuFormat::BC7RgbaUnormSrgb)
    );
    assert_eq!(AerogpuFormat::from_u32(0xDEAD_BEEF), None);
}
