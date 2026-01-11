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
    this.totalGraphicsSampleRecords = 0;

    // PF-005 (hot path identification): populated by the CPU worker and
    // forwarded to the main thread via postMessage. Exported as a snapshot.
    this.hotspots = [];
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
        } else if (record?.type === PerfRecordType.GraphicsSample) {
          this.totalGraphicsSampleRecords++;
          this.#mergeGraphicsSample(record);
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
        renderPasses: 0,
        pipelineSwitches: 0,
        bindGroupChanges: 0,
        uploadBytes: 0n,
        cpuTranslateUs: 0,
        cpuEncodeUs: 0,
        gpuTimeUs: 0,
        gpuTimeValid: false,
        gpuTimingSupported: false,
        gpuTimingEnabled: false,
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

  #mergeGraphicsSample(sample) {
    const frameId = sample.frameId >>> 0;
    const frame = this.#getOrCreateFrame(frameId);

    if (sample.workerKind === 0 /* main */ && sample.tUs !== 0) {
      frame.tUs = sample.tUs >>> 0;
    } else if (frame.tUs == null && sample.tUs !== 0) {
      frame.tUs = sample.tUs >>> 0;
    }

    frame.renderPasses = addU32Saturating(frame.renderPasses, sample.renderPasses);
    frame.pipelineSwitches = addU32Saturating(frame.pipelineSwitches, sample.pipelineSwitches);
    frame.bindGroupChanges = addU32Saturating(frame.bindGroupChanges, sample.bindGroupChanges);
    frame.cpuTranslateUs = addU32Saturating(frame.cpuTranslateUs, sample.cpuTranslateUs);
    frame.cpuEncodeUs = addU32Saturating(frame.cpuEncodeUs, sample.cpuEncodeUs);

    const nextUploadBytes = frame.uploadBytes + sample.uploadBytes;
    frame.uploadBytes = nextUploadBytes > 0xffff_ffff_ffff_ffffn ? 0xffff_ffff_ffff_ffffn : nextUploadBytes;

    frame.gpuTimingSupported = frame.gpuTimingSupported || sample.gpuTimingSupported !== 0;
    frame.gpuTimingEnabled = frame.gpuTimingEnabled || sample.gpuTimingEnabled !== 0;

    if (sample.gpuTimeValid !== 0) {
      frame.gpuTimeValid = true;
      frame.gpuTimeUs = Math.max(frame.gpuTimeUs, sample.gpuTimeUs >>> 0);
    }
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
        drawCallsPerFrame: 0,
        renderPassesPerFrame: 0,
        pipelineSwitchesPerFrame: 0,
        bindGroupChangesPerFrame: 0,
        gpuUploadBytesPerSec: 0,
        cpuTranslateMs: 0,
        cpuEncodeMs: 0,
        gpuTimeAvgMs: null,
        gpuTimingSupported: false,
        gpuTimingEnabled: false,
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

    const drawCallsSum = frames.reduce((acc, f) => acc + (f.drawCalls >>> 0), 0);
    const renderPassesSum = frames.reduce((acc, f) => acc + (f.renderPasses >>> 0), 0);
    const pipelineSwitchesSum = frames.reduce((acc, f) => acc + (f.pipelineSwitches >>> 0), 0);
    const bindGroupChangesSum = frames.reduce((acc, f) => acc + (f.bindGroupChanges >>> 0), 0);
    const cpuTranslateUsSum = frames.reduce((acc, f) => acc + (f.cpuTranslateUs >>> 0), 0);
    const cpuEncodeUsSum = frames.reduce((acc, f) => acc + (f.cpuEncodeUs >>> 0), 0);
    const uploadBytesSum = frames.reduce((acc, f) => acc + (f.uploadBytes ?? 0n), 0n);

    let gpuTimeUsSum = 0;
    let gpuTimeCount = 0;
    let gpuTimingSupported = false;
    let gpuTimingEnabled = false;
    for (const f of frames) {
      gpuTimingSupported = gpuTimingSupported || !!f.gpuTimingSupported;
      gpuTimingEnabled = gpuTimingEnabled || !!f.gpuTimingEnabled;
      if (f.gpuTimeValid) {
        gpuTimeUsSum += f.gpuTimeUs >>> 0;
        gpuTimeCount += 1;
      }
    }

    const drawCallsPerFrame = frames.length ? drawCallsSum / frames.length : 0;
    const renderPassesPerFrame = frames.length ? renderPassesSum / frames.length : 0;
    const pipelineSwitchesPerFrame = frames.length ? pipelineSwitchesSum / frames.length : 0;
    const bindGroupChangesPerFrame = frames.length ? bindGroupChangesSum / frames.length : 0;
    const cpuTranslateMs = frames.length ? cpuTranslateUsSum / 1000 / frames.length : 0;
    const cpuEncodeMs = frames.length ? cpuEncodeUsSum / 1000 / frames.length : 0;

    const gpuUploadBytesPerSec =
      totalTimeUs > 0n ? bigintDivToNumberScaled(uploadBytesSum * 1_000_000n, totalTimeUs, 1000) : 0;

    const gpuTimeAvgMs = gpuTimeCount > 0 ? gpuTimeUsSum / 1000 / gpuTimeCount : null;

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
      drawCallsPerFrame,
      renderPassesPerFrame,
      pipelineSwitchesPerFrame,
      bindGroupChangesPerFrame,
      gpuUploadBytesPerSec,
      cpuTranslateMs,
      cpuEncodeMs,
      gpuTimeAvgMs,
      gpuTimingSupported,
      gpuTimingEnabled,
    };
  }

  setHotspots(hotspots) {
    this.hotspots = Array.isArray(hotspots) ? hotspots : [];
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
        graphics_sample_records_total: this.totalGraphicsSampleRecords,
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
      hotspots: this.hotspots,
      graphics: {
        rolling: {
          window_frames: this.windowSize,
          draw_calls_avg: windowSummary.drawCallsPerFrame,
          render_passes_avg: windowSummary.renderPassesPerFrame,
          pipeline_switches_avg: windowSummary.pipelineSwitchesPerFrame,
          bind_group_changes_avg: windowSummary.bindGroupChangesPerFrame,
          upload_mib_per_s: windowSummary.gpuUploadBytesPerSec / (1024 * 1024),
          cpu_translate_ms_avg: windowSummary.cpuTranslateMs,
          cpu_encode_ms_avg: windowSummary.cpuEncodeMs,
          gpu_time_ms_avg: windowSummary.gpuTimeAvgMs,
        },
        gpu_timing: {
          supported: windowSummary.gpuTimingSupported,
          enabled: windowSummary.gpuTimingEnabled,
        },
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
          graphics: {
            draw_calls: f.drawCalls,
            render_passes: f.renderPasses,
            pipeline_switches: f.pipelineSwitches,
            bind_group_changes: f.bindGroupChanges,
            upload_bytes: f.uploadBytes.toString(),
            cpu_translate_ms: f.cpuTranslateUs / 1000,
            cpu_encode_ms: f.cpuEncodeUs / 1000,
            gpu_time_ms: f.gpuTimeValid ? f.gpuTimeUs / 1000 : null,
            gpu_timing: {
              supported: !!f.gpuTimingSupported,
              enabled: !!f.gpuTimingEnabled,
            },
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

  const featuresRaw = maybeBuild.features ?? g.__AERO_BUILD_FEATURES__ ?? env.AERO_BUILD_FEATURES ?? null;
  let features = null;
  if (featuresRaw && typeof featuresRaw === "object" && !Array.isArray(featuresRaw)) {
    try {
      JSON.stringify(featuresRaw);
      features = featuresRaw;
    } catch {
      features = null;
    }
  }

  return {
    git_sha: gitSha,
    mode,
    features,
  };
}
