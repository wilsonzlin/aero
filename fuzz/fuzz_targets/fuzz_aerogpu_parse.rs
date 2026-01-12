#![no_main]

use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::protocol_d3d11 as d3d11;
use aero_gpu::VecGuestMemory;
use aero_gpu::{AeroGpuCommandProcessor, AeroGpuSubmissionAllocation};
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

fn write_pkt_hdr(buf: &mut [u8], off: &mut usize, opcode: u32, size: usize) -> Option<usize> {
    let start = *off;
    let end = start.checked_add(size)?;
    let hdr_end = start.checked_add(cmd::AerogpuCmdHdr::SIZE_BYTES)?;
    if end > buf.len() || hdr_end > buf.len() {
        return None;
    }

    buf[start..start + 4].copy_from_slice(&opcode.to_le_bytes());
    buf[start + 4..start + 8].copy_from_slice(&(size as u32).to_le_bytes());
    *off = end;
    Some(start)
}

fn fuzz_cmd_stream(cmd_bytes: &[u8]) {
    fn packet_bytes<'a>(cmd_bytes: &'a [u8], pkt: &cmd::AerogpuCmdPacket<'a>) -> Option<&'a [u8]> {
        let base = cmd_bytes.as_ptr() as usize;
        let payload = pkt.payload.as_ptr() as usize;
        let hdr_off = payload
            .checked_sub(base)
            .and_then(|o| o.checked_sub(cmd::AerogpuCmdHdr::SIZE_BYTES))?;
        let packet_len = pkt.hdr.size_bytes as usize;
        let packet_end = hdr_off.checked_add(packet_len)?;
        cmd_bytes.get(hdr_off..packet_end)
    }

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
                    if let Ok((_cmd, blob_bytes)) = pkt.decode_create_input_layout_payload_le() {
                        // Parse the ILAY blob header + elements (host-side helper types).
                        //
                        // The blob payload is guest-controlled and used by higher layers, so it is
                        // worth hardening its bounds checks as well.
                        let Some(blob_hdr) =
                            aero_gpu::AeroGpuInputLayoutBlobHeader::parse(blob_bytes)
                        else {
                            continue;
                        };
                        let element_count = (blob_hdr.element_count as usize).min(64);
                        let header_size = cmd::AerogpuInputLayoutBlobHeader::SIZE_BYTES;
                        let elem_size = cmd::AerogpuInputLayoutElementDxgi::SIZE_BYTES;
                        for idx in 0..element_count {
                            let Some(start) = idx
                                .checked_mul(elem_size)
                                .and_then(|o| header_size.checked_add(o))
                            else {
                                break;
                            };
                            let Some(end) = start.checked_add(elem_size) else {
                                break;
                            };
                            let Some(elem_bytes) = blob_bytes.get(start..end) else {
                                break;
                            };
                            let _ = aero_gpu::AeroGpuInputLayoutElementDxgi::parse(elem_bytes);
                        }
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetShaderConstantsF) => {
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_set_shader_constants_f_payload_le(packet_bytes);
                }
                Some(cmd::AerogpuCmdOpcode::CopyBuffer) => {
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_copy_buffer_le(packet_bytes);
                }
                Some(cmd::AerogpuCmdOpcode::CopyTexture2d) => {
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_copy_texture2d_le(packet_bytes);
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

    // Exercise the "collect into Vec" helper too (different allocation patterns / code paths).
    // Cap size to avoid creating huge vectors for pathological inputs.
    if cmd_bytes.len() <= 64 * 1024 {
        let _ = cmd::AerogpuCmdStreamView::decode_from_le_bytes(cmd_bytes);
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

    // Also exercise the fence-page writer helper (encode path).
    if ring_bytes.len() >= ring::AerogpuFencePage::SIZE_BYTES {
        let mut tmp = [0u8; ring::AerogpuFencePage::SIZE_BYTES];
        tmp.copy_from_slice(&ring_bytes[..ring::AerogpuFencePage::SIZE_BYTES]);
        let _ = ring::write_fence_page_completed_fence_le(&mut tmp, 0xDEAD_BEEF);
    }
}

fn fuzz_d3d11_cmd_stream(bytes: &[u8]) {
    // `protocol_d3d11` defines a separate word-based command protocol used by some higher-level
    // D3D11 paths. It is pure parsing/validation (no device creation), so it's safe to fuzz here.
    const MAX_WORDS: usize = 16 * 1024;
    if bytes.len() < 8 {
        return;
    }

    let word_count = (bytes.len() / 4).min(MAX_WORDS);
    let mut words = Vec::with_capacity(word_count);
    for chunk in bytes.chunks_exact(4).take(word_count) {
        words.push(u32::from_le_bytes(chunk.try_into().unwrap()));
    }

    for pkt in d3d11::CmdStream::new(&words).take(1024) {
        let Ok(pkt) = pkt else { continue };
        // Touch a few payload fields to exercise enum/bitflag decoding logic without assuming
        // the payload is well-formed.
        match pkt.header.opcode {
            d3d11::D3D11Opcode::CreateTexture2D => {
                if let Some(format) = pkt.payload.get(5).copied() {
                    let _ = d3d11::DxgiFormat::from_word(format);
                }
                if let Some(usage) = pkt.payload.get(6).copied() {
                    let _ = d3d11::TextureUsage::from_bits_truncate(usage);
                }
            }
            d3d11::D3D11Opcode::CreateBuffer => {
                if let Some(usage) = pkt.payload.get(3).copied() {
                    let _ = d3d11::BufferUsage::from_bits_truncate(usage);
                }
            }
            d3d11::D3D11Opcode::SetIndexBuffer => {
                if let Some(format) = pkt.payload.get(1).copied() {
                    let _ = match format {
                        x if x == d3d11::IndexFormat::Uint16 as u32 => {
                            Some(d3d11::IndexFormat::Uint16)
                        }
                        x if x == d3d11::IndexFormat::Uint32 as u32 => {
                            Some(d3d11::IndexFormat::Uint32)
                        }
                        _ => None,
                    };
                }
            }
            _ => {}
        }
    }
}

fn fuzz_command_processor(
    cmd_stream_bytes: &[u8],
    allocations: Option<&[AeroGpuSubmissionAllocation]>,
) {
    let mut proc = AeroGpuCommandProcessor::new();
    let _ = proc.process_submission_with_allocations(
        cmd_stream_bytes,
        allocations,
        /*signal_fence=*/ 1,
    );
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
    fuzz_d3d11_cmd_stream(ring_bytes);

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
    fuzz_command_processor(&cmd_patched, None);

    // Synthetic command stream: a fixed sequence of minimal valid packets using the fuzzer input
    // as filler. This ensures we consistently exercise a broad set of typed decoders.
    const SET_BLEND_STATE_LEGACY_SIZE_BYTES: usize = 28;

    const SYNTH_DXBC_BYTES: usize = 4;
    const SYNTH_UPLOAD_BYTES: usize = 4;
    const SYNTH_INPUT_LAYOUT_ELEMENT_COUNT: usize = 1;
    const SYNTH_INPUT_LAYOUT_BLOB_BYTES: usize = cmd::AerogpuInputLayoutBlobHeader::SIZE_BYTES
        + SYNTH_INPUT_LAYOUT_ELEMENT_COUNT * cmd::AerogpuInputLayoutElementDxgi::SIZE_BYTES;
    const SYNTH_SHADER_CONST_VEC4_COUNT: usize = 1;
    const SYNTH_VERTEX_BUFFER_COUNT: usize = 1;
    const SYNTH_SAMPLER_COUNT: usize = 1;
    const SYNTH_CONSTANT_BUFFER_COUNT: usize = 1;

    const CREATE_SHADER_DXBC_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdCreateShaderDxbc::SIZE_BYTES + SYNTH_DXBC_BYTES;
    const UPLOAD_RESOURCE_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdUploadResource::SIZE_BYTES + SYNTH_UPLOAD_BYTES;
    const CREATE_INPUT_LAYOUT_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdCreateInputLayout::SIZE_BYTES + SYNTH_INPUT_LAYOUT_BLOB_BYTES;
    const SET_SHADER_CONSTANTS_F_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdSetShaderConstantsF::SIZE_BYTES + SYNTH_SHADER_CONST_VEC4_COUNT * 16;
    const SET_VERTEX_BUFFERS_SYNTH_SIZE_BYTES: usize = cmd::AerogpuCmdSetVertexBuffers::SIZE_BYTES
        + SYNTH_VERTEX_BUFFER_COUNT * cmd::AerogpuVertexBufferBinding::SIZE_BYTES;
    const SET_SAMPLERS_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdSetSamplers::SIZE_BYTES + SYNTH_SAMPLER_COUNT * 4;
    const SET_CONSTANT_BUFFERS_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdSetConstantBuffers::SIZE_BYTES
            + SYNTH_CONSTANT_BUFFER_COUNT * cmd::AerogpuConstantBufferBinding::SIZE_BYTES;

    let cmd_synth_len = cmd::AerogpuCmdStreamHeader::SIZE_BYTES
        + cmd::AerogpuCmdHdr::SIZE_BYTES // NOP
        + cmd::AerogpuCmdHdr::SIZE_BYTES // DEBUG_MARKER (empty)
        + cmd::AerogpuCmdCreateBuffer::SIZE_BYTES
        + cmd::AerogpuCmdCreateTexture2d::SIZE_BYTES
        + cmd::AerogpuCmdDestroyResource::SIZE_BYTES
        + cmd::AerogpuCmdResourceDirtyRange::SIZE_BYTES
        + UPLOAD_RESOURCE_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdCopyBuffer::SIZE_BYTES
        + cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES
        + CREATE_SHADER_DXBC_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdDestroyShader::SIZE_BYTES
        + cmd::AerogpuCmdBindShaders::SIZE_BYTES
        + SET_SHADER_CONSTANTS_F_SYNTH_SIZE_BYTES
        + CREATE_INPUT_LAYOUT_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdDestroyInputLayout::SIZE_BYTES
        + cmd::AerogpuCmdSetInputLayout::SIZE_BYTES
        + SET_BLEND_STATE_LEGACY_SIZE_BYTES // legacy SET_BLEND_STATE (28 bytes)
        + cmd::AerogpuCmdSetBlendState::SIZE_BYTES // modern SET_BLEND_STATE
        + cmd::AerogpuCmdSetDepthStencilState::SIZE_BYTES
        + cmd::AerogpuCmdSetRasterizerState::SIZE_BYTES
        + cmd::AerogpuCmdSetRenderTargets::SIZE_BYTES
        + cmd::AerogpuCmdSetViewport::SIZE_BYTES
        + cmd::AerogpuCmdSetScissor::SIZE_BYTES
        + SET_VERTEX_BUFFERS_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdSetIndexBuffer::SIZE_BYTES
        + cmd::AerogpuCmdSetPrimitiveTopology::SIZE_BYTES
        + cmd::AerogpuCmdSetTexture::SIZE_BYTES
        + cmd::AerogpuCmdSetSamplerState::SIZE_BYTES
        + cmd::AerogpuCmdSetRenderState::SIZE_BYTES
        + cmd::AerogpuCmdCreateSampler::SIZE_BYTES
        + cmd::AerogpuCmdDestroySampler::SIZE_BYTES
        + SET_SAMPLERS_SYNTH_SIZE_BYTES
        + SET_CONSTANT_BUFFERS_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdClear::SIZE_BYTES
        + cmd::AerogpuCmdDraw::SIZE_BYTES
        + cmd::AerogpuCmdDrawIndexed::SIZE_BYTES
        + cmd::AerogpuCmdPresent::SIZE_BYTES
        + cmd::AerogpuCmdPresentEx::SIZE_BYTES
        + cmd::AerogpuCmdExportSharedSurface::SIZE_BYTES
        + cmd::AerogpuCmdImportSharedSurface::SIZE_BYTES
        + cmd::AerogpuCmdReleaseSharedSurface::SIZE_BYTES
        + cmd::AerogpuCmdFlush::SIZE_BYTES
        + cmd::AerogpuCmdHdr::SIZE_BYTES; // Unknown opcode (header-only)
    let mut cmd_synth = vec![0u8; cmd_synth_len];
    let cmd_synth_copy_len = cmd_synth.len().min(data.len());
    cmd_synth[..cmd_synth_copy_len].copy_from_slice(&data[..cmd_synth_copy_len]);
    cmd_synth[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_synth[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    let cmd_synth_size_bytes = cmd_synth.len() as u32;
    cmd_synth[8..12].copy_from_slice(&cmd_synth_size_bytes.to_le_bytes());

    let mut off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;

    // NOP
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Nop as u32,
        cmd::AerogpuCmdHdr::SIZE_BYTES,
    );

    // DEBUG_MARKER (empty)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DebugMarker as u32,
        cmd::AerogpuCmdHdr::SIZE_BYTES,
    );

    // CREATE_BUFFER (fixed-size)
    let create_buffer_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateBuffer as u32,
        cmd::AerogpuCmdCreateBuffer::SIZE_BYTES,
    );

    // CREATE_TEXTURE2D (fixed-size)
    let create_texture_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateTexture2d as u32,
        cmd::AerogpuCmdCreateTexture2d::SIZE_BYTES,
    );

    // DESTROY_RESOURCE (fixed-size)
    let destroy_resource_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DestroyResource as u32,
        cmd::AerogpuCmdDestroyResource::SIZE_BYTES,
    );

    // RESOURCE_DIRTY_RANGE (fixed-size)
    let dirty_range_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::ResourceDirtyRange as u32,
        cmd::AerogpuCmdResourceDirtyRange::SIZE_BYTES,
    );

    // CREATE_SHADER_DXBC (dxbc_size_bytes=4)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32,
        CREATE_SHADER_DXBC_SYNTH_SIZE_BYTES,
    ) {
        if let Some(dxbc_size_bytes) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            dxbc_size_bytes.copy_from_slice(&(SYNTH_DXBC_BYTES as u32).to_le_bytes());
        }
    }

    // DESTROY_SHADER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DestroyShader as u32,
        cmd::AerogpuCmdDestroyShader::SIZE_BYTES,
    );

    // BIND_SHADERS (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::BindShaders as u32,
        cmd::AerogpuCmdBindShaders::SIZE_BYTES,
    );

    // UPLOAD_RESOURCE (size_bytes=4)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::UploadResource as u32,
        UPLOAD_RESOURCE_SYNTH_SIZE_BYTES,
    ) {
        if let Some(size_bytes) = cmd_synth.get_mut(pkt + 24..pkt + 32) {
            size_bytes.copy_from_slice(&(SYNTH_UPLOAD_BYTES as u64).to_le_bytes());
        }
    }

    // CREATE_INPUT_LAYOUT (blob_size_bytes=header+1 element)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateInputLayout as u32,
        CREATE_INPUT_LAYOUT_SYNTH_SIZE_BYTES,
    ) {
        if let Some(blob_size_bytes) = cmd_synth.get_mut(pkt + 12..pkt + 16) {
            blob_size_bytes.copy_from_slice(&(SYNTH_INPUT_LAYOUT_BLOB_BYTES as u32).to_le_bytes());
        }

        if let Some(blob_start) = pkt.checked_add(cmd::AerogpuCmdCreateInputLayout::SIZE_BYTES) {
            if let Some(blob_end) =
                blob_start.checked_add(cmd::AerogpuInputLayoutBlobHeader::SIZE_BYTES)
            {
                if let Some(blob_hdr) = cmd_synth.get_mut(blob_start..blob_end) {
                    // "ILAY" header
                    blob_hdr[0..4]
                        .copy_from_slice(&cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
                    blob_hdr[4..8]
                        .copy_from_slice(&cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
                    blob_hdr[8..12]
                        .copy_from_slice(&(SYNTH_INPUT_LAYOUT_ELEMENT_COUNT as u32).to_le_bytes());
                    blob_hdr[12..16].fill(0);
                }
            }
        }
    }

    // DESTROY_INPUT_LAYOUT (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DestroyInputLayout as u32,
        cmd::AerogpuCmdDestroyInputLayout::SIZE_BYTES,
    );

    // SET_INPUT_LAYOUT (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetInputLayout as u32,
        cmd::AerogpuCmdSetInputLayout::SIZE_BYTES,
    );

    // SET_BLEND_STATE (legacy, 28 bytes)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetBlendState as u32,
        SET_BLEND_STATE_LEGACY_SIZE_BYTES,
    );

    // SET_BLEND_STATE (modern, full size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetBlendState as u32,
        cmd::AerogpuCmdSetBlendState::SIZE_BYTES,
    );

    // SET_DEPTH_STENCIL_STATE (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetDepthStencilState as u32,
        cmd::AerogpuCmdSetDepthStencilState::SIZE_BYTES,
    );

    // SET_RASTERIZER_STATE (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetRasterizerState as u32,
        cmd::AerogpuCmdSetRasterizerState::SIZE_BYTES,
    );

    // SET_RENDER_TARGETS (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetRenderTargets as u32,
        cmd::AerogpuCmdSetRenderTargets::SIZE_BYTES,
    );

    // SET_VIEWPORT (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetViewport as u32,
        cmd::AerogpuCmdSetViewport::SIZE_BYTES,
    );

    // SET_SCISSOR (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetScissor as u32,
        cmd::AerogpuCmdSetScissor::SIZE_BYTES,
    );

    // SET_SHADER_CONSTANTS_F (vec4_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32,
        SET_SHADER_CONSTANTS_F_SYNTH_SIZE_BYTES,
    ) {
        if let Some(vec4_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            vec4_count.copy_from_slice(&(SYNTH_SHADER_CONST_VEC4_COUNT as u32).to_le_bytes());
        }
    }

    // SET_VERTEX_BUFFERS (buffer_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetVertexBuffers as u32,
        SET_VERTEX_BUFFERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(buffer_count) = cmd_synth.get_mut(pkt + 12..pkt + 16) {
            buffer_count.copy_from_slice(&(SYNTH_VERTEX_BUFFER_COUNT as u32).to_le_bytes());
        }
    }

    // SET_INDEX_BUFFER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetIndexBuffer as u32,
        cmd::AerogpuCmdSetIndexBuffer::SIZE_BYTES,
    );

    // SET_PRIMITIVE_TOPOLOGY (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
        cmd::AerogpuCmdSetPrimitiveTopology::SIZE_BYTES,
    );

    // SET_TEXTURE (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetTexture as u32,
        cmd::AerogpuCmdSetTexture::SIZE_BYTES,
    );

    // SET_SAMPLER_STATE (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetSamplerState as u32,
        cmd::AerogpuCmdSetSamplerState::SIZE_BYTES,
    );

    // SET_RENDER_STATE (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetRenderState as u32,
        cmd::AerogpuCmdSetRenderState::SIZE_BYTES,
    );

    // CREATE_SAMPLER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateSampler as u32,
        cmd::AerogpuCmdCreateSampler::SIZE_BYTES,
    );

    // DESTROY_SAMPLER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DestroySampler as u32,
        cmd::AerogpuCmdDestroySampler::SIZE_BYTES,
    );

    // SET_SAMPLERS (sampler_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetSamplers as u32,
        SET_SAMPLERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(sampler_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            sampler_count.copy_from_slice(&(SYNTH_SAMPLER_COUNT as u32).to_le_bytes());
        }
    }

    // SET_CONSTANT_BUFFERS (buffer_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetConstantBuffers as u32,
        SET_CONSTANT_BUFFERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(buffer_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            buffer_count.copy_from_slice(&(SYNTH_CONSTANT_BUFFER_COUNT as u32).to_le_bytes());
        }
    }

    // CLEAR (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Clear as u32,
        cmd::AerogpuCmdClear::SIZE_BYTES,
    );

    // DRAW (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Draw as u32,
        cmd::AerogpuCmdDraw::SIZE_BYTES,
    );

    // DRAW_INDEXED (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DrawIndexed as u32,
        cmd::AerogpuCmdDrawIndexed::SIZE_BYTES,
    );

    // PRESENT (fixed-size)
    let present_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Present as u32,
        cmd::AerogpuCmdPresent::SIZE_BYTES,
    );

    // PRESENT_EX (fixed-size)
    let present_ex_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::PresentEx as u32,
        cmd::AerogpuCmdPresentEx::SIZE_BYTES,
    );

    // EXPORT_SHARED_SURFACE (fixed-size)
    let export_shared_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::ExportSharedSurface as u32,
        cmd::AerogpuCmdExportSharedSurface::SIZE_BYTES,
    );

    // IMPORT_SHARED_SURFACE (fixed-size)
    let import_shared_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::ImportSharedSurface as u32,
        cmd::AerogpuCmdImportSharedSurface::SIZE_BYTES,
    );

    // RELEASE_SHARED_SURFACE (fixed-size)
    let release_shared_off = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::ReleaseSharedSurface as u32,
        cmd::AerogpuCmdReleaseSharedSurface::SIZE_BYTES,
    );

    // FLUSH (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Flush as u32,
        cmd::AerogpuCmdFlush::SIZE_BYTES,
    );

    // COPY_BUFFER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CopyBuffer as u32,
        cmd::AerogpuCmdCopyBuffer::SIZE_BYTES,
    );

    // COPY_TEXTURE2D (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CopyTexture2d as u32,
        cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES,
    );

    // Unknown opcode (header-only)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        0xFFFF_FFFFu32,
        cmd::AerogpuCmdHdr::SIZE_BYTES,
    );

    fuzz_cmd_stream(&cmd_synth);

    // Also exercise the higher-level command processor's guest-input validation paths.
    //
    // Patch a few critical fields to be self-consistent so the processor can make progress past
    // the first resource-creation packets.
    let mut cmd_proc_synth = cmd_synth.clone();
    let alloc_id = 1u32;
    let buf_handle = 1u32;
    let tex_handle = 2u32;
    let alias_handle = 3u32;
    let share_token = 0x1234_5678_9ABC_DEF0u64;
    let allocs = [AeroGpuSubmissionAllocation {
        alloc_id,
        gpa: 0x2000,
        size_bytes: 0x1000,
    }];

    if let Some(pkt) = create_buffer_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&buf_handle.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 16..pkt + 24) {
            v.copy_from_slice(&64u64.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 24..pkt + 28) {
            v.copy_from_slice(&alloc_id.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 28..pkt + 32) {
            v.copy_from_slice(&0u32.to_le_bytes());
        }
    }

    if let Some(pkt) = create_texture_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&tex_handle.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&(pci::AerogpuFormat::B8G8R8A8Unorm as u32).to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 20..pkt + 24) {
            v.copy_from_slice(&4u32.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 24..pkt + 28) {
            v.copy_from_slice(&4u32.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 28..pkt + 32) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 32..pkt + 36) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 36..pkt + 40) {
            v.copy_from_slice(&16u32.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 40..pkt + 44) {
            v.copy_from_slice(&alloc_id.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 44..pkt + 48) {
            v.copy_from_slice(&0u32.to_le_bytes());
        }
    }

    if let Some(pkt) = destroy_resource_off {
        // Use an unknown handle to make this a no-op (avoids breaking later shared-surface ops).
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&999u32.to_le_bytes());
        }
    }

    if let Some(pkt) = dirty_range_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&buf_handle.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 16..pkt + 24) {
            v.copy_from_slice(&0u64.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 24..pkt + 32) {
            v.copy_from_slice(&4u64.to_le_bytes());
        }
    }

    if let Some(pkt) = present_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&0u32.to_le_bytes());
        }
    }

    if let Some(pkt) = present_ex_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&0u32.to_le_bytes());
        }
    }

    if let Some(pkt) = export_shared_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&tex_handle.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 16..pkt + 24) {
            v.copy_from_slice(&share_token.to_le_bytes());
        }
    }

    if let Some(pkt) = import_shared_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 12) {
            v.copy_from_slice(&alias_handle.to_le_bytes());
        }
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 16..pkt + 24) {
            v.copy_from_slice(&share_token.to_le_bytes());
        }
    }

    if let Some(pkt) = release_shared_off {
        if let Some(v) = cmd_proc_synth.get_mut(pkt + 8..pkt + 16) {
            v.copy_from_slice(&share_token.to_le_bytes());
        }
    }

    // Run multiple submissions through a single processor instance to exercise stateful validation
    // (e.g. retired share tokens, idempotent resource rebinds, fence monotonicity).
    let mut proc = AeroGpuCommandProcessor::new();
    let _ = proc.process_submission_with_allocations(&cmd_proc_synth, Some(&allocs), 1);
    // Rebind mismatch: attempt to re-create the same buffer handle with a different size.
    let mut cmd_proc_rebind = cmd_proc_synth.clone();
    if let Some(pkt) = create_buffer_off {
        if let Some(v) = cmd_proc_rebind.get_mut(pkt + 16..pkt + 24) {
            v.copy_from_slice(&128u64.to_le_bytes());
        }
    }
    let _ = proc.process_submission_with_allocations(&cmd_proc_rebind, Some(&allocs), 2);
    // Replaying the original stream should hit "retired token" style paths (we retired the token
    // in the first submission via RELEASE_SHARED_SURFACE).
    let _ = proc.process_submission_with_allocations(&cmd_proc_synth, Some(&allocs), 3);
    // Also try the same stream without providing allocations to trigger missing-alloc-table paths.
    let _ = proc.process_submission_with_allocations(&cmd_proc_synth, None, 4);

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

    // Patched alloc table (forward-compat stride): declare a larger entry stride to exercise the
    // per-entry decode paths (rather than the fast aligned/borrowed slice path).
    let stride_large = stride + 8;
    let mut alloc_stride_large = vec![0u8; header_size + stride_large];
    let alloc_stride_large_copy_len = alloc_stride_large.len().min(alloc_bytes.len());
    alloc_stride_large[..alloc_stride_large_copy_len]
        .copy_from_slice(&alloc_bytes[..alloc_stride_large_copy_len]);
    alloc_stride_large[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_stride_large[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    let alloc_stride_large_size_bytes = alloc_stride_large.len() as u32;
    alloc_stride_large[8..12].copy_from_slice(&alloc_stride_large_size_bytes.to_le_bytes());
    alloc_stride_large[12..16].copy_from_slice(&1u32.to_le_bytes());
    alloc_stride_large[16..20].copy_from_slice(&(stride_large as u32).to_le_bytes());
    alloc_stride_large[20..24].fill(0);
    let entry_off = header_size;
    alloc_stride_large[entry_off..entry_off + 4].copy_from_slice(&1u32.to_le_bytes()); // alloc_id
    alloc_stride_large[entry_off + 4..entry_off + 8].fill(0); // flags
    alloc_stride_large[entry_off + 8..entry_off + 16].copy_from_slice(&0x3000u64.to_le_bytes()); // gpa
    alloc_stride_large[entry_off + 16..entry_off + 24].copy_from_slice(&0x1000u64.to_le_bytes()); // size_bytes
    alloc_stride_large[entry_off + 24..entry_off + 32].fill(0); // reserved0
    fuzz_alloc_table(&alloc_stride_large);

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
