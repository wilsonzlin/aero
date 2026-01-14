use std::time::Duration;

use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::profile::HotnessProfile;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep PR runtime low.
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

/// Deterministic RNG suitable for microbench input generation without pulling in `rand`.
#[derive(Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // https://en.wikipedia.org/wiki/Splitmix64
        let mut z = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.state = z;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_usize(&mut self, upper_exclusive: usize) -> usize {
        debug_assert!(upper_exclusive != 0);
        (self.next_u64() as usize) % upper_exclusive
    }
}

const CACHE_BLOCKS: usize = 10_000;
const QUERY_COUNT: usize = 8_192; // power-of-two for cheap wrapping
const RNG_SEED: u64 = 0xDDBA_7D66_9E3B_4A01;

fn rip_for_index(idx: usize) -> u64 {
    // Use a small stride so RIPs look like real instruction pointer values (aligned).
    (idx as u64) << 4
}

fn make_handle(entry_rip: u64) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index: (entry_rip as u32).wrapping_mul(2654435761),
        meta: CompiledBlockMeta {
            code_paddr: entry_rip,
            byte_len: 32,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count: 1,
            inhibit_interrupts_after_block: false,
        },
    }
}

fn build_cache_near_capacity() -> CodeCache {
    let mut cache = CodeCache::new(CACHE_BLOCKS, 0);
    for i in 0..CACHE_BLOCKS {
        cache.insert(make_handle(rip_for_index(i)));
    }
    cache
}

fn bench_code_cache(c: &mut Criterion) {
    // --- get_cloned patterns ---
    let mut group = c.benchmark_group("code_cache");
    group.throughput(Throughput::Elements(1));

    group.bench_function("get_cloned_hit_100pct", |b| {
        let mut cache = build_cache_near_capacity();

        let mut rng = SplitMix64::new(RNG_SEED);
        let queries: Vec<u64> = (0..QUERY_COUNT)
            .map(|_| rip_for_index(rng.next_usize(CACHE_BLOCKS)))
            .collect();

        let mut idx = 0usize;
        b.iter(|| {
            let rip = queries[idx & (QUERY_COUNT - 1)];
            idx = idx.wrapping_add(1);
            black_box(cache.get_cloned(black_box(rip)));
        });
    });

    group.bench_function("get_cloned_hit_50pct", |b| {
        let mut cache = build_cache_near_capacity();

        let mut rng = SplitMix64::new(RNG_SEED ^ 0xA5A5_A5A5_A5A5_A5A5);
        let queries: Vec<u64> = (0..QUERY_COUNT)
            .map(|i| {
                if (i & 1) == 0 {
                    rip_for_index(rng.next_usize(CACHE_BLOCKS))
                } else {
                    // Guaranteed miss: outside the initial pre-filled range.
                    rip_for_index(CACHE_BLOCKS + rng.next_usize(CACHE_BLOCKS))
                }
            })
            .collect();

        let mut idx = 0usize;
        b.iter(|| {
            let rip = queries[idx & (QUERY_COUNT - 1)];
            idx = idx.wrapping_add(1);
            black_box(cache.get_cloned(black_box(rip)));
        });
    });

    group.bench_function("get_cloned_miss_0pct", |b| {
        let mut cache = build_cache_near_capacity();

        let mut rng = SplitMix64::new(RNG_SEED ^ 0x5A5A_5A5A_5A5A_5A5A);
        let queries: Vec<u64> = (0..QUERY_COUNT)
            .map(|_| rip_for_index(CACHE_BLOCKS + rng.next_usize(CACHE_BLOCKS)))
            .collect();

        let mut idx = 0usize;
        b.iter(|| {
            let rip = queries[idx & (QUERY_COUNT - 1)];
            idx = idx.wrapping_add(1);
            black_box(cache.get_cloned(black_box(rip)));
        });
    });

    // --- insert/evict ---
    const INSERT_OPS: usize = 1_024;
    group.throughput(Throughput::Elements(INSERT_OPS as u64));
    group.bench_function("insert_evict", |b| {
        let mut cache = build_cache_near_capacity();
        let mut next_rip = rip_for_index(CACHE_BLOCKS);
        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..INSERT_OPS {
                let evicted = cache.insert(make_handle(next_rip));
                if let Some(&rip) = evicted.first() {
                    checksum ^= rip;
                }
                next_rip = next_rip.wrapping_add(0x10);
            }
            black_box(checksum);
        });
    });

    group.finish();
}

fn bench_hotness_profile(c: &mut Criterion) {
    let mut group = c.benchmark_group("hotness_profile");
    group.throughput(Throughput::Elements(1));

    // Common RIP set: same scale as the code cache to mimic steady-state dispatch tables.
    let rips: Vec<u64> = (0..CACHE_BLOCKS).map(rip_for_index).collect();
    let profile_capacity = HotnessProfile::recommended_capacity(CACHE_BLOCKS);

    // --- record_hit: compiled path (no requested/threshold logic) ---
    group.bench_function("record_hit_compiled_sequential", |b| {
        let mut profile = HotnessProfile::new_with_capacity(32, profile_capacity);
        for &rip in &rips {
            profile.record_hit(rip, true);
        }

        let len = rips.len();
        let mut idx = 0usize;
        b.iter(|| {
            let rip = rips[idx];
            idx = idx.wrapping_add(1);
            if idx == len {
                idx = 0;
            }
            black_box(profile.record_hit(black_box(rip), true));
        });
    });

    group.bench_function("record_hit_compiled_random", |b| {
        let mut profile = HotnessProfile::new_with_capacity(32, profile_capacity);
        for &rip in &rips {
            profile.record_hit(rip, true);
        }

        let mut rng = SplitMix64::new(RNG_SEED ^ 0x1234_5678_9ABC_DEF0);
        let queries: Vec<u64> = (0..QUERY_COUNT)
            .map(|_| rips[rng.next_usize(rips.len())])
            .collect();

        let mut idx = 0usize;
        b.iter(|| {
            let rip = queries[idx & (QUERY_COUNT - 1)];
            idx = idx.wrapping_add(1);
            black_box(profile.record_hit(black_box(rip), true));
        });
    });

    // --- record_hit: uncompiled path (threshold + requested-set checks) ---
    const THRESHOLD: u32 = 16;
    group.bench_function("record_hit_uncompiled_sequential", |b| {
        let mut profile = HotnessProfile::new_with_capacity(THRESHOLD, profile_capacity);
        // Drive each RIP above the threshold and into the requested set so the benchmark measures
        // the steady-state cost of the bookkeeping (HashMap counter + HashSet probe).
        for &rip in &rips {
            for _ in 0..THRESHOLD {
                profile.record_hit(rip, false);
            }
        }

        let len = rips.len();
        let mut idx = 0usize;
        b.iter(|| {
            let rip = rips[idx];
            idx = idx.wrapping_add(1);
            if idx == len {
                idx = 0;
            }
            black_box(profile.record_hit(black_box(rip), false));
        });
    });

    group.bench_function("record_hit_uncompiled_random", |b| {
        let mut profile = HotnessProfile::new_with_capacity(THRESHOLD, profile_capacity);
        for &rip in &rips {
            for _ in 0..THRESHOLD {
                profile.record_hit(rip, false);
            }
        }

        let mut rng = SplitMix64::new(RNG_SEED ^ 0x0F0F_0F0F_0F0F_0F0F);
        let queries: Vec<u64> = (0..QUERY_COUNT)
            .map(|_| rips[rng.next_usize(rips.len())])
            .collect();

        let mut idx = 0usize;
        b.iter(|| {
            let rip = queries[idx & (QUERY_COUNT - 1)];
            idx = idx.wrapping_add(1);
            black_box(profile.record_hit(black_box(rip), false));
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_code_cache, bench_hotness_profile
}
criterion_main!(benches);
