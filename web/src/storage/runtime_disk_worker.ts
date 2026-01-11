/// <reference lib="webworker" />
import { OpfsCowDisk } from "./opfs_cow";
import { OpfsRawDisk } from "./opfs_raw";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import type { AsyncSectorDisk } from "./disk";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { benchSequentialRead, benchSequentialWrite } from "./bench";
import type { DiskImageMetadata } from "./metadata";
import { RemoteStreamingDisk, type RemoteDiskOptions, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { RemoteChunkedDisk, type RemoteChunkedDiskOpenOptions } from "./remote_chunked_disk";

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
};

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
    throw new Error("remote disks are not supported by this worker yet");
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

        return { disk: new OpfsCowDisk(base, overlay), readOnly: false, io: emptyIoTelemetry() };
      } catch (err) {
        // If SyncAccessHandle isn't available, sparse overlays can't work efficiently.
        // Fall back to direct raw writes (still in a worker, but slower).
        if (meta.format !== "raw" && meta.format !== "iso" && meta.format !== "unknown") throw err;
      }
    }

    const disk = await openBase();
    return { disk, readOnly, io: emptyIoTelemetry() };
  }

  // IndexedDB backend: disk data is stored in the `chunks` store (sparse).
  const disk = await IdbChunkDisk.open(meta.id, meta.sizeBytes);
  return { disk, readOnly, io: emptyIoTelemetry() };
}

async function openRemoteDisk(url: string, options?: RemoteDiskOptions): Promise<DiskEntry> {
  const disk = await RemoteStreamingDisk.open(url, options);
  return { disk, readOnly: true, io: emptyIoTelemetry() };
}

async function openChunkedDisk(manifestUrl: string, options?: RemoteChunkedDiskOpenOptions): Promise<DiskEntry> {
  const disk = await RemoteChunkedDisk.open(manifestUrl, options);
  return { disk, readOnly: true, io: emptyIoTelemetry() };
}

async function requireDisk(handle: number): Promise<DiskEntry> {
  const entry = disks.get(handle);
  if (!entry) throw new Error(`unknown disk handle ${handle}`);
  return entry;
}

globalThis.onmessage = (ev: MessageEvent<RequestMessage>) => {
  const msg = ev.data;
  if (!msg || msg.type !== "request") return;
  void handleRequest(msg).catch((err) => postErr(msg.requestId, err));
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
