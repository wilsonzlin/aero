import { afterEach, describe, expect, it } from "vitest";

import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

function makeTestImage(size: number): Uint8Array {
  const buf = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) buf[i] = (i * 13) & 0xff;
  return buf;
}

function installMockRangeFetch(data: Uint8Array, opts: { etag: string }): { restore: () => void } {
  const original = globalThis.fetch;

  const headerValue = (init: RequestInit | undefined, name: string): string | null => {
    const h = init?.headers;
    if (!h) return null;
    if (h instanceof Headers) return h.get(name);
    if (Array.isArray(h)) {
      for (const [k, v] of h) {
        if (k.toLowerCase() === name.toLowerCase()) return v;
      }
      return null;
    }
    const rec = h as Record<string, string>;
    for (const [k, v] of Object.entries(rec)) {
      if (k.toLowerCase() === name.toLowerCase()) return v;
    }
    return null;
  };

  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const method = (init?.method ?? "GET").toUpperCase();

    if (method === "HEAD") {
      return new Response(null, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const range = headerValue(init, "Range");
    if (!range) {
      return new Response(data.slice().buffer, {
        status: 200,
        headers: {
          "Content-Length": String(data.byteLength),
          "Accept-Ranges": "bytes",
          ETag: opts.etag,
        },
      });
    }

    const match = /^bytes=(\d+)-(\d+)$/.exec(range);
    if (!match) {
      return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${data.byteLength}` } });
    }

    const start = Number(match[1]);
    const endInclusive = Number(match[2]);
    const body = data.slice(start, endInclusive + 1);

    return new Response(body.buffer, {
      status: 206,
      headers: {
        "Accept-Ranges": "bytes",
        "Cache-Control": "no-transform",
        "Content-Range": `bytes ${start}-${endInclusive}/${data.byteLength}`,
        "Content-Length": String(body.byteLength),
        ETag: opts.etag,
      },
    });
  }) as typeof fetch;

  return { restore: () => (globalThis.fetch = original) };
}

let restoreOpfs: (() => void) | null = null;
let restoreFetch: (() => void) | null = null;

afterEach(() => {
  restoreFetch?.();
  restoreFetch = null;
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("RuntimeDiskWorker legacy remote-streaming local disks", () => {
  it("opens and reads a legacy remote-streaming CD/ISO (read-only)", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const image = makeTestImage(512 * 4);
    restoreFetch = installMockRangeFetch(image, { etag: '"v1"' }).restore;

    const meta: DiskImageMetadata = {
      source: "local",
      id: "legacy-iso",
      name: "remote.iso",
      backend: "opfs",
      kind: "cd",
      format: "iso",
      fileName: "legacy-iso.iso",
      sizeBytes: image.byteLength,
      createdAtMs: Date.now(),
      remote: {
        url: "https://example.test/remote.iso",
        blockSizeBytes: 512,
        cacheLimitBytes: null,
        prefetchSequentialBlocks: 0,
      },
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "cow" },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    expect(openResp.result.readOnly).toBe(true);
    const handle = openResp.result.handle as number;

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "read",
      payload: { handle, lba: 0, byteLength: 512 * 2 },
    } satisfies RuntimeDiskRequestMessage);

    const readResp = posted.shift();
    expect(readResp.ok).toBe(true);
    expect(Array.from(readResp.result.data as Uint8Array)).toEqual(Array.from(image.subarray(0, 512 * 2)));

    // Writes are rejected by the worker for read-only disks.
    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "write",
      payload: { handle, lba: 0, data: new Uint8Array(512) },
    } satisfies RuntimeDiskRequestMessage);

    const writeResp = posted.shift();
    expect(writeResp.ok).toBe(false);
    expect(String(writeResp.error.message)).toMatch(/read-only/i);
  });

  it("opens a legacy remote-streaming HDD in raw mode as read-only (remote base)", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const image = makeTestImage(512 * 4);
    restoreFetch = installMockRangeFetch(image, { etag: '"v1"' }).restore;

    const meta: DiskImageMetadata = {
      source: "local",
      id: "legacy-hdd-raw",
      name: "remote.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy-hdd-raw.img",
      sizeBytes: image.byteLength,
      createdAtMs: Date.now(),
      remote: {
        url: "https://example.test/remote.img",
        blockSizeBytes: 512,
        cacheLimitBytes: null,
        prefetchSequentialBlocks: 0,
      },
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "direct" },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    expect(openResp.result.readOnly).toBe(true);
    const handle = openResp.result.handle as number;

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "write",
      payload: { handle, lba: 0, data: new Uint8Array(512) },
    } satisfies RuntimeDiskRequestMessage);

    const writeResp = posted.shift();
    expect(writeResp.ok).toBe(false);
    expect(String(writeResp.error.message)).toMatch(/read-only/i);
  });

  it("uses a runtime COW overlay for a legacy remote-streaming HDD (writes go to overlay)", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    const image = makeTestImage(512 * 4);
    restoreFetch = installMockRangeFetch(image, { etag: '"v1"' }).restore;

    const meta: DiskImageMetadata = {
      source: "local",
      id: "legacy-hdd-cow",
      name: "remote.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy-hdd-cow.img",
      sizeBytes: image.byteLength,
      createdAtMs: Date.now(),
      remote: {
        url: "https://example.test/remote.img",
        blockSizeBytes: 512,
        cacheLimitBytes: null,
        prefetchSequentialBlocks: 0,
      },
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta }, mode: "cow", overlayBlockSizeBytes: 512 },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    expect(openResp.result.readOnly).toBe(false);
    const handle = openResp.result.handle as number;

    // Initial read comes from the remote base.
    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "read",
      payload: { handle, lba: 0, byteLength: 512 },
    } satisfies RuntimeDiskRequestMessage);
    const beforeWrite = posted.shift();
    expect(beforeWrite.ok).toBe(true);
    expect(Array.from(beforeWrite.result.data as Uint8Array)).toEqual(Array.from(image.subarray(0, 512)));

    const writeData = new Uint8Array(512).fill(0x5a);
    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "write",
      payload: { handle, lba: 0, data: writeData },
    } satisfies RuntimeDiskRequestMessage);
    expect(posted.shift().ok).toBe(true);

    await worker.handleMessage({
      type: "request",
      requestId: 4,
      op: "read",
      payload: { handle, lba: 0, byteLength: 512 },
    } satisfies RuntimeDiskRequestMessage);
    const afterWrite = posted.shift();
    expect(afterWrite.ok).toBe(true);
    expect(Array.from(afterWrite.result.data as Uint8Array)).toEqual(Array.from(writeData));
  });
});
