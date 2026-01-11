import type { DiskBackend, DiskFormat, DiskKind } from "./metadata";

export type RemoteDiskValidator =
  | { kind: "etag"; value: string }
  | { kind: "lastModified"; value: string };

export type RemoteDiskBaseSnapshot = {
  imageId: string;
  version: string;
  deliveryType: string;
  expectedValidator?: RemoteDiskValidator;
  chunkSize: number;
};

export type DiskOverlaySnapshot = {
  fileName: string;
  diskSizeBytes: number;
  blockSizeBytes: number;
};

export type DiskCacheSnapshot = {
  fileName: string;
};

export type LocalDiskBackendSnapshot = {
  kind: "local";
  backend: DiskBackend;
  /**
   * Stable backend key/path:
   * - OPFS: `fileName`
   * - IDB: disk id
   */
  key: string;
  format: DiskFormat;
  diskKind: DiskKind;
  sizeBytes: number;
  overlay?: DiskOverlaySnapshot;
};

export type RemoteDiskBackendSnapshot = {
  kind: "remote";
  /**
   * Storage backend used for the remote disk cache + overlay.
   *
   * v1 snapshots assumed OPFS; this is optional for backwards compatibility.
   */
  backend?: DiskBackend;
  diskKind: DiskKind;
  sizeBytes: number;
  base: RemoteDiskBaseSnapshot;
  overlay: DiskOverlaySnapshot;
  cache: DiskCacheSnapshot;
};

export type DiskBackendSnapshot = LocalDiskBackendSnapshot | RemoteDiskBackendSnapshot;

export type RuntimeDiskSnapshotEntry = {
  handle: number;
  readOnly: boolean;
  sectorSize: number;
  capacityBytes: number;
  backend: DiskBackendSnapshot;
};

export type RuntimeDiskSnapshot = {
  version: 1;
  nextHandle: number;
  disks: RuntimeDiskSnapshotEntry[];
};

export function serializeRuntimeDiskSnapshot(snapshot: RuntimeDiskSnapshot): Uint8Array {
  const json = JSON.stringify(snapshot);
  return new TextEncoder().encode(json);
}

export function deserializeRuntimeDiskSnapshot(bytes: Uint8Array): RuntimeDiskSnapshot {
  const json = new TextDecoder().decode(bytes);
  const parsed = JSON.parse(json) as Partial<RuntimeDiskSnapshot> | null;
  if (!parsed || typeof parsed !== "object") {
    throw new Error("Invalid disk snapshot payload (not an object).");
  }
  if (parsed.version !== 1) {
    throw new Error(`Unsupported disk snapshot version: ${String(parsed.version)}`);
  }
  if (!Array.isArray(parsed.disks)) {
    throw new Error("Invalid disk snapshot payload (disks missing).");
  }
  return parsed as RuntimeDiskSnapshot;
}

export type RemoteCacheBinding = {
  version: 1;
  base: RemoteDiskBaseSnapshot;
};

export function shouldInvalidateRemoteCache(
  expected: RemoteDiskBaseSnapshot,
  binding: RemoteCacheBinding | null | undefined,
): boolean {
  if (!binding || binding.version !== 1) return true;
  const base = binding.base;
  if (!base) return true;
  if (base.imageId !== expected.imageId) return true;
  if (base.version !== expected.version) return true;
  if (base.deliveryType !== expected.deliveryType) return true;
  if (base.chunkSize !== expected.chunkSize) return true;

  const a = expected.expectedValidator;
  const b = base.expectedValidator;
  if (!a && !b) return false;
  if (!a || !b) return true;
  return a.kind !== b.kind || a.value !== b.value;
}
