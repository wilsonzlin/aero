#![no_main]

use aero_dxbc::{DxbcFile, FourCC};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Cap fuzz input size so DXBC-declared sizes and signature table allocations stay bounded.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Limit chunk walking work for pathological containers with huge (but in-bounds) chunk counts.
const MAX_CHUNK_WALK: usize = 4096;

/// `DxbcFile::{get_chunk,get_signature,find_first_shader_chunk}` scan the whole chunk table.
/// Only call them for reasonably small containers to keep fuzzing throughput stable.
const MAX_FULL_SCAN_CHUNK_COUNT: usize = 8192;

/// Signature chunk parsing can allocate `param_count` entries + semantic name strings.
/// Keep both the chunk size and declared entry count bounded.
const MAX_SIGNATURE_CHUNK_BYTES: usize = 16 * 1024;
const MAX_SIGNATURE_ENTRIES: usize = 256;

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
    }

    let chunk_count = dxbc.header().chunk_count as usize;
    if chunk_count > MAX_FULL_SCAN_CHUNK_COUNT {
        return;
    }

    // Exercise common lookup helpers.
    for &fourcc in COMMON_FOURCCS {
        if let Some(chunk) = dxbc.get_chunk(fourcc) {
            fuzz_signature_decoders(chunk.fourcc, chunk.data);
        }
    }

    // Exercise the signature chunk lookup helper (tries variants like ISGN/ISG1).
    let _ = dxbc.get_signature(FourCC(*b"ISGN"));
    let _ = dxbc.get_signature(FourCC(*b"OSGN"));
    let _ = dxbc.get_signature(FourCC(*b"PSGN"));

    // Exercise shader chunk lookup (`SHDR`/`SHEX`).
    let _ = dxbc.find_first_shader_chunk();

    // Exercise aero-d3d11's signature collection wrapper. This walks the container again, so keep
    // it guarded by the same caps as `get_chunk`/`get_signature`.
    //
    // Also avoid calling into it if any signature chunk in the prefix would exceed our signature
    // parsing caps.
    let mut safe_for_d3d11_signatures = true;
    for chunk in dxbc.chunks().take(chunk_count.min(MAX_CHUNK_WALK)) {
        if is_signature_fourcc(chunk.fourcc) && !should_parse_signature_chunk(chunk.fourcc, chunk.data)
        {
            safe_for_d3d11_signatures = false;
            break;
        }
    }
    if safe_for_d3d11_signatures {
        let _ = aero_d3d11::parse_signatures(&dxbc);
    }
}

fn take_capped_bytes<'a>(u: &mut Unstructured<'a>, cap: usize) -> &'a [u8] {
    let requested = u.arbitrary::<u16>().unwrap_or(0) as usize;
    let len = requested.min(cap).min(u.len());
    u.bytes(len).unwrap_or(&[])
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

    let isgn_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let osgn_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let psgn_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SIG_BYTES);
    let shader_bytes = take_capped_bytes(&mut u, MAX_PATCHED_SHADER_BYTES);
    let rdef_bytes = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);
    let stat_bytes = take_capped_bytes(&mut u, MAX_PATCHED_OTHER_BYTES);

    let chunks: &[(FourCC, &[u8])] = &[
        (isgn_fourcc, isgn_bytes),
        (osgn_fourcc, osgn_bytes),
        (psgn_fourcc, psgn_bytes),
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
