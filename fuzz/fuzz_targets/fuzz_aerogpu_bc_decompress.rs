#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Cap output allocations to keep fuzzing fast and avoid OOM.
///
/// Worst case per function: `MAX_DIM * MAX_DIM * 4` bytes.
const MAX_DIM: u32 = 512;

/// Cap compressed input per format to avoid long loops on large buffers.
const MAX_COMPRESSED_BYTES_PER_FORMAT: usize = 64 * 1024;

fn expected_rgba8_len(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(4)?;
    usize::try_from(bytes).ok()
}

fn check_decompress_deterministic_and_len(
    name: &'static str,
    f: impl Fn(u32, u32, &[u8]) -> Vec<u8>,
    width: u32,
    height: u32,
    data: &[u8],
) {
    let out1 = f(width, height, data);
    let out2 = f(width, height, data);
    if out1 != out2 {
        panic!("{name}: output not deterministic (len={})", out1.len());
    }

    match expected_rgba8_len(width, height) {
        Some(expected) => {
            // The decoder may reject some hostile dimensions by returning an empty output. If it
            // accepts the dimensions, the output must be exactly RGBA8 for the full image.
            if out1.len() != 0 && out1.len() != expected {
                panic!(
                    "{name}: unexpected output length {} (expected {}) for {}x{}",
                    out1.len(),
                    expected,
                    width,
                    height
                );
            }
        }
        None => {
            if !out1.is_empty() {
                panic!(
                    "{name}: expected rejected/overflow dims to return empty vec, got len {} for {}x{}",
                    out1.len(),
                    width,
                    height
                );
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Model "mip-like" derived dimensions: pick a mip level within a declared mip count, then
    // shift base dimensions down.
    let base_width = u.arbitrary::<u32>().unwrap_or(0);
    let base_height = u.arbitrary::<u32>().unwrap_or(0);
    let mip_levels = u.arbitrary::<u8>().unwrap_or(0) & 31;
    let mip_level = u.arbitrary::<u8>().unwrap_or(0) & 31;
    let flags = u.arbitrary::<u8>().unwrap_or(0);

    let level = if mip_levels == 0 {
        0u32
    } else {
        (mip_level % mip_levels) as u32
    };

    let mut width = base_width >> level;
    let mut height = base_height >> level;

    // Some inputs should directly exercise overflow / rejected-dimension paths. Use the MSB so it
    // is easy for libFuzzer to toggle.
    if (flags & 0x80) != 0 {
        width = u32::MAX;
        height = u32::MAX;
    } else {
        width %= MAX_DIM + 1;
        height %= MAX_DIM + 1;
    }

    // Split the remainder of the fuzz input into per-format compressed streams so the fuzzer can
    // make progress on each independently.
    let bc1_len = (u.arbitrary::<u16>().unwrap_or(0) as usize).min(MAX_COMPRESSED_BYTES_PER_FORMAT);
    let bc2_len = (u.arbitrary::<u16>().unwrap_or(0) as usize).min(MAX_COMPRESSED_BYTES_PER_FORMAT);
    let bc3_len = (u.arbitrary::<u16>().unwrap_or(0) as usize).min(MAX_COMPRESSED_BYTES_PER_FORMAT);
    let bc7_len = (u.arbitrary::<u16>().unwrap_or(0) as usize).min(MAX_COMPRESSED_BYTES_PER_FORMAT);

    let bc1_len = bc1_len.min(u.len());
    let bc1 = u.bytes(bc1_len).unwrap_or(&[]);

    let bc2_len = bc2_len.min(u.len());
    let bc2 = u.bytes(bc2_len).unwrap_or(&[]);

    let bc3_len = bc3_len.min(u.len());
    let bc3 = u.bytes(bc3_len).unwrap_or(&[]);

    let bc7_len = bc7_len.min(u.len());
    let bc7 = u.bytes(bc7_len).unwrap_or(&[]);

    check_decompress_deterministic_and_len(
        "decompress_bc1_rgba8",
        aero_gpu::bc_decompress::decompress_bc1_rgba8,
        width,
        height,
        bc1,
    );
    check_decompress_deterministic_and_len(
        "decompress_bc2_rgba8",
        aero_gpu::bc_decompress::decompress_bc2_rgba8,
        width,
        height,
        bc2,
    );
    check_decompress_deterministic_and_len(
        "decompress_bc3_rgba8",
        aero_gpu::bc_decompress::decompress_bc3_rgba8,
        width,
        height,
        bc3,
    );
    check_decompress_deterministic_and_len(
        "decompress_bc7_rgba8",
        aero_gpu::bc_decompress::decompress_bc7_rgba8,
        width,
        height,
        bc7,
    );
});
