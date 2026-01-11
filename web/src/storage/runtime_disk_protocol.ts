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
  | { type: "request"; requestId: number; op: "write"; payload: { handle: number; lba: number; data: Uint8Array } }
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
  if ((specOrMeta as DiskOpenSpec).kind === "local" || (specOrMeta as DiskOpenSpec).kind === "remote") {
    return specOrMeta as DiskOpenSpec;
  }
  return { kind: "local", meta: specOrMeta as DiskImageMetadata };
}
