import { SpscRingBuffer } from "./ring_buffer.js";
import { decodePerfRecord, workerKindToString, PerfRecordType, PERF_RECORD_SIZE_BYTES } from "./record.js";
import { FrameTimeStats } from "../../../packages/aero-stats/src/index.js";

function addU32Saturating(a, b) {
  const sum = (a >>> 0) + (b >>> 0);
  return sum > 0xffff_ffff ? 0xffff_ffff : sum >>> 0;
}

function bigintDivToNumberScaled(numerator, denominator, scale) {
  if (denominator === 0n) return 0;
  // Produce a small-ish integer we can safely convert to number.
  const scaled = (numerator * BigInt(scale)) / denominator;
  return Number(scaled) / scale;
}

export class PerfAggregator {
  constructor(channel, { windowSize = 120, captureSize = 2000, maxDrainPerBuffer = 5000 } = {}) {
    if (!channel?.buffers) {
      throw new Error(`PerfAggregator requires a perf channel config with buffers`);
    }
    this.channel = channel;
    this.windowSize = windowSize;
    this.captureSize = Math.max(captureSize, windowSize);
    this.maxDrainPerBuffer = maxDrainPerBuffer;

    this.frames = new Map(); // frameId -> aggregated frame
    this.completedFrameIds = [];

    this.frameTimeScratch = new FrameTimeStats();

    this.readers = new Map();
    this.recordCountsByWorkerKind = new Map();
    this.totalRecordsDrained = 0;
    this.totalFrameSampleRecords = 0;
    for (const [kind, sab] of Object.entries(channel.buffers)) {
      // Object.entries yields string keys.
      const workerKind = Number(kind) >>> 0;
      this.readers.set(workerKind, new SpscRingBuffer(sab, { expectedRecordSize: PERF_RECORD_SIZE_BYTES }));
      this.recordCountsByWorkerKind.set(workerKind, 0);
    }
  }

  drain() {
    for (const [workerKind, ring] of this.readers.entries()) {
      ring.drain(this.maxDrainPerBuffer, (view, byteOffset) => {
        this.totalRecordsDrained++;
        this.recordCountsByWorkerKind.set(workerKind, (this.recordCountsByWorkerKind.get(workerKind) ?? 0) + 1);
        const record = decodePerfRecord(view, byteOffset);
        if (record?.type === PerfRecordType.FrameSample) {
          this.totalFrameSampleRecords++;
          this.#mergeFrameSample(record);
        }
      });
    }
  }

  #getOrCreateFrame(frameId) {
    let frame = this.frames.get(frameId);
    if (!frame) {
      frame = {
        frameId,
        tUs: undefined,
        frameUs: 0,
        cpuUs: 0,
        gpuUs: 0,
        ioUs: 0,
        jitUs: 0,
        instructions: 0n,
        memoryBytes: 0n,
        drawCalls: 0,
        ioReadBytes: 0,
        ioWriteBytes: 0,
        hasMainFrameTime: false,
      };
      this.frames.set(frameId, frame);
    }
    return frame;
  }

  #mergeFrameSample(sample) {
    const frameId = sample.frameId >>> 0;
    const frame = this.#getOrCreateFrame(frameId);

    // Timestamp: prefer main thread timestamp if available; otherwise keep first.
    if (sample.workerKind === 0 /* main */ && sample.tUs !== 0) {
      frame.tUs = sample.tUs >>> 0;
    } else if (frame.tUs == null && sample.tUs !== 0) {
      frame.tUs = sample.tUs >>> 0;
    }

    // Frame duration should not be additive (multiple producers may report it).
    if (sample.frameUs !== 0) {
      frame.frameUs = Math.max(frame.frameUs, sample.frameUs);
      if (sample.workerKind === 0 /* main */ && !frame.hasMainFrameTime) {
        frame.hasMainFrameTime = true;
        this.completedFrameIds.push(frameId);
        this.#evictIfNeeded();
      }
    }

    frame.cpuUs = addU32Saturating(frame.cpuUs, sample.cpuUs);
    frame.gpuUs = addU32Saturating(frame.gpuUs, sample.gpuUs);
    frame.ioUs = addU32Saturating(frame.ioUs, sample.ioUs);
    frame.jitUs = addU32Saturating(frame.jitUs, sample.jitUs);

    frame.instructions += sample.instructions;
    frame.memoryBytes = frame.memoryBytes > sample.memoryBytes ? frame.memoryBytes : sample.memoryBytes;
    frame.drawCalls = addU32Saturating(frame.drawCalls, sample.drawCalls);
    frame.ioReadBytes = addU32Saturating(frame.ioReadBytes, sample.ioReadBytes);
    frame.ioWriteBytes = addU32Saturating(frame.ioWriteBytes, sample.ioWriteBytes);
  }

  #evictIfNeeded() {
    while (this.completedFrameIds.length > this.captureSize) {
      const evicted = this.completedFrameIds.shift();
      if (evicted != null) {
        this.frames.delete(evicted);
      }
    }
  }

  getStats() {
    const frameIds = this.completedFrameIds.slice(-this.windowSize);
    const frames = frameIds.map((id) => this.frames.get(id)).filter(Boolean);
    if (frames.length === 0) {
      return {
        windowSize: this.windowSize,
        frames: 0,
        avgFrameMs: 0,
        p50FrameMs: 0,
        p95FrameMs: 0,
        p99FrameMs: 0,
        p999FrameMs: 0,
        avgFps: 0,
        fpsMedian: 0,
        fpsP95: 0,
        fps1pLow: 0,
        fps0_1pLow: 0,
        varianceFrameMs2: 0,
        stdevFrameMs: 0,
        covFrameTime: 0,
        avgMips: 0,
      };
    }

    this.frameTimeScratch.clear();
    const frameTimesUs = [];
    for (const f of frames) {
      if (!f || f.frameUs <= 0) continue;
      frameTimesUs.push(f.frameUs);
      this.frameTimeScratch.pushFrameTimeMs(f.frameUs / 1000);
    }

    const ft = this.frameTimeScratch.summary();

    const totalTimeUs = frameTimesUs.reduce((acc, v) => acc + BigInt(v >>> 0), 0n);
    const totalInstructions = frames.reduce((acc, f) => acc + f.instructions, 0n);

    const avgMips = bigintDivToNumberScaled(totalInstructions, totalTimeUs, 1000);

    return {
      windowSize: this.windowSize,
      frames: ft.frames,
      avgFrameMs: ft.meanFrameTimeMs,
      p50FrameMs: ft.frameTimeP50Ms,
      p95FrameMs: ft.frameTimeP95Ms,
      p99FrameMs: ft.frameTimeP99Ms,
      p999FrameMs: ft.frameTimeP999Ms,
      avgFps: ft.fpsAvg,
      fpsMedian: ft.fpsMedian,
      fpsP95: ft.fpsP95,
      fps1pLow: ft.fps1Low,
      fps0_1pLow: ft.fps0_1Low,
      varianceFrameMs2: ft.varianceFrameTimeMs2,
      stdevFrameMs: ft.stdevFrameTimeMs,
      covFrameTime: ft.covFrameTime,
      avgMips,
    };
  }

  export() {
    const windowSummary = this.getStats();
    const frameIds = this.completedFrameIds.slice(-this.captureSize);
    const frames = frameIds.map((id) => this.frames.get(id)).filter(Boolean);

    const captureFrameTimeStats = new FrameTimeStats();
    let captureTimeUs = 0n;
    let captureInstructions = 0n;
    for (const f of frames) {
      if (!f || f.frameUs <= 0) continue;
      captureFrameTimeStats.pushFrameTimeMs(f.frameUs / 1000);
      captureTimeUs += BigInt(f.frameUs >>> 0);
      captureInstructions += f.instructions;
    }
    const captureFrameTimeSummary = captureFrameTimeStats.summary();
    const captureMipsAvg = bigintDivToNumberScaled(captureInstructions, captureTimeUs, 1000);

    const buffers = [];
    let droppedTotal = 0;
    for (const [kind, ring] of this.readers.entries()) {
      const droppedRecords = ring.getDroppedCount();
      droppedTotal += droppedRecords;
      buffers.push({
        workerKind: kind,
        worker: workerKindToString(kind),
        capacity: ring.getCapacity(),
        recordSize: ring.getRecordSize(),
        droppedRecords,
        drainedRecords: this.recordCountsByWorkerKind.get(kind) ?? 0,
      });
    }

    const env = collectEnvironmentMetadata();
    const build = collectBuildMetadata();

    let captureStartUs = 0;
    let captureEndUs = 0;
    for (const frame of frames) {
      if (frame.tUs == null || frame.tUs === 0) continue;
      if (captureStartUs === 0 || frame.tUs < captureStartUs) captureStartUs = frame.tUs;
      if (frame.tUs > captureEndUs) captureEndUs = frame.tUs;
    }

    return {
      schema_version: 1,
      build,
      run_start_epoch_ms: this.channel.runStartEpochMs,
      exported_at_epoch_ms: env.now_epoch_ms,
      env,
      buffers,
      counts: {
        records_drained_total: this.totalRecordsDrained,
        frame_sample_records_total: this.totalFrameSampleRecords,
        dropped_records_total: droppedTotal,
      },
      capture: {
        start_t_us: captureStartUs,
        end_t_us: captureEndUs,
        duration_ms: captureEndUs >= captureStartUs ? (captureEndUs - captureStartUs) / 1000 : 0,
      },
      frame_time: {
        summary: captureFrameTimeSummary,
        stats: captureFrameTimeStats.toJSON(),
      },
      hud_window_summary: windowSummary,
      samples: {
        frame_count: frames.length,
        frames: frames.map((f) => ({
          frame_id: f.frameId,
          t_us: f.tUs ?? 0,
          durations_us: {
            frame: f.frameUs,
            cpu: f.cpuUs,
            gpu: f.gpuUs,
            io: f.ioUs,
            jit: f.jitUs,
          },
          counters: {
            instructions: f.instructions.toString(),
            memory_bytes: f.memoryBytes.toString(),
            draw_calls: f.drawCalls,
            io_read_bytes: f.ioReadBytes,
            io_write_bytes: f.ioWriteBytes,
          },
        })),
      },
      summary: {
        frameTime: captureFrameTimeSummary,
        mipsAvg: captureMipsAvg,
      },
    };
  }
}

export function collectEnvironmentMetadata() {
  const nav = typeof navigator !== "undefined" ? navigator : undefined;
  const ua = nav?.userAgent ?? null;
  const platform = nav?.platform ?? null;
  const hardwareConcurrency = nav?.hardwareConcurrency ?? null;
  const devicePixelRatio = typeof globalThis.devicePixelRatio === "number" ? globalThis.devicePixelRatio : null;
  const webgpu = !!(nav && "gpu" in nav);

  return {
    now_epoch_ms: typeof performance !== "undefined" ? performance.timeOrigin + performance.now() : Date.now(),
    userAgent: ua,
    platform,
    hardwareConcurrency,
    devicePixelRatio,
    webgpu,
  };
}

export function collectBuildMetadata() {
  // Best-effort; callers may inject at build time using a bundler define.
  const g = globalThis;
  const maybeBuild = g.__AERO_BUILD__ && typeof g.__AERO_BUILD__ === "object" ? g.__AERO_BUILD__ : {};
  const env = typeof process !== "undefined" ? process.env ?? {} : {};

  const gitSha =
    maybeBuild.git_sha ??
    g.__AERO_GIT_SHA__ ??
    env.AERO_GIT_SHA ??
    env.GIT_SHA ??
    env.VERCEL_GIT_COMMIT_SHA ??
    null;

  const mode = maybeBuild.mode ?? g.__AERO_BUILD_MODE__ ?? env.AERO_BUILD_MODE ?? env.NODE_ENV ?? null;

  return {
    git_sha: gitSha,
    mode,
  };
}
