#![no_main]

use libfuzzer_sys::fuzz_target;

/// Hard cap on the raw libFuzzer input size. This limits allocations in both the D3D9 shader token
/// parser and the optional DXBC wrapper parser.
const MAX_INPUT_SIZE_BYTES: usize = 256 * 1024; // 256 KiB

/// Cap the token-stream bytes we feed into `aero_d3d9_shader` to keep parsing time predictable.
/// D3D9 shader blobs are typically small, and we mainly want to stress bounds checks / decoding.
const MAX_TOKEN_STREAM_BYTES: usize = 64 * 1024; // 64 KiB

/// Cap the amount of work done by the disassembler (string formatting can otherwise be quadratic
/// in the worst case for huge instruction streams).
const MAX_DECLARATIONS: usize = 512;
const MAX_INSTRUCTIONS: usize = 2048;

fn wrap_dxbc(shader_bytes: &[u8], chunk_fourcc: [u8; 4]) -> Vec<u8> {
    // Minimal DXBC container with a single shader bytecode chunk.
    // Header is:
    // - magic "DXBC"
    // - 16-byte checksum (ignored by our parser unless md5 feature is enabled)
    // - 4-byte reserved
    // - total_size
    // - chunk_count
    // followed by chunk offsets table.
    //
    // Each chunk is:
    // - fourcc
    // - size
    // - payload bytes
    const HEADER_SIZE: usize = 32;
    const OFFSET_TABLE_SIZE: usize = 4;
    let chunk_offset = (HEADER_SIZE + OFFSET_TABLE_SIZE) as u32;
    let total_size = chunk_offset as usize + 8 + shader_bytes.len();
    let mut dxbc = Vec::with_capacity(total_size);
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&0u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&(total_size as u32).to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&chunk_offset.to_le_bytes());
    dxbc.extend_from_slice(&chunk_fourcc);
    dxbc.extend_from_slice(&(shader_bytes.len() as u32).to_le_bytes());
    dxbc.extend_from_slice(shader_bytes);
    dxbc
}

fn disassemble_bounded(mut shader: aero_d3d9_shader::D3d9Shader) {
    shader.declarations.truncate(MAX_DECLARATIONS);
    shader.instructions.truncate(MAX_INSTRUCTIONS);
    let dis = shader.disassemble();
    // Touch the result to keep the optimizer from discarding formatting work.
    let _ = dis.len();
}

fn patched_token_stream(data: &[u8]) -> Vec<u8> {
    // Copy a bounded number of bytes and align to 32-bit tokens.
    let mut out = data[..data.len().min(MAX_TOKEN_STREAM_BYTES)].to_vec();
    out.truncate((out.len() / 4) * 4);
    if out.len() < 4 {
        out.resize(4, 0);
    }

    // Force a valid D3D9 version token (vs/ps + {1,2,3}_x) so we reach deeper decode paths more
    // often than relying on pure random chance.
    let b0 = data.get(0).copied().unwrap_or(0);
    let b1 = data.get(1).copied().unwrap_or(0);
    let b2 = data.get(2).copied().unwrap_or(0);
    let shader_type: u16 = if (b0 & 1) == 0 { 0xFFFE } else { 0xFFFF }; // vs / ps
    let major: u8 = (b1 % 3) + 1;
    let minor: u8 = b2 % 4;
    let version_token: u32 = ((shader_type as u32) << 16) | ((major as u32) << 8) | minor as u32;
    out[0..4].copy_from_slice(&version_token.to_le_bytes());

    // Encourage termination: place an END token at the end of the buffer when possible.
    if out.len() >= 8 {
        let end = out.len();
        out[end - 4..end].copy_from_slice(&0x0000_FFFFu32.to_le_bytes());
    }

    out
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // 1) Parse the bytes as-is. This will exercise the DXBC extraction path when `data` happens to
    // start with "DXBC", and will otherwise attempt to treat the input as a raw DWORD token
    // stream.
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(data) {
        disassemble_bounded(shader);
    }

    // 2) Also patch the first token to a valid shader version token and ensure the input is
    // DWORD-aligned so the token-stream parser gets more coverage.
    let patched = patched_token_stream(data);
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(&patched) {
        disassemble_bounded(shader);
    }

    // 3) Wrap the patched token stream in a minimal valid DXBC container. This reliably exercises
    // the DXBC extraction path even when the raw fuzz input does not start with "DXBC".
    let chunk = if data.get(0).copied().unwrap_or(0) & 1 == 0 {
        *b"SHDR"
    } else {
        *b"SHEX"
    };
    let dxbc = wrap_dxbc(&patched, chunk);
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(&dxbc) {
        disassemble_bounded(shader);
    }
});
