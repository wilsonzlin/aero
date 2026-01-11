use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_create_input_layout_blob_le, decode_cmd_create_shader_dxbc_payload_le,
    decode_cmd_set_vertex_buffers_bindings_le, decode_cmd_upload_resource_payload_le,
    AerogpuCmdDecodeError, AerogpuCmdOpcode, AerogpuCmdStreamIter, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

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
    assert_eq!(size_bytes % 4, 0);

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
    bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    bytes
}

#[test]
fn iterates_valid_stream_and_decodes_variable_payloads() {
    let dxbc_bytes = b"DXBC!";
    let upload_bytes = b"hello world";
    let input_layout_blob = b"ILAYblob";

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
    assert_eq!(packets.len(), 5);
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
fn packet_size_bytes_misaligned_is_an_error() {
    let mut packet = Vec::new();
    push_u32(&mut packet, AerogpuCmdOpcode::Nop as u32);
    push_u32(&mut packet, 10); // misaligned size_bytes
    packet.extend_from_slice(&[0u8; 2]);

    let stream = build_stream(vec![packet]);
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
