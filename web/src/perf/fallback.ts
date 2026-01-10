import type { PerfApi, PerfCaptureState, PerfHudSnapshot, PerfJitSnapshot, PerfTimeBreakdownMs } from './types';
import type { PerfExport } from './export';
import { ResponsivenessTracker, type ResponsivenessHudSnapshot } from './responsiveness';

import { FrameTimeStats } from '../../../packages/aero-stats/src/index.js';

import { ByteSizedCacheTracker, GpuAllocationTracker, MemoryTelemetry } from './memory';

export type InstallFallbackPerfOptions = {
  guestRamBytes?: number;
  wasmMemory?: WebAssembly.Memory;
  wasmMemoryMaxPages?: number;
  gpuTracker?: GpuAllocationTracker;
  jitCacheTracker?: ByteSizedCacheTracker;
  shaderCacheTracker?: ByteSizedCacheTracker;
};

const STATS_CAPACITY = 600;
const CAPTURE_CAPACITY = 12_000;
const P95 = 0.95;

const clampMs = (ms: number): number => {
  if (!Number.isFinite(ms)) return 0;
  if (ms < 0) return 0;
  if (ms > 250) return 250;
  return ms;
};

const isValidNumber = (value: number): boolean => Number.isFinite(value) && !Number.isNaN(value);

const JIT_DISABLED_SNAPSHOT: PerfJitSnapshot = {
  enabled: false,
  totals: {
    tier1: { blocksCompiled: 0, compileMs: 0 },
    tier2: { blocksCompiled: 0, compileMs: 0, passesMs: { constFold: 0, dce: 0, regalloc: 0 } },
    cache: { lookupHit: 0, lookupMiss: 0, capacityBytes: 0, usedBytes: 0 },
    deopt: { count: 0, guardFail: 0 },
  },
  rolling: { windowMs: 0, cacheHitRate: 0, compileMsPerSec: 0, blocksCompiledPerSec: 0 },
};

export class FallbackPerf implements PerfApi {
  readonly guestRamBytes?: number;
  readonly memoryTelemetry: MemoryTelemetry;
  readonly gpuTracker: GpuAllocationTracker;
  readonly jitCacheTracker: ByteSizedCacheTracker;
  readonly shaderCacheTracker: ByteSizedCacheTracker;

  private frameTimeScratch = new FrameTimeStats();

  private statsCursor = 0;
  private statsCount = 0;

  private frameTimesMs = new Float32Array(STATS_CAPACITY);

  private instructions = new Float64Array(STATS_CAPACITY);
  private instructionsSum = 0;
  private instructionsFrameTimeSumMs = 0;
  private instructionsCount = 0;

  private mips = new Float32Array(STATS_CAPACITY);
  private mipsCount = 0;

  private cpuMs = new Float32Array(STATS_CAPACITY);
  private cpuSumMs = 0;
  private cpuCount = 0;

  private gpuMs = new Float32Array(STATS_CAPACITY);
  private gpuSumMs = 0;
  private gpuCount = 0;

  private ioMs = new Float32Array(STATS_CAPACITY);
  private ioSumMs = 0;
  private ioCount = 0;

  private jitMs = new Float32Array(STATS_CAPACITY);
  private jitSumMs = 0;
  private jitCount = 0;

  private drawCalls = new Float32Array(STATS_CAPACITY);
  private drawCallsSum = 0;
  private drawCallsCount = 0;

  private ioBytes = new Float64Array(STATS_CAPACITY);
  private ioBytesSum = 0;
  private ioBytesFrameTimeSumMs = 0;
  private ioBytesCount = 0;

  private frameTimeSumMs = 0;

  private tmpMips = new Float32Array(STATS_CAPACITY);
  private hudActive = false;
  private raf: number | null = null;
  private lastRafNowMs = 0;

  private lastFrameTimeMs: number | undefined;
  private lastMips: number | undefined;

  private responsiveness = new ResponsivenessTracker();
  private responsivenessSnapshot: ResponsivenessHudSnapshot = {};

  private captureActive = false;
  private captureStartNowMs = 0;
  private captureStartUnixMs = 0;
  private captureDurationMs = 0;
  private captureDroppedRecords = 0;
  private captureRecords = 0;

  private captureTms = new Float32Array(CAPTURE_CAPACITY);
  private captureFrameTimesMs = new Float32Array(CAPTURE_CAPACITY);
  private captureInstructions = new Float64Array(CAPTURE_CAPACITY);
  private captureCpuMs = new Float32Array(CAPTURE_CAPACITY);
  private captureGpuMs = new Float32Array(CAPTURE_CAPACITY);
  private captureIoMs = new Float32Array(CAPTURE_CAPACITY);
  private captureJitMs = new Float32Array(CAPTURE_CAPACITY);
  private captureDrawCalls = new Float32Array(CAPTURE_CAPACITY);
  private captureIoBytes = new Float64Array(CAPTURE_CAPACITY);

  constructor(options: InstallFallbackPerfOptions = {}) {
    this.guestRamBytes = options.guestRamBytes;

    this.gpuTracker = options.gpuTracker ?? new GpuAllocationTracker();
    this.jitCacheTracker = options.jitCacheTracker ?? new ByteSizedCacheTracker();
    this.shaderCacheTracker = options.shaderCacheTracker ?? new ByteSizedCacheTracker();

    this.memoryTelemetry = new MemoryTelemetry({
      wasmMemory: options.wasmMemory,
      wasmMemoryMaxPages: options.wasmMemoryMaxPages ?? null,
      getGuestMemoryStats: options.guestRamBytes
        ? () => ({ configured_bytes: options.guestRamBytes!, committed_bytes: options.guestRamBytes! })
        : null,
      gpuTracker: this.gpuTracker,
      jitCacheTracker: this.jitCacheTracker,
      shaderCacheTracker: this.shaderCacheTracker,
      sampleHz: 1,
      maxSamples: 600,
    });
    this.memoryTelemetry.sampleNow('boot');

    this.instructions.fill(Number.NaN);
    this.mips.fill(Number.NaN);
    this.cpuMs.fill(Number.NaN);
    this.gpuMs.fill(Number.NaN);
    this.ioMs.fill(Number.NaN);
    this.jitMs.fill(Number.NaN);
    this.drawCalls.fill(Number.NaN);
    this.ioBytes.fill(Number.NaN);

    this.captureInstructions.fill(Number.NaN);
    this.captureCpuMs.fill(Number.NaN);
    this.captureGpuMs.fill(Number.NaN);
    this.captureIoMs.fill(Number.NaN);
    this.captureJitMs.fill(Number.NaN);
    this.captureDrawCalls.fill(Number.NaN);
    this.captureIoBytes.fill(Number.NaN);
  }

  recordFrame(
    frameTimeMs: number,
    instructions?: number,
    breakdownMs?: PerfTimeBreakdownMs,
    drawCalls?: number,
    ioBytes?: number,
  ): void {
    const ft = clampMs(frameTimeMs);

    if (this.statsCount === STATS_CAPACITY) {
      const idx = this.statsCursor;

      this.frameTimeSumMs -= this.frameTimesMs[idx] ?? 0;

      const oldInstructions = this.instructions[idx];
      if (isValidNumber(oldInstructions)) {
        this.instructionsSum -= oldInstructions;
        this.instructionsFrameTimeSumMs -= this.frameTimesMs[idx] ?? 0;
        this.instructionsCount -= 1;
      }

      const oldMips = this.mips[idx];
      if (isValidNumber(oldMips)) {
        this.mipsCount -= 1;
      }

      const oldCpuMs = this.cpuMs[idx];
      if (isValidNumber(oldCpuMs)) {
        this.cpuSumMs -= oldCpuMs;
        this.cpuCount -= 1;
      }

      const oldGpuMs = this.gpuMs[idx];
      if (isValidNumber(oldGpuMs)) {
        this.gpuSumMs -= oldGpuMs;
        this.gpuCount -= 1;
      }

      const oldIoMs = this.ioMs[idx];
      if (isValidNumber(oldIoMs)) {
        this.ioSumMs -= oldIoMs;
        this.ioCount -= 1;
      }

      const oldJitMs = this.jitMs[idx];
      if (isValidNumber(oldJitMs)) {
        this.jitSumMs -= oldJitMs;
        this.jitCount -= 1;
      }

      const oldDrawCalls = this.drawCalls[idx];
      if (isValidNumber(oldDrawCalls)) {
        this.drawCallsSum -= oldDrawCalls;
        this.drawCallsCount -= 1;
      }

      const oldIoBytes = this.ioBytes[idx];
      if (isValidNumber(oldIoBytes)) {
        this.ioBytesSum -= oldIoBytes;
        this.ioBytesFrameTimeSumMs -= this.frameTimesMs[idx] ?? 0;
        this.ioBytesCount -= 1;
      }
    } else {
      this.statsCount += 1;
    }

    const idx = this.statsCursor;
    this.statsCursor = (this.statsCursor + 1) % STATS_CAPACITY;

    this.frameTimesMs[idx] = ft;
    this.frameTimeSumMs += ft;

    if (instructions === undefined) {
      this.instructions[idx] = Number.NaN;
      this.mips[idx] = Number.NaN;
    } else {
      this.instructions[idx] = instructions;
      this.instructionsSum += instructions;
      this.instructionsFrameTimeSumMs += ft;
      this.instructionsCount += 1;

      if (ft > 0) {
        const mips = (instructions / (ft / 1000)) / 1_000_000;
        this.mips[idx] = mips;
        this.mipsCount += 1;
        this.lastMips = mips;
      } else {
        this.mips[idx] = Number.NaN;
        this.lastMips = undefined;
      }
    }

    if (instructions === undefined) {
      this.lastMips = undefined;
    }

    const cpu = breakdownMs?.cpu;
    if (cpu === undefined) {
      this.cpuMs[idx] = Number.NaN;
    } else {
      this.cpuMs[idx] = cpu;
      this.cpuSumMs += cpu;
      this.cpuCount += 1;
    }

    const gpu = breakdownMs?.gpu;
    if (gpu === undefined) {
      this.gpuMs[idx] = Number.NaN;
    } else {
      this.gpuMs[idx] = gpu;
      this.gpuSumMs += gpu;
      this.gpuCount += 1;
    }

    const io = breakdownMs?.io;
    if (io === undefined) {
      this.ioMs[idx] = Number.NaN;
    } else {
      this.ioMs[idx] = io;
      this.ioSumMs += io;
      this.ioCount += 1;
    }

    const jit = breakdownMs?.jit;
    if (jit === undefined) {
      this.jitMs[idx] = Number.NaN;
    } else {
      this.jitMs[idx] = jit;
      this.jitSumMs += jit;
      this.jitCount += 1;
    }

    if (drawCalls === undefined) {
      this.drawCalls[idx] = Number.NaN;
    } else {
      this.drawCalls[idx] = drawCalls;
      this.drawCallsSum += drawCalls;
      this.drawCallsCount += 1;
    }

    if (ioBytes === undefined) {
      this.ioBytes[idx] = Number.NaN;
    } else {
      this.ioBytes[idx] = ioBytes;
      this.ioBytesSum += ioBytes;
      this.ioBytesFrameTimeSumMs += ft;
      this.ioBytesCount += 1;
    }

    this.lastFrameTimeMs = ft;

    if (this.captureActive) {
      const nowMs = performance.now();
      const tMs = nowMs - this.captureStartNowMs;
      const capIdx = this.captureRecords;

      if (capIdx >= CAPTURE_CAPACITY) {
        this.captureDroppedRecords += 1;
      } else {
        this.captureTms[capIdx] = tMs;
        this.captureFrameTimesMs[capIdx] = ft;
        this.captureInstructions[capIdx] = instructions ?? Number.NaN;
        this.captureCpuMs[capIdx] = cpu ?? Number.NaN;
        this.captureGpuMs[capIdx] = gpu ?? Number.NaN;
        this.captureIoMs[capIdx] = io ?? Number.NaN;
        this.captureJitMs[capIdx] = jit ?? Number.NaN;
        this.captureDrawCalls[capIdx] = drawCalls ?? Number.NaN;
        this.captureIoBytes[capIdx] = ioBytes ?? Number.NaN;
        this.captureRecords += 1;
      }
    }
  }

  getHudSnapshot(out: PerfHudSnapshot): PerfHudSnapshot {
    out.nowMs = performance.now();

    if (this.statsCount === 0) {
      out.fpsAvg = undefined;
      out.fps1Low = undefined;
      out.frameTimeAvgMs = undefined;
      out.frameTimeP95Ms = undefined;
      out.mipsAvg = undefined;
      out.mipsP95 = undefined;
      out.lastFrameTimeMs = this.lastFrameTimeMs;
      out.lastMips = this.lastMips;
      out.breakdownAvgMs = undefined;
      out.drawCallsPerFrame = undefined;
      out.pipelineSwitchesPerFrame = undefined;
      out.ioBytesPerSec = undefined;
      out.gpuUploadBytesPerSec = undefined;
      out.gpuTimingSupported = undefined;
      out.gpuTimingEnabled = undefined;
    } else {
      this.frameTimeScratch.clear();
      for (let i = 0; i < this.statsCount; i += 1) {
        const srcIdx = (this.statsCursor + STATS_CAPACITY - this.statsCount + i) % STATS_CAPACITY;
        this.frameTimeScratch.pushFrameTimeMs(this.frameTimesMs[srcIdx] ?? 0);
      }

      const ft = this.frameTimeScratch.summary();
      out.frameTimeAvgMs = ft.meanFrameTimeMs;
      out.fpsAvg = ft.fpsAvg;
      out.frameTimeP95Ms = ft.frameTimeP95Ms;
      out.fps1Low = ft.fps1Low;

      if (this.instructionsCount > 0 && this.instructionsFrameTimeSumMs > 0) {
        out.mipsAvg = (this.instructionsSum / (this.instructionsFrameTimeSumMs / 1000)) / 1_000_000;
      } else {
        out.mipsAvg = undefined;
      }

      if (this.mipsCount > 0) {
        let count = 0;
        for (let i = 0; i < this.statsCount; i += 1) {
          const srcIdx = (this.statsCursor + STATS_CAPACITY - this.statsCount + i) % STATS_CAPACITY;
          const v = this.mips[srcIdx];
          if (isValidNumber(v)) {
            this.tmpMips[count] = v;
            count += 1;
          }
        }
        this.tmpMips.subarray(0, count).sort();
        const p95Idx = Math.floor((count - 1) * P95);
        const p95 = this.tmpMips[p95Idx];
        out.mipsP95 = isValidNumber(p95) ? p95 : undefined;
      } else {
        out.mipsP95 = undefined;
      }

      out.lastFrameTimeMs = this.lastFrameTimeMs;
      out.lastMips = this.lastMips;

      if (this.cpuCount > 0 || this.gpuCount > 0 || this.ioCount > 0 || this.jitCount > 0) {
        const breakdown: PerfTimeBreakdownMs = out.breakdownAvgMs ?? {};
        if (this.cpuCount > 0) breakdown.cpu = this.cpuSumMs / this.cpuCount;
        if (this.gpuCount > 0) breakdown.gpu = this.gpuSumMs / this.gpuCount;
        if (this.ioCount > 0) breakdown.io = this.ioSumMs / this.ioCount;
        if (this.jitCount > 0) breakdown.jit = this.jitSumMs / this.jitCount;
        out.breakdownAvgMs = breakdown;
      } else {
        out.breakdownAvgMs = undefined;
      }

      out.drawCallsPerFrame = this.drawCallsCount > 0 ? this.drawCallsSum / this.drawCallsCount : undefined;
      out.pipelineSwitchesPerFrame = undefined;
      if (this.ioBytesCount > 0 && this.ioBytesFrameTimeSumMs > 0) {
        out.ioBytesPerSec = this.ioBytesSum / (this.ioBytesFrameTimeSumMs / 1000);
      } else {
        out.ioBytesPerSec = undefined;
      }
      out.gpuUploadBytesPerSec = undefined;
      out.gpuTimingSupported = undefined;
      out.gpuTimingEnabled = undefined;
    }

    const memSample = this.memoryTelemetry.getLatestSample();
    out.hostJsHeapUsedBytes = memSample?.js_heap_used_bytes ?? undefined;
    out.hostJsHeapTotalBytes = memSample?.js_heap_total_bytes ?? undefined;
    out.hostJsHeapLimitBytes = memSample?.js_heap_limit_bytes ?? undefined;

    out.guestRamBytes = this.guestRamBytes;

    out.wasmMemoryBytes = memSample?.wasm_memory_bytes ?? undefined;
    out.wasmMemoryPages = memSample?.wasm_memory_pages ?? undefined;
    out.wasmMemoryMaxPages = memSample?.wasm_memory_max_pages ?? undefined;

    out.gpuEstimatedBytes = memSample?.gpu_total_bytes ?? undefined;
    out.jitCodeCacheBytes = memSample?.jit_code_cache_bytes ?? undefined;
    out.shaderCacheBytes = memSample?.shader_cache_bytes ?? undefined;

    out.jit = JIT_DISABLED_SNAPSHOT;

    out.peakHostJsHeapUsedBytes = this.memoryTelemetry.peaks.js_heap_used_bytes ?? undefined;
    out.peakWasmMemoryBytes = this.memoryTelemetry.peaks.wasm_memory_bytes ?? undefined;
    out.peakGpuEstimatedBytes = this.memoryTelemetry.peaks.gpu_total_bytes ?? undefined;
    out.responsiveness = this.responsiveness.getHudSnapshot(this.responsivenessSnapshot);

    const captureDurationMs = this.captureActive ? performance.now() - this.captureStartNowMs : this.captureDurationMs;
    const capture: PerfCaptureState = out.capture;
    capture.active = this.captureActive;
    capture.durationMs = captureDurationMs;
    capture.droppedRecords = this.captureDroppedRecords;
    capture.records = this.captureRecords;

    return out;
  }

  setHudActive(active: boolean): void {
    if (this.hudActive === active) return;
    this.hudActive = active;
    this.syncRaf();
  }

  noteInputCaptured(id: number, tCaptureMs: number = performance.now()): void {
    this.responsiveness.noteInputCaptured(id, tCaptureMs);
  }

  noteInputInjected(
    id: number,
    tInjectedMs: number = performance.now(),
    queueDepth?: number,
    queueOldestCaptureMs?: number | null,
  ): void {
    this.responsiveness.noteInputInjected(id, tInjectedMs, queueDepth, queueOldestCaptureMs);
  }

  noteInputConsumed(
    id: number,
    tConsumedMs: number = performance.now(),
    queueDepth?: number,
    queueOldestCaptureMs?: number | null,
  ): void {
    this.responsiveness.noteInputConsumed(id, tConsumedMs, queueDepth, queueOldestCaptureMs);
  }

  notePresent(tPresentMs: number = performance.now()): void {
    this.responsiveness.notePresent(tPresentMs);
  }

  captureStart(): void {
    if (this.captureActive) return;
    this.captureActive = true;
    this.captureStartNowMs = performance.now();
    this.captureStartUnixMs = Date.now();
    this.captureDurationMs = 0;
    this.captureDroppedRecords = 0;
    this.captureRecords = 0;
    this.memoryTelemetry.sampleNow('capture_start');
    this.syncRaf();
  }

  captureStop(): void {
    if (!this.captureActive) return;
    this.captureDurationMs = performance.now() - this.captureStartNowMs;
    this.captureActive = false;
    this.memoryTelemetry.sampleNow('capture_stop');
    this.syncRaf();
  }

  captureReset(): void {
    this.memoryTelemetry.reset();
    this.memoryTelemetry.sampleNow('capture_reset');
    if (this.captureActive) {
      this.captureStartNowMs = performance.now();
      this.captureStartUnixMs = Date.now();
    }
    this.captureDurationMs = 0;
    this.captureDroppedRecords = 0;
    this.captureRecords = 0;
    this.responsiveness.reset();
  }

  export(): PerfExport {
    const records = new Array(this.captureRecords);
    for (let i = 0; i < this.captureRecords; i += 1) {
      const instructions = this.captureInstructions[i];
      const cpuMs = this.captureCpuMs[i];
      const gpuMs = this.captureGpuMs[i];
      const ioMs = this.captureIoMs[i];
      const jitMs = this.captureJitMs[i];
      const drawCalls = this.captureDrawCalls[i];
      const ioBytes = this.captureIoBytes[i];

      records[i] = {
        tMs: this.captureTms[i],
        frameTimeMs: this.captureFrameTimesMs[i],
        instructions: isValidNumber(instructions) ? instructions : null,
        cpuMs: isValidNumber(cpuMs) ? cpuMs : null,
        gpuMs: isValidNumber(gpuMs) ? gpuMs : null,
        ioMs: isValidNumber(ioMs) ? ioMs : null,
        jitMs: isValidNumber(jitMs) ? jitMs : null,
        drawCalls: isValidNumber(drawCalls) ? drawCalls : null,
        ioBytes: isValidNumber(ioBytes) ? ioBytes : null,
      };
    }

    const frameTimeStats = new FrameTimeStats();
    let instructionsSum = 0;
    let instructionsFrameTimeSumMs = 0;
    for (let i = 0; i < this.captureRecords; i += 1) {
      const ft = this.captureFrameTimesMs[i];
      frameTimeStats.pushFrameTimeMs(ft);

      const inst = this.captureInstructions[i];
      if (isValidNumber(inst)) {
        instructionsSum += inst;
        instructionsFrameTimeSumMs += ft;
      }
    }

    const mipsAvg =
      instructionsFrameTimeSumMs > 0 ? (instructionsSum / (instructionsFrameTimeSumMs / 1000)) / 1_000_000 : null;

    return {
      kind: 'aero-perf-capture',
      version: 1,
      startUnixMs: this.captureStartUnixMs || null,
      durationMs: this.captureActive ? performance.now() - this.captureStartNowMs : this.captureDurationMs,
      droppedRecords: this.captureDroppedRecords,
      guestRamBytes: this.guestRamBytes ?? null,
      jit: JIT_DISABLED_SNAPSHOT,
      memory: this.memoryTelemetry.export(),
      summary: {
        frameTime: frameTimeStats.summary(),
        mipsAvg,
      },
      frameTime: {
        summary: frameTimeStats.summary(),
        stats: frameTimeStats.toJSON(),
      },
      responsiveness: this.responsiveness.export(),
      records,
    };
  }

  private syncRaf(): void {
    const shouldRun = this.hudActive || this.captureActive;
    this.responsiveness.setActive(shouldRun);
    if (shouldRun) {
      this.memoryTelemetry.start();
    } else {
      this.memoryTelemetry.stop();
    }
    if (shouldRun) {
      if (this.raf !== null) return;
      this.lastRafNowMs = performance.now();
      const tick = (nowMs: number) => {
        if (this.raf === null) return;
        this.raf = requestAnimationFrame(tick);
        const frameTimeMs = nowMs - this.lastRafNowMs;
        this.lastRafNowMs = nowMs;
        this.responsiveness.notePresent(nowMs);
        this.recordFrame(frameTimeMs);
      };
      this.raf = requestAnimationFrame(tick);
    } else {
      if (this.raf === null) return;
      cancelAnimationFrame(this.raf);
      this.raf = null;
    }
  }
}

export const installFallbackPerf = (options: InstallFallbackPerfOptions = {}): FallbackPerf => {
  return new FallbackPerf(options);
};
