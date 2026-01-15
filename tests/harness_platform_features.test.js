import test from "node:test";
import assert from "node:assert/strict";

import { detectPlatformFeatures, explainMissingRequirements } from "../src/platform/features.js";

const GLOBALS = globalThis;

function withGlobals(setup, fn) {
  const prev = {
    crossOriginIsolated: GLOBALS.crossOriginIsolated,
    FileSystemFileHandle: GLOBALS.FileSystemFileHandle,
    navigatorStorage: typeof navigator !== "undefined" ? navigator.storage : undefined,
  };
  try {
    setup();
    return fn();
  } finally {
    if (prev.crossOriginIsolated === undefined) delete GLOBALS.crossOriginIsolated;
    else GLOBALS.crossOriginIsolated = prev.crossOriginIsolated;

    if (prev.FileSystemFileHandle === undefined) delete GLOBALS.FileSystemFileHandle;
    else GLOBALS.FileSystemFileHandle = prev.FileSystemFileHandle;

    if (typeof navigator !== "undefined") {
      if (prev.navigatorStorage === undefined) delete navigator.storage;
      else navigator.storage = prev.navigatorStorage;
    }
  }
}

function report(overrides = {}) {
  return {
    crossOriginIsolated: false,
    sharedArrayBuffer: false,
    wasmSimd: false,
    wasmThreads: false,
    jit_dynamic_wasm: false,
    webgpu: false,
    webusb: false,
    opfs: false,
    opfsSyncAccessHandle: false,
    audioWorklet: false,
    offscreenCanvas: false,
    ...overrides,
  };
}

test("src/platform/features: detectPlatformFeatures detects OPFS SyncAccessHandle", () => {
  withGlobals(
    () => {
      // Ensure `opfs: true`.
      navigator.storage = { getDirectory: () => Promise.resolve(null) };

      // `createSyncAccessHandle()` is worker-only at runtime, but for detection it is sufficient
      // to check that the method exists on the prototype.
      GLOBALS.FileSystemFileHandle = class FileSystemFileHandle {
        createSyncAccessHandle() {}
      };
    },
    () => {
      const detected = detectPlatformFeatures();
      assert.equal(detected.opfs, true);
      assert.equal(detected.opfsSyncAccessHandle, true);
    },
  );
});

test("src/platform/features: missing SyncAccessHandle message mentions IndexedDB", () => {
  const messages = explainMissingRequirements(
    report({
      crossOriginIsolated: true,
      sharedArrayBuffer: true,
      wasmSimd: true,
      wasmThreads: true,
      jit_dynamic_wasm: true,
      webgpu: true,
      webusb: true,
      opfs: true,
      opfsSyncAccessHandle: false,
      audioWorklet: true,
      offscreenCanvas: true,
    }),
  );

  const text = messages.join("\n");
  assert.ok(/SyncAccessHandle/i.test(text), "expected SyncAccessHandle message");
  assert.ok(/IndexedDB/i.test(text), "expected IndexedDB mention in SyncAccessHandle message");
});

