import { afterEach, describe, expect, it } from "vitest";

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";

import { RemoteChunkedDisk, type BinaryStore } from "./remote_chunked_disk";
import { OPFS_AERO_DIR, OPFS_DISKS_DIR, OPFS_REMOTE_CACHE_DIR } from "./metadata";
import { RemoteCacheManager } from "./remote_cache_manager";

class TestMemoryStore implements BinaryStore {
  readonly files = new Map<string, Uint8Array<ArrayBuffer>>();

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  return data.buffer instanceof ArrayBuffer ? (data as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(data);
}

class BlockingRemoveStore implements BinaryStore {
  private readonly files = new Map<string, Uint8Array<ArrayBuffer>>();
  private readonly started: Promise<void>;
  private readonly released: Promise<void>;
  private startedResolve: (() => void) | null = null;
  private releasedResolve: (() => void) | null = null;
  private blockRecursiveRemove = false;

  constructor() {
    this.started = new Promise<void>((resolve) => {
      this.startedResolve = resolve;
    });
    this.released = new Promise<void>((resolve) => {
      this.releasedResolve = resolve;
    });
  }

  waitForRecursiveRemove(): Promise<void> {
    return this.started;
  }

  armRecursiveRemoveBlock(): void {
    this.blockRecursiveRemove = true;
  }

  releaseRecursiveRemove(): void {
    if (!this.blockRecursiveRemove) return;
    this.blockRecursiveRemove = false;
    this.releasedResolve?.();
  }

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    if (options.recursive && this.blockRecursiveRemove) {
      this.startedResolve?.();
      await this.released;
    }

    if (options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", toArrayBufferUint8(data));
  const bytes = new Uint8Array(digest);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function buildTestImageBytes(totalSize: number): Uint8Array {
  const bytes = new Uint8Array(totalSize);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = i & 0xff;
  return bytes;
}

async function withServer(handler: (req: IncomingMessage, res: ServerResponse) => void): Promise<{
  baseUrl: string;
  hits: Map<string, number>;
  close: () => Promise<void>;
}> {
  const hits = new Map<string, number>();
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    hits.set(url.pathname, (hits.get(url.pathname) ?? 0) + 1);
    handler(req, res);
  });

  await new Promise<void>((resolve) => server.listen(0, resolve));
  const addr = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${addr.port}`;

  return {
    baseUrl,
    hits,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

describe("RemoteChunkedDisk", () => {
  let closeServer: (() => Promise<void>) | null = null;
  afterEach(async () => {
    if (closeServer) await closeServer();
    closeServer = null;
  });

  it("maps byte offsets to chunk indexes and serves data from cache on repeat reads", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = 2560; // 2 full chunks + 1 half chunk
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, 1024), img.slice(1024, 2048), img.slice(2048, 2560)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: 1024, sha256: await sha256Hex(chunks[0]!) },
        { size: 1024, sha256: await sha256Hex(chunks[1]!) },
        { size: 512, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const manifestUrl1 = `${baseUrl}/manifest.json?sig=aaa`;
    const manifestUrl2 = `${baseUrl}/manifest.json?sig=bbb`;

    const disk = await RemoteChunkedDisk.open(manifestUrl1, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    // Read spanning chunks 0 and 1: offset=512..2048.
    const buf = new Uint8Array(1536);
    await disk.readSectors(1, buf);
    expect(buf).toEqual(img.slice(512, 2048));

    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    const t1 = disk.getTelemetrySnapshot();
    expect(t1.totalSize).toBe(totalSize);
    expect(t1.cachedBytes).toBe(2048);
    expect(t1.blockRequests).toBe(2);
    expect(t1.cacheHits).toBe(0);
    expect(t1.cacheMisses).toBe(2);
    expect(t1.requests).toBe(2);
    expect(t1.bytesDownloaded).toBe(2048);
    expect(t1.lastFetchMs).not.toBeNull();

    // Re-read: should be served from cache with no additional chunk GETs.
    const buf2 = new Uint8Array(1536);
    await disk.readSectors(1, buf2);
    expect(buf2).toEqual(img.slice(512, 2048));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    const t2 = disk.getTelemetrySnapshot();
    expect(t2.cachedBytes).toBe(2048);
    expect(t2.blockRequests).toBe(4);
    expect(t2.cacheHits).toBe(2);
    expect(t2.cacheMisses).toBe(2);
    expect(t2.requests).toBe(2);
    expect(t2.bytesDownloaded).toBe(2048);

    // Cache metadata should not store the signed manifest URL (querystring secrets).
    const metaKey = Array.from(store.files.keys()).find((k) => k.endsWith("/meta.json"));
    expect(metaKey).toBeTruthy();
    const metaRaw = await store.read(metaKey!);
    expect(metaRaw).toBeTruthy();
    const meta = JSON.parse(new TextDecoder().decode(metaRaw!)) as Record<string, unknown>;
    expect(meta.version).toBe(1);
    expect(meta.imageId).toBe("test");
    expect(meta.imageVersion).toBe("v1");
    expect(meta.manifestUrl).toBeUndefined();
    expect(JSON.stringify(meta)).not.toContain("sig=aaa");

    await disk.close();

    // Re-open with the same store: should still hit cache (no extra chunk GETs).
    const disk2 = await RemoteChunkedDisk.open(manifestUrl2, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    const buf3 = new Uint8Array(1024);
    await disk2.readSectors(3, buf3);
    expect(buf3).toEqual(img.slice(1536, 2560));
    expect(hits.get("/chunks/00000001.bin")).toBe(1);
    expect(hits.get("/chunks/00000002.bin")).toBe(1);

    const t3 = disk2.getTelemetrySnapshot();
    expect(t3.totalSize).toBe(totalSize);
    expect(t3.cachedBytes).toBe(totalSize);
    expect(t3.cacheHits).toBe(1);
    expect(t3.cacheMisses).toBe(1);
    expect(t3.requests).toBe(1);
    expect(t3.bytesDownloaded).toBe(512);

    await disk2.close();
  });

  it("evicts least-recently-used cached chunks when the cache limit is exceeded", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = chunkSize * 3;
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize, chunkSize * 2), img.slice(chunkSize * 2)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      cacheLimitBytes: chunkSize * 2,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    // Populate cache with chunks 0 and 1.
    await disk.readSectors(0, new Uint8Array(512));
    await disk.readSectors(2, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);

    // Touch chunk 0 so chunk 1 becomes LRU.
    await disk.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Fetching chunk 2 should evict chunk 1 to stay within limit.
    await disk.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000002.bin")).toBe(1);

    // Chunk 0 should still be cached (no extra fetch).
    await disk.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    // Chunk 1 should have been evicted (re-fetch).
    await disk.readSectors(2, new Uint8Array(512));
    expect(hits.get("/chunks/00000001.bin")).toBe(2);

    await disk.close();
  });

  it("heals cache metadata when a chunk file is missing", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize * 2;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const common = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
      cacheLimitBytes: null,
    };

    const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
    await disk1.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    const cacheKey = await RemoteCacheManager.deriveCacheKey({
      imageId: common.cacheImageId,
      version: common.cacheVersion,
      deliveryType: `chunked:${chunkSize}`,
    });
    const cacheRoot = `${OPFS_AERO_DIR}/${OPFS_DISKS_DIR}/${OPFS_REMOTE_CACHE_DIR}`;
    await store.remove(`${cacheRoot}/${cacheKey}/chunks/0.bin`);

    const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, common);
    await disk2.readSectors(0, new Uint8Array(512));
    // Missing file should trigger a cache miss (refetch).
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    await disk2.close();
  });

  it("enforces cacheLimitBytes on open by evicting older chunks", async () => {
    const chunkSize = 1024;
    const totalSize = chunkSize * 3;
    const chunkCount = 3;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize, chunkSize * 2), img.slice(chunkSize * 2)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: chunkSize, sha256: await sha256Hex(chunks[0]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[1]!) },
        { size: chunkSize, sha256: await sha256Hex(chunks[2]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new TestMemoryStore();
    const stable = {
      store,
      cacheImageId: "img-1",
      cacheVersion: "v1",
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    };

    // First run: cache chunks 0,1,2 in order (2 is MRU).
    const disk1 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: null });
    await disk1.readSectors(0, new Uint8Array(512));
    await disk1.readSectors(2, new Uint8Array(512));
    await disk1.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(hits.get("/chunks/00000001.bin")).toBe(1);
    expect(hits.get("/chunks/00000002.bin")).toBe(1);
    await disk1.close();

    // Re-open with a strict limit: should evict older chunks on open and keep chunk 2.
    const disk2 = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, { ...stable, cacheLimitBytes: chunkSize });
    await disk2.readSectors(4, new Uint8Array(512));
    expect(hits.get("/chunks/00000002.bin")).toBe(1); // cache hit
    await disk2.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2); // evicted => refetch
    await disk2.close();
  });

  it("retries on integrity mismatch and then fails", async () => {
    const chunkSize = 1024;
    const totalSize = 2048;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const goodChunks = [img.slice(0, 1024), img.slice(1024, 2048)];

    // Corrupt chunk 0 on the wire.
    const corrupt0 = goodChunks[0]!.slice();
    corrupt0[0] ^= 0xff;

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [
        { size: 1024, sha256: await sha256Hex(goodChunks[0]!) },
        { size: 1024, sha256: await sha256Hex(goodChunks[1]!) },
      ],
    };

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(corrupt0);
        return;
      }

      if (url.pathname === "/chunks/00000001.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(goodChunks[1]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store: new TestMemoryStore(),
      prefetchSequentialChunks: 0,
      maxAttempts: 2,
      retryBaseDelayMs: 0,
    });

    const buf = new Uint8Array(512);
    await expect(disk.readSectors(0, buf)).rejects.toThrow(/sha256 mismatch/i);
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheMisses).toBe(1);
    expect(t.requests).toBe(2);
    expect(t.bytesDownloaded).toBe(2048);
    expect(t.cachedBytes).toBe(0);
    expect(t.lastFetchMs).toBeNull();
    await disk.close();
  });

  it("does not wipe telemetry for reads that occur while clearCache is in-flight", async () => {
    const chunkSize = 1024; // multiple of 512
    const totalSize = 1024;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, 1024)];

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "test",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
      chunks: [{ size: 1024, sha256: await sha256Hex(chunks[0]!) }],
    };

    const { baseUrl, close } = await withServer((_req, res) => {
      const url = new URL(_req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const idx = Number(m[1]);
        const data = chunks[idx];
        if (!data) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(data);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const store = new BlockingRemoveStore();
    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      store,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });

    store.armRecursiveRemoveBlock();
    const clearPromise = disk.clearCache();
    await store.waitForRecursiveRemove();

    const buf = new Uint8Array(512);
    await disk.readSectors(0, buf);
    expect(buf).toEqual(img.slice(0, 512));

    store.releaseRecursiveRemove();
    await clearPromise;

    const t = disk.getTelemetrySnapshot();
    expect(t.requests).toBe(1);
    expect(t.bytesDownloaded).toBe(1024);

    await disk.close();
  });
});
