// Criterion microbenchmarks for JIT tiering bookkeeping overhead.
//
// Focus: regress-test the costs of:
// - `CodeCache` lookups + O(1) LRU maintenance
// - `HotnessProfile::record_hit` counter updates + hot-threshold trigger
//
// These benches intentionally keep per-iteration allocations to a minimum by:
// - pre-generating RIP sequences / working sets
// - reusing a single cache/profile per benchmark
// - using empty page-version metadata (no clone allocation)
//
// The default configuration is CI-friendly so `bash ./scripts/safe-run.sh cargo bench ...` fits
// under the default timeout. Set `AERO_BENCH_PROFILE=full` for longer local runs.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::profile::HotnessProfile;
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    // Default to a CI-friendly profile so `cargo bench` completes under `scripts/safe-run.sh`'s
    // default timeout. Opt into longer runs explicitly with `AERO_BENCH_PROFILE=full`.
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("full") => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
        _ => Criterion::default()
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn dummy_handle(entry_rip: u64, table_index: u32, byte_len: u32) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index,
        // Empty `page_versions` avoids per-lookup allocation/copy during `get_cloned()`.
        meta: CompiledBlockMeta {
            code_paddr: 0,
            byte_len,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_code_cache(c: &mut Criterion) {
    const SIZES: &[usize] = &[8, 64, 256, 1024];
    const OPS_PER_ITER: usize = 1024;

    let mut group = c.benchmark_group("jit/code_cache");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    for &size in SIZES {
        group.bench_with_input(BenchmarkId::new("get_cloned_hit", size), &size, |b, &size| {
            let mut cache = CodeCache::new(size, 0);
            for i in 0..size {
                cache.insert(dummy_handle(i as u64, i as u32, 16));
            }

            let rips: Vec<u64> = (0..size as u64).collect();
            let mut pos = 0usize;

            b.iter(|| {
                let mut checksum = 0u64;
                for _ in 0..OPS_PER_ITER {
                    let rip = rips[pos];
                    pos = (pos + 1) % rips.len();
                    let handle = cache.get_cloned(black_box(rip));
                    checksum ^= handle
                        .as_ref()
                        .map(|h| u64::from(h.table_index))
                        .unwrap_or(0);
                }
                black_box(checksum);
            });
        });

        group.bench_with_input(BenchmarkId::new("get_cloned_miss", size), &size, |b, &size| {
            let mut cache = CodeCache::new(size, 0);
            for i in 0..size {
                cache.insert(dummy_handle(i as u64, i as u32, 16));
            }

            let miss_rip = (size as u64).wrapping_mul(0x1_0000).wrapping_add(123);

            b.iter(|| {
                let mut checksum = 0u64;
                for _ in 0..OPS_PER_ITER {
                    let handle = cache.get_cloned(black_box(miss_rip));
                    checksum ^= handle.is_some() as u64;
                }
                black_box(checksum);
            });
        });

        // Replace existing keys: no eviction, but exercises HashMap replacement + LRU relinking.
        group.bench_with_input(BenchmarkId::new("insert_replace", size), &size, |b, &size| {
            let mut cache = CodeCache::new(size, 0);
            for i in 0..size {
                cache.insert(dummy_handle(i as u64, i as u32, 16));
            }

            let rips: Vec<u64> = (0..size as u64).collect();
            let mut pos = 0usize;
            let mut gen = 0u32;

            b.iter(|| {
                let mut checksum = 0u64;
                for _ in 0..OPS_PER_ITER {
                    let rip = rips[pos];
                    pos = (pos + 1) % rips.len();

                    gen = gen.wrapping_add(1);
                    let handle = dummy_handle(rip, gen, 16);
                    let evicted = cache.insert(black_box(handle));
                    checksum ^= evicted.len() as u64;
                }
                checksum ^= cache.len() as u64;
                black_box(checksum);
            });
        });

        // Insert distinct keys into a full cache so every insert triggers an eviction.
        group.bench_with_input(BenchmarkId::new("insert_evict", size), &size, |b, &size| {
            let mut cache = CodeCache::new(size, 0);
            for i in 0..size {
                cache.insert(dummy_handle(i as u64, i as u32, 16));
            }

            let insert_rips: Vec<u64> = (size as u64..(size as u64 * 3)).collect();
            let mut pos = 0usize;
            let mut gen = 0u32;

            b.iter(|| {
                let mut checksum = 0u64;
                for _ in 0..OPS_PER_ITER {
                    let rip = insert_rips[pos];
                    pos = (pos + 1) % insert_rips.len();

                    gen = gen.wrapping_add(1);
                    let handle = dummy_handle(rip, gen, 16);
                    let evicted = cache.insert(black_box(handle));
                    checksum ^= evicted.first().copied().unwrap_or(0);
                }
                checksum ^= cache.len() as u64;
                black_box(checksum);
            });
        });
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_hotness_profile(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 4096;

    let mut group = c.benchmark_group("jit/hotness_profile");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    // Hot hit: we already have a compiled block, so we only do a counter increment.
    group.bench_function("record_hit_has_compiled", |b| {
        let mut profile = HotnessProfile::new(32);
        let rip = 0x1000u64;
        profile.record_hit(rip, true);

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                profile.record_hit(black_box(rip), true);
            }
            black_box(profile.counter(rip));
        });
    });

    // Cold-but-not-hot: no compiled block, but far from the threshold so `requested` isn't touched.
    group.bench_function("record_hit_below_threshold", |b| {
        let mut profile = HotnessProfile::new(u32::MAX);
        let rip = 0x2000u64;
        profile.record_hit(rip, false);

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                profile.record_hit(black_box(rip), false);
            }
            black_box(profile.counter(rip));
        });
    });

    // Threshold trigger path: crossing the threshold inserts into `requested` and returns true.
    // Toggle `clear_requested()` each op to keep exercising the trigger without growing the set.
    group.bench_function("record_hit_trigger", |b| {
        let mut profile = HotnessProfile::new(1);
        let rip = 0x3000u64;
        profile.record_hit(rip, false);
        profile.clear_requested(rip);

        b.iter(|| {
            let mut triggered = 0u64;
            for _ in 0..OPS_PER_ITER {
                if profile.record_hit(black_box(rip), false) {
                    triggered += 1;
                }
                profile.clear_requested(rip);
            }
            black_box(triggered);
        });
    });

    // Steady state while compilation is in-flight: RIP is already in `requested`.
    group.bench_function("record_hit_already_requested", |b| {
        let mut profile = HotnessProfile::new(1);
        let rip = 0x4000u64;
        assert!(profile.record_hit(rip, false));

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                black_box(profile.record_hit(black_box(rip), false));
            }
            black_box(profile.counter(rip));
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_code_cache, bench_hotness_profile
}

#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);

