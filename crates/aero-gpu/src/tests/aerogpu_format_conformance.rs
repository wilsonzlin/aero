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

fn expected_tight_row_pitch_bytes(format: AerogpuFormat, width: u32) -> u32 {
    match format {
        AerogpuFormat::Invalid => 1,

        AerogpuFormat::B8G8R8A8Unorm
        | AerogpuFormat::B8G8R8X8Unorm
        | AerogpuFormat::R8G8B8A8Unorm
        | AerogpuFormat::R8G8B8X8Unorm
        | AerogpuFormat::B8G8R8A8UnormSrgb
        | AerogpuFormat::B8G8R8X8UnormSrgb
        | AerogpuFormat::R8G8B8A8UnormSrgb
        | AerogpuFormat::R8G8B8X8UnormSrgb
        | AerogpuFormat::D24UnormS8Uint
        | AerogpuFormat::D32Float => width * 4,

        AerogpuFormat::B5G6R5Unorm | AerogpuFormat::B5G5R5A1Unorm => width * 2,

        AerogpuFormat::BC1RgbaUnorm | AerogpuFormat::BC1RgbaUnormSrgb => {
            let blocks_x = width.div_ceil(4);
            blocks_x * 8
        }
        AerogpuFormat::BC2RgbaUnorm
        | AerogpuFormat::BC2RgbaUnormSrgb
        | AerogpuFormat::BC3RgbaUnorm
        | AerogpuFormat::BC3RgbaUnormSrgb
        | AerogpuFormat::BC7RgbaUnorm
        | AerogpuFormat::BC7RgbaUnormSrgb => {
            let blocks_x = width.div_ceil(4);
            blocks_x * 16
        }
    }
}

fn expected_rows_in_layout(format: AerogpuFormat, height: u32) -> u32 {
    match format {
        AerogpuFormat::Invalid => 1,

        AerogpuFormat::B8G8R8A8Unorm
        | AerogpuFormat::B8G8R8X8Unorm
        | AerogpuFormat::R8G8B8A8Unorm
        | AerogpuFormat::R8G8B8X8Unorm
        | AerogpuFormat::B5G6R5Unorm
        | AerogpuFormat::B5G5R5A1Unorm
        | AerogpuFormat::B8G8R8A8UnormSrgb
        | AerogpuFormat::B8G8R8X8UnormSrgb
        | AerogpuFormat::R8G8B8A8UnormSrgb
        | AerogpuFormat::R8G8B8X8UnormSrgb
        | AerogpuFormat::D24UnormS8Uint
        | AerogpuFormat::D32Float => height,

        AerogpuFormat::BC1RgbaUnorm
        | AerogpuFormat::BC1RgbaUnormSrgb
        | AerogpuFormat::BC2RgbaUnorm
        | AerogpuFormat::BC2RgbaUnormSrgb
        | AerogpuFormat::BC3RgbaUnorm
        | AerogpuFormat::BC3RgbaUnormSrgb
        | AerogpuFormat::BC7RgbaUnorm
        | AerogpuFormat::BC7RgbaUnormSrgb => height.div_ceil(4),
    }
}

fn expected_d3d9_wgpu_format(format: AerogpuFormat) -> Option<wgpu::TextureFormat> {
    Some(match format {
        AerogpuFormat::Invalid => return None,

        AerogpuFormat::B8G8R8A8Unorm
        | AerogpuFormat::B8G8R8X8Unorm
        | AerogpuFormat::B8G8R8A8UnormSrgb
        | AerogpuFormat::B8G8R8X8UnormSrgb => wgpu::TextureFormat::Bgra8Unorm,

        AerogpuFormat::R8G8B8A8Unorm
        | AerogpuFormat::R8G8B8X8Unorm
        | AerogpuFormat::R8G8B8A8UnormSrgb
        | AerogpuFormat::R8G8B8X8UnormSrgb => wgpu::TextureFormat::Rgba8Unorm,

        // Packed 16-bit formats are converted to RGBA8 in the D3D9 executor.
        AerogpuFormat::B5G6R5Unorm | AerogpuFormat::B5G5R5A1Unorm => {
            wgpu::TextureFormat::Rgba8Unorm
        }

        AerogpuFormat::D24UnormS8Uint => wgpu::TextureFormat::Depth24PlusStencil8,
        AerogpuFormat::D32Float => wgpu::TextureFormat::Depth32Float,

        AerogpuFormat::BC1RgbaUnorm | AerogpuFormat::BC1RgbaUnormSrgb => {
            wgpu::TextureFormat::Bc1RgbaUnorm
        }
        AerogpuFormat::BC2RgbaUnorm | AerogpuFormat::BC2RgbaUnormSrgb => {
            wgpu::TextureFormat::Bc2RgbaUnorm
        }
        AerogpuFormat::BC3RgbaUnorm | AerogpuFormat::BC3RgbaUnormSrgb => {
            wgpu::TextureFormat::Bc3RgbaUnorm
        }
        AerogpuFormat::BC7RgbaUnorm | AerogpuFormat::BC7RgbaUnormSrgb => {
            wgpu::TextureFormat::Bc7RgbaUnorm
        }
    })
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
        let result =
            proc.process_submission_with_allocations(&bytes, None, /*signal_fence=*/ 1);

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
                    matches!(
                        result,
                        Err(crate::CommandProcessorError::InvalidCreateTexture2d)
                    ),
                    "expected unsupported format {format:?} to be rejected, got {result:?}"
                );
            }
        }
    }
}

#[test]
fn command_processor_guest_backed_texture_layout_matches_protocol_formats() {
    use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

    // Use a minimal guest-backed texture that exercises `TextureFormatLayout` sizing:
    // - width/height are chosen so BC formats have a single block row/column.
    // - `row_pitch_bytes` is set to the *minimum* tight pitch so mistakes in per-format layout
    //   computation are likely to trigger an out-of-bounds rejection.
    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 4;

    const HANDLE: u32 = 1;
    const ALLOC_ID: u32 = 1;

    for &format in ALL_PROTOCOL_FORMATS {
        let expected_row_pitch = expected_tight_row_pitch_bytes(format, WIDTH);
        let expected_rows = expected_rows_in_layout(format, HEIGHT);
        let expected_size_bytes = u64::from(expected_row_pitch) * u64::from(expected_rows);

        let allocs = [crate::AeroGpuSubmissionAllocation {
            alloc_id: ALLOC_ID,
            gpa: 0x1000,
            size_bytes: expected_size_bytes,
        }];

        let mut w = AerogpuCmdWriter::new();
        w.create_texture2d(
            HANDLE,
            /*usage_flags=*/ 0,
            format as u32,
            WIDTH,
            HEIGHT,
            /*mip_levels=*/ 1,
            /*array_layers=*/ 1,
            /*row_pitch_bytes=*/ expected_row_pitch,
            /*backing_alloc_id=*/ ALLOC_ID,
            /*backing_offset_bytes=*/ 0,
        );
        let bytes = w.finish();

        let mut proc = crate::AeroGpuCommandProcessor::new();
        let result = proc.process_submission_with_allocations(
            &bytes,
            Some(&allocs),
            /*signal_fence=*/ 1,
        );

        match expected_protocol_support(format) {
            ExpectedSupport::Supported => {
                result.unwrap_or_else(|err| {
                    panic!(
                        "guest-backed CREATE_TEXTURE2D should accept protocol format {format:?} ({}), got error: {err:?}",
                        format as u32
                    )
                });
            }
            ExpectedSupport::Unsupported => {
                assert!(
                    matches!(
                        result,
                        Err(crate::CommandProcessorError::InvalidCreateTexture2d)
                    ),
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
                let got = mapped.unwrap_or_else(|err| {
                    panic!(
                        "D3D9 executor should map protocol format {format:?} ({}), got error: {err:?}",
                        format as u32
                    )
                });

                let expected = expected_d3d9_wgpu_format(format).unwrap_or_else(|| {
                    panic!(
                        "test bug: format {format:?} is marked supported but has no expected D3D9 mapping"
                    )
                });
                assert_eq!(
                    got, expected,
                    "unexpected D3D9 wgpu format mapping for {format:?}"
                );
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
