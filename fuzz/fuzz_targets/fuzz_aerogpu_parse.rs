#![no_main]

use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations inside command-stream / alloc-table
/// decoding paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Non-zero guest physical address to place the candidate alloc-table bytes at.
const ALLOC_TABLE_GPA: u64 = 0x1000;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Split the raw fuzzer input into independent byte slices so libFuzzer can make progress on
    // multiple parsers that each expect different "magic" prefixes (e.g. "ACMD" vs "ALOC").
    let mut u = Unstructured::new(data);

    let cmd_len = u.arbitrary::<u16>().unwrap_or(0) as usize;
    let cmd_len = cmd_len.min(u.len());
    let cmd_bytes = u.bytes(cmd_len).unwrap_or(&[]);

    let alloc_len = u.arbitrary::<u16>().unwrap_or(0) as usize;
    let alloc_len = alloc_len.min(u.len());
    let alloc_bytes = u.bytes(alloc_len).unwrap_or(&[]);

    let ring_bytes = u.take_rest();

    // Treat the command-stream slice as a candidate command stream. All errors are acceptable.
    let _ = aero_gpu::parse_cmd_stream(cmd_bytes);

    // Also exercise the canonical protocol-level command stream parser + typed packet decoders.
    //
    // This avoids wgpu device creation and stays in pure parsing/bounds-checking code.
    let _ = cmd::decode_cmd_stream_header_le(cmd_bytes);
    if let Ok(iter) = cmd::AerogpuCmdStreamIter::new(cmd_bytes) {
        for pkt in iter.take(1024) {
            let Ok(pkt) = pkt else { continue };
            match pkt.opcode {
                Some(cmd::AerogpuCmdOpcode::CreateShaderDxbc) => {
                    let _ = pkt.decode_create_shader_dxbc_payload_le();
                }
                Some(cmd::AerogpuCmdOpcode::UploadResource) => {
                    let _ = pkt.decode_upload_resource_payload_le();
                }
                Some(cmd::AerogpuCmdOpcode::CreateInputLayout) => {
                    let _ = pkt.decode_create_input_layout_payload_le();
                }
                Some(cmd::AerogpuCmdOpcode::SetVertexBuffers) => {
                    let _ = pkt.decode_set_vertex_buffers_payload_le();
                }
                Some(cmd::AerogpuCmdOpcode::SetSamplers) => {
                    let _ = pkt.decode_set_samplers_payload_le();
                }
                Some(cmd::AerogpuCmdOpcode::SetConstantBuffers) => {
                    let _ = pkt.decode_set_constant_buffers_payload_le();
                }
                _ => {}
            }
        }
    }

    // If the input is large enough, also try alloc-table decoding via both the canonical protocol
    // decoder and the executor's GuestMemory-backed decoder.
    if alloc_bytes.len() < ring::AerogpuAllocTableHeader::SIZE_BYTES {
        return;
    }

    // Header decode/validation (canonical helpers).
    let declared_size_bytes = ring::AerogpuAllocTableHeader::decode_from_le_bytes(alloc_bytes)
        .ok()
        .and_then(|header| header.validate_prefix().ok().map(|_| header.size_bytes))
        .unwrap_or(0);

    // Whole-table decode (canonical helpers).
    let _ = ring::decode_alloc_table_le(alloc_bytes);

    // Ring + submission layouts are adjacent to alloc-table parsing in the protocol crate; decode
    // them too to harden their bounds checks.
    if let Ok(hdr) = ring::AerogpuRingHeader::decode_from_le_bytes(ring_bytes) {
        let _ = hdr.validate_prefix();
    }
    if let Ok(desc) = ring::AerogpuSubmitDesc::decode_from_le_bytes(ring_bytes) {
        let _ = desc.validate_prefix();
    }
    if let Ok(page) = ring::AerogpuFencePage::decode_from_le_bytes(ring_bytes) {
        let _ = page.validate_prefix();
    }

    // Executor decode (GuestMemory-backed). Use the header's declared size when it is both
    // well-formed and within our input cap; otherwise, fall back to the provided buffer length.
    let table_size_bytes = match u32::try_from(alloc_bytes.len()) {
        Ok(len) => match declared_size_bytes {
            0 => len,
            declared
                if declared as usize <= alloc_bytes.len()
                    && declared as usize <= MAX_INPUT_SIZE_BYTES =>
            {
                declared
            }
            _ => len,
        },
        Err(_) => return,
    };

    let guest_mem_size = (ALLOC_TABLE_GPA as usize).saturating_add(table_size_bytes as usize);
    if guest_mem_size > (ALLOC_TABLE_GPA as usize).saturating_add(MAX_INPUT_SIZE_BYTES) {
        return;
    }

    let mut guest = VecGuestMemory::new(guest_mem_size);
    let _ = guest.write(ALLOC_TABLE_GPA, &alloc_bytes[..table_size_bytes as usize]);
    let _ = AllocTable::decode_from_guest_memory(&mut guest, ALLOC_TABLE_GPA, table_size_bytes);
});
