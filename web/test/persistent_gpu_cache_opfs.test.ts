import "fake-indexeddb/auto";

import assert from "node:assert/strict";
import test from "node:test";

import { installOpfsMock } from "./opfs_mock.ts";
import { PersistentGpuCache } from "../gpu-cache/persistent_cache.ts";

test("PersistentGpuCache OPFS: large shader spills to OPFS and metadata stays in IDB", async () => {
  const realNavigatorStorage = (navigator as any).storage;
  const hadNavigatorStorage = Object.prototype.hasOwnProperty.call(navigator as any, "storage");
  const root = installOpfsMock();

  try {
    // Best-effort cleanup.
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }

    const cache1 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });

    const key = "test-shader-key-opfs-spill";
    const padLine = "// padding ................................................................................\n";
    const wgsl = padLine.repeat(4000) + "@compute @workgroup_size(1) fn main() {}";
    const reflection = { bindings: [] };
    let expectedWgsl = wgsl;
    let expectedReflection = reflection;

    // Sanity: ensure payload is well above the 256KiB OPFS threshold.
    assert.ok(new TextEncoder().encode(wgsl).byteLength > 300 * 1024);

    try {
      const statsBefore = await cache1.stats();
      assert.equal(statsBefore.opfs, true);

      await cache1.putShader(key, { wgsl, reflection });

      // Verify IDB record is metadata-only and points at OPFS.
      const tx = (cache1 as any)._db.transaction(["shaders"], "readonly");
      const store = tx.objectStore("shaders");
      const record = await new Promise<any>((resolve, reject) => {
        const req = store.get(key);
        req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
        req.onsuccess = () => resolve(req.result ?? null);
      });
      await new Promise<void>((resolve) => {
        tx.oncomplete = () => resolve();
        tx.onabort = () => resolve();
        tx.onerror = () => resolve();
      });

      assert.ok(record, "expected shader record in IndexedDB");
      assert.equal(record.storage, "opfs");
      assert.equal(record.opfsFile, `${key}.json`);
      assert.equal(typeof record.wgsl, "undefined");
      assert.equal(typeof record.reflection, "undefined");

      // Verify OPFS JSON file exists and contains the shader payload.
      const cacheDir = await root.getDirectoryHandle("aero-gpu-cache");
      const shadersDir = await cacheDir.getDirectoryHandle("shaders");
      const handle = await shadersDir.getFileHandle(`${key}.json`);
      const file = await handle.getFile();
      const text = await file.text();
      const parsed = JSON.parse(text);
      assert.equal(parsed.wgsl, wgsl);
      assert.deepEqual(parsed.reflection, reflection);
      assert.ok(file.size > 256 * 1024);

      // Verify reads go through OPFS and roundtrip.
      const got = await cache1.getShader(key);
      assert.deepEqual(got, { wgsl, reflection });

      // Rewriting the same key should truncate/replace the OPFS blob (not append).
      const wgsl2 = `// v2\n${wgsl}`;
      const reflection2 = { bindings: [], version: 2 };
      assert.ok(new TextEncoder().encode(wgsl2).byteLength > 300 * 1024);
      await cache1.putShader(key, { wgsl: wgsl2, reflection: reflection2 });

      const handle2 = await shadersDir.getFileHandle(`${key}.json`);
      const file2 = await handle2.getFile();
      const parsed2 = JSON.parse(await file2.text());
      assert.equal(parsed2.wgsl, wgsl2);
      assert.deepEqual(parsed2.reflection, reflection2);

      const gotUpdated = await cache1.getShader(key);
      assert.deepEqual(gotUpdated, { wgsl: wgsl2, reflection: reflection2 });

      expectedWgsl = wgsl2;
      expectedReflection = reflection2;
    } finally {
      await cache1.close();
    }

    // Re-open to simulate a new browser session and verify the OPFS payload can
    // be read back.
    const cache2 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });
    try {
      const statsAfter = await cache2.stats();
      assert.equal(statsAfter.opfs, true);

      const got2 = await cache2.getShader(key);
      assert.deepEqual(got2, { wgsl: expectedWgsl, reflection: expectedReflection });

      // Rewrite to a small payload. This should store directly in IDB and delete
      // the OPFS blob for the key.
      const wgsl3 = "@compute @workgroup_size(1) fn main() {}";
      const reflection3 = { bindings: [], version: 3 };
      await cache2.putShader(key, { wgsl: wgsl3, reflection: reflection3 });

      const cacheDir = await root.getDirectoryHandle("aero-gpu-cache");
      const shadersDir = await cacheDir.getDirectoryHandle("shaders");
      let fileExists = true;
      try {
        await shadersDir.getFileHandle(`${key}.json`);
      } catch {
        fileExists = false;
      }
      assert.equal(fileExists, false);

      const got3 = await cache2.getShader(key);
      assert.deepEqual(got3, { wgsl: wgsl3, reflection: reflection3 });

      expectedWgsl = wgsl3;
      expectedReflection = reflection3;
    } finally {
      await cache2.close();
    }

    const cache3 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });
    try {
      const got4 = await cache3.getShader(key);
      assert.deepEqual(got4, { wgsl: expectedWgsl, reflection: expectedReflection });
    } finally {
      await cache3.close();
    }

    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }
  } finally {
    if (hadNavigatorStorage) {
      (navigator as any).storage = realNavigatorStorage;
    } else {
      delete (navigator as any).storage;
    }
  }
});

test("PersistentGpuCache OPFS: large pipeline descriptor spills to OPFS and can migrate back to IDB", async () => {
  const realNavigatorStorage = (navigator as any).storage;
  const hadNavigatorStorage = Object.prototype.hasOwnProperty.call(navigator as any, "storage");
  const root = installOpfsMock();

  try {
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }

    const key = "test-pipeline-key-opfs-spill";
    const largeDesc = { big: "x".repeat(310 * 1024), version: 1 };
    assert.ok(new TextEncoder().encode(JSON.stringify(largeDesc)).byteLength > 300 * 1024);

    const cache1 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });
    let expectedDesc = largeDesc;

    try {
      const statsBefore = await cache1.stats();
      assert.equal(statsBefore.opfs, true);

      await cache1.putPipelineDescriptor(key, largeDesc);

      // Verify IDB record is metadata-only and points at OPFS.
      const tx = (cache1 as any)._db.transaction(["pipelines"], "readonly");
      const store = tx.objectStore("pipelines");
      const record = await new Promise<any>((resolve, reject) => {
        const req = store.get(key);
        req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
        req.onsuccess = () => resolve(req.result ?? null);
      });
      await new Promise<void>((resolve) => {
        tx.oncomplete = () => resolve();
        tx.onabort = () => resolve();
        tx.onerror = () => resolve();
      });

      assert.ok(record, "expected pipeline record in IndexedDB");
      assert.equal(record.storage, "opfs");
      assert.equal(record.opfsFile, `${key}.json`);
      assert.equal(typeof record.desc, "undefined");

      const cacheDir = await root.getDirectoryHandle("aero-gpu-cache");
      const pipelinesDir = await cacheDir.getDirectoryHandle("pipelines");
      const handle = await pipelinesDir.getFileHandle(`${key}.json`);
      const file = await handle.getFile();
      assert.ok(file.size > 256 * 1024);
      assert.deepEqual(JSON.parse(await file.text()), largeDesc);

      // Rewrite to a different large payload; should overwrite the same OPFS blob.
      const largeDesc2 = { big: "y".repeat(310 * 1024), version: 2 };
      await cache1.putPipelineDescriptor(key, largeDesc2);
      expectedDesc = largeDesc2;

      const handle2 = await pipelinesDir.getFileHandle(`${key}.json`);
      const file2 = await handle2.getFile();
      assert.deepEqual(JSON.parse(await file2.text()), largeDesc2);
    } finally {
      await cache1.close();
    }

    // Re-open to simulate a new browser session and verify the OPFS payload can
    // be read back.
    const cache2 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });
    try {
      cache2.pipelineDescriptors.clear(); // force read path rather than warmed map
      const got = await cache2.getPipelineDescriptor(key);
      assert.deepEqual(got, expectedDesc);

      // Rewrite to a small payload, forcing an OPFS -> IDB migration and blob deletion.
      const smallDesc = { version: 3, name: "small" };
      await cache2.putPipelineDescriptor(key, smallDesc);
      expectedDesc = smallDesc;

      const cacheDir = await root.getDirectoryHandle("aero-gpu-cache");
      const pipelinesDir = await cacheDir.getDirectoryHandle("pipelines");
      let fileExists = true;
      try {
        await pipelinesDir.getFileHandle(`${key}.json`);
      } catch {
        fileExists = false;
      }
      assert.equal(fileExists, false);

      cache2.pipelineDescriptors.clear();
      const got2 = await cache2.getPipelineDescriptor(key);
      assert.deepEqual(got2, smallDesc);
    } finally {
      await cache2.close();
    }

    const cache3 = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });
    try {
      cache3.pipelineDescriptors.clear();
      const got3 = await cache3.getPipelineDescriptor(key);
      assert.deepEqual(got3, expectedDesc);
    } finally {
      await cache3.close();
    }
  } finally {
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }

    if (hadNavigatorStorage) {
      (navigator as any).storage = realNavigatorStorage;
    } else {
      delete (navigator as any).storage;
    }
  }
});
