import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { AudioFrameClock } from "./audio_frame_clock";

type Vector = {
  name: string;
  sample_rate_hz: number;
  start_time_ns: number;
  steps: number[];
  expected_frames_per_step: number[];
  expected_final_frac: number;
};

function loadVectors(): Vector[] {
  const url = new URL("../../../tests/fixtures/audio_frame_clock_vectors.json", import.meta.url);
  return JSON.parse(readFileSync(url, "utf8")) as Vector[];
}

function numberToBigIntSafe(label: string, value: number): bigint {
  if (!Number.isSafeInteger(value)) {
    throw new Error(`${label} must be a safe integer to convert to bigint losslessly, got ${String(value)}`);
  }
  if (value < 0) {
    throw new Error(`${label} must be >= 0, got ${value}`);
  }
  return BigInt(value);
}

describe("AudioFrameClock conformance vectors (shared with Rust)", () => {
  it("matches the shared JSON test vectors", () => {
    const vectors = loadVectors();

    expect(vectors.length).toBeGreaterThan(0);

    for (const testCase of vectors) {
      expect(testCase.steps.length, `${testCase.name}: steps length`).toBe(testCase.expected_frames_per_step.length);

      const clock = new AudioFrameClock(testCase.sample_rate_hz, numberToBigIntSafe(`${testCase.name}: start_time_ns`, testCase.start_time_ns));
      for (let i = 0; i < testCase.steps.length; i++) {
        const nowNs = numberToBigIntSafe(`${testCase.name}: steps[${i}]`, testCase.steps[i]!);
        const expected = testCase.expected_frames_per_step[i]!;
        const actual = clock.advanceTo(nowNs);
        expect(actual, `${testCase.name}: step ${i} (now_ns=${nowNs})`).toBe(expected);
      }

      expect(clock.fracNsTimesRate, `${testCase.name}: fracNsTimesRate`).toBe(
        numberToBigIntSafe(`${testCase.name}: expected_final_frac`, testCase.expected_final_frac),
      );
    }
  });
});
