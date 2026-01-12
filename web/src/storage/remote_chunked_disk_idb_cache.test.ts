import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";

import { clearIdb } from "./metadata";
import { RemoteChunkedDisk } from "./remote_chunked_disk";

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

describe("RemoteChunkedDisk (IndexedDB cache)", () => {
  let closeServer: (() => Promise<void>) | null = null;

  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    if (closeServer) await closeServer();
    closeServer = null;
    await clearIdb();
  });

  it("persists cached chunks across re-open and invalidates when manifest version changes", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize * 2;
    const chunkCount = 2;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, chunkSize), img.slice(chunkSize, totalSize)];

    let manifestVersion = "v1";
    let manifestEtag = '"m1"';

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", manifestEtag);
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: manifestVersion,
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
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

    const manifestUrl = `${baseUrl}/manifest.json`;

    const disk1 = await RemoteChunkedDisk.open(manifestUrl, {
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      cacheLimitBytes: null,
    });
    expect(disk1.getTelemetrySnapshot().cacheLimitBytes).toBeNull();

    const buf = new Uint8Array(512);
    await disk1.readSectors(0, buf);
    expect(buf).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    const disk2 = await RemoteChunkedDisk.open(manifestUrl, {
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      cacheLimitBytes: null,
    });
    const buf2 = new Uint8Array(512);
    await disk2.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // Re-open should hit the persistent IDB cache (no extra chunk fetch).
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk2.close();

    // Change the manifest version+etag; cache binding should invalidate and re-fetch.
    manifestVersion = "v2";
    manifestEtag = '"m2"';

    const disk3 = await RemoteChunkedDisk.open(manifestUrl, {
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      cacheLimitBytes: null,
    });
    const buf3 = new Uint8Array(512);
    await disk3.readSectors(0, buf3);
    expect(buf3).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    await disk3.close();
  });

  it("disables persistent caching entirely when cacheLimitBytes is 0", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, totalSize)];

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", '"m1"');
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: "v1",
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const manifestUrl = `${baseUrl}/manifest.json`;
    const openOpts = {
      cacheBackend: "idb" as const,
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    };

    const disk1 = await RemoteChunkedDisk.open(manifestUrl, openOpts);
    expect(disk1.getTelemetrySnapshot().cachedBytes).toBe(0);

    const buf1 = new Uint8Array(512);
    await disk1.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    expect(disk1.getTelemetrySnapshot().cachedBytes).toBe(0);

    // Repeat read should not use any cache (always re-fetch).
    const buf2 = new Uint8Array(512);
    await disk1.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);
    expect(disk1.getTelemetrySnapshot().cachedBytes).toBe(0);

    await disk1.close();

    // Re-open should not persist/consult a cache.
    const disk2 = await RemoteChunkedDisk.open(manifestUrl, openOpts);
    const buf3 = new Uint8Array(512);
    await disk2.readSectors(0, buf3);
    expect(buf3).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(3);
    expect(disk2.getTelemetrySnapshot().cachedBytes).toBe(0);
    await disk2.close();
  });

  it("reuses the cache across signed manifest URLs when imageId+version is stable", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunks = [img.slice(0, totalSize)];

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", '"m1"');
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: "v1",
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunks[0]);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const manifestUrl1 = `${baseUrl}/manifest.json?sig=aaa`;
    const manifestUrl2 = `${baseUrl}/manifest.json?sig=bbb`;

    const disk1 = await RemoteChunkedDisk.open(manifestUrl1, {
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      cacheLimitBytes: null,
    });
    const buf1 = new Uint8Array(512);
    await disk1.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    const disk2 = await RemoteChunkedDisk.open(manifestUrl2, {
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      cacheLimitBytes: null,
    });
    const buf2 = new Uint8Array(512);
    await disk2.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // Querystring changes should not create a new cache entry (avoid signed URL secrets).
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk2.close();
  });

  it("disables caching when cacheLimitBytes is 0 (no persistent or in-memory cache hits)", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", '"m1"');
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: "v1",
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      cacheBackend: "idb",
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    expect(disk.getTelemetrySnapshot().cacheLimitBytes).toBe(0);

    const buf1 = new Uint8Array(512);
    await disk.readSectors(0, buf1);
    expect(buf1).toEqual(img.slice(0, 512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    const buf2 = new Uint8Array(512);
    await disk.readSectors(0, buf2);
    expect(buf2).toEqual(img.slice(0, 512));
    // With cache disabled, this must re-fetch the chunk from the network.
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheLimitBytes).toBe(0);
    expect(t.cachedBytes).toBe(0);
    expect(t.cacheHits).toBe(0);

    await disk.close();
  });

  it("ignores previously persisted chunks when cacheLimitBytes is 0", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", '"m1"');
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: "v1",
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(chunk0);
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const manifestUrl = `${baseUrl}/manifest.json`;

    // Prime the persistent cache.
    const disk1 = await RemoteChunkedDisk.open(manifestUrl, {
      cacheBackend: "idb",
      cacheLimitBytes: null,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    await disk1.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(1);
    await disk1.close();

    // Re-open with caching disabled: should still fetch from network (ignore IDB).
    const disk2 = await RemoteChunkedDisk.open(manifestUrl, {
      cacheBackend: "idb",
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
    });
    await disk2.readSectors(0, new Uint8Array(512));
    expect(hits.get("/chunks/00000000.bin")).toBe(2);

    const t = disk2.getTelemetrySnapshot();
    expect(t.cacheLimitBytes).toBe(0);
    expect(t.cachedBytes).toBe(0);
    expect(t.cacheHits).toBe(0);

    await disk2.close();
  });

  it("still de-dupes concurrent in-flight reads when cacheLimitBytes is 0", async () => {
    const chunkSize = 512 * 1024;
    const totalSize = chunkSize;
    const chunkCount = 1;

    const img = buildTestImageBytes(totalSize);
    const chunk0 = img.slice(0, chunkSize);

    // Hold the chunk response open so the second read overlaps the first and
    // must join the in-flight request (not issue a second network fetch).
    const pendingChunkResponses: ServerResponse[] = [];
    let chunkRequestStartedResolve: (() => void) | null = null;
    const chunkRequestStarted = new Promise<void>((resolve) => {
      chunkRequestStartedResolve = resolve;
    });
    // Use a definite assignment assertion so TypeScript understands that the Promise
    // executor sets the resolver synchronously.
    let releaseChunkResponsesResolve!: () => void;
    const releaseChunkResponses = new Promise<void>((resolve) => {
      releaseChunkResponsesResolve = resolve;
    });

    const { baseUrl, hits, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/manifest.json") {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.setHeader("etag", '"m1"');
        res.end(
          JSON.stringify({
            schema: "aero.chunked-disk-image.v1",
            imageId: "test",
            version: "v1",
            mimeType: "application/octet-stream",
            totalSize,
            chunkSize,
            chunkCount,
            chunkIndexWidth: 8,
          }),
        );
        return;
      }

      if (url.pathname === "/chunks/00000000.bin") {
        pendingChunkResponses.push(res);
        if (pendingChunkResponses.length === 1) {
          chunkRequestStartedResolve?.();
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        void releaseChunkResponses.then(() => {
          // Best-effort: only end the response once.
          if (!res.writableEnded) res.end(chunk0);
        });
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    closeServer = close;

    const disk = await RemoteChunkedDisk.open(`${baseUrl}/manifest.json`, {
      cacheBackend: "idb",
      cacheLimitBytes: 0,
      prefetchSequentialChunks: 0,
      retryBaseDelayMs: 0,
      maxConcurrentFetches: 1,
    });

    const buf1 = new Uint8Array(512);
    const p1 = disk.readSectors(0, buf1);
    await chunkRequestStarted;

    const buf2 = new Uint8Array(512);
    const p2 = disk.readSectors(0, buf2);

    // Release all pending chunk responses (there should only be one request).
    releaseChunkResponsesResolve();
    await Promise.all([p1, p2]);

    expect(buf1).toEqual(img.slice(0, 512));
    expect(buf2).toEqual(img.slice(0, 512));

    // Only one chunk GET should occur because the second read joins the in-flight fetch.
    expect(hits.get("/chunks/00000000.bin")).toBe(1);

    const t = disk.getTelemetrySnapshot();
    expect(t.cacheLimitBytes).toBe(0);
    expect(t.cachedBytes).toBe(0);
    expect(t.cacheHits).toBe(0);
    expect(t.inflightJoins).toBe(1);

    await disk.close();
  });
});
