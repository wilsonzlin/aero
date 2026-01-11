import { describe, expect, it } from "vitest";

import { RuntimeDiskWorker, type OpenDiskFn } from "./runtime_disk_worker_impl";
import type { DiskOpenSpec, RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import type { RemoteRangeDiskMetadataStore, RemoteRangeDiskSparseCacheFactory } from "./remote_range_disk";
import { RemoteRangeDisk } from "./remote_range_disk";
import { MemorySparseDisk } from "./memory_sparse_disk";

function createRangeFetch(data: Uint8Array): { fetch: typeof fetch; calls: Array<{ method: string; range?: string }> } {
  const calls: Array<{ method: string; range?: string }> = [];
  const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
    const buf = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buf).set(bytes);
    return buf;
  };
  const fetcher: typeof fetch = async (_url, init) => {
    const method = String(init?.method || "GET").toUpperCase();
    const headers = init?.headers;
    const rangeHeader =
      headers instanceof Headers
        ? headers.get("Range") || undefined
        : typeof headers === "object" && headers
          ? ((headers as any).Range as string | undefined) ?? ((headers as any).range as string | undefined)
          : undefined;

    calls.push({ method, range: rangeHeader });

    if (method === "HEAD") {
      return new Response(null, { status: 200, headers: { "Content-Length": String(data.byteLength) } });
    }

    if (rangeHeader) {
      const m = /^bytes=(\d+)-(\d+)$/.exec(rangeHeader);
      if (!m) throw new Error(`invalid Range header: ${rangeHeader}`);
      const start = Number(m[1]);
      const end = Number(m[2]);
      const slice = data.subarray(start, Math.min(end + 1, data.byteLength));
      return new Response(toArrayBuffer(slice), {
        status: 206,
        headers: { "Content-Range": `bytes ${start}-${start + slice.byteLength - 1}/${data.byteLength}` },
      });
    }

    return new Response(toArrayBuffer(data), { status: 200, headers: { "Content-Length": String(data.byteLength) } });
  };

  return { fetch: fetcher, calls };
}

describe("RuntimeDiskWorker (remote)", () => {
  it("opens and reads a remote range disk (read-only)", async () => {
    const base = new Uint8Array(512 * 8);
    for (let i = 0; i < base.length; i++) base[i] = (i * 13) & 0xff;

    const { fetch: fetcher, calls } = createRangeFetch(base);

    const openDisk: OpenDiskFn = async (spec, mode, overlayBlockSizeBytes) => {
      expect(spec.kind).toBe("remote");
      expect(mode).toBe("cow");
      expect(overlayBlockSizeBytes).toBeUndefined();

      if (spec.kind !== "remote") {
        throw new Error("expected remote disk open spec");
      }
      const remote = spec.remote;
      if (remote.delivery !== "range") {
        throw new Error("expected range remote disk spec");
      }
      const caches = new Map<string, MemorySparseDisk>();
      const sparseCacheFactory: RemoteRangeDiskSparseCacheFactory = {
        async open(cacheId) {
          const hit = caches.get(cacheId);
          if (!hit) throw new Error("cache not found");
          return hit;
        },
        async create(cacheId, opts) {
          const disk = MemorySparseDisk.create({ diskSizeBytes: opts.diskSizeBytes, blockSizeBytes: opts.blockSizeBytes });
          caches.set(cacheId, disk);
          return disk;
        },
        async delete(cacheId) {
          caches.delete(cacheId);
        },
      };
      const metaMap = new Map<string, any>();
      const metadataStore: RemoteRangeDiskMetadataStore = {
        async read(cacheId) {
          return metaMap.get(cacheId) ?? null;
        },
        async write(cacheId, meta) {
          metaMap.set(cacheId, meta);
        },
        async delete(cacheId) {
          metaMap.delete(cacheId);
        },
      };

      const disk = await RemoteRangeDisk.open(remote.url, {
        cacheKeyParts: { imageId: remote.imageId ?? remote.cacheKey, version: remote.version ?? "1", deliveryType: remote.delivery },
        chunkSize: remote.chunkSizeBytes ?? 1024,
        fetchFn: fetcher,
        metadataStore,
        sparseCacheFactory,
        readAheadChunks: 0,
      });

      // Treat as read-only regardless of requested mode; this is the remote ISO path.
      return { disk, readOnly: true, backendSnapshot: null };
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
      },
    };

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);
    const handle = openResp.result.handle as number;

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "read",
      payload: { handle, lba: 0, byteLength: 512 * 2 },
    } satisfies RuntimeDiskRequestMessage);

    const readResp = posted.shift();
    expect(readResp.ok).toBe(true);
    expect(Array.from(readResp.result.data as Uint8Array)).toEqual(Array.from(base.subarray(0, 512 * 2)));

    // Writes fail deterministically for read-only disks.
    await worker.handleMessage({
      type: "request",
      requestId: 3,
      op: "write",
      payload: { handle, lba: 0, data: new Uint8Array(512) },
    } satisfies RuntimeDiskRequestMessage);

    const writeResp = posted.shift();
    expect(writeResp.ok).toBe(false);
    expect(String(writeResp.error.message)).toMatch(/read-only/);

    // Should have performed at least one Range fetch (plus one HEAD probe).
    expect(calls.some((c) => c.method === "GET" && typeof c.range === "string")).toBe(true);
  });
});
