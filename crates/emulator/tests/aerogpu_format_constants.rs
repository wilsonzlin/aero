use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat as ProtocolAerogpuFormat;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;

const AEROGPU_FORMAT_CASES: &[(ProtocolAerogpuFormat, AeroGpuFormat)] = &[
    (ProtocolAerogpuFormat::Invalid, AeroGpuFormat::Invalid),
    (
        ProtocolAerogpuFormat::B8G8R8A8Unorm,
        AeroGpuFormat::B8G8R8A8Unorm,
    ),
    (
        ProtocolAerogpuFormat::B8G8R8X8Unorm,
        AeroGpuFormat::B8G8R8X8Unorm,
    ),
    (
        ProtocolAerogpuFormat::R8G8B8A8Unorm,
        AeroGpuFormat::R8G8B8A8Unorm,
    ),
    (
        ProtocolAerogpuFormat::R8G8B8X8Unorm,
        AeroGpuFormat::R8G8B8X8Unorm,
    ),
    (
        ProtocolAerogpuFormat::B5G6R5Unorm,
        AeroGpuFormat::B5G6R5Unorm,
    ),
    (
        ProtocolAerogpuFormat::B5G5R5A1Unorm,
        AeroGpuFormat::B5G5R5A1Unorm,
    ),
    (
        ProtocolAerogpuFormat::B8G8R8A8UnormSrgb,
        AeroGpuFormat::B8G8R8A8UnormSrgb,
    ),
    (
        ProtocolAerogpuFormat::B8G8R8X8UnormSrgb,
        AeroGpuFormat::B8G8R8X8UnormSrgb,
    ),
    (
        ProtocolAerogpuFormat::R8G8B8A8UnormSrgb,
        AeroGpuFormat::R8G8B8A8UnormSrgb,
    ),
    (
        ProtocolAerogpuFormat::R8G8B8X8UnormSrgb,
        AeroGpuFormat::R8G8B8X8UnormSrgb,
    ),
    (
        ProtocolAerogpuFormat::D24UnormS8Uint,
        AeroGpuFormat::D24UnormS8Uint,
    ),
    (ProtocolAerogpuFormat::D32Float, AeroGpuFormat::D32Float),
    (ProtocolAerogpuFormat::BC1RgbaUnorm, AeroGpuFormat::Bc1Unorm),
    (
        ProtocolAerogpuFormat::BC1RgbaUnormSrgb,
        AeroGpuFormat::Bc1UnormSrgb,
    ),
    (ProtocolAerogpuFormat::BC2RgbaUnorm, AeroGpuFormat::Bc2Unorm),
    (
        ProtocolAerogpuFormat::BC2RgbaUnormSrgb,
        AeroGpuFormat::Bc2UnormSrgb,
    ),
    (ProtocolAerogpuFormat::BC3RgbaUnorm, AeroGpuFormat::Bc3Unorm),
    (
        ProtocolAerogpuFormat::BC3RgbaUnormSrgb,
        AeroGpuFormat::Bc3UnormSrgb,
    ),
    (ProtocolAerogpuFormat::BC7RgbaUnorm, AeroGpuFormat::Bc7Unorm),
    (
        ProtocolAerogpuFormat::BC7RgbaUnormSrgb,
        AeroGpuFormat::Bc7UnormSrgb,
    ),
];

#[test]
fn aerogpu_format_discriminants_match_protocol() {
    for &(protocol, local) in AEROGPU_FORMAT_CASES {
        assert_eq!(
            local as u32, protocol as u32,
            "AeroGpuFormat::{local:?} discriminant differs from aero-protocol"
        );
    }
}

#[test]
fn aerogpu_format_from_u32_matches_protocol_values() {
    for &(protocol, local) in AEROGPU_FORMAT_CASES {
        let value = protocol as u32;
        assert_eq!(
            AeroGpuFormat::from_u32(value),
            local,
            "AeroGpuFormat::from_u32({value}) should return {local:?}"
        );
    }

    assert_eq!(AeroGpuFormat::from_u32(0xffff_ffff), AeroGpuFormat::Invalid);
}
