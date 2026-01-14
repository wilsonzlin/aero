use core::mem::offset_of;

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_cmd_set_constant_buffers_bindings_le,
    decode_cmd_set_shader_resource_buffers_bindings_le,
    decode_cmd_set_samplers_handles_le, AerogpuCmdDecodeError, AerogpuCmdOpcode,
    decode_cmd_set_unordered_access_buffers_bindings_le, AerogpuCmdSetConstantBuffers,
    AerogpuCmdSetSamplers, AerogpuCmdSetShaderResourceBuffers, AerogpuCmdSetUnorderedAccessBuffers,
    AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuConstantBufferBinding,
    AerogpuSamplerAddressMode, AerogpuSamplerFilter, AerogpuShaderResourceBufferBinding,
    AerogpuShaderStage, AerogpuUnorderedAccessBufferBinding,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn binding_table_payloads_round_trip_via_packet_and_free_function_decoders() {
    let mut w = AerogpuCmdWriter::new();

    w.create_sampler(
        1,
        AerogpuSamplerFilter::Linear,
        AerogpuSamplerAddressMode::Repeat,
        AerogpuSamplerAddressMode::ClampToEdge,
        AerogpuSamplerAddressMode::MirrorRepeat,
    );

    let samplers: [u32; 3] = [10, 11, 12];
    w.set_samplers(AerogpuShaderStage::Pixel, 2, &samplers);

    let cbs: [AerogpuConstantBufferBinding; 2] = [
        AerogpuConstantBufferBinding {
            buffer: 100,
            offset_bytes: 0,
            size_bytes: 64,
            reserved0: 0,
        },
        AerogpuConstantBufferBinding {
            buffer: 101,
            offset_bytes: 16,
            size_bytes: 128,
            reserved0: 0,
        },
    ];
    w.set_constant_buffers(AerogpuShaderStage::Vertex, 0, &cbs);

    w.destroy_sampler(1);
    w.flush();

    let bytes = w.finish();

    let packets = AerogpuCmdStreamIter::new(&bytes)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(packets.len(), 5);
    assert_eq!(packets[0].opcode, Some(AerogpuCmdOpcode::CreateSampler));
    assert_eq!(packets[1].opcode, Some(AerogpuCmdOpcode::SetSamplers));
    assert_eq!(
        packets[2].opcode,
        Some(AerogpuCmdOpcode::SetConstantBuffers)
    );
    assert_eq!(packets[3].opcode, Some(AerogpuCmdOpcode::DestroySampler));
    assert_eq!(packets[4].opcode, Some(AerogpuCmdOpcode::Flush));

    // Packet helpers.
    {
        let (cmd, handles) = packets[1].decode_set_samplers_payload_le().unwrap();
        let shader_stage = cmd.shader_stage;
        let start_slot = cmd.start_slot;
        let sampler_count = cmd.sampler_count;
        assert_eq!(shader_stage, AerogpuShaderStage::Pixel as u32);
        assert_eq!(start_slot, 2);
        assert_eq!(sampler_count, samplers.len() as u32);
        assert_eq!(handles, &samplers);
    }
    {
        let (cmd, bindings) = packets[2].decode_set_constant_buffers_payload_le().unwrap();
        let shader_stage = cmd.shader_stage;
        let start_slot = cmd.start_slot;
        let buffer_count = cmd.buffer_count;
        assert_eq!(shader_stage, AerogpuShaderStage::Vertex as u32);
        assert_eq!(start_slot, 0);
        assert_eq!(buffer_count, cbs.len() as u32);
        assert_eq!(bindings.len(), cbs.len());
        let b0_buffer = bindings[0].buffer;
        let b0_offset_bytes = bindings[0].offset_bytes;
        let b0_size_bytes = bindings[0].size_bytes;
        let b1_buffer = bindings[1].buffer;
        let b1_offset_bytes = bindings[1].offset_bytes;
        let b1_size_bytes = bindings[1].size_bytes;
        assert_eq!(b0_buffer, 100);
        assert_eq!(b0_offset_bytes, 0);
        assert_eq!(b0_size_bytes, 64);
        assert_eq!(b1_buffer, 101);
        assert_eq!(b1_offset_bytes, 16);
        assert_eq!(b1_size_bytes, 128);
    }

    // Free-function decoders should agree with the packet helpers.
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // Skip CREATE_SAMPLER.
    cursor += decode_cmd_hdr_le(&bytes[cursor..]).unwrap().size_bytes as usize;

    // SET_SAMPLERS.
    let set_samplers_hdr = decode_cmd_hdr_le(&bytes[cursor..]).unwrap();
    let set_samplers_pkt = &bytes[cursor..cursor + set_samplers_hdr.size_bytes as usize];
    let (cmd, handles) = decode_cmd_set_samplers_handles_le(set_samplers_pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let start_slot = cmd.start_slot;
    assert_eq!(shader_stage, AerogpuShaderStage::Pixel as u32);
    assert_eq!(start_slot, 2);
    assert_eq!(handles, &samplers);
    cursor += set_samplers_hdr.size_bytes as usize;

    // SET_CONSTANT_BUFFERS.
    let set_cbs_hdr = decode_cmd_hdr_le(&bytes[cursor..]).unwrap();
    let set_cbs_pkt = &bytes[cursor..cursor + set_cbs_hdr.size_bytes as usize];
    let (cmd, bindings) = decode_cmd_set_constant_buffers_bindings_le(set_cbs_pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let start_slot = cmd.start_slot;
    assert_eq!(shader_stage, AerogpuShaderStage::Vertex as u32);
    assert_eq!(start_slot, 0);
    assert_eq!(bindings.len(), cbs.len());
    let b0_buffer = bindings[0].buffer;
    let b1_buffer = bindings[1].buffer;
    assert_eq!(b0_buffer, 100);
    assert_eq!(b1_buffer, 101);
}

#[test]
fn srv_uav_binding_table_payloads_round_trip_via_packet_and_free_function_decoders() {
    let mut w = AerogpuCmdWriter::new();

    let srvs: [AerogpuShaderResourceBufferBinding; 2] = [
        AerogpuShaderResourceBufferBinding {
            buffer: 10,
            offset_bytes: 0,
            size_bytes: 64,
            reserved0: 0,
        },
        AerogpuShaderResourceBufferBinding {
            buffer: 11,
            offset_bytes: 16,
            size_bytes: 0,
            reserved0: 0,
        },
    ];
    w.set_shader_resource_buffers(AerogpuShaderStage::Pixel, 1, &srvs);

    let uavs: [AerogpuUnorderedAccessBufferBinding; 1] = [AerogpuUnorderedAccessBufferBinding {
        buffer: 20,
        offset_bytes: 4,
        size_bytes: 128,
        initial_count: 0,
    }];
    w.set_unordered_access_buffers(AerogpuShaderStage::Compute, 0, &uavs);

    w.flush();

    let bytes = w.finish();

    let packets = AerogpuCmdStreamIter::new(&bytes)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(packets.len(), 3);
    assert_eq!(
        packets[0].opcode,
        Some(AerogpuCmdOpcode::SetShaderResourceBuffers)
    );
    assert_eq!(
        packets[1].opcode,
        Some(AerogpuCmdOpcode::SetUnorderedAccessBuffers)
    );
    assert_eq!(packets[2].opcode, Some(AerogpuCmdOpcode::Flush));

    // Packet helpers.
    {
        let (cmd, bindings) = packets[0]
            .decode_set_shader_resource_buffers_payload_le()
            .unwrap();
        let shader_stage = cmd.shader_stage;
        let start_slot = cmd.start_slot;
        let buffer_count = cmd.buffer_count;
        let reserved0 = cmd.reserved0;
        assert_eq!(shader_stage, AerogpuShaderStage::Pixel as u32);
        assert_eq!(start_slot, 1);
        assert_eq!(buffer_count, srvs.len() as u32);
        assert_eq!(reserved0, 0);
        assert_eq!(bindings.len(), srvs.len());
        let b0_buffer = bindings[0].buffer;
        let b0_offset_bytes = bindings[0].offset_bytes;
        let b0_size_bytes = bindings[0].size_bytes;
        let b1_buffer = bindings[1].buffer;
        let b1_offset_bytes = bindings[1].offset_bytes;
        let b1_size_bytes = bindings[1].size_bytes;
        assert_eq!(b0_buffer, 10);
        assert_eq!(b0_offset_bytes, 0);
        assert_eq!(b0_size_bytes, 64);
        assert_eq!(b1_buffer, 11);
        assert_eq!(b1_offset_bytes, 16);
        assert_eq!(b1_size_bytes, 0);
    }
    {
        let (cmd, bindings) = packets[1]
            .decode_set_unordered_access_buffers_payload_le()
            .unwrap();
        let shader_stage = cmd.shader_stage;
        let start_slot = cmd.start_slot;
        let uav_count = cmd.uav_count;
        let reserved0 = cmd.reserved0;
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(start_slot, 0);
        assert_eq!(uav_count, uavs.len() as u32);
        assert_eq!(reserved0, 0);
        assert_eq!(bindings.len(), uavs.len());
        let b0_buffer = bindings[0].buffer;
        let b0_offset_bytes = bindings[0].offset_bytes;
        let b0_size_bytes = bindings[0].size_bytes;
        let b0_initial_count = bindings[0].initial_count;
        assert_eq!(b0_buffer, 20);
        assert_eq!(b0_offset_bytes, 4);
        assert_eq!(b0_size_bytes, 128);
        assert_eq!(b0_initial_count, 0);
    }

    // Free-function decoders should agree with the packet helpers.
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_SHADER_RESOURCE_BUFFERS.
    let set_srvs_hdr = decode_cmd_hdr_le(&bytes[cursor..]).unwrap();
    let set_srvs_pkt = &bytes[cursor..cursor + set_srvs_hdr.size_bytes as usize];
    let (cmd, bindings) = decode_cmd_set_shader_resource_buffers_bindings_le(set_srvs_pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let start_slot = cmd.start_slot;
    let buffer_count = cmd.buffer_count;
    assert_eq!(shader_stage, AerogpuShaderStage::Pixel as u32);
    assert_eq!(start_slot, 1);
    assert_eq!(buffer_count, srvs.len() as u32);
    assert_eq!(bindings.len(), srvs.len());
    let b0_buffer = bindings[0].buffer;
    assert_eq!(b0_buffer, 10);
    cursor += set_srvs_hdr.size_bytes as usize;

    // SET_UNORDERED_ACCESS_BUFFERS.
    let set_uavs_hdr = decode_cmd_hdr_le(&bytes[cursor..]).unwrap();
    let set_uavs_pkt = &bytes[cursor..cursor + set_uavs_hdr.size_bytes as usize];
    let (cmd, bindings) =
        decode_cmd_set_unordered_access_buffers_bindings_le(set_uavs_pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let start_slot = cmd.start_slot;
    let uav_count = cmd.uav_count;
    assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(start_slot, 0);
    assert_eq!(uav_count, uavs.len() as u32);
    assert_eq!(bindings.len(), uavs.len());
    let b0_buffer = bindings[0].buffer;
    assert_eq!(b0_buffer, 20);
}

#[test]
fn set_samplers_count_overrun_is_rejected() {
    let mut w = AerogpuCmdWriter::new();
    w.set_samplers(AerogpuShaderStage::Pixel, 0, &[1]);
    let mut bytes = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    bytes[packet_offset + offset_of!(AerogpuCmdSetSamplers, sampler_count)
        ..packet_offset + offset_of!(AerogpuCmdSetSamplers, sampler_count) + 4]
        .copy_from_slice(&2u32.to_le_bytes());

    let pkt_len = decode_cmd_hdr_le(&bytes[packet_offset..])
        .unwrap()
        .size_bytes as usize;
    let pkt = &bytes[packet_offset..packet_offset + pkt_len];
    assert!(matches!(
        decode_cmd_set_samplers_handles_le(pkt),
        Err(AerogpuCmdDecodeError::BadSizeBytes { .. })
    ));
}

#[test]
fn set_constant_buffers_count_overrun_is_rejected() {
    let mut w = AerogpuCmdWriter::new();
    let bindings: [AerogpuConstantBufferBinding; 1] = [AerogpuConstantBufferBinding {
        buffer: 1,
        offset_bytes: 0,
        size_bytes: 16,
        reserved0: 0,
    }];
    w.set_constant_buffers(AerogpuShaderStage::Vertex, 0, &bindings);
    let mut bytes = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    bytes[packet_offset + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count)
        ..packet_offset + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count) + 4]
        .copy_from_slice(&2u32.to_le_bytes());

    let pkt_len = decode_cmd_hdr_le(&bytes[packet_offset..])
        .unwrap()
        .size_bytes as usize;
    let pkt = &bytes[packet_offset..packet_offset + pkt_len];
    assert!(matches!(
        decode_cmd_set_constant_buffers_bindings_le(pkt),
        Err(AerogpuCmdDecodeError::BadSizeBytes { .. })
    ));
}

#[test]
fn set_shader_resource_buffers_count_overrun_is_rejected() {
    let mut w = AerogpuCmdWriter::new();
    let bindings: [AerogpuShaderResourceBufferBinding; 1] = [AerogpuShaderResourceBufferBinding {
        buffer: 1,
        offset_bytes: 0,
        size_bytes: 16,
        reserved0: 0,
    }];
    w.set_shader_resource_buffers(AerogpuShaderStage::Pixel, 0, &bindings);
    let mut bytes = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    bytes[packet_offset + offset_of!(AerogpuCmdSetShaderResourceBuffers, buffer_count)
        ..packet_offset + offset_of!(AerogpuCmdSetShaderResourceBuffers, buffer_count) + 4]
        .copy_from_slice(&2u32.to_le_bytes());

    let pkt_len = decode_cmd_hdr_le(&bytes[packet_offset..])
        .unwrap()
        .size_bytes as usize;
    let pkt = &bytes[packet_offset..packet_offset + pkt_len];
    assert!(matches!(
        decode_cmd_set_shader_resource_buffers_bindings_le(pkt),
        Err(AerogpuCmdDecodeError::BadSizeBytes { .. })
    ));
}

#[test]
fn set_unordered_access_buffers_count_overrun_is_rejected() {
    let mut w = AerogpuCmdWriter::new();
    let bindings: [AerogpuUnorderedAccessBufferBinding; 1] = [AerogpuUnorderedAccessBufferBinding {
        buffer: 1,
        offset_bytes: 0,
        size_bytes: 16,
        initial_count: 0,
    }];
    w.set_unordered_access_buffers(AerogpuShaderStage::Compute, 0, &bindings);
    let mut bytes = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    bytes[packet_offset + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, uav_count)
        ..packet_offset + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, uav_count) + 4]
        .copy_from_slice(&2u32.to_le_bytes());

    let pkt_len = decode_cmd_hdr_le(&bytes[packet_offset..])
        .unwrap()
        .size_bytes as usize;
    let pkt = &bytes[packet_offset..packet_offset + pkt_len];
    assert!(matches!(
        decode_cmd_set_unordered_access_buffers_bindings_le(pkt),
        Err(AerogpuCmdDecodeError::BadSizeBytes { .. })
    ));
}
