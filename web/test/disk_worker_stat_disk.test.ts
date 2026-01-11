import test from "node:test";
import assert from "node:assert/strict";

import "./fake_indexeddb_auto.ts";
import { installOpfsMock } from "./opfs_mock.ts";
import { clearIdb, idbTxDone, openDiskManagerDb, opfsGetDisksDir, opfsGetRemoteCacheDir } from "../src/storage/metadata.ts";
import { RemoteCacheManager, remoteChunkedDeliveryType, remoteRangeDeliveryType } from "../src/storage/remote_cache_manager.ts";

// The disk worker expects to run in a DedicatedWorkerGlobalScope where `self` and `postMessage` exist.
// In unit tests we run it in-process and drive its `onmessage` handler directly.
(globalThis as unknown as { self?: unknown }).self = globalThis;

type Pending = { resolve: (v: unknown) => void; reject: (e: unknown) => void };
const pending = new Map<number, Pending>();
let nextRequestId = 1;

// Capture messages posted by the worker.
(globalThis as unknown as { postMessage?: (msg: any) => void }).postMessage = (msg: any) => {
  if (!msg || msg.type !== "response" || typeof msg.requestId !== "number") return;
  const entry = pending.get(msg.requestId);
  if (!entry) return;
  pending.delete(msg.requestId);
  if (msg.ok) entry.resolve(msg.result);
  else entry.reject(Object.assign(new Error(msg.error?.message || "disk_worker error"), msg.error));
};

// Load the worker module after globals are in place.
await import("../src/storage/disk_worker.ts");

async function requestDiskWorker<T>(backend: "opfs" | "idb", op: string, payload: any): Promise<T> {
  const requestId = nextRequestId++;
  return new Promise<T>((resolve, reject) => {
    pending.set(requestId, { resolve, reject });
    const onmessage = (globalThis as unknown as { onmessage?: (e: any) => void }).onmessage;
    if (!onmessage) {
      pending.delete(requestId);
      reject(new Error("disk_worker did not register onmessage"));
      return;
    }
    onmessage({ data: { type: "request", requestId, backend, op, payload } });
  });
}

async function writeOpfsFile(fileName: string, sizeBytes: number): Promise<void> {
  const dir = await opfsGetDisksDir();
  const handle = await dir.getFileHandle(fileName, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  await writable.write(new Uint8Array(sizeBytes));
  await writable.close();
}

async function writeOpfsTextFile(fileName: string, text: string): Promise<void> {
  const dir = await opfsGetDisksDir();
  const handle = await dir.getFileHandle(fileName, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  await writable.write(text);
  await writable.close();
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

async function stableCacheId(key: string): Promise<string> {
  try {
    const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
    if (!subtle) throw new Error("missing crypto.subtle");
    const digest = await subtle.digest("SHA-256", new TextEncoder().encode(key));
    return bytesToHex(new Uint8Array(digest));
  } catch {
    return encodeURIComponent(key).replaceAll("%", "_").slice(0, 128);
  }
}

test("disk_worker stat_disk", async (t) => {
  await t.test("remote OPFS range: includes overlay + modern caches + legacy artifacts", async () => {
    installOpfsMock();
    await clearIdb();

    const meta = await requestDiskWorker<any>("opfs", "create_remote", {
      name: "remote",
      imageId: "img",
      version: "v1",
      delivery: "range",
      urls: { url: "https://example.com/disk.img" },
      sizeBytes: 4096,
      // Keep sizes small in tests, but still aligned.
      chunkSizeBytes: 1024,
      overlayBlockSizeBytes: 1024,
      cacheFileName: "disk.cache.aerospar",
      overlayFileName: "disk.overlay.aerospar",
    });

    // Overlay file (user state).
    await writeOpfsFile(meta.cache.overlayFileName, 100);

    // Legacy per-disk cache file.
    await writeOpfsFile(meta.cache.fileName, 50);

    // Legacy RemoteRangeDisk cache keyed by remote base identity.
    const imageKey = `${meta.remote.imageId}:${meta.remote.version}:${meta.remote.delivery}`;
    const cacheId = await stableCacheId(imageKey);
    await writeOpfsFile(`remote-range-cache-${cacheId}.aerospar`, 20);
    await writeOpfsTextFile(`remote-range-cache-${cacheId}.json`, "0123456789"); // 10 bytes

    // Modern RemoteStreamingDisk OPFS cache (OpfsLruChunkCache) under RemoteCacheManager cache key.
    const remoteCacheDir = await opfsGetRemoteCacheDir();
    const deliveryTypes = [remoteRangeDeliveryType(meta.cache.chunkSizeBytes), "range"];
    const [modernKey, legacyKey] = await Promise.all(
      deliveryTypes.map((deliveryType) =>
        RemoteCacheManager.deriveCacheKey({
          imageId: meta.remote.imageId,
          version: meta.remote.version,
          deliveryType,
        }),
      ),
    );

    // 1) Modern key: valid index.json => counted via index parsing (30 bytes).
    {
      const cacheDir = await remoteCacheDir.getDirectoryHandle(modernKey, { create: true });
      const indexHandle = await cacheDir.getFileHandle("index.json", { create: true });
      const writable = await indexHandle.createWritable({ keepExistingData: false });
      await writable.write(
        JSON.stringify({
          version: 1,
          chunkSize: meta.cache.chunkSizeBytes,
          accessCounter: 1,
          chunks: {
            "0": { byteLength: 10, lastAccess: 1 },
            "1": { byteLength: 20, lastAccess: 1 },
          },
        }),
      );
      await writable.close();
    }

    // 2) Legacy key: corrupt index.json => counted via scanning chunks/*.bin (40 bytes).
    {
      const cacheDir = await remoteCacheDir.getDirectoryHandle(legacyKey, { create: true });
      const indexHandle = await cacheDir.getFileHandle("index.json", { create: true });
      const writable = await indexHandle.createWritable({ keepExistingData: false });
      await writable.write("not-json");
      await writable.close();

      const chunksDir = await cacheDir.getDirectoryHandle("chunks", { create: true });
      const c0 = await chunksDir.getFileHandle("0.bin", { create: true });
      const w0 = await c0.createWritable({ keepExistingData: false });
      await w0.write(new Uint8Array(40));
      await w0.close();
    }

    const stat = await requestDiskWorker<any>("opfs", "stat_disk", { id: meta.id });

    // overlay: 100
    // caches: 30 (index.json) + 40 (chunks scan) + 50 (meta.cache.fileName) + 20 + 10 (remote-range-cache files)
    assert.equal(stat.actualSizeBytes, 100 + (30 + 40 + 50 + 20 + 10));
  });

  await t.test("local IDB: reports allocated chunk bytes (sparse disk)", async () => {
    installOpfsMock();
    await clearIdb();

    const meta = await requestDiskWorker<any>("idb", "create_blank", {
      name: "blank",
      sizeBytes: 1024 * 1024,
      kind: "hdd",
      format: "raw",
    });

    const db = await openDiskManagerDb();
    try {
      const tx = db.transaction(["chunks"], "readwrite");
      const store = tx.objectStore("chunks");
      store.put({ id: meta.id, index: 0, data: new Uint8Array(10).buffer });
      store.put({ id: meta.id, index: 1, data: new Uint8Array(5).buffer });
      await idbTxDone(tx);
    } finally {
      db.close();
    }

    const stat = await requestDiskWorker<any>("idb", "stat_disk", { id: meta.id });
    assert.equal(stat.actualSizeBytes, 10 + 5);
  });

  await t.test("remote IDB chunked: includes overlay + remote_chunk_meta bytesUsed", async () => {
    installOpfsMock();
    await clearIdb();

    const meta = await requestDiskWorker<any>("opfs", "create_remote", {
      name: "remote-idb",
      imageId: "img",
      version: "v2",
      delivery: "chunked",
      urls: { url: "https://example.com/manifest.json" },
      sizeBytes: 4096,
      cacheBackend: "idb",
      // Must be within the IDB remote-chunk bounds.
      chunkSizeBytes: 512 * 1024,
      overlayBlockSizeBytes: 512 * 1024,
      cacheFileName: "idb.cache",
      overlayFileName: "idb.overlay",
    });

    const db = await openDiskManagerDb();
    try {
      const derivedKey = await RemoteCacheManager.deriveCacheKey({
        imageId: meta.remote.imageId,
        version: meta.remote.version,
        deliveryType: remoteChunkedDeliveryType(meta.cache.chunkSizeBytes),
      });
      const legacyKey = await RemoteCacheManager.deriveCacheKey({
        imageId: meta.remote.imageId,
        version: meta.remote.version,
        deliveryType: "chunked",
      });

      const tx = db.transaction(["chunks", "remote_chunk_meta"], "readwrite");
      const chunks = tx.objectStore("chunks");
      const metaStore = tx.objectStore("remote_chunk_meta");

      // Overlay bytes stored in the `chunks` store (sparse; only allocated chunks exist).
      chunks.put({ id: meta.cache.overlayFileName, index: 0, data: new Uint8Array(10).buffer });
      chunks.put({ id: meta.cache.overlayFileName, index: 1, data: new Uint8Array(5).buffer });

      // Legacy per-disk cache bytes stored in `chunks`.
      chunks.put({ id: meta.cache.fileName, index: 0, data: new Uint8Array(7).buffer });

      // Remote chunk caches tracked via `remote_chunk_meta.bytesUsed`.
      metaStore.put({ cacheKey: derivedKey, bytesUsed: 40 });
      metaStore.put({ cacheKey: legacyKey, bytesUsed: 10 });
      metaStore.put({ cacheKey: meta.cache.fileName, bytesUsed: 3 });

      await idbTxDone(tx);
    } finally {
      db.close();
    }

    const stat = await requestDiskWorker<any>("opfs", "stat_disk", { id: meta.id });

    // Overlay chunks: 10 + 5
    // Legacy cache chunks: 7
    // Remote cache meta bytesUsed: 40 + 10 + 3
    assert.equal(stat.actualSizeBytes, (10 + 5) + 7 + (40 + 10 + 3));
  });
});
