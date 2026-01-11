import "fake-indexeddb/auto";

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
});

