import test from "node:test";
import assert from "node:assert/strict";

test("mic worklet processor module is importable in Node (no AudioWorklet globals)", async () => {
  const mod = await import("../src/audio/mic-worklet-processor.js?worker&url");

  assert.equal(typeof mod.AeroMicCaptureProcessor, "function");
  assert.equal(typeof mod.default, "string");
});

