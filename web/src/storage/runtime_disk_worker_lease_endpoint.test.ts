// @vitest-environment node
import "../../test/fake_indexeddb_auto.ts";

import { afterEach, describe, expect, it } from "vitest";

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";

import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { DiskOpenSpec, RuntimeDiskRequestMessage } from "./runtime_disk_protocol";

function buildTestBytes(totalSize: number): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(new ArrayBuffer(totalSize));
  for (let i = 0; i < buf.length; i += 1) buf[i] = i & 0xff;
  return buf;
}

async function withServer(handler: (req: IncomingMessage, res: ServerResponse) => void): Promise<{
  baseUrl: string;
  close: () => Promise<void>;
}> {
  const server = createServer((req, res) => {
    res.setHeader("cache-control", "no-transform");
    handler(req, res);
  });
  await new Promise<void>((resolve) => server.listen(0, resolve));
  const addr = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${addr.port}`;
  return {
    baseUrl,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

describe("RuntimeDiskWorker (leaseEndpoint)", () => {
  let closeServer: (() => Promise<void>) | null = null;
  afterEach(async () => {
    if (closeServer) await closeServer();
    closeServer = null;
  });

  it(
    "refreshes chunked manifest leases on 403 and does not persist signed URLs in snapshots",
    async () => {
      const chunkSize = 512 * 1024; // must be within IDB bounds (see idb_remote_chunk_cache.ts)
      const chunkCount = 2;
      const totalSize = chunkSize * chunkCount;
      const bytes = buildTestBytes(totalSize);

    let currentToken = "t1";
    let baseUrl = "";

    const leaseCalls: string[] = [];
    const chunkTokens = new Map<string, string[]>();

    const manifest = {
      schema: "aero.chunked-disk-image.v1",
      imageId: "img1",
      version: "v1",
      mimeType: "application/octet-stream",
      totalSize,
      chunkSize,
      chunkCount,
      chunkIndexWidth: 8,
    };

    const { baseUrl: startedBaseUrl, close } = await withServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      const token = url.searchParams.get("token") ?? "";

      if (url.pathname === "/lease") {
        leaseCalls.push("lease");
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(
          JSON.stringify({
            url: `${baseUrl}/manifest.json?token=${currentToken}`,
            chunked: {
              delivery: "chunked",
              manifestUrl: `${baseUrl}/manifest.json?token=${currentToken}`,
            },
          }),
        );
        return;
      }

      if (url.pathname === "/manifest.json") {
        if (token !== currentToken) {
          res.statusCode = 403;
          res.end("forbidden");
          return;
        }
        res.statusCode = 200;
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(manifest));
        return;
      }

      const m = url.pathname.match(/^\/chunks\/(\d+)\.bin$/);
      if (m) {
        const name = m[1]!;
        const idx = Number(name);

        const arr = chunkTokens.get(url.pathname) ?? [];
        arr.push(token);
        chunkTokens.set(url.pathname, arr);

        if (token !== currentToken) {
          res.statusCode = 403;
          res.end("forbidden");
          return;
        }

        const start = idx * chunkSize;
        const end = start + chunkSize;
        if (start < 0 || end > bytes.length) {
          res.statusCode = 404;
          res.end("missing");
          return;
        }

        res.statusCode = 200;
        res.setHeader("content-type", "application/octet-stream");
        res.end(Buffer.from(bytes.subarray(start, end)));
        return;
      }

      res.statusCode = 404;
      res.end("not found");
    });
    baseUrl = startedBaseUrl;
    closeServer = close;

    const globals = globalThis as unknown as { location?: unknown };
    const oldLocation = globals.location;
    globals.location = { href: `${baseUrl}/` };
    try {
      const meta: DiskImageMetadata = {
        source: "remote",
        id: "disk1",
        name: "disk1",
        kind: "cd",
        format: "iso",
        sizeBytes: totalSize,
        createdAtMs: 0,
        remote: {
          imageId: "img1",
          version: "v1",
          delivery: "chunked",
          urls: { leaseEndpoint: "/lease" },
        },
        cache: {
          chunkSizeBytes: chunkSize,
          backend: "idb",
          fileName: "cache.aerospar",
          overlayFileName: "overlay.aerospar",
          overlayBlockSizeBytes: 1024,
        },
      };

      const posted: any[] = [];
      const worker = new RuntimeDiskWorker((msg) => posted.push(msg));
      const spec: DiskOpenSpec = { kind: "local", meta };

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec },
      } satisfies RuntimeDiskRequestMessage);

      const openResp = posted.shift();
      if (!openResp.ok) {
        throw new Error(String(openResp.error?.message ?? "open failed"));
      }
      const handle = openResp.result.handle as number;

      // Read from chunk 0 to populate cache.
      await worker.handleMessage({
        type: "request",
        requestId: 2,
        op: "read",
        payload: { handle, lba: 0, byteLength: 512 },
      } satisfies RuntimeDiskRequestMessage);

      const read0 = posted.shift();
      expect(read0.ok).toBe(true);
      expect(Array.from(read0.result.data as Uint8Array)).toEqual(Array.from(bytes.subarray(0, 512)));

      // Rotate token (simulates signed URL expiry + new lease).
      currentToken = "t2";

      // Read from an uncached chunk; should trigger a second /lease call and use the new token.
      await worker.handleMessage({
        type: "request",
        requestId: 3,
        op: "read",
        payload: { handle, lba: chunkSize / 512, byteLength: 512 },
      } satisfies RuntimeDiskRequestMessage);

      const read1 = posted.shift();
      expect(read1.ok).toBe(true);
      expect(Array.from(read1.result.data as Uint8Array)).toEqual(Array.from(bytes.subarray(chunkSize, chunkSize + 512)));

      expect(leaseCalls.length).toBe(2);
      const seen = chunkTokens.get("/chunks/00000001.bin") ?? [];
      expect(seen.length).toBeGreaterThanOrEqual(2);
      expect(seen[seen.length - 1]).toBe("t2");

      await worker.handleMessage({
        type: "request",
        requestId: 4,
        op: "prepareSnapshot",
        payload: {},
      } satisfies RuntimeDiskRequestMessage);

      const snapResp = posted.shift();
      expect(snapResp.ok).toBe(true);
      const state = snapResp.result.state as Uint8Array;
      const json = new TextDecoder().decode(state);
      expect(json).not.toContain("token=");

      await worker.handleMessage({
        type: "request",
        requestId: 5,
        op: "close",
        payload: { handle },
      } satisfies RuntimeDiskRequestMessage);
      const closeResp = posted.shift();
      expect(closeResp.ok).toBe(true);
      } finally {
        globals.location = oldLocation;
      }
    },
    // This test can be sensitive to Vitest's file-level parallelism (other suites may
    // be running CPU-intensive Vite builds), so keep the timeout a bit higher than
    // the default 5s to avoid flakes.
    20_000,
  );
});
