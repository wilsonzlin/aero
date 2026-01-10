import type { PerfAggregator, AggregatedFrame } from "./aggregator.js";
import type { PerfChannel } from "./shared.js";

import { PerfAggregator as PerfAggregatorImpl } from "./aggregator.js";
import { encodeFrameSampleRecord, msToUsU32, PERF_RECORD_SIZE_BYTES, WorkerKind } from "./record.js";
import { SpscRingBuffer } from "./ring_buffer.js";
import { createPerfChannel } from "./shared.js";
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
  private readonly runStartNowMs: number;
  private readonly mainRing: SpscRingBuffer;
  private readonly mainRecord: {
    workerKind: number;
    frameId: number;
    tUs: number;
    frameUs: number;
    cpuUs: number;
    gpuUs: number;
    ioUs: number;
    jitUs: number;
    instructionsLo: number;
    instructionsHi: number;
    memoryLo: number;
    memoryHi: number;
    drawCalls: number;
    ioReadBytes: number;
    ioWriteBytes: number;
  };
  private readonly encodeMainRecord: (view: DataView, byteOffset: number) => void;

  private aggregator: PerfAggregator;

  private hudActive = false;
  private captureActive = false;
  private captureStartNowMs = 0;
  private captureDurationMs = 0;
  private captureDroppedBase = 0;
  private captureDropped = 0;
  private captureRecords = 0;
  private captureStartFrameId: number | null = null;
  private captureEndFrameId: number | null = null;
  private captureExport: unknown | null = null;

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
    this.runStartNowMs = this.channel.runStartEpochMs - performance.timeOrigin;
    const mainBuffer = this.channel.buffers[WorkerKind.Main];
    if (!(mainBuffer instanceof SharedArrayBuffer)) {
      throw new Error("PerfSession expected main perf buffer to be a SharedArrayBuffer.");
    }

    this.mainRing = new SpscRingBuffer(mainBuffer, { expectedRecordSize: PERF_RECORD_SIZE_BYTES });
    this.mainRecord = {
      workerKind: WorkerKind.Main,
      frameId: 0,
      tUs: 0,
      frameUs: 0,
      cpuUs: 0,
      gpuUs: 0,
      ioUs: 0,
      jitUs: 0,
      instructionsLo: 0,
      instructionsHi: 0,
      memoryLo: 0,
      memoryHi: 0,
      drawCalls: 0,
      ioReadBytes: 0,
      ioWriteBytes: 0,
    };
    this.encodeMainRecord = (view: DataView, byteOffset: number) => {
      encodeFrameSampleRecord(view, byteOffset, this.mainRecord);
    };

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
    this.captureDroppedBase = this.getDroppedRecords();
    this.captureDropped = 0;
    this.captureRecords = 0;
    this.captureStartFrameId = (this.frameId + 1) >>> 0;
    this.captureEndFrameId = null;
    this.captureExport = null;
    this.syncLoops();
  }

  captureStop(): void {
    if (!this.captureActive) return;
    this.aggregator.drain();
    this.captureDurationMs = performance.now() - this.captureStartNowMs;
    this.captureActive = false;
    this.captureEndFrameId = this.frameId >>> 0;
    this.captureDropped = Math.max(0, this.getDroppedRecords() - this.captureDroppedBase);
    const out = this.buildCaptureExport() as Record<string, unknown>;
    out.responsiveness = this.responsiveness.export();
    this.captureExport = out;
    this.syncLoops();
  }

  captureReset(): void {
    this.captureExport = null;
    this.captureRecords = 0;
    this.captureDroppedBase = this.getDroppedRecords();
    this.captureDropped = 0;
    this.captureDurationMs = 0;

    this.peakHostJsHeapUsedBytes = undefined;
    this.peakWasmMemoryBytes = undefined;
    this.peakGpuEstimatedBytes = undefined;
    this.responsiveness.reset();

    if (this.captureActive) {
      this.captureStartNowMs = performance.now();
      this.captureStartFrameId = (this.frameId + 1) >>> 0;
      this.captureEndFrameId = null;
    } else {
      // Represent an empty capture interval so downloads after reset produce an
      // empty capture until Start is pressed again.
      this.captureStartFrameId = (this.frameId + 1) >>> 0;
      this.captureEndFrameId = this.frameId >>> 0;
    }
  }

  export(): unknown {
    if (!this.captureActive && this.captureExport) {
      return this.captureExport;
    }
    this.aggregator.drain();
    const out = this.buildCaptureExport() as Record<string, unknown>;
    out.responsiveness = this.responsiveness.export();
    if (!this.captureActive) {
      this.captureExport = out;
    }
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
    capture.droppedRecords = this.captureActive
      ? Math.max(0, this.getDroppedRecords() - this.captureDroppedBase)
      : this.captureDropped;
    capture.records = this.captureRecords;

    return out;
  }

  private shouldRun(): boolean {
    return this.hudActive || this.captureActive;
  }

  private syncLoops(): void {
    const shouldRun = this.shouldRun();

    this.responsiveness.setActive(shouldRun);
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
      this.mainRecord.frameId = this.frameId;
      this.mainRecord.frameUs = msToUsU32(frameTimeMs);
      const tUs = Math.max(0, Math.min(0xffff_ffff, Math.round((nowMs - this.runStartNowMs) * 1000))) >>> 0;
      this.mainRecord.tUs = tUs;
      const wrote = this.mainRing.tryWriteRecord(this.encodeMainRecord);
      if (this.captureActive && wrote) {
        this.captureRecords += 1;
      }
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

  private buildCaptureExport(): unknown {
    const base = this.aggregator.export() as Record<string, unknown>;

    const startFrameId = this.captureStartFrameId;
    const endFrameId = this.captureEndFrameId ?? (this.captureActive ? (this.frameId >>> 0) : null);

    const samples = (base.samples as Record<string, unknown>) ?? {};
    const framesRaw = samples.frames;
    if (!Array.isArray(framesRaw)) {
      return base;
    }

    if (startFrameId == null || endFrameId == null) {
      samples.frame_count = 0;
      samples.frames = [];
      base.samples = samples;
      base.capture = { start_t_us: 0, end_t_us: 0, duration_ms: 0 };
      base.capture_control = {
        start_frame_id: startFrameId,
        end_frame_id: endFrameId,
        dropped_records: this.captureActive
          ? Math.max(0, this.getDroppedRecords() - this.captureDroppedBase)
          : this.captureDropped,
        records: this.captureRecords,
      };
      return base;
    }

    const frames = [];
    let startUs = 0;
    let endUs = 0;

    for (const frame of framesRaw) {
      if (!frame || typeof frame !== "object") continue;
      const id = (frame as { frame_id?: unknown }).frame_id;
      if (typeof id !== "number") continue;
      if (id < startFrameId || id > endFrameId) continue;
      frames.push(frame);

      const tUs = (frame as { t_us?: unknown }).t_us;
      if (typeof tUs === "number" && tUs > 0) {
        if (startUs === 0 || tUs < startUs) startUs = tUs;
        if (tUs > endUs) endUs = tUs;
      }
    }

    samples.frame_count = frames.length;
    samples.frames = frames;
    base.samples = samples;

    base.capture = {
      start_t_us: startUs,
      end_t_us: endUs,
      duration_ms: endUs >= startUs ? (endUs - startUs) / 1000 : 0,
    };

    base.capture_control = {
      start_frame_id: startFrameId,
      end_frame_id: endFrameId,
      dropped_records: this.captureActive
        ? Math.max(0, this.getDroppedRecords() - this.captureDroppedBase)
        : this.captureDropped,
      records: this.captureRecords,
    };

    return base;
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
