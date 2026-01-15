import test from "node:test";
import assert from "node:assert/strict";

import {
  detectPlatformFeatures,
  explainMissingRequirements,
} from "../web/src/platform/features.js";
import { requestWebGpuDevice } from "../web/src/platform/webgpu.js";
import { getOpfsRoot, OpfsUnavailableError } from "../web/src/platform/opfs.js";

test("detectPlatformFeatures returns a stable boolean report shape", () => {
  const report = detectPlatformFeatures();
  for (const [key, value] of Object.entries(report)) {
    assert.equal(typeof value, "boolean", `${key} must be boolean`);
  }
});

test("explainMissingRequirements is empty when all capabilities are present", () => {
  const allTrue = {
    crossOriginIsolated: true,
    sharedArrayBuffer: true,
    wasmSimd: true,
    wasmThreads: true,
    jit_dynamic_wasm: true,
    webgpu: true,
    webusb: true,
    webgl2: true,
    opfs: true,
    opfsSyncAccessHandle: true,
    audioWorklet: true,
    offscreenCanvas: true,
  };

  assert.deepEqual(explainMissingRequirements(allTrue), []);
});

test("requestWebGpuDevice fails gracefully when WebGPU is unavailable", async () => {
  await assert.rejects(() => requestWebGpuDevice(), /WebGPU is not available/);
});

test("getOpfsRoot fails gracefully when OPFS is unavailable", async () => {
  await assert.rejects(
    () => getOpfsRoot(),
    (err) => err instanceof OpfsUnavailableError,
  );
});
