/// <reference lib="webworker" />
import { OpfsCowDisk } from "./opfs_cow";
import { OpfsRawDisk } from "./opfs_raw";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import type { AsyncSectorDisk } from "./disk";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { benchSequentialRead, benchSequentialWrite } from "./bench";

type DiskBackend = "opfs" | "idb";
type DiskKind = "hdd" | "cd";
type DiskFormat = "raw" | "iso" | "qcow2" | "unknown";

type DiskImageMetadata = {
  id: string;
  backend: DiskBackend;
  kind: DiskKind;
  format: DiskFormat;
  fileName: string;
  sizeBytes: number;
};

type OpenMode = "direct" | "cow";

type DiskEntry = {
  disk: AsyncSectorDisk;
  readOnly: boolean;
};

type RequestMessage =
  | {
      type: "request";
      requestId: number;
      op: "open";
      payload: { meta: DiskImageMetadata; mode?: OpenMode; overlayBlockSizeBytes?: number };
    }
  | { type: "request"; requestId: number; op: "close"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "flush"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "read"; payload: { handle: number; lba: number; byteLength: number } }
  | { type: "request"; requestId: number; op: "write"; payload: { handle: number; lba: number; data: Uint8Array } }
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

async function openDisk(meta: DiskImageMetadata, mode: OpenMode, overlayBlockSizeBytes?: number): Promise<DiskEntry> {
  const readOnly = meta.kind === "cd" || meta.format === "iso";

  if (meta.backend === "opfs") {
    // OPFS stores images as raw files. For HDD images we default to a COW overlay so
    // the imported base image remains unchanged.
    if (mode === "cow" && !readOnly) {
      try {
        const base = await OpfsRawDisk.open(meta.fileName, { create: false, sizeBytes: meta.sizeBytes });
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

        return { disk: new OpfsCowDisk(base, overlay), readOnly: false };
      } catch {
        // If SyncAccessHandle isn't available, sparse overlays can't work efficiently.
        // Fall back to direct raw writes (still in a worker, but slower).
      }
    }

    const disk = await OpfsRawDisk.open(meta.fileName, { create: false, sizeBytes: meta.sizeBytes });
    return { disk, readOnly };
  }

  // IndexedDB backend: disk data is stored in the `chunks` store (sparse).
  const disk = await IdbChunkDisk.open(meta.id, meta.sizeBytes);
  return { disk, readOnly };
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
      if (entry.readOnly) {
        postOk(msg.requestId, { ok: true });
        return;
      }
      await entry.disk.flush();
      postOk(msg.requestId, { ok: true });
      return;
    }

    case "read": {
      const { handle, lba, byteLength } = msg.payload;
      const entry = await requireDisk(handle);
      const buf = new Uint8Array(byteLength);
      await entry.disk.readSectors(lba, buf);
      // Transfer the ArrayBuffer to avoid copying on postMessage.
      postOk(msg.requestId, { data: buf }, [buf.buffer]);
      return;
    }

    case "write": {
      const { handle, lba, data } = msg.payload;
      const entry = await requireDisk(handle);
      if (entry.readOnly) throw new Error("disk is read-only");
      await entry.disk.writeSectors(lba, data);
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
