use aero_gpu::{parse_cmd_stream, AeroGpuCommandProcessor, AeroGpuEvent};

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, 0x444D_4341); // "ACMD"
    push_u32(&mut out, 0x0001_0000); // abi_version (major=1 minor=0)
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
    out[start + 4..start + 8].copy_from_slice(&size_bytes.to_le_bytes());
}

#[test]
fn present_ex_and_shared_surfaces_update_state_and_emit_events() {
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let stream = build_stream(|out| {
        // Unknown packet should be skipped by the processor.
        emit_packet(out, 0xDEAD_BEEF, |out| {
            push_u32(out, 0xAABB_CCDD);
        });

        // Export an existing surface handle.
        emit_packet(out, 0x710, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        // Import the surface under a new handle.
        emit_packet(out, 0x711, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        // PresentEx on scanout 0.
        emit_packet(out, 0x701, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 1); // flags (AEROGPU_PRESENT_FLAG_VSYNC)
            push_u32(out, 0x1); // d3d9_present_flags (D3DPRESENT_DONOTWAIT)
            push_u32(out, 0); // reserved0
        });
    });

    let parsed = parse_cmd_stream(&stream).expect("parse should succeed");
    assert_eq!(parsed.cmds.len(), 4);

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
        emit_packet(out, 0x711, |out| {
            push_u32(out, 0x20); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1234); // share_token
        });
    });

    let mut processor = AeroGpuCommandProcessor::new();
    let err = processor.process_submission(&stream, 1).unwrap_err();
    assert!(err.to_string().contains("unknown shared surface token"));
}
