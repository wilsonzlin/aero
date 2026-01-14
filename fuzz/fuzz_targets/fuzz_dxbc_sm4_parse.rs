#![no_main]

use aero_dxbc::{test_utils as dxbc_test_utils, DxbcFile, FourCC};
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

    // Other common DXBC reflection/debug chunks used by Aero.
    // Use the higher-level helpers so we also cover variant/fallback IDs and duplicate-chunk
    // handling (e.g. `RD11` for RDEF).
    let _ = dxbc.get_rdef();
    let _ = dxbc.get_ctab();

    // SM4/SM5 token parsing (no GPU required).
    let _ = aero_dxbc::sm4::Sm4Program::parse_from_dxbc(&dxbc);
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Option<Vec<u8>> {
    let bytes = dxbc_test_utils::build_container(chunks);
    if bytes.len() > MAX_INPUT_SIZE_BYTES {
        return None;
    }
    Some(bytes)
}

fn build_signature_chunk(seed: &[u8], entry_size: usize, param_count: usize) -> Vec<u8> {
    // Minimal signature chunk that is either:
    // - v0 layout (24-byte entries), or
    // - v1 layout (32-byte entries)
    //
    // The chunk is always self-consistent (offsets are in-bounds and strings are NUL terminated),
    // but many fields are derived from the seed so libFuzzer can influence parsing.
    let header_len = 8usize;
    let entry_size = if entry_size == 32 { 32usize } else { 24usize };
    let param_count = param_count.clamp(1, 4);
    let table_len = param_count * entry_size;

    // Precompute string payloads.
    let mut strings = Vec::new();
    for entry_index in 0..param_count {
        let base = 16 * entry_index;
        let name_len = (seed.get(base).copied().unwrap_or(0) % 16) as usize + 1;
        for i in 0..name_len {
            let b = seed.get(base + 1 + i).copied().unwrap_or(b'A');
            strings.push(if b == 0 { b'A' } else { b });
        }
        strings.push(0);
    }

    let mut out = vec![0u8; header_len + table_len + strings.len()];
    out[0..4].copy_from_slice(&(param_count as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(header_len as u32).to_le_bytes());

    let mut string_cursor = header_len + table_len;
    for entry_index in 0..param_count {
        let entry_start = header_len + entry_index * entry_size;
        let name_offset = string_cursor as u32;
        out[entry_start..entry_start + 4].copy_from_slice(&name_offset.to_le_bytes());

        // Entry fields (these are common between v0/v1 for the first 20 bytes).
        out[entry_start + 4..entry_start + 8].copy_from_slice(
            &u32::from(seed.get(2 + entry_index).copied().unwrap_or(0)).to_le_bytes(),
        ); // semantic_index
        out[entry_start + 8..entry_start + 12].copy_from_slice(
            &u32::from(seed.get(6 + entry_index).copied().unwrap_or(0)).to_le_bytes(),
        ); // system_value_type
        out[entry_start + 12..entry_start + 16].copy_from_slice(
            &u32::from(seed.get(10 + entry_index).copied().unwrap_or(0)).to_le_bytes(),
        ); // component_type
        out[entry_start + 16..entry_start + 20].copy_from_slice(
            &u32::from(seed.get(14 + entry_index).copied().unwrap_or(0)).to_le_bytes(),
        ); // register

        let mask = seed.get(32 + entry_index).copied().unwrap_or(0xF);
        let rw_mask = seed.get(36 + entry_index).copied().unwrap_or(0xF);
        let stream = (seed.get(40 + entry_index).copied().unwrap_or(0) % 4) as u32;

        if entry_size == 24 {
            let packed = (mask as u32 & 0xFF)
                | ((rw_mask as u32 & 0xFF) << 8)
                | ((stream & 0xFF) << 16);
            out[entry_start + 20..entry_start + 24].copy_from_slice(&packed.to_le_bytes());
        } else {
            // entry_size == 32
            out[entry_start + 20] = mask;
            out[entry_start + 21] = rw_mask;
            out[entry_start + 24..entry_start + 28].copy_from_slice(&stream.to_le_bytes());
            // min_precision at entry_start+28..32 is left as 0.
        }

        // Copy this entry's semantic string (already built/terminated).
        let base = 16 * entry_index;
        let name_len = (seed.get(base).copied().unwrap_or(0) % 16) as usize + 1;
        let str_total = name_len + 1;
        let str_start = string_cursor;
        let str_end = str_start + str_total;
        out[str_start..str_end].copy_from_slice(
            &strings[str_start - (header_len + table_len)..str_end - (header_len + table_len)],
        );
        string_cursor = str_end;
    }

    out
}

fn build_min_rdef_chunk(seed: &[u8]) -> Vec<u8> {
    // Minimal RDEF chunk with a single resource binding and a short NUL-terminated name.
    //
    // This is enough to reach the table parsing and string decoding paths in `parse_rdef_chunk`.
    let name_len = (seed.get(0).copied().unwrap_or(0) % 16) as usize + 1;
    let header_len = 28usize;
    let entry_len = 32usize;
    let name_off = header_len + entry_len;
    let total_len = name_off + name_len + 1;
    let mut out = vec![0u8; total_len];

    // Header fields (u32).
    // cb_count, cb_offset
    out[0..4].copy_from_slice(&0u32.to_le_bytes());
    out[4..8].copy_from_slice(&0u32.to_le_bytes());
    // resource_count=1, resource_offset=28
    out[8..12].copy_from_slice(&1u32.to_le_bytes());
    out[12..16].copy_from_slice(&(header_len as u32).to_le_bytes());
    // shader_model, flags, creator_offset
    out[16..20].copy_from_slice(&0u32.to_le_bytes());
    out[20..24].copy_from_slice(&0u32.to_le_bytes());
    out[24..28].copy_from_slice(&0u32.to_le_bytes());

    // Single resource entry.
    let entry = header_len;
    out[entry..entry + 4].copy_from_slice(&(name_off as u32).to_le_bytes()); // name_offset
    out[entry + 4..entry + 8].copy_from_slice(&u32::from(seed.get(1).copied().unwrap_or(0)).to_le_bytes()); // type
    out[entry + 8..entry + 12].copy_from_slice(&u32::from(seed.get(2).copied().unwrap_or(0)).to_le_bytes()); // return type
    out[entry + 12..entry + 16].copy_from_slice(&u32::from(seed.get(3).copied().unwrap_or(0)).to_le_bytes()); // dimension
    out[entry + 16..entry + 20].copy_from_slice(&u32::from(seed.get(4).copied().unwrap_or(0)).to_le_bytes()); // num samples
    out[entry + 20..entry + 24].copy_from_slice(&u32::from(seed.get(5).copied().unwrap_or(0)).to_le_bytes()); // bind point
    out[entry + 24..entry + 28].copy_from_slice(&u32::from(seed.get(6).copied().unwrap_or(1)).to_le_bytes()); // bind count
    out[entry + 28..entry + 32].copy_from_slice(&u32::from(seed.get(7).copied().unwrap_or(0)).to_le_bytes()); // flags

    // Name string.
    for i in 0..name_len {
        let b = seed.get(8 + i).copied().unwrap_or(b'A');
        out[name_off + i] = if b == 0 { b'A' } else { b };
    }
    out[name_off + name_len] = 0;

    out
}

fn build_min_ctab_chunk(seed: &[u8]) -> Vec<u8> {
    // Minimal CTAB chunk with a single constant and short `target` + `name` strings.
    //
    // Enough to reach constant table parsing and string decoding in `parse_ctab_chunk`.
    let target_str = if seed.get(0).copied().unwrap_or(0) & 1 == 0 {
        b"ps_2_0"
    } else {
        b"vs_3_0"
    };
    let name_len = (seed.get(1).copied().unwrap_or(0) % 16) as usize + 1;
    let header_len = 28usize;
    let entry_len = 20usize;
    let target_off = header_len + entry_len;
    let name_off = target_off + target_str.len() + 1;
    let total_len = name_off + name_len + 1;
    let mut out = vec![0u8; total_len];

    // Header.
    out[0..4].copy_from_slice(&0u32.to_le_bytes()); // size (ignored)
    out[4..8].copy_from_slice(&0u32.to_le_bytes()); // creator_offset
    out[8..12].copy_from_slice(&0u32.to_le_bytes()); // version
    out[12..16].copy_from_slice(&1u32.to_le_bytes()); // constant_count
    out[16..20].copy_from_slice(&(header_len as u32).to_le_bytes()); // constant_offset
    out[20..24].copy_from_slice(&0u32.to_le_bytes()); // flags
    out[24..28].copy_from_slice(&(target_off as u32).to_le_bytes()); // target_offset

    // Constant info entry.
    let entry = header_len;
    out[entry..entry + 4].copy_from_slice(&(name_off as u32).to_le_bytes()); // name_offset
    out[entry + 4..entry + 6].copy_from_slice(&0u16.to_le_bytes()); // register set
    let reg_index = u16::from(seed.get(2).copied().unwrap_or(0));
    out[entry + 6..entry + 8].copy_from_slice(&reg_index.to_le_bytes()); // register index
    let reg_count = (u16::from(seed.get(3).copied().unwrap_or(0)) % 8).max(1);
    out[entry + 8..entry + 10].copy_from_slice(&reg_count.to_le_bytes()); // register count
    out[entry + 10..entry + 12].copy_from_slice(&0u16.to_le_bytes()); // reserved
    out[entry + 12..entry + 16].copy_from_slice(&0u32.to_le_bytes()); // type info offset
    out[entry + 16..entry + 20].copy_from_slice(&0u32.to_le_bytes()); // default value offset

    // Strings.
    out[target_off..target_off + target_str.len()].copy_from_slice(target_str);
    out[target_off + target_str.len()] = 0;

    for i in 0..name_len {
        let b = seed.get(4 + i).copied().unwrap_or(b'C');
        out[name_off + i] = if b == 0 { b'C' } else { b };
    }
    out[name_off + name_len] = 0;

    out
}

fn build_patched_dxbc(data: &[u8]) -> Option<Vec<u8>> {
    // Create a small, syntactically valid DXBC container that always contains:
    // - ISGN/OSGN/PSGN signature chunks (minimal valid payloads)
    // - SHDR shader chunk (payload derived from the fuzzer input with a self-consistent header)
    //
    // This helps libFuzzer reach signature parsing and SM4/SM5 token parsing even when the raw
    // input does not already look like a DXBC container.

    let sig_isgn_seed = data;
    let sig_osgn_seed = data.get(16..).unwrap_or(data);
    let sig_psgn_seed = data.get(32..).unwrap_or(data);

    let isgn_fourcc = if sig_isgn_seed.get(0).copied().unwrap_or(0) & 1 != 0 {
        FourCC(*b"ISG1")
    } else {
        FourCC(*b"ISGN")
    };
    let osgn_fourcc = if sig_osgn_seed.get(0).copied().unwrap_or(0) & 1 != 0 {
        FourCC(*b"OSG1")
    } else {
        FourCC(*b"OSGN")
    };
    let psgn_fourcc = if sig_psgn_seed.get(0).copied().unwrap_or(0) & 1 != 0 {
        FourCC(*b"PSG1")
    } else {
        FourCC(*b"PSGN")
    };

    // Allow independent selection of the entry layout (24 vs 32 bytes), regardless of the FourCC.
    // This exercises both the preferred layout and the fallback layout.
    let isgn_entry_size = if sig_isgn_seed.get(1).copied().unwrap_or(0) & 1 != 0 {
        32usize
    } else {
        24usize
    };
    let osgn_entry_size = if sig_osgn_seed.get(1).copied().unwrap_or(0) & 1 != 0 {
        32usize
    } else {
        24usize
    };
    let psgn_entry_size = if sig_psgn_seed.get(1).copied().unwrap_or(0) & 1 != 0 {
        32usize
    } else {
        24usize
    };

    let isgn_param_count = (sig_isgn_seed.get(2).copied().unwrap_or(0) % 4) as usize + 1;
    let osgn_param_count = (sig_osgn_seed.get(2).copied().unwrap_or(0) % 4) as usize + 1;
    let psgn_param_count = (sig_psgn_seed.get(2).copied().unwrap_or(0) % 4) as usize + 1;

    let sig_isgn = build_signature_chunk(sig_isgn_seed, isgn_entry_size, isgn_param_count);
    let sig_osgn = build_signature_chunk(sig_osgn_seed, osgn_entry_size, osgn_param_count);
    let sig_psgn = build_signature_chunk(sig_psgn_seed, psgn_entry_size, psgn_param_count);
    let rdef = build_min_rdef_chunk(data.get(48..).unwrap_or(data));
    let ctab = build_min_ctab_chunk(data.get(64..).unwrap_or(data));

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
    let shader_fourcc = if major >= 5 {
        FourCC(*b"SHEX")
    } else {
        FourCC(*b"SHDR")
    };

    let mut shdr = vec![0u8; shader_len];
    let copy_len = shader_len.min(data.len());
    shdr[..copy_len].copy_from_slice(&data[..copy_len]);
    shdr[0..4].copy_from_slice(&version.to_le_bytes());
    shdr[4..8].copy_from_slice(&(shader_dwords as u32).to_le_bytes());

    build_dxbc(&[
        (isgn_fourcc, &sig_isgn),
        (osgn_fourcc, &sig_osgn),
        (psgn_fourcc, &sig_psgn),
        (FourCC(*b"RDEF"), &rdef),
        (FourCC(*b"CTAB"), &ctab),
        (shader_fourcc, &shdr),
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
