use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_create_shader_dxbc_payload_le, decode_cmd_hdr_le, AerogpuCmdStreamHeader,
    AerogpuShaderStage, AerogpuShaderStageEx,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn create_shader_dxbc_ex_encodes_stage_ex_in_reserved0() {
    let mut w = AerogpuCmdWriter::new();
    w.create_shader_dxbc_ex(
        123,
        AerogpuShaderStageEx::Geometry,
        &[0xAA, 0xBB, 0xCC],
    );
    let bytes = w.finish();

    let pkt_off = AerogpuCmdStreamHeader::SIZE_BYTES;
    let (cmd, payload) =
        decode_cmd_create_shader_dxbc_payload_le(&bytes[pkt_off..]).expect("decode packet");
    let shader_handle = cmd.shader_handle;
    let stage = cmd.stage;
    let reserved0 = cmd.reserved0;
    assert_eq!(shader_handle, 123);
    assert_eq!(stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(reserved0, AerogpuShaderStageEx::Geometry as u32);
    assert_eq!(payload, &[0xAA, 0xBB, 0xCC]);
}

#[test]
fn create_shader_dxbc_legacy_reserved0_remains_zero() {
    let mut w = AerogpuCmdWriter::new();
    w.create_shader_dxbc(1, AerogpuShaderStage::Vertex, &[0x00]);
    w.create_shader_dxbc(2, AerogpuShaderStage::Pixel, &[0x11, 0x22]);
    let bytes = w.finish();

    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
    for (expected_stage, expected_handle) in [
        (AerogpuShaderStage::Vertex, 1u32),
        (AerogpuShaderStage::Pixel, 2u32),
    ] {
        let hdr = decode_cmd_hdr_le(&bytes[cursor..]).expect("decode packet header");
        let (cmd, _payload) =
            decode_cmd_create_shader_dxbc_payload_le(&bytes[cursor..]).expect("decode packet");
        let shader_handle = cmd.shader_handle;
        let stage = cmd.stage;
        let reserved0 = cmd.reserved0;
        assert_eq!(shader_handle, expected_handle);
        assert_eq!(stage, expected_stage as u32);
        assert_eq!(reserved0, 0);
        cursor += hdr.size_bytes as usize;
    }
}

// Note: Pixel shaders must use the legacy `stage = PIXEL` encoding; `stage_ex` is intentionally
// non-zero-only and cannot represent the DXBC program-type 0 value.
#[test]
#[should_panic(expected = "stage_ex (Geometry) may only be encoded when shader_stage==COMPUTE")]
fn create_shader_dxbc_with_stage_ex_panics_for_non_compute_stages() {
    let mut w = AerogpuCmdWriter::new();
    w.create_shader_dxbc_with_stage_ex(
        1,
        AerogpuShaderStage::Pixel,
        &[0x00],
        Some(AerogpuShaderStageEx::Geometry),
    );
    let _ = w.finish();
}
