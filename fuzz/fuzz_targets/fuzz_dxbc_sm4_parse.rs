#![no_main]

use aero_dxbc::{DxbcFile, FourCC};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in DXBC/SM4 parsing paths.
///
/// This matches the cap used by `fuzz_aerogpu_parse.rs`.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Avoid worst-case O(n) behavior in `DxbcFile::parse` (it validates each chunk offset) and
/// unbounded allocations in `DxbcFile::debug_summary()` by refusing DXBC headers that declare an
/// absurd number of chunks.
const MAX_DXBC_CHUNKS: u32 = 1024;

/// Limit the size of the synthesized shader chunk used to help the fuzzer reach deeper parsing
/// paths quickly. The raw fuzzer input is still fed into `DxbcFile::parse` unchanged.
const MAX_PATCHED_SHADER_BYTES: usize = 64 * 1024;

fn exercise_dxbc(bytes: &[u8]) {
    // `DxbcFile::parse` validates every chunk offset in a loop, so adversarial inputs can encode
    // extremely large `chunk_count` values (bounded only by the input size cap). Pre-filter those
    // cases once the DXBC magic is present to keep fuzz iterations fast and deterministic.
    if bytes.len() >= 32 && &bytes[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            return;
        }
    }

    let Ok(dxbc) = DxbcFile::parse(bytes) else {
        return;
    };

    // Exercise chunk iteration (bounded).
    for chunk in dxbc.chunks().take(MAX_DXBC_CHUNKS as usize) {
        // Touch a couple of fields so the calls aren't trivially optimized out.
        let _ = (chunk.fourcc, chunk.data.len());
    }

    // `debug_summary` iterates over all chunks and builds a string; keep it bounded.
    if dxbc.header().chunk_count <= MAX_DXBC_CHUNKS {
        let _ = dxbc.debug_summary();
    }

    // Signature parsing (these return `Option<Result<...>>`; all outcomes are acceptable).
    let _ = dxbc.get_signature(FourCC(*b"ISGN"));
    let _ = dxbc.get_signature(FourCC(*b"OSGN"));
    let _ = dxbc.get_signature(FourCC(*b"PSGN"));

    // SM4/SM5 token parsing (no GPU required).
    let _ = aero_dxbc::sm4::Sm4Program::parse_from_dxbc(&dxbc);
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Option<Vec<u8>> {
    let chunk_count = u32::try_from(chunks.len()).ok()?;
    let header_len = 4usize + 16 + 4 + 4 + 4 + chunks.len().checked_mul(4)?;

    // Compute chunk offsets.
    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(u32::try_from(cursor).ok()?);
        cursor = cursor.checked_add(8)?.checked_add(data.len())?;
    }
    if cursor > MAX_INPUT_SIZE_BYTES {
        return None;
    }

    let total_size = u32::try_from(cursor).ok()?;
    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    Some(bytes)
}

fn build_min_signature_chunk(seed: &[u8]) -> Vec<u8> {
    // Minimal valid v0 signature chunk containing a single entry.
    //
    // Keep the semantic name short and always NUL-terminated so the parser can reach both success
    // and error paths without large allocations.
    let name_len = (seed.get(0).copied().unwrap_or(0) % 16) as usize + 1;
    let total_len = 32 + name_len + 1; // header(8) + entry(24) + name + NUL
    let mut out = vec![0u8; total_len];

    // Header: param_count=1, param_offset=8.
    out[0..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..8].copy_from_slice(&8u32.to_le_bytes());

    // Single v0 entry at offset 8.
    let semantic_off = 32u32;
    out[8..12].copy_from_slice(&semantic_off.to_le_bytes());
    // Other fields can be arbitrary; they do not affect bounds checks.
    out[12..16].copy_from_slice(&u32::from(seed.get(1).copied().unwrap_or(0)).to_le_bytes()); // semantic_index
    out[16..20].copy_from_slice(&u32::from(seed.get(2).copied().unwrap_or(0)).to_le_bytes()); // system_value_type
    out[20..24].copy_from_slice(&u32::from(seed.get(3).copied().unwrap_or(0)).to_le_bytes()); // component_type
    out[24..28].copy_from_slice(&u32::from(seed.get(4).copied().unwrap_or(0)).to_le_bytes()); // register
    let mask = seed.get(5).copied().unwrap_or(0xF) as u32;
    let rw_mask = seed.get(6).copied().unwrap_or(0xF) as u32;
    let stream = (seed.get(7).copied().unwrap_or(0) % 4) as u32;
    let packed = (mask & 0xFF) | ((rw_mask & 0xFF) << 8) | ((stream & 0xFF) << 16);
    out[28..32].copy_from_slice(&packed.to_le_bytes());

    // Semantic name at offset 32.
    for i in 0..name_len {
        let b = seed.get(8 + i).copied().unwrap_or(b'A');
        out[32 + i] = if b == 0 { b'A' } else { b };
    }
    out[32 + name_len] = 0;

    out
}

fn build_patched_dxbc(data: &[u8]) -> Option<Vec<u8>> {
    // Create a small, syntactically valid DXBC container that always contains:
    // - ISGN/OSGN/PSGN signature chunks (minimal valid payloads)
    // - SHDR shader chunk (payload derived from the fuzzer input with a self-consistent header)
    //
    // This helps libFuzzer reach signature parsing and SM4/SM5 token parsing even when the raw
    // input does not already look like a DXBC container.

    let sig_isgn = build_min_signature_chunk(data);
    let sig_osgn = build_min_signature_chunk(data.get(16..).unwrap_or(data));
    let sig_psgn = build_min_signature_chunk(data.get(32..).unwrap_or(data));

    // Shader chunk payload: copy a prefix of the fuzzer data, but patch the header to be
    // self-consistent (version + declared length).
    let mut shader_len = data.len().min(MAX_PATCHED_SHADER_BYTES);
    shader_len = shader_len.max(8);
    shader_len &= !3;
    if shader_len < 8 {
        shader_len = 8;
    }
    let shader_dwords = shader_len / 4;

    let b0 = data.get(0).copied().unwrap_or(0);
    let b1 = data.get(1).copied().unwrap_or(0);
    let b2 = data.get(2).copied().unwrap_or(0);
    let ty = (b0 % 6) as u32;
    let major = 4 + (b1 % 2) as u32; // 4 or 5
    let minor = (b2 % 2) as u32;
    let version = (ty << 16) | (major << 4) | minor;

    let mut shdr = vec![0u8; shader_len];
    let copy_len = shader_len.min(data.len());
    shdr[..copy_len].copy_from_slice(&data[..copy_len]);
    shdr[0..4].copy_from_slice(&version.to_le_bytes());
    shdr[4..8].copy_from_slice(&(shader_dwords as u32).to_le_bytes());

    build_dxbc(&[
        (FourCC(*b"ISGN"), &sig_isgn),
        (FourCC(*b"OSGN"), &sig_osgn),
        (FourCC(*b"PSGN"), &sig_psgn),
        (FourCC(*b"SHDR"), &shdr),
    ])
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Raw fuzzer input.
    exercise_dxbc(data);

    // Also try a synthetically valid DXBC container derived from the input to help the fuzzer
    // reach deeper parsing paths more consistently.
    if let Some(patched) = build_patched_dxbc(data) {
        exercise_dxbc(&patched);
    }
});
