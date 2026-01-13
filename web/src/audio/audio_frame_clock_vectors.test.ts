import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { AudioFrameClock } from "./audio_frame_clock";

type VectorsFile = {
  version: number;
  description?: string;
  cases: VectorCase[];
};

type VectorCase = {
  name: string;
  sample_rate_hz: number;
  start_time_ns: string;
  now_ns: string[];
  expected_frames: number[];
  expected_end: {
    last_time_ns: string;
    frac_fp: string;
  };
};

function loadVectors(): VectorsFile {
  const url = new URL("../../../tests/fixtures/audio_frame_clock_vectors.json", import.meta.url);
  return JSON.parse(readFileSync(url, "utf8")) as VectorsFile;
}

describe("AudioFrameClock conformance vectors (shared with Rust)", () => {
  it("matches the shared JSON test vectors", () => {
    const vectors = loadVectors();

    for (const testCase of vectors.cases) {
      expect(testCase.now_ns.length, `${testCase.name}: now_ns length`).toBe(testCase.expected_frames.length);

      const clock = new AudioFrameClock(testCase.sample_rate_hz, BigInt(testCase.start_time_ns));
      for (let i = 0; i < testCase.now_ns.length; i++) {
        const nowNs = BigInt(testCase.now_ns[i]!);
        const expected = testCase.expected_frames[i]!;
        const actual = clock.advanceTo(nowNs);
        expect(actual, `${testCase.name}: step ${i} (now_ns=${nowNs})`).toBe(expected);
      }

      expect(clock.lastTimeNs, `${testCase.name}: lastTimeNs`).toBe(BigInt(testCase.expected_end.last_time_ns));
      expect(clock.fracNsTimesRate, `${testCase.name}: fracNsTimesRate`).toBe(BigInt(testCase.expected_end.frac_fp));
    }
  });
});

