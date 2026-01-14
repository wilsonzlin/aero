// Criterion microbenchmarks for core JIT bookkeeping components.
//
// These benches focus on small, deterministic inputs so we can track performance regressions in:
// - `CodeCache` lookups + O(1) LRU maintenance
// - bounded `HotnessProfile` bookkeeping under pressure
// - `PageVersionTracker` snapshotting for small code spans

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::JitMetricsSink;
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::profile::HotnessProfile;
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime, PageVersionTracker,
    PAGE_SIZE,
};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct CountingMetricsSink {
    cache_hit: AtomicU64,
    cache_miss: AtomicU64,
    install: AtomicU64,
    evict: AtomicU64,
    invalidate: AtomicU64,
    stale_install_reject: AtomicU64,
    compile_request: AtomicU64,
    cache_bytes_used: AtomicU64,
    cache_bytes_capacity: AtomicU64,
}

#[cfg(not(target_arch = "wasm32"))]
impl JitMetricsSink for CountingMetricsSink {
    fn record_cache_hit(&self) {
        self.cache_hit.fetch_add(1, Ordering::Relaxed);
    }

    fn record_cache_miss(&self) {
        self.cache_miss.fetch_add(1, Ordering::Relaxed);
    }

    fn record_install(&self) {
        self.install.fetch_add(1, Ordering::Relaxed);
    }

    fn record_evict(&self, n: u64) {
        self.evict.fetch_add(n, Ordering::Relaxed);
    }

    fn record_invalidate(&self) {
        self.invalidate.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stale_install_reject(&self) {
        self.stale_install_reject.fetch_add(1, Ordering::Relaxed);
    }

    fn record_compile_request(&self) {
        self.compile_request.fetch_add(1, Ordering::Relaxed);
    }

    fn set_cache_bytes(&self, used: u64, capacity: u64) {
        self.cache_bytes_used.store(used, Ordering::Relaxed);
        self.cache_bytes_capacity.store(capacity, Ordering::Relaxed);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep CI runtime low.
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
        Ok("full") => Criterion::default()
            // More stable results at the cost of runtime.
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
        _ => Criterion::default()
            // This bench target contains many sub-benchmarks. Use a moderately-sized default so
            // `cargo bench --bench jit_bookkeeping` stays practical for local runs (and within
            // `scripts/safe-run.sh`'s default timeout), while still allowing a slower/more-stable
            // run via `AERO_BENCH_PROFILE=full`.
            .warm_up_time(Duration::from_millis(250))
            .measurement_time(Duration::from_secs(1))
            .sample_size(30)
            .noise_threshold(0.03),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn dummy_handle(entry_rip: u64, table_index: u32, byte_len: u32) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index,
        // `page_versions` intentionally empty: these benches are meant to focus on bookkeeping
        // structures (HashMap lookup + O(1) LRU updates) rather than per-lookup allocation/copy.
        meta: CompiledBlockMeta {
            code_paddr: 0,
            byte_len,
            page_versions: Vec::new(),
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn dummy_handle_with_pages(
    entry_rip: u64,
    table_index: u32,
    byte_len: u32,
    pages: usize,
) -> CompiledBlockHandle {
    let mut page_versions = Vec::with_capacity(pages);
    for i in 0..pages {
        page_versions.push(PageVersionSnapshot {
            page: i as u64,
            version: i as u32,
        });
    }
    CompiledBlockHandle {
        entry_rip,
        table_index,
        meta: CompiledBlockMeta {
            code_paddr: 0,
            byte_len,
            page_versions,
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_code_cache(c: &mut Criterion) {
    const SIZES: &[usize] = &[8, 64, 256, 1024];
    // Reuse a fixed work factor inside each criterion iteration so the harness overhead is
    // negligible vs the bookkeeping we're measuring.
    const OPS_PER_ITER: usize = 1024;

    let mut group = c.benchmark_group("jit/code_cache");

    for &size in SIZES {
        // Cache hit case: exercise LRU promotion by walking the whole keyspace in a deterministic
        // order.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
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

        // Hot-hot cache hit: repeatedly hit the current LRU head so `touch_idx` early-exits without
        // relinking. This isolates the raw map lookup + handle clone overhead.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
        group.bench_with_input(
            BenchmarkId::new("get_cloned_hit_head", size),
            &size,
            |b, &size| {
                let mut cache = CodeCache::new(size, 0);
                for i in 0..size {
                    cache.insert(dummy_handle(i as u64, i as u32, 16));
                }

                let rip = (size as u64).saturating_sub(1);

                b.iter(|| {
                    let mut checksum = 0u64;
                    for _ in 0..OPS_PER_ITER {
                        let handle = cache.get_cloned(black_box(rip));
                        checksum ^= handle
                            .as_ref()
                            .map(|h| u64::from(h.table_index))
                            .unwrap_or(0);
                    }
                    black_box(checksum);
                });
            },
        );

        // Cache miss case: same lookup but for a key that will never be present.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
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

        // Insert (no eviction): replace an existing key, which exercises HashMap replacement plus
        // LRU relinking.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
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
                    // Observe both the eviction result and the resulting cache state to prevent
                    // dead-code elimination.
                    checksum ^= evicted.len() as u64;
                }
                checksum ^= cache.current_bytes() as u64;
                black_box(checksum);
            });
        });

        // Insert w/ eviction: keep the cache at steady-state capacity and insert distinct keys so
        // every insert triggers an eviction.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
        group.bench_with_input(BenchmarkId::new("insert_evict", size), &size, |b, &size| {
            let mut cache = CodeCache::new(size, 0);
            for i in 0..size {
                cache.insert(dummy_handle(i as u64, i as u32, 16));
            }

            // Use a ring of keys larger than the cache so every insert is for a missing key.
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

        // Insert w/ eviction due to max-bytes: keep `max_blocks` high enough that eviction is
        // driven by `max_bytes` accounting rather than the entry count.
        group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
        group.bench_with_input(
            BenchmarkId::new("insert_evict_max_bytes", size),
            &size,
            |b, &size| {
                let byte_len = 16u32;
                let max_bytes = size.saturating_mul(byte_len as usize);
                let max_blocks = size.saturating_mul(8).max(1);
                let mut cache = CodeCache::new(max_blocks, max_bytes);
                for i in 0..size {
                    cache.insert(dummy_handle(i as u64, i as u32, byte_len));
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
                        let handle = dummy_handle(rip, gen, byte_len);
                        let evicted = cache.insert(black_box(handle));
                        checksum ^= evicted.first().copied().unwrap_or(0);
                    }
                    checksum ^= cache.current_bytes() as u64;
                    black_box(checksum);
                });
            },
        );
    }

    // --- BM-002 required cases ---

    // Cache hit bench: large cache with a small hot working set.
    {
        const CACHE_BLOCKS: usize = 8192;
        const WORKING_SET: usize = 256;
        const LOOKUPS_PER_ITER: usize = 32 * 1024;

        group.throughput(Throughput::Elements(LOOKUPS_PER_ITER as u64));
        group.bench_function("codecache_get_hit", |b| {
            let mut cache = CodeCache::new(CACHE_BLOCKS, 0);
            for i in 0..CACHE_BLOCKS {
                let entry_rip = 0x1000u64 + (i as u64) * 16;
                cache.insert(dummy_handle(entry_rip, i as u32, 16));
            }

            // Pre-generate a deterministic hot working set access pattern.
            let mut rips = Vec::with_capacity(LOOKUPS_PER_ITER);
            for i in 0..LOOKUPS_PER_ITER {
                // Use a stride that is co-prime with `WORKING_SET` so we don't accidentally create
                // runs of identical RIPs (which would bypass the LRU `touch` path).
                let idx = (i.wrapping_mul(17)) % WORKING_SET;
                rips.push(0x1000u64 + (idx as u64) * 16);
            }

            b.iter(|| {
                let mut checksum = 0u64;
                for &rip in &rips {
                    let handle = cache
                        .get_cloned(black_box(rip))
                        .expect("RIP must exist in the code cache");
                    checksum = checksum.wrapping_add(handle.table_index as u64);
                }
                black_box(checksum);
            });
        });
    }

    // Insert+evict bench: keep the cache at steady-state capacity and insert distinct keys so
    // evictions happen on every insert.
    {
        const MAX_BLOCKS: usize = 1024;
        const STREAM_LEN: usize = MAX_BLOCKS * 16; // > MAX_BLOCKS so keys don't repeat while resident.
        const INSERTS_PER_ITER: usize = 4096;

        group.throughput(Throughput::Elements(INSERTS_PER_ITER as u64));
        group.bench_function("codecache_insert_evict", |b| {
            let mut cache = CodeCache::new(MAX_BLOCKS, 0);

            // Pre-generate a deterministic insertion stream.
            let mut rips = Vec::with_capacity(STREAM_LEN);
            for i in 0..STREAM_LEN {
                rips.push(0x2_0000u64 + (i as u64) * 16);
            }

            // Fill the cache to its steady-state occupancy (no evictions yet).
            for (i, &rip) in rips.iter().enumerate().take(MAX_BLOCKS) {
                cache.insert(dummy_handle(rip, i as u32, 32));
            }

            let mut pos = MAX_BLOCKS;

            b.iter(|| {
                let mut checksum = 0u64;
                for _ in 0..INSERTS_PER_ITER {
                    if pos >= rips.len() {
                        pos = 0;
                    }
                    let entry_rip = rips[pos];
                    pos += 1;

                    let evicted = cache.insert(dummy_handle(entry_rip, 0, 32));
                    // Touch the returned vector to ensure eviction isn't optimized away.
                    checksum ^= evicted.first().copied().unwrap_or(0);
                }
                black_box(checksum);
            });
        });
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_code_cache_clone_overhead(c: &mut Criterion) {
    // Measure the cost of cloning handles with non-empty page-version metadata (which requires
    // allocating + copying the `Vec<PageVersionSnapshot>`).
    const OPS_PER_ITER: usize = 1024;
    const PAGES: &[usize] = &[0, 1, 4, 16, 64];

    let mut group = c.benchmark_group("jit/code_cache_clone");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    for &pages in PAGES {
        group.bench_with_input(
            BenchmarkId::new("get_cloned_head_pages", pages),
            &pages,
            |b, &pages| {
                let mut cache = CodeCache::new(1024, 0);
                let rip = 0xDEAD_BEEF;
                cache.insert(dummy_handle_with_pages(rip, 1, 16, pages));

                b.iter(|| {
                    let mut checksum = 0u64;
                    for _ in 0..OPS_PER_ITER {
                        let handle = cache.get_cloned(black_box(rip));
                        checksum ^= handle
                            .as_ref()
                            .map(|h| h.meta.page_versions.len() as u64)
                            .unwrap_or(0);
                    }
                    black_box(checksum);
                });
            },
        );
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_hotness_profile(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 4096;

    let mut group = c.benchmark_group("jit/hotness_profile");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    // Hot path: we already have a compiled block, so we only do a counter increment.
    group.bench_function("record_hit_has_compiled", |b| {
        let mut profile = HotnessProfile::new(32);
        let rip = 0x1000u64;
        // Seed the HashMap/HashSet capacity outside measurement.
        profile.record_hit(rip, true);

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                profile.record_hit(black_box(rip), true);
            }
            black_box(profile.counter(rip));
        });
    });

    // Cold-but-not-hot path: no compiled block, but far from the hotness threshold so we don't
    // touch the `requested` set.
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

    // Threshold trigger path: no compiled block and we cross/are above the threshold, so we insert
    // into `requested` and return true.
    //
    // `record_hit()` only triggers once per RIP until `clear_requested()` is called, so we toggle
    // it each operation to keep exercising the trigger behavior without growing the underlying
    // HashSet.
    group.bench_function("record_hit_trigger", |b| {
        let mut profile = HotnessProfile::new(1);
        let rip = 0x3000u64;
        // Seed capacity + counter entry.
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

    // After a block crosses the threshold once, its RIP stays in the `requested` set until the
    // embedder installs the compiled block (or the entry is evicted). This is the steady-state cost
    // while compilation is in-flight.
    group.bench_function("record_hit_already_requested", |b| {
        let mut profile = HotnessProfile::new(1);
        let rip = 0x4000u64;
        assert!(
            profile.record_hit(rip, false),
            "first hit should trigger request"
        );

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                black_box(profile.record_hit(black_box(rip), false));
            }
            black_box(profile.counter(rip));
        });
    });

    // `mark_requested` steady-state: the entry is already in the requested set, so we just update
    // the counter entry's `last_hit` timestamp.
    group.bench_function("mark_requested_existing", |b| {
        let mut profile = HotnessProfile::new(32);
        let rip = 0x5000u64;
        profile.record_hit(rip, false);
        profile.mark_requested(rip);

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                profile.mark_requested(black_box(rip));
            }
            black_box(profile.counter(rip));
        });
    });

    // `mark_requested` under capacity pressure: each call inserts a new requested RIP and forces an
    // eviction.
    const MARK_EVICT_OPS_PER_ITER: usize = 256;
    group.throughput(Throughput::Elements(MARK_EVICT_OPS_PER_ITER as u64));
    group.bench_function("mark_requested_evict", |b| {
        const CAPACITY: usize = 256;
        let mut profile = HotnessProfile::new_with_capacity(32, CAPACITY);
        for i in 0..CAPACITY {
            profile.record_hit(i as u64, false);
        }

        let mut next_rip = CAPACITY as u64;
        b.iter(|| {
            for _ in 0..MARK_EVICT_OPS_PER_ITER {
                profile.mark_requested(black_box(next_rip));
                next_rip = next_rip.wrapping_add(1);
            }
            black_box(profile.len());
        });
    });

    // Pathological case: the profile is fully occupied by requested keys (e.g. compilation jobs are
    // in-flight for all entries). New RIPs cannot be inserted because we avoid evicting requested
    // entries, so `record_hit` falls back to a full scan that finds no eviction victim.
    const SATURATED_OPS_PER_ITER: usize = 256;
    group.throughput(Throughput::Elements(SATURATED_OPS_PER_ITER as u64));
    group.bench_function("record_hit_saturated_requested", |b| {
        const CAPACITY: usize = 256;

        let mut profile = HotnessProfile::new_with_capacity(u32::MAX, CAPACITY);
        for i in 0..CAPACITY {
            let rip = i as u64;
            profile.record_hit(rip, false);
            profile.mark_requested(rip);
        }

        let mut next_rip = CAPACITY as u64;
        b.iter(|| {
            for _ in 0..SATURATED_OPS_PER_ITER {
                black_box(profile.record_hit(black_box(next_rip), false));
                next_rip = next_rip.wrapping_add(1);
            }
            black_box(profile.counter(0));
        });
    });

    // Capacity pressure path: insert new RIPs once the internal counter table is full.
    //
    // This exercises the profile's eviction logic (victim selection + HashMap/HashSet removal) and
    // is a useful proxy for worst-case bookkeeping when guests execute a very large number of cold
    // blocks.
    const EVICT_OPS_PER_ITER: usize = 256;
    group.throughput(Throughput::Elements(EVICT_OPS_PER_ITER as u64));
    group.bench_function("record_hit_new_key_eviction", |b| {
        const CAPACITY: usize = 256;
        // Keep the per-iteration work factor lower here: each operation can scan the whole table to
        // pick an eviction victim.

        let mut profile = HotnessProfile::new_with_capacity(u32::MAX, CAPACITY);
        for i in 0..CAPACITY {
            profile.record_hit(i as u64, false);
        }

        let mut next_rip = CAPACITY as u64;
        b.iter(|| {
            for _ in 0..EVICT_OPS_PER_ITER {
                profile.record_hit(black_box(next_rip), false);
                next_rip = next_rip.wrapping_add(1);
            }
            black_box(profile.counter(0));
        });
    });

    // --- BM-002 required case ---
    // Mix hot/cold RIPs that exceed the bounded capacity, forcing steady-state eviction.
    {
        const CAPACITY: usize = 4096;
        const HOT_SET: usize = 64;
        const COLD_SET: usize = CAPACITY * 8;
        const HITS_PER_ITER: usize = 16_384;
        const COLD_STRIDE: usize = 64; // 1 cold RIP per 64 hits (~1.6% cold)
        const THRESHOLD: u32 = 32;

        group.throughput(Throughput::Elements(HITS_PER_ITER as u64));
        group.bench_function("hotness_record_hit", |b| {
            let mut profile = HotnessProfile::new_with_capacity(THRESHOLD, CAPACITY);

            let mut hot_rips = Vec::with_capacity(HOT_SET);
            for i in 0..HOT_SET {
                hot_rips.push(0x10_0000u64 + (i as u64) * 16);
            }

            let mut cold_rips = Vec::with_capacity(COLD_SET);
            for i in 0..COLD_SET {
                cold_rips.push(0x20_0000u64 + (i as u64) * 16);
            }

            // Warm the profile to steady state:
            // - ensure the hot set has higher counters so it won't be evicted easily.
            // - then fill the remaining capacity with cold keys so subsequent cold hits cause evictions.
            for _ in 0..THRESHOLD {
                for &rip in &hot_rips {
                    let _ = profile.record_hit(rip, false);
                }
            }
            for &rip in cold_rips.iter().take(CAPACITY.saturating_sub(HOT_SET)) {
                let _ = profile.record_hit(rip, false);
            }

            // Pre-generate a deterministic hot/cold access pattern.
            // `u16::MAX` is a sentinel meaning "use next cold RIP".
            let mut pattern = Vec::with_capacity(HITS_PER_ITER);
            for i in 0..HITS_PER_ITER {
                if i % COLD_STRIDE == 0 {
                    pattern.push(u16::MAX);
                } else {
                    pattern.push((i % HOT_SET) as u16);
                }
            }

            let mut cold_pos = 0usize;

            b.iter(|| {
                let mut requested = 0u64;
                for &slot in &pattern {
                    let rip = if slot == u16::MAX {
                        let rip = cold_rips[cold_pos];
                        cold_pos += 1;
                        if cold_pos >= cold_rips.len() {
                            cold_pos = 0;
                        }
                        rip
                    } else {
                        hot_rips[slot as usize]
                    };
                    requested = requested
                        .wrapping_add(profile.record_hit(black_box(rip), false) as u64);
                }
                black_box(requested);
            });
        });
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone)]
struct NullBackend;

#[cfg(not(target_arch = "wasm32"))]
impl JitBackend for NullBackend {
    type Cpu = ();

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        unreachable!("bench-only backend")
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct NullCompileSink;

#[cfg(not(target_arch = "wasm32"))]
impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_jit_runtime_prepare_block(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 2048;
    let mut group = c.benchmark_group("jit/runtime");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    group.bench_function("prepare_block_hit", |b| {
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);
        let rip = 0x4000u64;
        jit.install_handle(dummy_handle(rip, 1, 16));

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                let h = jit.prepare_block(black_box(rip));
                checksum ^= h.map(|h| u64::from(h.table_index)).unwrap_or(0);
            }
            black_box(checksum);
        });
    });

    group.bench_function("prepare_block_miss", |b| {
        let config = JitConfig {
            // Keep this far enough away that we don't hit the trigger path during the bench.
            hot_threshold: u32::MAX,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);
        let rip = 0x5000u64;
        // Pre-seed the hotness table entry so we don't benchmark first-hit HashMap allocation.
        black_box(jit.prepare_block(rip));

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                black_box(jit.prepare_block(black_box(rip)));
            }
            black_box(jit.hotness(rip));
        });
    });

    group.bench_function("prepare_block_miss_already_requested", |b| {
        let config = JitConfig {
            // Trigger hotness on the first call, but keep it from retriggering while requested.
            hot_threshold: 1,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);
        let rip = 0x5A00u64;

        // First call triggers the request and populates the requested set.
        black_box(jit.prepare_block(rip));

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                black_box(jit.prepare_block(black_box(rip)));
            }
            black_box(jit.hotness(rip));
        });
    });

    group.bench_function("prepare_block_hit_metrics_sink", |b| {
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let metrics = Arc::new(CountingMetricsSink::default());
        let mut jit =
            JitRuntime::new(config, NullBackend, NullCompileSink).with_metrics_sink(metrics.clone());
        let rip = 0x6000u64;
        jit.install_handle(dummy_handle(rip, 1, 16));

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                let h = jit.prepare_block(black_box(rip));
                checksum ^= h.map(|h| u64::from(h.table_index)).unwrap_or(0);
            }
            black_box(checksum);
        });
    });

    group.bench_function("prepare_block_miss_metrics_sink", |b| {
        let config = JitConfig {
            hot_threshold: u32::MAX,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let metrics = Arc::new(CountingMetricsSink::default());
        let mut jit =
            JitRuntime::new(config, NullBackend, NullCompileSink).with_metrics_sink(metrics.clone());
        let rip = 0x7000u64;
        // Pre-seed the hotness table entry so we don't benchmark first-hit HashMap allocation.
        black_box(jit.prepare_block(rip));

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                black_box(jit.prepare_block(black_box(rip)));
            }
            black_box(jit.hotness(rip));
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_jit_runtime_prepare_block_compile_request(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 2048;
    let mut group = c.benchmark_group("jit/runtime_compile_request");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    // Measures the end-to-end overhead of the hotness-triggered compile request path:
    // - HotnessProfile `requested` insertion
    // - `CompileRequestSink::request_compile` virtual call
    // - (optional) `JitMetricsSink::record_compile_request`
    #[derive(Clone)]
    struct AtomicCompileSink(Arc<AtomicU64>);

    impl CompileRequestSink for AtomicCompileSink {
        fn request_compile(&mut self, _entry_rip: u64) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    group.bench_function("prepare_block_trigger_compile", |b| {
        let config = JitConfig {
            enabled: true,
            hot_threshold: 1,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
        };

        // Pre-generate a stable RIP set so no allocations occur in the measured loop.
        let rips: Vec<u64> = (0..OPS_PER_ITER as u64).map(|i| 0xA000u64 + i).collect();

        let compile_counter = Arc::new(AtomicU64::new(0));
        b.iter_batched_ref(
            || JitRuntime::new(config.clone(), NullBackend, AtomicCompileSink(compile_counter.clone())),
            |jit| {
                for &rip in &rips {
                    black_box(jit.prepare_block(black_box(rip)));
                }
                black_box(jit.stats_snapshot());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("prepare_block_trigger_compile_metrics_sink", |b| {
        let config = JitConfig {
            enabled: true,
            hot_threshold: 1,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
        };

        let rips: Vec<u64> = (0..OPS_PER_ITER as u64).map(|i| 0xB000u64 + i).collect();

        let compile_counter = Arc::new(AtomicU64::new(0));
        let metrics = Arc::new(CountingMetricsSink::default());
        b.iter_batched_ref(
            || {
                JitRuntime::new(config.clone(), NullBackend, AtomicCompileSink(compile_counter.clone()))
                    .with_metrics_sink(metrics.clone())
            },
            |jit| {
                for &rip in &rips {
                    black_box(jit.prepare_block(black_box(rip)));
                }
                black_box(jit.stats_snapshot());
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_jit_runtime_install_handle(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 2048;

    let mut group = c.benchmark_group("jit/runtime_install");
    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));

    group.bench_function("install_handle_replace", |b| {
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);

        let rip = 0x8000u64;
        jit.install_handle(dummy_handle(rip, 0, 16));
        let mut gen = 0u32;

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                gen = gen.wrapping_add(1);
                let evicted = jit.install_handle(dummy_handle(rip, gen, 16));
                checksum ^= evicted.len() as u64;
            }
            black_box(checksum);
        });
    });

    group.bench_function("install_handle_evict", |b| {
        const CACHE_SIZE: usize = 64;
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: CACHE_SIZE,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);

        for i in 0..CACHE_SIZE {
            jit.install_handle(dummy_handle(i as u64, i as u32, 16));
        }

        let mut next_rip = CACHE_SIZE as u64;
        let mut gen = 0u32;

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                gen = gen.wrapping_add(1);
                let evicted = jit.install_handle(dummy_handle(next_rip, gen, 16));
                checksum ^= evicted.first().copied().unwrap_or(0);
                next_rip = next_rip.wrapping_add(1);
            }
            black_box(checksum);
        });
    });

    group.bench_function("install_handle_replace_metrics_sink", |b| {
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let metrics = Arc::new(CountingMetricsSink::default());
        let mut jit =
            JitRuntime::new(config, NullBackend, NullCompileSink).with_metrics_sink(metrics.clone());

        let rip = 0x9000u64;
        jit.install_handle(dummy_handle(rip, 0, 16));
        let mut gen = 0u32;

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                gen = gen.wrapping_add(1);
                let evicted = jit.install_handle(dummy_handle(rip, gen, 16));
                checksum ^= evicted.len() as u64;
            }
            black_box(checksum);
        });
    });

    group.bench_function("install_handle_evict_metrics_sink", |b| {
        const CACHE_SIZE: usize = 64;
        let config = JitConfig {
            hot_threshold: 1_000_000,
            cache_max_blocks: CACHE_SIZE,
            cache_max_bytes: 0,
            // Keep bench setup lightweight; page-version tracking isn't exercised here.
            code_version_max_pages: 0,
            ..JitConfig::default()
        };
        let metrics = Arc::new(CountingMetricsSink::default());
        let mut jit =
            JitRuntime::new(config, NullBackend, NullCompileSink).with_metrics_sink(metrics.clone());

        for i in 0..CACHE_SIZE {
            jit.install_handle(dummy_handle(i as u64, i as u32, 16));
        }

        let mut next_rip = CACHE_SIZE as u64;
        let mut gen = 0u32;

        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..OPS_PER_ITER {
                gen = gen.wrapping_add(1);
                let evicted = jit.install_handle(dummy_handle(next_rip, gen, 16));
                checksum ^= evicted.first().copied().unwrap_or(0);
                next_rip = next_rip.wrapping_add(1);
            }
            black_box(checksum);
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_page_version_tracker(c: &mut Criterion) {
    const OPS_PER_ITER: usize = 4096;

    let mut group = c.benchmark_group("jit/page_versions");

    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
    group.bench_function("bump_write_1_page", |b| {
        let tracker = PageVersionTracker::new(1024);
        let paddr = 0x1234u64;
        let len = 4usize;

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                tracker.bump_write(black_box(paddr), black_box(len));
            }
            black_box(tracker.version(0));
        });
    });

    group.throughput(Throughput::Elements(OPS_PER_ITER as u64));
    group.bench_function("bump_write_2_pages", |b| {
        let tracker = PageVersionTracker::new(1024);
        // Straddle a page boundary.
        let paddr = 0x1FF0u64;
        let len = 0x40usize;

        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                tracker.bump_write(black_box(paddr), black_box(len));
            }
            black_box(tracker.version(1));
        });
    });

    // Larger range write: fewer ops per iter so the bench stays reasonably fast under the CI
    // profile.
    const BIG_OPS_PER_ITER: usize = 256;
    group.throughput(Throughput::Elements(BIG_OPS_PER_ITER as u64));
    group.bench_function("bump_write_16_pages", |b| {
        let tracker = PageVersionTracker::new(4096);
        let paddr = 0u64;
        let len = 16 * 4096usize;

        b.iter(|| {
            for _ in 0..BIG_OPS_PER_ITER {
                tracker.bump_write(black_box(paddr), black_box(len));
            }
            black_box(tracker.version(0));
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_page_versions_snapshot_small(c: &mut Criterion) {
    let tracker = PageVersionTracker::new(1024);

    // Pre-populate a small, contiguous region so `snapshot` reads from the dense table rather than
    // always hitting the implicit "version 0" path.
    let base_page = 0x100u64;
    for i in 0..8u64 {
        tracker.set_version(base_page + i, (i as u32).wrapping_mul(31).wrapping_add(7));
    }

    // Snapshot a 4-page span (typical for small blocks that may straddle page boundaries).
    let code_paddr = base_page * PAGE_SIZE;
    let byte_len = (PAGE_SIZE * 4) as u32;

    let mut group = c.benchmark_group("jit/page_versions");
    group.throughput(Throughput::Elements(4));
    group.bench_function("page_versions_snapshot_small", |b| {
        b.iter(|| {
            let snapshot = tracker.snapshot(black_box(code_paddr), black_box(byte_len));
            black_box(snapshot);
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_code_cache, bench_code_cache_clone_overhead, bench_hotness_profile, bench_jit_runtime_prepare_block, bench_jit_runtime_prepare_block_compile_request, bench_jit_runtime_install_handle, bench_page_version_tracker, bench_page_versions_snapshot_small
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
