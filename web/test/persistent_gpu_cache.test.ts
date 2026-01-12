import assert from "node:assert/strict";
import { test } from "node:test";

import {
  BACKEND_KIND_DXBC_TO_WGSL,
  CACHE_SCHEMA_VERSION,
  computePipelineCacheKey,
  computeShaderCacheKey,
  formatCacheKey,
} from "../gpu-cache/persistent_cache.ts";

test("formatCacheKey changes with schema version / backend kind / device fingerprint", () => {
  const base = {
    schemaVersion: 1,
    backendKind: "example-backend",
    deviceFingerprint: "dev-a",
    contentHash: "abcd",
  };

  const k1 = formatCacheKey(base);
  const k2 = formatCacheKey({ ...base, schemaVersion: 2 });
  const k3 = formatCacheKey({ ...base, backendKind: "other-backend" });
  const k4 = formatCacheKey({ ...base, deviceFingerprint: "dev-b" });

  assert.notEqual(k1, k2);
  assert.notEqual(k1, k3);
  assert.notEqual(k1, k4);

  // Ensure schema version is embedded in the key string for easy debugging.
  assert.match(k1, new RegExp(`gpu-cache-v${base.schemaVersion}-`));
});

test("computeShaderCacheKey is stable and sensitive to inputs", async () => {
  const dxbcA = new Uint8Array([0x44, 0x58, 0x42, 0x43, 1, 2, 3, 4]);
  const dxbcB = new Uint8Array([0x44, 0x58, 0x42, 0x43, 9, 9, 9, 9]);

  const flagsBase = { halfPixelCenter: false, capsHash: "caps-a" };

  const k1 = await computeShaderCacheKey(dxbcA, flagsBase);
  const k1b = await computeShaderCacheKey(dxbcA, flagsBase);
  assert.equal(k1, k1b);

  // Changing content bytes should change key.
  const k2 = await computeShaderCacheKey(dxbcB, flagsBase);
  assert.notEqual(k1, k2);

  // Changing translation flags should change key (content_bytes component).
  const k3 = await computeShaderCacheKey(dxbcA, { ...flagsBase, halfPixelCenter: true });
  assert.notEqual(k1, k3);

  // Changing device fingerprint should change key (device_fingerprint component).
  const k4 = await computeShaderCacheKey(dxbcA, { ...flagsBase, capsHash: "caps-b" });
  assert.notEqual(k1, k4);

  // Changing any additional translation flags should change key (content_bytes component).
  // This is relied upon by the D3D9 DXBC->WGSL shader cache, which adds a translator-version
  // field to safely invalidate cached WGSL when translator semantics change.
  const k5 = await computeShaderCacheKey(dxbcA, { ...flagsBase, d3d9TranslatorVersion: 1 });
  const k6 = await computeShaderCacheKey(dxbcA, { ...flagsBase, d3d9TranslatorVersion: 2 });
  assert.notEqual(k5, k6);

  // Sanity check: the backend kind is present and schema version is embedded.
  assert.match(k1, new RegExp(`gpu-cache-v${CACHE_SCHEMA_VERSION}-${BACKEND_KIND_DXBC_TO_WGSL}-`));
});

test("computePipelineCacheKey is stable", async () => {
  const desc = { vertex: { hash: "a" }, fragment: { hash: "b" }, format: "rgba8unorm" };
  const k1 = await computePipelineCacheKey(desc);
  const k2 = await computePipelineCacheKey(desc);
  assert.equal(k1, k2);
});
