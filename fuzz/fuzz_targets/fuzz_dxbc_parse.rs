#![no_main]

use aero_dxbc::{test_utils as dxbc_test_utils, DxbcFile, FourCC};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Cap fuzz input size so DXBC-declared sizes and signature table allocations stay bounded.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Limit chunk walking work for pathological containers with huge (but in-bounds) chunk counts.
const MAX_CHUNK_WALK: usize = 1024;

/// `DxbcFile::{get_chunk,get_signature,find_first_shader_chunk}` scan the whole chunk table.
/// Only call them for reasonably small containers to keep fuzzing throughput stable.
const MAX_FULL_SCAN_CHUNK_COUNT: usize = 1024;

/// Signature chunk parsing can allocate `param_count` entries + semantic name strings.
/// Keep both the chunk size and declared entry count bounded.
const MAX_SIGNATURE_CHUNK_BYTES: usize = 16 * 1024;
const MAX_SIGNATURE_ENTRIES: usize = 256;

/// Reflection parsers (`RDEF`/`CTAB`) can allocate entry tables and strings based on declared
/// counts/offsets. Keep chunk sizes small to ensure parsing stays bounded and fuzz throughput
/// remains stable.
const MAX_REFLECTION_CHUNK_BYTES: usize = 32 * 1024;
const MAX_RDEF_CONSTANT_BUFFERS: usize = 128;
const MAX_RDEF_RESOURCES: usize = 512;
const MAX_RDEF_VARIABLES_PER_CBUFFER: usize = 512;
const MAX_CTAB_CONSTANTS: usize = 512;

/// Patched DXBC builder chunk payload caps (kept small to avoid large allocations in signature
/// parsing helpers and to keep the synthesized container fast to parse).
const MAX_PATCHED_SIG_BYTES: usize = 4096;
const MAX_PATCHED_SHADER_BYTES: usize = 16 * 1024;
const MAX_PATCHED_OTHER_BYTES: usize = 4096;

const COMMON_FOURCCS: &[FourCC] = &[
    FourCC(*b"ISGN"),
    FourCC(*b"ISG1"),
    FourCC(*b"OSGN"),
    FourCC(*b"OSG1"),
    FourCC(*b"PSGN"),
    FourCC(*b"PSG1"),
    FourCC(*b"PCSG"),
    FourCC(*b"PCG1"),
    FourCC(*b"SHDR"),
    FourCC(*b"SHEX"),
    FourCC(*b"RDEF"),
    FourCC(*b"RD11"),
    FourCC(*b"STAT"),
    FourCC(*b"CTAB"),
    FourCC(*b"SPDB"),
    FourCC(*b"SFI0"),
    FourCC(*b"IFCE"),
];

fn is_signature_fourcc(fourcc: FourCC) -> bool {
    matches!(
        fourcc.0,
        [b'I', b'S', b'G', b'N']
            | [b'I', b'S', b'G', b'1']
            | [b'O', b'S', b'G', b'N']
            | [b'O', b'S', b'G', b'1']
            | [b'P', b'S', b'G', b'N']
            | [b'P', b'S', b'G', b'1']
            | [b'P', b'C', b'S', b'G']
            | [b'P', b'C', b'G', b'1']
    )
}

fn signature_param_count(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    Some(count)
}

fn should_parse_signature_chunk(fourcc: FourCC, bytes: &[u8]) -> bool {
    if !is_signature_fourcc(fourcc) {
        return false;
    }
    if bytes.len() > MAX_SIGNATURE_CHUNK_BYTES {
        return false;
    }
    if signature_param_count(bytes).unwrap_or(0) > MAX_SIGNATURE_ENTRIES {
        return false;
    }
    true
}

fn fuzz_signature_decoders(fourcc: FourCC, bytes: &[u8]) {
    if !should_parse_signature_chunk(fourcc, bytes) {
        return;
    }

    // Raw aero-dxbc signature parsing (alloc + UTF-8 decoding).
    let _ = aero_dxbc::signature::parse_signature_chunk_for_fourcc(fourcc, bytes);
    let _ = aero_dxbc::signature::parse_signature_chunk(bytes);

    // Higher-level aero-d3d11 conversion helpers used by the translator/runtime.
    let _ = aero_d3d11::parse_signature_chunk(fourcc, bytes);
}

fn fuzz_reflection_decoders(fourcc: FourCC, bytes: &[u8]) {
    match fourcc.0 {
        [b'R', b'D', b'E', b'F'] | [b'R', b'D', b'1', b'1'] => {
            if !should_parse_rdef_chunk(bytes) {
                return;
            }
            let _ = aero_dxbc::parse_rdef_chunk_for_fourcc(fourcc, bytes);
        }
        [b'C', b'T', b'A', b'B'] => {
            if !should_parse_ctab_chunk(bytes) {
                return;
            }
            let _ = aero_dxbc::parse_ctab_chunk(bytes);
        }
        _ => {}
    }
}

fn should_parse_rdef_chunk(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_REFLECTION_CHUNK_BYTES {
        return false;
    }
    if bytes.len() < 28 {
        // Truncated headers fail quickly without allocations; still safe to try.
        return true;
    }

    let cb_count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let cb_offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let rb_count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;

    if cb_count > MAX_RDEF_CONSTANT_BUFFERS || rb_count > MAX_RDEF_RESOURCES {
        return false;
    }

    // Scan per-cbuffer var counts so a single small chunk can't request huge allocations.
    if cb_count > 0 {
        let cb_desc_len = 24usize;
        let table_bytes = match cb_count.checked_mul(cb_desc_len) {
            Some(v) => v,
            None => return false,
        };
        let table_end = match cb_offset.checked_add(table_bytes) {
            Some(v) => v,
            None => return false,
        };
        if table_end <= bytes.len() {
            for i in 0..cb_count {
                let entry = cb_offset + i * cb_desc_len;
                let var_count_off = entry + 4;
                if var_count_off + 4 > bytes.len() {
                    break;
                }
                let var_count = u32::from_le_bytes([
                    bytes[var_count_off],
                    bytes[var_count_off + 1],
                    bytes[var_count_off + 2],
                    bytes[var_count_off + 3],
                ]) as usize;
                if var_count > MAX_RDEF_VARIABLES_PER_CBUFFER {
                    return false;
                }
            }
        }
    }

    true
}

fn should_parse_ctab_chunk(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_REFLECTION_CHUNK_BYTES {
        return false;
    }
    if bytes.len() < 16 {
        // Truncated headers fail quickly without allocations; still safe to try.
        return true;
    }
    let constant_count = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    constant_count <= MAX_CTAB_CONSTANTS
}

fn fuzz_dxbc_container(bytes: &[u8]) {
    let dxbc = match DxbcFile::parse(bytes) {
        Ok(dxbc) => dxbc,
        Err(_) => return,
    };

    // Touch basic accessors.
    let _ = dxbc.header();
    let _ = dxbc.bytes();

    // Iterate over chunks, but cap work for very large containers.
    for chunk in dxbc.chunks().take(MAX_CHUNK_WALK) {
        let _ = (chunk.fourcc, chunk.data.len());

        // Exercise signature parsing helpers when the chunk id matches.
        fuzz_signature_decoders(chunk.fourcc, chunk.data);
        fuzz_reflection_decoders(chunk.fourcc, chunk.data);
    }

    let chunk_count = dxbc.header().chunk_count as usize;
    if chunk_count > MAX_FULL_SCAN_CHUNK_COUNT {
        return;
    }

    // Exercise summary formatting (walks all chunks and builds a string).
    let _ = dxbc.debug_summary();

    // Exercise common lookup helpers.
    for &fourcc in COMMON_FOURCCS {
        if let Some(chunk) = dxbc.get_chunk(fourcc) {
            fuzz_signature_decoders(chunk.fourcc, chunk.data);
            fuzz_reflection_decoders(chunk.fourcc, chunk.data);
        }
    }

    // Exercise shader chunk lookup (`SHDR`/`SHEX`).
    let _ = dxbc.find_first_shader_chunk();

    // Exercise aero-d3d11's signature collection wrapper. This walks the container again, so keep
    // it guarded by the same caps as `get_chunk`/`get_signature`.
    //
    // Also avoid calling into it if any signature chunk in the prefix would exceed our signature
    // parsing caps.
    let safe_for_d3d11_signatures = dxbc.chunks().take(chunk_count).all(|chunk| {
        !is_signature_fourcc(chunk.fourcc) || should_parse_signature_chunk(chunk.fourcc, chunk.data)
    });
    if safe_for_d3d11_signatures {
        // Now that we've verified all signature chunks are within conservative caps, we can safely
        // exercise helper APIs that internally parse signatures.
        let _ = dxbc.get_signature(FourCC(*b"ISGN"));
        let _ = dxbc.get_signature(FourCC(*b"OSGN"));
        let _ = dxbc.get_signature(FourCC(*b"PSGN"));
        let _ = dxbc.get_signature(FourCC(*b"ISG1"));
        let _ = dxbc.get_signature(FourCC(*b"OSG1"));
        let _ = dxbc.get_signature(FourCC(*b"PSG1"));
        // Patch-constant signature variants (these are also parsed by `aero-d3d11::parse_signatures`).
        let _ = dxbc.get_signature(FourCC(*b"PCSG"));
        let _ = dxbc.get_signature(FourCC(*b"PCG1"));
        let _ = aero_d3d11::parse_signatures(&dxbc);
    }

    // Exercise higher-level aero-dxbc reflection helpers (`get_rdef`/`get_ctab`), but only when
    // all reflection chunks are within conservative size caps. These helpers scan the whole chunk
    // list and parse the first matching chunk, so keep them bounded.
    let safe_for_reflection = dxbc.chunks().take(chunk_count).all(|chunk| {
        !matches!(
            chunk.fourcc.0,
            [b'R', b'D', b'E', b'F'] | [b'R', b'D', b'1', b'1'] | [b'C', b'T', b'A', b'B']
        ) || match chunk.fourcc.0 {
            [b'R', b'D', b'E', b'F'] | [b'R', b'D', b'1', b'1'] => {
                should_parse_rdef_chunk(chunk.data)
            }
            [b'C', b'T', b'A', b'B'] => should_parse_ctab_chunk(chunk.data),
            _ => true,
        }
    });
    if safe_for_reflection {
        let _ = dxbc.get_rdef();
        let _ = dxbc.get_ctab();
    }
}

fn take_capped_bytes<'a>(u: &mut Unstructured<'a>, cap: usize) -> &'a [u8] {
    let requested = u.arbitrary::<u16>().unwrap_or(0) as usize;
    let len = requested.min(cap).min(u.len());
    u.bytes(len).unwrap_or(&[])
}

fn build_malformed_signature_chunk(seed: &[u8]) -> Vec<u8> {
    // Build a tiny signature-like payload that intentionally fails parsing quickly (e.g. param_offset
    // points into the header). Keep it within our caps so higher-level helpers still attempt to parse
    // it, exercising the "skip malformed duplicates" logic.
    let mut u = Unstructured::new(seed);
    let param_count = (u.arbitrary::<u8>().unwrap_or(0) % 8) as u32 + 1;
    let param_offset = u.arbitrary::<u8>().unwrap_or(0) % 8; // < SIGNATURE_HEADER_LEN
    let mut out = vec![0u8; 8];
    out[0..4].copy_from_slice(&param_count.to_le_bytes());
    out[4..8].copy_from_slice(&(param_offset as u32).to_le_bytes());
    out
}

fn build_patched_signature_chunk(fourcc: FourCC, seed: &[u8]) -> Vec<u8> {
    let mut u = Unstructured::new(seed);
    let param_count = (u.arbitrary::<u8>().unwrap_or(0) % 4) as usize + 1; // 1..=4

    let mut semantic_names = Vec::<String>::with_capacity(param_count);
    for _ in 0..param_count {
        // Generate a small ASCII semantic name so signature parsing can reach deeper paths.
        let name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;
        let mut name = String::with_capacity(name_len);
        for _ in 0..name_len {
            let b = u.arbitrary::<u8>().unwrap_or(0);
            name.push((b'A' + (b % 26)) as char);
        }
        semantic_names.push(name);
    }

    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = (0..param_count)
        .map(|i| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: semantic_names[i].as_str(),
            semantic_index: u32::from(u.arbitrary::<u8>().unwrap_or(0) % 8),
            system_value_type: u32::from(u.arbitrary::<u8>().unwrap_or(0)),
            component_type: u32::from(u.arbitrary::<u8>().unwrap_or(0)),
            register: u32::from(u.arbitrary::<u8>().unwrap_or(0)),
            mask: 0xF,
            read_write_mask: 0xF,
            stream: u32::from(u.arbitrary::<u8>().unwrap_or(0) % 4),
            min_precision: 0,
        })
        .collect();

    let out = dxbc_test_utils::build_signature_chunk_for_fourcc(fourcc, &entries);
    debug_assert!(out.len() <= MAX_PATCHED_SIG_BYTES);
    out
}

fn build_patched_rdef_chunk(seed: &[u8]) -> Vec<u8> {
    // Minimal, self-consistent RDEF chunk payload (not including the DXBC chunk header).
    //
    // Keep it small but structurally valid so fuzzing reaches deeper reflection parsing paths.
    //
    // This includes:
    // - 1 constant buffer with 1 variable and a simple type descriptor.
    // - 1 resource binding entry whose name matches the constant buffer name, so `parse_rdef_chunk`
    //   exercises the "link cbuffer -> bind slot" logic.
    let mut u = Unstructured::new(seed);
    let header_len = 28usize;
    let cb_desc_len = 24usize;
    let var_desc_len = 24usize;
    let type_desc_len = 16usize;
    let member_desc_len = 8usize;
    let resource_desc_len = 32usize;

    let cb_name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;
    let var_name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;

    // Optionally include a single struct member so the parser exercises type recursion and member
    // table decoding.
    let include_member = u.arbitrary::<u8>().unwrap_or(0) & 1 != 0;
    let member_name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;

    let include_creator = u.arbitrary::<u8>().unwrap_or(0) & 1 != 0;
    let creator_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;

    let cb_off = header_len;
    let var_off = cb_off + cb_desc_len;
    let type_off = var_off + var_desc_len;
    let member_off = type_off + type_desc_len;
    let type_member_off = member_off + if include_member { member_desc_len } else { 0 };
    let resource_off = type_member_off + if include_member { type_desc_len } else { 0 };
    let cb_name_off = resource_off + resource_desc_len;
    let var_name_off = cb_name_off + cb_name_len + 1;
    let member_name_off = var_name_off + var_name_len + 1;
    let creator_off = member_name_off
        + if include_member {
            member_name_len + 1
        } else {
            0
        };

    let total_len = if include_creator {
        creator_off + creator_len + 1
    } else {
        creator_off
    };
    let mut out = vec![0u8; total_len];

    // Header fields (u32):
    // cb_count=1, cb_offset=cb_off
    out[0..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..8].copy_from_slice(&(cb_off as u32).to_le_bytes());
    // resource_count=1, resource_offset=resource_off
    out[8..12].copy_from_slice(&1u32.to_le_bytes());
    out[12..16].copy_from_slice(&(resource_off as u32).to_le_bytes());
    // target, flags, creator_offset
    out[16..20].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());
    out[20..24].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());
    let creator_offset = if include_creator {
        creator_off as u32
    } else {
        0u32
    };
    out[24..28].copy_from_slice(&creator_offset.to_le_bytes());

    // Constant buffer desc.
    // name_offset, var_count=1, var_offset, size, flags, cb_type
    out[cb_off..cb_off + 4].copy_from_slice(&(cb_name_off as u32).to_le_bytes());
    out[cb_off + 4..cb_off + 8].copy_from_slice(&1u32.to_le_bytes());
    out[cb_off + 8..cb_off + 12].copy_from_slice(&(var_off as u32).to_le_bytes());
    out[cb_off + 12..cb_off + 16]
        .copy_from_slice(&u32::from(u.arbitrary::<u8>().unwrap_or(16)).to_le_bytes());
    out[cb_off + 16..cb_off + 20].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());
    out[cb_off + 20..cb_off + 24].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());

    // Variable desc.
    // name_offset, start_offset, size, flags, type_offset, default_value_offset
    out[var_off..var_off + 4].copy_from_slice(&(var_name_off as u32).to_le_bytes());
    out[var_off + 4..var_off + 8]
        .copy_from_slice(&u32::from(u.arbitrary::<u8>().unwrap_or(0)).to_le_bytes());
    out[var_off + 8..var_off + 12]
        .copy_from_slice(&u32::from(u.arbitrary::<u8>().unwrap_or(4)).to_le_bytes());
    out[var_off + 12..var_off + 16]
        .copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());
    out[var_off + 16..var_off + 20].copy_from_slice(&(type_off as u32).to_le_bytes());
    out[var_off + 20..var_off + 24].copy_from_slice(&0u32.to_le_bytes());

    // Type desc (very simple: no struct members).
    let class = u.arbitrary::<u16>().unwrap_or(0);
    let ty = u.arbitrary::<u16>().unwrap_or(0);
    let rows = u16::from(u.arbitrary::<u8>().unwrap_or(1) % 4) + 1;
    let cols = u16::from(u.arbitrary::<u8>().unwrap_or(1) % 4) + 1;
    let elements = u16::from(u.arbitrary::<u8>().unwrap_or(0) % 4);
    out[type_off..type_off + 2].copy_from_slice(&class.to_le_bytes());
    out[type_off + 2..type_off + 4].copy_from_slice(&ty.to_le_bytes());
    out[type_off + 4..type_off + 6].copy_from_slice(&rows.to_le_bytes());
    out[type_off + 6..type_off + 8].copy_from_slice(&cols.to_le_bytes());
    out[type_off + 8..type_off + 10].copy_from_slice(&elements.to_le_bytes());
    out[type_off + 10..type_off + 12].copy_from_slice(&u16::from(include_member).to_le_bytes()); // member_count
    let member_offset = if include_member {
        member_off as u32
    } else {
        0u32
    };
    out[type_off + 12..type_off + 16].copy_from_slice(&member_offset.to_le_bytes());

    if include_member {
        // Member table entry: name_offset + type_offset.
        out[member_off..member_off + 4].copy_from_slice(&(member_name_off as u32).to_le_bytes());
        out[member_off + 4..member_off + 8]
            .copy_from_slice(&(type_member_off as u32).to_le_bytes());

        // Secondary type desc (no further nesting).
        let class2 = u.arbitrary::<u16>().unwrap_or(0);
        let ty2 = u.arbitrary::<u16>().unwrap_or(0);
        let rows2 = u16::from(u.arbitrary::<u8>().unwrap_or(1) % 4) + 1;
        let cols2 = u16::from(u.arbitrary::<u8>().unwrap_or(1) % 4) + 1;
        let elements2 = u16::from(u.arbitrary::<u8>().unwrap_or(0) % 4);
        out[type_member_off..type_member_off + 2].copy_from_slice(&class2.to_le_bytes());
        out[type_member_off + 2..type_member_off + 4].copy_from_slice(&ty2.to_le_bytes());
        out[type_member_off + 4..type_member_off + 6].copy_from_slice(&rows2.to_le_bytes());
        out[type_member_off + 6..type_member_off + 8].copy_from_slice(&cols2.to_le_bytes());
        out[type_member_off + 8..type_member_off + 10].copy_from_slice(&elements2.to_le_bytes());
        out[type_member_off + 10..type_member_off + 12].copy_from_slice(&0u16.to_le_bytes());
        out[type_member_off + 12..type_member_off + 16].copy_from_slice(&0u32.to_le_bytes());
    }

    // Single resource binding entry.
    //
    // Use input_type=0 so `parse_rdef_chunk` considers it a cbuffer entry when linking.
    let entry = resource_off;
    out[entry..entry + 4].copy_from_slice(&(cb_name_off as u32).to_le_bytes()); // name_offset
    out[entry + 4..entry + 8].copy_from_slice(&0u32.to_le_bytes()); // input_type (cbuffer)
    out[entry + 8..entry + 12].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes()); // return_type
    out[entry + 12..entry + 16].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes()); // dimension
    out[entry + 16..entry + 20].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes()); // sample_count
    out[entry + 20..entry + 24].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes());
    // bind_count: keep non-zero and small
    let bind_count: u32 = u32::from(u.arbitrary::<u8>().unwrap_or(0) % 8) + 1;
    out[entry + 24..entry + 28].copy_from_slice(&bind_count.to_le_bytes());
    out[entry + 28..entry + 32].copy_from_slice(&u.arbitrary::<u32>().unwrap_or(0).to_le_bytes()); // flags

    // CBuffer/resource name string (must be valid UTF-8).
    for i in 0..cb_name_len {
        let b = u.arbitrary::<u8>().unwrap_or(0);
        out[cb_name_off + i] = b'A' + (b % 26);
    }
    out[cb_name_off + cb_name_len] = 0;

    // Variable name string (ASCII).
    for i in 0..var_name_len {
        let b = u.arbitrary::<u8>().unwrap_or(0);
        out[var_name_off + i] = b'a' + (b % 26);
    }
    out[var_name_off + var_name_len] = 0;

    if include_member {
        for i in 0..member_name_len {
            let b = u.arbitrary::<u8>().unwrap_or(0);
            out[member_name_off + i] = b'A' + (b % 26);
        }
        out[member_name_off + member_name_len] = 0;
    }

    if include_creator {
        for i in 0..creator_len {
            let b = u.arbitrary::<u8>().unwrap_or(0);
            out[creator_off + i] = b'A' + (b % 26);
        }
        out[creator_off + creator_len] = 0;
    }

    out
}

fn build_patched_ctab_chunk(seed: &[u8]) -> Vec<u8> {
    // Minimal, self-consistent CTAB chunk payload (not including the DXBC chunk header).
    //
    // Keep it small but structurally valid so fuzzing reaches deeper constant table parsing paths.
    let mut u = Unstructured::new(seed);
    let target_str: &[u8] = if u.arbitrary::<u8>().unwrap_or(0) & 1 == 0 {
        b"ps_2_0"
    } else {
        b"vs_3_0"
    };
    let include_creator = u.arbitrary::<u8>().unwrap_or(0) & 1 != 0;
    let creator_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;
    let name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;
    let header_len = 28usize;
    let entry_len = 20usize;
    let target_off = header_len + entry_len;
    let name_off = target_off + target_str.len() + 1;
    let creator_off = name_off + name_len + 1;
    let total_len = if include_creator {
        creator_off + creator_len + 1
    } else {
        creator_off
    };
    let mut out = vec![0u8; total_len];

    // Header.
    out[0..4].copy_from_slice(&0u32.to_le_bytes()); // size (ignored)
    let creator_offset = if include_creator {
        creator_off as u32
    } else {
        0u32
    };
    out[4..8].copy_from_slice(&creator_offset.to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes()); // version
    out[12..16].copy_from_slice(&1u32.to_le_bytes()); // constant_count
    out[16..20].copy_from_slice(&(header_len as u32).to_le_bytes()); // constant_offset
    out[20..24].copy_from_slice(&0u32.to_le_bytes()); // flags
    out[24..28].copy_from_slice(&(target_off as u32).to_le_bytes()); // target_offset

    // Single constant entry.
    let entry = header_len;
    out[entry..entry + 4].copy_from_slice(&(name_off as u32).to_le_bytes()); // name_offset
    let register_index: u16 = u.arbitrary::<u16>().unwrap_or(0);
    let register_count: u16 = (u.arbitrary::<u8>().unwrap_or(1) as u16).max(1);
    out[entry + 6..entry + 8].copy_from_slice(&register_index.to_le_bytes());
    out[entry + 8..entry + 10].copy_from_slice(&register_count.to_le_bytes());

    // Target string + NUL.
    out[target_off..target_off + target_str.len()].copy_from_slice(target_str);
    out[target_off + target_str.len()] = 0;

    // Name string + NUL (ASCII).
    for i in 0..name_len {
        let b = u.arbitrary::<u8>().unwrap_or(0);
        out[name_off + i] = b'A' + (b % 26);
    }
    out[name_off + name_len] = 0;

    // Optional creator string + NUL.
    if include_creator {
        for i in 0..creator_len {
            let b = u.arbitrary::<u8>().unwrap_or(0);
            out[creator_off + i] = b'A' + (b % 26);
        }
        out[creator_off + creator_len] = 0;
    }

    out
}

fn build_patched_dxbc(input: &[u8]) -> Vec<u8> {
    let mut u = Unstructured::new(input);
    let selector = u.arbitrary::<u8>().unwrap_or(0);

    // For each signature type, include both `*SGN` (v0) and `*SG1` (v1) chunk IDs.
    // Then use `selector` bits to decide which variant is valid vs. present-but-malformed, so
    // container-level helpers exercise fallback logic when the preferred ID exists but does not
    // successfully parse.
    let isg1_good = selector & 1 != 0;
    let osg1_good = selector & 2 != 0;
    let psg1_good = selector & 4 != 0;
    let pcg1_good = selector & 8 != 0;

    let shader_fourcc = if selector & 16 == 0 {
        FourCC(*b"SHDR")
    } else {
        FourCC(*b"SHEX")
    };
    let rdef_fourcc = if selector & 32 == 0 {
        FourCC(*b"RDEF")
    } else {
        FourCC(*b"RD11")
    };

    let isgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let isg1_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let osgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let osg1_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let psgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let psg1_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let pcsg_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let pcg1_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let bad_isgn_seed = take_capped_bytes(&mut u, 32);
    let bad_isg1_seed = take_capped_bytes(&mut u, 32);
    let bad_osgn_seed = take_capped_bytes(&mut u, 32);
    let bad_osg1_seed = take_capped_bytes(&mut u, 32);
    let bad_psgn_seed = take_capped_bytes(&mut u, 32);
    let bad_psg1_seed = take_capped_bytes(&mut u, 32);
    let bad_pcsg_seed = take_capped_bytes(&mut u, 32);
    let bad_pcg1_seed = take_capped_bytes(&mut u, 32);

    let isgn_payload = build_patched_signature_chunk(FourCC(*b"ISGN"), isgn_seed);
    let isg1_payload = build_patched_signature_chunk(FourCC(*b"ISG1"), isg1_seed);
    let osgn_payload = build_patched_signature_chunk(FourCC(*b"OSGN"), osgn_seed);
    let osg1_payload = build_patched_signature_chunk(FourCC(*b"OSG1"), osg1_seed);
    let psgn_payload = build_patched_signature_chunk(FourCC(*b"PSGN"), psgn_seed);
    let psg1_payload = build_patched_signature_chunk(FourCC(*b"PSG1"), psg1_seed);
    let pcsg_payload = build_patched_signature_chunk(FourCC(*b"PCSG"), pcsg_seed);
    let pcg1_payload = build_patched_signature_chunk(FourCC(*b"PCG1"), pcg1_seed);

    let bad_isgn_payload = build_malformed_signature_chunk(bad_isgn_seed);
    let bad_osgn_payload = build_malformed_signature_chunk(bad_osgn_seed);
    let bad_psgn_payload = build_malformed_signature_chunk(bad_psgn_seed);
    let bad_pcsg_payload = build_malformed_signature_chunk(bad_pcsg_seed);
    let bad_isg1_payload = build_malformed_signature_chunk(bad_isg1_seed);
    let bad_osg1_payload = build_malformed_signature_chunk(bad_osg1_seed);
    let bad_psg1_payload = build_malformed_signature_chunk(bad_psg1_seed);
    let bad_pcg1_payload = build_malformed_signature_chunk(bad_pcg1_seed);

    let shader_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SHADER_BYTES);
    let rdef_seed = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);
    let ctab_seed = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);
    let rdef_payload = build_patched_rdef_chunk(rdef_seed);
    let ctab_payload = build_patched_ctab_chunk(ctab_seed);
    let bad_rdef_payload = vec![0u8; 4];
    let bad_ctab_payload = vec![0u8; 4];
    let stat_bytes = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);

    let mut chunks: Vec<(FourCC, &[u8])> = Vec::with_capacity(16);
    // Insert malformed duplicates before the valid chunks so helper APIs exercise their
    // "try all chunks in file order and return the first that parses" behavior.
    chunks.push((FourCC(*b"ISG1"), &bad_isg1_payload));
    if isg1_good {
        chunks.push((FourCC(*b"ISG1"), &isg1_payload));
    }
    chunks.push((FourCC(*b"ISGN"), &bad_isgn_payload));
    if !isg1_good {
        chunks.push((FourCC(*b"ISGN"), &isgn_payload));
    }

    chunks.push((FourCC(*b"OSG1"), &bad_osg1_payload));
    if osg1_good {
        chunks.push((FourCC(*b"OSG1"), &osg1_payload));
    }
    chunks.push((FourCC(*b"OSGN"), &bad_osgn_payload));
    if !osg1_good {
        chunks.push((FourCC(*b"OSGN"), &osgn_payload));
    }

    chunks.push((FourCC(*b"PSG1"), &bad_psg1_payload));
    if psg1_good {
        chunks.push((FourCC(*b"PSG1"), &psg1_payload));
    }
    chunks.push((FourCC(*b"PSGN"), &bad_psgn_payload));
    if !psg1_good {
        chunks.push((FourCC(*b"PSGN"), &psgn_payload));
    }

    // Patch-constant signature (`PCG1` preferred, `PCSG` fallback).
    chunks.push((FourCC(*b"PCG1"), &bad_pcg1_payload));
    if pcg1_good {
        chunks.push((FourCC(*b"PCG1"), &pcg1_payload));
    }
    chunks.push((FourCC(*b"PCSG"), &bad_pcsg_payload));
    if !pcg1_good {
        chunks.push((FourCC(*b"PCSG"), &pcsg_payload));
    }

    chunks.push((shader_fourcc, shader_bytes));

    // Occasionally include a malformed primary `RDEF` chunk even when we store the real payload
    // under the alternate `RD11` id, to exercise the fallback path when `RDEF` exists but is
    // malformed.
    if rdef_fourcc.0 == *b"RD11" && selector & 64 != 0 {
        chunks.push((FourCC(*b"RDEF"), &bad_rdef_payload));
    }
    chunks.push((rdef_fourcc, &bad_rdef_payload));
    chunks.push((rdef_fourcc, &rdef_payload));

    chunks.push((FourCC(*b"CTAB"), &bad_ctab_payload));
    chunks.push((FourCC(*b"CTAB"), &ctab_payload));

    chunks.push((FourCC(*b"STAT"), stat_bytes));

    // Keep the synthesized container bounded regardless of future cap tweaks.
    let out = dxbc_test_utils::build_container(&chunks);
    debug_assert!(out.len() <= MAX_INPUT_SIZE_BYTES);
    out
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // `DxbcFile::parse` validates the chunk offset table in an O(chunk_count) loop.
    // Skip absurd `chunk_count` values up-front so a valid DXBC header cannot force
    // pathological parse time.
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]) as usize;
        if chunk_count > MAX_FULL_SCAN_CHUNK_COUNT {
            return;
        }
    }

    // Fuzz the raw bytes directly.
    fuzz_dxbc_container(data);

    // Also fuzz a patched container that forces a valid DXBC header and a few common chunk IDs so
    // the fuzzer can reach deeper parsing paths more often.
    let patched = build_patched_dxbc(data);
    fuzz_dxbc_container(&patched);
});
