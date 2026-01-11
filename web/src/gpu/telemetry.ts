/**
 * GPU/perf telemetry primitives.
 *
 * This module is intentionally dependency-free so it can run in:
 * - the main thread (for overlays)
 * - a GPU worker (for low-overhead collection + periodic snapshot/postMessage)
 * - Playwright benchmark pages (for CI regression tracking)
 *
 * The API is "push" based: the render/translation code records events, and
 * consumers periodically call `snapshot()` to obtain a structured-cloneable
 * object suitable for JSON serialization.
 */

export type HistogramStats = {
  count: number;
  min: number | null;
  max: number | null;
  mean: number | null;
  p50: number | null;
  p95: number | null;
  p99: number | null;
};

export type HistogramSnapshot = {
  bucketSize: number;
  min: number;
  max: number;
  underflow: number;
  overflow: number;
  buckets: number[];
  stats: HistogramStats;
};

type HistogramConfig = { bucketSize: number; min: number; max: number };

class FixedHistogram {
  bucketSize = 0;
  min = 0;
  max = 1;

  _buckets = new Uint32Array(0);

  _underflow = 0;
  _overflow = 0;
  _count = 0;
  _sum = 0;
  _minSeen = Infinity;
  _maxSeen = -Infinity;

  constructor(cfg: HistogramConfig) {
    if (!(cfg.bucketSize > 0)) {
      throw new Error("Histogram bucketSize must be > 0");
    }
    if (!(cfg.max > cfg.min)) {
      throw new Error("Histogram max must be > min");
    }

    this.bucketSize = cfg.bucketSize;
    this.min = cfg.min;
    this.max = cfg.max;

    const bucketCount = Math.ceil((this.max - this.min) / this.bucketSize);
    this._buckets = new Uint32Array(bucketCount);

    this.reset();
  }

  reset() {
    this._buckets.fill(0);
    this._underflow = 0;
    this._overflow = 0;
    this._count = 0;
    this._sum = 0;
    this._minSeen = Infinity;
    this._maxSeen = -Infinity;
  }

  add(value: number): void {
    if (!Number.isFinite(value)) {
      return;
    }
    this._count += 1;
    this._sum += value;
    if (value < this._minSeen) this._minSeen = value;
    if (value > this._maxSeen) this._maxSeen = value;

    if (value < this.min) {
      this._underflow += 1;
      return;
    }
    if (value >= this.max) {
      this._overflow += 1;
      return;
    }
    const idx = Math.floor((value - this.min) / this.bucketSize);
    this._buckets[idx] += 1;
  }

  percentile(p: number): number | null {
    if (!(p >= 0 && p <= 1)) {
      throw new Error("percentile p must be in [0, 1]");
    }
    if (this._count === 0) {
      return null;
    }
    const target = Math.ceil(this._count * p);
    let cumulative = this._underflow;
    if (cumulative >= target) {
      return this.min;
    }
    for (let i = 0; i < this._buckets.length; i += 1) {
      cumulative += this._buckets[i];
      if (cumulative >= target) {
        // Return the mid-point of the bucket. This is an approximation but is
        // stable and low overhead.
        return this.min + (i + 0.5) * this.bucketSize;
      }
    }
    return this.max;
  }

  stats(): HistogramStats {
    if (this._count === 0) {
      return {
        count: 0,
        min: null,
        max: null,
        mean: null,
        p50: null,
        p95: null,
        p99: null,
      };
    }
    return {
      count: this._count,
      min: this._minSeen,
      max: this._maxSeen,
      mean: this._sum / this._count,
      p50: this.percentile(0.5),
      p95: this.percentile(0.95),
      p99: this.percentile(0.99),
    };
  }

  snapshot(): HistogramSnapshot {
    return {
      bucketSize: this.bucketSize,
      min: this.min,
      max: this.max,
      underflow: this._underflow,
      overflow: this._overflow,
      buckets: Array.from(this._buckets),
      stats: this.stats(),
    };
  }

  get count() {
    return this._count;
  }

  get sum() {
    return this._sum;
  }
}

export type GpuTelemetrySnapshot = {
  wallTimeTotalMs: number | null;
  frameTimeMs: HistogramSnapshot;
  presentLatencyMs: HistogramSnapshot;
  droppedFrames: number;
  shaderTranslationMs: HistogramSnapshot;
  shaderCompilationMs: HistogramSnapshot;
  pipelineCache: {
    hits: number;
    misses: number;
    hitRate: number | null;
    entries: number | null;
    sizeBytes: number | null;
  };
  textureUpload: {
    bytesTotal: number;
    bytesPerFrame: HistogramSnapshot;
    bandwidthBytesPerSecAvg: number | null;
  };
};

export type GpuTelemetryOptions = {
  frameBudgetMs?: number;
  frameTimeHistogram?: HistogramConfig;
  presentLatencyHistogram?: HistogramConfig;
  shaderTranslationHistogram?: HistogramConfig;
  shaderCompilationHistogram?: HistogramConfig;
  textureUploadBytesPerFrameHistogram?: HistogramConfig;
};

/**
 * Telemetry collector.
 *
 * The collector itself does not schedule any work; callers are expected to
 * invoke `beginFrame()`/`endFrame()` and record events at the appropriate
 * points in their pipeline.
 */
export class GpuTelemetry {
  frameBudgetMs = 1000 / 60;

  _frameStartMs: number | null = null;
  _firstFrameStartMs: number | null = null;
  _lastFrameEndMs: number | null = null;
  _currentFrameTextureUploadBytes = 0;

  frameTimeMs = new FixedHistogram({ bucketSize: 0.5, min: 0, max: 100 });
  presentLatencyMs = new FixedHistogram({ bucketSize: 0.25, min: 0, max: 50 });
  shaderTranslationMs = new FixedHistogram({ bucketSize: 0.1, min: 0, max: 50 });
  shaderCompilationMs = new FixedHistogram({ bucketSize: 0.1, min: 0, max: 50 });
  textureUploadBytesPerFrame = new FixedHistogram({
    bucketSize: 256 * 1024,
    min: 0,
    max: 64 * 1024 * 1024,
  });

  droppedFrames = 0;

  pipelineCacheHits = 0;
  pipelineCacheMisses = 0;
  pipelineCacheEntries: number | null = null;
  pipelineCacheSizeBytes: number | null = null;

  textureUploadBytesTotal = 0;

  constructor(opts: GpuTelemetryOptions = {}) {
    this.frameBudgetMs = opts.frameBudgetMs ?? this.frameBudgetMs;

    if (opts.frameTimeHistogram) {
      this.frameTimeMs = new FixedHistogram(opts.frameTimeHistogram);
    }
    if (opts.presentLatencyHistogram) {
      this.presentLatencyMs = new FixedHistogram(opts.presentLatencyHistogram);
    }
    if (opts.shaderTranslationHistogram) {
      this.shaderTranslationMs = new FixedHistogram(opts.shaderTranslationHistogram);
    }
    if (opts.shaderCompilationHistogram) {
      this.shaderCompilationMs = new FixedHistogram(opts.shaderCompilationHistogram);
    }
    if (opts.textureUploadBytesPerFrameHistogram) {
      this.textureUploadBytesPerFrame = new FixedHistogram(opts.textureUploadBytesPerFrameHistogram);
    }
  }

  reset() {
    this._frameStartMs = null;
    this._firstFrameStartMs = null;
    this._lastFrameEndMs = null;
    this._currentFrameTextureUploadBytes = 0;

    this.frameTimeMs.reset();
    this.presentLatencyMs.reset();
    this.shaderTranslationMs.reset();
    this.shaderCompilationMs.reset();
    this.textureUploadBytesPerFrame.reset();

    this.droppedFrames = 0;

    this.pipelineCacheHits = 0;
    this.pipelineCacheMisses = 0;
    this.pipelineCacheEntries = null;
    this.pipelineCacheSizeBytes = null;

    this.textureUploadBytesTotal = 0;
  }

  beginFrame(nowMs: number = performance.now()): void {
    if (this._firstFrameStartMs == null) {
      this._firstFrameStartMs = nowMs;
    }
    this._frameStartMs = nowMs;
    this._currentFrameTextureUploadBytes = 0;
  }

  endFrame(nowMs: number = performance.now()): void {
    if (this._frameStartMs == null) {
      return;
    }
    const dt = nowMs - this._frameStartMs;
    this.frameTimeMs.add(dt);

    // Dropped frame estimation: use the time between successive endFrame calls
    // as a proxy for present cadence. If the cadence exceeds the budget by
    // multiple intervals, count the missing intervals as dropped.
    if (this._lastFrameEndMs != null) {
      const interval = nowMs - this._lastFrameEndMs;
      const expected = this.frameBudgetMs;
      const missed = Math.max(0, Math.round(interval / expected) - 1);
      this.droppedFrames += missed;
    }
    this._lastFrameEndMs = nowMs;

    this.textureUploadBytesTotal += this._currentFrameTextureUploadBytes;
    this.textureUploadBytesPerFrame.add(this._currentFrameTextureUploadBytes);

    this._frameStartMs = null;
  }

  recordPresentLatencyMs(latencyMs: number): void {
    this.presentLatencyMs.add(latencyMs);
  }

  recordShaderTranslationMs(ms: number): void {
    this.shaderTranslationMs.add(ms);
  }

  recordShaderCompilationMs(ms: number): void {
    this.shaderCompilationMs.add(ms);
  }

  recordTextureUploadBytes(bytes: number): void {
    if (!Number.isFinite(bytes) || bytes <= 0) {
      return;
    }
    this._currentFrameTextureUploadBytes += bytes;
  }

  recordDroppedFrames(count: number = 1): void {
    if (!Number.isFinite(count) || count <= 0) {
      return;
    }
    this.droppedFrames += count;
  }

  recordPipelineCacheHit() {
    this.pipelineCacheHits += 1;
  }

  recordPipelineCacheMiss() {
    this.pipelineCacheMisses += 1;
  }

  setPipelineCacheStats(stats: { entries?: number | null; sizeBytes?: number | null }): void {
    if ("entries" in stats) {
      this.pipelineCacheEntries = stats.entries ?? null;
    }
    if ("sizeBytes" in stats) {
      this.pipelineCacheSizeBytes = stats.sizeBytes ?? null;
    }
  }

  snapshot(): GpuTelemetrySnapshot {
    const cacheLookups = this.pipelineCacheHits + this.pipelineCacheMisses;
    const hitRate = cacheLookups === 0 ? null : this.pipelineCacheHits / cacheLookups;

    const wallTimeTotalMs =
      this._firstFrameStartMs != null && this._lastFrameEndMs != null
        ? this._lastFrameEndMs - this._firstFrameStartMs
        : null;
    const bandwidthBytesPerSecAvg =
      wallTimeTotalMs != null && wallTimeTotalMs > 0
        ? this.textureUploadBytesTotal / (wallTimeTotalMs / 1000)
        : null;

    return {
      wallTimeTotalMs,
      frameTimeMs: this.frameTimeMs.snapshot(),
      presentLatencyMs: this.presentLatencyMs.snapshot(),
      droppedFrames: this.droppedFrames,
      shaderTranslationMs: this.shaderTranslationMs.snapshot(),
      shaderCompilationMs: this.shaderCompilationMs.snapshot(),
      pipelineCache: {
        hits: this.pipelineCacheHits,
        misses: this.pipelineCacheMisses,
        hitRate,
        entries: this.pipelineCacheEntries,
        sizeBytes: this.pipelineCacheSizeBytes,
      },
      textureUpload: {
        bytesTotal: this.textureUploadBytesTotal,
        bytesPerFrame: this.textureUploadBytesPerFrame.snapshot(),
        bandwidthBytesPerSecAvg,
      },
    };
  }
}

// Optional global registration to make it easy for benchmark pages and ad-hoc
// debugging to access the implementation without a bundler.
if (typeof globalThis !== "undefined") {
  const g = globalThis as any;
  if (!g.AeroGpuTelemetry) {
    g.AeroGpuTelemetry = { GpuTelemetry };
  } else if (!g.AeroGpuTelemetry.GpuTelemetry) {
    g.AeroGpuTelemetry.GpuTelemetry = GpuTelemetry;
  }
}
