use aero_protocol::aerogpu::aerogpu_cmd::AerogpuIndexFormat;

#[test]
fn index_format_from_u32_decodes_known_values() {
    assert_eq!(
        AerogpuIndexFormat::from_u32(0),
        Some(AerogpuIndexFormat::Uint16)
    );
    assert_eq!(
        AerogpuIndexFormat::from_u32(1),
        Some(AerogpuIndexFormat::Uint32)
    );
    assert_eq!(AerogpuIndexFormat::from_u32(2), None);
}
