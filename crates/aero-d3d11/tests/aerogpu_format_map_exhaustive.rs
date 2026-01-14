use aero_d3d11::runtime::aerogpu_resources::map_aerogpu_format;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

// Single source of truth for protocol format coverage:
// - The generated `match` over `AerogpuFormat` is exhaustive, so adding a new protocol variant
//   fails compilation until this list is updated.
// - Each variant (excluding `Invalid`) is checked against `map_aerogpu_format`.
macro_rules! define_format_coverage_test {
    ($(
        $variant:ident => $status:ident $( ( $reason:expr ) )?
    ),+ $(,)?) => {
        // Compile-time exhaustiveness check: if `AerogpuFormat` gains a new variant, this match
        // becomes non-exhaustive and the test will fail to compile until the macro invocation is
        // updated.
        #[allow(dead_code)]
        fn _assert_aerogpu_format_cases_exhaustive(format: AerogpuFormat) {
            match format {
                AerogpuFormat::Invalid => (),
                $(AerogpuFormat::$variant => (),)+
            }
        }

        #[test]
        fn map_aerogpu_format_covers_all_protocol_formats() {
            // The protocol uses `Invalid` as a sentinel; `map_aerogpu_format` should reject it.
            assert!(map_aerogpu_format(AerogpuFormat::Invalid as u32).is_err());

            $(
                define_format_coverage_test!(@check $variant => $status $( ( $reason ) )?);
            )+
        }
    };

    (@check $variant:ident => supported) => {{
        let format = AerogpuFormat::$variant;
        let value = format as u32;
        let _ = map_aerogpu_format(value).unwrap_or_else(|e| {
            panic!(
                "map_aerogpu_format is missing support for AerogpuFormat::{} ({value}): {e:#}",
                stringify!($variant),
            )
        });
    }};

    (@check $variant:ident => unsupported($reason:expr)) => {{
        let format = AerogpuFormat::$variant;
        let value = format as u32;
        assert!(
            map_aerogpu_format(value).is_err(),
            "AerogpuFormat::{} ({value}) is expected to be unsupported by aero-d3d11's map_aerogpu_format: {}",
            stringify!($variant),
            $reason,
        );
    }};
}

define_format_coverage_test! {
    B8G8R8A8Unorm => supported,
    B8G8R8X8Unorm => supported,
    R8G8B8A8Unorm => supported,
    R8G8B8X8Unorm => supported,

    B5G6R5Unorm => unsupported(
        "wgpu 0.20 does not expose the packed 16-bit B5G6R5 texture format in WebGPU; aero-d3d11 does not currently implement a CPU expand-to-RGBA8 fallback like aero-gpu's D3D9 executor"
    ),
    B5G5R5A1Unorm => unsupported(
        "wgpu 0.20 does not expose the packed 16-bit B5G5R5A1 texture format in WebGPU; aero-d3d11 does not currently implement a CPU expand-to-RGBA8 fallback like aero-gpu's D3D9 executor"
    ),

    // ABI 1.2+: explicit sRGB format variants.
    B8G8R8A8UnormSrgb => supported,
    B8G8R8X8UnormSrgb => supported,
    R8G8B8A8UnormSrgb => supported,
    R8G8B8X8UnormSrgb => supported,

    D24UnormS8Uint => supported,
    D32Float => supported,

    BC1RgbaUnorm => supported,
    BC1RgbaUnormSrgb => supported,
    BC2RgbaUnorm => supported,
    BC2RgbaUnormSrgb => supported,
    BC3RgbaUnorm => supported,
    BC3RgbaUnormSrgb => supported,
    BC7RgbaUnorm => supported,
    BC7RgbaUnormSrgb => supported,
}
