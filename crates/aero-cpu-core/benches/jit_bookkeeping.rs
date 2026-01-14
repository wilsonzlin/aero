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
use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::profile::HotnessProfile;
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

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
        _ => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn dummy_handle(entry_rip: u64, table_index: u32, byte_len: u32) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index,
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
            enabled: true,
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
            enabled: true,
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

    group.bench_function("prepare_block_hit_metrics_sink", |b| {
        let config = JitConfig {
            enabled: true,
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
            enabled: true,
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
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_code_cache, bench_hotness_profile, bench_jit_runtime_prepare_block
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
