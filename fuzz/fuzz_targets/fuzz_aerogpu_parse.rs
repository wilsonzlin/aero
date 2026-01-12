#![no_main]

use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations inside command-stream / alloc-table
/// decoding paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Non-zero guest physical address to place the candidate alloc-table bytes at.
const ALLOC_TABLE_GPA: u64 = 0x1000;

fn fuzz_cmd_stream(cmd_bytes: &[u8]) {
    // Treat the slice as a candidate command stream. All errors are acceptable.
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
                Some(cmd::AerogpuCmdOpcode::SetShaderConstantsF) => {
                    // The shader constants decoder lives as a free function that expects the whole
                    // packet bytes. Reconstruct the packet slice without allocating by finding the
                    // payload's offset into the original stream.
                    let base = cmd_bytes.as_ptr() as usize;
                    let payload = pkt.payload.as_ptr() as usize;
                    let Some(hdr_off) = payload
                        .checked_sub(base)
                        .and_then(|o| o.checked_sub(cmd::AerogpuCmdHdr::SIZE_BYTES))
                    else {
                        continue;
                    };
                    let packet_len = pkt.hdr.size_bytes as usize;
                    let Some(packet_end) = hdr_off.checked_add(packet_len) else {
                        continue;
                    };
                    let Some(packet_bytes) = cmd_bytes.get(hdr_off..packet_end) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_set_shader_constants_f_payload_le(packet_bytes);
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
}

fn fuzz_alloc_table(alloc_bytes: &[u8]) {
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
}

fn fuzz_ring_layouts(ring_bytes: &[u8]) {
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
}

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

    fuzz_cmd_stream(cmd_bytes);
    fuzz_alloc_table(alloc_bytes);
    fuzz_ring_layouts(ring_bytes);

    // Additionally, try patching the fixed headers to valid magic/version values (while keeping
    // the rest of the input intact) so the fuzzer can reach deeper parsing paths more often.
    //
    // This is especially useful because the different AeroGPU blobs all expect different magic
    // prefixes at offset 0, which can otherwise fight each other in a single corpus.

    // Patched command stream: force valid magic/version and size_bytes.
    let mut cmd_patched = cmd_bytes.to_vec();
    let cmd_min_len = cmd::AerogpuCmdStreamHeader::SIZE_BYTES + cmd::AerogpuCmdHdr::SIZE_BYTES;
    if cmd_patched.len() < cmd_min_len {
        cmd_patched.resize(cmd_min_len, 0);
    }
    cmd_patched[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_patched[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    let cmd_size_bytes = cmd_patched.len() as u32;
    cmd_patched[8..12].copy_from_slice(&cmd_size_bytes.to_le_bytes());
    // Force a well-formed first packet (NOP) so the iterator can reach later packets more often.
    let cmd_hdr_off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    cmd_patched[cmd_hdr_off..cmd_hdr_off + 4].copy_from_slice(&0u32.to_le_bytes());
    cmd_patched[cmd_hdr_off + 4..cmd_hdr_off + 8]
        .copy_from_slice(&(cmd::AerogpuCmdHdr::SIZE_BYTES as u32).to_le_bytes());
    fuzz_cmd_stream(&cmd_patched);

    // Patched alloc table: force valid magic/version/stride and a self-consistent entry_count.
    let mut alloc_patched = alloc_bytes.to_vec();
    if alloc_patched.len() < ring::AerogpuAllocTableHeader::SIZE_BYTES {
        alloc_patched.resize(ring::AerogpuAllocTableHeader::SIZE_BYTES, 0);
    }
    let header_size = ring::AerogpuAllocTableHeader::SIZE_BYTES;
    let stride = ring::AerogpuAllocEntry::SIZE_BYTES;
    let entry_count = (alloc_patched.len().saturating_sub(header_size) / stride) as u32;
    alloc_patched[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_patched[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    let alloc_size_bytes = alloc_patched.len() as u32;
    alloc_patched[8..12].copy_from_slice(&alloc_size_bytes.to_le_bytes());
    alloc_patched[12..16].copy_from_slice(&entry_count.to_le_bytes());
    alloc_patched[16..20].copy_from_slice(&(stride as u32).to_le_bytes());
    fuzz_alloc_table(&alloc_patched);

    // Patched alloc table (minimal): force a single well-formed entry so the executor alloc-table
    // validation path can succeed more often.
    let mut alloc_one = vec![0u8; header_size + stride];
    let alloc_one_copy_len = alloc_one.len().min(alloc_bytes.len());
    alloc_one[..alloc_one_copy_len].copy_from_slice(&alloc_bytes[..alloc_one_copy_len]);
    alloc_one[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_one[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    let alloc_one_size_bytes = alloc_one.len() as u32;
    alloc_one[8..12].copy_from_slice(&alloc_one_size_bytes.to_le_bytes());
    alloc_one[12..16].copy_from_slice(&1u32.to_le_bytes());
    alloc_one[16..20].copy_from_slice(&(stride as u32).to_le_bytes());
    alloc_one[20..24].fill(0);
    let entry_off = header_size;
    alloc_one[entry_off..entry_off + 4].copy_from_slice(&1u32.to_le_bytes()); // alloc_id
    alloc_one[entry_off + 4..entry_off + 8].fill(0); // flags
    alloc_one[entry_off + 8..entry_off + 16].copy_from_slice(&0x2000u64.to_le_bytes()); // gpa
    alloc_one[entry_off + 16..entry_off + 24].copy_from_slice(&0x1000u64.to_le_bytes()); // size_bytes
    alloc_one[entry_off + 24..entry_off + 32].fill(0); // reserved0
    fuzz_alloc_table(&alloc_one);

    // Patched ring layouts: create fixed-size prefix buffers so we can set magic/version and hit
    // deeper validation checks without copying the entire (potentially large) ring slice.
    let mut ring_hdr_bytes = [0u8; ring::AerogpuRingHeader::SIZE_BYTES];
    let ring_hdr_copy_len = ring_hdr_bytes.len().min(ring_bytes.len());
    ring_hdr_bytes[..ring_hdr_copy_len].copy_from_slice(&ring_bytes[..ring_hdr_copy_len]);
    ring_hdr_bytes[0..4].copy_from_slice(&ring::AEROGPU_RING_MAGIC.to_le_bytes());
    ring_hdr_bytes[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    ring_hdr_bytes[8..12].copy_from_slice(&(128u32).to_le_bytes());
    ring_hdr_bytes[12..16].copy_from_slice(&(1u32).to_le_bytes());
    ring_hdr_bytes[16..20]
        .copy_from_slice(&(ring::AerogpuSubmitDesc::SIZE_BYTES as u32).to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bytes);

    let mut submit_bytes = [0u8; ring::AerogpuSubmitDesc::SIZE_BYTES];
    let submit_copy_len = submit_bytes.len().min(ring_bytes.len());
    submit_bytes[..submit_copy_len].copy_from_slice(&ring_bytes[..submit_copy_len]);
    submit_bytes[0..4].copy_from_slice(&(ring::AerogpuSubmitDesc::SIZE_BYTES as u32).to_le_bytes());
    fuzz_ring_layouts(&submit_bytes);

    let mut fence_bytes = [0u8; ring::AerogpuFencePage::SIZE_BYTES];
    let fence_copy_len = fence_bytes.len().min(ring_bytes.len());
    fence_bytes[..fence_copy_len].copy_from_slice(&ring_bytes[..fence_copy_len]);
    fence_bytes[0..4].copy_from_slice(&ring::AEROGPU_FENCE_PAGE_MAGIC.to_le_bytes());
    fence_bytes[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    fuzz_ring_layouts(&fence_bytes);
});
