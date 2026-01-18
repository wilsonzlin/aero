import { unrefBestEffort } from '../unrefSafe';

export const WASM_PAGE_SIZE_BYTES = 64 * 1024;

type PerformanceMemoryLike = {
  usedJSHeapSize: number;
  totalJSHeapSize: number;
  jsHeapSizeLimit: number;
};

const perfMemory = (): PerformanceMemoryLike | undefined => {
  return (performance as unknown as { memory?: PerformanceMemoryLike }).memory;
};

const nowMs = (): number => {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
};

export type GuestMemoryStats = {
  configured_bytes: number;
  committed_bytes: number;
};

export type MemoryCapabilities = {
  wasm_memory: boolean;
  js_heap: boolean;
  gpu_estimate: boolean;
  guest_memory: boolean;
  jit_code_cache: boolean;
  shader_cache: boolean;
};

export type MemorySample = {
  t_ms: number;
  event: string | null;

  wasm_memory_bytes: number | null;
  wasm_memory_pages: number | null;
  wasm_memory_max_pages: number | null;

  guest_configured_bytes: number | null;
  guest_committed_bytes: number | null;

  js_heap_used_bytes: number | null;
  js_heap_total_bytes: number | null;
  js_heap_limit_bytes: number | null;

  gpu_buffer_bytes: number | null;
  gpu_texture_bytes: number | null;
  gpu_total_bytes: number | null;
  gpu_buffer_count: number | null;
  gpu_texture_count: number | null;

  jit_code_cache_bytes: number | null;
  jit_wasm_modules: number | null;

  shader_cache_bytes: number | null;
  shader_modules: number | null;
};

const METRIC_KEYS = [
  'wasm_memory_bytes',
  'wasm_memory_pages',
  'wasm_memory_max_pages',
  'guest_configured_bytes',
  'guest_committed_bytes',
  'js_heap_used_bytes',
  'js_heap_total_bytes',
  'js_heap_limit_bytes',
  'gpu_buffer_bytes',
  'gpu_texture_bytes',
  'gpu_total_bytes',
  'gpu_buffer_count',
  'gpu_texture_count',
  'jit_code_cache_bytes',
  'jit_wasm_modules',
  'shader_cache_bytes',
  'shader_modules',
] as const;

type MetricKey = (typeof METRIC_KEYS)[number];

export type MemoryPeaks = Record<MetricKey, number | null>;

export type MemorySummary = {
  sample_count: number;
  duration_ms: number;
  start: MemorySample | null;
  end: MemorySample | null;
  deltas: MemoryPeaks;
};

export type MemoryTelemetryExport = {
  capabilities: MemoryCapabilities;
  sample_hz: number;
  samples: MemorySample[];
  peaks: MemoryPeaks;
  summary: MemorySummary;
};

export class GpuAllocationTracker {
  private nextId = 1;
  private buffers = new Map<number, number>();
  private textures = new Map<number, number>();

  trackBuffer(bytes: number): number {
    const id = this.nextId++;
    this.buffers.set(id, bytes);
    return id;
  }

  untrackBuffer(id: number): void {
    this.buffers.delete(id);
  }

  trackTexture(bytes: number): number {
    const id = this.nextId++;
    this.textures.set(id, bytes);
    return id;
  }

  untrackTexture(id: number): void {
    this.textures.delete(id);
  }

  getStats(): Pick<
    MemorySample,
    'gpu_buffer_bytes' | 'gpu_texture_bytes' | 'gpu_total_bytes' | 'gpu_buffer_count' | 'gpu_texture_count'
  > {
    let bufferBytes = 0;
    for (const v of this.buffers.values()) bufferBytes += v;
    let textureBytes = 0;
    for (const v of this.textures.values()) textureBytes += v;

    return {
      gpu_buffer_bytes: bufferBytes,
      gpu_texture_bytes: textureBytes,
      gpu_total_bytes: bufferBytes + textureBytes,
      gpu_buffer_count: this.buffers.size,
      gpu_texture_count: this.textures.size,
    };
  }
}

export class ByteSizedCacheTracker {
  private nextId = 1;
  private entries = new Map<number, number>();

  trackBytes(bytes: number): number {
    const id = this.nextId++;
    this.entries.set(id, bytes);
    return id;
  }

  untrackBytes(id: number): void {
    this.entries.delete(id);
  }

  getTotalBytes(): number {
    let total = 0;
    for (const v of this.entries.values()) total += v;
    return total;
  }

  getCount(): number {
    return this.entries.size;
  }
}

export type MemoryTelemetryOptions = {
  wasmMemory?: WebAssembly.Memory | null;
  wasmMemoryMaxPages?: number | null;
  getGuestMemoryStats?: (() => GuestMemoryStats) | null;
  gpuTracker?: GpuAllocationTracker | null;
  jitCacheTracker?: ByteSizedCacheTracker | null;
  shaderCacheTracker?: ByteSizedCacheTracker | null;
  sampleHz?: number;
  maxSamples?: number;
};

export class MemoryTelemetry {
  readonly capabilities: MemoryCapabilities;
  readonly peaks: MemoryPeaks;
  readonly samples: MemorySample[] = [];

  private readonly wasmMemory: WebAssembly.Memory | null;
  private readonly wasmMemoryMaxPages: number | null;
  private readonly getGuestMemoryStats: (() => GuestMemoryStats) | null;
  private readonly gpuTracker: GpuAllocationTracker | null;
  private readonly jitCacheTracker: ByteSizedCacheTracker | null;
  private readonly shaderCacheTracker: ByteSizedCacheTracker | null;
  private readonly listeners = new Set<(sample: MemorySample) => void>();
  private timer: ReturnType<typeof setInterval> | null = null;

  readonly sampleHz: number;
  readonly maxSamples: number;

  constructor(options: MemoryTelemetryOptions = {}) {
    this.wasmMemory = options.wasmMemory ?? null;
    this.wasmMemoryMaxPages = options.wasmMemoryMaxPages ?? null;
    this.getGuestMemoryStats = options.getGuestMemoryStats ?? null;
    this.gpuTracker = options.gpuTracker ?? null;
    this.jitCacheTracker = options.jitCacheTracker ?? null;
    this.shaderCacheTracker = options.shaderCacheTracker ?? null;

    this.sampleHz = options.sampleHz ?? 1;
    this.maxSamples = options.maxSamples ?? 300;

    const mem = perfMemory();
    this.capabilities = {
      wasm_memory: !!this.wasmMemory,
      js_heap: !!mem,
      gpu_estimate: !!this.gpuTracker,
      guest_memory: typeof this.getGuestMemoryStats === 'function',
      jit_code_cache: !!this.jitCacheTracker,
      shader_cache: !!this.shaderCacheTracker,
    };

    this.peaks = Object.fromEntries(METRIC_KEYS.map((k) => [k, null])) as MemoryPeaks;
  }

  onSample(cb: (sample: MemorySample) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  start(): void {
    if (this.timer !== null) return;
    const intervalMs = Math.max(1, Math.floor(1000 / this.sampleHz));
    this.timer = setInterval(() => this.sampleNow(null), intervalMs);
    unrefBestEffort(this.timer);
  }

  stop(): void {
    if (this.timer === null) return;
    clearInterval(this.timer);
    this.timer = null;
  }

  reset(): void {
    this.samples.length = 0;
    for (const key of METRIC_KEYS) this.peaks[key] = null;
  }

  getLatestSample(): MemorySample | undefined {
    return this.samples[this.samples.length - 1];
  }

  getRecentSamples(limit: number): MemorySample[] {
    return this.samples.slice(Math.max(0, this.samples.length - limit));
  }

  sampleNow(event: string | null): MemorySample {
    const sample = this.collectSample(event);
    this.samples.push(sample);
    if (this.samples.length > this.maxSamples) this.samples.shift();
    this.updatePeaks(sample);
    for (const cb of this.listeners) cb(sample);
    return sample;
  }

  export(): MemoryTelemetryExport {
    return {
      capabilities: this.capabilities,
      sample_hz: this.sampleHz,
      samples: this.samples.slice(),
      peaks: { ...this.peaks },
      summary: this.computeSummary(),
    };
  }

  private collectSample(event: string | null): MemorySample {
    const t_ms = nowMs();

    let wasm_memory_bytes: number | null = null;
    let wasm_memory_pages: number | null = null;
    let wasm_memory_max_pages: number | null = null;
    if (this.wasmMemory) {
      wasm_memory_bytes = this.wasmMemory.buffer.byteLength;
      wasm_memory_pages = wasm_memory_bytes / WASM_PAGE_SIZE_BYTES;
      wasm_memory_max_pages = this.wasmMemoryMaxPages;
    }

    let guest_configured_bytes: number | null = null;
    let guest_committed_bytes: number | null = null;
    if (this.getGuestMemoryStats) {
      try {
        const stats = this.getGuestMemoryStats();
        guest_configured_bytes = stats.configured_bytes;
        guest_committed_bytes = stats.committed_bytes;
      } catch {
        guest_configured_bytes = null;
        guest_committed_bytes = null;
      }
    }

    let js_heap_used_bytes: number | null = null;
    let js_heap_total_bytes: number | null = null;
    let js_heap_limit_bytes: number | null = null;
    const mem = perfMemory();
    if (mem) {
      js_heap_used_bytes = mem.usedJSHeapSize;
      js_heap_total_bytes = mem.totalJSHeapSize;
      js_heap_limit_bytes = mem.jsHeapSizeLimit;
    }

    const gpu = this.gpuTracker?.getStats() ?? {
      gpu_buffer_bytes: null,
      gpu_texture_bytes: null,
      gpu_total_bytes: null,
      gpu_buffer_count: null,
      gpu_texture_count: null,
    };

    const jit_code_cache_bytes = this.jitCacheTracker?.getTotalBytes() ?? null;
    const jit_wasm_modules = this.jitCacheTracker?.getCount() ?? null;
    const shader_cache_bytes = this.shaderCacheTracker?.getTotalBytes() ?? null;
    const shader_modules = this.shaderCacheTracker?.getCount() ?? null;

    return {
      t_ms,
      event,

      wasm_memory_bytes,
      wasm_memory_pages,
      wasm_memory_max_pages,

      guest_configured_bytes,
      guest_committed_bytes,

      js_heap_used_bytes,
      js_heap_total_bytes,
      js_heap_limit_bytes,

      ...gpu,

      jit_code_cache_bytes,
      jit_wasm_modules,

      shader_cache_bytes,
      shader_modules,
    };
  }

  private updatePeaks(sample: MemorySample): void {
    for (const key of METRIC_KEYS) {
      const v = sample[key];
      if (typeof v !== 'number') continue;
      const current = this.peaks[key];
      if (current === null || v > current) {
        this.peaks[key] = v;
      }
    }
  }

  private computeSummary(): MemorySummary {
    const start = this.samples[0] ?? null;
    const end = this.getLatestSample() ?? null;
    const deltas = Object.fromEntries(METRIC_KEYS.map((k) => [k, null])) as MemoryPeaks;

    if (start && end) {
      for (const key of METRIC_KEYS) {
        const a = start[key];
        const b = end[key];
        if (typeof a === 'number' && typeof b === 'number') {
          deltas[key] = b - a;
        }
      }
    }

    return {
      sample_count: this.samples.length,
      duration_ms: start && end ? end.t_ms - start.t_ms : 0,
      start,
      end,
      deltas,
    };
  }
}
