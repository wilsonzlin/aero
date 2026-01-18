import type { PerfAggregator, AggregatedFrame } from "./aggregator.js";
import type { PerfChannel } from "./shared.js";

import { PerfAggregator as PerfAggregatorImpl, collectBuildMetadata, collectEnvironmentMetadata } from "./aggregator.js";
import { FrameTimeStats } from "../../../packages/aero-stats/src/index.js";
import type { PerfBufferStats, PerfCaptureRecord, PerfExport } from "./export";
import { JIT_DISABLED_SNAPSHOT } from "./export";
import { encodeFrameSampleRecord, msToUsU32, PERF_RECORD_SIZE_BYTES, WorkerKind, workerKindToString } from "./record.js";
import { SpscRingBuffer } from "./ring_buffer.js";
import {
  createPerfChannel,
  PERF_FRAME_HEADER_ENABLED_INDEX,
  PERF_FRAME_HEADER_FRAME_ID_INDEX,
  PERF_FRAME_HEADER_T_US_INDEX,
} from "./shared.js";
import type { ByteSizedCacheTracker, GpuAllocationTracker } from "./memory";
import { MemoryTelemetry } from "./memory";
import type { PerfApi, PerfHudSnapshot, PerfTimeBreakdownMs } from "./types";
import { ResponsivenessTracker, type ResponsivenessHudSnapshot } from "./responsiveness";
import { unrefBestEffort } from "../unrefSafe";

export type InstallPerfSessionOptions = {
  guestRamBytes?: number;
  wasmMemory?: WebAssembly.Memory;
  wasmMemoryMaxPages?: number;
  gpuTracker?: GpuAllocationTracker;
  jitCacheTracker?: ByteSizedCacheTracker;
  shaderCacheTracker?: ByteSizedCacheTracker;
};

const MIPS_SCALE = 1000;
const P95 = 0.95;

const bigintDivToNumberScaled = (numerator: bigint, denominator: bigint, scale: number): number => {
  if (denominator === 0n) return 0;
  const scaled = (numerator * BigInt(scale)) / denominator;
  return Number(scaled) / scale;
};

const bigintToJsonNumberOrString = (value: bigint): number | string => {
  if (value <= BigInt(Number.MAX_SAFE_INTEGER)) return Number(value);
  return value.toString();
};

export class PerfSession implements PerfApi {
  readonly guestRamBytes?: number;
  readonly wasmMemory?: WebAssembly.Memory;
  readonly wasmMemoryMaxPages?: number;
  readonly gpuTracker?: GpuAllocationTracker;
  readonly jitCacheTracker?: ByteSizedCacheTracker;
  readonly shaderCacheTracker?: ByteSizedCacheTracker;
  readonly memoryTelemetry: MemoryTelemetry;

  readonly channel: PerfChannel;
  private readonly frameHeader: Int32Array;
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
  private captureStartUnixMs = 0;
  private captureEndUnixMs = 0;
  private captureDurationMs = 0;
  private captureDroppedBase = 0;
  private captureDropped = 0;
  private captureRecords = 0;
  private captureStartFrameId: number | null = null;
  private captureEndFrameId: number | null = null;
  private captureExport: PerfExport | null = null;

  private frameId = 0;
  private raf: number | null = null;
  private lastRafNowMs = 0;

  private drainTimer: number | null = null;

  private responsiveness = new ResponsivenessTracker();
  private responsivenessSnapshot: ResponsivenessHudSnapshot = {};

  private tmpMips: Float32Array;

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

    this.memoryTelemetry = new MemoryTelemetry({
      wasmMemory: options.wasmMemory,
      wasmMemoryMaxPages: options.wasmMemoryMaxPages ?? null,
      getGuestMemoryStats: options.guestRamBytes
        ? () => ({ configured_bytes: options.guestRamBytes!, committed_bytes: options.guestRamBytes! })
        : null,
      gpuTracker: options.gpuTracker,
      jitCacheTracker: options.jitCacheTracker,
      shaderCacheTracker: options.shaderCacheTracker,
      sampleHz: 1,
      maxSamples: 600,
    });
    this.memoryTelemetry.sampleNow("boot");

    this.channel = createPerfChannel();
    this.runStartNowMs = this.channel.runStartEpochMs - performance.timeOrigin;
    if (!(this.channel.frameHeader instanceof SharedArrayBuffer)) {
      throw new Error("PerfSession expected perf frame header to be a SharedArrayBuffer.");
    }
    this.frameHeader = new Int32Array(this.channel.frameHeader);
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
    this.tmpMips = new Float32Array(this.aggregator.windowSize);
  }

  getChannel(): PerfChannel {
    return this.channel;
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
    this.captureStartUnixMs = Date.now();
    this.captureEndUnixMs = 0;
    this.captureDurationMs = 0;
    this.captureDroppedBase = this.getDroppedRecords();
    this.captureDropped = 0;
    this.captureRecords = 0;
    const lastCompletedFrameId = this.aggregator.completedFrameIds.at(-1);
    const startFrom = lastCompletedFrameId == null ? this.frameId : Math.max(this.frameId, lastCompletedFrameId);
    this.captureStartFrameId = (startFrom + 1) >>> 0;
    this.captureEndFrameId = null;
    this.captureExport = null;
    this.memoryTelemetry.sampleNow("capture_start");
    this.syncLoops();
  }

  captureStop(): void {
    if (!this.captureActive) return;
    this.aggregator.drain();
    this.captureDurationMs = performance.now() - this.captureStartNowMs;
    this.captureActive = false;
    this.captureEndUnixMs = Date.now();
    const lastCompletedFrameId = this.aggregator.completedFrameIds.at(-1);
    const endAt = lastCompletedFrameId == null ? this.frameId : Math.max(this.frameId, lastCompletedFrameId);
    this.captureEndFrameId = endAt >>> 0;
    this.captureDropped = Math.max(0, this.getDroppedRecords() - this.captureDroppedBase);
    this.memoryTelemetry.sampleNow("capture_stop");
    this.captureExport = this.buildCaptureExport();
    this.syncLoops();
  }

  captureReset(): void {
    this.captureExport = null;
    this.captureRecords = 0;
    this.captureDroppedBase = this.getDroppedRecords();
    this.captureDropped = 0;
    this.captureDurationMs = 0;

    this.memoryTelemetry.reset();
    this.memoryTelemetry.sampleNow("capture_reset");
    this.responsiveness.reset();

    if (this.captureActive) {
      this.captureStartNowMs = performance.now();
      this.captureStartUnixMs = Date.now();
      this.captureEndUnixMs = 0;
      const lastCompletedFrameId = this.aggregator.completedFrameIds.at(-1);
      const startFrom = lastCompletedFrameId == null ? this.frameId : Math.max(this.frameId, lastCompletedFrameId);
      this.captureStartFrameId = (startFrom + 1) >>> 0;
      this.captureEndFrameId = null;
    } else {
      this.captureStartUnixMs = 0;
      this.captureEndUnixMs = 0;
      // Represent an empty capture interval so downloads after reset produce an
      // empty capture until Start is pressed again.
      this.captureStartFrameId = (this.frameId + 1) >>> 0;
      this.captureEndFrameId = this.frameId >>> 0;
    }
  }

  export(): PerfExport {
    if (!this.captureActive && this.captureExport) {
      return this.captureExport;
    }
    this.aggregator.drain();
    const out = this.buildCaptureExport();
    if (!this.captureActive) {
      this.captureExport = out;
    }
    return out;
  }

  getHudSnapshot(out: PerfHudSnapshot): PerfHudSnapshot {
    this.aggregator.drain();

    const stats = this.aggregator.getStats();
    const hasGraphicsSamples = this.aggregator.totalGraphicsSampleRecords > 0;

    out.nowMs = performance.now();
    out.fpsAvg = stats.avgFps > 0 ? stats.avgFps : undefined;
    out.fps1Low = stats.fps1pLow > 0 ? stats.fps1pLow : undefined;
    out.frameTimeAvgMs = stats.avgFrameMs > 0 ? stats.avgFrameMs : undefined;
    out.frameTimeP95Ms = stats.p95FrameMs > 0 ? stats.p95FrameMs : undefined;
    out.mipsAvg = stats.avgMips > 0 ? stats.avgMips : undefined;
    out.mipsP95 = stats.p95Mips > 0 ? stats.p95Mips : undefined;

    const windowAgg = this.computeWindowAggregates();
    out.lastFrameTimeMs = windowAgg.lastFrameTimeMs;
    out.lastMips = windowAgg.lastMips;
    out.mipsP95 = windowAgg.mipsP95;
    out.breakdownAvgMs = windowAgg.breakdownAvgMs;
    out.drawCallsPerFrame = windowAgg.drawCallsPerFrame;
    out.pipelineSwitchesPerFrame = hasGraphicsSamples ? stats.pipelineSwitchesPerFrame : undefined;
    out.ioBytesPerSec = windowAgg.ioBytesPerSec;
    out.gpuUploadBytesPerSec = hasGraphicsSamples ? stats.gpuUploadBytesPerSec : undefined;

    out.gpuTimingSupported = hasGraphicsSamples ? stats.gpuTimingSupported : undefined;
    out.gpuTimingEnabled = hasGraphicsSamples ? stats.gpuTimingEnabled : undefined;

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

    Atomics.store(this.frameHeader, PERF_FRAME_HEADER_ENABLED_INDEX, shouldRun ? 1 : 0);
    if (!shouldRun) {
      // When the HUD/capture is inactive, clear the shared frame header so workers
      // can treat perf sampling as fully disabled.
      Atomics.store(this.frameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX, 0);
      Atomics.store(this.frameHeader, PERF_FRAME_HEADER_T_US_INDEX, 0);
    }
    this.responsiveness.setActive(shouldRun);
    if (shouldRun) {
      this.memoryTelemetry.start();
    } else {
      this.memoryTelemetry.stop();
    }
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
    const timer = window.setInterval(() => {
      this.aggregator.drain();
    }, 200);
    unrefBestEffort(timer);
    this.drainTimer = timer;
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
      const frameId = this.frameId;
      this.responsiveness.notePresent(nowMs);
      const tUs = Math.max(0, Math.min(0xffff_ffff, Math.round((nowMs - this.runStartNowMs) * 1000))) >>> 0;
      Atomics.store(this.frameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX, frameId);
      Atomics.store(this.frameHeader, PERF_FRAME_HEADER_T_US_INDEX, tUs);

      this.mainRecord.frameId = frameId;
      this.mainRecord.frameUs = msToUsU32(frameTimeMs);
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

  private buildCaptureExport(): PerfExport {
    const build = collectBuildMetadata() as PerfExport["build"];
    const env = collectEnvironmentMetadata() as PerfExport["env"];

    const durationMs = this.captureActive ? performance.now() - this.captureStartNowMs : this.captureDurationMs;

    const startFrameId = this.captureStartFrameId;
    const lastCompletedFrameId = this.aggregator.completedFrameIds.at(-1);
    const liveEndFrameId =
      lastCompletedFrameId == null ? (this.frameId >>> 0) : Math.max(this.frameId, lastCompletedFrameId) >>> 0;
    const endFrameId = this.captureEndFrameId ?? (this.captureActive ? liveEndFrameId : null);

    const captureStartUs = msToUsU32(this.captureStartNowMs - this.runStartNowMs);
    const frameTimeStats = new FrameTimeStats();
    const records: PerfCaptureRecord[] = [];

    let captureInstructionTotal = 0n;
    let captureFrameTimeTotalUs = 0n;

    if (startFrameId != null && endFrameId != null && endFrameId >= startFrameId) {
      for (const frameId of this.aggregator.completedFrameIds) {
        if (frameId < startFrameId || frameId > endFrameId) continue;
        const frame = this.aggregator.frames.get(frameId);
        if (!frame) continue;

        const frameTimeMs = frame.frameUs / 1000;
        frameTimeStats.pushFrameTimeMs(frameTimeMs);

        captureInstructionTotal += frame.instructions;
        captureFrameTimeTotalUs += BigInt(frame.frameUs >>> 0);

        const tUs = frame.tUs ?? 0;
        const tMs = tUs > captureStartUs ? (tUs - captureStartUs) / 1000 : 0;

        const ioBytes = (frame.ioReadBytes + frame.ioWriteBytes) >>> 0;

        records.push({
          tMs,
          frameTimeMs,
          instructions: frame.instructions > 0n ? bigintToJsonNumberOrString(frame.instructions) : null,
          cpuMs: frame.cpuUs > 0 ? frame.cpuUs / 1000 : null,
          gpuMs: frame.gpuUs > 0 ? frame.gpuUs / 1000 : null,
          ioMs: frame.ioUs > 0 ? frame.ioUs / 1000 : null,
          jitMs: frame.jitUs > 0 ? frame.jitUs / 1000 : null,
          drawCalls: frame.drawCalls > 0 ? frame.drawCalls : null,
          ioBytes: ioBytes > 0 ? ioBytes : null,
        });
      }
    }

    const frameTimeSummary = frameTimeStats.summary();
    const mipsAvg =
      captureInstructionTotal > 0n && captureFrameTimeTotalUs > 0n
        ? bigintDivToNumberScaled(captureInstructionTotal, captureFrameTimeTotalUs, MIPS_SCALE)
        : null;

    const buffers: PerfBufferStats[] = [];
    for (const [workerKind, ring] of this.aggregator.readers.entries()) {
      buffers.push({
        workerKind,
        worker: workerKindToString(workerKind),
        capacity: ring.getCapacity(),
        recordSize: ring.getRecordSize(),
        droppedRecords: ring.getDroppedCount(),
        drainedRecords: this.aggregator.recordCountsByWorkerKind.get(workerKind) ?? 0,
      });
    }

    const droppedRecords = this.captureActive
      ? Math.max(0, this.getDroppedRecords() - this.captureDroppedBase)
      : this.captureDropped;

    return {
      kind: "aero-perf-capture",
      version: 2,
      build,
      env,
      capture: {
        startUnixMs: this.captureStartUnixMs || null,
        endUnixMs: this.captureActive ? Date.now() : this.captureEndUnixMs || null,
        durationMs,
      },
      capture_control: {
        startFrameId,
        endFrameId,
        droppedRecords,
        records: records.length,
      },
      buffers,
      guestRamBytes: this.guestRamBytes ?? null,
      memory: this.memoryTelemetry.export(),
      summary: {
        frameTime: frameTimeSummary,
        mipsAvg,
      },
      frameTime: {
        summary: frameTimeSummary,
        stats: frameTimeStats.toJSON(),
      },
      responsiveness: this.responsiveness.export(),
      jit: JIT_DISABLED_SNAPSHOT,
      records,
    };
  }

  private computeWindowAggregates(): {
    lastFrameTimeMs?: number;
    lastMips?: number;
    mipsP95?: number;
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
    let mipsCount = 0;

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

      if (frame.frameUs > 0 && frame.instructions > 0n) {
        if (mipsCount >= this.tmpMips.length) {
          // In case the aggregator window size changes.
          this.tmpMips = new Float32Array(Math.max(this.tmpMips.length * 2, mipsCount + 1));
        }
        this.tmpMips[mipsCount] = bigintDivToNumberScaled(frame.instructions, BigInt(frame.frameUs >>> 0), MIPS_SCALE);
        mipsCount += 1;
      }
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

    let mipsP95: number | undefined;
    if (mipsCount > 0) {
      this.tmpMips.subarray(0, mipsCount).sort();
      const p95Idx = Math.floor((mipsCount - 1) * P95);
      const sample = this.tmpMips[p95Idx];
      mipsP95 = Number.isFinite(sample) ? sample : undefined;
    }

    return { lastFrameTimeMs, lastMips, mipsP95, breakdownAvgMs, drawCallsPerFrame, ioBytesPerSec };
  }
}
