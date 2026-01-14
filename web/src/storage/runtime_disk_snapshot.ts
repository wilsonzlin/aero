import { DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES, type DiskBackend, type DiskFormat, type DiskKind } from "./metadata";

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
  /**
   * Persistent cache size limit for remote delivery.
   *
   * - `null`: unbounded (no eviction)
   * - `0`: disable caching entirely
   * - positive number: bounded cache size
   */
  cacheLimitBytes: number | null;
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

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function nullProto<T extends object>(): T {
  return Object.create(null) as T;
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
  const kind = hasOwn(obj, "kind") ? obj.kind : undefined;
  if (kind !== "etag" && kind !== "lastModified") {
    fail(`${path}.kind`, 'expected "etag" or "lastModified"');
  }
  const valueRaw = hasOwn(obj, "value") ? obj.value : undefined;
  const value = requireBoundedString(valueRaw, `${path}.value`, { nonEmpty: true });
  if (kind === "etag") {
    const out = nullProto<{ kind: "etag"; value: string }>();
    out.kind = "etag";
    out.value = value;
    return out;
  }
  const out = nullProto<{ kind: "lastModified"; value: string }>();
  out.kind = "lastModified";
  out.value = value;
  return out;
}

function validateOverlaySnapshot(v: unknown, path: string, expectedDiskSizeBytes: number): DiskOverlaySnapshot {
  const obj = requireRecord(v, path);
  const fileName = requireBoundedString(hasOwn(obj, "fileName") ? obj.fileName : undefined, `${path}.fileName`, {
    nonEmpty: true,
  });
  const diskSizeBytes = requireSafeInteger(hasOwn(obj, "diskSizeBytes") ? obj.diskSizeBytes : undefined, `${path}.diskSizeBytes`, {
    min: 1,
  });
  if (diskSizeBytes !== expectedDiskSizeBytes) {
    fail(`${path}.diskSizeBytes`, `must match sizeBytes=${expectedDiskSizeBytes}`);
  }
  const blockSizeBytes = requirePowerOfTwoMultipleOf512Within(
    hasOwn(obj, "blockSizeBytes") ? obj.blockSizeBytes : undefined,
    `${path}.blockSizeBytes`,
    MAX_OVERLAY_BLOCK_SIZE_BYTES,
  );
  const out = nullProto<DiskOverlaySnapshot>();
  out.fileName = fileName;
  out.diskSizeBytes = diskSizeBytes;
  out.blockSizeBytes = blockSizeBytes;
  return out;
}

function validateCacheSnapshot(v: unknown, path: string): DiskCacheSnapshot {
  const obj = requireRecord(v, path);
  const fileName = requireBoundedString(hasOwn(obj, "fileName") ? obj.fileName : undefined, `${path}.fileName`, {
    nonEmpty: true,
  });

  // Backward compatibility: older snapshots omitted `cacheLimitBytes`. Treat it as the default
  // bounded cache size (currently 512 MiB).
  const raw = hasOwn(obj, "cacheLimitBytes") ? (obj as { cacheLimitBytes?: unknown }).cacheLimitBytes : undefined;
  const cacheLimitBytes =
    raw === undefined ? DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES : raw === null ? null : requireSafeInteger(raw, `${path}.cacheLimitBytes`, { min: 0 });
  const out = nullProto<DiskCacheSnapshot>();
  out.fileName = fileName;
  out.cacheLimitBytes = cacheLimitBytes;
  return out;
}

function validateRemoteBaseSnapshot(v: unknown, path: string): RemoteDiskBaseSnapshot {
  const obj = requireRecord(v, path);
  const imageId = requireBoundedString(hasOwn(obj, "imageId") ? obj.imageId : undefined, `${path}.imageId`, { nonEmpty: true });
  const version = requireBoundedString(hasOwn(obj, "version") ? obj.version : undefined, `${path}.version`, { nonEmpty: true });
  const deliveryType = requireBoundedString(hasOwn(obj, "deliveryType") ? obj.deliveryType : undefined, `${path}.deliveryType`, { nonEmpty: true });

  const leaseEndpointRaw = hasOwn(obj, "leaseEndpoint") ? obj.leaseEndpoint : undefined;
  const leaseEndpoint = leaseEndpointRaw !== undefined ? validateLeaseEndpoint(leaseEndpointRaw, `${path}.leaseEndpoint`) : undefined;

  const expectedValidatorRaw = hasOwn(obj, "expectedValidator") ? obj.expectedValidator : undefined;
  const expectedValidator =
    expectedValidatorRaw !== undefined ? validateRemoteValidator(expectedValidatorRaw, `${path}.expectedValidator`) : undefined;

  const chunkSize = requirePowerOfTwoMultipleOf512Within(
    hasOwn(obj, "chunkSize") ? obj.chunkSize : undefined,
    `${path}.chunkSize`,
    MAX_REMOTE_CHUNK_SIZE_BYTES,
  );

  const out = nullProto<RemoteDiskBaseSnapshot>();
  out.imageId = imageId;
  out.version = version;
  out.deliveryType = deliveryType;
  if (leaseEndpoint) out.leaseEndpoint = leaseEndpoint;
  if (expectedValidator) out.expectedValidator = expectedValidator;
  out.chunkSize = chunkSize;
  return out;
}

function validateBackendSnapshot(
  v: unknown,
  path: string,
  opts: { sectorSize: number; capacityBytes: number },
): DiskBackendSnapshot {
  const obj = requireRecord(v, path);
  const kind = hasOwn(obj, "kind") ? obj.kind : undefined;
  if (kind === "local") {
    const backend = requireDiskBackend(hasOwn(obj, "backend") ? obj.backend : undefined, `${path}.backend`);
    const key = requireBoundedString(hasOwn(obj, "key") ? obj.key : undefined, `${path}.key`, { nonEmpty: true });
    const dirPathRaw = hasOwn(obj, "dirPath") ? obj.dirPath : undefined;
    const dirPath =
      dirPathRaw !== undefined ? requireBoundedString(dirPathRaw, `${path}.dirPath`, { nonEmpty: true }) : undefined;
    const format = requireDiskFormat(hasOwn(obj, "format") ? obj.format : undefined, `${path}.format`);
    const diskKind = requireDiskKind(hasOwn(obj, "diskKind") ? obj.diskKind : undefined, `${path}.diskKind`);
    const sizeBytes = requireSafeInteger(hasOwn(obj, "sizeBytes") ? obj.sizeBytes : undefined, `${path}.sizeBytes`, {
      min: 1,
    });
    if (sizeBytes !== opts.capacityBytes) {
      fail(`${path}.sizeBytes`, `must match capacityBytes=${opts.capacityBytes}`);
    }
    if (sizeBytes % opts.sectorSize !== 0) {
      fail(`${path}.sizeBytes`, `must be a multiple of sectorSize=${opts.sectorSize}`);
    }

    const overlayRaw = hasOwn(obj, "overlay") ? obj.overlay : undefined;
    const overlay =
      overlayRaw !== undefined ? validateOverlaySnapshot(overlayRaw, `${path}.overlay`, sizeBytes) : undefined;

    const out = nullProto<LocalDiskBackendSnapshot>();
    out.kind = "local";
    out.backend = backend;
    out.key = key;
    if (dirPath) out.dirPath = dirPath;
    out.format = format;
    out.diskKind = diskKind;
    out.sizeBytes = sizeBytes;
    if (overlay) out.overlay = overlay;
    return out;
  }

  if (kind === "remote") {
    const backendRaw = hasOwn(obj, "backend") ? obj.backend : undefined;
    const backend = backendRaw !== undefined ? requireDiskBackend(backendRaw, `${path}.backend`) : undefined;
    const diskKind = requireDiskKind(hasOwn(obj, "diskKind") ? obj.diskKind : undefined, `${path}.diskKind`);
    const sizeBytes = requireSafeInteger(hasOwn(obj, "sizeBytes") ? obj.sizeBytes : undefined, `${path}.sizeBytes`, {
      min: 1,
    });
    if (sizeBytes !== opts.capacityBytes) {
      fail(`${path}.sizeBytes`, `must match capacityBytes=${opts.capacityBytes}`);
    }
    if (sizeBytes % opts.sectorSize !== 0) {
      fail(`${path}.sizeBytes`, `must be a multiple of sectorSize=${opts.sectorSize}`);
    }

    const base = validateRemoteBaseSnapshot(hasOwn(obj, "base") ? obj.base : undefined, `${path}.base`);
    const overlay = validateOverlaySnapshot(hasOwn(obj, "overlay") ? obj.overlay : undefined, `${path}.overlay`, sizeBytes);
    const cache = validateCacheSnapshot(hasOwn(obj, "cache") ? obj.cache : undefined, `${path}.cache`);

    const out = nullProto<RemoteDiskBackendSnapshot>();
    out.kind = "remote";
    if (backend) out.backend = backend;
    out.diskKind = diskKind;
    out.sizeBytes = sizeBytes;
    out.base = base;
    out.overlay = overlay;
    out.cache = cache;
    return out;
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

  const version = hasOwn(obj, "version") ? obj.version : undefined;
  if (version !== 1) {
    throw new Error(`Unsupported disk snapshot version: ${String(version)}`);
  }

  const nextHandle = requireSafeInteger(hasOwn(obj, "nextHandle") ? obj.nextHandle : undefined, "nextHandle", { min: 1 });

  const disksRaw = requireArray(hasOwn(obj, "disks") ? obj.disks : undefined, "disks");
  if (disksRaw.length > MAX_DISKS) {
    fail("disks", `too many disks (max ${MAX_DISKS})`);
  }

  const disks: RuntimeDiskSnapshotEntry[] = [];
  const seenHandles = new Set<number>();
  for (let i = 0; i < disksRaw.length; i++) {
    const entryPath = `disks[${i}]`;
    const diskObj = requireRecord(disksRaw[i], entryPath);
    const handle = requireSafeInteger(hasOwn(diskObj, "handle") ? diskObj.handle : undefined, `${entryPath}.handle`, {
      min: 1,
    });
    if (seenHandles.has(handle)) {
      fail(`${entryPath}.handle`, `duplicate handle ${handle}`);
    }
    seenHandles.add(handle);

    const readOnly = requireBoolean(hasOwn(diskObj, "readOnly") ? diskObj.readOnly : undefined, `${entryPath}.readOnly`);
    const sectorSize = requireSectorSize(
      hasOwn(diskObj, "sectorSize") ? diskObj.sectorSize : undefined,
      `${entryPath}.sectorSize`,
    );
    const capacityBytes = requireSafeInteger(
      hasOwn(diskObj, "capacityBytes") ? diskObj.capacityBytes : undefined,
      `${entryPath}.capacityBytes`,
      { min: 1 },
    );
    if (capacityBytes % sectorSize !== 0) {
      fail(`${entryPath}.capacityBytes`, `must be a multiple of sectorSize=${sectorSize}`);
    }

    const backend = validateBackendSnapshot(hasOwn(diskObj, "backend") ? diskObj.backend : undefined, `${entryPath}.backend`, {
      sectorSize,
      capacityBytes,
    });

    const entry = nullProto<RuntimeDiskSnapshotEntry>();
    entry.handle = handle;
    entry.readOnly = readOnly;
    entry.sectorSize = sectorSize;
    entry.capacityBytes = capacityBytes;
    entry.backend = backend;
    disks.push(entry);
  }

  const out = nullProto<RuntimeDiskSnapshot>();
  out.version = 1;
  out.nextHandle = nextHandle;
  out.disks = disks;
  return out;
}

export type RemoteCacheBinding = {
  version: 1;
  base: RemoteDiskBaseSnapshot;
};

function remoteDeliveryKind(deliveryType: string): string {
  const idx = deliveryType.indexOf(":");
  return idx === -1 ? deliveryType : deliveryType.slice(0, idx);
}

function isRemoteDiskValidator(value: unknown): value is RemoteDiskValidator {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const obj = value as Record<string, unknown>;
  const kind = hasOwn(obj, "kind") ? obj.kind : undefined;
  if (kind !== "etag" && kind !== "lastModified") return false;
  const v = hasOwn(obj, "value") ? obj.value : undefined;
  if (typeof v !== "string" || v.trim().length === 0) return false;
  return true;
}

export function shouldInvalidateRemoteOverlay(
  expected: RemoteDiskBaseSnapshot,
  binding: RemoteCacheBinding | null | undefined,
): boolean {
  // Overlay invalidation must be conservative: if we don't have a binding, keep the overlay
  // (it may contain user state). Only invalidate when we have positive evidence that the
  // overlay was created against a different remote base identity.
  if (!binding || !isRecord(binding)) return false;
  const bindingAny = binding as unknown as Record<string, unknown>;
  const bindingVersion = hasOwn(bindingAny, "version") ? bindingAny.version : undefined;
  if (bindingVersion !== 1) return false;
  const base = hasOwn(bindingAny, "base") ? bindingAny.base : undefined;
  if (!isRecord(base)) return false;
  const baseAny = base as unknown as Record<string, unknown>;
  const imageId = hasOwn(baseAny, "imageId") ? baseAny.imageId : undefined;
  const version = hasOwn(baseAny, "version") ? baseAny.version : undefined;
  const deliveryType = hasOwn(baseAny, "deliveryType") ? baseAny.deliveryType : undefined;
  if (typeof imageId !== "string" || imageId.trim().length === 0) return false;
  if (typeof version !== "string" || version.trim().length === 0) return false;
  if (typeof deliveryType !== "string" || deliveryType.trim().length === 0) return false;

  if (imageId !== expected.imageId) return true;
  if (version !== expected.version) return true;
  if (remoteDeliveryKind(deliveryType) !== remoteDeliveryKind(expected.deliveryType)) return true;

  // NOTE: We intentionally *do not* compare `chunkSize` here. `chunkSize` is a local cache
  // tuning parameter for remote delivery and can be changed without changing the underlying
  // remote bytes. Invalidating the overlay on chunk size changes would unnecessarily discard
  // user state.

  const a = expected.expectedValidator;
  const expectedValidatorRaw = hasOwn(baseAny, "expectedValidator") ? baseAny.expectedValidator : undefined;
  const b = isRemoteDiskValidator(expectedValidatorRaw) ? (expectedValidatorRaw as RemoteDiskValidator) : undefined;
  // Only invalidate when both expected and binding provide a validator and they conflict. Missing
  // validator info is not positive evidence of mismatch, and overlays may contain user data.
  if (!a || !b) return false;
  return a.kind !== b.kind || a.value !== b.value;
}

export function shouldInvalidateRemoteCache(
  expected: RemoteDiskBaseSnapshot,
  binding: RemoteCacheBinding | null | undefined,
): boolean {
  if (!binding || !isRecord(binding)) return true;
  const bindingAny = binding as unknown as Record<string, unknown>;
  const bindingVersion = hasOwn(bindingAny, "version") ? bindingAny.version : undefined;
  if (bindingVersion !== 1) return true;
  const base = hasOwn(bindingAny, "base") ? bindingAny.base : undefined;
  if (!isRecord(base)) return true;
  const baseAny = base as unknown as Record<string, unknown>;
  const imageId = hasOwn(baseAny, "imageId") ? baseAny.imageId : undefined;
  const version = hasOwn(baseAny, "version") ? baseAny.version : undefined;
  const deliveryType = hasOwn(baseAny, "deliveryType") ? baseAny.deliveryType : undefined;
  const chunkSize = hasOwn(baseAny, "chunkSize") ? baseAny.chunkSize : undefined;
  if (typeof imageId !== "string" || imageId.trim().length === 0) return true;
  if (typeof version !== "string" || version.trim().length === 0) return true;
  if (typeof deliveryType !== "string" || deliveryType.trim().length === 0) return true;
  if (typeof chunkSize !== "number" || !Number.isSafeInteger(chunkSize) || chunkSize <= 0) return true;
  if (imageId !== expected.imageId) return true;
  if (version !== expected.version) return true;
  if (deliveryType !== expected.deliveryType) return true;
  if (chunkSize !== expected.chunkSize) return true;

  const a = expected.expectedValidator;
  const expectedValidatorRaw = hasOwn(baseAny, "expectedValidator") ? baseAny.expectedValidator : undefined;
  const b = isRemoteDiskValidator(expectedValidatorRaw) ? (expectedValidatorRaw as RemoteDiskValidator) : undefined;
  if (!a && !b) return false;
  if (!a || !b) return true;
  return a.kind !== b.kind || a.value !== b.value;
}
