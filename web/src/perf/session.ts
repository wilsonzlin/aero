import type { PerfAggregator, AggregatedFrame } from "./aggregator.js";
import type { PerfChannel } from "./shared.js";
import type { PerfWriter } from "./writer.js";

import { PerfAggregator as PerfAggregatorImpl } from "./aggregator.js";
import { WorkerKind } from "./record.js";
import { createPerfChannel } from "./shared.js";
import { PerfWriter as PerfWriterImpl } from "./writer.js";
import type { ByteSizedCacheTracker, GpuAllocationTracker } from "./memory";
import { WASM_PAGE_SIZE_BYTES } from "./memory";
import type { PerfApi, PerfHudSnapshot, PerfTimeBreakdownMs } from "./types";
import { ResponsivenessTracker, type ResponsivenessHudSnapshot } from "./responsiveness";

export type InstallPerfSessionOptions = {
  guestRamBytes?: number;
  wasmMemory?: WebAssembly.Memory;
  wasmMemoryMaxPages?: number;
  gpuTracker?: GpuAllocationTracker;
  jitCacheTracker?: ByteSizedCacheTracker;
  shaderCacheTracker?: ByteSizedCacheTracker;
};

type PerformanceMemoryLike = {
  usedJSHeapSize: number;
  totalJSHeapSize: number;
  jsHeapSizeLimit: number;
};

const perfMemory = (): PerformanceMemoryLike | undefined => {
  return (performance as unknown as { memory?: PerformanceMemoryLike }).memory;
};

const MIPS_SCALE = 1000;

const bigintDivToNumberScaled = (numerator: bigint, denominator: bigint, scale: number): number => {
  if (denominator === 0n) return 0;
  const scaled = (numerator * BigInt(scale)) / denominator;
  return Number(scaled) / scale;
};

export class PerfSession implements PerfApi {
  readonly guestRamBytes?: number;
  readonly wasmMemory?: WebAssembly.Memory;
  readonly wasmMemoryMaxPages?: number;
  readonly gpuTracker?: GpuAllocationTracker;
  readonly jitCacheTracker?: ByteSizedCacheTracker;
  readonly shaderCacheTracker?: ByteSizedCacheTracker;

  readonly channel: PerfChannel;

  private readonly writer: PerfWriter;
  private aggregator: PerfAggregator;

  private hudActive = false;
  private captureActive = false;
  private captureStartNowMs = 0;
  private captureDurationMs = 0;

  private frameId = 0;
  private raf: number | null = null;
  private lastRafNowMs = 0;

  private drainTimer: number | null = null;

  private responsiveness = new ResponsivenessTracker();
  private responsivenessSnapshot: ResponsivenessHudSnapshot = {};

  private peakHostJsHeapUsedBytes: number | undefined;
  private peakWasmMemoryBytes: number | undefined;
  private peakGpuEstimatedBytes: number | undefined;

  private readonly aggregatorOptions = {
    windowSize: 120,
    captureSize: 12_000,
    maxDrainPerBuffer: 5_000,
  };

  constructor(options: InstallPerfSessionOptions = {}) {
    this.guestRamBytes = options.guestRamBytes;
    this.wasmMemory = options.wasmMemory;
    this.wasmMemoryMaxPages = options.wasmMemoryMaxPages;
    this.gpuTracker = options.gpuTracker;
    this.jitCacheTracker = options.jitCacheTracker;
    this.shaderCacheTracker = options.shaderCacheTracker;

    this.channel = createPerfChannel();
    const mainBuffer = this.channel.buffers[WorkerKind.Main];
    if (!(mainBuffer instanceof SharedArrayBuffer)) {
      throw new Error("PerfSession expected main perf buffer to be a SharedArrayBuffer.");
    }

    this.writer = new PerfWriterImpl(mainBuffer, {
      workerKind: WorkerKind.Main,
      runStartEpochMs: this.channel.runStartEpochMs,
      enabled: false,
    });

    this.aggregator = new PerfAggregatorImpl(this.channel, this.aggregatorOptions);
  }

  setHudActive(active: boolean): void {
    if (this.hudActive === active) return;
    this.hudActive = active;
    this.syncLoops();
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
    this.captureDurationMs = 0;
    this.syncLoops();
  }

  captureStop(): void {
    if (!this.captureActive) return;
    this.captureDurationMs = performance.now() - this.captureStartNowMs;
    this.captureActive = false;
    this.syncLoops();
  }

  captureReset(): void {
    const shouldRunAfter = this.shouldRun();

    this.writer.setEnabled(false);
    this.stopRaf();
    this.stopDrainTimer();

    // Reset the underlying ring buffers (drops counters + head/tail) before
    // recreating the aggregator to avoid re-processing old samples.
    for (const ring of this.aggregator.readers.values()) {
      ring.reset();
    }

    this.aggregator = new PerfAggregatorImpl(this.channel, this.aggregatorOptions);
    this.frameId = 0;

    if (this.captureActive) {
      this.captureStartNowMs = performance.now();
    }
    this.captureDurationMs = 0;

    this.peakHostJsHeapUsedBytes = undefined;
    this.peakWasmMemoryBytes = undefined;
    this.peakGpuEstimatedBytes = undefined;
    this.responsiveness.reset();

    if (shouldRunAfter) {
      this.writer.setEnabled(true);
      this.syncLoops();
    }
  }

  export(): unknown {
    this.aggregator.drain();
    const out = this.aggregator.export() as Record<string, unknown>;
    out.responsiveness = this.responsiveness.export();
    return out;
  }

  getHudSnapshot(out: PerfHudSnapshot): PerfHudSnapshot {
    this.aggregator.drain();

    const stats = this.aggregator.getStats();

    out.nowMs = performance.now();
    out.fpsAvg = stats.avgFps > 0 ? stats.avgFps : undefined;
    out.fps1Low = stats.fps1pLow > 0 ? stats.fps1pLow : undefined;
    out.frameTimeAvgMs = stats.avgFrameMs > 0 ? stats.avgFrameMs : undefined;
    out.frameTimeP95Ms = stats.p95FrameMs > 0 ? stats.p95FrameMs : undefined;
    out.mipsAvg = stats.avgMips > 0 ? stats.avgMips : undefined;

    const windowAgg = this.computeWindowAggregates();
    out.lastFrameTimeMs = windowAgg.lastFrameTimeMs;
    out.lastMips = windowAgg.lastMips;
    out.breakdownAvgMs = windowAgg.breakdownAvgMs;
    out.drawCallsPerFrame = windowAgg.drawCallsPerFrame;
    out.ioBytesPerSec = windowAgg.ioBytesPerSec;

    const memory = perfMemory();
    out.hostJsHeapUsedBytes = memory?.usedJSHeapSize;
    out.hostJsHeapTotalBytes = memory?.totalJSHeapSize;
    out.hostJsHeapLimitBytes = memory?.jsHeapSizeLimit;

    out.guestRamBytes = this.guestRamBytes;

    if (this.wasmMemory) {
      const wasmBytes = this.wasmMemory.buffer.byteLength;
      out.wasmMemoryBytes = wasmBytes;
      out.wasmMemoryPages = wasmBytes / WASM_PAGE_SIZE_BYTES;
      out.wasmMemoryMaxPages = this.wasmMemoryMaxPages;
    } else {
      out.wasmMemoryBytes = undefined;
      out.wasmMemoryPages = undefined;
      out.wasmMemoryMaxPages = this.wasmMemoryMaxPages;
    }

    const gpuStats = this.gpuTracker?.getStats();
    out.gpuEstimatedBytes = gpuStats?.gpu_total_bytes ?? undefined;
    out.jitCodeCacheBytes = this.jitCacheTracker?.getTotalBytes();
    out.shaderCacheBytes = this.shaderCacheTracker?.getTotalBytes();

    if (out.hostJsHeapUsedBytes !== undefined) {
      this.peakHostJsHeapUsedBytes = Math.max(this.peakHostJsHeapUsedBytes ?? 0, out.hostJsHeapUsedBytes);
    }
    if (out.wasmMemoryBytes !== undefined) {
      this.peakWasmMemoryBytes = Math.max(this.peakWasmMemoryBytes ?? 0, out.wasmMemoryBytes);
    }
    if (out.gpuEstimatedBytes !== undefined) {
      this.peakGpuEstimatedBytes = Math.max(this.peakGpuEstimatedBytes ?? 0, out.gpuEstimatedBytes);
    }

    out.peakHostJsHeapUsedBytes = this.peakHostJsHeapUsedBytes;
    out.peakWasmMemoryBytes = this.peakWasmMemoryBytes;
    out.peakGpuEstimatedBytes = this.peakGpuEstimatedBytes;
    out.responsiveness = this.responsiveness.getHudSnapshot(this.responsivenessSnapshot);

    const captureDurationMs = this.captureActive ? performance.now() - this.captureStartNowMs : this.captureDurationMs;

    const capture = out.capture;
    capture.active = this.captureActive;
    capture.durationMs = captureDurationMs;
    capture.droppedRecords = this.getDroppedRecords();
    capture.records = this.aggregator.completedFrameIds.length;

    return out;
  }

  private shouldRun(): boolean {
    return this.hudActive || this.captureActive;
  }

  private syncLoops(): void {
    const shouldRun = this.shouldRun();

    this.responsiveness.setActive(shouldRun);
    this.writer.setEnabled(shouldRun);
    if (shouldRun) {
      this.startRaf();
      // When the HUD is visible, `getHudSnapshot()` is called frequently enough to
      // keep up with the per-frame writers. When capture is active while the HUD is
      // hidden, we run a small drain pump so the ring buffers don't overflow.
      if (this.captureActive && !this.hudActive) {
        this.startDrainTimer();
      } else {
        this.stopDrainTimer();
      }
    } else {
      this.stopRaf();
      this.stopDrainTimer();
    }
  }

  private startDrainTimer(): void {
    if (this.drainTimer !== null) return;
    this.drainTimer = window.setInterval(() => {
      this.aggregator.drain();
    }, 200);
  }

  private stopDrainTimer(): void {
    if (this.drainTimer === null) return;
    window.clearInterval(this.drainTimer);
    this.drainTimer = null;
  }

  private startRaf(): void {
    if (this.raf !== null) return;
    this.lastRafNowMs = performance.now();
    const tick = (nowMs: number) => {
      if (this.raf === null) return;
      this.raf = requestAnimationFrame(tick);

      const frameTimeMs = nowMs - this.lastRafNowMs;
      this.lastRafNowMs = nowMs;

      this.frameId = (this.frameId + 1) >>> 0;
      this.responsiveness.notePresent(nowMs);
      this.writer.frameSample(this.frameId, { durations: { frame_ms: frameTimeMs } });
    };
    this.raf = requestAnimationFrame(tick);
  }

  private stopRaf(): void {
    if (this.raf === null) return;
    cancelAnimationFrame(this.raf);
    this.raf = null;
  }

  private getDroppedRecords(): number {
    let dropped = 0;
    for (const ring of this.aggregator.readers.values()) {
      dropped += ring.getDroppedCount();
    }
    return dropped;
  }

  private computeWindowAggregates(): {
    lastFrameTimeMs?: number;
    lastMips?: number;
    breakdownAvgMs?: PerfTimeBreakdownMs;
    drawCallsPerFrame?: number;
    ioBytesPerSec?: number;
  } {
    const ids = this.aggregator.completedFrameIds;
    const windowSize = this.aggregator.windowSize;
    const startIdx = Math.max(0, ids.length - windowSize);

    let count = 0;
    let sumFrameUs = 0n;
    let sumCpuUs = 0n;
    let sumGpuUs = 0n;
    let sumIoUs = 0n;
    let sumJitUs = 0n;
    let sumDrawCalls = 0;
    let sumIoBytes = 0n;

    let lastFrame: AggregatedFrame | undefined;

    for (let i = startIdx; i < ids.length; i += 1) {
      const frame = this.aggregator.frames.get(ids[i]);
      if (!frame) continue;

      count += 1;
      sumFrameUs += BigInt(frame.frameUs >>> 0);
      sumCpuUs += BigInt(frame.cpuUs >>> 0);
      sumGpuUs += BigInt(frame.gpuUs >>> 0);
      sumIoUs += BigInt(frame.ioUs >>> 0);
      sumJitUs += BigInt(frame.jitUs >>> 0);
      sumDrawCalls += frame.drawCalls >>> 0;
      sumIoBytes += BigInt((frame.ioReadBytes + frame.ioWriteBytes) >>> 0);
      lastFrame = frame;
    }

    if (count === 0) {
      return {};
    }

    const breakdown: PerfTimeBreakdownMs = {};
    const avgCpuMs = Number(sumCpuUs) / count / 1000;
    const avgGpuMs = Number(sumGpuUs) / count / 1000;
    const avgIoMs = Number(sumIoUs) / count / 1000;
    const avgJitMs = Number(sumJitUs) / count / 1000;
    if (avgCpuMs > 0) breakdown.cpu = avgCpuMs;
    if (avgGpuMs > 0) breakdown.gpu = avgGpuMs;
    if (avgIoMs > 0) breakdown.io = avgIoMs;
    if (avgJitMs > 0) breakdown.jit = avgJitMs;

    const breakdownAvgMs = Object.keys(breakdown).length === 0 ? undefined : breakdown;

    const drawCallsPerFrame = sumDrawCalls > 0 ? sumDrawCalls / count : undefined;
    const ioBytesPerSec = sumIoBytes > 0n && sumFrameUs > 0n ? Number(sumIoBytes) / (Number(sumFrameUs) / 1_000_000) : undefined;

    const lastFrameTimeMs = lastFrame?.frameUs ? lastFrame.frameUs / 1000 : undefined;
    const lastMips =
      lastFrame && lastFrame.frameUs > 0 && lastFrame.instructions > 0n
        ? bigintDivToNumberScaled(lastFrame.instructions, BigInt(lastFrame.frameUs >>> 0), MIPS_SCALE)
        : undefined;

    return { lastFrameTimeMs, lastMips, breakdownAvgMs, drawCallsPerFrame, ioBytesPerSec };
  }
}
