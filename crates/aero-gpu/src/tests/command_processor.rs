use crate::{AeroGpuCommandProcessor, AeroGpuOpcode, CommandProcessorError, AEROGPU_CMD_STREAM_MAGIC};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
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
    assert_eq!(size_bytes % 4, 0);
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
            push_u32(out, 28); // format (opaque numeric)
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
            push_u32(out, 28);
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

