import { SpscRingBuffer } from "./ring_buffer.js";
import { decodePerfRecord, workerKindToString, PerfRecordType, PERF_RECORD_SIZE_BYTES } from "./record.js";

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.floor(p * (sorted.length - 1))));
  return sorted[idx];
}

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
        avgFps: 0,
        fps1pLow: 0,
        avgMips: 0,
      };
    }

    const frameTimesUs = frames.map((f) => f.frameUs).filter((v) => v > 0);
    const sortedUs = frameTimesUs.slice().sort((a, b) => a - b);

    const totalTimeUs = frameTimesUs.reduce((acc, v) => acc + BigInt(v >>> 0), 0n);
    const totalInstructions = frames.reduce((acc, f) => acc + f.instructions, 0n);

    const avgFrameUs = frameTimesUs.reduce((a, b) => a + b, 0) / frameTimesUs.length;
    const p50Us = percentile(sortedUs, 0.5);
    const p95Us = percentile(sortedUs, 0.95);
    const p99Us = percentile(sortedUs, 0.99);

    const avgFps = bigintDivToNumberScaled(BigInt(frameTimesUs.length) * 1_000_000n, totalTimeUs, 1000);
    const avgMips = bigintDivToNumberScaled(totalInstructions, totalTimeUs, 1000);

    const fps1pLow = p99Us > 0 ? 1_000_000 / p99Us : 0;

    return {
      windowSize: this.windowSize,
      frames: frames.length,
      avgFrameMs: avgFrameUs / 1000,
      p50FrameMs: p50Us / 1000,
      p95FrameMs: p95Us / 1000,
      p99FrameMs: p99Us / 1000,
      avgFps,
      fps1pLow,
      avgMips,
    };
  }

  export() {
    const stats = this.getStats();
    const frameIds = this.completedFrameIds.slice(-this.captureSize);
    const frames = frameIds.map((id) => this.frames.get(id)).filter(Boolean);

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
      summary: stats,
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
