use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

/// Conformance tests ensuring aero-gpu's protocol format handling stays in sync with
/// `aero_protocol::aerogpu_pci::AerogpuFormat`.
///
/// If a new protocol format is added, these tests should fail at compile time because the
/// expectation match table is intentionally exhaustive (no `_ => ...`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedSupport {
    Supported,
    Unsupported,
}

macro_rules! aerogpu_format_expectations {
    ($($variant:ident => $support:ident,)+) => {
        // Keep the protocol-format list and the match table in sync by generating both from the
        // same source of truth.
        //
        // The match is intentionally exhaustive (no `_ => ...`), so adding a new protocol enum
        // variant forces this table (and therefore the tests) to be updated.
        const ALL_PROTOCOL_FORMATS: &[AerogpuFormat] = &[
            $(AerogpuFormat::$variant,)+
        ];

        fn expected_protocol_support(format: AerogpuFormat) -> ExpectedSupport {
            match format {
                $(AerogpuFormat::$variant => ExpectedSupport::$support,)+
            }
        }
    }
}

aerogpu_format_expectations! {
    Invalid => Unsupported,

    B8G8R8A8Unorm => Supported,
    B8G8R8X8Unorm => Supported,
    R8G8B8A8Unorm => Supported,
    R8G8B8X8Unorm => Supported,
    B5G6R5Unorm => Supported,
    B5G5R5A1Unorm => Supported,

    B8G8R8A8UnormSrgb => Supported,
    B8G8R8X8UnormSrgb => Supported,
    R8G8B8A8UnormSrgb => Supported,
    R8G8B8X8UnormSrgb => Supported,

    D24UnormS8Uint => Supported,
    D32Float => Supported,

    BC1RgbaUnorm => Supported,
    BC1RgbaUnormSrgb => Supported,
    BC2RgbaUnorm => Supported,
    BC2RgbaUnormSrgb => Supported,
    BC3RgbaUnorm => Supported,
    BC3RgbaUnormSrgb => Supported,
    BC7RgbaUnorm => Supported,
    BC7RgbaUnormSrgb => Supported,
}

#[test]
fn command_processor_create_texture2d_accepts_all_protocol_formats() {
    use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

    for &format in ALL_PROTOCOL_FORMATS {
        let mut w = AerogpuCmdWriter::new();
        // Host-owned texture (backing_alloc_id=0) so the test doesn't need a guest allocation table.
        w.create_texture2d(
            /*texture_handle=*/ 1,
            /*usage_flags=*/ 0,
            format as u32,
            /*width=*/ 4,
            /*height=*/ 4,
            /*mip_levels=*/ 1,
            /*array_layers=*/ 1,
            /*row_pitch_bytes=*/ 0,
            /*backing_alloc_id=*/ 0,
            /*backing_offset_bytes=*/ 0,
        );
        let bytes = w.finish();

        let mut proc = crate::AeroGpuCommandProcessor::new();
        let result = proc.process_submission_with_allocations(&bytes, None, /*signal_fence=*/ 1);

        match expected_protocol_support(format) {
            ExpectedSupport::Supported => {
                result.unwrap_or_else(|err| {
                    panic!(
                        "CREATE_TEXTURE2D should accept protocol format {format:?} ({}), got error: {err:?}",
                        format as u32
                    )
                });
            }
            ExpectedSupport::Unsupported => {
                assert!(
                    matches!(result, Err(crate::CommandProcessorError::InvalidCreateTexture2d)),
                    "expected unsupported format {format:?} to be rejected, got {result:?}"
                );
            }
        }
    }
}

#[test]
fn d3d9_executor_format_mapping_covers_all_protocol_formats() {
    use crate::aerogpu_d3d9_executor::map_aerogpu_format;

    for &format in ALL_PROTOCOL_FORMATS {
        let mapped = map_aerogpu_format(format as u32);

        match expected_protocol_support(format) {
            ExpectedSupport::Supported => {
                mapped.unwrap_or_else(|err| {
                    panic!(
                        "D3D9 executor should map protocol format {format:?} ({}), got error: {err:?}",
                        format as u32
                    )
                });
            }
            ExpectedSupport::Unsupported => {
                assert!(
                    matches!(mapped, Err(crate::AerogpuD3d9Error::UnsupportedFormat(_))),
                    "expected unsupported format {format:?} to be rejected, got {mapped:?}"
                );
            }
        }
    }
}
