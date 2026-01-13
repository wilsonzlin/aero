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

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // `DxbcFile::parse` validates every chunk offset in a loop, so adversarial inputs can encode
    // extremely large `chunk_count` values (bounded only by the input size cap). Pre-filter those
    // cases once the DXBC magic is present to keep fuzz iterations fast and deterministic.
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            return;
        }
    }

    let Ok(dxbc) = DxbcFile::parse(data) else {
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
});

