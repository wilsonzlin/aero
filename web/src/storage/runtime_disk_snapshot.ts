import type { DiskBackend, DiskFormat, DiskKind } from "./metadata";

export type RemoteDiskValidator =
  | { kind: "etag"; value: string }
  | { kind: "lastModified"; value: string };

// Snapshots may be loaded from untrusted sources (e.g. downloaded files). Keep decoding bounded so
// corrupted snapshots cannot force pathological allocations.
//
// Keep these limits in sync with Rust (`crates/aero-io-snapshot/src/io/storage/state.rs`).
const MAX_SNAPSHOT_BYTES = 1024 * 1024; // 1 MiB
const MAX_DISKS = 64;
const MAX_DISK_STRING_BYTES = 64 * 1024;
const MAX_REMOTE_CHUNK_SIZE_BYTES = 64 * 1024 * 1024;
const MAX_OVERLAY_BLOCK_SIZE_BYTES = 64 * 1024 * 1024;

export type RemoteDiskBaseSnapshot = {
  imageId: string;
  version: string;
  deliveryType: string;
  /**
   * Optional same-origin API endpoint used to mint refreshable `DiskAccessLease`s for this image.
   *
   * This value is safe to persist (unlike signed URLs). When present, restore flows can call it
   * to re-acquire a fresh stream URL/cookie lease without embedding secrets in the snapshot.
   */
  leaseEndpoint?: string;
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
  /**
   * For OPFS-backed disks, the directory containing `key` relative to the OPFS root.
   *
   * This allows snapshot/restore to reopen adopted legacy images stored outside the
   * default `aero/disks` directory.
   */
  dirPath?: string;
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

function fail(path: string, message: string): never {
  throw new Error(`Invalid disk snapshot payload (${path}): ${message}`);
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function requireRecord(v: unknown, path: string): Record<string, unknown> {
  if (!isRecord(v)) fail(path, "expected an object");
  return v;
}

function requireArray(v: unknown, path: string): unknown[] {
  if (!Array.isArray(v)) fail(path, "expected an array");
  return v;
}

function requireBoolean(v: unknown, path: string): boolean {
  if (typeof v !== "boolean") fail(path, "expected a boolean");
  return v;
}

function requireSafeInteger(v: unknown, path: string, opts: { min?: number } = {}): number {
  if (typeof v !== "number" || !Number.isSafeInteger(v)) {
    fail(path, "expected a safe integer");
  }
  if (opts.min !== undefined && v < opts.min) {
    fail(path, `expected >= ${opts.min}`);
  }
  return v;
}

function utf8LenAtMost(s: string, limit: number): number {
  // Count UTF-8 bytes without allocating.
  // This intentionally matches the behavior of `TextEncoder` for unpaired surrogates
  // (they become U+FFFD, which is 3 bytes in UTF-8).
  let bytes = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) {
      bytes += 1;
    } else if (c < 0x800) {
      bytes += 2;
    } else if (c >= 0xd800 && c <= 0xdbff) {
      const next = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
      if (next >= 0xdc00 && next <= 0xdfff) {
        // Surrogate pair => 4 bytes.
        bytes += 4;
        i++;
      } else {
        // Unpaired surrogate => U+FFFD => 3 bytes.
        bytes += 3;
      }
    } else if (c >= 0xdc00 && c <= 0xdfff) {
      // Unpaired low surrogate => U+FFFD.
      bytes += 3;
    } else {
      bytes += 3;
    }

    if (bytes > limit) return bytes;
  }
  return bytes;
}

function requireBoundedString(v: unknown, path: string, opts: { nonEmpty?: boolean } = {}): string {
  if (typeof v !== "string") fail(path, "expected a string");
  if (opts.nonEmpty && v.trim().length === 0) {
    fail(path, "must not be empty");
  }
  // Quick reject to avoid scanning extremely large strings.
  if (v.length > MAX_DISK_STRING_BYTES) {
    fail(path, `string too long (max ${MAX_DISK_STRING_BYTES} bytes)`);
  }
  const bytes = utf8LenAtMost(v, MAX_DISK_STRING_BYTES);
  if (bytes > MAX_DISK_STRING_BYTES) {
    fail(path, `string too long (max ${MAX_DISK_STRING_BYTES} bytes)`);
  }
  return v;
}

function requireDiskBackend(v: unknown, path: string): DiskBackend {
  if (v !== "opfs" && v !== "idb") {
    fail(path, 'expected "opfs" or "idb"');
  }
  return v;
}

function requireDiskKind(v: unknown, path: string): DiskKind {
  if (v !== "hdd" && v !== "cd") {
    fail(path, 'expected "hdd" or "cd"');
  }
  return v;
}

function requireDiskFormat(v: unknown, path: string): DiskFormat {
  switch (v) {
    case "raw":
    case "iso":
    case "qcow2":
    case "vhd":
    case "aerospar":
    case "unknown":
      return v;
    default:
      fail(path, "unsupported disk format");
  }
}

function requireSectorSize(v: unknown, path: string): number {
  const n = requireSafeInteger(v, path, { min: 1 });
  if (n !== 512 && n !== 4096) {
    fail(path, "sectorSize must be 512 or 4096");
  }
  return n;
}

function requirePowerOfTwoMultipleOf512Within(v: unknown, path: string, max: number): number {
  const n = requireSafeInteger(v, path, { min: 1 });
  if (n % 512 !== 0) {
    fail(path, "must be a multiple of 512");
  }
  // We cap to <= 64MiB, so 32-bit bitwise ops are safe.
  if ((n & (n - 1)) !== 0) {
    fail(path, "must be a power of two");
  }
  if (n > max) {
    fail(path, `too large (max ${max})`);
  }
  return n;
}

function validateLeaseEndpoint(v: unknown, path: string): string {
  const s = requireBoundedString(v, path, { nonEmpty: true }).trim();
  if (!s.startsWith("/")) {
    fail(path, "must be a same-origin path starting with '/'");
  }
  // `//example.com` is a protocol-relative URL (cross-origin). Disallow it even though it
  // starts with `/`.
  if (s.startsWith("//")) {
    fail(path, "must not start with '//'");
  }
  if (s.includes("http:") || s.includes("https:")) {
    fail(path, "must not contain http:/https:");
  }
  return s;
}

function validateRemoteValidator(v: unknown, path: string): RemoteDiskValidator {
  const obj = requireRecord(v, path);
  const kind = obj.kind;
  if (kind !== "etag" && kind !== "lastModified") {
    fail(`${path}.kind`, 'expected "etag" or "lastModified"');
  }
  const value = requireBoundedString(obj.value, `${path}.value`, { nonEmpty: true });
  return { kind, value };
}

function validateOverlaySnapshot(v: unknown, path: string, expectedDiskSizeBytes: number): DiskOverlaySnapshot {
  const obj = requireRecord(v, path);
  const fileName = requireBoundedString(obj.fileName, `${path}.fileName`, { nonEmpty: true });
  const diskSizeBytes = requireSafeInteger(obj.diskSizeBytes, `${path}.diskSizeBytes`, { min: 1 });
  if (diskSizeBytes !== expectedDiskSizeBytes) {
    fail(`${path}.diskSizeBytes`, `must match sizeBytes=${expectedDiskSizeBytes}`);
  }
  const blockSizeBytes = requirePowerOfTwoMultipleOf512Within(
    obj.blockSizeBytes,
    `${path}.blockSizeBytes`,
    MAX_OVERLAY_BLOCK_SIZE_BYTES,
  );
  return { fileName, diskSizeBytes, blockSizeBytes };
}

function validateCacheSnapshot(v: unknown, path: string): DiskCacheSnapshot {
  const obj = requireRecord(v, path);
  const fileName = requireBoundedString(obj.fileName, `${path}.fileName`, { nonEmpty: true });
  return { fileName };
}

function validateRemoteBaseSnapshot(v: unknown, path: string): RemoteDiskBaseSnapshot {
  const obj = requireRecord(v, path);
  const imageId = requireBoundedString(obj.imageId, `${path}.imageId`, { nonEmpty: true });
  const version = requireBoundedString(obj.version, `${path}.version`, { nonEmpty: true });
  const deliveryType = requireBoundedString(obj.deliveryType, `${path}.deliveryType`, { nonEmpty: true });

  const leaseEndpointRaw = obj.leaseEndpoint;
  const leaseEndpoint = leaseEndpointRaw !== undefined ? validateLeaseEndpoint(leaseEndpointRaw, `${path}.leaseEndpoint`) : undefined;

  const expectedValidatorRaw = obj.expectedValidator;
  const expectedValidator =
    expectedValidatorRaw !== undefined ? validateRemoteValidator(expectedValidatorRaw, `${path}.expectedValidator`) : undefined;

  const chunkSize = requirePowerOfTwoMultipleOf512Within(obj.chunkSize, `${path}.chunkSize`, MAX_REMOTE_CHUNK_SIZE_BYTES);

  return {
    imageId,
    version,
    deliveryType,
    ...(leaseEndpoint ? { leaseEndpoint } : {}),
    ...(expectedValidator ? { expectedValidator } : {}),
    chunkSize,
  };
}

function validateBackendSnapshot(
  v: unknown,
  path: string,
  opts: { sectorSize: number; capacityBytes: number },
): DiskBackendSnapshot {
  const obj = requireRecord(v, path);
  const kind = obj.kind;
  if (kind === "local") {
    const backend = requireDiskBackend(obj.backend, `${path}.backend`);
    const key = requireBoundedString(obj.key, `${path}.key`, { nonEmpty: true });
    const dirPathRaw = obj.dirPath;
    const dirPath =
      dirPathRaw !== undefined ? requireBoundedString(dirPathRaw, `${path}.dirPath`, { nonEmpty: true }) : undefined;
    const format = requireDiskFormat(obj.format, `${path}.format`);
    const diskKind = requireDiskKind(obj.diskKind, `${path}.diskKind`);
    const sizeBytes = requireSafeInteger(obj.sizeBytes, `${path}.sizeBytes`, { min: 1 });
    if (sizeBytes !== opts.capacityBytes) {
      fail(`${path}.sizeBytes`, `must match capacityBytes=${opts.capacityBytes}`);
    }
    if (sizeBytes % opts.sectorSize !== 0) {
      fail(`${path}.sizeBytes`, `must be a multiple of sectorSize=${opts.sectorSize}`);
    }

    const overlayRaw = obj.overlay;
    const overlay =
      overlayRaw !== undefined ? validateOverlaySnapshot(overlayRaw, `${path}.overlay`, sizeBytes) : undefined;

    return {
      kind: "local",
      backend,
      key,
      ...(dirPath ? { dirPath } : {}),
      format,
      diskKind,
      sizeBytes,
      ...(overlay ? { overlay } : {}),
    };
  }

  if (kind === "remote") {
    const backendRaw = obj.backend;
    const backend = backendRaw !== undefined ? requireDiskBackend(backendRaw, `${path}.backend`) : undefined;
    const diskKind = requireDiskKind(obj.diskKind, `${path}.diskKind`);
    const sizeBytes = requireSafeInteger(obj.sizeBytes, `${path}.sizeBytes`, { min: 1 });
    if (sizeBytes !== opts.capacityBytes) {
      fail(`${path}.sizeBytes`, `must match capacityBytes=${opts.capacityBytes}`);
    }
    if (sizeBytes % opts.sectorSize !== 0) {
      fail(`${path}.sizeBytes`, `must be a multiple of sectorSize=${opts.sectorSize}`);
    }

    const base = validateRemoteBaseSnapshot(obj.base, `${path}.base`);
    const overlay = validateOverlaySnapshot(obj.overlay, `${path}.overlay`, sizeBytes);
    const cache = validateCacheSnapshot(obj.cache, `${path}.cache`);

    return {
      kind: "remote",
      ...(backend ? { backend } : {}),
      diskKind,
      sizeBytes,
      base,
      overlay,
      cache,
    };
  }

  fail(`${path}.kind`, 'expected "local" or "remote"');
}

export function deserializeRuntimeDiskSnapshot(bytes: Uint8Array): RuntimeDiskSnapshot {
  if (bytes.byteLength > MAX_SNAPSHOT_BYTES) {
    throw new Error(`Invalid disk snapshot payload (too large: max=${MAX_SNAPSHOT_BYTES} bytes).`);
  }
  const json = new TextDecoder().decode(bytes);
  let parsed: unknown;
  try {
    parsed = JSON.parse(json) as unknown;
  } catch {
    throw new Error("Invalid disk snapshot payload (invalid JSON).");
  }
  const obj = requireRecord(parsed, "root");

  if (obj.version !== 1) {
    throw new Error(`Unsupported disk snapshot version: ${String(obj.version)}`);
  }

  const nextHandle = requireSafeInteger(obj.nextHandle, "nextHandle", { min: 1 });

  const disksRaw = requireArray(obj.disks, "disks");
  if (disksRaw.length > MAX_DISKS) {
    fail("disks", `too many disks (max ${MAX_DISKS})`);
  }

  const disks: RuntimeDiskSnapshotEntry[] = [];
  const seenHandles = new Set<number>();
  for (let i = 0; i < disksRaw.length; i++) {
    const entryPath = `disks[${i}]`;
    const diskObj = requireRecord(disksRaw[i], entryPath);
    const handle = requireSafeInteger(diskObj.handle, `${entryPath}.handle`, { min: 1 });
    if (seenHandles.has(handle)) {
      fail(`${entryPath}.handle`, `duplicate handle ${handle}`);
    }
    seenHandles.add(handle);

    const readOnly = requireBoolean(diskObj.readOnly, `${entryPath}.readOnly`);
    const sectorSize = requireSectorSize(diskObj.sectorSize, `${entryPath}.sectorSize`);
    const capacityBytes = requireSafeInteger(diskObj.capacityBytes, `${entryPath}.capacityBytes`, { min: 1 });
    if (capacityBytes % sectorSize !== 0) {
      fail(`${entryPath}.capacityBytes`, `must be a multiple of sectorSize=${sectorSize}`);
    }

    const backend = validateBackendSnapshot(diskObj.backend, `${entryPath}.backend`, {
      sectorSize,
      capacityBytes,
    });

    disks.push({
      handle,
      readOnly,
      sectorSize,
      capacityBytes,
      backend,
    });
  }

  return { version: 1, nextHandle, disks };
}

export type RemoteCacheBinding = {
  version: 1;
  base: RemoteDiskBaseSnapshot;
};

function remoteDeliveryKind(deliveryType: string): string {
  const idx = deliveryType.indexOf(":");
  return idx === -1 ? deliveryType : deliveryType.slice(0, idx);
}

export function shouldInvalidateRemoteOverlay(
  expected: RemoteDiskBaseSnapshot,
  binding: RemoteCacheBinding | null | undefined,
): boolean {
  // Overlay invalidation must be conservative: if we don't have a binding, keep the overlay
  // (it may contain user state). Only invalidate when we have positive evidence that the
  // overlay was created against a different remote base identity.
  if (!binding || binding.version !== 1) return false;
  const base = binding.base;
  if (!base) return false;

  if (base.imageId !== expected.imageId) return true;
  if (base.version !== expected.version) return true;
  if (remoteDeliveryKind(base.deliveryType) !== remoteDeliveryKind(expected.deliveryType)) return true;

  // NOTE: We intentionally *do not* compare `chunkSize` here. `chunkSize` is a local cache
  // tuning parameter for remote delivery and can be changed without changing the underlying
  // remote bytes. Invalidating the overlay on chunk size changes would unnecessarily discard
  // user state.

  const a = expected.expectedValidator;
  const b = base.expectedValidator;
  if (!a && !b) return false;
  if (!a || !b) return true;
  return a.kind !== b.kind || a.value !== b.value;
}

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
