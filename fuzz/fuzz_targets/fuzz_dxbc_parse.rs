#![no_main]

use aero_dxbc::{DxbcFile, FourCC};
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
    FourCC(*b"SHDR"),
    FourCC(*b"SHEX"),
    FourCC(*b"RDEF"),
    FourCC(*b"STAT"),
    FourCC(*b"CTAB"),
    FourCC(*b"SPDB"),
    FourCC(*b"SFI0"),
    FourCC(*b"IFCE"),
    FourCC(*b"PCSG"),
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
    if bytes.len() > MAX_REFLECTION_CHUNK_BYTES {
        return;
    }
    match fourcc.0 {
        [b'R', b'D', b'E', b'F'] => {
            let _ = aero_dxbc::parse_rdef_chunk_for_fourcc(fourcc, bytes);
        }
        [b'C', b'T', b'A', b'B'] => {
            let _ = aero_dxbc::parse_ctab_chunk(bytes);
        }
        _ => {}
    }
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
        let _ = aero_d3d11::parse_signatures(&dxbc);
    }
}

fn take_capped_bytes<'a>(u: &mut Unstructured<'a>, cap: usize) -> &'a [u8] {
    let requested = u.arbitrary::<u16>().unwrap_or(0) as usize;
    let len = requested.min(cap).min(u.len());
    u.bytes(len).unwrap_or(&[])
}

fn build_patched_signature_chunk(fourcc: FourCC, seed: &[u8]) -> Vec<u8> {
    // Build a small, self-consistent signature chunk payload (not including the DXBC chunk header).
    //
    // This intentionally keeps `param_count` small and ensures semantic name offsets point at valid
    // NUL-terminated strings so signature parsing reaches deeper paths (vs. immediately bailing out
    // on nonsense offsets/counts).
    //
    // The resulting buffer size stays well below `MAX_PATCHED_SIG_BYTES`.
    let mut u = Unstructured::new(seed);
    let entry_size = if fourcc.0[3] == b'1' { 32usize } else { 24usize };
    let param_count = (u.arbitrary::<u8>().unwrap_or(0) % 4) as usize + 1; // 1..=4
    let header_len = 8usize;
    let table_len = param_count * entry_size;

    let mut out = vec![0u8; header_len + table_len];
    out[0..4].copy_from_slice(&(param_count as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(header_len as u32).to_le_bytes());

    for entry_index in 0..param_count {
        let entry_start = header_len + entry_index * entry_size;

        // Place the semantic name after the table so it is never inside the header or entries region.
        let semantic_name_offset = out.len() as u32;
        out[entry_start..entry_start + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());

        let semantic_index = u32::from(u.arbitrary::<u8>().unwrap_or(0) % 8);
        let system_value_type = u32::from(u.arbitrary::<u8>().unwrap_or(0));
        let component_type = u32::from(u.arbitrary::<u8>().unwrap_or(0));
        let register = u32::from(u.arbitrary::<u8>().unwrap_or(0));

        out[entry_start + 4..entry_start + 8].copy_from_slice(&semantic_index.to_le_bytes());
        out[entry_start + 8..entry_start + 12].copy_from_slice(&system_value_type.to_le_bytes());
        out[entry_start + 12..entry_start + 16].copy_from_slice(&component_type.to_le_bytes());
        out[entry_start + 16..entry_start + 20].copy_from_slice(&register.to_le_bytes());

        // Keep masks simple and valid-looking.
        let mask: u8 = 0xF;
        let read_write_mask: u8 = 0xF;
        let stream: u8 = u.arbitrary::<u8>().unwrap_or(0) % 4;

        match entry_size {
            24 => {
                let packed = (mask as u32 & 0xFF)
                    | ((read_write_mask as u32 & 0xFF) << 8)
                    | ((stream as u32 & 0xFF) << 16);
                out[entry_start + 20..entry_start + 24].copy_from_slice(&packed.to_le_bytes());
            }
            32 => {
                out[entry_start + 20] = mask;
                out[entry_start + 21] = read_write_mask;
                out[entry_start + 24..entry_start + 28]
                    .copy_from_slice(&(stream as u32).to_le_bytes());
                // min_precision at entry_start+28..32 left as 0.
            }
            _ => unreachable!(),
        }

        // Append a small ASCII semantic name + NUL terminator (must be valid UTF-8).
        let name_len = (u.arbitrary::<u8>().unwrap_or(0) % 16) as usize + 1;
        for _ in 0..name_len {
            let b = u.arbitrary::<u8>().unwrap_or(0);
            out.push(b'A' + (b % 26));
        }
        out.push(0);
    }

    debug_assert!(out.len() <= MAX_PATCHED_SIG_BYTES);
    out
}

fn build_patched_dxbc(input: &[u8]) -> Vec<u8> {
    let mut u = Unstructured::new(input);
    let selector = u.arbitrary::<u8>().unwrap_or(0);

    let isgn_fourcc = if selector & 1 == 0 {
        FourCC(*b"ISGN")
    } else {
        FourCC(*b"ISG1")
    };
    let osgn_fourcc = if selector & 2 == 0 {
        FourCC(*b"OSGN")
    } else {
        FourCC(*b"OSG1")
    };
    let psgn_fourcc = if selector & 4 == 0 {
        FourCC(*b"PSGN")
    } else {
        FourCC(*b"PSG1")
    };
    let shader_fourcc = if selector & 8 == 0 {
        FourCC(*b"SHDR")
    } else {
        FourCC(*b"SHEX")
    };

    let isgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let osgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let psgn_seed = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let isgn_payload = build_patched_signature_chunk(isgn_fourcc, isgn_seed);
    let osgn_payload = build_patched_signature_chunk(osgn_fourcc, osgn_seed);
    let psgn_payload = build_patched_signature_chunk(psgn_fourcc, psgn_seed);
    let shader_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SHADER_BYTES);
    let rdef_bytes = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);
    let stat_bytes = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);

    let chunks: &[(FourCC, &[u8])] = &[
        (isgn_fourcc, &isgn_payload),
        (osgn_fourcc, &osgn_payload),
        (psgn_fourcc, &psgn_payload),
        (shader_fourcc, shader_bytes),
        (FourCC(*b"RDEF"), rdef_bytes),
        (FourCC(*b"STAT"), stat_bytes),
    ];

    // DXBC header is always 32 bytes:
    // magic (4) + checksum (16) + reserved (4) + total_size (4) + chunk_count (4).
    let header_len = 32usize;
    let offset_table_len = chunks.len() * 4;
    let mut total_size = header_len + offset_table_len;
    for &(_fourcc, data) in chunks {
        total_size = total_size.saturating_add(8).saturating_add(data.len());
    }

    // Keep the synthesized container bounded regardless of future cap tweaks.
    if total_size > MAX_INPUT_SIZE_BYTES {
        total_size = MAX_INPUT_SIZE_BYTES;
    }

    let mut out = Vec::with_capacity(total_size);
    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0u8; 16]); // checksum
    out.extend_from_slice(&[0u8; 4]); // reserved
    out.extend_from_slice(&(total_size as u32).to_le_bytes());
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());

    let offset_table_start = out.len();
    out.resize(offset_table_start + offset_table_len, 0);

    for (i, (fourcc, data)) in chunks.iter().enumerate() {
        let chunk_offset = out.len();
        let table_entry_off = offset_table_start + i * 4;
        out[table_entry_off..table_entry_off + 4]
            .copy_from_slice(&(chunk_offset as u32).to_le_bytes());

        out.extend_from_slice(&fourcc.0);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if out.len() > MAX_INPUT_SIZE_BYTES {
            // Truncate defensively; DxbcFile::parse will reject `total_size` anyway.
            out.truncate(MAX_INPUT_SIZE_BYTES);
            break;
        }
    }

    // Fix up total_size to match the actual synthesized buffer size.
    let actual_size = out.len().min(u32::MAX as usize);
    out[24..28].copy_from_slice(&(actual_size as u32).to_le_bytes());

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
