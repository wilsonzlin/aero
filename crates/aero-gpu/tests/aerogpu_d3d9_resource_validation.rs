mod common;

use aero_d3d9::shader::ShaderStage as D3d9ShaderStage;
use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    // D3D9 SM2/SM3 encodes the *total* instruction length in DWORD tokens (including the opcode
    // token) in bits 24..27.
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn enc_inst_with_extra(opcode: u16, extra: u32, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24) | extra;
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn assemble_vs_passthrough_pos() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_dcl_cube_s0() -> Vec<u8> {
    // ps_3_0 with a `dcl_cube s0` declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type is encoded in opcode_token[16..20] for SM2/3 `dcl`.
    words.extend(enc_inst_with_extra(
        0x001F,
        3u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_dcl_1d_s0() -> Vec<u8> {
    // ps_3_0 with a `dcl_1d s0` (Texture1D) declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type is encoded in opcode_token[16..20] for SM2/3 `dcl`.
    words.extend(enc_inst_with_extra(
        0x001F,
        1u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_dcl_volume_s0() -> Vec<u8> {
    // ps_3_0 with a `dcl_volume s0` declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type is encoded in opcode_token[16..20] for SM2/3 `dcl`.
    words.extend(enc_inst_with_extra(
        0x001F,
        4u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_dcl_unknown_sampler_s0() -> Vec<u8> {
    // ps_3_0 with a `dcl` sampler declaration that uses an unknown texture type encoding.
    let mut words = vec![0xFFFF_0300];
    // Texture type is encoded in opcode_token[16..20] for SM2/3 `dcl`.
    words.extend(enc_inst_with_extra(
        0x001F,
        5u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn vertex_decl_position0_stream0() -> Vec<u8> {
    // D3DVERTEXELEMENT9 array (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u16.to_le_bytes()); // stream
    bytes.extend_from_slice(&0u16.to_le_bytes()); // offset
    bytes.push(3); // type = FLOAT4
    bytes.push(0); // method
    bytes.push(0); // usage = POSITION
    bytes.push(0); // usage_index

    bytes.extend_from_slice(&0x00FFu16.to_le_bytes()); // stream = 0xFF
    bytes.extend_from_slice(&0u16.to_le_bytes()); // offset
    bytes.push(17); // type = UNUSED
    bytes.push(0); // method
    bytes.push(0); // usage
    bytes.push(0); // usage_index

    bytes
}

#[test]
fn d3d9_create_buffer_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(
        0,  // buffer_handle
        0,  // usage_flags
        16, // size_bytes
        0,  // backing_alloc_id
        0,  // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_BUFFER with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        0,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(0, AerogpuShaderStage::Vertex, &[]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_SHADER_DXBC with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_rejects_unsupported_stage() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Compute, &[]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_SHADER_DXBC with compute stage to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("unsupported")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_rejects_stage_mismatch() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let vs_bytes = assemble_vs_passthrough_pos();

    // Submit a vertex shader but label it as pixel stage.
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &vs_bytes);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_SHADER_DXBC stage mismatch to be rejected"),
        Err(AerogpuD3d9Error::ShaderStageMismatch {
            shader_handle,
            expected,
            actual,
        }) => {
            assert_eq!(shader_handle, 1);
            assert_eq!(expected, D3d9ShaderStage::Pixel);
            assert_eq!(actual, D3d9ShaderStage::Vertex);
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_accepts_cube_sampler_declaration() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let ps_bytes = assemble_ps3_dcl_cube_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_accepts_1d_sampler_declaration() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Accepting `dcl_1d` does not imply full 1D texture binding support in the command protocol;
    // the executor will bind a dummy 1D texture for declared 1D samplers.
    let ps_bytes = assemble_ps3_dcl_1d_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_accepts_volume_sampler_declaration() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Accepting `dcl_volume` does not imply full 3D texture binding support in the command
    // protocol; the executor will bind a dummy 3D texture for declared volume samplers.
    let ps_bytes = assemble_ps3_dcl_volume_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_rejects_unknown_sampler_texture_type() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let ps_bytes = assemble_ps3_dcl_unknown_sampler_s0();
    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(1, AerogpuShaderStage::Pixel, &ps_bytes);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected unknown sampler texture type to be rejected"),
        Err(AerogpuD3d9Error::ShaderTranslation(msg)) => {
            assert!(
                msg.contains("sampler texture type"),
                "unexpected translation error: {msg}"
            );
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_input_layout_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_input_layout(0, &[]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_INPUT_LAYOUT with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_guest_backed_row_pitch_zero() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // backing_alloc_id != 0 requires a non-zero row pitch.
    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes (invalid for guest-backed)
        1,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!(
            "expected CREATE_TEXTURE2D with guest backing and row_pitch_bytes=0 to be rejected"
        ),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("row_pitch_bytes")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_shader_dxbc_rejects_handle_already_used_by_resource() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );
    writer.create_shader_dxbc(1, AerogpuShaderStage::Vertex, &[]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_SHADER_DXBC handle collision to be rejected"),
        Err(AerogpuD3d9Error::ShaderHandleInUse(1)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_input_layout_rejects_handle_already_used_by_resource() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(1, 0, 16, 0, 0);
    writer.create_input_layout(1, &[]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_INPUT_LAYOUT handle collision to be rejected"),
        Err(AerogpuD3d9Error::InputLayoutHandleInUse(1)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_resource_rejects_handle_already_used_by_input_layout() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let vertex_decl = vertex_decl_position0_stream0();

    let mut writer = AerogpuCmdWriter::new();
    writer.create_input_layout(2, &vertex_decl);
    writer.create_texture2d(
        2,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D handle collision to be rejected"),
        Err(AerogpuD3d9Error::ResourceHandleInUse(2)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_resource_rejects_handle_already_used_by_shader() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let vs_bytes = assemble_vs_passthrough_pos();

    let mut writer = AerogpuCmdWriter::new();
    writer.create_shader_dxbc(3, AerogpuShaderStage::Vertex, &vs_bytes);
    writer.create_buffer(3, 0, 16, 0, 0);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_BUFFER handle collision to be rejected"),
        Err(AerogpuD3d9Error::ResourceHandleInUse(3)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_import_shared_surface_rejects_alias_handle_already_used_by_shader() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let vs_bytes = assemble_vs_passthrough_pos();

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,
        1,
        1,
        1,
        4,
        0,
        0,
    );
    writer.export_shared_surface(1, TOKEN);
    writer.create_shader_dxbc(2, AerogpuShaderStage::Vertex, &vs_bytes);
    writer.import_shared_surface(2, TOKEN);

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected IMPORT_SHARED_SURFACE handle collision to be rejected"),
        Err(AerogpuD3d9Error::ResourceHandleInUse(2)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_export_shared_surface_rejects_buffer_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(1, 0, 16, 0, 0);
    writer.export_shared_surface(1, TOKEN);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected EXPORT_SHARED_SURFACE on buffer to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("only textures")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_export_shared_surface_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.export_shared_surface(0, 0x1122_3344_5566_7788);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected EXPORT_SHARED_SURFACE with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_export_shared_surface_rejects_zero_share_token() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );
    writer.export_shared_surface(1, 0);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected EXPORT_SHARED_SURFACE with share_token=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("share_token")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_import_shared_surface_rejects_zero_handle() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );
    writer.export_shared_surface(1, TOKEN);
    writer.import_shared_surface(0, TOKEN);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected IMPORT_SHARED_SURFACE with handle=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("reserved")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_import_shared_surface_rejects_zero_share_token() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.import_shared_surface(2, 0);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected IMPORT_SHARED_SURFACE with share_token=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("share_token")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_zero_dimensions() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        0,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with width=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("width/height")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_zero_mip_levels() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        0,                                   // mip_levels
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with mip_levels=0 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("mip_levels/array_layers")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_array_layers_not_one() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        1,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        2,                                   // array_layers (unsupported)
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with array_layers!=1 to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => {
            assert!(msg.contains("array_layers"), "{msg}");
            assert!(
                msg.contains("not supported")
                    || msg.contains("unsupported")
                    || msg.contains("!= 1"),
                "{msg}"
            );
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_mip_levels_beyond_chain_length() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // 4x4 textures only have 3 mip levels (4x4, 2x2, 1x1). Requesting 4 should be rejected
    // before hitting wgpu validation.
    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        4,                                   // width
        4,                                   // height
        4,                                   // mip_levels (invalid)
        1,                                   // array_layers
        0,                                   // row_pitch_bytes
        0,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with too many mips to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("mip_levels too large")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
#[test]
fn d3d9_create_texture2d_rejects_guest_backed_row_pitch_too_small() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut guest_memory = VecGuestMemory::new(0x1000);
    let alloc_table = AllocTable::new([(
        1,
        AllocEntry {
            flags: 0,
            gpa: 0x1000,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    // width=2 => required row_pitch is 8 bytes for RGBA8, but we pass 4.
    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                   // texture_handle
        0,                                   // usage_flags
        AerogpuFormat::R8G8B8A8Unorm as u32, // format
        2,                                   // width
        1,                                   // height
        1,                                   // mip_levels
        1,                                   // array_layers
        4,                                   // row_pitch_bytes (too small)
        1,                                   // backing_alloc_id
        0,                                   // backing_offset_bytes
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
    {
        Ok(_) => panic!("expected CREATE_TEXTURE2D with invalid row_pitch_bytes to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("row_pitch_bytes")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_guest_backing_bounds_bc1() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // BC1 4x4 is exactly 1 block => 8 bytes.
    let mut guest_memory = VecGuestMemory::new(0x1000);

    let ok_alloc = AllocTable::new([(
        1,
        AllocEntry {
            flags: 0,
            gpa: 0x1000,
            size_bytes: 8,
        },
    )])
    .expect("alloc table");

    let mut writer_ok = AerogpuCmdWriter::new();
    writer_ok.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        8,                                  // row_pitch_bytes (1 block row)
        1,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    let stream_ok = writer_ok.finish();
    exec.execute_cmd_stream_with_guest_memory(&stream_ok, &mut guest_memory, Some(&ok_alloc))
        .expect("BC1 create_texture2d should succeed with exact backing size");

    let small_alloc = AllocTable::new([(
        1,
        AllocEntry {
            flags: 0,
            gpa: 0x1000,
            size_bytes: 7,
        },
    )])
    .expect("alloc table");

    let mut writer_fail = AerogpuCmdWriter::new();
    writer_fail.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        8,                                  // row_pitch_bytes (1 block row)
        1,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    let stream_fail = writer_fail.finish();
    match exec.execute_cmd_stream_with_guest_memory(
        &stream_fail,
        &mut guest_memory,
        Some(&small_alloc),
    ) {
        Ok(_) => panic!("expected BC1 CREATE_TEXTURE2D to be rejected when alloc is too small"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("out of bounds")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_guest_backing_bounds_bc3() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // BC3 4x4 is exactly 1 block => 16 bytes.
    let mut guest_memory = VecGuestMemory::new(0x1000);

    let ok_alloc = AllocTable::new([(
        1,
        AllocEntry {
            flags: 0,
            gpa: 0x1000,
            size_bytes: 16,
        },
    )])
    .expect("alloc table");

    let mut writer_ok = AerogpuCmdWriter::new();
    writer_ok.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC3RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        16,                                 // row_pitch_bytes (1 block row)
        1,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    let stream_ok = writer_ok.finish();
    exec.execute_cmd_stream_with_guest_memory(&stream_ok, &mut guest_memory, Some(&ok_alloc))
        .expect("BC3 create_texture2d should succeed with exact backing size");

    let small_alloc = AllocTable::new([(
        1,
        AllocEntry {
            flags: 0,
            gpa: 0x1000,
            size_bytes: 15,
        },
    )])
    .expect("alloc table");

    let mut writer_fail = AerogpuCmdWriter::new();
    writer_fail.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC3RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        16,                                 // row_pitch_bytes (1 block row)
        1,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    let stream_fail = writer_fail.finish();
    match exec.execute_cmd_stream_with_guest_memory(
        &stream_fail,
        &mut guest_memory,
        Some(&small_alloc),
    ) {
        Ok(_) => panic!("expected BC3 CREATE_TEXTURE2D to be rejected when alloc is too small"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("out of bounds")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_create_texture2d_rejects_bc_render_target_usage() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    };

    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1, // texture_handle
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::BC1RgbaUnorm as u32,
        4,
        4,
        1,
        1,
        0,
        0,
        0,
    );
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!(
            "expected CREATE_TEXTURE2D with BC format and RENDER_TARGET usage to be rejected"
        ),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("BC formats")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_rejects_misaligned_bc_region() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        8,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    writer.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        8,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );

    // Misaligned destination x (BC blocks are 4x4).
    writer.copy_texture2d(
        2, // dst_texture
        1, // src_texture
        0, // dst_mip_level
        0, // dst_array_layer
        0, // src_mip_level
        0, // src_array_layer
        1, // dst_x (misaligned)
        0, // dst_y
        0, // src_x
        0, // src_y
        4, // width
        4, // height
        0, // flags
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {
            panic!("expected COPY_TEXTURE2D for BC format with misaligned region to be rejected")
        }
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("BC copy origin")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_rejects_misaligned_bc_width() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        8,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    writer.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        8,                                  // width
        4,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );

    // Non-block-aligned width that does not reach the mip edge.
    writer.copy_texture2d(
        2, // dst_texture
        1, // src_texture
        0, // dst_mip_level
        0, // dst_array_layer
        0, // src_mip_level
        0, // src_array_layer
        0, // dst_x
        0, // dst_y
        0, // src_x
        0, // src_y
        2, // width (BC blocks are 4x4)
        4, // height
        0, // flags
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {
            panic!("expected COPY_TEXTURE2D for BC format with misaligned width to be rejected")
        }
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("BC copy width")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_rejects_misaligned_bc_height() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        8,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    writer.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        8,                                  // height
        1,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );

    // Non-block-aligned height that does not reach the mip edge.
    writer.copy_texture2d(
        2, // dst_texture
        1, // src_texture
        0, // dst_mip_level
        0, // dst_array_layer
        0, // src_mip_level
        0, // src_array_layer
        0, // dst_x
        0, // dst_y
        0, // src_x
        0, // src_y
        4, // width
        2, // height (BC blocks are 4x4)
        0, // flags
    );

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => {
            panic!("expected COPY_TEXTURE2D for BC format with misaligned height to be rejected")
        }
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("BC copy height")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_allows_mip_edge_bc_region() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        1,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width (mip1 width=2)
        4,                                  // height (mip1 height=2)
        2,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );
    writer.create_texture2d(
        2,                                  // texture_handle
        0,                                  // usage_flags
        AerogpuFormat::BC1RgbaUnorm as u32, // format
        4,                                  // width
        4,                                  // height
        2,                                  // mip_levels
        1,                                  // array_layers
        0,                                  // row_pitch_bytes
        0,                                  // backing_alloc_id
        0,                                  // backing_offset_bytes
    );

    // Copy the entire mip even though mip dimensions are smaller than the BC block size
    // (mip1 is 2x2). This should be accepted.
    writer.copy_texture2d(
        2, // dst_texture
        1, // src_texture
        1, // dst_mip_level
        0, // dst_array_layer
        1, // src_mip_level
        0, // src_array_layer
        0, // dst_x
        0, // dst_y
        0, // src_x
        0, // src_y
        2, // width
        2, // height
        0, // flags
    );

    let stream = writer.finish();
    exec.execute_cmd_stream(&stream)
        .expect("COPY_TEXTURE2D edge BC region should be accepted");
}

#[test]
fn d3d9_create_buffer_rejects_unaligned_size() {
    use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuCmdCreateBuffer, AerogpuCmdStreamHeader};

    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(
        1, // buffer_handle
        0, // usage_flags
        4, // size_bytes (writer requires aligned; patch to invalid below)
        0, // backing_alloc_id
        0, // backing_offset_bytes
    );

    let mut stream = writer.finish();
    // Patch CREATE_BUFFER.size_bytes to be unaligned without panicking the safe writer.
    let size_offset = AerogpuCmdStreamHeader::SIZE_BYTES
        + core::mem::offset_of!(AerogpuCmdCreateBuffer, size_bytes);
    stream[size_offset..size_offset + 8].copy_from_slice(&3u64.to_le_bytes());
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected CREATE_BUFFER with unaligned size_bytes to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("CREATE_BUFFER")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_upload_resource_rejects_unaligned_buffer_range() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(
        1,  // buffer_handle
        0,  // usage_flags
        16, // size_bytes
        0,  // backing_alloc_id
        0,  // backing_offset_bytes
    );
    writer.upload_resource(1, 2, &[0u8; 4]); // offset_bytes is not aligned

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected UPLOAD_RESOURCE with unaligned offset_bytes to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("UPLOAD_RESOURCE")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_buffer_rejects_unaligned_range() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdCopyBuffer, AerogpuCmdCreateBuffer, AerogpuCmdStreamHeader,
    };

    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(1, 0, 16, 0, 0); // src
    writer.create_buffer(2, 0, 16, 0, 0); // dst
    writer.copy_buffer(
        2, // dst_buffer
        1, // src_buffer
        0, // dst_offset_bytes
        0, // src_offset_bytes
        4, // size_bytes (writer requires aligned; patch to invalid below)
        0, // flags
    );

    let mut stream = writer.finish();
    // Patch COPY_BUFFER.size_bytes to be unaligned without panicking the safe writer.
    let copy_base =
        AerogpuCmdStreamHeader::SIZE_BYTES + 2 * core::mem::size_of::<AerogpuCmdCreateBuffer>();
    let size_offset = copy_base + core::mem::offset_of!(AerogpuCmdCopyBuffer, size_bytes);
    stream[size_offset..size_offset + 8].copy_from_slice(&2u64.to_le_bytes());
    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected COPY_BUFFER with unaligned size_bytes to be rejected"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("COPY_BUFFER")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
