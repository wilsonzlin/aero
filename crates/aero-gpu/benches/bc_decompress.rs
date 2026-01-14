#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_gpu::{
    decompress_bc1_rgba8_into, decompress_bc2_rgba8_into, decompress_bc3_rgba8_into,
    decompress_bc7_rgba8_into,
};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Benchmarks for the CPU BCn decompression paths used when BC texture sampling is unavailable
/// (WebGL2 fallback, capability fallback).
///
/// Criterion output interpretation:
/// - `thrpt` is decompressed RGBA8 bytes/s (useful for MB/s).
/// - `time` is per full-image decode; divide by `width * height` to get ns/pixel.
#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
        _ => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct SplitMix64 {
    state: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn gen_deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = vec![0u8; len];
    SplitMix64::new(seed).fill_bytes(&mut out);
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn bc_data_len_bytes(width: u32, height: u32, bytes_per_block: usize) -> usize {
    let blocks_w = width.div_ceil(4) as usize;
    let blocks_h = height.div_ceil(4) as usize;
    blocks_w * blocks_h * bytes_per_block
}

#[cfg(not(target_arch = "wasm32"))]
fn rgba8_len_bytes(width: u32, height: u32) -> usize {
    (width as usize) * (height as usize) * 4
}

#[cfg(not(target_arch = "wasm32"))]
type DecompressFn = fn(u32, u32, &[u8], &mut [u8]);

#[cfg(not(target_arch = "wasm32"))]
fn bench_bc_decompress(c: &mut Criterion) {
    // Representative texture sizes:
    // - 256x256: small-ish texture
    // - 1024x1024: larger, common power-of-two
    // - 1000x1001: exercises edge handling (not divisible by 4)
    const SIZES: &[(u32, u32)] = &[(256, 256), (1024, 1024), (1000, 1001)];

    const FORMATS: &[(&str, usize, DecompressFn)] = &[
        ("bc1", 8, decompress_bc1_rgba8_into),
        ("bc2", 16, decompress_bc2_rgba8_into),
        ("bc3", 16, decompress_bc3_rgba8_into),
        ("bc7", 16, decompress_bc7_rgba8_into),
    ];

    {
        let mut group = c.benchmark_group("bc_decompress_rgba8_bytes");
        for (format_name, bytes_per_block, func) in FORMATS {
            for &(width, height) in SIZES {
                let input_len = bc_data_len_bytes(width, height, *bytes_per_block);
                let output_len = rgba8_len_bytes(width, height);

                // Deterministic per-(format,size) seed so results are stable across runs.
                // Arbitrary fixed prefix; keep it stable so bench inputs don't change.
                let seed = 0xA3E0_BC00_0000_0000u64
                    ^ ((*bytes_per_block as u64) << 48)
                    ^ (u64::from(width) << 16)
                    ^ u64::from(height);

                let input = gen_deterministic_bytes(input_len, seed);
                let mut out = vec![0u8; output_len];
                let size_label = format!("{width}x{height}");

                group.throughput(Throughput::Bytes(output_len as u64));
                group.bench_function(BenchmarkId::new(*format_name, &size_label), |b| {
                    b.iter(|| {
                        func(
                            black_box(width),
                            black_box(height),
                            black_box(&input),
                            black_box(out.as_mut_slice()),
                        );
                        black_box(&out);
                    })
                });
            }
        }
        group.finish();
    }

    {
        let mut group = c.benchmark_group("bc_decompress_rgba8_pixels");
        // The pixels/sec group is redundant but provides an easy path to ns/pixel:
        //   ns/pixel = 1e9 / (pixels/sec).
        group
            .warm_up_time(Duration::from_millis(200))
            .measurement_time(Duration::from_millis(500))
            .sample_size(20);

        for (format_name, bytes_per_block, func) in FORMATS {
            for &(width, height) in SIZES {
                let input_len = bc_data_len_bytes(width, height, *bytes_per_block);
                let output_len = rgba8_len_bytes(width, height);
                let pixels = u64::from(width) * u64::from(height);

                let seed = 0xA3E0_BC00_0000_0000u64
                    ^ ((*bytes_per_block as u64) << 48)
                    ^ (u64::from(width) << 16)
                    ^ u64::from(height);

                let input = gen_deterministic_bytes(input_len, seed);
                let mut out = vec![0u8; output_len];
                let size_label = format!("{width}x{height}");

                group.throughput(Throughput::Elements(pixels));
                group.bench_function(BenchmarkId::new(*format_name, &size_label), |b| {
                    b.iter(|| {
                        func(
                            black_box(width),
                            black_box(height),
                            black_box(&input),
                            black_box(out.as_mut_slice()),
                        );
                        black_box(&out);
                    })
                });
            }
        }
        group.finish();
    }
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_bc_decompress
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
