use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_create_input_layout_blob_le, decode_cmd_create_shader_dxbc_payload_le,
    decode_cmd_dispatch_le, decode_cmd_set_shader_resource_buffers_bindings_le,
    decode_cmd_set_unordered_access_buffers_bindings_le, decode_cmd_set_vertex_buffers_bindings_le,
    decode_cmd_upload_resource_payload_le, AerogpuCmdDecodeError, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn pad_to_4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn build_packet(opcode: u32, mut payload: Vec<u8>) -> Vec<u8> {
    pad_to_4(&mut payload);
    let size_bytes = (8 + payload.len()) as u32;
    assert!(size_bytes.is_multiple_of(4));

    let mut packet = Vec::new();
    push_u32(&mut packet, opcode);
    push_u32(&mut packet, size_bytes);
    packet.extend_from_slice(&payload);
    packet
}

fn build_stream(mut packets: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut bytes, 0); // size_bytes (patched later)
    push_u32(&mut bytes, 0); // flags
    push_u32(&mut bytes, 0); // reserved0
    push_u32(&mut bytes, 0); // reserved1

    for packet in packets.drain(..) {
        bytes.extend_from_slice(&packet);
    }

    let size_bytes = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    bytes
}

#[test]
fn iterates_valid_stream_and_decodes_variable_payloads() {
    let dxbc_bytes = b"DXBC!";
    let upload_bytes = b"hello world";
    let input_layout_blob = b"ILAYblob";
    let sampler_handles = [10u32, 11, 12];

    let mut create_shader_payload = Vec::new();
    push_u32(&mut create_shader_payload, 0xAABB_CCDD); // shader_handle
    push_u32(&mut create_shader_payload, 1); // stage
    push_u32(&mut create_shader_payload, dxbc_bytes.len() as u32);
    push_u32(&mut create_shader_payload, 0); // reserved0
    create_shader_payload.extend_from_slice(dxbc_bytes);

    let mut upload_resource_payload = Vec::new();
    push_u32(&mut upload_resource_payload, 0x1122_3344); // resource_handle
    push_u32(&mut upload_resource_payload, 0); // reserved0
    push_u64(&mut upload_resource_payload, 0x10); // offset_bytes
    push_u64(&mut upload_resource_payload, upload_bytes.len() as u64);
    upload_resource_payload.extend_from_slice(upload_bytes);

    let mut create_input_layout_payload = Vec::new();
    push_u32(&mut create_input_layout_payload, 0x5566_7788); // input_layout_handle
    push_u32(
        &mut create_input_layout_payload,
        input_layout_blob.len() as u32,
    );
    push_u32(&mut create_input_layout_payload, 0); // reserved0
    create_input_layout_payload.extend_from_slice(input_layout_blob);

    let mut set_vertex_buffers_payload = Vec::new();
    push_u32(&mut set_vertex_buffers_payload, 2); // start_slot
    push_u32(&mut set_vertex_buffers_payload, 2); // buffer_count
                                                  // binding[0]
    push_u32(&mut set_vertex_buffers_payload, 11); // buffer
    push_u32(&mut set_vertex_buffers_payload, 16); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0
                                                  // binding[1]
    push_u32(&mut set_vertex_buffers_payload, 22); // buffer
    push_u32(&mut set_vertex_buffers_payload, 32); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 4); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0

    let mut set_samplers_payload = Vec::new();
    push_u32(&mut set_samplers_payload, 1); // shader_stage
    push_u32(&mut set_samplers_payload, 2); // start_slot
    push_u32(&mut set_samplers_payload, sampler_handles.len() as u32); // sampler_count
    push_u32(&mut set_samplers_payload, 0); // reserved0
    for h in sampler_handles {
        push_u32(&mut set_samplers_payload, h);
    }

    let mut set_constant_buffers_payload = Vec::new();
    push_u32(&mut set_constant_buffers_payload, 0); // shader_stage
    push_u32(&mut set_constant_buffers_payload, 0); // start_slot
    push_u32(&mut set_constant_buffers_payload, 2); // buffer_count
    push_u32(&mut set_constant_buffers_payload, 0); // reserved0
                                                    // binding[0]
    push_u32(&mut set_constant_buffers_payload, 100); // buffer
    push_u32(&mut set_constant_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_constant_buffers_payload, 64); // size_bytes
    push_u32(&mut set_constant_buffers_payload, 0); // reserved0
                                                    // binding[1]
    push_u32(&mut set_constant_buffers_payload, 101); // buffer
    push_u32(&mut set_constant_buffers_payload, 16); // offset_bytes
    push_u32(&mut set_constant_buffers_payload, 128); // size_bytes
    push_u32(&mut set_constant_buffers_payload, 0); // reserved0

    let stream = build_stream(vec![
        build_packet(AerogpuCmdOpcode::Nop as u32, Vec::new()),
        build_packet(
            AerogpuCmdOpcode::CreateShaderDxbc as u32,
            create_shader_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::UploadResource as u32,
            upload_resource_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::CreateInputLayout as u32,
            create_input_layout_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::SetVertexBuffers as u32,
            set_vertex_buffers_payload,
        ),
        build_packet(AerogpuCmdOpcode::SetSamplers as u32, set_samplers_payload),
        build_packet(
            AerogpuCmdOpcode::SetConstantBuffers as u32,
            set_constant_buffers_payload,
        ),
    ]);

    let iter = AerogpuCmdStreamIter::new(&stream).unwrap();
    let header = *iter.header();
    let magic = header.magic;
    let abi_version = header.abi_version;
    let size_bytes = header.size_bytes;
    assert_eq!(magic, AEROGPU_CMD_STREAM_MAGIC);
    assert_eq!(abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(size_bytes as usize, stream.len());

    let packets = iter.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(packets.len(), 7);
    assert_eq!(packets[0].opcode, Some(AerogpuCmdOpcode::Nop));
    assert!(packets[0].payload.is_empty());

    let (create_shader, parsed_dxbc) = packets[1].decode_create_shader_dxbc_payload_le().unwrap();
    let shader_handle = create_shader.shader_handle;
    let shader_stage = create_shader.stage;
    let shader_dxbc_size_bytes = create_shader.dxbc_size_bytes;
    assert_eq!(shader_handle, 0xAABB_CCDD);
    assert_eq!(shader_stage, 1);
    assert_eq!(shader_dxbc_size_bytes as usize, dxbc_bytes.len());
    assert_eq!(parsed_dxbc, dxbc_bytes);

    let (upload, parsed_upload) = packets[2].decode_upload_resource_payload_le().unwrap();
    let upload_resource_handle = upload.resource_handle;
    let upload_offset_bytes = upload.offset_bytes;
    let upload_size_bytes = upload.size_bytes;
    assert_eq!(upload_resource_handle, 0x1122_3344);
    assert_eq!(upload_offset_bytes, 0x10);
    assert_eq!(upload_size_bytes as usize, upload_bytes.len());
    assert_eq!(parsed_upload, upload_bytes);

    let (create_layout, parsed_blob) = packets[3].decode_create_input_layout_payload_le().unwrap();
    let input_layout_handle = create_layout.input_layout_handle;
    let input_layout_blob_size_bytes = create_layout.blob_size_bytes;
    assert_eq!(input_layout_handle, 0x5566_7788);
    assert_eq!(
        input_layout_blob_size_bytes as usize,
        input_layout_blob.len()
    );
    assert_eq!(parsed_blob, input_layout_blob);

    let (set_vbs, bindings) = packets[4].decode_set_vertex_buffers_payload_le().unwrap();
    let start_slot = set_vbs.start_slot;
    let buffer_count = set_vbs.buffer_count;
    assert_eq!(start_slot, 2);
    assert_eq!(buffer_count, 2);
    assert_eq!(bindings.len(), 2);
    let binding0_buffer = bindings[0].buffer;
    let binding0_stride = bindings[0].stride_bytes;
    let binding0_offset = bindings[0].offset_bytes;
    let binding1_buffer = bindings[1].buffer;
    let binding1_stride = bindings[1].stride_bytes;
    let binding1_offset = bindings[1].offset_bytes;
    assert_eq!(binding0_buffer, 11);
    assert_eq!(binding0_stride, 16);
    assert_eq!(binding0_offset, 0);
    assert_eq!(binding1_buffer, 22);
    assert_eq!(binding1_stride, 32);
    assert_eq!(binding1_offset, 4);

    let (set_samplers, handles) = packets[5].decode_set_samplers_payload_le().unwrap();
    let shader_stage = set_samplers.shader_stage;
    let start_slot = set_samplers.start_slot;
    let sampler_count = set_samplers.sampler_count;
    assert_eq!(shader_stage, 1);
    assert_eq!(start_slot, 2);
    assert_eq!(sampler_count, 3);
    assert_eq!(handles, &[10u32, 11, 12]);

    let (set_cbs, bindings) = packets[6].decode_set_constant_buffers_payload_le().unwrap();
    let shader_stage = set_cbs.shader_stage;
    let start_slot = set_cbs.start_slot;
    let buffer_count = set_cbs.buffer_count;
    assert_eq!(shader_stage, 0);
    assert_eq!(start_slot, 0);
    assert_eq!(buffer_count, 2);
    assert_eq!(bindings.len(), 2);
    let binding0_buffer = bindings[0].buffer;
    let binding0_offset_bytes = bindings[0].offset_bytes;
    let binding0_size_bytes = bindings[0].size_bytes;
    let binding1_buffer = bindings[1].buffer;
    let binding1_offset_bytes = bindings[1].offset_bytes;
    let binding1_size_bytes = bindings[1].size_bytes;
    assert_eq!(binding0_buffer, 100);
    assert_eq!(binding0_offset_bytes, 0);
    assert_eq!(binding0_size_bytes, 64);
    assert_eq!(binding1_buffer, 101);
    assert_eq!(binding1_offset_bytes, 16);
    assert_eq!(binding1_size_bytes, 128);
}

#[test]
fn variable_payload_decoders_allow_trailing_bytes() {
    // All payload sizes are multiples of 4 so the additional u32 is unambiguously a trailing
    // extension field (not alignment padding).
    let dxbc_bytes = b"DXBC1234";
    let upload_bytes = b"DATA5678";
    let input_layout_blob = b"ILAYBLOB";

    let mut create_shader_payload = Vec::new();
    push_u32(&mut create_shader_payload, 0xAABB_CCDD); // shader_handle
    push_u32(&mut create_shader_payload, 1); // stage
    push_u32(&mut create_shader_payload, dxbc_bytes.len() as u32);
    push_u32(&mut create_shader_payload, 0); // reserved0
    create_shader_payload.extend_from_slice(dxbc_bytes);
    push_u32(&mut create_shader_payload, 0xDEAD_BEEF); // trailing extension

    let mut upload_resource_payload = Vec::new();
    push_u32(&mut upload_resource_payload, 0x1122_3344); // resource_handle
    push_u32(&mut upload_resource_payload, 0); // reserved0
    push_u64(&mut upload_resource_payload, 0x10); // offset_bytes
    push_u64(&mut upload_resource_payload, upload_bytes.len() as u64);
    upload_resource_payload.extend_from_slice(upload_bytes);
    push_u32(&mut upload_resource_payload, 0xDEAD_BEEF); // trailing extension

    let mut create_input_layout_payload = Vec::new();
    push_u32(&mut create_input_layout_payload, 0x5566_7788); // input_layout_handle
    push_u32(
        &mut create_input_layout_payload,
        input_layout_blob.len() as u32,
    );
    push_u32(&mut create_input_layout_payload, 0); // reserved0
    create_input_layout_payload.extend_from_slice(input_layout_blob);
    push_u32(&mut create_input_layout_payload, 0xDEAD_BEEF); // trailing extension

    let mut set_vertex_buffers_payload = Vec::new();
    push_u32(&mut set_vertex_buffers_payload, 2); // start_slot
    push_u32(&mut set_vertex_buffers_payload, 1); // buffer_count

    // binding[0]
    push_u32(&mut set_vertex_buffers_payload, 11); // buffer
    push_u32(&mut set_vertex_buffers_payload, 16); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0
    push_u32(&mut set_vertex_buffers_payload, 0xDEAD_BEEF); // trailing extension

    let mut set_samplers_payload = Vec::new();
    push_u32(&mut set_samplers_payload, 1); // shader_stage
    push_u32(&mut set_samplers_payload, 0); // start_slot
    push_u32(&mut set_samplers_payload, 1); // sampler_count
    push_u32(&mut set_samplers_payload, 0); // reserved0
    push_u32(&mut set_samplers_payload, 10); // handles[0]
    push_u32(&mut set_samplers_payload, 0xDEAD_BEEF); // trailing extension

    let mut set_constant_buffers_payload = Vec::new();
    push_u32(&mut set_constant_buffers_payload, 0); // shader_stage
    push_u32(&mut set_constant_buffers_payload, 0); // start_slot
    push_u32(&mut set_constant_buffers_payload, 1); // buffer_count
    push_u32(&mut set_constant_buffers_payload, 0); // reserved0
                                                    // binding[0]
    push_u32(&mut set_constant_buffers_payload, 100); // buffer
    push_u32(&mut set_constant_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_constant_buffers_payload, 64); // size_bytes
    push_u32(&mut set_constant_buffers_payload, 0); // reserved0
    push_u32(&mut set_constant_buffers_payload, 0xDEAD_BEEF); // trailing extension

    let mut set_srvs_payload = Vec::new();
    push_u32(&mut set_srvs_payload, 1); // shader_stage
    push_u32(&mut set_srvs_payload, 0); // start_slot
    push_u32(&mut set_srvs_payload, 1); // buffer_count
    push_u32(&mut set_srvs_payload, 0); // reserved0
                                        // binding[0]
    push_u32(&mut set_srvs_payload, 10); // buffer
    push_u32(&mut set_srvs_payload, 0); // offset_bytes
    push_u32(&mut set_srvs_payload, 64); // size_bytes
    push_u32(&mut set_srvs_payload, 0); // reserved0
    push_u32(&mut set_srvs_payload, 0xDEAD_BEEF); // trailing extension

    let mut set_uavs_payload = Vec::new();
    push_u32(&mut set_uavs_payload, 2); // shader_stage
    push_u32(&mut set_uavs_payload, 0); // start_slot
    push_u32(&mut set_uavs_payload, 1); // uav_count
    push_u32(&mut set_uavs_payload, 0); // reserved0
                                        // binding[0]
    push_u32(&mut set_uavs_payload, 20); // buffer
    push_u32(&mut set_uavs_payload, 4); // offset_bytes
    push_u32(&mut set_uavs_payload, 128); // size_bytes
    push_u32(&mut set_uavs_payload, 0); // initial_count
    push_u32(&mut set_uavs_payload, 0xDEAD_BEEF); // trailing extension

    let stream = build_stream(vec![
        build_packet(
            AerogpuCmdOpcode::CreateShaderDxbc as u32,
            create_shader_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::UploadResource as u32,
            upload_resource_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::CreateInputLayout as u32,
            create_input_layout_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::SetVertexBuffers as u32,
            set_vertex_buffers_payload,
        ),
        build_packet(AerogpuCmdOpcode::SetSamplers as u32, set_samplers_payload),
        build_packet(
            AerogpuCmdOpcode::SetConstantBuffers as u32,
            set_constant_buffers_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
            set_srvs_payload,
        ),
        build_packet(
            AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
            set_uavs_payload,
        ),
    ]);

    let packets = AerogpuCmdStreamIter::new(&stream)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(packets.len(), 8);

    let (_create_shader, parsed_dxbc) = packets[0].decode_create_shader_dxbc_payload_le().unwrap();
    assert_eq!(parsed_dxbc, dxbc_bytes);

    let (_upload, parsed_upload) = packets[1].decode_upload_resource_payload_le().unwrap();
    assert_eq!(parsed_upload, upload_bytes);

    let (_create_layout, parsed_blob) = packets[2].decode_create_input_layout_payload_le().unwrap();
    assert_eq!(parsed_blob, input_layout_blob);

    let (_set_vbs, bindings) = packets[3].decode_set_vertex_buffers_payload_le().unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 11);

    let (_set_samplers, handles) = packets[4].decode_set_samplers_payload_le().unwrap();
    assert_eq!(handles, &[10u32]);

    let (_set_cbs, bindings) = packets[5].decode_set_constant_buffers_payload_le().unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 100);

    let (_set_srvs, bindings) = packets[6]
        .decode_set_shader_resource_buffers_payload_le()
        .unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 10);

    let (_set_uavs, bindings) = packets[7]
        .decode_set_unordered_access_buffers_payload_le()
        .unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 20);
}

#[test]
fn unknown_opcode_is_preserved_and_skipped() {
    let unknown_payload = vec![1u8, 2, 3, 4];
    let stream = build_stream(vec![
        build_packet(AerogpuCmdOpcode::Nop as u32, Vec::new()),
        build_packet(0xDEAD_BEEF, unknown_payload.clone()),
        build_packet(AerogpuCmdOpcode::Nop as u32, Vec::new()),
    ]);

    let packets = AerogpuCmdStreamIter::new(&stream)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(packets.len(), 3);
    assert_eq!(packets[1].opcode, None);
    assert_eq!(packets[1].payload, unknown_payload);
    assert_eq!(packets[2].opcode, Some(AerogpuCmdOpcode::Nop));
}

#[test]
fn stream_size_bytes_allows_trailing_buffer_bytes() {
    // `AerogpuCmdStreamHeader::size_bytes` describes how many bytes are part of the stream. The
    // caller may provide a larger buffer (e.g. a fixed-size ring slot), so the iterator should
    // ignore bytes beyond `size_bytes`.
    let mut stream = build_stream(vec![build_packet(AerogpuCmdOpcode::Nop as u32, Vec::new())]);
    stream.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

    let packets = AerogpuCmdStreamIter::new(&stream)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].opcode, Some(AerogpuCmdOpcode::Nop));
    assert!(packets[0].payload.is_empty());
}

#[test]
fn stream_size_bytes_misaligned_is_an_error() {
    let mut stream = build_stream(vec![build_packet(AerogpuCmdOpcode::Nop as u32, Vec::new())]);
    // Set size_bytes to a value that is within the provided buffer but not 4-byte aligned.
    let misaligned = (stream.len() as u32).saturating_sub(1);
    assert!(misaligned >= AerogpuCmdStreamHeader::SIZE_BYTES as u32);
    assert!(!misaligned.is_multiple_of(4));
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&misaligned.to_le_bytes());

    let err = match AerogpuCmdStreamIter::new(&stream) {
        Ok(_) => panic!("expected SizeNotAligned error"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        AerogpuCmdDecodeError::SizeNotAligned { found } if found == misaligned
    ));
}

#[test]
fn packet_size_bytes_misaligned_is_an_error() {
    let mut packet = Vec::new();
    push_u32(&mut packet, AerogpuCmdOpcode::Nop as u32);
    push_u32(&mut packet, 10); // misaligned size_bytes
    packet.extend_from_slice(&[0u8; 2]);

    let mut stream = build_stream(vec![packet]);
    // Stream header size_bytes must be 4-byte aligned even when the packet itself is malformed.
    // Treat the extra bytes at the end as trailing buffer padding (outside `size_bytes`).
    let aligned_size = (stream.len() as u32) & !3u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&aligned_size.to_le_bytes());
    let mut iter = AerogpuCmdStreamIter::new(&stream).unwrap();
    match iter.next() {
        Some(Err(err)) => assert!(matches!(
            err,
            AerogpuCmdDecodeError::SizeNotAligned { found: 10 }
        )),
        Some(Ok(_)) => panic!("expected SizeNotAligned error"),
        None => panic!("expected SizeNotAligned error"),
    }
}

#[test]
fn packet_size_bytes_overruns_stream_is_an_error() {
    let mut packet = Vec::new();
    push_u32(&mut packet, AerogpuCmdOpcode::Nop as u32);
    push_u32(&mut packet, 12); // claims an extra 4 bytes that aren't present

    let stream = build_stream(vec![packet]);
    let mut iter = AerogpuCmdStreamIter::new(&stream).unwrap();
    match iter.next() {
        Some(Err(err)) => assert!(matches!(
            err,
            AerogpuCmdDecodeError::PacketOverrunsStream {
                offset: 24,
                packet_size_bytes: 12,
                ..
            }
        )),
        Some(Ok(_)) => panic!("expected PacketOverrunsStream error"),
        None => panic!("expected PacketOverrunsStream error"),
    }
}

#[test]
fn create_shader_dxbc_payload_size_mismatch_is_rejected() {
    let dxbc_bytes = b"abcd";

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // shader_handle
    push_u32(&mut payload, 0); // stage
    push_u32(&mut payload, 8); // dxbc_size_bytes (does not match actual payload)
    push_u32(&mut payload, 0); // reserved0
    payload.extend_from_slice(dxbc_bytes);

    let stream = build_stream(vec![build_packet(
        AerogpuCmdOpcode::CreateShaderDxbc as u32,
        payload,
    )]);

    let packets = AerogpuCmdStreamIter::new(&stream)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(matches!(
        packets[0].decode_create_shader_dxbc_payload_le(),
        Err(AerogpuCmdDecodeError::PayloadSizeMismatch { .. })
    ));
}

#[test]
fn set_vertex_buffers_count_mismatch_is_rejected() {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0); // start_slot
    push_u32(&mut payload, 2); // buffer_count (but only 1 binding follows)
    push_u32(&mut payload, 1); // buffer
    push_u32(&mut payload, 16); // stride_bytes
    push_u32(&mut payload, 0); // offset_bytes
    push_u32(&mut payload, 0); // reserved0

    let stream = build_stream(vec![build_packet(
        AerogpuCmdOpcode::SetVertexBuffers as u32,
        payload,
    )]);
    let packets = AerogpuCmdStreamIter::new(&stream)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(matches!(
        packets[0].decode_set_vertex_buffers_payload_le(),
        Err(AerogpuCmdDecodeError::PayloadSizeMismatch { .. })
    ));
}

#[test]
fn free_function_payload_decoders_allow_trailing_bytes() {
    // All payload sizes are multiples of 4 so the additional u32 is unambiguously a trailing
    // extension field (not alignment padding).
    let dxbc_bytes = b"DXBC1234";
    let upload_bytes = b"DATA5678";
    let input_layout_blob = b"ILAYBLOB";

    let mut create_shader_payload = Vec::new();
    push_u32(&mut create_shader_payload, 0xAABB_CCDD); // shader_handle
    push_u32(&mut create_shader_payload, 1); // stage
    push_u32(&mut create_shader_payload, dxbc_bytes.len() as u32);
    push_u32(&mut create_shader_payload, 0); // reserved0
    create_shader_payload.extend_from_slice(dxbc_bytes);
    push_u32(&mut create_shader_payload, 0xDEAD_BEEF); // trailing extension
    let create_shader_packet = build_packet(
        AerogpuCmdOpcode::CreateShaderDxbc as u32,
        create_shader_payload,
    );

    let (_cmd, parsed_dxbc) =
        decode_cmd_create_shader_dxbc_payload_le(&create_shader_packet).unwrap();
    assert_eq!(parsed_dxbc, dxbc_bytes);

    let mut upload_resource_payload = Vec::new();
    push_u32(&mut upload_resource_payload, 0x1122_3344); // resource_handle
    push_u32(&mut upload_resource_payload, 0); // reserved0
    push_u64(&mut upload_resource_payload, 0x10); // offset_bytes
    push_u64(&mut upload_resource_payload, upload_bytes.len() as u64);
    upload_resource_payload.extend_from_slice(upload_bytes);
    push_u32(&mut upload_resource_payload, 0xDEAD_BEEF); // trailing extension
    let upload_packet = build_packet(
        AerogpuCmdOpcode::UploadResource as u32,
        upload_resource_payload,
    );

    let (_cmd, parsed_upload) = decode_cmd_upload_resource_payload_le(&upload_packet).unwrap();
    assert_eq!(parsed_upload, upload_bytes);

    let mut create_input_layout_payload = Vec::new();
    push_u32(&mut create_input_layout_payload, 0x5566_7788); // input_layout_handle
    push_u32(
        &mut create_input_layout_payload,
        input_layout_blob.len() as u32,
    );
    push_u32(&mut create_input_layout_payload, 0); // reserved0
    create_input_layout_payload.extend_from_slice(input_layout_blob);
    push_u32(&mut create_input_layout_payload, 0xDEAD_BEEF); // trailing extension
    let input_layout_packet = build_packet(
        AerogpuCmdOpcode::CreateInputLayout as u32,
        create_input_layout_payload,
    );

    let (_cmd, parsed_blob) = decode_cmd_create_input_layout_blob_le(&input_layout_packet).unwrap();
    assert_eq!(parsed_blob, input_layout_blob);

    let mut set_vertex_buffers_payload = Vec::new();
    push_u32(&mut set_vertex_buffers_payload, 2); // start_slot
    push_u32(&mut set_vertex_buffers_payload, 1); // buffer_count
                                                  // binding[0]
    push_u32(&mut set_vertex_buffers_payload, 11); // buffer
    push_u32(&mut set_vertex_buffers_payload, 16); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0
    push_u32(&mut set_vertex_buffers_payload, 0xDEAD_BEEF); // trailing extension
    let set_vbs_packet = build_packet(
        AerogpuCmdOpcode::SetVertexBuffers as u32,
        set_vertex_buffers_payload,
    );

    let (_cmd, bindings) = decode_cmd_set_vertex_buffers_bindings_le(&set_vbs_packet).unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 11);

    let mut set_srvs_payload = Vec::new();
    push_u32(&mut set_srvs_payload, 1); // shader_stage
    push_u32(&mut set_srvs_payload, 0); // start_slot
    push_u32(&mut set_srvs_payload, 1); // buffer_count
    push_u32(&mut set_srvs_payload, 0); // reserved0
                                        // binding[0]
    push_u32(&mut set_srvs_payload, 10); // buffer
    push_u32(&mut set_srvs_payload, 0); // offset_bytes
    push_u32(&mut set_srvs_payload, 64); // size_bytes
    push_u32(&mut set_srvs_payload, 0); // reserved0
    push_u32(&mut set_srvs_payload, 0xDEAD_BEEF); // trailing extension
    let set_srvs_packet = build_packet(
        AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
        set_srvs_payload,
    );

    let (_cmd, bindings) =
        decode_cmd_set_shader_resource_buffers_bindings_le(&set_srvs_packet).unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 10);

    let mut set_uavs_payload = Vec::new();
    push_u32(&mut set_uavs_payload, 2); // shader_stage (compute)
    push_u32(&mut set_uavs_payload, 0); // start_slot
    push_u32(&mut set_uavs_payload, 1); // uav_count
    push_u32(&mut set_uavs_payload, 0); // reserved0
                                        // binding[0]
    push_u32(&mut set_uavs_payload, 20); // buffer
    push_u32(&mut set_uavs_payload, 4); // offset_bytes
    push_u32(&mut set_uavs_payload, 128); // size_bytes
    push_u32(&mut set_uavs_payload, 0); // initial_count
    push_u32(&mut set_uavs_payload, 0xDEAD_BEEF); // trailing extension
    let set_uavs_packet = build_packet(
        AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
        set_uavs_payload,
    );

    let (_cmd, bindings) =
        decode_cmd_set_unordered_access_buffers_bindings_le(&set_uavs_packet).unwrap();
    assert_eq!(bindings.len(), 1);
    let buffer = bindings[0].buffer;
    assert_eq!(buffer, 20);

    let mut dispatch_payload = Vec::new();
    push_u32(&mut dispatch_payload, 1); // group_count_x
    push_u32(&mut dispatch_payload, 2); // group_count_y
    push_u32(&mut dispatch_payload, 3); // group_count_z
    push_u32(&mut dispatch_payload, 0); // reserved0
    push_u32(&mut dispatch_payload, 0xDEAD_BEEF); // trailing extension
    let dispatch_packet = build_packet(AerogpuCmdOpcode::Dispatch as u32, dispatch_payload);

    let dispatch = decode_cmd_dispatch_le(&dispatch_packet).unwrap();
    let group_count_x = dispatch.group_count_x;
    let group_count_y = dispatch.group_count_y;
    let group_count_z = dispatch.group_count_z;
    assert_eq!(group_count_x, 1);
    assert_eq!(group_count_y, 2);
    assert_eq!(group_count_z, 3);
}

#[test]
fn free_function_payload_decoders_match_packet_helpers() {
    let dxbc_bytes = b"DXBC!";
    let upload_bytes = b"hello world";
    let input_layout_blob = b"ILAYblob";

    let mut create_shader_payload = Vec::new();
    push_u32(&mut create_shader_payload, 0xAABB_CCDD); // shader_handle
    push_u32(&mut create_shader_payload, 1); // stage
    push_u32(&mut create_shader_payload, dxbc_bytes.len() as u32);
    push_u32(&mut create_shader_payload, 0); // reserved0
    create_shader_payload.extend_from_slice(dxbc_bytes);
    let create_shader_packet = build_packet(
        AerogpuCmdOpcode::CreateShaderDxbc as u32,
        create_shader_payload,
    );

    let (create_shader, parsed_dxbc) =
        decode_cmd_create_shader_dxbc_payload_le(&create_shader_packet).unwrap();
    let shader_handle = create_shader.shader_handle;
    let stage = create_shader.stage;
    let dxbc_size_bytes = create_shader.dxbc_size_bytes;
    assert_eq!(shader_handle, 0xAABB_CCDD);
    assert_eq!(stage, 1);
    assert_eq!(dxbc_size_bytes as usize, dxbc_bytes.len());
    assert_eq!(parsed_dxbc, dxbc_bytes);

    let mut upload_resource_payload = Vec::new();
    push_u32(&mut upload_resource_payload, 0x1122_3344); // resource_handle
    push_u32(&mut upload_resource_payload, 0); // reserved0
    push_u64(&mut upload_resource_payload, 0x10); // offset_bytes
    push_u64(&mut upload_resource_payload, upload_bytes.len() as u64);
    upload_resource_payload.extend_from_slice(upload_bytes);
    let upload_packet = build_packet(
        AerogpuCmdOpcode::UploadResource as u32,
        upload_resource_payload,
    );

    let (upload, parsed_upload) = decode_cmd_upload_resource_payload_le(&upload_packet).unwrap();
    let resource_handle = upload.resource_handle;
    let offset_bytes = upload.offset_bytes;
    let size_bytes = upload.size_bytes;
    assert_eq!(resource_handle, 0x1122_3344);
    assert_eq!(offset_bytes, 0x10);
    assert_eq!(size_bytes as usize, upload_bytes.len());
    assert_eq!(parsed_upload, upload_bytes);

    let mut create_input_layout_payload = Vec::new();
    push_u32(&mut create_input_layout_payload, 0x5566_7788); // input_layout_handle
    push_u32(
        &mut create_input_layout_payload,
        input_layout_blob.len() as u32,
    );
    push_u32(&mut create_input_layout_payload, 0); // reserved0
    create_input_layout_payload.extend_from_slice(input_layout_blob);
    let input_layout_packet = build_packet(
        AerogpuCmdOpcode::CreateInputLayout as u32,
        create_input_layout_payload,
    );

    let (create_layout, parsed_blob) =
        decode_cmd_create_input_layout_blob_le(&input_layout_packet).unwrap();
    let input_layout_handle = create_layout.input_layout_handle;
    let blob_size_bytes = create_layout.blob_size_bytes;
    assert_eq!(input_layout_handle, 0x5566_7788);
    assert_eq!(blob_size_bytes as usize, input_layout_blob.len());
    assert_eq!(parsed_blob, input_layout_blob);

    let mut set_vertex_buffers_payload = Vec::new();
    push_u32(&mut set_vertex_buffers_payload, 2); // start_slot
    push_u32(&mut set_vertex_buffers_payload, 2); // buffer_count
                                                  // binding[0]
    push_u32(&mut set_vertex_buffers_payload, 11); // buffer
    push_u32(&mut set_vertex_buffers_payload, 16); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0
                                                  // binding[1]
    push_u32(&mut set_vertex_buffers_payload, 22); // buffer
    push_u32(&mut set_vertex_buffers_payload, 32); // stride_bytes
    push_u32(&mut set_vertex_buffers_payload, 4); // offset_bytes
    push_u32(&mut set_vertex_buffers_payload, 0); // reserved0
    let set_vbs_packet = build_packet(
        AerogpuCmdOpcode::SetVertexBuffers as u32,
        set_vertex_buffers_payload,
    );

    let (set_vbs, bindings) = decode_cmd_set_vertex_buffers_bindings_le(&set_vbs_packet).unwrap();
    let start_slot = set_vbs.start_slot;
    let buffer_count = set_vbs.buffer_count;
    assert_eq!(start_slot, 2);
    assert_eq!(buffer_count, 2);
    assert_eq!(bindings.len(), 2);
    let binding0_buffer = bindings[0].buffer;
    let binding1_buffer = bindings[1].buffer;
    assert_eq!(binding0_buffer, 11);
    assert_eq!(binding1_buffer, 22);
}
