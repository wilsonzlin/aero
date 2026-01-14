import { describe, expect, it, vi } from "vitest";

import { RuntimeDiskWorker, type OpenDiskFn } from "./runtime_disk_worker_impl";
import type { DiskOpenSpec, RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import type { RemoteRangeDiskMetadataStore, RemoteRangeDiskSparseCacheFactory } from "./remote_range_disk";
import { RemoteRangeDisk } from "./remote_range_disk";
import { MemorySparseDisk } from "./memory_sparse_disk";
import { RemoteCacheManager, remoteRangeDeliveryType } from "./remote_cache_manager";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import type { DiskImageMetadata } from "./metadata";
import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

function createRangeFetch(
  data: Uint8Array<ArrayBuffer>,
): { fetch: typeof fetch; calls: Array<{ method: string; range?: string }> } {
  const calls: Array<{ method: string; range?: string }> = [];
  const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
    const buf = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buf).set(bytes);
    return buf;
  };
  const fetcher: typeof fetch = async (_url, init) => {
    const method = String(init?.method || "GET").toUpperCase();
    const headers = init?.headers;
    const rangeHeader = (() => {
      if (headers instanceof Headers) {
        return headers.get("Range") || undefined;
      }
      if (Array.isArray(headers)) {
        const hit = headers.find((h) => h[0].toLowerCase() === "range");
        return hit?.[1];
      }
      if (headers && typeof headers === "object") {
        const rec = headers as Record<string, unknown>;
        const raw = rec.Range ?? rec.range;
        return typeof raw === "string" ? raw : undefined;
      }
      return undefined;
    })();

    calls.push({ method, range: rangeHeader });

    if (method === "HEAD") {
      return new Response(null, { status: 200, headers: { "Content-Length": String(data.byteLength) } });
    }

    if (rangeHeader) {
      const m = /^bytes=(\d+)-(\d+)$/.exec(rangeHeader);
      const suffix = /^bytes=-(\d+)$/.exec(rangeHeader);
      if (!m && !suffix) throw new Error(`invalid Range header: ${rangeHeader}`);
      const start = m ? Number(m[1]) : Math.max(0, data.byteLength - Number(suffix![1]));
      const end = m ? Number(m[2]) : data.byteLength - 1;
      const slice = data.subarray(start, Math.min(end + 1, data.byteLength));
      const body = toArrayBuffer(slice);
      return new Response(body, {
        status: 206,
        headers: {
          "Cache-Control": "no-transform",
          "Content-Range": `bytes ${start}-${start + body.byteLength - 1}/${data.byteLength}`,
        },
      });
    }

    return new Response(toArrayBuffer(data), { status: 200, headers: { "Content-Length": String(data.byteLength) } });
  };

  return { fetch: fetcher, calls };
}

describe("RuntimeDiskWorker (remote)", () => {
  it("opens and reads a remote range disk (read-only)", async () => {
    const base = new Uint8Array(new ArrayBuffer(512 * 8));
    for (let i = 0; i < base.length; i++) base[i] = (i * 13) & 0xff;

    const { fetch: fetcher, calls } = createRangeFetch(base);

    const openDisk: OpenDiskFn = async (spec, mode, overlayBlockSizeBytes) => {
      expect(spec.kind).toBe("remote");
      if (spec.kind !== "remote") {
        throw new Error("expected remote disk spec");
      }
      expect(mode).toBe("cow");
      expect(overlayBlockSizeBytes).toBeUndefined();

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

      const chunkSize = remote.chunkSizeBytes ?? 1024;
      const disk = await RemoteRangeDisk.open(remote.url, {
        cacheKeyParts: {
          imageId: remote.imageId ?? remote.cacheKey,
          version: remote.version ?? "1",
          deliveryType: remoteRangeDeliveryType(chunkSize),
        },
        chunkSize,
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

  it("derives distinct cacheIds for the same image/version when chunkSize differs (regression)", async () => {
    vi.resetModules();

    const derivedCacheIds: string[] = [];
    const openMock = vi.fn(async (_url: string, opts: any) => {
      const cacheId = await RemoteCacheManager.deriveCacheKey(opts.cacheKeyParts);
      derivedCacheIds.push(cacheId);
      // Minimal AsyncSectorDisk stub for the worker open path.
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    const makeSpec = (chunkSizeBytes: number): DiskOpenSpec => ({
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        imageId: "test-image",
        version: "v1",
        chunkSizeBytes,
        cacheBackend: "opfs",
        cacheLimitBytes: null,
      },
    });

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: makeSpec(1024) },
    } satisfies RuntimeDiskRequestMessage);

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "open",
      payload: { spec: makeSpec(2048) },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(2);
    expect(openMock.mock.calls[0]?.[1]?.chunkSize).toBe(1024);
    expect(openMock.mock.calls[0]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(1024));
    expect(openMock.mock.calls[1]?.[1]?.chunkSize).toBe(2048);
    expect(openMock.mock.calls[1]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(2048));
    expect(derivedCacheIds).toHaveLength(2);
    expect(derivedCacheIds[0]).not.toBe(derivedCacheIds[1]);
  });

  it("uses RemoteStreamingDisk for openRemote when cacheLimitBytes is undefined (regression)", async () => {
    vi.resetModules();

    const openRangeMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    const openStreamingMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openRangeMock },
      };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return {
        ...actual,
        RemoteStreamingDisk: { open: openStreamingMock },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: undefined,
          cacheImageId: "test-image",
          cacheVersion: "v1",
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    expect(openStreamingMock).toHaveBeenCalledTimes(1);
    expect(openRangeMock).toHaveBeenCalledTimes(0);
  });

  it("uses RemoteRangeDisk for openRemote only when cacheLimitBytes is null (unbounded cache)", async () => {
    vi.resetModules();

    const openRangeMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    const openStreamingMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openRangeMock },
      };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return {
        ...actual,
        RemoteStreamingDisk: { open: openStreamingMock },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: null,
          cacheImageId: "test-image",
          cacheVersion: "v1",
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    expect(openRangeMock).toHaveBeenCalledTimes(1);
    expect(openStreamingMock).toHaveBeenCalledTimes(0);
  });

  it("uses RemoteRangeDisk only for unbounded OPFS cache (cacheLimitBytes=null) when supported (remote spec)", async () => {
    vi.resetModules();

    const rangeOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });
    const streamingOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return { ...actual, hasOpfsSyncAccessHandle: () => true };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return { ...actual, RemoteRangeDisk: { open: rangeOpenMock } };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return { ...actual, RemoteStreamingDisk: { open: streamingOpenMock } };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        cacheBackend: "opfs",
        cacheLimitBytes: null,
      },
    };

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec } } satisfies RuntimeDiskRequestMessage);

    expect(rangeOpenMock).toHaveBeenCalledTimes(1);
    expect(streamingOpenMock).toHaveBeenCalledTimes(0);
  });

  it("uses RemoteStreamingDisk for bounded caches (remote spec)", async () => {
    vi.resetModules();

    const rangeOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });
    const streamingOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return { ...actual, hasOpfsSyncAccessHandle: () => true };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return { ...actual, RemoteRangeDisk: { open: rangeOpenMock } };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return { ...actual, RemoteStreamingDisk: { open: streamingOpenMock } };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        cacheBackend: "opfs",
        cacheLimitBytes: 128 * 1024 * 1024,
        chunkSizeBytes: 1024,
      },
    };

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec } } satisfies RuntimeDiskRequestMessage);

    expect(rangeOpenMock).toHaveBeenCalledTimes(0);
    expect(streamingOpenMock).toHaveBeenCalledTimes(1);
    expect(streamingOpenMock.mock.calls[0]?.[1]?.cacheBackend).toBe("opfs");
    expect(streamingOpenMock.mock.calls[0]?.[1]?.cacheLimitBytes).toBe(128 * 1024 * 1024);
    expect(streamingOpenMock.mock.calls[0]?.[1]?.blockSize).toBe(1024);
  });

  it("cacheLimitBytes=0 does not touch OPFS for legacy range cache cleanup (remote spec)", async () => {
    vi.resetModules();

    const rangeOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });
    const streamingOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return { ...actual, hasOpfsSyncAccessHandle: () => true };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return { ...actual, RemoteRangeDisk: { open: rangeOpenMock } };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return { ...actual, RemoteStreamingDisk: { open: streamingOpenMock } };
    });

    const { RemoteCacheManager: FreshRemoteCacheManager } = await import("./remote_cache_manager");
    const openOpfsSpy = vi.spyOn(FreshRemoteCacheManager, "openOpfs");

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        cacheBackend: "opfs",
        cacheLimitBytes: 0,
        chunkSizeBytes: 1024,
      },
    };

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec } } satisfies RuntimeDiskRequestMessage);

    expect(rangeOpenMock).toHaveBeenCalledTimes(0);
    expect(streamingOpenMock).toHaveBeenCalledTimes(1);
    expect(openOpfsSpy).not.toHaveBeenCalled();
  });

  it("falls back to RemoteStreamingDisk when OPFS SyncAccessHandle is not available (remote spec)", async () => {
    vi.resetModules();

    const rangeOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });
    const streamingOpenMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return { ...actual, hasOpfsSyncAccessHandle: () => false };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return { ...actual, RemoteRangeDisk: { open: rangeOpenMock } };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return { ...actual, RemoteStreamingDisk: { open: streamingOpenMock } };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        cacheBackend: "opfs",
        cacheLimitBytes: null,
        chunkSizeBytes: 1024,
      },
    };

    await worker.handleMessage({ type: "request", requestId: 1, op: "open", payload: { spec } } satisfies RuntimeDiskRequestMessage);

    expect(rangeOpenMock).toHaveBeenCalledTimes(0);
    expect(streamingOpenMock).toHaveBeenCalledTimes(1);
  });

  it("does not collide caches when opening via openRemote with different blockSize", async () => {
    vi.resetModules();

    const cacheIds: string[] = [];
    const openMock = vi.fn(async (_url: string, opts: any) => {
      cacheIds.push(await RemoteCacheManager.deriveCacheKey(opts.cacheKeyParts));
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: null,
          cacheImageId: "test-image",
          cacheVersion: "v1",
          blockSize: 1024,
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    await worker.handleMessage({
      type: "request",
      requestId: 2,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: null,
          cacheImageId: "test-image",
          cacheVersion: "v1",
          blockSize: 2048,
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(2);
    expect(openMock.mock.calls[0]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(1024));
    expect(openMock.mock.calls[1]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(2048));
    expect(openMock.mock.calls[0]?.[1]?.chunkSize).toBe(1024);
    expect(openMock.mock.calls[1]?.[1]?.chunkSize).toBe(2048);
    expect(cacheIds).toHaveLength(2);
    expect(cacheIds[0]).not.toBe(cacheIds[1]);
  });

  it("best-effort deletes legacy range cache dir keyed as deliveryType='range' on openRemote", async () => {
    vi.resetModules();

    const clearCache = vi.fn(async () => {});
    const openMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    const { RemoteCacheManager: FreshRemoteCacheManager } = await import("./remote_cache_manager");
    const openOpfsSpy = vi.spyOn(FreshRemoteCacheManager, "openOpfs").mockResolvedValue({ clearCache } as unknown as RemoteCacheManager);
    const expectedLegacyCacheKey = await FreshRemoteCacheManager.deriveCacheKey({
      imageId: "test-image",
      version: "v1",
      deliveryType: "range",
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: null,
          cacheImageId: "test-image",
          cacheVersion: "v1",
          blockSize: 1024,
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(1);
    expect(openOpfsSpy).toHaveBeenCalledTimes(1);
    expect(clearCache).toHaveBeenCalledWith(expectedLegacyCacheKey);
  });

  it("best-effort deletes legacy range cache dir keyed as deliveryType='range' on open (remote spec)", async () => {
    vi.resetModules();

    const clearCache = vi.fn(async () => {});
    const openMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    const { RemoteCacheManager: FreshRemoteCacheManager } = await import("./remote_cache_manager");
    const openOpfsSpy = vi.spyOn(FreshRemoteCacheManager, "openOpfs").mockResolvedValue({ clearCache } as unknown as RemoteCacheManager);
    const expectedLegacyCacheKey = await FreshRemoteCacheManager.deriveCacheKey({
      imageId: "test-image",
      version: "v1",
      deliveryType: "range",
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        imageId: "test-image",
        version: "v1",
        chunkSizeBytes: 1024,
        cacheBackend: "opfs",
        cacheLimitBytes: null,
      },
    };

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(1);
    expect(openOpfsSpy).toHaveBeenCalledTimes(1);
    expect(clearCache).toHaveBeenCalledWith(expectedLegacyCacheKey);
  });

  it("uses default RANGE_STREAM_CHUNK_SIZE when opening via openRemote without blockSize", async () => {
    vi.resetModules();

    const openMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "openRemote",
      payload: {
        url: "https://example.invalid/disk.img",
        options: {
          cacheBackend: "opfs",
          cacheLimitBytes: null,
          cacheImageId: "test-image",
          cacheVersion: "v1",
          // blockSize intentionally omitted.
        },
      },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(1);
    expect(openMock.mock.calls[0]?.[1]?.chunkSize).toBe(RANGE_STREAM_CHUNK_SIZE);
    expect(openMock.mock.calls[0]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(RANGE_STREAM_CHUNK_SIZE));
  });

  it("uses default RANGE_STREAM_CHUNK_SIZE when opening a remote range spec without chunkSizeBytes", async () => {
    vi.resetModules();

    const openMock = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: openMock },
      };
    });

    vi.doMock("./metadata", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./metadata")>();
      return {
        ...actual,
        hasOpfsSyncAccessHandle: () => true,
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const worker = new MockedWorker(() => {});

    const spec: DiskOpenSpec = {
      kind: "remote",
      remote: {
        delivery: "range",
        kind: "cd",
        format: "iso",
        url: "https://example.invalid/disk.iso",
        cacheKey: "test.iso.v1",
        imageId: "test-image",
        version: "v1",
        cacheBackend: "opfs",
        cacheLimitBytes: null,
        // chunkSizeBytes intentionally omitted.
      },
    };

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec },
    } satisfies RuntimeDiskRequestMessage);

    expect(openMock).toHaveBeenCalledTimes(1);
    expect(openMock.mock.calls[0]?.[1]?.chunkSize).toBe(RANGE_STREAM_CHUNK_SIZE);
    expect(openMock.mock.calls[0]?.[1]?.cacheKeyParts?.deliveryType).toBe(remoteRangeDeliveryType(RANGE_STREAM_CHUNK_SIZE));
  });

  it("passes cacheLimitBytes to RemoteStreamingDisk when opening a remote range disk from metadata (opfs backend)", async () => {
    vi.resetModules();

    const streamingOpen = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });
    const sparseOpen = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("../platform/remote_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../platform/remote_disk")>();
      return {
        ...actual,
        RemoteStreamingDisk: {
          ...actual.RemoteStreamingDisk,
          open: streamingOpen,
        },
      };
    });

    vi.doMock("./remote_range_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_range_disk")>();
      return {
        ...actual,
        RemoteRangeDisk: { open: sparseOpen },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    const meta: DiskImageMetadata = {
      source: "remote",
      id: "disk1",
      name: "disk1",
      kind: "cd",
      format: "iso",
      sizeBytes: 512,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.img" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
        cacheLimitBytes: 1234,
      },
    };

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta } },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);

    expect(streamingOpen).toHaveBeenCalledTimes(1);
    expect(streamingOpen.mock.calls[0]?.[1]?.cacheBackend).toBe("opfs");
    expect(streamingOpen.mock.calls[0]?.[1]?.cacheLimitBytes).toBe(1234);
    expect(sparseOpen).not.toHaveBeenCalled();
  });

  it("passes cacheLimitBytes to RemoteChunkedDisk when opening a remote chunked disk from metadata", async () => {
    vi.resetModules();

    const chunkedOpen = vi.fn(async (_url: string, _opts: any) => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        async readSectors() {},
        async writeSectors() {
          throw new Error("read-only");
        },
        async flush() {},
      };
    });

    vi.doMock("./remote_chunked_disk", async (importOriginal) => {
      const actual = await importOriginal<typeof import("./remote_chunked_disk")>();
      return {
        ...actual,
        RemoteChunkedDisk: { open: chunkedOpen },
      };
    });

    const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
    const posted: any[] = [];
    const worker = new MockedWorker((msg) => posted.push(msg));

    const meta: DiskImageMetadata = {
      source: "remote",
      id: "disk1",
      name: "disk1",
      kind: "cd",
      format: "iso",
      sizeBytes: 512,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "chunked",
        urls: { url: "https://example.invalid/manifest.json" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "idb",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
        cacheLimitBytes: 999,
      },
    };

    await worker.handleMessage({
      type: "request",
      requestId: 1,
      op: "open",
      payload: { spec: { kind: "local", meta } },
    } satisfies RuntimeDiskRequestMessage);

    const openResp = posted.shift();
    expect(openResp.ok).toBe(true);

    expect(chunkedOpen).toHaveBeenCalledTimes(1);
    expect(chunkedOpen.mock.calls[0]?.[1]?.cacheBackend).toBe("idb");
    expect(chunkedOpen.mock.calls[0]?.[1]?.cacheLimitBytes).toBe(999);
  });

  it("uses RemoteRangeDisk only when cacheLimitBytes is null for remote disk metadata (opfs backend)", async () => {
    vi.resetModules();

    const root = new MemoryDirectoryHandle("root");
    const restoreOpfs = installMemoryOpfs(root).restore;
    try {
      const sparseOpen = vi.fn(async (_url: string, _opts: any) => {
        return {
          sectorSize: 512,
          capacityBytes: 512,
          async readSectors() {},
          async writeSectors() {
            throw new Error("read-only");
          },
          async flush() {},
        };
      });
      const streamingOpen = vi.fn(async (_url: string, _opts: any) => {
        return {
          sectorSize: 512,
          capacityBytes: 512,
          async readSectors() {},
          async writeSectors() {
            throw new Error("read-only");
          },
          async flush() {},
        };
      });

      vi.doMock("./metadata", async (importOriginal) => {
        const actual = await importOriginal<typeof import("./metadata")>();
        return {
          ...actual,
          hasOpfsSyncAccessHandle: () => true,
        };
      });

      vi.doMock("./remote_range_disk", async (importOriginal) => {
        const actual = await importOriginal<typeof import("./remote_range_disk")>();
        return {
          ...actual,
          RemoteRangeDisk: { open: sparseOpen },
        };
      });

      vi.doMock("../platform/remote_disk", async (importOriginal) => {
        const actual = await importOriginal<typeof import("../platform/remote_disk")>();
        return {
          ...actual,
          RemoteStreamingDisk: {
            ...actual.RemoteStreamingDisk,
            open: streamingOpen,
          },
        };
      });

      const { RuntimeDiskWorker: MockedWorker } = await import("./runtime_disk_worker_impl");
      const posted: any[] = [];
      const worker = new MockedWorker((msg) => posted.push(msg));

      const meta: DiskImageMetadata = {
        source: "remote",
        id: "disk1",
        name: "disk1",
        kind: "cd",
        format: "iso",
        sizeBytes: 512,
        createdAtMs: 0,
        remote: {
          imageId: "img1",
          version: "v1",
          delivery: "range",
          urls: { url: "https://example.invalid/disk.img" },
        },
        cache: {
          chunkSizeBytes: 1024,
          backend: "opfs",
          fileName: "cache.aerospar",
          overlayFileName: "overlay.aerospar",
          overlayBlockSizeBytes: 1024,
          cacheLimitBytes: null,
        },
      };

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec: { kind: "local", meta } },
      } satisfies RuntimeDiskRequestMessage);

      const openResp = posted.shift();
      expect(openResp.ok).toBe(true);

      expect(sparseOpen).toHaveBeenCalledTimes(1);
      expect(streamingOpen).not.toHaveBeenCalled();
    } finally {
      restoreOpfs();
    }
  });
});
