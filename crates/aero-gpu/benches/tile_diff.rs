#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

// Required by the included `tile_diff` module (`use crate::Rect;`).
#[cfg(not(target_arch = "wasm32"))]
use aero_gpu::Rect;

// We keep the benchmark compiled even when the `diff-engine` feature is disabled by including the
// implementation directly. This ensures `cargo bench -p aero-gpu` always exercises the current
// diff logic without requiring feature flags.
#[cfg(not(target_arch = "wasm32"))]
#[path = "../src/tile_diff.rs"]
mod tile_diff;

#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep PR runtime low.
            .warm_up_time(Duration::from_millis(150))
            // Some frame sizes can take multiple milliseconds per iteration; keep enough budget to
            // avoid Criterion extending the measurement window (which makes CI output noisier).
            .measurement_time(Duration::from_millis(600))
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
#[derive(Clone, Copy, Debug)]
struct FrameSize {
    width: u32,
    height: u32,
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tile_diff(c: &mut Criterion) {
    const BYTES_PER_PIXEL: usize = 4;
    let sizes = [
        FrameSize {
            width: 800,
            height: 600,
        },
        FrameSize {
            width: 1024,
            height: 768,
        },
        FrameSize {
            width: 1280,
            height: 720,
        },
    ];
    let tile_sizes = [16u32, 32u32, 64u32];

    let mut group = c.benchmark_group("tile_diff_dirty_tiles");

    for &tile_size in &tile_sizes {
        for size in sizes {
            let row_bytes = size.width as usize * BYTES_PER_PIXEL;
            let len = row_bytes * size.height as usize;
            group.throughput(Throughput::Bytes(len as u64));

            // `no_change`: identical frames (best-case for "tiles dirty" count).
            let frame_same = vec![0x3Cu8; len];
            let mut diff = if tile_size == 32 {
                tile_diff::TileDiff::new(size.width, size.height, BYTES_PER_PIXEL)
            } else {
                tile_diff::TileDiff::new_with_tile_size(
                    size.width,
                    size.height,
                    BYTES_PER_PIXEL,
                    tile_size,
                )
            };
            // Prime the internal snapshot to avoid measuring one-time allocation.
            let _ = diff.diff(&frame_same, row_bytes);

            group.bench_function(
                BenchmarkId::new(
                    "no_change",
                    format!("{}x{}_tile{}", size.width, size.height, tile_size),
                ),
                move |b| {
                    b.iter(|| {
                        let dirty = diff.diff(black_box(&frame_same), row_bytes);
                        black_box(dirty.len());
                    })
                },
            );

            // `all_change`: alternating between two frames with all bytes different.
            let frame_a = vec![0u8; len];
            let frame_b = vec![0xFFu8; len];
            let mut diff = if tile_size == 32 {
                tile_diff::TileDiff::new(size.width, size.height, BYTES_PER_PIXEL)
            } else {
                tile_diff::TileDiff::new_with_tile_size(
                    size.width,
                    size.height,
                    BYTES_PER_PIXEL,
                    tile_size,
                )
            };
            let _ = diff.diff(&frame_a, row_bytes);
            let mut flip = false;

            group.bench_function(
                BenchmarkId::new(
                    "all_change",
                    format!("{}x{}_tile{}", size.width, size.height, tile_size),
                ),
                move |b| {
                    b.iter(|| {
                        let frame = if flip { &frame_a } else { &frame_b };
                        flip = !flip;
                        let dirty = diff.diff(black_box(frame), row_bytes);
                        black_box(dirty.len());
                    })
                },
            );
        }
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tile_diff
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
