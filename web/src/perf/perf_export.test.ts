import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { PerfSession } from "./session";
import { FallbackPerf } from "./fallback";
import { PerfWriter } from "./writer.js";
import { WorkerKind } from "./record.js";
import type { PerfHudSnapshot } from "./types";

function assertJsonSerializable(value: unknown): void {
  expect(() => JSON.stringify(value)).not.toThrow();
}

function assertPerfExportV2(value: unknown): asserts value is Record<string, unknown> {
  expect(value).toBeTruthy();
  expect(typeof value).toBe("object");
  expect(Array.isArray(value)).toBe(false);

  const exp = value as Record<string, unknown>;
  expect(exp.kind).toBe("aero-perf-capture");
  expect(exp.version).toBe(2);

  for (const key of [
    "build",
    "env",
    "capture",
    "capture_control",
    "summary",
    "frameTime",
    "records",
    "memory",
    "responsiveness",
    "jit",
    "buffers",
  ]) {
    expect(key in exp).toBe(true);
  }

  expect(Array.isArray(exp.records)).toBe(true);
  expect(Array.isArray(exp.buffers)).toBe(true);
}

describe("perf export schema", () => {
  const originals: Partial<Record<string, unknown>> = {};

  beforeEach(() => {
    // The perf layer is written for browsers; unit tests run under node.
    const g = globalThis as unknown as {
      window?: unknown;
      requestAnimationFrame?: unknown;
      cancelAnimationFrame?: unknown;
    };
    originals.window = g.window;
    originals.requestAnimationFrame = g.requestAnimationFrame;
    originals.cancelAnimationFrame = g.cancelAnimationFrame;

    g.window = globalThis;
    g.requestAnimationFrame = () => 0;
    g.cancelAnimationFrame = () => {};
  });

  afterEach(() => {
    const g = globalThis as unknown as {
      window?: unknown;
      requestAnimationFrame?: unknown;
      cancelAnimationFrame?: unknown;
    };
    g.window = originals.window;
    g.requestAnimationFrame = originals.requestAnimationFrame;
    g.cancelAnimationFrame = originals.cancelAnimationFrame;
  });

  it("exports v2 schema in SharedArrayBuffer mode (PerfSession)", () => {
    const session = new PerfSession();

    session.captureStart();

    const runStartEpochMs = session.channel.runStartEpochMs;
    const mainWriter = new PerfWriter(session.channel.buffers[WorkerKind.Main], {
      workerKind: WorkerKind.Main,
      runStartEpochMs,
    });
    const cpuWriter = new PerfWriter(session.channel.buffers[WorkerKind.CPU], {
      workerKind: WorkerKind.CPU,
      runStartEpochMs,
    });

    // Emit a few frames. The aggregator marks frames "complete" when it sees a
    // main-thread FrameSample with a non-zero frame duration.
    const baseEpochMs = runStartEpochMs + 100;
    for (let i = 0; i < 3; i += 1) {
      const frameId = 1 + i;
      const now_epoch_ms = baseEpochMs + i * 16;

      mainWriter.frameSample(frameId, { now_epoch_ms, durations: { frame_ms: 16 } });
      cpuWriter.frameSample(frameId, {
        now_epoch_ms,
        durations: { cpu_ms: 5 },
        counters: { instructions: 50_000n },
      });
    }

    session.captureStop();
    const exported = session.export();

    assertPerfExportV2(exported);
    assertJsonSerializable(exported);

    expect(exported.capture_control).toMatchObject({
      startFrameId: 1,
      endFrameId: 3,
    });
    expect((exported.records as unknown[]).length).toBe(3);
    const captureControl = exported.capture_control as Record<string, unknown>;
    expect(captureControl.records).toBe(3);
  });

  it("exports v2 schema in fallback mode (FallbackPerf)", () => {
    const perf = new FallbackPerf();

    perf.captureStart();
    perf.recordFrame(16, 50_000, { cpu: 5 });
    perf.recordFrame(16, 50_000, { cpu: 5 });
    perf.recordFrame(16, 50_000, { cpu: 5 });
    perf.captureStop();

    const exported = perf.export();
    assertPerfExportV2(exported);
    assertJsonSerializable(exported);

    expect((exported.records as unknown[]).length).toBe(3);
    const captureControl = exported.capture_control as Record<string, unknown>;
    expect(captureControl.records).toBe(3);
  });

  it("computes MIPS p95 for PerfSession HUD snapshots", () => {
    const session = new PerfSession();

    const runStartEpochMs = session.channel.runStartEpochMs;
    const mainWriter = new PerfWriter(session.channel.buffers[WorkerKind.Main], {
      workerKind: WorkerKind.Main,
      runStartEpochMs,
    });
    const cpuWriter = new PerfWriter(session.channel.buffers[WorkerKind.CPU], {
      workerKind: WorkerKind.CPU,
      runStartEpochMs,
    });

    const windowSize = (session as unknown as { aggregator: { windowSize: number } }).aggregator.windowSize;
    // Ensure we're testing a rolling window rather than "all recorded frames".
    const outlierFrames = 8;
    const totalFrames = windowSize + outlierFrames;
    const frameMs = 16;
    const frameUs = BigInt(frameMs * 1000);

    const baseEpochMs = runStartEpochMs + 100;
    for (let i = 0; i < totalFrames; i += 1) {
      const frameId = 1 + i;
      const now_epoch_ms = baseEpochMs + i * frameMs;

      expect(mainWriter.frameSample(frameId, { now_epoch_ms, durations: { frame_ms: frameMs } })).toBe(true);

      // First `outlierFrames` samples are outside of the HUD window and should
      // not affect the reported percentile.
      const mips = i < outlierFrames ? 1000 : i - outlierFrames + 1; // 1..windowSize
      const instructions = BigInt(mips) * frameUs;

      expect(cpuWriter.frameSample(frameId, { now_epoch_ms, counters: { instructions } })).toBe(true);
    }

    const snapshot: PerfHudSnapshot = {
      nowMs: 0,
      capture: {
        active: false,
        durationMs: 0,
        droppedRecords: 0,
        records: 0,
      },
    };

    session.getHudSnapshot(snapshot);

    const mipsP95 = snapshot.mipsP95;
    expect(mipsP95).toBeDefined();
    expect(Number.isFinite(mipsP95!)).toBe(true);

    // Values inside the HUD window are 1..windowSize, so p95 is deterministic.
    const p95Idx = Math.floor((windowSize - 1) * 0.95);
    const expected = p95Idx + 1;
    expect(mipsP95).toBeCloseTo(expected, 6);
  });
});
