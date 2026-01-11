import test from "node:test";
import assert from "node:assert/strict";

test("mic worklet processor module is importable in Node (no AudioWorklet globals)", async () => {
  // Import the worklet module directly to ensure it doesn't depend on
  // AudioWorklet-only globals at module evaluation time.
  const mod = await import("../src/audio/mic-worklet-processor.js");

  assert.equal(typeof mod.AeroMicCaptureProcessor, "function");
  assert.equal(typeof mod.default, "string");

  // And ensure the Vite-style `?worker&url` import path is usable under Node's
  // unit-test runner (our ESM loader synthesizes a default export URL string).
  const urlMod = await import("../src/audio/mic-worklet-processor.js?worker&url");
  assert.equal(typeof urlMod.default, "string");
});
