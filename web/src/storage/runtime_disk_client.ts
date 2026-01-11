import type { BenchResult } from "./bench";
import type { DiskImageMetadata } from "./metadata";
export type { DiskImageMetadata } from "./metadata";
import type { RemoteDiskOptions, RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import type { RemoteChunkedDiskOpenOptions } from "./remote_chunked_disk";

type OpenMode = "direct" | "cow";

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
      payload: { handle: number; totalBytes: number; chunkBytes?: number; mode?: "read" | "write" | "rw" };
    };

type ResponseMessage =
  | { type: "response"; requestId: number; ok: true; result: any }
  | { type: "response"; requestId: number; ok: false; error: { message: string; name?: string; stack?: string } };

export type OpenResult = {
  handle: number;
  sectorSize: number;
  capacityBytes: number;
  readOnly: boolean;
};

export type DiskIoTelemetry = {
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

export type DiskStats = {
  handle: number;
  sectorSize: number;
  capacityBytes: number;
  readOnly: boolean;
  io: DiskIoTelemetry;
  remote: RemoteDiskTelemetrySnapshot | null;
};

export class RuntimeDiskClient {
  private readonly worker: Worker;
  private nextRequestId = 1;
  private readonly pending = new Map<number, { resolve: (v: any) => void; reject: (e: any) => void }>();

  constructor(worker?: Worker) {
    this.worker =
      worker ??
      new Worker(new URL("./runtime_disk_worker.ts", import.meta.url), {
        type: "module",
      });

    this.worker.onmessage = (event) => {
      const msg = event.data as Partial<ResponseMessage>;
      if (!msg || msg.type !== "response" || typeof msg.requestId !== "number") return;
      const entry = this.pending.get(msg.requestId);
      if (!entry) return;
      this.pending.delete(msg.requestId);
      if (msg.ok) {
        entry.resolve((msg as any).result);
      } else {
        const e = (msg as any).error;
        const err = Object.assign(new Error(e?.message || "runtime disk worker error"), e);
        entry.reject(err);
      }
    };
  }

  close(): void {
    this.worker.terminate();
    this.pending.clear();
  }

  private request<T>(op: RequestMessage["op"], payload: any, transfer?: Transferable[]): Promise<T> {
    const requestId = this.nextRequestId++;
    return new Promise((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject });
      const msg: RequestMessage = { type: "request", requestId, op, payload } as any;
      this.worker.postMessage(msg, transfer ?? []);
    });
  }

  open(meta: DiskImageMetadata, opts: { mode?: OpenMode; overlayBlockSizeBytes?: number } = {}): Promise<OpenResult> {
    return this.request("open", { meta, ...opts });
  }

  openRemote(url: string, options?: RemoteDiskOptions): Promise<OpenResult> {
    return this.request("openRemote", { url, options });
  }

  openChunked(manifestUrl: string, options?: RemoteChunkedDiskOpenOptions): Promise<OpenResult> {
    return this.request("openChunked", { manifestUrl, options });
  }

  read(handle: number, lba: number, byteLength: number): Promise<Uint8Array> {
    return this.request<{ data: Uint8Array }>("read", { handle, lba, byteLength }).then((r) => r.data);
  }

  write(handle: number, lba: number, data: Uint8Array): Promise<void> {
    // Transfer to avoid copying when possible.
    const buf = data.slice();
    return this.request("write", { handle, lba, data: buf }, [buf.buffer]).then(() => undefined);
  }

  flush(handle: number): Promise<void> {
    return this.request("flush", { handle }).then(() => undefined);
  }

  clearCache(handle: number): Promise<void> {
    return this.request("clearCache", { handle }).then(() => undefined);
  }

  closeDisk(handle: number): Promise<void> {
    return this.request("close", { handle }).then(() => undefined);
  }

  stats(handle: number): Promise<DiskStats> {
    return this.request("stats", { handle });
  }

  bench(
    handle: number,
    opts: { totalBytes: number; chunkBytes?: number; mode?: "read" | "write" | "rw" },
  ): Promise<{ read?: BenchResult; write?: BenchResult }> {
    return this.request("bench", { handle, ...opts });
  }
}
