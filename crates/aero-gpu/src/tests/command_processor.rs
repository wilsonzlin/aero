use crate::{
    AeroGpuCommandProcessor, AeroGpuOpcode, AeroGpuSubmissionAllocation, CommandProcessorError,
    AEROGPU_CMD_STREAM_MAGIC,
};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn alloc_entry(alloc_id: u32, size_bytes: u64) -> AeroGpuSubmissionAllocation {
    AeroGpuSubmissionAllocation {
        alloc_id,
        gpa: 0x1000,
        size_bytes,
    }
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    pad4(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert!(size_bytes.is_multiple_of(4));
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

#[test]
fn command_processor_rejects_reusing_handle_with_different_buffer_desc() {
    // If protocol handle allocation regresses (e.g. collisions across processes/APIs),
    // the host-side processor should deterministically reject attempts to create a
    // different resource under an existing handle.
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10); // buffer_handle
            push_u32(out, 0x3); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Same handle, but different size => immutable descriptor mismatch.
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0x3);
            push_u64(out, 32);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::CreateRebindMismatch {
            resource_handle: 0x10
        }
    ));
}

#[test]
fn command_processor_rejects_reusing_handle_with_different_texture_desc() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x20); // texture_handle
            push_u32(out, 0x4); // usage_flags
            push_u32(out, 3); // format (AEROGPU_FORMAT_R8G8B8A8_UNORM)
            push_u32(out, 64); // width
            push_u32(out, 64); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 256); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Same handle, but different width => immutable descriptor mismatch.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0x4);
            push_u32(out, 3);
            push_u32(out, 128);
            push_u32(out, 64);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 512);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::CreateRebindMismatch {
            resource_handle: 0x20
        }
    ));
}

#[test]
fn command_processor_rejects_invalid_create_buffer_alignment() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10); // buffer_handle
            push_u32(out, 0); // usage_flags
            push_u64(out, 3); // size_bytes (not COPY_BUFFER_ALIGNMENT aligned)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(err, CommandProcessorError::InvalidCreateBuffer));
}

#[test]
fn command_processor_accepts_srgb_create_texture2d_formats() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        for &(handle, format) in &[
            (0x30, AerogpuFormat::B8G8R8A8UnormSrgb as u32),
            (0x31, AerogpuFormat::B8G8R8X8UnormSrgb as u32),
            (0x32, AerogpuFormat::R8G8B8A8UnormSrgb as u32),
            (0x33, AerogpuFormat::R8G8B8X8UnormSrgb as u32),
        ] {
            emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, handle); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, format); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (host-owned => tight)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }
    });

    proc.process_submission(&stream, 0)
        .expect("sRGB CREATE_TEXTURE2D formats should be accepted");
}

#[test]
fn command_processor_accepts_bc_create_texture2d_formats() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        for &(handle, format) in &[
            (0x40, AerogpuFormat::BC1RgbaUnorm as u32),
            (0x41, AerogpuFormat::BC1RgbaUnormSrgb as u32),
            (0x42, AerogpuFormat::BC2RgbaUnorm as u32),
            (0x43, AerogpuFormat::BC2RgbaUnormSrgb as u32),
            (0x44, AerogpuFormat::BC3RgbaUnorm as u32),
            (0x45, AerogpuFormat::BC3RgbaUnormSrgb as u32),
            (0x46, AerogpuFormat::BC7RgbaUnorm as u32),
            (0x47, AerogpuFormat::BC7RgbaUnormSrgb as u32),
        ] {
            emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, handle); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, format); // format
                push_u32(out, 4); // width (one BC block)
                push_u32(out, 4); // height (one BC block)
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (host-owned => tight)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }
    });

    proc.process_submission(&stream, 0)
        .expect("BC CREATE_TEXTURE2D formats should be accepted");
}

#[test]
fn command_processor_does_not_register_shared_surface_on_failed_create_texture2d() {
    let mut proc = AeroGpuCommandProcessor::new();

    let create_stream = build_stream(|out| {
        // Guest-backed texture with row_pitch_bytes=0 is invalid; the processor should reject the
        // command without registering the handle in the shared-surface tables.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format (opaque numeric)
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (invalid when guest-backed)
            push_u32(out, 1); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&create_stream, 0).unwrap_err();
    assert!(matches!(err, CommandProcessorError::InvalidCreateTexture2d));

    let export_stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
    });

    let err = proc.process_submission(&export_stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::UnknownSharedSurfaceHandle(0x10)
    ));
}

#[test]
fn command_processor_rejects_creating_texture_under_shared_surface_alias_handle() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        // Create + export + import shared surface.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Attempt to create a new texture using the alias handle.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x20); // texture_handle (alias)
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::SharedSurfaceHandleInUse(0x20)
    ));
}

#[test]
fn command_processor_rejects_creating_buffer_under_shared_surface_alias_handle() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        // Create + export + import shared surface.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Attempt to create a new buffer using the alias handle.
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x20); // buffer_handle (alias)
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::SharedSurfaceHandleInUse(0x20)
    ));
}

#[test]
fn command_processor_rejects_creating_buffer_under_shared_surface_underlying_handle() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        // Create + export + import shared surface.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Destroy the original handle but keep the resource alive via the alias.
        emit_packet(out, AeroGpuOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
        });

        // Attempt to create a new buffer using the underlying (now free-looking) handle.
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10); // buffer_handle (underlying id)
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::SharedSurfaceHandleInUse(0x10)
    ));
}

#[test]
fn command_processor_reports_handle_collision_before_missing_alloc_table() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Guest-backed create (backing_alloc_id != 0) would normally require an allocation table.
        // We should still report the handle collision first.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x20); // texture_handle (alias)
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 1); // backing_alloc_id (allocation table is missing)
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::SharedSurfaceHandleInUse(0x20)
    ));
}

#[test]
fn command_processor_rejects_import_into_existing_buffer_handle() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        // Buffer occupying handle 0x10.
        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10); // buffer_handle
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Create + export shared surface under a different handle.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x20); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format (opaque numeric)
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Attempt to import into the existing buffer handle.
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // out_resource_handle (collides with buffer)
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::SharedSurfaceHandleInUse(0x10)
    ));
}

#[test]
fn command_processor_release_shared_surface_unknown_token_is_noop() {
    let mut proc = AeroGpuCommandProcessor::new();

    const TEX: u32 = 0x10;
    const ALIAS: u32 = 0x20;
    const TOKEN: u64 = 0xDEAD_BEEF_CAFE_F00D;

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, TEX); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format (opaque numeric)
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Token has not been exported yet; this should not retire it.
        emit_packet(out, AeroGpuOpcode::ReleaseSharedSurface as u32, |out| {
            push_u64(out, TOKEN);
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, TEX); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, ALIAS); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });
    });

    proc.process_submission(&stream, 0)
        .expect("release-before-export must not retire the token");

    assert_eq!(proc.lookup_shared_surface_token(TOKEN), Some(TEX));
    assert_eq!(proc.resolve_shared_surface(ALIAS), TEX);
}

#[test]
fn command_processor_rejects_reexporting_token_after_release() {
    let mut proc = AeroGpuCommandProcessor::new();

    const TEX_A: u32 = 0x10;
    const TEX_B: u32 = 0x11;
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, TEX_A); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format (opaque numeric)
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, TEX_A); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });
        emit_packet(out, AeroGpuOpcode::ReleaseSharedSurface as u32, |out| {
            push_u64(out, TOKEN);
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, TEX_B); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, TEX_B); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(err, CommandProcessorError::ShareTokenRetired(t) if t == TOKEN));
}

#[test]
fn command_processor_rejects_dirty_range_for_destroyed_shared_surface_handle() {
    let mut proc = AeroGpuCommandProcessor::new();

    let stream = build_stream(|out| {
        // Create + export + import shared surface.
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x10); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });
        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        // Destroy the original handle while keeping the underlying resource alive via alias.
        emit_packet(out, AeroGpuOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
        });

        // Dirty range should not be accepted for the destroyed handle, even though the underlying
        // resource entry is still alive.
        emit_packet(out, AeroGpuOpcode::ResourceDirtyRange as u32, |out| {
            push_u32(out, 0x10); // resource_handle (destroyed)
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, 4); // size_bytes
        });
    });

    let err = proc.process_submission(&stream, 0).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::UnknownResourceHandle(0x10)
    ));
}

#[test]
fn command_processor_accepts_non_power_of_two_mip_chain_with_exact_backing_size() {
    const TEX: u32 = 0x10;
    const ALLOC_ID: u32 = 1;

    let mut proc = AeroGpuCommandProcessor::new();

    // RGBA8 3x3 with 2 mips:
    // - mip0: row_pitch=12, rows=3 => 36 bytes
    // - mip1: 1x1, tight row_pitch=4 => 4 bytes
    // Total: 40 bytes.
    let create_stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, TEX); // texture_handle
            push_u32(out, 0); // usage_flags
            push_u32(out, 3); // format (AEROGPU_FORMAT_R8G8B8A8_UNORM)
            push_u32(out, 3); // width
            push_u32(out, 3); // height
            push_u32(out, 2); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 12); // row_pitch_bytes (mip0)
            push_u32(out, ALLOC_ID); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    let allocs = [alloc_entry(ALLOC_ID, /*size_bytes=*/ 40)];
    proc.process_submission_with_allocations(&create_stream, Some(&allocs), 0)
        .expect("CREATE_TEXTURE2D should accept exact-sized backing allocation");

    // Touch bytes in mip1 (immediately after mip0).
    let dirty_stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::ResourceDirtyRange as u32, |out| {
            push_u32(out, TEX); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 36); // offset_bytes (start of mip1)
            push_u64(out, 4); // size_bytes
        });
    });

    proc.process_submission_with_allocations(&dirty_stream, Some(&allocs), 0)
        .expect("dirty range into mip1 must be in-bounds");
}
