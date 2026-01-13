#![no_main]

use aero_dxbc::DxbcFile;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations and long runtimes on
/// malformed "container" blobs.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Treat the slice as a candidate DXBC container. All errors are acceptable.
    let Ok(dxbc) = DxbcFile::parse(data) else {
        return;
    };

    // Exercise the common "find shader bytecode chunk" helper.
    let _ = dxbc.find_first_shader_chunk();

    // Also iterate a bounded number of chunks to stress the offset table / chunk
    // header parsing paths without risking long runtimes on huge chunk counts.
    for chunk in dxbc.chunks().take(64) {
        let _ = (chunk.fourcc, chunk.data.len());
    }
});

