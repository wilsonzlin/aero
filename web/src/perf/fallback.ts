import type { PerfApi, PerfCaptureState, PerfHudSnapshot, PerfTimeBreakdownMs } from './types';

export type InstallFallbackPerfOptions = {
  guestRamBytes?: number;
};

const STATS_CAPACITY = 600;
const CAPTURE_CAPACITY = 12_000;

const P95 = 0.95;
const P99 = 0.99;

type PerformanceMemoryLike = {
  usedJSHeapSize: number;
  jsHeapSizeLimit: number;
};

const perfMemory = (): PerformanceMemoryLike | undefined => {
  return (performance as unknown as { memory?: PerformanceMemoryLike }).memory;
};

const clampMs = (ms: number): number => {
  if (!Number.isFinite(ms)) return 0;
  if (ms < 0) return 0;
  if (ms > 250) return 250;
  return ms;
};

const isValidNumber = (value: number): boolean => Number.isFinite(value) && !Number.isNaN(value);

export class FallbackPerf implements PerfApi {
  readonly guestRamBytes?: number;

  private statsCursor = 0;
  private statsCount = 0;

  private frameTimesMs = new Float32Array(STATS_CAPACITY);

  private instructions = new Float64Array(STATS_CAPACITY);
  private instructionsSum = 0;
  private instructionsFrameTimeSumMs = 0;
  private instructionsCount = 0;

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

  private tmpFrameTimes = new Float32Array(STATS_CAPACITY);

  private hudActive = false;
  private raf: number | null = null;
  private lastRafNowMs = 0;

  private lastFrameTimeMs: number | undefined;
  private lastMips: number | undefined;

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

    this.instructions.fill(Number.NaN);
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
    } else {
      this.instructions[idx] = instructions;
      this.instructionsSum += instructions;
      this.instructionsFrameTimeSumMs += ft;
      this.instructionsCount += 1;

      if (ft > 0) {
        this.lastMips = (instructions / (ft / 1000)) / 1_000_000;
      } else {
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
      out.lastFrameTimeMs = this.lastFrameTimeMs;
      out.lastMips = this.lastMips;
      out.breakdownAvgMs = undefined;
      out.drawCallsPerFrame = undefined;
      out.ioBytesPerSec = undefined;
    } else {
      const avgFrameTimeMs = this.frameTimeSumMs / this.statsCount;
      out.frameTimeAvgMs = avgFrameTimeMs;
      out.fpsAvg = avgFrameTimeMs > 0 ? 1000 / avgFrameTimeMs : undefined;

      for (let i = 0; i < this.statsCount; i += 1) {
        const srcIdx = (this.statsCursor + STATS_CAPACITY - this.statsCount + i) % STATS_CAPACITY;
        this.tmpFrameTimes[i] = this.frameTimesMs[srcIdx] ?? 0;
      }
      for (let i = this.statsCount; i < STATS_CAPACITY; i += 1) {
        this.tmpFrameTimes[i] = Number.POSITIVE_INFINITY;
      }
      this.tmpFrameTimes.sort();

      const p95Idx = Math.floor((this.statsCount - 1) * P95);
      const p99Idx = Math.floor((this.statsCount - 1) * P99);

      const p95 = this.tmpFrameTimes[p95Idx];
      const p99 = this.tmpFrameTimes[p99Idx];

      out.frameTimeP95Ms = isValidNumber(p95) ? p95 : undefined;
      out.fps1Low = isValidNumber(p99) && p99 > 0 ? 1000 / p99 : undefined;

      if (this.instructionsCount > 0 && this.instructionsFrameTimeSumMs > 0) {
        out.mipsAvg = (this.instructionsSum / (this.instructionsFrameTimeSumMs / 1000)) / 1_000_000;
      } else {
        out.mipsAvg = undefined;
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
      if (this.ioBytesCount > 0 && this.ioBytesFrameTimeSumMs > 0) {
        out.ioBytesPerSec = this.ioBytesSum / (this.ioBytesFrameTimeSumMs / 1000);
      } else {
        out.ioBytesPerSec = undefined;
      }
    }

    const memory = perfMemory();
    out.hostJsHeapUsedBytes = memory?.usedJSHeapSize;
    out.hostJsHeapTotalBytes = memory?.jsHeapSizeLimit;

    out.guestRamBytes = this.guestRamBytes;

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

  captureStart(): void {
    if (this.captureActive) return;
    this.captureActive = true;
    this.captureStartNowMs = performance.now();
    this.captureStartUnixMs = Date.now();
    this.captureDurationMs = 0;
    this.captureDroppedRecords = 0;
    this.captureRecords = 0;
    this.syncRaf();
  }

  captureStop(): void {
    if (!this.captureActive) return;
    this.captureDurationMs = performance.now() - this.captureStartNowMs;
    this.captureActive = false;
    this.syncRaf();
  }

  captureReset(): void {
    if (this.captureActive) {
      this.captureStartNowMs = performance.now();
      this.captureStartUnixMs = Date.now();
    }
    this.captureDurationMs = 0;
    this.captureDroppedRecords = 0;
    this.captureRecords = 0;
  }

  export(): unknown {
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

    return {
      kind: 'aero-perf-capture',
      version: 1,
      startUnixMs: this.captureStartUnixMs || null,
      durationMs: this.captureActive ? performance.now() - this.captureStartNowMs : this.captureDurationMs,
      droppedRecords: this.captureDroppedRecords,
      guestRamBytes: this.guestRamBytes ?? null,
      records,
    };
  }

  private syncRaf(): void {
    const shouldRun = this.hudActive || this.captureActive;
    if (shouldRun) {
      if (this.raf !== null) return;
      this.lastRafNowMs = performance.now();
      const tick = (nowMs: number) => {
        if (this.raf === null) return;
        this.raf = requestAnimationFrame(tick);
        const frameTimeMs = nowMs - this.lastRafNowMs;
        this.lastRafNowMs = nowMs;
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
