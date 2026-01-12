import "fake-indexeddb/auto";

import assert from "node:assert/strict";
import test from "node:test";

import { PersistentGpuCache } from "../gpu-cache/persistent_cache.ts";

test("PersistentGpuCache IndexedDB: CRUD + telemetry", async () => {
  // Best-effort: the DB may not exist yet or may be blocked by another open handle.
  try {
    await PersistentGpuCache.clearAll();
  } catch {
    // Ignore.
  }

  const cache = await PersistentGpuCache.open({
    shaderLimits: { maxEntries: 16, maxBytes: 1024 * 1024 },
    pipelineLimits: { maxEntries: 16, maxBytes: 1024 * 1024 },
  });

  try {
    const key = "test-shader-key-crud";
    const value = {
      wgsl: "@compute @workgroup_size(1) fn main() {}",
      reflection: { entryPoints: [{ stage: "compute", name: "main" }], bindings: [] },
    };

    cache.resetTelemetry();

    // Miss before insert.
    assert.equal(await cache.getShader(key), null);

    await cache.putShader(key, value);

    const got = await cache.getShader(key);
    assert.deepEqual(got, value);

    const telemetry = cache.getTelemetry();
    assert.equal(telemetry.shader.misses, 1);
    assert.equal(telemetry.shader.hits, 1);
    assert.ok(telemetry.shader.bytesWritten > 0);
    assert.ok(telemetry.shader.bytesRead > 0);
    assert.equal(telemetry.shader.evictions, 0);

    const stats = await cache.stats();
    assert.equal(stats.opfs, false);
    assert.equal(stats.shaders.entries, 1);
    assert.ok(stats.shaders.bytes > 0);
  } finally {
    await cache.close();
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }
  }
});

test("PersistentGpuCache IndexedDB: LRU eviction (maxEntries)", async () => {
  try {
    await PersistentGpuCache.clearAll();
  } catch {
    // Ignore.
  }

  const cache = await PersistentGpuCache.open({
    shaderLimits: { maxEntries: 2, maxBytes: 10_000_000 },
    pipelineLimits: { maxEntries: 16, maxBytes: 1024 * 1024 },
  });

  const realNow = Date.now;
  try {
    cache.resetTelemetry();

    // Make `lastUsed` deterministic so eviction order is stable.
    let t = 0;
    (Date as unknown as { now: () => number }).now = () => {
      t += 1;
      return t;
    };

    const key1 = "test-shader-key-evict-1";
    const key2 = "test-shader-key-evict-2";
    const key3 = "test-shader-key-evict-3";

    await cache.putShader(key1, { wgsl: "// shader 1", reflection: { id: 1 } });
    await cache.putShader(key2, { wgsl: "// shader 2", reflection: { id: 2 } });
    await cache.putShader(key3, { wgsl: "// shader 3", reflection: { id: 3 } });

    // Key1 should have been evicted as the least-recently-used entry.
    assert.equal(await cache.getShader(key1), null);

    assert.ok(await cache.getShader(key2));
    assert.ok(await cache.getShader(key3));

    const stats = await cache.stats();
    assert.equal(stats.shaders.entries, 2);

    const telemetry = cache.getTelemetry();
    assert.equal(telemetry.shader.evictions, 1);
  } finally {
    (Date as unknown as { now: () => number }).now = realNow;
    await cache.close();
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }
  }
});

