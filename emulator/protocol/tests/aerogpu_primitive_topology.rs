use aero_protocol::aerogpu::aerogpu_cmd::AerogpuPrimitiveTopology;

#[test]
fn primitive_topology_from_u32_decodes_known_values() {
    assert_eq!(
        AerogpuPrimitiveTopology::from_u32(1),
        Some(AerogpuPrimitiveTopology::PointList)
    );
    assert_eq!(
        AerogpuPrimitiveTopology::from_u32(13),
        Some(AerogpuPrimitiveTopology::TriangleStripAdj)
    );
    assert_eq!(
        AerogpuPrimitiveTopology::from_u32(33),
        Some(AerogpuPrimitiveTopology::PatchList1)
    );
    assert_eq!(
        AerogpuPrimitiveTopology::from_u32(64),
        Some(AerogpuPrimitiveTopology::PatchList32)
    );

    // Values outside the D3D11 primitive-topology range must not decode.
    assert_eq!(AerogpuPrimitiveTopology::from_u32(0), None);
    assert_eq!(AerogpuPrimitiveTopology::from_u32(32), None);
    assert_eq!(AerogpuPrimitiveTopology::from_u32(65), None);
}

