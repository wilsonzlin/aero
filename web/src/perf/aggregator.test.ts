import { describe, expect, it } from "vitest";

import { PerfAggregator } from "./aggregator.js";
import { msToUsU32, WorkerKind } from "./record.js";
import { createPerfChannel } from "./shared.js";
import { PerfWriter } from "./writer.js";

describe("PerfAggregator", () => {
  it("merges per-frame samples from multiple worker kinds", () => {
    const channel = createPerfChannel({ capacity: 32 });
    const aggregator = new PerfAggregator(channel, { windowSize: 8, captureSize: 32, maxDrainPerBuffer: 128 });

    const runStartEpochMs = channel.runStartEpochMs;
    const nowEpochMs = runStartEpochMs + 5;

    const frameId = 1;

    const main = new PerfWriter(channel.buffers[WorkerKind.Main], { workerKind: WorkerKind.Main, runStartEpochMs });
    const cpu = new PerfWriter(channel.buffers[WorkerKind.CPU], { workerKind: WorkerKind.CPU, runStartEpochMs });
    const gpu = new PerfWriter(channel.buffers[WorkerKind.GPU], { workerKind: WorkerKind.GPU, runStartEpochMs });
    const io = new PerfWriter(channel.buffers[WorkerKind.IO], { workerKind: WorkerKind.IO, runStartEpochMs });

    main.frameSample(frameId, { now_epoch_ms: nowEpochMs, durations: { frame_ms: 16 } });
    cpu.frameSample(frameId, {
      now_epoch_ms: nowEpochMs,
      durations: { cpu_ms: 5 },
      counters: { instructions: 1_600_000n },
    });
    gpu.frameSample(frameId, { now_epoch_ms: nowEpochMs, durations: { gpu_ms: 2 } });
    io.frameSample(frameId, {
      now_epoch_ms: nowEpochMs,
      durations: { io_ms: 1 },
      counters: { io_read_bytes: 10, io_write_bytes: 20 },
    });

    aggregator.drain();

    const frame = aggregator.frames.get(frameId);
    expect(frame).toBeTruthy();
    expect(frame?.hasMainFrameTime).toBe(true);
    expect(aggregator.completedFrameIds).toContain(frameId);

    expect(frame?.frameUs).toBe(msToUsU32(16));
    expect(frame?.cpuUs).toBe(msToUsU32(5));
    expect(frame?.gpuUs).toBe(msToUsU32(2));
    expect(frame?.ioUs).toBe(msToUsU32(1));
    expect(frame?.jitUs).toBe(0);

    expect(frame?.instructions).toBe(1_600_000n);
    expect(frame?.ioReadBytes).toBe(10);
    expect(frame?.ioWriteBytes).toBe(20);

    const stats = aggregator.getStats();
    expect(stats.avgMips).toBeCloseTo(100, 3);
    expect(stats.p95Mips).toBeCloseTo(100, 3);
  });
});

