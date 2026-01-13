#![no_main]

use aero_dxbc::DxbcFile;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations and long runtimes on
/// malformed "container" blobs.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// `DxbcFile::parse` validates each chunk offset in a loop. Cap `chunk_count` for
/// deterministic fuzz iteration cost (especially when libFuzzer generates a
/// valid DXBC header but an absurd number of chunks).
const MAX_DXBC_CHUNKS: u32 = 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Pre-filter absurd `chunk_count` values to avoid worst-case O(n) parsing
    // time on otherwise-valid DXBC headers.
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            return;
        }
    }

    // Treat the slice as a candidate DXBC container. All errors are acceptable.
    let Ok(dxbc) = DxbcFile::parse(data) else {
        return;
    };

    // Exercise the common "find shader bytecode chunk" helper.
    let _ = dxbc.find_first_shader_chunk();

    // Also iterate a bounded number of chunks to stress the offset table / chunk
    // header parsing paths without risking long runtimes on huge chunk counts.
    for chunk in dxbc.chunks().take(MAX_DXBC_CHUNKS as usize) {
        let _ = (chunk.fourcc, chunk.data.len());
    }
});
