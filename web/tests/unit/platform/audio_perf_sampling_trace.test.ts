import { describe, expect, it } from "vitest";

import { AeroPerf } from "../../../src/perf/perf";
import { startAudioPerfSampling } from "../../../src/platform/audio";

describe("startAudioPerfSampling trace integration", () => {
  it("records audio.* counter events in exported traces when tracing is enabled", async () => {
    class FakePort {
      addEventListener(): void {}
      removeEventListener(): void {}
      start(): void {}
    }

    const output = {
      enabled: true,
      context: { sampleRate: 48_000, state: "running" as const },
      node: { port: new FakePort() },
      ringBuffer: { capacityFrames: 100, channelCount: 2 },
      resume: async () => {},
      close: async () => {},
      writeInterleaved: () => 0,
      getBufferLevelFrames: () => 10,
      getUnderrunCount: () => 1,
      getOverrunCount: () => 2,
      getMetrics: () => ({
        bufferLevelFrames: 10,
        capacityFrames: 100,
        underrunCount: 1,
        overrunCount: 2,
        sampleRate: 48_000,
        state: "running" as const,
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const perf = new AeroPerf();
    perf.traceStart();
    const stop = startAudioPerfSampling(output, perf, 10_000);
    stop();
    perf.traceStop();

    const trace = await perf.exportTrace();
    const keys = new Set<string>();
    for (const ev of trace.traceEvents) {
      if (ev.ph !== "C" || !ev.args) continue;
      for (const key of Object.keys(ev.args)) keys.add(key);
    }

    expect(keys.has("audio.bufferLevelFrames")).toBe(true);
    expect(keys.has("audio.underruns")).toBe(true);
    expect(keys.has("audio.overruns")).toBe(true);
    expect(keys.has("audio.sampleRate")).toBe(true);
  });
});

