import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  samplesAvailable,
  samplesAvailableClamped,
  samplesFree,
} from "./mic_ring.js";

type Expected = {
  samples_available: number;
  samples_available_clamped: number;
  samples_free: number;
};

type Vector = {
  name?: string;
  read_pos: number;
  write_pos: number;
  capacity_samples: number;
  expected: Expected;
};

/**
 * Shared conformance vectors for microphone capture ring index math.
 *
 * These vectors are consumed by both Rust (`crates/platform/src/audio/mic_bridge.rs`) and the web
 * unit tests to prevent subtle drift across languages.
 *
 * If you intentionally change the semantics, update:
 * - `crates/platform/src/audio/mic_bridge.rs`
 * - `web/src/audio/mic_ring.js`
 * - `tests/fixtures/mic_ring_vectors.json`
 */
describe("Mic ring index math matches shared vectors", () => {
  it("samplesAvailable / samplesAvailableClamped / samplesFree", () => {
    const vectorsUrl = new URL("../../../tests/fixtures/mic_ring_vectors.json", import.meta.url);
    const vectors = JSON.parse(readFileSync(vectorsUrl, "utf8")) as Vector[];
    expect(vectors.length).toBeGreaterThan(0);

    for (const [i, v] of vectors.entries()) {
      const label = v.name ?? "<unnamed vector; please add a name>";

      expect(samplesAvailable(v.read_pos, v.write_pos), `vector[${i}] ${label}: samplesAvailable`).toBe(
        v.expected.samples_available,
      );

      expect(
        samplesAvailableClamped(v.read_pos, v.write_pos, v.capacity_samples),
        `vector[${i}] ${label}: samplesAvailableClamped`,
      ).toBe(v.expected.samples_available_clamped);

      expect(samplesFree(v.read_pos, v.write_pos, v.capacity_samples), `vector[${i}] ${label}: samplesFree`).toBe(
        v.expected.samples_free,
      );

      // Sanity: clamped/free are bounded by capacity.
      expect(v.expected.samples_available_clamped, `vector[${i}] ${label}: clamp sanity`).toBeLessThanOrEqual(
        v.capacity_samples,
      );
      expect(v.expected.samples_free, `vector[${i}] ${label}: free sanity`).toBeLessThanOrEqual(v.capacity_samples);
    }
  });
});

