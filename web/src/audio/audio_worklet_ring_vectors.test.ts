import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  framesAvailable,
  framesAvailableClamped,
  framesFree,
} from "../platform/audio_worklet_ring_layout.js";

type Expected = {
  frames_available: number;
  frames_available_clamped: number;
  frames_free: number;
};

type Vector = {
  name?: string;
  read_idx: number;
  write_idx: number;
  capacity_frames: number;
  expected: Expected;
};

/**
 * Shared conformance vectors for AudioWorklet playback ring index math.
 *
 * These vectors are consumed by both Rust (`crates/platform/src/audio/worklet_bridge.rs`) and the
 * web unit tests to prevent subtle drift across languages.
 *
 * If you intentionally change the semantics, update:
 * - `crates/platform/src/audio/worklet_bridge.rs`
 * - `web/src/platform/audio_worklet_ring_layout.js`
 * - `tests/fixtures/audio_worklet_ring_vectors.json`
 */
describe("AudioWorklet ring index math matches shared vectors", () => {
  it("framesAvailable / framesAvailableClamped / framesFree", () => {
    const vectorsUrl = new URL("../../../tests/fixtures/audio_worklet_ring_vectors.json", import.meta.url);
    const vectors = JSON.parse(readFileSync(vectorsUrl, "utf8")) as Vector[];
    expect(vectors.length).toBeGreaterThan(0);

    for (const [i, v] of vectors.entries()) {
      const label = v.name ?? "<unnamed vector; please add a name>";
      expect(framesAvailable(v.read_idx, v.write_idx), `vector[${i}] ${label}: framesAvailable`).toBe(
        v.expected.frames_available,
      );
      expect(
        framesAvailableClamped(v.read_idx, v.write_idx, v.capacity_frames),
        `vector[${i}] ${label}: framesAvailableClamped`,
      ).toBe(v.expected.frames_available_clamped);
      expect(framesFree(v.read_idx, v.write_idx, v.capacity_frames), `vector[${i}] ${label}: framesFree`).toBe(
        v.expected.frames_free,
      );

      // Sanity: clamped/free are bounded by capacity.
      expect(v.expected.frames_available_clamped, `vector[${i}] ${label}: clamp sanity`).toBeLessThanOrEqual(
        v.capacity_frames,
      );
      expect(v.expected.frames_free, `vector[${i}] ${label}: free sanity`).toBeLessThanOrEqual(v.capacity_frames);
    }
  });
});
