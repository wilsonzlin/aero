/// <reference lib="webworker" />
import { OpfsCowDisk } from "./opfs_cow";
import { OpfsRawDisk } from "./opfs_raw";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import type { AsyncSectorDisk } from "./disk";
import { IdbCowDisk } from "./idb_cow";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { benchSequentialRead, benchSequentialWrite } from "./bench";
import type { DiskImageMetadata } from "./metadata";
import { RemoteStreamingDisk, type RemoteDiskOptions, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { RemoteChunkedDisk, type RemoteChunkedDiskOpenOptions } from "./remote_chunked_disk";
import { opfsDeleteDisk, opfsGetDiskFileHandle } from "./import_export";
import { RemoteRangeDisk, defaultRemoteRangeUrl } from "./remote_range_disk";
import {
  deserializeRuntimeDiskSnapshot,
  serializeRuntimeDiskSnapshot,
  shouldInvalidateRemoteCache,
  type DiskBackendSnapshot,
  type RemoteCacheBinding,
  type RuntimeDiskSnapshot,
  type RuntimeDiskSnapshotEntry,
} from "./runtime_disk_snapshot";

type OpenMode = "direct" | "cow";

type DiskIoTelemetry = {
  reads: number;
  bytesRead: number;
  writes: number;
  bytesWritten: number;
  flushes: number;
  inflightReads: number;
  inflightWrites: number;
  inflightFlushes: number;
  lastReadMs: number | null;
  lastWriteMs: number | null;
  lastFlushMs: number | null;
};

type DiskEntry = {
  disk: AsyncSectorDisk;
  readOnly: boolean;
  io: DiskIoTelemetry;
  backendSnapshot: DiskBackendSnapshot | null;
};

function defaultRemoteChunkedManifestUrl(base: RemoteCacheBinding["base"]): string {
  // See: docs/18-chunked-disk-image-format.md ("images/<imageId>/<version>/manifest.json").
  // Like `defaultRemoteRangeUrl`, this is intentionally *not* a signed URL.
  return `/images/${encodeURIComponent(base.imageId)}/${encodeURIComponent(base.version)}/manifest.json`;
}

type RequestMessage =
  | {
      type: "request";
      requestId: number;
      op: "open";
      payload: { meta: DiskImageMetadata; mode?: OpenMode; overlayBlockSizeBytes?: number };
    }
  | {
      type: "request";
      requestId: number;
      op: "openRemote";
      payload: { url: string; options?: RemoteDiskOptions };
    }
  | {
      type: "request";
      requestId: number;
      op: "openChunked";
      payload: { manifestUrl: string; options?: RemoteChunkedDiskOpenOptions };
    }
  | { type: "request"; requestId: number; op: "close"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "flush"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "clearCache"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "read"; payload: { handle: number; lba: number; byteLength: number } }
  | { type: "request"; requestId: number; op: "write"; payload: { handle: number; lba: number; data: Uint8Array } }
  | { type: "request"; requestId: number; op: "stats"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "prepareSnapshot"; payload: Record<string, never> }
  | { type: "request"; requestId: number; op: "restoreFromSnapshot"; payload: { state: Uint8Array } }
  | {
      type: "request";
      requestId: number;
      op: "bench";
      payload: {
        handle: number;
        totalBytes: number;
        chunkBytes?: number;
        mode?: "read" | "write" | "rw";
      };
    };

type ResponseMessage =
  | { type: "response"; requestId: number; ok: true; result: unknown }
  | { type: "response"; requestId: number; ok: false; error: { message: string; name?: string; stack?: string } };

const disks = new Map<number, DiskEntry>();
let nextHandle = 1;

function cacheBindingFileName(cacheFileName: string): string {
  return `${cacheFileName}.binding.json`;
}

async function readCacheBinding(fileName: string): Promise<RemoteCacheBinding | null> {
  try {
    const handle = await opfsGetDiskFileHandle(fileName, { create: false });
    const file = await handle.getFile();
    const text = await file.text();
    if (!text.trim()) return null;
    return JSON.parse(text) as RemoteCacheBinding;
  } catch (err) {
    if (err instanceof DOMException && err.name === "NotFoundError") return null;
    return null;
  }
}

async function writeCacheBinding(fileName: string, binding: RemoteCacheBinding): Promise<void> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  await writable.write(JSON.stringify(binding, null, 2));
  await writable.close();
}

async function ensureRemoteCacheBinding(expected: RemoteCacheBinding["base"], cacheFileName: string): Promise<void> {
  const bindingName = cacheBindingFileName(cacheFileName);
  const existing = await readCacheBinding(bindingName);
  if (shouldInvalidateRemoteCache(expected, existing)) {
    await opfsDeleteDisk(cacheFileName);
    await opfsDeleteDisk(bindingName);
  }
  await writeCacheBinding(bindingName, { version: 1, base: expected });
}

function serializeError(err: unknown): { message: string; name?: string; stack?: string } {
  if (err instanceof Error) return { message: err.message, name: err.name, stack: err.stack };
  return { message: String(err) };
}

function postOk(requestId: number, result: unknown, transfer?: Transferable[]): void {
  const msg: ResponseMessage = { type: "response", requestId, ok: true, result };
  (globalThis as DedicatedWorkerGlobalScope).postMessage(msg, transfer ?? []);
}

function postErr(requestId: number, err: unknown): void {
  const msg: ResponseMessage = { type: "response", requestId, ok: false, error: serializeError(err) };
  (globalThis as DedicatedWorkerGlobalScope).postMessage(msg);
}

function emptyIoTelemetry(): DiskIoTelemetry {
  return {
    reads: 0,
    bytesRead: 0,
    writes: 0,
    bytesWritten: 0,
    flushes: 0,
    inflightReads: 0,
    inflightWrites: 0,
    inflightFlushes: 0,
    lastReadMs: null,
    lastWriteMs: null,
    lastFlushMs: null,
  };
}

async function openDisk(meta: DiskImageMetadata, mode: OpenMode, overlayBlockSizeBytes?: number): Promise<DiskEntry> {
  if (meta.source === "remote") {
    if (meta.cache.backend === "opfs" && meta.remote.delivery !== "range") {
      throw new Error(`unsupported remote delivery ${meta.remote.delivery} for OPFS cache backend (expected range)`);
    }

    const expectedValidator = meta.remote.validator?.etag
      ? { kind: "etag" as const, value: meta.remote.validator.etag }
      : meta.remote.validator?.lastModified
        ? { kind: "lastModified" as const, value: meta.remote.validator.lastModified }
        : undefined;

    const backend: DiskBackendSnapshot = {
      kind: "remote",
      backend: meta.cache.backend,
      diskKind: meta.kind,
      sizeBytes: meta.sizeBytes,
      base: {
        imageId: meta.remote.imageId,
        version: meta.remote.version,
        deliveryType: meta.remote.delivery,
        ...(expectedValidator ? { expectedValidator } : {}),
        chunkSize: meta.cache.chunkSizeBytes,
      },
      overlay: {
        fileName: meta.cache.overlayFileName,
        diskSizeBytes: meta.sizeBytes,
        blockSizeBytes: meta.cache.overlayBlockSizeBytes,
      },
      cache: { fileName: meta.cache.fileName },
    };

    const readOnly = meta.kind === "cd" || meta.format === "iso";
    return await openDiskFromSnapshot({
      handle: 0,
      readOnly,
      sectorSize: 512,
      capacityBytes: meta.sizeBytes,
      backend,
    });
  }
  const readOnly = meta.kind === "cd" || meta.format === "iso";

  if (meta.backend === "opfs") {
    async function openBase(): Promise<AsyncSectorDisk> {
      switch (meta.format) {
        case "aerospar": {
          const disk = await OpfsAeroSparseDisk.open(meta.fileName);
          if (disk.capacityBytes !== meta.sizeBytes) {
            await disk.close?.();
            throw new Error(`disk size mismatch: expected=${meta.sizeBytes} actual=${disk.capacityBytes}`);
          }
          return disk;
        }
        case "raw":
        case "iso":
        case "unknown":
          return await OpfsRawDisk.open(meta.fileName, { create: false, sizeBytes: meta.sizeBytes });
        case "qcow2":
        case "vhd":
          throw new Error(`unsupported OPFS disk format ${meta.format} (convert to aerospar first)`);
      }
    }

    // For HDD images we default to a COW overlay so the imported base image remains unchanged.
    if (mode === "cow" && !readOnly) {
      try {
        const base = await openBase();
        const overlayName = `${meta.id}.overlay.aerospar`;

        let overlay: OpfsAeroSparseDisk;
        try {
          overlay = await OpfsAeroSparseDisk.open(overlayName);
        } catch {
          overlay = await OpfsAeroSparseDisk.create(overlayName, {
            diskSizeBytes: meta.sizeBytes,
            blockSizeBytes: overlayBlockSizeBytes ?? 1024 * 1024,
          });
        }

        return {
          disk: new OpfsCowDisk(base, overlay),
          readOnly: false,
          io: emptyIoTelemetry(),
          backendSnapshot: {
            kind: "local",
            backend: "opfs",
            key: meta.fileName,
            format: meta.format,
            diskKind: meta.kind,
            sizeBytes: meta.sizeBytes,
            overlay: {
              fileName: overlayName,
              diskSizeBytes: meta.sizeBytes,
              blockSizeBytes: overlay.blockSizeBytes,
            },
          },
        };
      } catch (err) {
        // If SyncAccessHandle isn't available, sparse overlays can't work efficiently.
        // Fall back to direct raw writes (still in a worker, but slower).
        if (meta.format !== "raw" && meta.format !== "iso" && meta.format !== "unknown") throw err;
      }
    }

    const disk = await openBase();
    return {
      disk,
      readOnly,
      io: emptyIoTelemetry(),
      backendSnapshot: {
        kind: "local",
        backend: "opfs",
        key: meta.fileName,
        format: meta.format,
        diskKind: meta.kind,
        sizeBytes: meta.sizeBytes,
      },
    };
  }

  // IndexedDB backend: disk data is stored in the `chunks` store (sparse).
  const disk = await IdbChunkDisk.open(meta.id, meta.sizeBytes);
  return {
    disk,
    readOnly,
    io: emptyIoTelemetry(),
    backendSnapshot: {
      kind: "local",
      backend: "idb",
      key: meta.id,
      format: meta.format,
      diskKind: meta.kind,
      sizeBytes: meta.sizeBytes,
    },
  };
}

async function openRemoteDisk(url: string, options?: RemoteDiskOptions): Promise<DiskEntry> {
  const disk = await RemoteStreamingDisk.open(url, options);
  return { disk, readOnly: true, io: emptyIoTelemetry(), backendSnapshot: null };
}

async function openChunkedDisk(manifestUrl: string, options?: RemoteChunkedDiskOpenOptions): Promise<DiskEntry> {
  const disk = await RemoteChunkedDisk.open(manifestUrl, options);
  return { disk, readOnly: true, io: emptyIoTelemetry(), backendSnapshot: null };
}

async function requireDisk(handle: number): Promise<DiskEntry> {
  const entry = disks.get(handle);
  if (!entry) throw new Error(`unknown disk handle ${handle}`);
  return entry;
}

async function openSparseOrCreate(
  fileName: string,
  opts: { diskSizeBytes: number; blockSizeBytes: number },
): Promise<OpfsAeroSparseDisk> {
  try {
    const disk = await OpfsAeroSparseDisk.open(fileName);
    if (disk.capacityBytes !== opts.diskSizeBytes) {
      await disk.close?.();
      throw new Error(`disk size mismatch: expected=${opts.diskSizeBytes} actual=${disk.capacityBytes}`);
    }
    if (disk.blockSizeBytes !== opts.blockSizeBytes) {
      await disk.close?.();
      throw new Error(`block size mismatch: expected=${opts.blockSizeBytes} actual=${disk.blockSizeBytes}`);
    }
    return disk;
  } catch {
    return await OpfsAeroSparseDisk.create(fileName, opts);
  }
}

async function openDiskFromSnapshot(entry: RuntimeDiskSnapshotEntry): Promise<DiskEntry> {
  const backend = entry.backend;
  if (backend.kind === "local") {
    if (backend.backend === "opfs") {
      let base: AsyncSectorDisk;
      switch (backend.format) {
        case "aerospar": {
          const disk = await OpfsAeroSparseDisk.open(backend.key);
          if (disk.capacityBytes !== backend.sizeBytes) {
            await disk.close?.();
            throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${disk.capacityBytes}`);
          }
          base = disk;
          break;
        }
        case "raw":
        case "iso":
        case "unknown":
          base = await OpfsRawDisk.open(backend.key, { create: false, sizeBytes: backend.sizeBytes });
          break;
        case "qcow2":
        case "vhd":
          throw new Error(`unsupported OPFS disk format ${backend.format} (convert to aerospar first)`);
      }

      if (backend.overlay && !entry.readOnly) {
        const overlay = await openSparseOrCreate(backend.overlay.fileName, {
          diskSizeBytes: backend.overlay.diskSizeBytes,
          blockSizeBytes: backend.overlay.blockSizeBytes,
        });
        return {
          disk: new OpfsCowDisk(base, overlay),
          readOnly: entry.readOnly,
          io: emptyIoTelemetry(),
          backendSnapshot: backend,
        };
      }

      return { disk: base, readOnly: entry.readOnly, io: emptyIoTelemetry(), backendSnapshot: backend };
    }

    const disk = await IdbChunkDisk.open(backend.key, backend.sizeBytes);
    return { disk, readOnly: entry.readOnly, io: emptyIoTelemetry(), backendSnapshot: backend };
  }

  // Remote base image with OPFS cache + overlay.
  const remoteCacheBackend = backend.backend ?? "opfs";
  if (remoteCacheBackend !== "opfs" && remoteCacheBackend !== "idb") {
    throw new Error(`unsupported remote cache backend ${String(remoteCacheBackend)}`);
  }
  if (remoteCacheBackend === "opfs") {
    await ensureRemoteCacheBinding(backend.base, backend.cache.fileName);
  }

  if (backend.base.deliveryType !== "range" && backend.base.deliveryType !== "chunked") {
    throw new Error(`unsupported remote deliveryType=${backend.base.deliveryType}`);
  }

  let base: AsyncSectorDisk;
  if (remoteCacheBackend === "opfs") {
    if (backend.base.deliveryType !== "range") {
      throw new Error(`unsupported remote deliveryType=${backend.base.deliveryType} for OPFS cache backend`);
    }

    const url = defaultRemoteRangeUrl(backend.base);
    const imageKey = `${backend.base.imageId}:${backend.base.version}:${backend.base.deliveryType}`;
    const sparseCacheFactory = {
      open: async (_cacheId: string) => await OpfsAeroSparseDisk.open(backend.cache.fileName),
      create: async (_cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }) =>
        await OpfsAeroSparseDisk.create(backend.cache.fileName, opts),
      delete: async (_cacheId: string) => {
        await opfsDeleteDisk(backend.cache.fileName);
      },
    };
    base = await RemoteRangeDisk.open(url, { imageKey, chunkSize: backend.base.chunkSize, sparseCacheFactory });
    if (base.capacityBytes !== backend.sizeBytes) {
      await base.close?.();
      throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${base.capacityBytes}`);
    }
  } else {
    if (backend.base.deliveryType === "range") {
      const expectedEtag =
        backend.base.expectedValidator?.kind === "etag" ? backend.base.expectedValidator.value : undefined;
      const url = defaultRemoteRangeUrl(backend.base);
      base = await RemoteStreamingDisk.open(url, {
        blockSize: backend.base.chunkSize,
        cacheBackend: remoteCacheBackend,
        credentials: "same-origin",
        cacheImageId: backend.base.imageId,
        cacheVersion: backend.base.version,
        cacheEtag: expectedEtag,
        expectedSizeBytes: backend.sizeBytes,
      });
    } else {
      const manifestUrl = defaultRemoteChunkedManifestUrl(backend.base);
      base = await RemoteChunkedDisk.open(manifestUrl, { cacheBackend: remoteCacheBackend, credentials: "same-origin" });
      if (base.capacityBytes !== backend.sizeBytes) {
        await base.close?.();
        throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${base.capacityBytes}`);
      }
    }
  }

  if (entry.readOnly) {
    return {
      disk: base,
      readOnly: entry.readOnly,
      io: emptyIoTelemetry(),
      backendSnapshot: backend,
    };
  }

  if (remoteCacheBackend === "idb") {
    const disk = await IdbCowDisk.open(base, backend.overlay.fileName, backend.sizeBytes);
    return {
      disk,
      readOnly: entry.readOnly,
      io: emptyIoTelemetry(),
      backendSnapshot: backend,
    };
  }

  const overlay = await openSparseOrCreate(backend.overlay.fileName, {
    diskSizeBytes: backend.overlay.diskSizeBytes,
    blockSizeBytes: backend.overlay.blockSizeBytes,
  });

  return {
    disk: new OpfsCowDisk(base, overlay),
    readOnly: entry.readOnly,
    io: emptyIoTelemetry(),
    backendSnapshot: backend,
  };
}

// Serialize all worker requests to avoid races between in-flight disk I/O and snapshot/restore.
// This keeps snapshot semantics simple: `prepareSnapshot()` will only run after all previous
// reads/writes have completed and no new ones will start until it finishes.
let requestChain: Promise<void> = Promise.resolve();

globalThis.onmessage = (ev: MessageEvent<RequestMessage>) => {
  const msg = ev.data;
  if (!msg || msg.type !== "request") return;
  requestChain = requestChain.then(async () => {
    try {
      await handleRequest(msg);
    } catch (err) {
      postErr(msg.requestId, err);
    }
  });
};

async function handleRequest(msg: RequestMessage): Promise<void> {
  switch (msg.op) {
    case "open": {
      const { meta, mode, overlayBlockSizeBytes } = msg.payload;
      const entry = await openDisk(meta, mode ?? "cow", overlayBlockSizeBytes);
      const handle = nextHandle++;
      disks.set(handle, entry);
      postOk(msg.requestId, {
        handle,
        sectorSize: entry.disk.sectorSize,
        capacityBytes: entry.disk.capacityBytes,
        readOnly: entry.readOnly,
      });
      return;
    }

    case "openRemote": {
      const { url, options } = msg.payload;
      const entry = await openRemoteDisk(url, options);
      const handle = nextHandle++;
      disks.set(handle, entry);
      postOk(msg.requestId, {
        handle,
        sectorSize: entry.disk.sectorSize,
        capacityBytes: entry.disk.capacityBytes,
        readOnly: entry.readOnly,
      });
      return;
    }

    case "openChunked": {
      const { manifestUrl, options } = msg.payload;
      const entry = await openChunkedDisk(manifestUrl, options);
      const handle = nextHandle++;
      disks.set(handle, entry);
      postOk(msg.requestId, {
        handle,
        sectorSize: entry.disk.sectorSize,
        capacityBytes: entry.disk.capacityBytes,
        readOnly: entry.readOnly,
      });
      return;
    }

    case "close": {
      const { handle } = msg.payload;
      const entry = await requireDisk(handle);
      await entry.disk.close?.();
      disks.delete(handle);
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "flush": {
      const { handle } = msg.payload;
      const entry = await requireDisk(handle);
      const start = performance.now();
      entry.io.flushes++;
      entry.io.inflightFlushes++;
      try {
        await entry.disk.flush();
      } finally {
        entry.io.inflightFlushes--;
        entry.io.lastFlushMs = performance.now() - start;
      }
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "clearCache": {
      const { handle } = msg.payload;
      const entry = await requireDisk(handle);
      const diskAny = entry.disk as unknown as { clearCache?: () => Promise<void> };
      if (typeof diskAny.clearCache !== "function") {
        throw new Error("disk does not support cache clearing");
      }
      await diskAny.clearCache();
      entry.io = emptyIoTelemetry();
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "read": {
      const { handle, lba, byteLength } = msg.payload;
      const entry = await requireDisk(handle);
      const buf = new Uint8Array(byteLength);
      const start = performance.now();
      entry.io.reads++;
      entry.io.bytesRead += byteLength;
      entry.io.inflightReads++;
      try {
        await entry.disk.readSectors(lba, buf);
      } finally {
        entry.io.inflightReads--;
        entry.io.lastReadMs = performance.now() - start;
      }
      // Transfer the ArrayBuffer to avoid copying on postMessage.
      postOk(msg.requestId, { data: buf }, [buf.buffer]);
      return;
    }

    case "write": {
      const { handle, lba, data } = msg.payload;
      const entry = await requireDisk(handle);
      if (entry.readOnly) throw new Error("disk is read-only");
      const start = performance.now();
      entry.io.writes++;
      entry.io.bytesWritten += data.byteLength;
      entry.io.inflightWrites++;
      try {
        await entry.disk.writeSectors(lba, data);
      } finally {
        entry.io.inflightWrites--;
        entry.io.lastWriteMs = performance.now() - start;
      }
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "stats": {
      const { handle } = msg.payload;
      const entry = await requireDisk(handle);
      const diskAny = entry.disk as unknown as { getTelemetrySnapshot?: () => RemoteDiskTelemetrySnapshot };
      const remote = typeof diskAny.getTelemetrySnapshot === "function" ? diskAny.getTelemetrySnapshot() : null;
      postOk(msg.requestId, {
        handle,
        sectorSize: entry.disk.sectorSize,
        capacityBytes: entry.disk.capacityBytes,
        readOnly: entry.readOnly,
        io: entry.io,
        remote,
      });
      return;
    }

    case "prepareSnapshot": {
      for (const entry of disks.values()) {
        await entry.disk.flush();
        const backend = entry.backendSnapshot;
        if (!backend) {
          throw new Error("disk backend does not support snapshotting (missing backend descriptor)");
        }
        if (backend.kind === "remote") {
          if ((backend.backend ?? "opfs") === "opfs") {
            await writeCacheBinding(cacheBindingFileName(backend.cache.fileName), { version: 1, base: backend.base });
          }
        }
      }

      const ordered = Array.from(disks.entries()).sort(([a], [b]) => a - b);
      const disksSnapshot = ordered.map(([handle, entry]) => {
        const backend = entry.backendSnapshot;
        if (!backend) {
          throw new Error("disk backend does not support snapshotting (missing backend descriptor)");
        }
        return {
          handle,
          readOnly: entry.readOnly,
          sectorSize: entry.disk.sectorSize,
          capacityBytes: entry.disk.capacityBytes,
          backend,
        };
      });
      const snapshot: RuntimeDiskSnapshot = {
        version: 1,
        nextHandle,
        disks: disksSnapshot,
      };
      const state = serializeRuntimeDiskSnapshot(snapshot);
      postOk(msg.requestId, { state }, [state.buffer]);
      return;
    }

    case "restoreFromSnapshot": {
      const snapshot = deserializeRuntimeDiskSnapshot(msg.payload.state);

      for (const entry of disks.values()) {
        await entry.disk.close?.();
      }
      disks.clear();

      const opened = new Map<number, DiskEntry>();
      const maxHandle = snapshot.disks.reduce((max, d) => Math.max(max, d.handle), 0);
      const desiredNextHandle = Math.max(snapshot.nextHandle, maxHandle + 1);
      try {
        for (const diskEntry of snapshot.disks) {
          const entry = await openDiskFromSnapshot(diskEntry);
          opened.set(diskEntry.handle, entry);
        }
      } catch (err) {
        for (const entry of opened.values()) {
          await entry.disk.close?.();
        }
        throw err;
      }

      nextHandle = desiredNextHandle;
      for (const [handle, entry] of opened.entries()) {
        disks.set(handle, entry);
      }
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "bench": {
      const { handle, totalBytes, chunkBytes, mode } = msg.payload;
      const entry = await requireDisk(handle);

      const selected = mode ?? "rw";
      const results: Record<string, unknown> = {};

      if (selected === "write" || selected === "rw") {
        results.write = await benchSequentialWrite(entry.disk, { totalBytes, chunkBytes });
      }
      if (selected === "read" || selected === "rw") {
        results.read = await benchSequentialRead(entry.disk, { totalBytes, chunkBytes });
      }

      postOk(msg.requestId, results);
      return;
    }
  }
}
