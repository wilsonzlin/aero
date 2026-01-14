#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

// Required by the included `tile_diff` module (`use crate::Rect;`).
#[cfg(not(target_arch = "wasm32"))]
use aero_gpu::{merge_and_cap_rects, Rect};
#[cfg(not(target_arch = "wasm32"))]
use aero_shared::shared_framebuffer::{dirty_tiles_to_rects, SharedFramebufferLayout};

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
            // This bench contains many parameter combinations; keep the per-benchmark timing
            // budget tight so `cargo bench -p aero-gpu` stays reasonable in CI.
            .measurement_time(Duration::from_millis(400))
            .sample_size(10)
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
fn bench_dirty_tiles_to_rects(c: &mut Criterion) {
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

    let mut group = c.benchmark_group("dirty_tiles_to_rects");

    for &tile_size in &tile_sizes {
        for size in sizes {
            let layout = SharedFramebufferLayout::new_rgba8(size.width, size.height, tile_size)
                .expect("valid layout");
            let tile_count = layout.tile_count();
            group.throughput(Throughput::Elements(tile_count as u64));

            let word_len = layout.dirty_words_per_buffer as usize;

            // Fast-path: full dirty bitset should collapse to a single full-frame rect.
            let all_dirty_words = vec![u32::MAX; word_len];
            group.bench_function(
                BenchmarkId::new(
                    "all_dirty",
                    format!("{}x{}_tile{}", size.width, size.height, tile_size),
                ),
                move |b| {
                    b.iter(|| {
                        let rects = dirty_tiles_to_rects(layout, black_box(&all_dirty_words));
                        black_box(rects.len());
                    })
                },
            );

            // Stress-path: alternating bits produces many small rects (one per dirty tile per row).
            let mut checkerboard_words = vec![0u32; word_len];
            for tile_index in 0..tile_count {
                if (tile_index & 1) == 0 {
                    let word = tile_index / 32;
                    let bit = tile_index % 32;
                    checkerboard_words[word] |= 1u32 << bit;
                }
            }
            group.bench_function(
                BenchmarkId::new(
                    "checkerboard",
                    format!("{}x{}_tile{}", size.width, size.height, tile_size),
                ),
                move |b| {
                    b.iter(|| {
                        let rects = dirty_tiles_to_rects(layout, black_box(&checkerboard_words));
                        black_box(rects.len());
                    })
                },
            );
        }
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_merge_and_cap_rects(c: &mut Criterion) {
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

    let mut group = c.benchmark_group("merge_and_cap_rects");

    // Mirrors `present.rs` default.
    const CAP: usize = 128;

    for &tile_size in &tile_sizes {
        for size in sizes {
            let layout = SharedFramebufferLayout::new_rgba8(size.width, size.height, tile_size)
                .expect("valid layout");
            let tile_count = layout.tile_count();
            let word_len = layout.dirty_words_per_buffer as usize;

            // Use the same "checkerboard" dirty pattern as the `dirty_tiles_to_rects` bench so
            // `merge_and_cap_rects` sees a large, fragmented rect list.
            let mut checkerboard_words = vec![0u32; word_len];
            for tile_index in 0..tile_count {
                if (tile_index & 1) == 0 {
                    let word = tile_index / 32;
                    let bit = tile_index % 32;
                    checkerboard_words[word] |= 1u32 << bit;
                }
            }

            let shared_rects = dirty_tiles_to_rects(layout, &checkerboard_words);
            let rects: Vec<Rect> = shared_rects
                .iter()
                .map(|r| Rect::new(r.x, r.y, r.width, r.height))
                .collect();

            group.throughput(Throughput::Elements(rects.len() as u64));
            group.bench_function(
                BenchmarkId::new(
                    "checkerboard_cap128",
                    format!("{}x{}_tile{}", size.width, size.height, tile_size),
                ),
                move |b| {
                    b.iter(|| {
                        let out =
                            merge_and_cap_rects(black_box(&rects), (size.width, size.height), CAP);
                        black_box(out.rects.len());
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
    targets = bench_tile_diff, bench_dirty_tiles_to_rects, bench_merge_and_cap_rects
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
