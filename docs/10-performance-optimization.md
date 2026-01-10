# 10 - Performance Optimization Strategies

## Overview

Achieving acceptable performance for Windows 7 emulation requires aggressive optimization at every layer. This document details strategies across CPU, memory, graphics, and I/O subsystems.

---

## Performance Budget

### Target Metrics

| Metric | Target | Critical Threshold |
|--------|--------|-------------------|
| Boot Time | < 60s | < 120s |
| Desktop FPS | ≥ 30 | ≥ 15 |
| Application Launch | < 5s | < 15s |
| Input Latency | < 50ms | < 100ms |
| Memory Overhead | < 1.5x | < 2x |
| IPS (MIPS) | ≥ 500 | ≥ 200 |

### Performance Breakdown (Target)

```
┌─────────────────────────────────────────────────────────────────┐
│                    CPU Time Distribution                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  JIT-compiled code execution     40%  ████████████████          │
│  Memory access / MMU             25%  ██████████                 │
│  Device emulation                15%  ██████                     │
│  Interpreter (cold code)         10%  ████                       │
│  JIT compilation                  5%  ██                         │
│  Synchronization / IPC            5%  ██                         │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## CPU Optimization

### JIT Compilation Strategy

#### Block Detection and Compilation Thresholds

```rust
pub struct JitConfig {
    // Tier thresholds
    pub tier1_threshold: u32,     // 10 executions → baseline JIT
    pub tier2_threshold: u32,     // 1000 executions → optimizing JIT
    
    // Block limits
    pub max_block_size: usize,    // 1000 instructions
    pub max_trace_length: usize,  // 5000 instructions
    
    // Cache sizes
    pub code_cache_size: usize,   // 256 MB
    pub max_compiled_blocks: usize, // 100,000
    
    // Optimization settings
    pub inline_threshold: usize,  // Inline functions < 50 instructions
    pub unroll_threshold: usize,  // Unroll loops < 16 iterations
}
```

#### Hot Path Optimization

```rust
pub struct HotPathOptimizer {
    profile: ProfileData,
}

impl HotPathOptimizer {
    pub fn optimize_trace(&self, trace: &Trace) -> OptimizedTrace {
        let mut ir = self.build_ir(trace);
        
        // Optimization passes in order
        self.constant_folding(&mut ir);
        self.dead_code_elimination(&mut ir);
        self.common_subexpression_elimination(&mut ir);
        self.flag_elimination(&mut ir);        // x86-specific
        self.strength_reduction(&mut ir);
        self.loop_invariant_code_motion(&mut ir);
        self.register_allocation(&mut ir);
        self.instruction_scheduling(&mut ir);
        
        self.generate_wasm(&ir)
    }
    
    /// Eliminate redundant flag computations
    fn flag_elimination(&self, ir: &mut IrGraph) {
        // Track which flags are live at each point
        let liveness = self.compute_flag_liveness(ir);
        
        for block in ir.blocks_mut() {
            for inst in block.instructions_mut() {
                if let Some(flags_written) = inst.flags_written() {
                    let live_flags = liveness.at(inst.id());
                    let needed_flags = flags_written & live_flags;
                    
                    if needed_flags.is_empty() {
                        // No flags needed - remove computation entirely
                        inst.remove_flag_computation();
                    } else if needed_flags != flags_written {
                        // Only some flags needed - optimize
                        inst.set_required_flags(needed_flags);
                    }
                }
            }
        }
    }
}
```

#### WASM SIMD Utilization

```rust
impl WasmCodegen {
    fn emit_sse_operation(&mut self, op: &SseOp) {
        match op {
            SseOp::Addps { dst, src } => {
                // Use WASM SIMD v128 operations
                self.emit_v128_load(src);
                self.emit_v128_load(dst);
                self.emit(Instruction::F32x4Add);
                self.emit_v128_store(dst);
            }
            SseOp::Mulps { dst, src } => {
                self.emit_v128_load(src);
                self.emit_v128_load(dst);
                self.emit(Instruction::F32x4Mul);
                self.emit_v128_store(dst);
            }
            SseOp::Shufps { dst, src, imm } => {
                // WASM doesn't have direct shuffle - use swizzle
                self.emit_v128_load(src);
                self.emit_v128_load(dst);
                self.emit_shuffle_pattern(imm);
                self.emit(Instruction::I8x16Swizzle);
                self.emit_v128_store(dst);
            }
            // ... more SSE operations
        }
    }
}
```

### Interpreter Optimization

For cold code that doesn't warrant JIT compilation:

```rust
/// Threaded interpreter using computed goto pattern
pub struct ThreadedInterpreter {
    // Dispatch table indexed by opcode
    dispatch_table: [fn(&mut CpuState, &mut MemoryBus, u64) -> u64; 256],
}

impl ThreadedInterpreter {
    pub fn run(&mut self, cpu: &mut CpuState, memory: &mut MemoryBus) {
        loop {
            let opcode = memory.read_u8(cpu.rip);
            let next_ip = self.dispatch_table[opcode as usize](cpu, memory, cpu.rip);
            cpu.rip = next_ip;
            
            // Check for events periodically
            if cpu.instruction_count % 1000 == 0 {
                if self.check_events(cpu) {
                    break;
                }
            }
        }
    }
}

// Pre-decode common instruction sequences
pub struct DecodedBlock {
    instructions: Vec<DecodedInstruction>,
    cached_addresses: Vec<u64>,
}
```

---

## Memory Optimization

### JIT Memory Access Fast Path (Inline TLB + Direct RAM)

The single biggest baseline-JIT overhead in an emulator is usually **memory helper calls** (virtual→physical translation + MMIO routing) emitted for every guest load/store.

For common-case RAM accesses we should instead generate:

- **inline TLB lookup** against a compact JIT-visible TLB stored in linear memory
- **direct WASM `load/store`** from the guest RAM region on hit
- **clean exits** back to the runtime for MMIO / ROM / unmapped addresses
- **slow-path translation** (`mmu_translate_slow`) only on TLB miss or permission failure

This reduces imported calls from “every memory op” to “once per page” for tight loops, and turns the hot path into a small amount of integer work plus a final linear-memory load/store.

Key metrics to track:

- `mmu_translate_slow_calls / guest_mem_ops` (should approach `~1 / (page_bytes / access_bytes)` for streaming loops)
- `jit_exit_mmio_calls` (should match actual device accesses; validate that it does **not** trigger for RAM)
- instruction throughput delta for synthetic memory loops (interpreter vs baseline JIT vs optimized JIT)

### TLB Optimization

```rust
pub struct OptimizedTlb {
    // Hot entries (most recently used)
    hot_cache: [TlbEntry; 16],
    hot_count: usize,
    
    // Main TLB (set-associative)
    main_cache: [[TlbEntry; 4]; 256],  // 1024 entries, 4-way
    
    // Large page TLB
    large_cache: [TlbEntry; 32],
}

impl OptimizedTlb {
    #[inline(always)]
    pub fn lookup(&mut self, vaddr: u64) -> Option<u64> {
        let vpn = vaddr >> 12;
        
        // Check hot cache first (most common case)
        for entry in &self.hot_cache[..self.hot_count] {
            if entry.vpn == vpn && entry.valid {
                return Some(entry.ppn | (vaddr & 0xFFF));
            }
        }
        
        // Check main cache
        let set = (vpn as usize) & 0xFF;
        for entry in &self.main_cache[set] {
            if entry.vpn == vpn && entry.valid {
                // Promote to hot cache
                self.promote_to_hot(*entry);
                return Some(entry.ppn | (vaddr & 0xFFF));
            }
        }
        
        // Check large page TLB
        let large_vpn = vaddr >> 21;  // 2MB pages
        for entry in &self.large_cache {
            if entry.vpn == large_vpn && entry.valid && entry.large_page {
                return Some(entry.ppn | (vaddr & 0x1F_FFFF));
            }
        }
        
        None
    }
}
```

### Memory Access Batching

```rust
/// Batch memory operations for better cache utilization
pub struct MemoryBatcher {
    pending_reads: Vec<(u64, usize, usize)>,  // (addr, size, result_idx)
    pending_writes: Vec<(u64, usize, u64)>,   // (addr, size, value)
    results: Vec<u64>,
}

impl MemoryBatcher {
    pub fn queue_read(&mut self, addr: u64, size: usize) -> usize {
        let idx = self.results.len();
        self.results.push(0);
        self.pending_reads.push((addr, size, idx));
        idx
    }
    
    pub fn flush(&mut self, memory: &MemoryBus) {
        // Sort by address for sequential access
        self.pending_reads.sort_by_key(|(addr, _, _)| *addr);
        
        for (addr, size, idx) in &self.pending_reads {
            self.results[*idx] = memory.read(*addr, *size);
        }
        
        // Process writes
        self.pending_writes.sort_by_key(|(addr, _, _)| *addr);
        for (addr, size, value) in &self.pending_writes {
            memory.write(*addr, *size, *value);
        }
        
        self.pending_reads.clear();
        self.pending_writes.clear();
    }
}
```

### Sparse Memory with Lazy Allocation

```rust
pub struct LazyMemory {
    // Page table (1 entry per 4KB page)
    pages: Vec<Option<Box<[u8; 4096]>>>,
    total_pages: usize,
    allocated_pages: usize,
    
    // Zero page for unallocated reads
    zero_page: [u8; 4096],
}

impl LazyMemory {
    pub fn read(&self, addr: u64) -> u8 {
        let page_idx = (addr >> 12) as usize;
        let offset = (addr & 0xFFF) as usize;
        
        match &self.pages[page_idx] {
            Some(page) => page[offset],
            None => 0,  // Unallocated = zero
        }
    }
    
    pub fn write(&mut self, addr: u64, value: u8) {
        let page_idx = (addr >> 12) as usize;
        let offset = (addr & 0xFFF) as usize;
        
        // Allocate on first write
        if self.pages[page_idx].is_none() {
            self.pages[page_idx] = Some(Box::new([0u8; 4096]));
            self.allocated_pages += 1;
        }
        
        self.pages[page_idx].as_mut().unwrap()[offset] = value;
    }
}
```

---

## Graphics Optimization

### Draw Call Batching

```rust
pub struct DrawBatcher {
    batches: Vec<DrawBatch>,
    current_batch: Option<DrawBatch>,
}

impl DrawBatcher {
    pub fn add_draw(&mut self, draw: DrawCall) {
        if let Some(ref mut batch) = self.current_batch {
            if batch.can_merge(&draw) {
                batch.merge(draw);
                return;
            }
            self.batches.push(self.current_batch.take().unwrap());
        }
        self.current_batch = Some(DrawBatch::from(draw));
    }
    
    pub fn flush(&mut self, encoder: &mut GPUCommandEncoder) {
        if let Some(batch) = self.current_batch.take() {
            self.batches.push(batch);
        }
        
        // Sort batches to minimize state changes
        self.batches.sort_by_key(|b| (b.pipeline_hash, b.texture_hash));
        
        let mut current_pipeline = 0;
        let mut current_texture = 0;
        
        for batch in &self.batches {
            if batch.pipeline_hash != current_pipeline {
                encoder.set_pipeline(&batch.pipeline);
                current_pipeline = batch.pipeline_hash;
            }
            if batch.texture_hash != current_texture {
                encoder.set_bind_group(0, &batch.textures);
                current_texture = batch.texture_hash;
            }
            
            encoder.draw(batch.vertex_count, batch.instance_count, 0, 0);
        }
        
        self.batches.clear();
    }
}
```

### Shader Compilation Caching

```rust
pub struct ShaderCache {
    // In-memory cache
    compiled: HashMap<ShaderKey, CompiledShader>,
    
    // Persistent cache (IndexedDB)
    persistent: PersistentCache,
    
    // Compilation queue
    pending: VecDeque<ShaderCompileRequest>,
}

impl ShaderCache {
    pub async fn get_shader(&mut self, dxbc: &[u8], device: &GPUDevice) -> &CompiledShader {
        let key = ShaderKey::from_bytecode(dxbc);
        
        // Check memory cache
        if self.compiled.contains_key(&key) {
            return self.compiled.get(&key).unwrap();
        }
        
        // Check persistent cache
        if let Some(cached) = self.persistent.get(&key).await {
            let module = device.create_shader_module(&GPUShaderModuleDescriptor {
                code: &cached.wgsl,
            });
            self.compiled.insert(key.clone(), CompiledShader { module, wgsl: cached.wgsl });
            return self.compiled.get(&key).unwrap();
        }
        
        // Compile (potentially async)
        let wgsl = translate_dxbc_to_wgsl(dxbc);
        let module = device.create_shader_module(&GPUShaderModuleDescriptor {
            code: &wgsl,
        });
        
        // Store in both caches
        self.persistent.set(&key, &wgsl).await;
        self.compiled.insert(key.clone(), CompiledShader { module, wgsl });
        
        self.compiled.get(&key).unwrap()
    }
}
```

### Framebuffer Optimization

```rust
pub struct OptimizedFramebuffer {
    // Double buffering
    front: GPUTexture,
    back: GPUTexture,
    
    // Dirty region tracking
    dirty_regions: Vec<Rect>,
    
    // Previous frame for delta encoding
    previous_frame: Option<Vec<u8>>,
}

impl OptimizedFramebuffer {
    pub fn present(&mut self, queue: &GPUQueue) {
        if self.dirty_regions.is_empty() {
            return;  // No changes
        }
        
        // Only copy dirty regions
        for region in &self.dirty_regions {
            queue.copy_texture_to_texture(
                &self.back,
                &self.front,
                region.to_extent(),
            );
        }
        
        self.dirty_regions.clear();
        std::mem::swap(&mut self.front, &mut self.back);
    }
    
    pub fn mark_dirty(&mut self, region: Rect) {
        // Merge overlapping regions
        for existing in &mut self.dirty_regions {
            if existing.intersects(&region) {
                *existing = existing.union(&region);
                return;
            }
        }
        self.dirty_regions.push(region);
    }
}
```

---

## I/O Optimization

### Async Storage with Prefetching

```rust
pub struct PrefetchingStorage {
    backend: Box<dyn DiskBackend>,
    cache: LruCache<u64, Vec<u8>>,
    prefetch_queue: VecDeque<u64>,
    pending_reads: HashMap<u64, oneshot::Receiver<Vec<u8>>>,
}

impl PrefetchingStorage {
    pub async fn read_sector(&mut self, lba: u64) -> Vec<u8> {
        // Check cache
        if let Some(data) = self.cache.get(&lba) {
            // Prefetch next sectors
            self.queue_prefetch(lba + 1);
            self.queue_prefetch(lba + 2);
            return data.clone();
        }
        
        // Check pending reads
        if let Some(receiver) = self.pending_reads.remove(&lba) {
            return receiver.await.unwrap();
        }
        
        // Read from backend
        let data = self.backend.read_sectors(lba, 1).await;
        self.cache.put(lba, data.clone());
        
        // Prefetch
        self.queue_prefetch(lba + 1);
        self.queue_prefetch(lba + 2);
        
        data
    }
    
    fn queue_prefetch(&mut self, lba: u64) {
        if !self.cache.contains(&lba) && !self.pending_reads.contains_key(&lba) {
            let (tx, rx) = oneshot::channel();
            self.pending_reads.insert(lba, rx);
            
            // Spawn prefetch task
            let backend = self.backend.clone();
            spawn_local(async move {
                let data = backend.read_sectors(lba, 1).await;
                tx.send(data).ok();
            });
        }
    }
}
```

### Network Packet Coalescing

```rust
pub struct PacketCoalescer {
    pending_packets: Vec<Vec<u8>>,
    max_delay: Duration,
    last_flush: Instant,
}

impl PacketCoalescer {
    pub fn queue_packet(&mut self, packet: Vec<u8>) {
        self.pending_packets.push(packet);
        
        // Flush if too many packets or too much time
        if self.pending_packets.len() >= 10 || 
           self.last_flush.elapsed() > self.max_delay {
            self.flush();
        }
    }
    
    pub fn flush(&mut self) {
        if self.pending_packets.is_empty() {
            return;
        }
        
        // Combine packets into single WebSocket message
        let combined = self.encode_combined(&self.pending_packets);
        self.websocket.send(&combined);
        
        self.pending_packets.clear();
        self.last_flush = Instant::now();
    }
}
```

---

## Threading Optimization

### Work Distribution

```rust
pub struct WorkDistributor {
    cpu_worker: Worker,
    gpu_worker: Worker,
    io_worker: Worker,
    jit_worker: Worker,
}

impl WorkDistributor {
    pub fn distribute(&mut self) {
        // CPU emulation runs continuously on dedicated worker
        // GPU work submitted in batches
        // I/O operations are async with callbacks
        // JIT compilation happens in background
        
        // Balance work based on queue depths
        let cpu_queue_depth = self.cpu_worker.queue_depth();
        let gpu_queue_depth = self.gpu_worker.queue_depth();
        
        if gpu_queue_depth > cpu_queue_depth * 2 {
            // GPU bottleneck - reduce graphics quality
            self.reduce_graphics_quality();
        } else if cpu_queue_depth > 1000 {
            // CPU bottleneck - prioritize JIT compilation
            self.jit_worker.increase_priority();
        }
    }
}
```

### Lock-Free Communication

```rust
pub struct LockFreeQueue<T> {
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

impl<T> LockFreeQueue<T> {
    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let next_tail = (tail + 1) % self.capacity;
        
        if next_tail == self.head.load(Ordering::Acquire) {
            return Err(value);  // Queue full
        }
        
        unsafe {
            (*self.buffer[tail].get()).write(value);
        }
        
        self.tail.store(next_tail, Ordering::Release);
        Ok(())
    }
    
    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        
        if head == self.tail.load(Ordering::Acquire) {
            return None;  // Queue empty
        }
        
        let value = unsafe {
            (*self.buffer[head].get()).assume_init_read()
        };
        
        self.head.store((head + 1) % self.capacity, Ordering::Release);
        Some(value)
    }
}
```

---

## Profiling and Monitoring

### Nightly benchmark history

This repo includes a small, dependency-free benchmark harness in `bench/` and a scheduled workflow (`.github/workflows/perf-nightly.yml`) that:

- runs the benchmarks on a nightly schedule
- appends results into a versioned `history.json` time series (mean + variance indicators)
- publishes a static dashboard (trend graphs + commit links) as a workflow artifact and (optionally) to `gh-pages`

### Built-in Profiler

```rust
pub struct Profiler {
    samples: Vec<ProfileSample>,
    current_frame: FrameProfile,
    enabled: bool,
}

pub struct FrameProfile {
    start_time: Instant,
    cpu_time: Duration,
    gpu_time: Duration,
    io_time: Duration,
    jit_time: Duration,
    instructions_executed: u64,
    draw_calls: u32,
    memory_allocated: usize,
}

impl Profiler {
    pub fn begin_frame(&mut self) {
        self.current_frame = FrameProfile {
            start_time: Instant::now(),
            ..Default::default()
        };
    }
    
    pub fn end_frame(&mut self) {
        let frame_time = self.current_frame.start_time.elapsed();
        
        // Log slow frames
        if frame_time > Duration::from_millis(33) {
            log::warn!(
                "Slow frame: {:?} (CPU: {:?}, GPU: {:?}, IO: {:?})",
                frame_time,
                self.current_frame.cpu_time,
                self.current_frame.gpu_time,
                self.current_frame.io_time
            );
        }
        
        self.samples.push(ProfileSample {
            frame_time,
            profile: self.current_frame.clone(),
        });
    }
    
    pub fn get_stats(&self) -> ProfileStats {
        let recent = &self.samples[self.samples.len().saturating_sub(100)..];
        
        ProfileStats {
            avg_frame_time: recent.iter().map(|s| s.frame_time).sum::<Duration>() / recent.len() as u32,
            avg_fps: 1.0 / recent.iter().map(|s| s.frame_time.as_secs_f64()).sum::<f64>() * recent.len() as f64,
            avg_mips: recent.iter().map(|s| s.profile.instructions_executed).sum::<u64>() as f64 / 
                      recent.iter().map(|s| s.frame_time.as_secs_f64()).sum::<f64>() / 1_000_000.0,
        }
    }
}
```

### PF-006: JIT Optimization Analysis Metrics

To optimize a tiered JIT you need visibility into **(a)** compile cost (per tier and per pass), **(b)** cache behavior (is the JIT saving work or thrashing), and **(c)** stability (deopts/guard failures).

PF-006 defines a small, cheap-to-update telemetry surface that the rest of the performance tooling (PF-001 HUD + JSON export, PF-008 benchmarks) can consume.

#### Metrics to collect (exported totals + rolling)

The minimum required set is:

| Metric | Kind | Unit | Notes |
| --- | --- | --- | --- |
| `jit.tier1.compile_time_total` | duration | ms | Total time spent compiling Tier 1 (baseline) blocks this run |
| `jit.tier2.compile_time_total` | duration | ms | Total time spent compiling Tier 2 (optimizing) blocks this run |
| `jit.tier2.pass.<name>.time_total` | duration | ms | Total time spent in major Tier 2 passes (`const_fold`, `dce`, `regalloc`, …) |
| `jit.tier1.blocks_compiled_total` | counter | blocks | Count of compiled Tier 1 blocks/regions |
| `jit.tier2.blocks_compiled_total` | counter | blocks | Count of compiled Tier 2 blocks/regions |
| `jit.cache.capacity_bytes` | gauge | bytes | Configured code cache capacity |
| `jit.cache.used_bytes` | gauge | bytes | Current code cache usage (after inserts/evictions) |
| `jit.cache.lookup_hit_total` | counter | lookups | Cache lookup found an existing compiled block |
| `jit.cache.lookup_miss_total` | counter | lookups | Cache lookup did not find compiled code and triggered compilation |
| `jit.deopt_total` | counter | events | Number of deoptimizations (Tier 2 → Tier 1/0) |
| `jit.guard_fail_total` | counter | events | Number of guard failures (if guards are implemented) |

**Rolling metrics** (derived from the totals above) are required for HUD and for "what changed right now?" debugging:

| Rolling view | Derived from | Use |
| --- | --- | --- |
| `jit.cache_hit_rate_1s` | hits/misses deltas over 1s | Detect cache thrash or poor block formation |
| `jit.compile_ms_per_s_1s` | compile-time delta over 1s | Detect compile spikes and tune thresholds |
| `jit.blocks_compiled_per_s_1s` | block count delta over 1s | Detect pathological compilation churn |

The "1s" window is an example; PF-001 can choose another HUD window (e.g. last 60 frames) as long as it is consistent and documented in exports.

#### Data model + hot-path considerations

The JIT cache lookup is on the execution hot path, so updates must be **amortized and lock-free**:

- Use simple **counters** for events (`lookup_hit_total`, `lookup_miss_total`, `blocks_compiled_total`, `deopt_total`).
- Use **durations accumulated as integer nanoseconds** (or milliseconds) for compilation work; compilation runs off-thread, but we still want cheap aggregation.
- Prefer **per-thread counters** (or per-worker counters) that are merged periodically into a global view, to avoid high-frequency atomics on every lookup.

One workable shape (illustrative):

```rust
/// Collected by the JIT worker and periodically merged into the global telemetry sink.
pub struct JitMetricsTotals {
    // Cache behavior.
    pub cache_lookup_hit_total: u64,
    pub cache_lookup_miss_total: u64,

    // Tier distribution.
    pub tier1_blocks_compiled_total: u64,
    pub tier2_blocks_compiled_total: u64,

    // Compile time (accumulated).
    pub tier1_compile_ns_total: u64,
    pub tier2_compile_ns_total: u64,

    // Tier 2 pass breakdown (accumulated).
    pub tier2_pass_const_fold_ns_total: u64,
    pub tier2_pass_dce_ns_total: u64,
    pub tier2_pass_regalloc_ns_total: u64,

    // Deopts/guards.
    pub deopt_total: u64,
    pub guard_fail_total: u64,

    // Code cache footprint.
    pub code_cache_capacity_bytes: u64,
    pub code_cache_used_bytes: u64,
}
```

Compilation-phase timings should be captured at **pass boundaries** (not per instruction/IR node), so instrumentation stays negligible relative to actual compile work.

#### Reporting integration (PF-001 HUD + JSON export)

PF-001 should expose these metrics in two surfaces:

1) **HUD (small panel)**

Required fields:

- `JIT cache hit`: `hits / (hits + misses)` over rolling window
- `JIT blocks`: total blocks compiled (optionally `(+X/s)` from rolling window)
- `JIT compile`: compile ms/s over rolling window (sum Tier1+Tier2), with optional breakdown (`t1=… t2=…`)

2) **JSON export**

Include a `jit` section that contains both totals (since run start) and a rolling snapshot used by the HUD.

Example shape:

```json
{
  "jit": {
    "enabled": true,
    "totals": {
      "tier1": { "blocks_compiled": 1234, "compile_ms": 87.4 },
      "tier2": {
        "blocks_compiled": 56,
        "compile_ms": 45.1,
        "passes_ms": { "const_fold": 4.2, "dce": 6.7, "regalloc": 18.9 }
      },
      "cache": {
        "lookup_hit": 98765,
        "lookup_miss": 1234,
        "capacity_bytes": 268435456,
        "used_bytes": 104857600
      },
      "deopt": { "count": 0, "guard_fail": 0 }
    },
    "rolling": {
      "window_ms": 1000,
      "cache_hit_rate": 0.9875,
      "compile_ms_per_s": 3.4,
      "blocks_compiled_per_s": 12.0
    }
  }
}
```

The exact JSON schema is less important than: (a) stability, (b) explicit units, (c) totals + rolling included together.

#### Benchmark/regression linkage

PF-008 benchmarks should print a one-line summary of key `jit` metrics alongside throughput numbers so regressions can be attributed quickly (e.g. "MIPS dropped because Tier2 compile ms doubled").

See "Performance Benchmarks" in [Testing Strategy](./12-testing-strategy.md) for suggested output format.

#### Verification expectations

PF-006 should be validated with at least:

- A synthetic benchmark that executes enough distinct blocks to force compilation → **non-zero compile ms and block counts**.
- A run with JIT compilation disabled (interpreted-only mode) → **all `jit.*` counters and durations remain 0**, and the code path avoids doing cache lookups/instrumentation work.
### Standard performance metric definitions

To avoid drift between in-app HUD, exported perf summaries, and benchmark tooling, compute metrics using a single shared implementation (`packages/aero-stats`) and the following definitions:

- `avg_fps = frames / total_time_s` (equivalently `1000 / mean_frame_time_ms`)
- `median_fps = 1000 / p50_frame_time_ms`
- `1% low FPS = 1000 / p99_frame_time_ms`
- `0.1% low FPS = 1000 / p99.9_frame_time_ms`
- Variance/stdev/CoV are computed over frame times (ms) using Welford’s algorithm (population variance).

---

## Next Steps

- See [Browser APIs](./11-browser-apis.md) for platform-specific optimizations
- See [Testing Strategy](./12-testing-strategy.md) for performance testing
- See [Task Breakdown](./15-agent-task-breakdown.md) for optimization tasks
