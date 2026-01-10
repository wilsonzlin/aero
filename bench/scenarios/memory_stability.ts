import type { Page } from '@playwright/test';

type MemorySampleLike = Record<string, unknown> | null;

export type MemoryStabilityResult = {
  scenario: 'memory_stability';
  gc_available: boolean;
  start: MemorySampleLike;
  end: MemorySampleLike;
  deltas: Record<string, number | null>;
  thresholds: Record<string, number>;
  suspicious: Record<string, boolean>;
};

const bytesDelta = (start: MemorySampleLike, end: MemorySampleLike, key: string): number | null => {
  const a = start && typeof start === 'object' ? (start as Record<string, unknown>)[key] : null;
  const b = end && typeof end === 'object' ? (end as Record<string, unknown>)[key] : null;
  if (typeof a !== 'number' || typeof b !== 'number') return null;
  return b - a;
};

const maybeGc = async (page: Page): Promise<boolean> => {
  const available = await page.evaluate(() => typeof (window as unknown as { gc?: unknown }).gc === 'function');
  if (!available) return false;
  await page.evaluate(() => (window as unknown as { gc: () => void }).gc());
  // Give the browser a moment to finalize and update memory bookkeeping.
  await page.waitForTimeout(50);
  return true;
};

const sampleNow = async (page: Page, event: string): Promise<MemorySampleLike> => {
  return page.evaluate((evt) => {
    const aero = (window as Window).aero as unknown as { perf?: unknown } | undefined;
    const perf = aero && typeof aero === 'object' ? (aero as { perf?: any }).perf : undefined;
    const memory = perf?.memoryTelemetry;
    if (!memory?.sampleNow) return null;
    return memory.sampleNow(evt);
  }, event);
};

export const run = async (page: Page, baseUrl: string): Promise<MemoryStabilityResult> => {
  const normalizedBase = baseUrl.replace(/\/$/, '');
  const url = normalizedBase.endsWith('/web') ? `${normalizedBase}/` : `${normalizedBase}/web/`;
  await page.goto(url, { waitUntil: 'load' });

  // Memory telemetry is installed by the `/web/` entrypoint. If it's missing,
  // proceed in best-effort mode (returning null samples/deltas) rather than
  // failing the entire scenario.
  let telemetryAvailable = false;
  try {
    await page.waitForFunction(() => (window as any).aero?.perf?.memoryTelemetry?.sampleNow, {
      timeout: 5_000,
    });
    telemetryAvailable = true;
  } catch {
    telemetryAvailable = false;
  }

  const gc_available = await maybeGc(page);

  const start = telemetryAvailable ? await sampleNow(page, 'bench_start') : null;

  // Workload + idle: allocate memory and release it, then wait to observe if the heap stabilizes.
  await page.evaluate(async ({ rounds, jsAllocBytes, gpuAllocBytes, idleMs }) => {
    const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

    const aero = (window as Window).aero as unknown as { perf?: any } | undefined;
    const perf = aero?.perf;

    const gpuTracker = perf?.gpuTracker;
    const jitCacheTracker = perf?.jitCacheTracker;
    const shaderCacheTracker = perf?.shaderCacheTracker;

    for (let i = 0; i < rounds; i += 1) {
      // JS heap churn.
      const buf = new Uint8Array(jsAllocBytes);
      buf[0] = i & 0xff;

      // Optional: emulate WebGPU allocations and caches (best-effort).
      const gpuToken = gpuTracker?.trackBuffer ? gpuTracker.trackBuffer(gpuAllocBytes) : null;
      const jitToken = jitCacheTracker?.trackBytes ? jitCacheTracker.trackBytes(256 * 1024) : null;
      const shaderToken = shaderCacheTracker?.trackBytes ? shaderCacheTracker.trackBytes(128 * 1024) : null;

      await sleep(10);

      if (gpuToken != null && gpuTracker?.untrackBuffer) gpuTracker.untrackBuffer(gpuToken);
      if (jitToken != null && jitCacheTracker?.untrackBytes) jitCacheTracker.untrackBytes(jitToken);
      if (shaderToken != null && shaderCacheTracker?.untrackBytes) shaderCacheTracker.untrackBytes(shaderToken);
    }

    await sleep(idleMs);
  }, {
    rounds: 8,
    jsAllocBytes: 24 * 1024 * 1024,
    gpuAllocBytes: 8 * 1024 * 1024,
    idleMs: 2500,
  });

  await maybeGc(page);

  const end = telemetryAvailable ? await sampleNow(page, 'bench_end') : null;

  const deltas = {
    js_heap_used_bytes: bytesDelta(start, end, 'js_heap_used_bytes'),
    wasm_memory_bytes: bytesDelta(start, end, 'wasm_memory_bytes'),
    gpu_total_bytes: bytesDelta(start, end, 'gpu_total_bytes'),
    jit_code_cache_bytes: bytesDelta(start, end, 'jit_code_cache_bytes'),
    shader_cache_bytes: bytesDelta(start, end, 'shader_cache_bytes'),
  };

  // Thresholds are informational by default; memory is noisy across runs, especially in CI/headless.
  const thresholds = {
    js_heap_used_bytes: 8 * 1024 * 1024,
    wasm_memory_bytes: 0,
    gpu_total_bytes: 0,
    jit_code_cache_bytes: 0,
    shader_cache_bytes: 0,
  };

  const suspicious = {
    js_heap_used_bytes: deltas.js_heap_used_bytes != null && deltas.js_heap_used_bytes > thresholds.js_heap_used_bytes,
    wasm_memory_bytes: deltas.wasm_memory_bytes != null && deltas.wasm_memory_bytes > thresholds.wasm_memory_bytes,
    gpu_total_bytes: deltas.gpu_total_bytes != null && deltas.gpu_total_bytes > thresholds.gpu_total_bytes,
    jit_code_cache_bytes:
      deltas.jit_code_cache_bytes != null && deltas.jit_code_cache_bytes > thresholds.jit_code_cache_bytes,
    shader_cache_bytes:
      deltas.shader_cache_bytes != null && deltas.shader_cache_bytes > thresholds.shader_cache_bytes,
  };

  return {
    scenario: 'memory_stability',
    gc_available,
    start,
    end,
    deltas,
    thresholds,
    suspicious,
  };
};
