#![no_main]

use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::frame_source::FrameSource;
use aero_gpu::protocol_d3d11 as d3d11;
use aero_gpu::VecGuestMemory;
use aero_gpu::{
    AeroGpuCommandProcessor, AeroGpuSubmissionAllocation, Presenter, Rect, TextureWriter,
};
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use aero_shared::shared_framebuffer::{
    FramebufferFormat, SharedFramebufferHeader, SharedFramebufferLayout, SHARED_FRAMEBUFFER_MAGIC,
    SHARED_FRAMEBUFFER_VERSION,
};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::Ordering;

/// Max fuzz input size to avoid pathological allocations inside command-stream / alloc-table
/// decoding paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Non-zero guest physical address to place the candidate alloc-table bytes at.
const ALLOC_TABLE_GPA: u64 = 0x1000;

#[derive(Default)]
struct NoopTextureWriter;

impl TextureWriter for NoopTextureWriter {
    fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]) {
        // Touch parameters to keep the optimizer from discarding the call.
        let _ = (rect, bytes_per_row, data.len());
    }
}

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

fn write_guest_texture2d_4x4_bgra8(w: &mut AerogpuCmdWriter, texture_handle: u32, alloc_id: u32) {
    w.create_texture2d(
        texture_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 16,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
}

fn fuzz_cmd_stream(cmd_bytes: &[u8]) {
    fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
        let end = off.checked_add(4)?;
        let bytes: [u8; 4] = buf.get(off..end)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }

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

    // Deterministic stage_ex resolution edge cases (independent of packet bytes).
    //
    // These are cheap and ensure we always exercise the error branches in `resolve_stage` even if
    // the current input doesn't happen to encode them.
    let _ = cmd::resolve_stage(cmd::AerogpuShaderStage::Compute as u32, 1); // InvalidStageEx
    let _ = cmd::resolve_stage(cmd::AerogpuShaderStage::Compute as u32, 6); // UnknownStageEx
    let _ = cmd::resolve_stage(u32::MAX, 0); // UnknownShaderStage
    // Also exercise the stage_ex helper paths that return "unknown" via `None`/`Unknown`.
    let _ = cmd::decode_stage_ex(cmd::AerogpuShaderStage::Compute as u32, 6); // None (unknown stage_ex)
    let _ = cmd::resolve_shader_stage_with_ex(cmd::AerogpuShaderStage::Compute as u32, 6); // Unknown stage_ex
    let _ = cmd::resolve_shader_stage_with_ex(u32::MAX, 0); // Unknown legacy stage

    // Also exercise the canonical protocol-level command stream parser + typed packet decoders.
    //
    // This avoids wgpu device creation and stays in pure parsing/bounds-checking code.
    let _ = cmd::decode_cmd_stream_header_le(cmd_bytes);
    if let Ok(iter) = cmd::AerogpuCmdStreamIter::new(cmd_bytes) {
        let abi_minor = pci::abi_minor(iter.header().abi_version);
        for pkt in iter.take(1024) {
            let Ok(pkt) = pkt else { continue };
            // Also exercise the stage_ex decode helper for arbitrary `(shader_stage, reserved0)`
            // pairs derived from guest-controlled packet bytes (don't assume the packet is valid).
            //
            // Many command packets use a `(u32 shader_stage, ..., u32 reserved0)` pattern, with
            // stage_ex encoded via `(shader_stage=COMPUTE, reserved0=stage_ex)`.
            if let (Some(a0), Some(a1), Some(reserved0)) = (
                read_u32_le(pkt.payload, 0),
                read_u32_le(pkt.payload, 4),
                read_u32_le(pkt.payload, 12),
            ) {
                let _ = cmd::decode_stage_ex(a0, reserved0);
                let _ = cmd::decode_stage_ex(a1, reserved0);
                let _ = cmd::decode_stage_ex_gated(abi_minor, a0, reserved0);
                let _ = cmd::decode_stage_ex_gated(abi_minor, a1, reserved0);
                let _ = cmd::resolve_shader_stage_with_ex_gated(abi_minor, a0, reserved0);
                let _ = cmd::resolve_shader_stage_with_ex_gated(abi_minor, a1, reserved0);
                let _ = cmd::resolve_stage(a0, reserved0);
                let _ = cmd::resolve_stage(a1, reserved0);
                let _ = cmd::resolve_shader_stage_with_ex(a0, reserved0);
                let _ = cmd::resolve_shader_stage_with_ex(a1, reserved0);
                // Also exercise the stage_ex reserved0 encoder (safe path only).
                let stage_ex = cmd::AerogpuShaderStageEx::from_u32(reserved0);
                let _ = cmd::encode_stage_ex_reserved0(cmd::AerogpuShaderStage::Compute, stage_ex);
                if let Some(stage) = cmd::AerogpuShaderStage::from_u32(a0) {
                    let _ = cmd::encode_stage_ex_reserved0(stage, None);
                }
            }
            match pkt.opcode {
                Some(cmd::AerogpuCmdOpcode::CreateShaderDxbc) => {
                    // Exercise both the packet-level decoder (payload-only) and the public
                    // decode helper that re-parses the header (different validation paths).
                    if let Ok((cmd_hdr, _dxbc_bytes)) = pkt.decode_create_shader_dxbc_payload_le() {
                        // Exercise stage_ex resolution helpers (forward-compat path).
                        let _ = match cmd_hdr.stage {
                            0 => Some(cmd::AerogpuShaderStage::Vertex),
                            1 => Some(cmd::AerogpuShaderStage::Pixel),
                            2 => Some(cmd::AerogpuShaderStage::Compute),
                            3 => Some(cmd::AerogpuShaderStage::Geometry),
                            _ => None,
                        };
                        let stage_ex = cmd::decode_stage_ex(cmd_hdr.stage, cmd_hdr.reserved0);
                        if let Some(stage_ex) = stage_ex {
                            let _ = cmd::encode_stage_ex(stage_ex);
                        }
                        // Also exercise the stage resolution helpers (they accept/validate a wider
                        // range of legacy and stage_ex encodings).
                        let _ = cmd::resolve_stage(cmd_hdr.stage, cmd_hdr.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(cmd_hdr.stage, cmd_hdr.reserved0);
                        let _ = cmd_hdr.resolved_stage();
                        // Still fuzz the raw stage_ex decoder.
                        let _ = cmd::AerogpuShaderStageEx::from_u32(cmd_hdr.reserved0);
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_create_shader_dxbc_payload_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::BindShaders) => {
                    if let Ok((_cmd, ex)) = pkt.decode_bind_shaders_payload_le() {
                        // Touch the optional extended shader bindings to exercise the new
                        // variable-length decode path.
                        if let Some(ex) = ex {
                            let _ = (ex.gs, ex.hs, ex.ds);
                        }
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_bind_shaders_payload_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::UploadResource) => {
                    let _ = pkt.decode_upload_resource_payload_le();
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_upload_resource_payload_le(packet_bytes);
                    }
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
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_create_input_layout_blob_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetShaderConstantsF) => {
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    if let Ok((cmd_sc, _data)) =
                        cmd::decode_cmd_set_shader_constants_f_payload_le(packet_bytes)
                    {
                        let _ = cmd_sc.resolved_stage();
                        let _ = cmd::decode_stage_ex(cmd_sc.stage, cmd_sc.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(cmd_sc.stage, cmd_sc.reserved0);
                        let _ = cmd::resolve_stage(cmd_sc.stage, cmd_sc.reserved0);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetShaderConstantsI) => {
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_shader_constants_i_payload_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetShaderConstantsB) => {
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_shader_constants_b_payload_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::CopyBuffer) => {
                    let _ = pkt.decode_copy_buffer_payload_le();
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_copy_buffer_le(packet_bytes);
                }
                Some(cmd::AerogpuCmdOpcode::CopyTexture2d) => {
                    let _ = pkt.decode_copy_texture2d_payload_le();
                    let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) else {
                        continue;
                    };
                    let _ = cmd::decode_cmd_copy_texture2d_le(packet_bytes);
                }
                Some(cmd::AerogpuCmdOpcode::Dispatch) => {
                    let _ = pkt.decode_dispatch_payload_le();
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_dispatch_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetVertexBuffers) => {
                    let _ = pkt.decode_set_vertex_buffers_payload_le();
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_vertex_buffers_bindings_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetTexture) => {
                    if let Ok(cmd_set_texture) = pkt.decode_set_texture_payload_le() {
                        let _ = cmd::decode_stage_ex(cmd_set_texture.shader_stage, cmd_set_texture.reserved0);
                        let _ = cmd::resolve_stage(cmd_set_texture.shader_stage, cmd_set_texture.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(
                            cmd_set_texture.shader_stage,
                            cmd_set_texture.reserved0,
                        );
                        let _ = cmd_set_texture.resolved_shader_stage();
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_texture_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetSamplers) => {
                    if let Ok((cmd_samplers, _handles)) = pkt.decode_set_samplers_payload_le() {
                        let _ =
                            cmd::decode_stage_ex(cmd_samplers.shader_stage, cmd_samplers.reserved0);
                        let _ = cmd::resolve_stage(cmd_samplers.shader_stage, cmd_samplers.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(
                            cmd_samplers.shader_stage,
                            cmd_samplers.reserved0,
                        );
                        let _ = cmd_samplers.resolved_shader_stage();
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_samplers_handles_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetConstantBuffers) => {
                    if let Ok((cmd_hdr, bindings)) = pkt.decode_set_constant_buffers_payload_le() {
                        // Exercise stage_ex resolution helpers (forward-compat path).
                        let _ = match cmd_hdr.shader_stage {
                            0 => Some(cmd::AerogpuShaderStage::Vertex),
                            1 => Some(cmd::AerogpuShaderStage::Pixel),
                            2 => Some(cmd::AerogpuShaderStage::Compute),
                            3 => Some(cmd::AerogpuShaderStage::Geometry),
                            _ => None,
                        };
                        let stage_ex = cmd::decode_stage_ex(cmd_hdr.shader_stage, cmd_hdr.reserved0);
                        if let Some(stage_ex) = stage_ex {
                            let _ = cmd::encode_stage_ex(stage_ex);
                        }
                        let _ = cmd::resolve_stage(cmd_hdr.shader_stage, cmd_hdr.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(
                            cmd_hdr.shader_stage,
                            cmd_hdr.reserved0,
                        );
                        let _ = cmd_hdr.resolved_shader_stage();
                        // Touch a couple binding fields to exercise packed/unaligned field reads.
                        for binding in bindings.iter().take(4) {
                            let _ = (binding.buffer, binding.offset_bytes, binding.size_bytes);
                        }
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_constant_buffers_bindings_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetShaderResourceBuffers) => {
                    if let Ok((cmd_srv, _bindings)) =
                        pkt.decode_set_shader_resource_buffers_payload_le()
                    {
                        let _ = cmd::decode_stage_ex(cmd_srv.shader_stage, cmd_srv.reserved0);
                        let _ = cmd::resolve_stage(cmd_srv.shader_stage, cmd_srv.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(
                            cmd_srv.shader_stage,
                            cmd_srv.reserved0,
                        );
                        let _ = cmd_srv.resolved_shader_stage();
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ = cmd::decode_cmd_set_shader_resource_buffers_bindings_le(packet_bytes);
                    }
                }
                Some(cmd::AerogpuCmdOpcode::SetUnorderedAccessBuffers) => {
                    if let Ok((cmd_uav, _bindings)) =
                        pkt.decode_set_unordered_access_buffers_payload_le()
                    {
                        let _ = cmd::decode_stage_ex(cmd_uav.shader_stage, cmd_uav.reserved0);
                        let _ = cmd::resolve_stage(cmd_uav.shader_stage, cmd_uav.reserved0);
                        let _ = cmd::resolve_shader_stage_with_ex(
                            cmd_uav.shader_stage,
                            cmd_uav.reserved0,
                        );
                        let _ = cmd_uav.resolved_shader_stage();
                    }
                    if let Some(packet_bytes) = packet_bytes(cmd_bytes, &pkt) {
                        let _ =
                            cmd::decode_cmd_set_unordered_access_buffers_bindings_le(packet_bytes);
                    }
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
    //
    // Also try decoding with a "capacity" larger than the declared size_bytes (when applicable) to
    // exercise the executor's forward-compat behavior of ignoring trailing bytes.
    let cap_size_bytes = match u32::try_from(alloc_bytes.len()) {
        Ok(len) if len as usize <= MAX_INPUT_SIZE_BYTES => len,
        _ => return,
    };

    let table_size_bytes_primary = match declared_size_bytes {
        0 => cap_size_bytes,
        declared
            if declared as usize <= alloc_bytes.len()
                && declared as usize <= MAX_INPUT_SIZE_BYTES =>
        {
            declared
        }
        _ => cap_size_bytes,
    };

    let guest_mem_size = (ALLOC_TABLE_GPA as usize).saturating_add(cap_size_bytes as usize);
    if guest_mem_size > (ALLOC_TABLE_GPA as usize).saturating_add(MAX_INPUT_SIZE_BYTES) {
        return;
    }

    let mut guest = VecGuestMemory::new(guest_mem_size);
    let _ = guest.write(ALLOC_TABLE_GPA, alloc_bytes);
    let _ =
        AllocTable::decode_from_guest_memory(&mut guest, ALLOC_TABLE_GPA, table_size_bytes_primary);

    if declared_size_bytes != 0 && declared_size_bytes < cap_size_bytes {
        let _ = AllocTable::decode_from_guest_memory(&mut guest, ALLOC_TABLE_GPA, cap_size_bytes);
    }
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
        let mut arr = [0u8; 4];
        arr.copy_from_slice(chunk);
        words.push(u32::from_le_bytes(arr));
    }

    for pkt in d3d11::CmdStream::new(&words).take(1024) {
        let pkt = match pkt {
            Ok(pkt) => pkt,
            // Errors are acceptable fuzz outcomes; treat them as terminal for this stream.
            Err(_) => break,
        };
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

fn fuzz_bc_decompress(bytes: &[u8]) {
    // CPU-only BCn decompression + BC-dimension compatibility checks used by backend fallbacks.
    //
    // Keep dimensions small and derived from a few bytes so we don't allocate huge output buffers.
    if bytes.len() < 4 {
        return;
    }
    // Cap dimensions to keep decompression costs predictable (especially BC7).
    let width = (bytes[0] & 63) as u32;
    let height = (bytes[1] & 63) as u32;
    let mip_level_count = (bytes[2] % 16) as u32;
    let selector = bytes[3] & 3;
    let data = &bytes[4..];

    let _ = aero_gpu::wgpu_bc_texture_dimensions_compatible(width, height, mip_level_count);

    match selector {
        0 => {
            let _ = aero_gpu::decompress_bc1_rgba8(width, height, data);
        }
        1 => {
            let _ = aero_gpu::decompress_bc2_rgba8(width, height, data);
        }
        2 => {
            let _ = aero_gpu::decompress_bc3_rgba8(width, height, data);
        }
        _ => {
            let _ = aero_gpu::decompress_bc7_rgba8(width, height, data);
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

fn fuzz_command_processor_allocations(cmd_stream_bytes: &[u8], alloc_bytes: &[u8]) {
    // CPU-only: exercise allocation-backed validation paths with a synthetic per-submission
    // allocation table derived from the fuzzer input.
    let mut u = Unstructured::new(alloc_bytes);
    let alloc_count = (u.arbitrary::<u8>().unwrap_or(0) % 32) as usize;
    let mut allocs = Vec::with_capacity(alloc_count);
    for _ in 0..alloc_count {
        allocs.push(AeroGpuSubmissionAllocation {
            alloc_id: u.arbitrary::<u32>().unwrap_or(0),
            gpa: u.arbitrary::<u64>().unwrap_or(0),
            size_bytes: u.arbitrary::<u64>().unwrap_or(0),
        });
    }
    fuzz_command_processor(cmd_stream_bytes, Some(&allocs));
}

fn fuzz_presenter(bytes: &[u8]) {
    // CPU-only fuzzing for the framebuffer presenter logic (dirty-rect slicing, stride handling,
    // and size calculations).
    //
    // Keep dimensions small and derive all layout decisions from a few bytes so the fuzzer can
    // reach both success and error paths without triggering large allocations.
    let mut u = Unstructured::new(bytes);
    let width = (u.arbitrary::<u8>().unwrap_or(0) % 64) as u32 + 1;
    let height = (u.arbitrary::<u8>().unwrap_or(0) % 64) as u32 + 1;

    let bpp = match u.arbitrary::<u8>().unwrap_or(0) % 4 {
        0 => 1usize,
        1 => 2usize,
        2 => 4usize,
        _ => 8usize,
    };

    let full_row_bytes = width as usize * bpp;
    let stride_mode = u.arbitrary::<u8>().unwrap_or(0) % 4;
    let stride = match stride_mode {
        0 => full_row_bytes,
        1 => (full_row_bytes + 255) & !255,
        2 => full_row_bytes.saturating_add(u.arbitrary::<u8>().unwrap_or(0) as usize),
        _ => full_row_bytes.saturating_sub(u.arbitrary::<u8>().unwrap_or(0) as usize),
    };

    let rect_count = (u.arbitrary::<u8>().unwrap_or(0) % 8) as usize;
    let mut rects = Vec::with_capacity(rect_count);
    for _ in 0..rect_count {
        let x = (u.arbitrary::<u8>().unwrap_or(0) as u32) % width;
        let y = (u.arbitrary::<u8>().unwrap_or(0) as u32) % height;
        let max_w = width.saturating_sub(x);
        let max_h = height.saturating_sub(y);
        let w = (u.arbitrary::<u8>().unwrap_or(0) as u32) % (max_w + 1);
        let h = (u.arbitrary::<u8>().unwrap_or(0) as u32) % (max_h + 1);
        rects.push(Rect::new(x, y, w, h));
    }

    let frame_data = u.take_rest();
    let mut presenter = Presenter::new(width, height, bpp, NoopTextureWriter::default());
    let _ = if rect_count == 0 {
        presenter.present(frame_data, stride, None)
    } else {
        presenter.present(frame_data, stride, Some(&rects))
    };
}

fn fuzz_frame_source(bytes: &[u8]) {
    // Fuzz the shared-memory framebuffer header parsing/validation paths used by the browser
    // presenter. All header values are guest-controlled in the WASM/JS display pipeline.
    //
    // Keep dimensions small so the total shared-memory region (2 buffers + dirty tracking) stays
    // bounded and we can safely call `poll_frame()` a few times per iteration.
    const MAX_DIM: u32 = 64;
    const MAX_STRIDE_BYTES: u32 = 1024;

    let mut u = Unstructured::new(bytes);

    let width = (u.arbitrary::<u8>().unwrap_or(0) as u32 % MAX_DIM) + 1;
    let height = (u.arbitrary::<u8>().unwrap_or(0) as u32 % MAX_DIM) + 1;

    // Only use tile sizes accepted by `SharedFramebufferLayout::new` (0 or power-of-two).
    let tile_size = match u.arbitrary::<u8>().unwrap_or(0) % 7 {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        5 => 16,
        _ => 32,
    };

    let bpp = FramebufferFormat::Rgba8.bytes_per_pixel();
    let min_stride = width.saturating_mul(bpp);

    // Derive a candidate stride, including values that may be too small (to exercise layout errors).
    let stride_mode = u.arbitrary::<u8>().unwrap_or(0) % 4;
    let stride_delta = u.arbitrary::<u16>().unwrap_or(0) as u32;
    let stride_candidate = match stride_mode {
        0 => min_stride,
        1 => min_stride.saturating_add(stride_delta % 128),
        2 => min_stride.saturating_sub(stride_delta % 32),
        _ => stride_delta,
    }
    .min(MAX_STRIDE_BYTES);

    // Allocate backing storage for a layout that is always valid and large enough for any header
    // stride that can possibly succeed (i.e. `stride>=min_stride`).
    let alloc_stride = stride_candidate.max(min_stride).min(MAX_STRIDE_BYTES);
    let Ok(layout) = SharedFramebufferLayout::new(
        width,
        height,
        alloc_stride,
        FramebufferFormat::Rgba8,
        tile_size,
    ) else {
        return;
    };

    // `total_byte_len()` is always aligned to 64, so it is also a multiple of 4.
    let word_len = layout.total_byte_len() / 4;
    if word_len == 0 {
        return;
    }
    let mut backing_words = vec![0u32; word_len];

    // Fuzzer-controlled header field candidates.
    let magic_raw = u.arbitrary::<u32>().unwrap_or(0);
    let version_raw = u.arbitrary::<u32>().unwrap_or(0);
    let format_raw = u.arbitrary::<u32>().unwrap_or(0);
    let dirty_words_raw = u.arbitrary::<u32>().unwrap_or(0);
    let tiles_x_raw = u.arbitrary::<u32>().unwrap_or(0);
    let tiles_y_raw = u.arbitrary::<u32>().unwrap_or(0);

    let active_index_raw = u.arbitrary::<u32>().unwrap_or(0);
    let frame_seq_raw = u.arbitrary::<u32>().unwrap_or(0);
    let buf0_seq_raw = u.arbitrary::<u32>().unwrap_or(0);
    let buf1_seq_raw = u.arbitrary::<u32>().unwrap_or(0);
    let flags_raw = u.arbitrary::<u32>().unwrap_or(0);

    let rest = u.take_rest();

    // Mix the remaining bytes into the backing store so we don't only test zero-filled buffers.
    let backing_bytes = unsafe {
        std::slice::from_raw_parts_mut(backing_words.as_mut_ptr() as *mut u8, backing_words.len() * 4)
    };
    let copy_len = rest.len().min(backing_bytes.len());
    backing_bytes[..copy_len].copy_from_slice(&rest[..copy_len]);

    let header = unsafe { &*(backing_words.as_ptr() as *const SharedFramebufferHeader) };

    let base_ptr = backing_words.as_mut_ptr() as *mut u8;

    // Exercise basic pointer validation paths: `FrameSource::from_shared_memory` must gracefully
    // reject null and unaligned base pointers/offsets without panicking or performing UB.
    let _ = unsafe { FrameSource::from_shared_memory(std::ptr::null_mut(), 0) };
    // Unaligned base pointer (still within our allocation).
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr.add(1), 0) };
    // Unaligned offset (still within our allocation).
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 1) };

    let mut drive = |mut source: FrameSource| {
        let mut rest_u = Unstructured::new(rest);
        let shared = source.shared();
        let layout = shared.layout();
        let header = shared.header();

        // Try a few polls, updating sequence + dirty words so the logic sees both None and Some.
        for _ in 0..3 {
            let active_index = rest_u.arbitrary::<u32>().unwrap_or(0);
            header.active_index.store(active_index, Ordering::SeqCst);
            let active = (active_index & 1) as usize;

            // Poke dirty words for the active buffer.
            if layout.dirty_words_per_buffer != 0 {
                if let Some(off) = layout.dirty_offset_bytes(active) {
                    let start_word = off / 4;
                    let len = layout.dirty_words_per_buffer as usize;
                    if start_word + len <= backing_words.len() {
                        let poke_words = len.min(4);
                        for i in 0..poke_words {
                            backing_words[start_word + i] = rest_u.arbitrary::<u32>().unwrap_or(0);
                        }
                        if rest_u.arbitrary::<bool>().unwrap_or(false) && poke_words != 0 {
                            backing_words[start_word] |= 1;
                        }
                    }
                }
            }

            let next_seq = header.frame_seq.load(Ordering::SeqCst).wrapping_add(1);
            match active {
                0 => header.buf0_frame_seq.store(next_seq, Ordering::SeqCst),
                _ => header.buf1_frame_seq.store(next_seq, Ordering::SeqCst),
            }
            header.frame_seq.store(next_seq, Ordering::SeqCst);
            header
                .frame_dirty
                .store(rest_u.arbitrary::<u32>().unwrap_or(0), Ordering::SeqCst);

            if let Some(view) = source.poll_frame() {
                let _ = view.dirty_rects_for_presenter();
            }
        }
        // Exercise the "no new frame" fast path too.
        let _ = source.poll_frame();
    };

    let write_header = |magic: u32,
                        version: u32,
                        format: u32,
                        stride_bytes: u32,
                        dirty_words_per_buffer: u32,
                        tiles_x: u32,
                        tiles_y: u32| {
        // Always start from a known-good baseline layout so we can safely re-run `from_shared_memory`
        // multiple times per fuzz input while keeping the backing store consistent with the
        // allocation.
        header.init(layout);

        // Apply (potentially inconsistent) guest-controlled header values.
        header.magic.store(magic, Ordering::SeqCst);
        header.version.store(version, Ordering::SeqCst);
        header.format.store(format, Ordering::SeqCst);
        header.stride_bytes.store(stride_bytes, Ordering::SeqCst);
        header
            .dirty_words_per_buffer
            .store(dirty_words_per_buffer, Ordering::SeqCst);
        header.tiles_x.store(tiles_x, Ordering::SeqCst);
        header.tiles_y.store(tiles_y, Ordering::SeqCst);

        header.active_index.store(active_index_raw, Ordering::SeqCst);
        header.frame_seq.store(frame_seq_raw, Ordering::SeqCst);
        header.buf0_frame_seq.store(buf0_seq_raw, Ordering::SeqCst);
        header.buf1_frame_seq.store(buf1_seq_raw, Ordering::SeqCst);
        header.flags.store(flags_raw, Ordering::SeqCst);
    };

    // Fuzzed header.
    write_header(
        magic_raw,
        version_raw,
        format_raw,
        stride_candidate,
        dirty_words_raw,
        tiles_x_raw,
        tiles_y_raw,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);

    // Patched magic only: exercise version/format/layout error paths without requiring the fuzzer to
    // guess the 32-bit magic prefix.
    write_header(
        SHARED_FRAMEBUFFER_MAGIC,
        version_raw,
        format_raw,
        stride_candidate,
        dirty_words_raw,
        tiles_x_raw,
        tiles_y_raw,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);

    // Patched magic+version: same as above but bypass the version gate as well.
    write_header(
        SHARED_FRAMEBUFFER_MAGIC,
        SHARED_FRAMEBUFFER_VERSION,
        format_raw,
        stride_candidate,
        dirty_words_raw,
        tiles_x_raw,
        tiles_y_raw,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);

    // Patched: valid magic/version/format/stride but fuzzed dirty/tile metadata to hit the header
    // mismatch checks.
    write_header(
        SHARED_FRAMEBUFFER_MAGIC,
        SHARED_FRAMEBUFFER_VERSION,
        FramebufferFormat::Rgba8 as u32,
        layout.stride_bytes,
        dirty_words_raw,
        tiles_x_raw,
        tiles_y_raw,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);

    // Patched: valid magic/version/format/stride/dirty_words but fuzzed tiles to hit TilesMismatch.
    write_header(
        SHARED_FRAMEBUFFER_MAGIC,
        SHARED_FRAMEBUFFER_VERSION,
        FramebufferFormat::Rgba8 as u32,
        layout.stride_bytes,
        layout.dirty_words_per_buffer,
        tiles_x_raw,
        tiles_y_raw,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);

    // Patched header: force valid magic/version/format and consistent layout-derived tile metadata
    // so the fuzzer can reach the steady-state poll/present paths more often.
    write_header(
        SHARED_FRAMEBUFFER_MAGIC,
        SHARED_FRAMEBUFFER_VERSION,
        FramebufferFormat::Rgba8 as u32,
        stride_candidate.max(min_stride),
        layout.dirty_words_per_buffer,
        layout.tiles_x,
        layout.tiles_y,
    );
    let _ = unsafe { FrameSource::from_shared_memory(base_ptr, 0) }.map(&mut drive);
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
    fuzz_bc_decompress(ring_bytes);
    fuzz_presenter(ring_bytes);
    fuzz_frame_source(ring_bytes);

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
    fuzz_command_processor_allocations(&cmd_patched, alloc_bytes);

    // Deterministic command stream header/packet layout errors. These are small but help ensure
    // coverage of the host-side parse error mapping logic.
    let header_size = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    // Bad magic.
    let mut cmd_bad_magic = vec![0u8; header_size];
    cmd_bad_magic[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    cmd_bad_magic[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_magic[8..12].copy_from_slice(&(header_size as u32).to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_magic);
    // Unsupported ABI major.
    let mut cmd_bad_abi = vec![0u8; header_size];
    cmd_bad_abi[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    let unsupported_abi_version = ((pci::AEROGPU_ABI_MAJOR + 1) << 16) | pci::AEROGPU_ABI_MINOR;
    cmd_bad_abi[4..8].copy_from_slice(&unsupported_abi_version.to_le_bytes());
    cmd_bad_abi[8..12].copy_from_slice(&(header_size as u32).to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_abi);
    // Legacy ABI minor (< stage_ex): ensure `decode_stage_ex_gated` takes its "ignore reserved0"
    // branch for compute-stage packets.
    let legacy_abi_minor_u32 = (cmd::AEROGPU_STAGE_EX_MIN_ABI_MINOR as u32).saturating_sub(1);
    let legacy_abi_version = (pci::AEROGPU_ABI_MAJOR << 16) | legacy_abi_minor_u32;
    let mut cmd_stage_ex_legacy = vec![0u8; header_size + cmd::AerogpuCmdSetTexture::SIZE_BYTES];
    let cmd_stage_ex_legacy_size_u32 = cmd_stage_ex_legacy.len() as u32;
    cmd_stage_ex_legacy[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_stage_ex_legacy[4..8].copy_from_slice(&legacy_abi_version.to_le_bytes());
    cmd_stage_ex_legacy[8..12].copy_from_slice(&cmd_stage_ex_legacy_size_u32.to_le_bytes());
    cmd_stage_ex_legacy[12..16].fill(0);
    cmd_stage_ex_legacy[16..24].fill(0);
    let mut off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    if let Some(pkt) = write_pkt_hdr(
        cmd_stage_ex_legacy.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetTexture as u32,
        cmd::AerogpuCmdSetTexture::SIZE_BYTES,
    ) {
        if let Some(shader_stage) = cmd_stage_ex_legacy.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_stage_ex_legacy.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(cmd::AerogpuShaderStageEx::Hull as u32).to_le_bytes());
        }
    }
    fuzz_cmd_stream(&cmd_stage_ex_legacy);
    // Header size_bytes too small.
    let mut cmd_bad_size_small = vec![0u8; header_size];
    cmd_bad_size_small[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_bad_size_small[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_size_small[8..12].copy_from_slice(&0u32.to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_size_small);
    // Header size_bytes larger than provided buffer.
    let mut cmd_bad_size_large = vec![0u8; header_size];
    cmd_bad_size_large[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_bad_size_large[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_size_large[8..12].copy_from_slice(&((header_size as u32) + 4).to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_size_large);
    // Packet header with size_bytes < AerogpuCmdHdr::SIZE_BYTES.
    let mut cmd_bad_cmd_size_small = vec![0u8; header_size + cmd::AerogpuCmdHdr::SIZE_BYTES];
    let cmd_bad_cmd_size_small_u32 = cmd_bad_cmd_size_small.len() as u32;
    cmd_bad_cmd_size_small[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_bad_cmd_size_small[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_cmd_size_small[8..12].copy_from_slice(&cmd_bad_cmd_size_small_u32.to_le_bytes());
    cmd_bad_cmd_size_small[header_size..header_size + 4].copy_from_slice(&0u32.to_le_bytes());
    cmd_bad_cmd_size_small[header_size + 4..header_size + 8].copy_from_slice(&4u32.to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_cmd_size_small);
    // Packet header with misaligned size_bytes.
    let mut cmd_bad_cmd_size_align = vec![0u8; header_size + cmd::AerogpuCmdHdr::SIZE_BYTES];
    let cmd_bad_cmd_size_align_u32 = cmd_bad_cmd_size_align.len() as u32;
    cmd_bad_cmd_size_align[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_bad_cmd_size_align[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_cmd_size_align[8..12].copy_from_slice(&cmd_bad_cmd_size_align_u32.to_le_bytes());
    cmd_bad_cmd_size_align[header_size..header_size + 4].copy_from_slice(&0u32.to_le_bytes());
    cmd_bad_cmd_size_align[header_size + 4..header_size + 8].copy_from_slice(&10u32.to_le_bytes());
    fuzz_cmd_stream(&cmd_bad_cmd_size_align);
    // Packet overruns stream: header size_bytes is small but the first packet declares a larger
    // size.
    let mut cmd_overruns = vec![0u8; header_size + cmd::AerogpuCmdHdr::SIZE_BYTES];
    cmd_overruns[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_overruns[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_overruns[8..12]
        .copy_from_slice(&((header_size + cmd::AerogpuCmdHdr::SIZE_BYTES) as u32).to_le_bytes());
    cmd_overruns[header_size..header_size + 4].copy_from_slice(&0u32.to_le_bytes());
    cmd_overruns[header_size + 4..header_size + 8].copy_from_slice(&12u32.to_le_bytes());
    fuzz_cmd_stream(&cmd_overruns);

    // Synthetic command stream: a fixed sequence of minimal valid packets using the fuzzer input
    // as filler. This ensures we consistently exercise a broad set of typed decoders.
    const SET_BLEND_STATE_LEGACY_SIZE_BYTES: usize = 28;
    const BIND_SHADERS_BASE_SIZE_BYTES: usize = cmd::AerogpuCmdBindShaders::SIZE_BYTES;
    // Variable-length `BIND_SHADERS` extension: base + optional gs/hs/ds handles (3 * u32).
    const BIND_SHADERS_EXTENDED_SIZE_BYTES: usize = cmd::AerogpuCmdBindShaders::EX_SIZE_BYTES;

    const SYNTH_DXBC_BYTES: usize = 4;
    const SYNTH_UPLOAD_BYTES: usize = 4;
    const SYNTH_INPUT_LAYOUT_ELEMENT_COUNT: usize = 1;
    const SYNTH_INPUT_LAYOUT_BLOB_BYTES: usize = cmd::AerogpuInputLayoutBlobHeader::SIZE_BYTES
        + SYNTH_INPUT_LAYOUT_ELEMENT_COUNT * cmd::AerogpuInputLayoutElementDxgi::SIZE_BYTES;
    const SYNTH_SHADER_CONST_VEC4_COUNT: usize = 1;
    const SYNTH_VERTEX_BUFFER_COUNT: usize = 1;
    const SYNTH_SAMPLER_COUNT: usize = 1;
    const SYNTH_CONSTANT_BUFFER_COUNT: usize = 1;
    const SYNTH_SHADER_RESOURCE_BUFFER_COUNT: usize = 1;
    const SYNTH_UNORDERED_ACCESS_BUFFER_COUNT: usize = 1;

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
    const SET_SHADER_RESOURCE_BUFFERS_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdSetShaderResourceBuffers::SIZE_BYTES
            + SYNTH_SHADER_RESOURCE_BUFFER_COUNT * cmd::AerogpuShaderResourceBufferBinding::SIZE_BYTES;
    const SET_UNORDERED_ACCESS_BUFFERS_SYNTH_SIZE_BYTES: usize =
        cmd::AerogpuCmdSetUnorderedAccessBuffers::SIZE_BYTES
            + SYNTH_UNORDERED_ACCESS_BUFFER_COUNT
                * cmd::AerogpuUnorderedAccessBufferBinding::SIZE_BYTES;

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
        + BIND_SHADERS_BASE_SIZE_BYTES
        + BIND_SHADERS_EXTENDED_SIZE_BYTES
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
        + SET_SHADER_RESOURCE_BUFFERS_SYNTH_SIZE_BYTES
        + SET_UNORDERED_ACCESS_BUFFERS_SYNTH_SIZE_BYTES
        + cmd::AerogpuCmdClear::SIZE_BYTES
        + cmd::AerogpuCmdDraw::SIZE_BYTES
        + cmd::AerogpuCmdDrawIndexed::SIZE_BYTES
        + cmd::AerogpuCmdDispatch::SIZE_BYTES
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
    let stage_ex_seed0 = data.get(0).copied().unwrap_or(0) & 7;
    let stage_ex_seed1 = data.get(1).copied().unwrap_or(0) & 7;
    let stage_ex_seed2 = data.get(2).copied().unwrap_or(0) & 7;
    let stage_ex_seed3 = data.get(3).copied().unwrap_or(0) & 7;
    let stage_ex_seed4 = data.get(4).copied().unwrap_or(0) & 7;
    let stage_ex_seed5 = data.get(5).copied().unwrap_or(0) & 7;

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
        if let Some(stage) = cmd_synth.get_mut(pkt + 12..pkt + 16) {
            stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(dxbc_size_bytes) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            dxbc_size_bytes.copy_from_slice(&(SYNTH_DXBC_BYTES as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed0 as u32).to_le_bytes());
        }
    }

    // DESTROY_SHADER (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::DestroyShader as u32,
        cmd::AerogpuCmdDestroyShader::SIZE_BYTES,
    );

    // BIND_SHADERS (base)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::BindShaders as u32,
        BIND_SHADERS_BASE_SIZE_BYTES,
    );
    // BIND_SHADERS (extended: base + gs/hs/ds)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::BindShaders as u32,
        BIND_SHADERS_EXTENDED_SIZE_BYTES,
    ) {
        // Mirror gs into the legacy reserved0 field and also populate the append-only
        // `{gs,hs,ds}` tail.
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&0x1000u32.to_le_bytes()); // legacy gs
        }
        let tail = pkt + cmd::AerogpuCmdBindShaders::SIZE_BYTES;
        let ex_size = core::mem::size_of::<cmd::BindShadersEx>();
        if let Some(end) = tail.checked_add(ex_size) {
            if let Some(ex_bytes) = cmd_synth.get_mut(tail..end) {
                ex_bytes[0..4].copy_from_slice(&0x1000u32.to_le_bytes()); // gs
                ex_bytes[4..8].copy_from_slice(&0x1001u32.to_le_bytes()); // hs
                ex_bytes[8..12].copy_from_slice(&0x1002u32.to_le_bytes()); // ds
            }
        }
    }

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
        if let Some(stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(vec4_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            vec4_count.copy_from_slice(&(SYNTH_SHADER_CONST_VEC4_COUNT as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed3 as u32).to_le_bytes());
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

    // SET_TEXTURE (fixed-size; stage_ex: shader_stage=COMPUTE, reserved0=fuzzed)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetTexture as u32,
        cmd::AerogpuCmdSetTexture::SIZE_BYTES,
    ) {
        if let Some(shader_stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed0 as u32).to_le_bytes());
        }
    }

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
        if let Some(shader_stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(sampler_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            sampler_count.copy_from_slice(&(SYNTH_SAMPLER_COUNT as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed1 as u32).to_le_bytes());
        }
    }

    // SET_CONSTANT_BUFFERS (buffer_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetConstantBuffers as u32,
        SET_CONSTANT_BUFFERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(shader_stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(buffer_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            buffer_count.copy_from_slice(&(SYNTH_CONSTANT_BUFFER_COUNT as u32).to_le_bytes());
        }
        // Exercise stage_ex decoding: keep shader_stage=COMPUTE (sentinel) and stash a non-zero
        // DXBC program-type discriminator (1..=5) in `reserved0`.
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            let stage_ex_u32 = ((stage_ex_seed2 % 5) + 1) as u32;
            reserved0.copy_from_slice(&stage_ex_u32.to_le_bytes());
        }
    }

    // SET_SHADER_RESOURCE_BUFFERS (buffer_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
        SET_SHADER_RESOURCE_BUFFERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(shader_stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(buffer_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            buffer_count.copy_from_slice(&(SYNTH_SHADER_RESOURCE_BUFFER_COUNT as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed4 as u32).to_le_bytes());
        }
    }

    // SET_UNORDERED_ACCESS_BUFFERS (uav_count=1)
    if let Some(pkt) = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
        SET_UNORDERED_ACCESS_BUFFERS_SYNTH_SIZE_BYTES,
    ) {
        if let Some(shader_stage) = cmd_synth.get_mut(pkt + 8..pkt + 12) {
            shader_stage.copy_from_slice(&(cmd::AerogpuShaderStage::Compute as u32).to_le_bytes());
        }
        if let Some(uav_count) = cmd_synth.get_mut(pkt + 16..pkt + 20) {
            uav_count.copy_from_slice(&(SYNTH_UNORDERED_ACCESS_BUFFER_COUNT as u32).to_le_bytes());
        }
        if let Some(reserved0) = cmd_synth.get_mut(pkt + 20..pkt + 24) {
            reserved0.copy_from_slice(&(stage_ex_seed5 as u32).to_le_bytes());
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

    // DISPATCH (fixed-size)
    let _ = write_pkt_hdr(
        cmd_synth.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::Dispatch as u32,
        cmd::AerogpuCmdDispatch::SIZE_BYTES,
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
    // Also try a misaligned slice to exercise `align_to`/prefix handling in variable-length
    // packet decoders (SET_VERTEX_BUFFERS/SET_SAMPLERS/SET_CONSTANT_BUFFERS, ...).
    let mut cmd_synth_misaligned = Vec::with_capacity(cmd_synth.len().saturating_add(1));
    cmd_synth_misaligned.push(0);
    cmd_synth_misaligned.extend_from_slice(&cmd_synth);
    fuzz_cmd_stream(&cmd_synth_misaligned[1..]);

    // Exercise the host-side parser's legacy/partial SET_BLEND_STATE decoding logic.
    //
    // `parse_cmd_stream` accepts a legacy 28-byte packet (hdr + enable/src/dst/op/mask) and
    // progressively extended variants where the later fields become present.
    let set_blend_sizes = [
        28usize,
        32,
        36,
        40,
        44,
        48,
        52,
        56,
        cmd::AerogpuCmdSetBlendState::SIZE_BYTES,
    ];
    let cmd_blend_variants_size =
        cmd::AerogpuCmdStreamHeader::SIZE_BYTES + set_blend_sizes.iter().copied().sum::<usize>();
    let mut cmd_blend_variants = vec![0u8; cmd_blend_variants_size];
    let cmd_blend_variants_size_u32 = cmd_blend_variants.len() as u32;
    cmd_blend_variants[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_blend_variants[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_blend_variants[8..12].copy_from_slice(&cmd_blend_variants_size_u32.to_le_bytes());
    cmd_blend_variants[12..16].fill(0);
    cmd_blend_variants[16..24].fill(0);
    let mut off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    for &size in &set_blend_sizes {
        if write_pkt_hdr(
            cmd_blend_variants.as_mut_slice(),
            &mut off,
            cmd::AerogpuCmdOpcode::SetBlendState as u32,
            size,
        )
        .is_none()
        {
            break;
        }
    }
    fuzz_cmd_stream(&cmd_blend_variants);

    // Synthetic command stream with intentionally inconsistent length/count fields.
    //
    // This deterministically hits error paths in the typed packet decoders (BadSizeBytes /
    // PayloadSizeMismatch) that are otherwise hard to reach consistently.
    let cmd_bad_len_size = cmd::AerogpuCmdStreamHeader::SIZE_BYTES
        + cmd::AerogpuCmdCreateShaderDxbc::SIZE_BYTES
        // BIND_SHADERS with a truncated "extended" payload: payload.len() > 16 but < 28.
        // This should decode as "no appended gs/hs/ds handles" without reading past bounds.
        + (cmd::AerogpuCmdBindShaders::SIZE_BYTES + 4)
        + cmd::AerogpuCmdUploadResource::SIZE_BYTES
        + cmd::AerogpuCmdCreateInputLayout::SIZE_BYTES
        + cmd::AerogpuCmdSetShaderConstantsF::SIZE_BYTES
        + cmd::AerogpuCmdSetVertexBuffers::SIZE_BYTES
        + cmd::AerogpuCmdSetSamplers::SIZE_BYTES
        + cmd::AerogpuCmdSetConstantBuffers::SIZE_BYTES
        + cmd::AerogpuCmdSetShaderResourceBuffers::SIZE_BYTES
        + cmd::AerogpuCmdSetUnorderedAccessBuffers::SIZE_BYTES;
    let mut cmd_bad_len = vec![0u8; cmd_bad_len_size];
    let cmd_bad_len_size_u32 = cmd_bad_len.len() as u32;
    cmd_bad_len[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_bad_len[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_bad_len[8..12].copy_from_slice(&cmd_bad_len_size_u32.to_le_bytes());
    cmd_bad_len[12..16].fill(0);
    cmd_bad_len[16..24].fill(0);
    let mut off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;

    // CREATE_SHADER_DXBC: dxbc_size_bytes=1 but no payload bytes.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32,
        cmd::AerogpuCmdCreateShaderDxbc::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // BIND_SHADERS: truncated extended payload (payload len 20, but extended decode needs 28).
    let bind_shaders_truncated_size = cmd::AerogpuCmdBindShaders::SIZE_BYTES + 4;
    let _ = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::BindShaders as u32,
        bind_shaders_truncated_size,
    );
    // UPLOAD_RESOURCE: size_bytes=1 but no data bytes.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::UploadResource as u32,
        cmd::AerogpuCmdUploadResource::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 24..pkt + 32) {
            v.copy_from_slice(&1u64.to_le_bytes());
        }
    }
    // CREATE_INPUT_LAYOUT: blob_size_bytes=1 but no blob bytes.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::CreateInputLayout as u32,
        cmd::AerogpuCmdCreateInputLayout::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 12..pkt + 16) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_SHADER_CONSTANTS_F: vec4_count=1 but no float data bytes.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32,
        cmd::AerogpuCmdSetShaderConstantsF::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_VERTEX_BUFFERS: buffer_count=1 but no bindings.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetVertexBuffers as u32,
        cmd::AerogpuCmdSetVertexBuffers::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 12..pkt + 16) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_SAMPLERS: sampler_count=1 but no handles.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetSamplers as u32,
        cmd::AerogpuCmdSetSamplers::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_CONSTANT_BUFFERS: buffer_count=1 but no bindings.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetConstantBuffers as u32,
        cmd::AerogpuCmdSetConstantBuffers::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_SHADER_RESOURCE_BUFFERS: buffer_count=1 but no bindings.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
        cmd::AerogpuCmdSetShaderResourceBuffers::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    // SET_UNORDERED_ACCESS_BUFFERS: uav_count=1 but no bindings.
    if let Some(pkt) = write_pkt_hdr(
        cmd_bad_len.as_mut_slice(),
        &mut off,
        cmd::AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
        cmd::AerogpuCmdSetUnorderedAccessBuffers::SIZE_BYTES,
    ) {
        if let Some(v) = cmd_bad_len.get_mut(pkt + 16..pkt + 20) {
            v.copy_from_slice(&1u32.to_le_bytes());
        }
    }
    fuzz_cmd_stream(&cmd_bad_len);
    fuzz_command_processor(&cmd_bad_len, None);

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

    // Protocol extension coverage: stage_ex binding packets + extended BIND_SHADERS payload.
    //
    // This stream is small and synthetically constructed, but hits the new parsing surface even
    // when the raw fuzzer input doesn't naturally form these layouts.
    let stage_ex_buf_handle = buf_handle.wrapping_add(10);
    let stage_ex_sampler_handle = 1000u32;
    let stage_ex_cb_binding = [cmd::AerogpuConstantBufferBinding {
        buffer: stage_ex_buf_handle,
        offset_bytes: 0,
        size_bytes: 4,
        reserved0: 0,
    }];
    let stage_ex_srv_binding = [cmd::AerogpuShaderResourceBufferBinding {
        buffer: stage_ex_buf_handle,
        offset_bytes: 0,
        size_bytes: 4,
        reserved0: 0,
    }];
    let stage_ex_uav_binding = [cmd::AerogpuUnorderedAccessBufferBinding {
        buffer: stage_ex_buf_handle,
        offset_bytes: 0,
        size_bytes: 4,
        initial_count: 0,
    }];
    let stage_ex_constants_f = [0.0f32, 1.0, -1.0, f32::from_bits(u32::MAX)];

    let mut w = AerogpuCmdWriter::new();
    // Host-backed resources so the stream is self-contained (no allocation table required).
    w.create_buffer(
        stage_ex_buf_handle,
        cmd::AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER | cmd::AEROGPU_RESOURCE_USAGE_STORAGE,
        /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 0,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    w.create_sampler(
        stage_ex_sampler_handle,
        cmd::AerogpuSamplerFilter::Nearest,
        cmd::AerogpuSamplerAddressMode::ClampToEdge,
        cmd::AerogpuSamplerAddressMode::ClampToEdge,
        cmd::AerogpuSamplerAddressMode::ClampToEdge,
    );

    // stage_ex encoding uses (shader_stage=COMPUTE, reserved0=stage_ex).
    w.set_texture_ex(cmd::AerogpuShaderStageEx::Geometry, /*slot=*/ 0, tex_handle);
    w.set_texture_ex(cmd::AerogpuShaderStageEx::Hull, /*slot=*/ 1, tex_handle);
    w.set_texture_ex(cmd::AerogpuShaderStageEx::Domain, /*slot=*/ 2, tex_handle);
    w.set_samplers_ex(
        cmd::AerogpuShaderStageEx::Hull,
        /*start_slot=*/ 0,
        &[stage_ex_sampler_handle],
    );
    w.set_constant_buffers_ex(
        cmd::AerogpuShaderStageEx::Hull,
        /*start_slot=*/ 0,
        &stage_ex_cb_binding,
    );
    w.set_shader_resource_buffers_ex(
        cmd::AerogpuShaderStageEx::Geometry,
        /*start_slot=*/ 0,
        &stage_ex_srv_binding,
    );
    w.set_unordered_access_buffers_ex(
        cmd::AerogpuShaderStageEx::Hull,
        /*start_slot=*/ 0,
        &stage_ex_uav_binding,
    );
    w.set_shader_constants_f_ex(
        cmd::AerogpuShaderStageEx::Domain,
        /*start_register=*/ 0,
        &stage_ex_constants_f,
    );

    // Emit the append-only extended BIND_SHADERS payload using the cmd_writer helper so we keep
    // size_bytes/padding rules canonical.
    w.bind_shaders_ex(
        /*vs=*/ 10,
        /*ps=*/ 11,
        /*cs=*/ 12,
        /*gs=*/ 100,
        /*hs=*/ 101,
        /*ds=*/ 102,
    );
    let cmd_proc_stage_ex_bind_shaders_ex = w.finish();
    fuzz_cmd_stream(&cmd_proc_stage_ex_bind_shaders_ex);
    fuzz_command_processor(&cmd_proc_stage_ex_bind_shaders_ex, None);

    // Additional deterministic BIND_SHADERS layouts:
    //
    // - Legacy GS-in-reserved0 encoding (24-byte packet).
    // - Extended payload with unknown trailing bytes (forward-compat: must be ignored).
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_with_gs(/*vs=*/ 10, /*gs=*/ 999, /*ps=*/ 11, /*cs=*/ 12);
    let cmd_bind_shaders_legacy_gs = w.finish();
    fuzz_cmd_stream(&cmd_bind_shaders_legacy_gs);
    fuzz_command_processor(&cmd_bind_shaders_legacy_gs, None);

    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_ex(/*vs=*/ 10, /*ps=*/ 11, /*cs=*/ 12, /*gs=*/ 100, /*hs=*/ 101, /*ds=*/ 102);
    let mut cmd_bind_shaders_ex_trailing = w.finish();
    // Increase the packet size and append an extra u32 to simulate a newer guest extending the
    // packet beyond what the current decoder understands.
    let pkt_off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    let new_pkt_size = (cmd::AerogpuCmdBindShaders::EX_SIZE_BYTES + 4) as u32;
    if let Some(size_bytes) = cmd_bind_shaders_ex_trailing.get_mut(pkt_off + 4..pkt_off + 8) {
        size_bytes.copy_from_slice(&new_pkt_size.to_le_bytes());
    }
    cmd_bind_shaders_ex_trailing.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let new_stream_size = cmd_bind_shaders_ex_trailing.len() as u32;
    if let Some(size_bytes) = cmd_bind_shaders_ex_trailing.get_mut(8..12) {
        size_bytes.copy_from_slice(&new_stream_size.to_le_bytes());
    }
    fuzz_cmd_stream(&cmd_bind_shaders_ex_trailing);
    fuzz_command_processor(&cmd_bind_shaders_ex_trailing, None);

    // CommandProcessor edge-case: destroy the original shared surface handle while imported aliases
    // keep the underlying resource alive, then issue a dirty-range for the destroyed handle.
    //
    // This should hit the `UnknownResourceHandle` branch in `ResourceDirtyRange` validation.
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    w.destroy_resource(tex_handle);
    w.resource_dirty_range(tex_handle, /*offset_bytes=*/ 0, /*size_bytes=*/ 4);
    let cmd_proc_unknown_handle = w.finish();
    fuzz_command_processor(&cmd_proc_unknown_handle, Some(&allocs));

    // CommandProcessor edge-case: exporting the same share token for a different underlying handle
    // should error deterministically (`ShareTokenAlreadyExported`).
    let tex2_handle = 4u32;
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    write_guest_texture2d_4x4_bgra8(&mut w, tex2_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.export_shared_surface(tex2_handle, share_token);
    let cmd_proc_token_retarget = w.finish();
    fuzz_command_processor(&cmd_proc_token_retarget, Some(&allocs));

    // CommandProcessor edge-case: importing an unknown share token should error deterministically
    // (`UnknownShareToken`).
    let mut w = AerogpuCmdWriter::new();
    w.import_shared_surface(alias_handle, share_token);
    let cmd_proc_unknown_token = w.finish();
    fuzz_command_processor(&cmd_proc_unknown_token, None);

    // CommandProcessor edge-case: exporting a shared surface for an unknown handle should error
    // deterministically (`UnknownSharedSurfaceHandle`).
    let mut w = AerogpuCmdWriter::new();
    w.export_shared_surface(tex_handle, share_token);
    let cmd_proc_unknown_shared_handle = w.finish();
    fuzz_command_processor(&cmd_proc_unknown_shared_handle, None);

    // CommandProcessor edge-case: invalid share token 0 should be rejected (`InvalidShareToken`).
    let mut w = AerogpuCmdWriter::new();
    w.export_shared_surface(tex_handle, /*share_token=*/ 0);
    let cmd_proc_invalid_token = w.finish();
    fuzz_command_processor(&cmd_proc_invalid_token, None);

    // CommandProcessor edge-case: alias already bound to a different underlying handle should be
    // rejected (`SharedSurfaceAliasAlreadyBound`).
    let share_token2 = share_token.wrapping_add(1);
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    write_guest_texture2d_4x4_bgra8(&mut w, tex2_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.export_shared_surface(tex2_handle, share_token2);
    w.import_shared_surface(alias_handle, share_token);
    w.import_shared_surface(alias_handle, share_token2);
    let cmd_proc_alias_rebind = w.finish();
    fuzz_command_processor(&cmd_proc_alias_rebind, Some(&allocs));

    // CommandProcessor edge-case: creating a non-shared resource using an alias handle should be
    // rejected (`SharedSurfaceHandleInUse`).
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    w.create_buffer(
        alias_handle,
        /*usage_flags=*/ 0,
        /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_handle_in_use = w.finish();
    fuzz_command_processor(&cmd_proc_handle_in_use, Some(&allocs));

    // CommandProcessor edge-case: invalid resource handle 0 is rejected (`InvalidResourceHandle`).
    let mut w = AerogpuCmdWriter::new();
    w.export_shared_surface(/*resource_handle=*/ 0, share_token);
    let cmd_proc_invalid_handle = w.finish();
    fuzz_command_processor(&cmd_proc_invalid_handle, None);

    // Allocation-backed resource validation paths:
    //
    // - InvalidCreateBuffer: size_bytes=0 is rejected
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 0, /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_invalid_create_buf = w.finish();
    fuzz_command_processor(&cmd_proc_invalid_create_buf, None);

    // - InvalidCreateBuffer: size_bytes must be 4-byte aligned.
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4, /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let mut cmd_proc_invalid_create_buf_align = w.finish();
    let pkt = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    if let Some(v) = cmd_proc_invalid_create_buf_align.get_mut(pkt + 16..pkt + 24) {
        v.copy_from_slice(&6u64.to_le_bytes());
    }
    fuzz_command_processor(&cmd_proc_invalid_create_buf_align, None);

    // - UnknownAllocId: allocation table does not contain backing_alloc_id
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle,
        /*usage_flags=*/ 0,
        /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ alloc_id.wrapping_add(1),
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_unknown_alloc = w.finish();
    fuzz_command_processor(&cmd_proc_unknown_alloc, Some(&allocs));

    // - AllocationOutOfBounds: resource backing does not fit in allocation
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle,
        /*usage_flags=*/ 0,
        /*size_bytes=*/ allocs[0].size_bytes.wrapping_add(4),
        /*backing_alloc_id=*/ alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_alloc_oob = w.finish();
    fuzz_command_processor(&cmd_proc_alloc_oob, Some(&allocs));

    // - ResourceOutOfBounds: dirty range extends past resource size
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ alloc_id, /*backing_offset_bytes=*/ 0,
    );
    w.resource_dirty_range(buf_handle, /*offset_bytes=*/ 0, /*size_bytes=*/ 8);
    let cmd_proc_resource_oob = w.finish();
    fuzz_command_processor(&cmd_proc_resource_oob, Some(&allocs));

    // - SizeOverflow: dirty range offset+size arithmetic overflow.
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ alloc_id, /*backing_offset_bytes=*/ 0,
    );
    w.resource_dirty_range(
        buf_handle,
        /*offset_bytes=*/ u64::MAX,
        /*size_bytes=*/ 1,
    );
    let cmd_proc_dirty_overflow = w.finish();
    fuzz_command_processor(&cmd_proc_dirty_overflow, Some(&allocs));

    // - InvalidCreateTexture2d: guest-backed textures require non-zero row_pitch_bytes
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 0,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_invalid_tex = w.finish();
    fuzz_command_processor(&cmd_proc_invalid_tex, Some(&allocs));

    // - SizeOverflow: large height + pitch with multiple array layers overflows u64 arithmetic
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 1,
        /*height=*/ u32::MAX,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 2,
        /*row_pitch_bytes=*/ u32::MAX,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_size_overflow = w.finish();
    fuzz_command_processor(&cmd_proc_size_overflow, None);

    // - InvalidCreateTexture2d: unknown format values must be rejected (don't guess layout).
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        /*format=*/ u32::MAX,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 16,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_unknown_format = w.finish();
    fuzz_command_processor(&cmd_proc_unknown_format, Some(&allocs));

    // - Host-backed textures may omit row_pitch_bytes and use tight pitch for mip0.
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 2,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 0,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_host_tex_tight_pitch = w.finish();
    fuzz_command_processor(&cmd_proc_host_tex_tight_pitch, None);

    // - Block-compressed formats exercise the BC layout (ceil-div by 4 blocks).
    let bc_tex_handle = 5u32;
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        bc_tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::BC1RgbaUnorm as u32,
        /*width=*/ 5,
        /*height=*/ 5,
        /*mip_levels=*/ 2,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 16,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_bc1_guest_backed = w.finish();
    fuzz_command_processor(&cmd_proc_bc1_guest_backed, Some(&allocs));

    // - InvalidCreateTexture2d: BC formats still enforce minimum row_pitch_bytes for guest backing.
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        bc_tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::BC1RgbaUnorm as u32,
        /*width=*/ 5,
        /*height=*/ 5,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 8,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_bc1_invalid_pitch = w.finish();
    fuzz_command_processor(&cmd_proc_bc1_invalid_pitch, Some(&allocs));

    // - Host-backed BC textures can also use tight pitch (row_pitch_bytes=0).
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        /*texture_handle=*/ 6,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::BC7RgbaUnorm as u32,
        /*width=*/ 5,
        /*height=*/ 5,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 0,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_bc7_host_tight_pitch = w.finish();
    fuzz_command_processor(&cmd_proc_bc7_host_tight_pitch, None);

    // - Dirty ranges for host-owned resources are ignored (some streams conservatively emit them).
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4, /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    w.resource_dirty_range(buf_handle, /*offset_bytes=*/ 0, /*size_bytes=*/ 4);
    let cmd_proc_host_owned_dirty = w.finish();
    fuzz_command_processor(&cmd_proc_host_owned_dirty, None);

    // - MissingAllocationTable can be hit from ResourceDirtyRange, not just CREATE_*.
    let mut proc = AeroGpuCommandProcessor::new();
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        buf_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ alloc_id, /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_establish_guest_backed_buf = w.finish();
    let _ = proc.process_submission_with_allocations(
        &cmd_proc_establish_guest_backed_buf,
        Some(&allocs),
        1,
    );
    let mut w = AerogpuCmdWriter::new();
    w.resource_dirty_range(buf_handle, /*offset_bytes=*/ 0, /*size_bytes=*/ 4);
    let cmd_proc_dirty_missing_allocs = w.finish();
    let _ = proc.process_submission_with_allocations(&cmd_proc_dirty_missing_allocs, None, 2);

    // - Underlying shared-surface handles remain reserved while aliases keep the refcount alive.
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    w.destroy_resource(tex_handle);
    w.create_buffer(
        tex_handle, /*usage_flags=*/ 0, /*size_bytes=*/ 4, /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_handle_reuse_after_destroy = w.finish();
    fuzz_command_processor(&cmd_proc_handle_reuse_after_destroy, Some(&allocs));

    // - ExportSharedSurface is idempotent for an already-exported token targeting the same
    //   underlying handle.
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.export_shared_surface(tex_handle, share_token);
    let cmd_proc_export_idempotent = w.finish();
    fuzz_command_processor(&cmd_proc_export_idempotent, Some(&allocs));

    // - ImportSharedSurface is idempotent for an alias already bound to the same underlying
    //   handle.
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    let cmd_proc_import_idempotent = w.finish();
    fuzz_command_processor(&cmd_proc_import_idempotent, Some(&allocs));

    // - ReleaseSharedSurface for an unknown token is a no-op (idempotent).
    let mut w = AerogpuCmdWriter::new();
    w.release_shared_surface(share_token);
    let cmd_proc_release_unknown = w.finish();
    fuzz_command_processor(&cmd_proc_release_unknown, None);

    // - ImportSharedSurface rejects invalid share_token=0 (`InvalidShareToken`).
    let mut w = AerogpuCmdWriter::new();
    w.import_shared_surface(alias_handle, /*share_token=*/ 0);
    let cmd_proc_import_invalid_token = w.finish();
    fuzz_command_processor(&cmd_proc_import_invalid_token, None);

    // - ImportSharedSurface rejects out_resource_handle=0 (`InvalidResourceHandle`).
    let mut w = AerogpuCmdWriter::new();
    w.import_shared_surface(/*out_resource_handle=*/ 0, share_token);
    let cmd_proc_import_invalid_handle = w.finish();
    fuzz_command_processor(&cmd_proc_import_invalid_handle, None);

    // - CreateBuffer rejects handle=0 (`InvalidResourceHandle`).
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(
        /*buffer_handle=*/ 0, /*usage_flags=*/ 0, /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ 0, /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_create_buf_invalid_handle = w.finish();
    fuzz_command_processor(&cmd_proc_create_buf_invalid_handle, None);

    // - CreateTexture2d rejects handle=0 (`InvalidResourceHandle`).
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        /*texture_handle=*/ 0,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 16,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_create_tex_invalid_handle = w.finish();
    fuzz_command_processor(&cmd_proc_create_tex_invalid_handle, Some(&allocs));

    // - ImportSharedSurface must reject importing into a handle already used by a non-shared
    //   resource (`SharedSurfaceHandleInUse`).
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.create_buffer(
        alias_handle,
        /*usage_flags=*/ 0,
        /*size_bytes=*/ 4,
        /*backing_alloc_id=*/ 0,
        /*backing_offset_bytes=*/ 0,
    );
    w.import_shared_surface(alias_handle, share_token);
    let cmd_proc_import_handle_in_use = w.finish();
    fuzz_command_processor(&cmd_proc_import_handle_in_use, Some(&allocs));

    // - ImportSharedSurface must also reject importing into the destroyed original handle while
    //   aliases keep the underlying surface alive (`SharedSurfaceHandleInUse`).
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.export_shared_surface(tex_handle, share_token);
    w.import_shared_surface(alias_handle, share_token);
    w.destroy_resource(tex_handle);
    w.import_shared_surface(tex_handle, share_token);
    let cmd_proc_import_destroyed_handle = w.finish();
    fuzz_command_processor(&cmd_proc_import_destroyed_handle, Some(&allocs));

    // - CreateRebindMismatch: re-creating a texture handle with different immutable parameters.
    let mut w = AerogpuCmdWriter::new();
    write_guest_texture2d_4x4_bgra8(&mut w, tex_handle, alloc_id);
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 8,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 32,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_tex_rebind = w.finish();
    fuzz_command_processor(&cmd_proc_tex_rebind, Some(&allocs));

    // - InvalidCreateTexture2d: reject non-zero row_pitch_bytes smaller than the minimum tight
    //   pitch for the format/width.
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        tex_handle,
        /*usage_flags=*/ 0,
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
        /*width=*/ 4,
        /*height=*/ 4,
        /*mip_levels=*/ 1,
        /*array_layers=*/ 1,
        /*row_pitch_bytes=*/ 15,
        alloc_id,
        /*backing_offset_bytes=*/ 0,
    );
    let cmd_proc_bad_row_pitch = w.finish();
    fuzz_command_processor(&cmd_proc_bad_row_pitch, Some(&allocs));

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
    // Also decode from a misaligned base pointer to exercise the per-entry decode path even when
    // `entry_stride_bytes` matches the canonical entry size.
    let mut alloc_one_misaligned = Vec::with_capacity(alloc_one.len().saturating_add(1));
    alloc_one_misaligned.push(0);
    alloc_one_misaligned.extend_from_slice(&alloc_one);
    fuzz_alloc_table(&alloc_one_misaligned[1..]);

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

    // Deterministic executor alloc-table validation errors (AllocTable::new).
    //
    // The protocol decoder only validates structural layout; the executor additionally validates
    // per-entry invariants like alloc_id!=0, size_bytes!=0, gpa+size overflow, and duplicate ids.
    let alloc_two_size = header_size + stride * 2;
    let mut alloc_two_base = vec![0u8; alloc_two_size];
    alloc_two_base[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_two_base[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_two_base[8..12].copy_from_slice(&(alloc_two_size as u32).to_le_bytes());
    alloc_two_base[12..16].copy_from_slice(&2u32.to_le_bytes());
    alloc_two_base[16..20].copy_from_slice(&(stride as u32).to_le_bytes());
    alloc_two_base[20..24].fill(0);
    let write_entry = |buf: &mut [u8], idx: usize, alloc_id: u32, gpa: u64, size_bytes: u64| {
        let base = header_size + idx * stride;
        buf[base..base + 4].copy_from_slice(&alloc_id.to_le_bytes());
        buf[base + 4..base + 8].fill(0);
        buf[base + 8..base + 16].copy_from_slice(&gpa.to_le_bytes());
        buf[base + 16..base + 24].copy_from_slice(&size_bytes.to_le_bytes());
        buf[base + 24..base + 32].fill(0);
    };

    let mut alloc_id_zero = alloc_two_base.clone();
    write_entry(&mut alloc_id_zero, 0, /*alloc_id=*/ 0, 0x2000, 0x1000);
    write_entry(&mut alloc_id_zero, 1, /*alloc_id=*/ 1, 0x3000, 0x1000);
    fuzz_alloc_table(&alloc_id_zero);

    let mut alloc_size_zero = alloc_two_base.clone();
    write_entry(
        &mut alloc_size_zero,
        0,
        /*alloc_id=*/ 1,
        0x2000,
        /*size_bytes=*/ 0,
    );
    write_entry(
        &mut alloc_size_zero,
        1,
        /*alloc_id=*/ 2,
        0x3000,
        0x1000,
    );
    fuzz_alloc_table(&alloc_size_zero);

    let mut alloc_gpa_overflow = alloc_two_base.clone();
    write_entry(
        &mut alloc_gpa_overflow,
        0,
        /*alloc_id=*/ 1,
        u64::MAX - 1,
        /*size_bytes=*/ 2,
    );
    write_entry(
        &mut alloc_gpa_overflow,
        1,
        /*alloc_id=*/ 2,
        0x3000,
        0x1000,
    );
    fuzz_alloc_table(&alloc_gpa_overflow);

    let mut alloc_dup_id = alloc_two_base.clone();
    write_entry(&mut alloc_dup_id, 0, /*alloc_id=*/ 1, 0x2000, 0x1000);
    write_entry(&mut alloc_dup_id, 1, /*alloc_id=*/ 1, 0x4000, 0x1000);
    fuzz_alloc_table(&alloc_dup_id);

    // Exercise additional early validation errors in the executor alloc-table decoder that aren't
    // reachable via the normal `ALLOC_TABLE_GPA`-backed fuzz input path.
    let mut guest_small = VecGuestMemory::new(1);
    let _ = AllocTable::decode_from_guest_memory(
        &mut guest_small,
        /*table_gpa=*/ 0,
        /*size=*/ 0,
    );
    let _ = AllocTable::decode_from_guest_memory(
        &mut guest_small,
        /*table_gpa=*/ 0,
        /*size=*/ 4,
    );
    let _ = AllocTable::decode_from_guest_memory(
        &mut guest_small,
        /*table_gpa=*/ ALLOC_TABLE_GPA,
        /*size=*/ 0,
    );
    let _ = AllocTable::decode_from_guest_memory(
        &mut guest_small,
        /*table_gpa=*/ u64::MAX - 1,
        /*size=*/ 4,
    );
    let _ = AllocTable::decode_from_guest_memory(
        &mut guest_small,
        /*table_gpa=*/ ALLOC_TABLE_GPA,
        /*size=*/ (ring::AerogpuAllocTableHeader::SIZE_BYTES as u32).saturating_sub(1),
    );

    // Deterministic alloc-table decode errors for the canonical protocol decoder.
    let alloc_header_size = ring::AerogpuAllocTableHeader::SIZE_BYTES;
    let alloc_entry_stride = ring::AerogpuAllocEntry::SIZE_BYTES as u32;
    // Bad magic.
    let mut alloc_bad_magic = vec![0u8; alloc_header_size];
    alloc_bad_magic[0..4].copy_from_slice(&0u32.to_le_bytes());
    alloc_bad_magic[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_bad_magic[8..12].copy_from_slice(&(alloc_header_size as u32).to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_bad_magic);
    fuzz_alloc_table(&alloc_bad_magic);
    // Unsupported ABI major.
    let mut alloc_bad_abi = vec![0u8; alloc_header_size];
    alloc_bad_abi[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    let alloc_unsupported_abi_version =
        ((pci::AEROGPU_ABI_MAJOR + 1) << 16) | pci::AEROGPU_ABI_MINOR;
    alloc_bad_abi[4..8].copy_from_slice(&alloc_unsupported_abi_version.to_le_bytes());
    alloc_bad_abi[8..12].copy_from_slice(&(alloc_header_size as u32).to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_bad_abi);
    fuzz_alloc_table(&alloc_bad_abi);
    // size_bytes too small.
    let mut alloc_bad_size_small = vec![0u8; alloc_header_size];
    alloc_bad_size_small[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_bad_size_small[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_bad_size_small[8..12].copy_from_slice(&0u32.to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_bad_size_small);
    fuzz_alloc_table(&alloc_bad_size_small);
    // entry_stride_bytes too small.
    let mut alloc_bad_stride = vec![0u8; alloc_header_size];
    alloc_bad_stride[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_bad_stride[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_bad_stride[8..12].copy_from_slice(&(alloc_header_size as u32).to_le_bytes());
    alloc_bad_stride[16..20].copy_from_slice(&0u32.to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_bad_stride);
    fuzz_alloc_table(&alloc_bad_stride);
    // size_bytes larger than provided buffer.
    let mut alloc_bad_size_large = vec![0u8; alloc_header_size];
    alloc_bad_size_large[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_bad_size_large[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_bad_size_large[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    alloc_bad_size_large[16..20].copy_from_slice(&alloc_entry_stride.to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_bad_size_large);
    fuzz_alloc_table(&alloc_bad_size_large);
    // CountOutOfBounds: entry_count indicates an entry, but size_bytes omits the entries region.
    let mut alloc_count_oob = vec![0u8; alloc_header_size + alloc_entry_stride as usize];
    alloc_count_oob[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
    alloc_count_oob[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    alloc_count_oob[8..12].copy_from_slice(&(alloc_header_size as u32).to_le_bytes());
    alloc_count_oob[12..16].copy_from_slice(&1u32.to_le_bytes());
    alloc_count_oob[16..20].copy_from_slice(&alloc_entry_stride.to_le_bytes());
    let _ = ring::decode_alloc_table_le(&alloc_count_oob);
    fuzz_alloc_table(&alloc_count_oob);

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

    // Deterministic ring layout validation errors.
    let mut ring_hdr_bad_entry_count = ring_hdr_bytes;
    ring_hdr_bad_entry_count[12..16].copy_from_slice(&0u32.to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bad_entry_count);
    let mut ring_hdr_bad_stride = ring_hdr_bytes;
    ring_hdr_bad_stride[16..20].copy_from_slice(&0u32.to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bad_stride);
    let mut ring_hdr_bad_size = ring_hdr_bytes;
    ring_hdr_bad_size[8..12]
        .copy_from_slice(&(ring::AerogpuRingHeader::SIZE_BYTES as u32).to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bad_size);
    let mut ring_hdr_bad_abi = ring_hdr_bytes;
    let ring_unsupported_abi_version =
        ((pci::AEROGPU_ABI_MAJOR + 1) << 16) | pci::AEROGPU_ABI_MINOR;
    ring_hdr_bad_abi[4..8].copy_from_slice(&ring_unsupported_abi_version.to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bad_abi);
    let mut ring_hdr_bad_magic = ring_hdr_bytes;
    ring_hdr_bad_magic[0..4].copy_from_slice(&0u32.to_le_bytes());
    fuzz_ring_layouts(&ring_hdr_bad_magic);

    let mut submit_bad = submit_bytes;
    submit_bad[0..4].copy_from_slice(&0u32.to_le_bytes());
    fuzz_ring_layouts(&submit_bad);

    let mut fence_bad_magic = fence_bytes;
    fence_bad_magic[0..4].copy_from_slice(&0u32.to_le_bytes());
    fuzz_ring_layouts(&fence_bad_magic);
    let mut fence_bad_abi = fence_bytes;
    fence_bad_abi[4..8].copy_from_slice(&ring_unsupported_abi_version.to_le_bytes());
    fuzz_ring_layouts(&fence_bad_abi);

    // Fence page writer should gracefully reject too-small buffers.
    let mut too_small = [0u8; ring::AerogpuFencePage::SIZE_BYTES - 1];
    let _ = ring::write_fence_page_completed_fence_le(&mut too_small, 1);
});
