mod common;

use aero_gpu::AerogpuD3d9Error;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn d3d9_set_shader_constants_rejects_unsupported_stage() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_f(AerogpuShaderStage::Compute, 0, &[0.0, 0.0, 0.0, 0.0]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_F to reject compute stage"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("unsupported stage")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_set_shader_constants_rejects_out_of_range_registers() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // c255..c256 is out of range (each stage has only 256 registers).
    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_f(AerogpuShaderStage::Vertex, 255, &[0.0; 8]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_F to reject out-of-range registers"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("out of bounds")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_set_shader_constants_i_rejects_unsupported_stage() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_i(AerogpuShaderStage::Compute, 0, &[0, 0, 0, 0]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_I to reject compute stage"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("unsupported stage")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_set_shader_constants_i_rejects_out_of_range_registers() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // i255..i256 is out of range (each stage has only 256 registers).
    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_i(AerogpuShaderStage::Vertex, 255, &[0; 8]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_I to reject out-of-range registers"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("out of bounds")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_set_shader_constants_b_rejects_unsupported_stage() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_b(AerogpuShaderStage::Compute, 0, &[0]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_B to reject compute stage"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("unsupported stage")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_set_shader_constants_b_rejects_out_of_range_registers() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // b255..b256 is out of range (each stage has only 256 registers).
    let mut writer = AerogpuCmdWriter::new();
    writer.set_shader_constants_b(AerogpuShaderStage::Vertex, 255, &[0, 1]);
    let stream = writer.finish();

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected SET_SHADER_CONSTANTS_B to reject out-of-range registers"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("out of bounds")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
