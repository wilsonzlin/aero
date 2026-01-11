use aero_gpu::{parse_cmd_stream, AeroGpuCommandProcessor, AeroGpuEvent, CommandProcessorError};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    },
    aerogpu_pci::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn build_stream_with_abi(abi_version: u32, packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, abi_version);
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

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    build_stream_with_abi(AEROGPU_ABI_VERSION_U32, packets)
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn emit_create_texture_rgba8(out: &mut Vec<u8>, texture_handle: u32) {
    emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
        push_u32(out, texture_handle); // texture_handle
        push_u32(out, 0x0); // usage_flags
        push_u32(out, 3); // format (AEROGPU_FORMAT_R8G8B8A8_UNORM)
        push_u32(out, 1); // width
        push_u32(out, 1); // height
        push_u32(out, 1); // mip_levels
        push_u32(out, 1); // array_layers
        push_u32(out, 4); // row_pitch_bytes
        push_u32(out, 0); // backing_alloc_id
        push_u32(out, 0); // backing_offset_bytes
        push_u64(out, 0); // reserved0
    });
}

#[test]
fn present_ex_and_shared_surfaces_update_state_and_emit_events() {
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let stream = build_stream(|out| {
        // Unknown packet should be skipped by the processor.
        emit_packet(out, 0xDEAD_BEEF, |out| {
            push_u32(out, 0xAABB_CCDD);
        });

        // Create a texture so the command processor can track its lifetime.
        emit_create_texture_rgba8(out, 0x10);

        // Export an existing surface handle.
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        // Import the surface under a new handle.
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        // PresentEx on scanout 0.
        emit_packet(out, AerogpuCmdOpcode::PresentEx as u32, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 1); // flags (AEROGPU_PRESENT_FLAG_VSYNC)
            push_u32(out, 0x1); // d3d9_present_flags (D3DPRESENT_DONOTWAIT)
            push_u32(out, 0); // reserved0
        });
    });

    let parsed = parse_cmd_stream(&stream).expect("parse should succeed");
    assert_eq!(parsed.cmds.len(), 5);

    let mut processor = AeroGpuCommandProcessor::new();
    let events = processor
        .process_submission(&stream, 7)
        .expect("process_submission should succeed");

    assert_eq!(processor.completed_fence(), 7);
    assert_eq!(processor.present_count(), 1);

    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));
    assert_eq!(processor.resolve_shared_surface(0x20), 0x10);

    assert_eq!(
        events,
        vec![
            AeroGpuEvent::PresentCompleted {
                scanout_id: 0,
                present_count: 1
            },
            AeroGpuEvent::FenceSignaled { fence: 7 }
        ]
    );
}

#[test]
fn importing_unknown_token_is_an_error() {
    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1234); // share_token
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    let err = processor.process_submission(&stream, 1).unwrap_err();
    assert!(err.to_string().contains("unknown shared surface token"));
}

#[test]
fn exporting_the_same_token_twice_is_idempotent() {
    const TOKEN: u64 = 0x1111_2222_3333_4444;

    let stream = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&stream, 1)
        .expect("duplicate export should be idempotent");
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));
}

#[test]
fn exporting_same_token_for_different_resources_is_an_error() {
    const TOKEN: u64 = 0x9999_AAAA_BBBB_CCCC;

    let stream = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);
        emit_create_texture_rgba8(out, 0x11);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x11);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    let err = processor.process_submission(&stream, 1).unwrap_err();
    assert!(err.to_string().contains("already exported"));
}

#[test]
fn importing_into_an_existing_alias_is_idempotent_for_the_same_original() {
    const TOKEN: u64 = 0xABC0_DEF0_0000_0001;

    let stream = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&stream, 1)
        .expect("duplicate import should be idempotent");
    assert_eq!(processor.resolve_shared_surface(0x20), 0x10);
}

#[test]
fn importing_into_an_existing_alias_for_different_original_is_an_error() {
    const TOKEN_A: u64 = 0xABC0_DEF0_0000_0002;
    const TOKEN_B: u64 = 0xABC0_DEF0_0000_0003;

    let stream = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);
        emit_create_texture_rgba8(out, 0x11);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN_A);
        });
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x11);
            push_u32(out, 0);
            push_u64(out, TOKEN_B);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN_A);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN_B);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    let err = processor.process_submission(&stream, 1).unwrap_err();
    assert!(err.to_string().contains("already bound"));
}

#[test]
fn shared_surface_aliases_keep_resources_alive_until_last_handle_is_destroyed() {
    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0001;

    // Submission 1: create, export, import alias, then destroy original.
    let submit1 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x10); // DestroyResource(original)
            push_u32(out, 0);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&submit1, 1)
        .expect("submission 1 should succeed");

    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));
    assert_eq!(processor.resolve_shared_surface(0x20), 0x10);

    // Submission 2: import another alias (should still work), then destroy both aliases.
    let submit2 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x21); // out_resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x20); // DestroyResource(alias)
            push_u32(out, 0);
        });

        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x21); // DestroyResource(alias)
            push_u32(out, 0);
        });
    });

    processor
        .process_submission(&submit2, 2)
        .expect("submission 2 should succeed");

    // Underlying surface is now fully destroyed; token mapping should be removed.
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), None);

    // Submission 3: importing the token again should fail validation.
    let submit3 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x22); // out_resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let err = processor.process_submission(&submit3, 3).unwrap_err();
    assert!(err.to_string().contains("unknown shared surface token"));
}

#[test]
fn export_in_one_submission_import_in_next_submission_succeeds() {
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let submit1 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });
    let submit2 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&submit1, 1)
        .expect("submission 1 should succeed");
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));

    processor
        .process_submission(&submit2, 2)
        .expect("submission 2 should succeed");
    assert_eq!(processor.resolve_shared_surface(0x20), 0x10);
}

#[test]
fn destroy_alias_removes_mapping_but_token_remains_importable() {
    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0123;

    let submit1 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let submit2 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x20); // DestroyResource(alias)
            push_u32(out, 0);
        });
    });

    let submit3 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x30);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&submit1, 1)
        .expect("submission 1 should succeed");
    assert_eq!(processor.resolve_shared_surface(0x20), 0x10);

    processor
        .process_submission(&submit2, 2)
        .expect("submission 2 should succeed");

    // Destroying an alias handle removes only the alias mapping.
    assert_eq!(processor.resolve_shared_surface(0x20), 0x20);
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));

    // The exported token should remain importable even after the alias is destroyed.
    processor
        .process_submission(&submit3, 3)
        .expect("submission 3 should succeed");
    assert_eq!(processor.resolve_shared_surface(0x30), 0x10);
}

#[test]
fn destroy_last_handle_invalidates_token_and_rejects_future_imports() {
    const TOKEN: u64 = 0xAABB_CCDD_EEFF_4567;

    let submit1 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);

        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x20);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let submit2 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x20); // DestroyResource(alias)
            push_u32(out, 0);
        });
    });

    let submit3 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x10); // DestroyResource(original)
            push_u32(out, 0);
        });
    });

    let submit4 = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0x30);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&submit1, 1)
        .expect("submission 1 should succeed");
    processor
        .process_submission(&submit2, 2)
        .expect("submission 2 should succeed");

    // Destroying the final handle should remove the token mapping.
    processor
        .process_submission(&submit3, 3)
        .expect("submission 3 should succeed");
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), None);

    let err = processor.process_submission(&submit4, 4).unwrap_err();
    assert!(matches!(err, CommandProcessorError::UnknownShareToken(t) if t == TOKEN));
}

#[test]
fn exporting_same_token_for_different_resources_across_submissions_is_an_error() {
    const TOKEN: u64 = 0xDEAD_BEEF_0000_0001;

    let submit1 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x10);
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x10);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });
    let submit2 = build_stream(|out| {
        emit_create_texture_rgba8(out, 0x11);
        emit_packet(out, AerogpuCmdOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0x11);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    processor
        .process_submission(&submit1, 1)
        .expect("submission 1 should succeed");

    let err = processor.process_submission(&submit2, 2).unwrap_err();
    assert!(matches!(
        err,
        CommandProcessorError::ShareTokenAlreadyExported {
            share_token,
            existing,
            new
        } if share_token == TOKEN && existing == 0x10 && new == 0x11
    ));

    // Collisions should not perturb the existing mapping.
    assert_eq!(processor.lookup_shared_surface_token(TOKEN), Some(0x10));
}

#[test]
fn rejects_unsupported_abi_major() {
    let stream = build_stream_with_abi((AEROGPU_ABI_MAJOR + 1) << 16, |_| {});
    let err = parse_cmd_stream(&stream).unwrap_err();
    assert!(matches!(
        err,
        aero_gpu::AeroGpuCmdStreamParseError::UnsupportedAbiMajor { found }
            if found == (AEROGPU_ABI_MAJOR as u16 + 1)
    ));
}

#[test]
fn accepts_unknown_abi_minor() {
    let stream = build_stream_with_abi((AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1), |_| {});
    parse_cmd_stream(&stream).expect("unknown minor versions should be accepted");
}
