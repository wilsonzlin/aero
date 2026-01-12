import "fake-indexeddb/auto";

import assert from "node:assert/strict";
import test from "node:test";

import { PersistentGpuCache } from "../gpu-cache/persistent_cache.ts";

class InMemoryFile {
  constructor(private readonly _contents: string) {}

  get size() {
    return new TextEncoder().encode(this._contents).byteLength;
  }

  async text() {
    return this._contents;
  }
}

class InMemoryFileHandle {
  constructor(private readonly _dir: InMemoryDirectoryHandle, private readonly _name: string) {}

  async getFile() {
    return new InMemoryFile(this._dir._files.get(this._name) ?? "");
  }

  async createWritable() {
    const dir = this._dir;
    const name = this._name;
    return {
      write: async (contents: any) => {
        dir._files.set(name, typeof contents === "string" ? contents : String(contents));
      },
      close: async () => {},
    };
  }
}

class InMemoryDirectoryHandle {
  _dirs = new Map<string, InMemoryDirectoryHandle>();
  _files = new Map<string, string>();

  async getDirectoryHandle(name: string, opts?: { create?: boolean }) {
    const existing = this._dirs.get(name);
    if (existing) return existing;
    if (!opts?.create) throw new Error(`Directory not found: ${name}`);
    const dir = new InMemoryDirectoryHandle();
    this._dirs.set(name, dir);
    return dir;
  }

  async getFileHandle(name: string, opts?: { create?: boolean }) {
    if (this._files.has(name)) return new InMemoryFileHandle(this, name);
    if (!opts?.create) throw new Error(`File not found: ${name}`);
    this._files.set(name, "");
    return new InMemoryFileHandle(this, name);
  }

  async removeEntry(name: string, opts?: { recursive?: boolean }) {
    if (this._files.delete(name)) return;
    const dir = this._dirs.get(name);
    if (!dir) throw new Error(`Entry not found: ${name}`);
    if (!opts?.recursive && (dir._dirs.size > 0 || dir._files.size > 0)) {
      throw new Error(`Directory not empty: ${name}`);
    }
    this._dirs.delete(name);
  }
}

test("PersistentGpuCache OPFS: large shader spills to OPFS and metadata stays in IDB", async () => {
  const realNavigatorStorage = (navigator as any).storage;
  const root = new InMemoryDirectoryHandle();
  (navigator as any).storage = {
    getDirectory: async () => root,
  };

  try {
    // Best-effort cleanup.
    try {
      await PersistentGpuCache.clearAll();
    } catch {
      // Ignore.
    }

    const cache = await PersistentGpuCache.open({
      shaderLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
      pipelineLimits: { maxEntries: 16, maxBytes: 8 * 1024 * 1024 },
    });

    try {
      const key = "test-shader-key-opfs-spill";
      const padLine = "// padding ................................................................................\n";
      const wgsl = padLine.repeat(4000) + "@compute @workgroup_size(1) fn main() {}";
      const reflection = { bindings: [] };

      // Sanity: ensure payload is well above the 256KiB OPFS threshold.
      assert.ok(new TextEncoder().encode(wgsl).byteLength > 300 * 1024);

      await cache.putShader(key, { wgsl, reflection });

      // Verify IDB record is metadata-only and points at OPFS.
      const tx = (cache as any)._db.transaction(["shaders"], "readonly");
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
      const got = await cache.getShader(key);
      assert.deepEqual(got, { wgsl, reflection });
    } finally {
      await cache.close();
      try {
        await PersistentGpuCache.clearAll();
      } catch {
        // Ignore.
      }
    }
  } finally {
    (navigator as any).storage = realNavigatorStorage;
  }
});

