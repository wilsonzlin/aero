import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { PerfSession } from "./session";
import { FallbackPerf } from "./fallback";
import { PerfWriter } from "./writer.js";
import { WorkerKind } from "./record.js";

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
    originals.window = (globalThis as any).window;
    originals.requestAnimationFrame = (globalThis as any).requestAnimationFrame;
    originals.cancelAnimationFrame = (globalThis as any).cancelAnimationFrame;

    (globalThis as any).window = globalThis;
    (globalThis as any).requestAnimationFrame = () => 0;
    (globalThis as any).cancelAnimationFrame = () => {};
  });

  afterEach(() => {
    (globalThis as any).window = originals.window;
    (globalThis as any).requestAnimationFrame = originals.requestAnimationFrame;
    (globalThis as any).cancelAnimationFrame = originals.cancelAnimationFrame;
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
    expect((exported.capture_control as any).records).toBe(3);
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
    expect((exported.capture_control as any).records).toBe(3);
  });
});

