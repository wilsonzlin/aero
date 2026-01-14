import type { RemoteDiskOptions } from "../platform/remote_disk";
import type { RemoteChunkedDiskOpenOptions } from "./remote_chunked_disk";
import type { DiskImageMetadata, DiskKind } from "./metadata";

export type OpenMode = "direct" | "cow";

export type RemoteDiskDelivery = "range" | "chunked";

export type RemoteDiskIntegritySpec =
  | { kind: "manifest"; manifestUrl: string }
  | { kind: "sha256"; sha256: string[] };

type RemoteDiskOpenBase = {
  delivery: RemoteDiskDelivery;
  kind: DiskKind;
  format: "raw" | "iso";

  /**
   * Fetch credentials mode. Prefer `same-origin` when possible to avoid
   * accidentally sending cookies cross-site.
   */
  credentials?: RequestCredentials;

  /**
   * Stable identifier used ONLY for local cache/overlay file naming.
   *
   * This must NOT be a signed URL. It should remain stable across sessions and
   * should change when the underlying remote image bytes change (e.g. include a
   * version or content hash).
   */
  cacheKey: string;

  /**
   * Persistent cache backend selection.
   *
   * - `undefined` (default): use the runtime default backend.
   * - `"opfs"`: store cache data in Origin Private File System (best when available).
   * - `"idb"`: store cache data in IndexedDB.
   */
  cacheBackend?: "opfs" | "idb";

  /**
   * Persistent cache size limit (LRU-evicted for streaming/chunked backends).
   *
   * - `undefined` (default): use the default limit (currently 512 MiB)
   * - `null`: disable eviction (unbounded cache; subject to browser storage quota)
   * - `0`: disable caching entirely (no OPFS/IDB usage; always fetch via the network)
   */
  cacheLimitBytes?: number | null;

  /**
   * Optional stable identifiers carried for observability/debugging.
   * These must also be non-secret.
   */
  imageId?: string;
  version?: string;

  /**
   * Optional integrity data for remote blocks/chunks.
   *
   * If present, remote reads will verify SHA-256 before committing bytes to the
   * local cache.
   */
  integrity?: RemoteDiskIntegritySpec;
};

export type RemoteRangeDiskOpenSpec = RemoteDiskOpenBase & {
  delivery: "range";
  url: string;

  /**
   * The aligned fetch size for `Range` reads.
   *
   * Defaults to 1 MiB.
   */
  chunkSizeBytes?: number;
};

export type RemoteChunkedDiskOpenSpec = RemoteDiskOpenBase & {
  delivery: "chunked";
  manifestUrl: string;
};

export type RemoteDiskOpenSpec = RemoteRangeDiskOpenSpec | RemoteChunkedDiskOpenSpec;

export type DiskOpenSpec =
  | { kind: "local"; meta: DiskImageMetadata }
  | { kind: "remote"; remote: RemoteDiskOpenSpec };

export type OpenRequestPayload = {
  spec: DiskOpenSpec;
  mode?: OpenMode;
  overlayBlockSizeBytes?: number;
};

export type SharedArrayBufferSlice = {
  sab: SharedArrayBuffer;
  offsetBytes: number;
};

export type SharedArrayBufferRange = SharedArrayBufferSlice & {
  byteLength: number;
};

export type RuntimeDiskRequestMessage =
  | { type: "request"; requestId: number; op: "open"; payload: OpenRequestPayload }
  | { type: "request"; requestId: number; op: "openRemote"; payload: { url: string; options?: RemoteDiskOptions } }
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
  | {
      type: "request";
      requestId: number;
      op: "readInto";
      payload: { handle: number; lba: number; byteLength: number; dest: SharedArrayBufferSlice };
    }
  | { type: "request"; requestId: number; op: "write"; payload: { handle: number; lba: number; data: Uint8Array } }
  | {
      type: "request";
      requestId: number;
      op: "writeFrom";
      payload: { handle: number; lba: number; src: SharedArrayBufferRange };
    }
  | { type: "request"; requestId: number; op: "stats"; payload: { handle: number } }
  | { type: "request"; requestId: number; op: "prepareSnapshot"; payload: Record<string, never> }
  | { type: "request"; requestId: number; op: "restoreFromSnapshot"; payload: { state: Uint8Array } }
  | {
      type: "request";
      requestId: number;
      op: "bench";
      payload: { handle: number; totalBytes: number; chunkBytes?: number; mode?: "read" | "write" | "rw" };
    };

export type RuntimeDiskResponseMessage =
  | { type: "response"; requestId: number; ok: true; result: unknown }
  | { type: "response"; requestId: number; ok: false; error: { message: string; name?: string; stack?: string } };

export type OpenResult = {
  handle: number;
  sectorSize: number;
  capacityBytes: number;
  readOnly: boolean;
};

export function normalizeDiskOpenSpec(specOrMeta: DiskOpenSpec | DiskImageMetadata): DiskOpenSpec {
  // Treat inputs as untrusted: ignore inherited `kind` (prototype pollution).
  if (specOrMeta && typeof specOrMeta === "object") {
    const rec = specOrMeta as Record<string, unknown>;
    const kind = Object.prototype.hasOwnProperty.call(rec, "kind") ? rec.kind : undefined;
    if (kind === "local" || kind === "remote") {
      return specOrMeta as DiskOpenSpec;
    }
  }
  return { kind: "local", meta: specOrMeta as DiskImageMetadata };
}
