import { OpfsCowDisk } from "./opfs_cow";
import { OpfsRawDisk } from "./opfs_raw";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import { assertSectorAligned, checkedOffset, type AsyncSectorDisk } from "./disk";
import { IdbCowDisk } from "./idb_cow";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { benchSequentialRead, benchSequentialWrite } from "./bench";
import {
  DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES,
  hasOpfsSyncAccessHandle,
  idbReq,
  idbTxDone,
  OPFS_DISKS_PATH,
  openDiskManagerDb,
  pickDefaultBackend,
  type DiskBackend,
  type DiskImageMetadata,
} from "./metadata";
import { RemoteStreamingDisk, type RemoteDiskOptions, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import type {
  DiskOpenSpec,
  OpenMode,
  OpenRequestPayload,
  RemoteDiskIntegritySpec,
  RuntimeDiskRequestMessage,
  RuntimeDiskResponseMessage,
} from "./runtime_disk_protocol";
import { normalizeDiskOpenSpec } from "./runtime_disk_protocol";
import { idbDeleteDiskData, opfsDeleteDisk, opfsGetDiskFileHandle } from "./import_export";
import { RemoteRangeDisk, defaultRemoteRangeUrl, type RemoteRangeDiskMetadataStore } from "./remote_range_disk";
import { RemoteChunkedDisk, type RemoteChunkedDiskOpenOptions } from "./remote_chunked_disk";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import {
  RemoteCacheManager,
  remoteChunkedDeliveryType,
  remoteRangeDeliveryType,
  validateRemoteCacheMetaV1,
} from "./remote_cache_manager";
import { readJsonResponseWithLimit } from "./response_json";
import {
  deserializeRuntimeDiskSnapshot,
  serializeRuntimeDiskSnapshot,
  shouldInvalidateRemoteCache,
  shouldInvalidateRemoteOverlay,
  type DiskBackendSnapshot,
  type RemoteCacheBinding,
  type RuntimeDiskSnapshot,
  type RuntimeDiskSnapshotEntry,
} from "./runtime_disk_snapshot";
import { createDiskAccessLeaseFromLeaseEndpoint } from "./disk_access_lease";
import { RUNTIME_DISK_MAX_IO_BYTES } from "./runtime_disk_limits";

export type DiskEntry = {
  disk: AsyncSectorDisk;
  readOnly: boolean;
  backendSnapshot: DiskBackendSnapshot | null;
};

// Defensive bound for remote JSON manifests (sha256 lists). These can come from untrusted servers,
// so avoid reading/parsing arbitrarily large JSON blobs.
const MAX_SHA256_MANIFEST_JSON_BYTES = 64 * 1024 * 1024; // 64 MiB
const MAX_SHA256_MANIFEST_ENTRIES = 1_000_000;

export type OpenDiskFn = (spec: DiskOpenSpec, mode: OpenMode, overlayBlockSizeBytes?: number) => Promise<DiskEntry>;

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

type TrackedDiskEntry = DiskEntry & { io: DiskIoTelemetry };

function serializeError(err: unknown): { message: string; name?: string; stack?: string } {
  if (err instanceof Error) return { message: err.message, name: err.name, stack: err.stack };
  return { message: String(err) };
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

function requireSafeNonNegativeInteger(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value) || value < 0) {
    throw new Error(`invalid ${label}=${String(value)}`);
  }
  if (!Number.isSafeInteger(value)) {
    throw new Error(`invalid ${label}=${String(value)}`);
  }
  return value;
}

function requireSharedArrayBuffer(value: unknown, label: string): SharedArrayBuffer {
  // Some environments (non-COI browsers) do not define SharedArrayBuffer at all. Avoid
  // ReferenceError by checking via `typeof` first.
  if (typeof SharedArrayBuffer === "undefined" || !(value instanceof SharedArrayBuffer)) {
    throw new Error(`invalid ${label} (expected SharedArrayBuffer)`);
  }
  return value;
}

function createSabView(
  sab: unknown,
  offsetBytes: unknown,
  byteLength: number,
  label: string,
): { sab: SharedArrayBuffer; offsetBytes: number; view: Uint8Array } {
  const shared = requireSharedArrayBuffer(sab, `${label}.sab`);
  const off = requireSafeNonNegativeInteger(offsetBytes, `${label}.offsetBytes`);
  const end = off + byteLength;
  if (!Number.isSafeInteger(end)) {
    throw new Error(`invalid ${label} (offset+length overflow)`);
  }
  if (end > shared.byteLength) {
    throw new Error(`invalid ${label} (out of bounds)`);
  }
  return { sab: shared, offsetBytes: off, view: new Uint8Array(shared, off, byteLength) };
}

function stableImageIdFromUrl(url: string): string {
  // Use URL parsing when possible so we can drop querystring auth material.
  // Fall back to string splitting for relative URLs.
  try {
    const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
    const u = base ? new URL(url, base) : new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    const noHash = url.split("#", 1)[0] ?? url;
    return (noHash.split("?", 1)[0] ?? noHash).trim();
  }
}

async function bestEffortDeleteLegacyRemoteRangeCache(imageId: string, version: string): Promise<void> {
  try {
    const legacyCacheKey = await RemoteCacheManager.deriveCacheKey({ imageId, version, deliveryType: "range" });
    const manager = await RemoteCacheManager.openOpfs();
    await manager.clearCache(legacyCacheKey);
  } catch {
    // best-effort cleanup only
  }
}

async function openRemoteDisk(url: string, options?: RemoteDiskOptions): Promise<AsyncSectorDisk> {
  // Treat `options` as untrusted (postMessage payload). Copy into a null-prototype object so
  // `Object.prototype.cacheBackend`/etc pollution cannot affect option resolution.
  const optionsSafe = isRecord(options) ? (Object.assign(Object.create(null), options) as RemoteDiskOptions) : (Object.create(null) as RemoteDiskOptions);
  const cacheBackend: DiskBackend = optionsSafe.cacheBackend ?? pickDefaultBackend();
  const cacheLimitBytes = optionsSafe.cacheLimitBytes;
  // `RemoteRangeDisk` uses OPFS sparse files (requires SyncAccessHandle) and does not
  // implement cache eviction. Only select it when OPFS is requested *and* the caller
  // explicitly opts into an unbounded cache (`cacheLimitBytes: null`). Otherwise fall
  // back to `RemoteStreamingDisk`, which implements bounded eviction (default 512 MiB).
  if (cacheBackend === "opfs" && cacheLimitBytes === null && hasOpfsSyncAccessHandle()) {
    const chunkSize = typeof optionsSafe.blockSize === "number" ? optionsSafe.blockSize : RANGE_STREAM_CHUNK_SIZE;
    const cacheKeyParts = {
      imageId: (optionsSafe.cacheImageId ?? stableImageIdFromUrl(url)).trim(),
      version: (optionsSafe.cacheVersion ?? "1").trim(),
      deliveryType: remoteRangeDeliveryType(chunkSize),
    };
    if (!cacheKeyParts.imageId) throw new Error("cacheImageId must not be empty");
    if (!cacheKeyParts.version) throw new Error("cacheVersion must not be empty");

    // Backward-compat cleanup: older clients used `deliveryType: "range"`, which can collide
    // across different chunkSize configurations for the same image/version.
    await bestEffortDeleteLegacyRemoteRangeCache(cacheKeyParts.imageId, cacheKeyParts.version);
    return await RemoteRangeDisk.open(url, {
      cacheKeyParts,
      credentials: optionsSafe.credentials,
      chunkSize,
      readAheadChunks: optionsSafe.prefetchSequentialBlocks,
    });
  }

  optionsSafe.cacheBackend = cacheBackend;
  return await RemoteStreamingDisk.open(url, optionsSafe);
}

function safeOpfsNameComponent(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) throw new Error("cacheKey must not be empty");
  // OPFS file names cannot contain path separators. We also keep names mostly
  // ASCII to avoid platform-specific edge cases.
  const safe = trimmed.replace(/[^a-zA-Z0-9._-]/g, "_");
  if (safe === "." || safe === "..") throw new Error("invalid cacheKey");
  return safe;
}

async function openOpfsSparseDisk(
  name: string,
  opts: { diskSizeBytes: number; blockSizeBytes: number; dirPath?: string },
): Promise<OpfsAeroSparseDisk> {
  try {
    const opened = await OpfsAeroSparseDisk.open(name, { dirPath: opts.dirPath });
    if (opened.capacityBytes !== opts.diskSizeBytes) {
      await opened.close?.();
      throw new Error(`sparse disk size mismatch: expected=${opts.diskSizeBytes} actual=${opened.capacityBytes}`);
    }
    if (opened.blockSizeBytes !== opts.blockSizeBytes) {
      await opened.close?.();
      throw new Error(
        `sparse block size mismatch: expected=${opts.blockSizeBytes} actual=${opened.blockSizeBytes}`,
      );
    }
    return opened;
  } catch {
    return await OpfsAeroSparseDisk.create(name, {
      diskSizeBytes: opts.diskSizeBytes,
      blockSizeBytes: opts.blockSizeBytes,
      dirPath: opts.dirPath,
    });
  }
}

function cacheBindingFileName(cacheFileName: string): string {
  return `${cacheFileName}.binding.json`;
}

function remoteRangeMetaFileName(cacheFileName: string): string {
  return `${cacheFileName}.remote-range-meta.json`;
}

// Defensive bound: binding/meta JSON files can become corrupt/attacker-controlled. Keep reads small
// to avoid pathological allocations and JSON parse overhead.
const MAX_OPFS_BINDING_BYTES = 64 * 1024 * 1024; // 64 MiB

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function opfsRemoteRangeDiskMetadataStore(metaFileName: string): RemoteRangeDiskMetadataStore {
  return {
    async read(_cacheId: string) {
      try {
        const handle = await opfsGetDiskFileHandle(metaFileName, { create: false });
        const file = await handle.getFile();
        if (!Number.isFinite(file.size) || file.size < 0 || file.size > MAX_OPFS_BINDING_BYTES) return null;
        const text = await file.text();
        if (!text.trim()) return null;
        try {
          const parsed = JSON.parse(text) as unknown;
          return validateRemoteCacheMetaV1(parsed);
        } catch {
          return null;
        }
      } catch (err) {
        if (err instanceof DOMException && err.name === "NotFoundError") return null;
        // Corrupt JSON / transient failures: treat as missing so callers can regenerate.
        return null;
      }
    },
    async write(_cacheId: string, meta: any) {
      const handle = await opfsGetDiskFileHandle(metaFileName, { create: true });
      let writable: FileSystemWritableFileStream;
      let truncateFallback = false;
      try {
        writable = await handle.createWritable({ keepExistingData: false });
      } catch {
        // Some implementations may not accept options; fall back to default.
        writable = await handle.createWritable();
        truncateFallback = true;
      }
      if (truncateFallback) {
        // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
        // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
        try {
          await writable.truncate(0);
        } catch {
          // ignore
        }
      }
      try {
        await writable.write(JSON.stringify(meta, null, 2));
        await writable.close();
      } catch (err) {
        try {
          await writable.abort(err);
        } catch {
          // ignore abort failures
        }
        throw err;
      }
    },
    async delete(_cacheId: string) {
      await opfsDeleteDisk(metaFileName);
    },
  };
}

async function readCacheBinding(fileName: string): Promise<RemoteCacheBinding | null> {
  try {
    const handle = await opfsGetDiskFileHandle(fileName, { create: false });
    const file = await handle.getFile();
    if (!Number.isFinite(file.size) || file.size < 0 || file.size > MAX_OPFS_BINDING_BYTES) return null;
    const text = await file.text();
    if (!text.trim()) return null;
    let parsed: unknown;
    try {
      parsed = JSON.parse(text) as unknown;
    } catch {
      return null;
    }
    if (!isRecord(parsed)) return null;
    const obj = parsed as Record<string, unknown>;
    const version = hasOwn(obj, "version") ? obj.version : undefined;
    if (version !== 1) return null;
    const base = hasOwn(obj, "base") ? obj.base : undefined;
    if (!isRecord(base)) return null;
    const out = Object.create(null) as RemoteCacheBinding;
    out.version = 1;
    out.base = base as RemoteCacheBinding["base"];
    return out;
  } catch (err) {
    if (err instanceof DOMException && err.name === "NotFoundError") return null;
    return null;
  }
}

async function writeCacheBinding(fileName: string, binding: RemoteCacheBinding): Promise<void> {
  const handle = await opfsGetDiskFileHandle(fileName, { create: true });
  let writable: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    writable = await handle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
    truncateFallback = true;
  }
  if (truncateFallback) {
    // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
    // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
    try {
      await writable.truncate(0);
    } catch {
      // ignore
    }
  }
  try {
    await writable.write(JSON.stringify(binding, null, 2));
    await writable.close();
  } catch (err) {
    try {
      await writable.abort(err);
    } catch {
      // ignore abort failures
    }
    throw err;
  }
}

async function ensureRemoteCacheBinding(expected: RemoteCacheBinding["base"], cacheFileName: string): Promise<void> {
  const bindingName = cacheBindingFileName(cacheFileName);
  const existing = await readCacheBinding(bindingName);
  if (shouldInvalidateRemoteCache(expected, existing)) {
    await opfsDeleteDisk(cacheFileName);
    await opfsDeleteDisk(bindingName);
    await opfsDeleteDisk(remoteRangeMetaFileName(cacheFileName));
  }
  await writeCacheBinding(bindingName, { version: 1, base: expected });
}

async function ensureRemoteOverlayBinding(expected: RemoteCacheBinding["base"], overlayFileName: string): Promise<void> {
  const bindingName = cacheBindingFileName(overlayFileName);
  const existing = await readCacheBinding(bindingName);
  if (shouldInvalidateRemoteOverlay(expected, existing)) {
    await opfsDeleteDisk(overlayFileName);
    await opfsDeleteDisk(bindingName);
  }
  await writeCacheBinding(bindingName, { version: 1, base: expected });
}

function idbOverlayBindingKey(overlayDiskId: string): string {
  return `overlay-binding:${overlayDiskId}`;
}

async function readIdbOverlayBinding(overlayDiskId: string): Promise<RemoteCacheBinding | null> {
  const key = idbOverlayBindingKey(overlayDiskId);
  const db = await openDiskManagerDb();
  try {
    const tx = db.transaction(["remote_chunk_meta"], "readonly");
    const store = tx.objectStore("remote_chunk_meta");
    const rec = (await idbReq(store.get(key))) as unknown;
    await idbTxDone(tx);
    if (!isRecord(rec)) return null;
    const maybe = rec as Record<string, unknown>;
    const version = hasOwn(maybe, "version") ? maybe.version : undefined;
    if (version !== 1) return null;
    const base = hasOwn(maybe, "base") ? maybe.base : undefined;
    if (!isRecord(base)) return null;
    const out = Object.create(null) as RemoteCacheBinding;
    out.version = 1;
    out.base = base as RemoteCacheBinding["base"];
    return out;
  } finally {
    db.close();
  }
}

async function writeIdbOverlayBinding(overlayDiskId: string, binding: RemoteCacheBinding): Promise<void> {
  const db = await openDiskManagerDb();
  try {
    const tx = db.transaction(["remote_chunk_meta"], "readwrite");
    tx.objectStore("remote_chunk_meta").put({ cacheKey: idbOverlayBindingKey(overlayDiskId), ...binding });
    await idbTxDone(tx);
  } finally {
    db.close();
  }
}

async function ensureIdbRemoteOverlayBinding(expected: RemoteCacheBinding["base"], overlayDiskId: string): Promise<void> {
  const existing = await readIdbOverlayBinding(overlayDiskId);
  if (shouldInvalidateRemoteOverlay(expected, existing)) {
    const db = await openDiskManagerDb();
    try {
      await idbDeleteDiskData(db, overlayDiskId);
      // Best-effort cleanup: remove any stale record under this key.
      const tx = db.transaction(["remote_chunk_meta"], "readwrite");
      tx.objectStore("remote_chunk_meta").delete(idbOverlayBindingKey(overlayDiskId));
      await idbTxDone(tx);
    } finally {
      db.close();
    }
  }
  await writeIdbOverlayBinding(overlayDiskId, { version: 1, base: expected });
}

function defaultRemoteChunkedManifestUrl(base: RemoteCacheBinding["base"]): string {
  // See: docs/18-chunked-disk-image-format.md ("images/<imageId>/<version>/manifest.json").
  // Like `defaultRemoteRangeUrl`, this is intentionally *not* a signed URL.
  return `/images/${encodeURIComponent(base.imageId)}/${encodeURIComponent(base.version)}/manifest.json`;
}

async function openDiskFromMetadata(
  meta: DiskImageMetadata,
  mode: OpenMode,
  overlayBlockSizeBytes?: number,
): Promise<DiskEntry> {
  if (meta.source === "remote") {
    // Preserve `null` to mean "unbounded cache" (no eviction), while `undefined` selects the
    // default bounded cache size.
    const cacheLimitBytes =
      meta.cache.cacheLimitBytes === undefined ? DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES : meta.cache.cacheLimitBytes;

    const remoteCacheBackend = meta.cache.backend;
    if (remoteCacheBackend !== "opfs" && remoteCacheBackend !== "idb") {
      throw new Error(`unsupported remote cache backend ${String(remoteCacheBackend)}`);
    }
    if (meta.remote.delivery !== "range" && meta.remote.delivery !== "chunked") {
      throw new Error(`unsupported remote delivery ${meta.remote.delivery}`);
    }

    // Treat persisted + postMessage-provided metadata as untrusted: never observe inherited fields
    // (prototype pollution) when selecting remote URLs or validators.
    const validatorRaw = meta.remote.validator as unknown;
    const validatorRec = isRecord(validatorRaw) ? (validatorRaw as Record<string, unknown>) : null;
    const etag = validatorRec && hasOwn(validatorRec, "etag") ? validatorRec.etag : undefined;
    const lastModified = validatorRec && hasOwn(validatorRec, "lastModified") ? validatorRec.lastModified : undefined;
    const expectedValidator =
      typeof etag === "string"
        ? { kind: "etag" as const, value: etag }
        : typeof lastModified === "string"
          ? { kind: "lastModified" as const, value: lastModified }
          : undefined;

    const urlsRaw = (meta.remote as unknown as { urls?: unknown }).urls;
    const urlsRec = isRecord(urlsRaw) ? (urlsRaw as Record<string, unknown>) : (Object.create(null) as Record<string, unknown>);
    const stableUrlRaw = hasOwn(urlsRec, "url") ? urlsRec.url : undefined;
    const leaseEndpointRaw = hasOwn(urlsRec, "leaseEndpoint") ? urlsRec.leaseEndpoint : undefined;
    const stableUrl = typeof stableUrlRaw === "string" ? stableUrlRaw.trim() : "";
    const leaseEndpoint = typeof leaseEndpointRaw === "string" ? leaseEndpointRaw.trim() : "";

    const deliveryType =
      meta.remote.delivery === "range"
        ? remoteRangeDeliveryType(meta.cache.chunkSizeBytes)
        : remoteChunkedDeliveryType(meta.cache.chunkSizeBytes);

    const base: RemoteCacheBinding["base"] = {
      imageId: meta.remote.imageId,
      version: meta.remote.version,
      deliveryType,
      ...(leaseEndpoint ? { leaseEndpoint } : {}),
      ...(expectedValidator ? { expectedValidator } : {}),
      chunkSize: meta.cache.chunkSizeBytes,
    };

    const readOnly = meta.kind === "cd" || meta.format === "iso";
    const candidateSnapshot: DiskBackendSnapshot = {
      kind: "remote",
      backend: remoteCacheBackend,
      diskKind: meta.kind,
      sizeBytes: meta.sizeBytes,
      base,
      overlay: {
        fileName: meta.cache.overlayFileName,
        diskSizeBytes: meta.sizeBytes,
        blockSizeBytes: meta.cache.overlayBlockSizeBytes,
      },
      cache: { fileName: meta.cache.fileName, cacheLimitBytes },
    };

    const loc = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
    const origin = loc ? new URL(loc).origin : null;

    const shouldEnableSnapshotForUrl = (resolvedUrl: string, expectedPath: string): boolean => {
      if (!loc || !origin) return false;
      try {
        const u = new URL(resolvedUrl, loc);
        return u.origin === origin && u.pathname === expectedPath && !u.search && !u.hash;
      } catch {
        return false;
      }
    };

    let backendSnapshot: DiskBackendSnapshot | null = null;
    let baseDisk: AsyncSectorDisk;

    if (meta.remote.delivery === "range") {
      const shouldUseSparseRangeDisk =
        remoteCacheBackend === "opfs" && cacheLimitBytes === null && hasOpfsSyncAccessHandle();

      if (shouldUseSparseRangeDisk) {
        await ensureRemoteCacheBinding(base, meta.cache.fileName);

        const cacheKeyParts = { imageId: meta.remote.imageId, version: meta.remote.version, deliveryType: base.deliveryType };
        const metadataStore = opfsRemoteRangeDiskMetadataStore(remoteRangeMetaFileName(meta.cache.fileName));
        const sparseCacheFactory = {
          open: async (_cacheId: string) => await OpfsAeroSparseDisk.open(meta.cache.fileName),
          create: async (_cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }) =>
            await OpfsAeroSparseDisk.create(meta.cache.fileName, opts),
          delete: async (_cacheId: string) => {
            await opfsDeleteDisk(meta.cache.fileName);
          },
        };

        if (stableUrl) {
          baseDisk = await RemoteRangeDisk.open(stableUrl, {
            cacheKeyParts,
            chunkSize: meta.cache.chunkSizeBytes,
            metadataStore,
            sparseCacheFactory,
          });
        } else if (leaseEndpoint) {
          const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "range" });
          baseDisk = await RemoteRangeDisk.openWithLease(
            { sourceId: meta.remote.imageId, lease },
            {
              cacheKeyParts,
              chunkSize: meta.cache.chunkSizeBytes,
              metadataStore,
              sparseCacheFactory,
            },
          );
        } else {
          throw new Error("remote disk metadata missing urls.url and urls.leaseEndpoint");
        }
        if (baseDisk.capacityBytes !== meta.sizeBytes) {
          await baseDisk.close?.();
          throw new Error(`disk size mismatch: expected=${meta.sizeBytes} actual=${baseDisk.capacityBytes}`);
        }
      } else {
        const expectedEtag = expectedValidator?.kind === "etag" ? expectedValidator.value : undefined;
        if (stableUrl) {
          baseDisk = await RemoteStreamingDisk.open(stableUrl, {
            blockSize: meta.cache.chunkSizeBytes,
            cacheBackend: remoteCacheBackend,
            cacheLimitBytes,
            credentials: "same-origin",
            cacheImageId: meta.remote.imageId,
            cacheVersion: meta.remote.version,
            cacheEtag: expectedEtag,
            expectedSizeBytes: meta.sizeBytes,
          });
        } else if (leaseEndpoint) {
          const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "range" });
          await lease.refresh();
          baseDisk = await RemoteStreamingDisk.openWithLease({ sourceId: meta.remote.imageId, lease }, {
            blockSize: meta.cache.chunkSizeBytes,
            cacheBackend: remoteCacheBackend,
            cacheLimitBytes,
            cacheImageId: meta.remote.imageId,
            cacheVersion: meta.remote.version,
            cacheEtag: expectedEtag,
            expectedSizeBytes: meta.sizeBytes,
          });
        } else {
          throw new Error("remote disk metadata missing urls.url and urls.leaseEndpoint");
        }
      }

      if (stableUrl && shouldEnableSnapshotForUrl(stableUrl, defaultRemoteRangeUrl(base))) {
        backendSnapshot = candidateSnapshot;
      } else if (!stableUrl && leaseEndpoint) {
        // Lease-based remote URLs can include short-lived secrets (query params). Snapshots
        // remain safe because we persist only the lease endpoint, not the resolved URL.
        backendSnapshot = candidateSnapshot;
      }
    } else if (meta.remote.delivery === "chunked") {
      if (stableUrl) {
        baseDisk = await RemoteChunkedDisk.open(stableUrl, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          credentials: "same-origin",
          cacheImageId: meta.remote.imageId,
          cacheVersion: meta.remote.version,
        });
      } else if (leaseEndpoint) {
        const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "chunked" });
        // `RemoteChunkedDisk.openWithLease` expects `lease.url` to be set.
        await lease.refresh();
        baseDisk = await RemoteChunkedDisk.openWithLease({ sourceId: meta.remote.imageId, lease }, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          cacheImageId: meta.remote.imageId,
          cacheVersion: meta.remote.version,
        });
      } else {
        throw new Error("remote chunked disk metadata missing urls.url and urls.leaseEndpoint");
      }
      if (baseDisk.capacityBytes !== meta.sizeBytes) {
        await baseDisk.close?.();
        throw new Error(`disk size mismatch: expected=${meta.sizeBytes} actual=${baseDisk.capacityBytes}`);
      }

      if (stableUrl) {
        if (shouldEnableSnapshotForUrl(stableUrl, defaultRemoteChunkedManifestUrl(base))) {
          backendSnapshot = candidateSnapshot;
        }
      } else if (leaseEndpoint) {
        backendSnapshot = candidateSnapshot;
      }
    } else {
      throw new Error(`unsupported remote delivery ${meta.remote.delivery}`);
    }

    if (readOnly) {
      return { disk: baseDisk, readOnly, backendSnapshot };
    }

    try {
      if (remoteCacheBackend === "idb") {
        await ensureIdbRemoteOverlayBinding(base, meta.cache.overlayFileName);
        const disk = await IdbCowDisk.open(baseDisk, meta.cache.overlayFileName, meta.sizeBytes);
        return { disk, readOnly, backendSnapshot };
      }

      await ensureRemoteOverlayBinding(base, meta.cache.overlayFileName);
      let overlay: OpfsAeroSparseDisk | null = null;
      try {
        overlay = await openOpfsSparseDisk(meta.cache.overlayFileName, {
          diskSizeBytes: meta.sizeBytes,
          blockSizeBytes: meta.cache.overlayBlockSizeBytes,
        });
        return { disk: new OpfsCowDisk(baseDisk, overlay), readOnly, backendSnapshot };
      } catch (err) {
        await overlay?.close?.();
        throw err;
      }
    } catch (err) {
      await baseDisk.close?.();
      throw err;
    }

    throw new Error("openDiskFromMetadata(remote): unreachable");
  }

  if (meta.source !== "local") {
    throw new Error("expected local disk metadata");
  }

  const localMeta = meta;
  const readOnly = localMeta.kind === "cd" || localMeta.format === "iso";
  const hasRemoteBase = !!localMeta.remote;
  if (localMeta.backend === "opfs") {
    const fileName = localMeta.fileName;
    const sizeBytes = localMeta.sizeBytes;
    const dirPath = typeof localMeta.opfsDirectory === "string" && localMeta.opfsDirectory.trim() ? localMeta.opfsDirectory.trim() : undefined;
    const snapshotDirPath = dirPath && dirPath !== OPFS_DISKS_PATH ? dirPath : undefined;

    async function openBase(): Promise<AsyncSectorDisk> {
      if (localMeta.remote) {
        // Legacy remote-streaming metadata is persisted; treat it as untrusted and ignore inherited
        // URL fields (prototype pollution).
        const remoteRaw = localMeta.remote as unknown;
        const remoteRec = isRecord(remoteRaw) ? (remoteRaw as Record<string, unknown>) : null;
        const urlRaw = remoteRec && hasOwn(remoteRec, "url") ? remoteRec.url : undefined;
        const url = typeof urlRaw === "string" ? urlRaw.trim() : "";
        if (!url) throw new Error("remote disk metadata missing remote.url");
        // Legacy remote-streaming local disks always use RemoteStreamingDisk + OPFS chunk cache.
        // The base image is treated as read-only; HDD writes go to a runtime COW overlay.
        const blockSizeBytes = remoteRec && hasOwn(remoteRec, "blockSizeBytes") ? remoteRec.blockSizeBytes : undefined;
        const cacheLimitBytes = remoteRec && hasOwn(remoteRec, "cacheLimitBytes") ? remoteRec.cacheLimitBytes : undefined;
        const prefetchSequentialBlocks =
          remoteRec && hasOwn(remoteRec, "prefetchSequentialBlocks") ? remoteRec.prefetchSequentialBlocks : undefined;
        return await RemoteStreamingDisk.open(url, {
          blockSize: typeof blockSizeBytes === "number" ? blockSizeBytes : undefined,
          cacheLimitBytes: typeof cacheLimitBytes === "number" || cacheLimitBytes === null ? (cacheLimitBytes as any) : undefined,
          prefetchSequentialBlocks: typeof prefetchSequentialBlocks === "number" ? prefetchSequentialBlocks : undefined,
          cacheBackend: "opfs",
          expectedSizeBytes: sizeBytes,
        });
      }
      switch (localMeta.format) {
        case "aerospar": {
          const disk = await OpfsAeroSparseDisk.open(fileName, { dirPath });
          if (disk.capacityBytes !== sizeBytes) {
            await disk.close?.();
            throw new Error(`disk size mismatch: expected=${sizeBytes} actual=${disk.capacityBytes}`);
          }
          return disk;
        }
        case "raw":
        case "iso":
        case "unknown":
          return await OpfsRawDisk.open(fileName, { create: false, sizeBytes, dirPath });
        case "qcow2":
        case "vhd":
          throw new Error(`unsupported OPFS disk format ${localMeta.format} (convert to aerospar first)`);
      }
    }

    // For HDD images we default to a COW overlay so the imported base image remains unchanged.
    if (mode === "cow" && !readOnly) {
      let base: AsyncSectorDisk | null = null;
      let overlay: OpfsAeroSparseDisk | null = null;
      try {
        base = await openBase();
        const overlayName = `${localMeta.id}.overlay.aerospar`;

        overlay = await openOpfsSparseDisk(overlayName, {
          diskSizeBytes: localMeta.sizeBytes,
          blockSizeBytes: overlayBlockSizeBytes ?? 1024 * 1024,
          dirPath,
        });

        return {
          disk: new OpfsCowDisk(base, overlay),
          readOnly: false,
          // Remote-streaming local disks cannot currently be snapshotted because the backend snapshot
          // format does not capture the remote base URL/options.
          backendSnapshot: localMeta.remote
            ? null
            : {
                kind: "local",
                backend: "opfs",
                key: localMeta.fileName,
                ...(snapshotDirPath ? { dirPath: snapshotDirPath } : {}),
                format: localMeta.format,
                diskKind: localMeta.kind,
                sizeBytes: localMeta.sizeBytes,
                overlay: {
                  fileName: overlayName,
                  diskSizeBytes: localMeta.sizeBytes,
                  blockSizeBytes: overlay.blockSizeBytes,
                },
              },
        };
      } catch (err) {
        await overlay?.close?.();
        await base?.close?.();
        if (localMeta.remote) {
          // The remote base is read-only, so we cannot fall back to direct writes.
          const msg = err instanceof Error ? err.message : String(err);
          throw new Error(`failed to open COW overlay for remote-streaming disk (id=${localMeta.id}): ${msg}`);
        }
        // If SyncAccessHandle isn't available, sparse overlays can't work efficiently.
        // Fall back to direct raw writes (still in a worker, but slower).
        if (localMeta.format !== "raw" && localMeta.format !== "iso" && localMeta.format !== "unknown") throw err;
      }
    }

    const disk = await openBase();
    return {
      disk,
      // Treat remote-streaming local disks as read-only unless explicitly opened with a
      // COW overlay above.
      readOnly: readOnly || hasRemoteBase,
      // Remote-streaming local disks cannot currently be snapshotted because the backend snapshot
      // format does not capture the remote base URL/options.
      backendSnapshot: localMeta.remote
        ? null
        : {
            kind: "local",
            backend: "opfs",
            key: localMeta.fileName,
            ...(snapshotDirPath ? { dirPath: snapshotDirPath } : {}),
            format: localMeta.format,
            diskKind: localMeta.kind,
            sizeBytes: localMeta.sizeBytes,
          },
    };
  }

  // IndexedDB backend: disk data is stored in the `chunks` store (sparse).
  if (localMeta.format !== "raw" && localMeta.format !== "iso" && localMeta.format !== "unknown") {
    throw new Error(`unsupported IndexedDB disk format ${localMeta.format} (convert to aerospar first)`);
  }
  const disk = await IdbChunkDisk.open(localMeta.id, localMeta.sizeBytes);
  return {
    disk,
    readOnly,
    backendSnapshot: {
      kind: "local",
      backend: "idb",
      key: localMeta.id,
      format: localMeta.format,
      diskKind: localMeta.kind,
      sizeBytes: localMeta.sizeBytes,
    },
  };
}

async function loadSha256Manifest(
  integrity: RemoteDiskIntegritySpec | undefined,
  fetchFn: typeof fetch,
): Promise<string[] | undefined> {
  if (!integrity) return undefined;
  if (integrity.kind === "sha256") {
    if (integrity.sha256.length > MAX_SHA256_MANIFEST_ENTRIES) {
      throw new Error(`sha256 manifest too large: max=${MAX_SHA256_MANIFEST_ENTRIES} got=${integrity.sha256.length}`);
    }
    const out: string[] = [];
    for (const entry of integrity.sha256) {
      const normalized = String(entry).trim().toLowerCase();
      if (!/^[0-9a-f]{64}$/.test(normalized)) {
        throw new Error("sha256 manifest entries must be 64-char hex digests");
      }
      out.push(normalized);
    }
    return out;
  }

  const resp = await fetchFn(integrity.manifestUrl, { method: "GET" });
  if (!resp.ok) throw new Error(`failed to fetch sha256 manifest: ${resp.status}`);

  const json = await readJsonResponseWithLimit(resp, { maxBytes: MAX_SHA256_MANIFEST_JSON_BYTES, label: "sha256 manifest" });
  if (!Array.isArray(json)) {
    throw new Error("sha256 manifest must be a JSON array of hex digests");
  }
  if (json.length > MAX_SHA256_MANIFEST_ENTRIES) {
    throw new Error(`sha256 manifest too large: max=${MAX_SHA256_MANIFEST_ENTRIES} got=${json.length}`);
  }
  const out: string[] = [];
  for (const entry of json) {
    if (typeof entry !== "string") {
      throw new Error("sha256 manifest must be a JSON array of hex digests");
    }
    const normalized = entry.trim().toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(normalized)) {
      throw new Error("sha256 manifest entries must be 64-char hex digests");
    }
    out.push(normalized);
  }
  return out;
}

function remoteDeliveryKind(deliveryType: string): string {
  const idx = deliveryType.indexOf(":");
  return idx === -1 ? deliveryType : deliveryType.slice(0, idx);
}

async function openDiskFromSnapshot(entry: RuntimeDiskSnapshotEntry): Promise<DiskEntry> {
  const backend = entry.backend;
  if (backend.kind === "local") {
    if (backend.backend === "opfs") {
      const dirPath = typeof backend.dirPath === "string" && backend.dirPath.trim() ? backend.dirPath.trim() : undefined;
      let base: AsyncSectorDisk;
      switch (backend.format) {
        case "aerospar": {
          const disk = await OpfsAeroSparseDisk.open(backend.key, { dirPath });
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
          base = await OpfsRawDisk.open(backend.key, { create: false, sizeBytes: backend.sizeBytes, dirPath });
          break;
        case "qcow2":
        case "vhd":
          throw new Error(`unsupported OPFS disk format ${backend.format} (convert to aerospar first)`);
      }

      if (backend.overlay && !entry.readOnly) {
        let overlay: OpfsAeroSparseDisk | null = null;
        try {
          overlay = await openOpfsSparseDisk(backend.overlay.fileName, {
            diskSizeBytes: backend.overlay.diskSizeBytes,
            blockSizeBytes: backend.overlay.blockSizeBytes,
            dirPath,
          });
          return {
            disk: new OpfsCowDisk(base, overlay),
            readOnly: entry.readOnly,
            backendSnapshot: backend,
          };
        } catch (err) {
          await overlay?.close?.();
          await base.close?.();
          throw err;
        }
      }

      return { disk: base, readOnly: entry.readOnly, backendSnapshot: backend };
    }

    if (backend.format !== "raw" && backend.format !== "iso" && backend.format !== "unknown") {
      throw new Error(`unsupported IndexedDB disk format ${backend.format} (convert to aerospar first)`);
    }
    const disk = await IdbChunkDisk.open(backend.key, backend.sizeBytes);
    return { disk, readOnly: entry.readOnly, backendSnapshot: backend };
  }

  // Remote base image with cache + overlay.
  const remoteCacheBackend = backend.backend ?? "opfs";
  if (remoteCacheBackend !== "opfs" && remoteCacheBackend !== "idb") {
    throw new Error(`unsupported remote cache backend ${String(remoteCacheBackend)}`);
  }
  const kind = remoteDeliveryKind(backend.base.deliveryType);
  const cacheLimitBytes = backend.cache.cacheLimitBytes;

  if (kind !== "range" && kind !== "chunked") {
    throw new Error(`unsupported remote deliveryType=${backend.base.deliveryType}`);
  }

  const leaseEndpoint = typeof backend.base.leaseEndpoint === "string" ? backend.base.leaseEndpoint.trim() : "";

  let base: AsyncSectorDisk;
  if (remoteCacheBackend === "opfs") {
    if (kind === "range") {
      const shouldUseSparseRangeDisk = cacheLimitBytes === null && hasOpfsSyncAccessHandle();
      if (shouldUseSparseRangeDisk) {
        await ensureRemoteCacheBinding(backend.base, backend.cache.fileName);
        const cacheKeyParts = {
          imageId: backend.base.imageId,
          version: backend.base.version,
          deliveryType: backend.base.deliveryType,
        };
        const metadataStore = opfsRemoteRangeDiskMetadataStore(remoteRangeMetaFileName(backend.cache.fileName));
        const sparseCacheFactory = {
          open: async (_cacheId: string) => await OpfsAeroSparseDisk.open(backend.cache.fileName),
          create: async (_cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }) =>
            await OpfsAeroSparseDisk.create(backend.cache.fileName, opts),
          delete: async (_cacheId: string) => {
            await opfsDeleteDisk(backend.cache.fileName);
          },
        };

        if (leaseEndpoint) {
          const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "range" });
          base = await RemoteRangeDisk.openWithLease(
            { sourceId: backend.base.imageId, lease },
            { cacheKeyParts, chunkSize: backend.base.chunkSize, metadataStore, sparseCacheFactory },
          );
        } else {
          const url = defaultRemoteRangeUrl(backend.base);
          base = await RemoteRangeDisk.open(url, {
            cacheKeyParts,
            chunkSize: backend.base.chunkSize,
            metadataStore,
            sparseCacheFactory,
          });
        }

        if (base.capacityBytes !== backend.sizeBytes) {
          await base.close?.();
          throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${base.capacityBytes}`);
        }
      } else {
        const expectedEtag = backend.base.expectedValidator?.kind === "etag" ? backend.base.expectedValidator.value : undefined;
        if (leaseEndpoint) {
          const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "range" });
          await lease.refresh();
          base = await RemoteStreamingDisk.openWithLease({ sourceId: backend.base.imageId, lease }, {
            blockSize: backend.base.chunkSize,
            cacheBackend: remoteCacheBackend,
            cacheLimitBytes,
            cacheImageId: backend.base.imageId,
            cacheVersion: backend.base.version,
            cacheEtag: expectedEtag,
            expectedSizeBytes: backend.sizeBytes,
          });
        } else {
          const url = defaultRemoteRangeUrl(backend.base);
          base = await RemoteStreamingDisk.open(url, {
            blockSize: backend.base.chunkSize,
            cacheBackend: remoteCacheBackend,
            cacheLimitBytes,
            credentials: "same-origin",
            cacheImageId: backend.base.imageId,
            cacheVersion: backend.base.version,
            cacheEtag: expectedEtag,
            expectedSizeBytes: backend.sizeBytes,
          });
        }
      }
    } else {
      if (leaseEndpoint) {
        const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "chunked" });
        await lease.refresh();
        base = await RemoteChunkedDisk.openWithLease({ sourceId: backend.base.imageId, lease }, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      } else {
        const manifestUrl = defaultRemoteChunkedManifestUrl(backend.base);
        base = await RemoteChunkedDisk.open(manifestUrl, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          credentials: "same-origin",
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      }

      if (base.capacityBytes !== backend.sizeBytes) {
        await base.close?.();
        throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${base.capacityBytes}`);
      }
    }
  } else {
    if (kind === "range") {
      const expectedEtag = backend.base.expectedValidator?.kind === "etag" ? backend.base.expectedValidator.value : undefined;
      if (leaseEndpoint) {
        const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "range" });
        await lease.refresh();
        base = await RemoteStreamingDisk.openWithLease({ sourceId: backend.base.imageId, lease }, {
          blockSize: backend.base.chunkSize,
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
          cacheEtag: expectedEtag,
          expectedSizeBytes: backend.sizeBytes,
        });
      } else {
        const url = defaultRemoteRangeUrl(backend.base);
        base = await RemoteStreamingDisk.open(url, {
          blockSize: backend.base.chunkSize,
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          credentials: "same-origin",
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
          cacheEtag: expectedEtag,
          expectedSizeBytes: backend.sizeBytes,
        });
      }
    } else {
      if (leaseEndpoint) {
        const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "chunked" });
        await lease.refresh();
        base = await RemoteChunkedDisk.openWithLease({ sourceId: backend.base.imageId, lease }, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      } else {
        const manifestUrl = defaultRemoteChunkedManifestUrl(backend.base);
        base = await RemoteChunkedDisk.open(manifestUrl, {
          cacheBackend: remoteCacheBackend,
          cacheLimitBytes,
          credentials: "same-origin",
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      }

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
      backendSnapshot: backend,
    };
  }

  if (remoteCacheBackend === "idb") {
    await ensureIdbRemoteOverlayBinding(backend.base, backend.overlay.fileName);
    try {
      const disk = await IdbCowDisk.open(base, backend.overlay.fileName, backend.sizeBytes);
      return {
        disk,
        readOnly: entry.readOnly,
        backendSnapshot: backend,
      };
    } catch (err) {
      await base.close?.();
      throw err;
    }
  }

  await ensureRemoteOverlayBinding(backend.base, backend.overlay.fileName);
  let overlay: OpfsAeroSparseDisk | null = null;
  try {
    overlay = await openOpfsSparseDisk(backend.overlay.fileName, {
      diskSizeBytes: backend.overlay.diskSizeBytes,
      blockSizeBytes: backend.overlay.blockSizeBytes,
    });
    return {
      disk: new OpfsCowDisk(base, overlay),
      readOnly: entry.readOnly,
      backendSnapshot: backend,
    };
  } catch (err) {
    await overlay?.close?.();
    await base.close?.();
    throw err;
  }
}

async function openRemoteBackedDisk(
  remoteSpec: DiskOpenSpec & { kind: "remote" },
  mode: OpenMode,
  overlayBlockSizeBytes?: number,
): Promise<DiskEntry> {
  // Treat remote open specs as untrusted (postMessage). Never observe inherited fields.
  const remoteRaw = (remoteSpec as unknown as { remote?: unknown }).remote;
  if (!isRecord(remoteRaw)) {
    throw new Error("invalid remote disk spec (expected object)");
  }
  const remote = remoteRaw as Record<string, unknown>;

  const delivery = hasOwn(remote, "delivery") ? remote.delivery : undefined;
  if (delivery !== "range" && delivery !== "chunked") {
    throw new Error(`invalid remote delivery=${String(delivery)}`);
  }

  const diskKind = hasOwn(remote, "kind") ? remote.kind : undefined;
  if (diskKind !== "hdd" && diskKind !== "cd") {
    throw new Error(`invalid remote kind=${String(diskKind)}`);
  }

  const format = hasOwn(remote, "format") ? remote.format : undefined;
  if (format !== "raw" && format !== "iso") {
    throw new Error(`invalid remote format=${String(format)}`);
  }

  const cacheKeyRaw = hasOwn(remote, "cacheKey") ? remote.cacheKey : undefined;
  if (typeof cacheKeyRaw !== "string" || !cacheKeyRaw.trim()) {
    throw new Error("remote cacheKey must not be empty");
  }
  const cacheKey = cacheKeyRaw;

  const readOnlyBase = diskKind === "cd" || format === "iso";

  const credentialsRaw = hasOwn(remote, "credentials") ? remote.credentials : undefined;
  const credentials = credentialsRaw === undefined ? "same-origin" : credentialsRaw;
  if (credentials !== "same-origin" && credentials !== "include" && credentials !== "omit") {
    throw new Error(`invalid remote credentials=${String(credentialsRaw)}`);
  }
  const fetchFn: typeof fetch = (input, init = {}) => fetch(input, { ...init, credentials: init.credentials ?? credentials });

  const imageIdRaw = hasOwn(remote, "imageId") ? remote.imageId : undefined;
  const imageId = typeof imageIdRaw === "string" ? imageIdRaw : undefined;
  const cacheImageId = (imageId ?? cacheKey).trim();
  if (!cacheImageId) throw new Error("remote cacheKey must not be empty");

  const versionRaw = hasOwn(remote, "version") ? remote.version : undefined;
  const version = typeof versionRaw === "string" ? versionRaw : undefined;
  const rangeCacheVersion = (version ?? "1").trim();
  if (!rangeCacheVersion) throw new Error("remote version must not be empty");

  const cacheBackendRaw = hasOwn(remote, "cacheBackend") ? remote.cacheBackend : undefined;
  const cacheBackend = (cacheBackendRaw ?? pickDefaultBackend()) as DiskBackend;
  if (cacheBackend !== "opfs" && cacheBackend !== "idb") {
    throw new Error(`invalid remote cacheBackend=${String(cacheBackendRaw)}`);
  }

  const cacheLimitBytes = hasOwn(remote, "cacheLimitBytes") ? remote.cacheLimitBytes : undefined;
  if (cacheLimitBytes !== undefined && cacheLimitBytes !== null) {
    if (typeof cacheLimitBytes !== "number" || !Number.isSafeInteger(cacheLimitBytes) || cacheLimitBytes < 0) {
      throw new Error(`invalid remote cacheLimitBytes=${String(cacheLimitBytes)}`);
    }
  }

  let integrity: RemoteDiskIntegritySpec | undefined = undefined;
  if (hasOwn(remote, "integrity")) {
    const rawIntegrity = remote.integrity;
    if (rawIntegrity !== undefined && rawIntegrity !== null) {
      if (!isRecord(rawIntegrity)) {
        throw new Error("invalid integrity spec (expected object)");
      }
      const rec = rawIntegrity as Record<string, unknown>;
      const kind = hasOwn(rec, "kind") ? rec.kind : undefined;
      if (kind === "sha256") {
        const shaRaw = hasOwn(rec, "sha256") ? rec.sha256 : undefined;
        if (!Array.isArray(shaRaw)) {
          throw new Error("invalid sha256 integrity spec (expected sha256: string[])");
        }
        integrity = { kind: "sha256", sha256: shaRaw.map((v) => String(v)) };
      } else if (kind === "manifest") {
        const manifestUrl = hasOwn(rec, "manifestUrl") ? rec.manifestUrl : undefined;
        if (typeof manifestUrl !== "string" || !manifestUrl.trim()) {
          throw new Error("invalid integrity manifestUrl");
        }
        integrity = { kind: "manifest", manifestUrl };
      } else {
        throw new Error(`invalid integrity kind=${String(kind)}`);
      }
    }
  }

  let base: AsyncSectorDisk;
  if (delivery === "range") {
    const urlRaw = hasOwn(remote, "url") ? remote.url : undefined;
    if (typeof urlRaw !== "string" || !urlRaw.trim()) {
      throw new Error("invalid remote url");
    }
    const url = urlRaw;

    const chunkSizeRaw = hasOwn(remote, "chunkSizeBytes") ? remote.chunkSizeBytes : undefined;
    const chunkSize =
      typeof chunkSizeRaw === "number" && Number.isSafeInteger(chunkSizeRaw) && chunkSizeRaw > 0
        ? chunkSizeRaw
        : RANGE_STREAM_CHUNK_SIZE;

    // `RemoteRangeDisk` uses an unbounded sparse OPFS cache (no eviction); only select it
    // when the caller explicitly requested an unbounded OPFS cache.
    if (cacheBackend === "opfs" && cacheLimitBytes === null && hasOpfsSyncAccessHandle()) {
      // Backward-compat cleanup: older clients keyed range caches as `deliveryType: "range"`,
      // which collides across different chunkSize values.
      await bestEffortDeleteLegacyRemoteRangeCache(cacheImageId, rangeCacheVersion);

      base = await RemoteRangeDisk.open(url, {
        cacheKeyParts: {
          imageId: cacheImageId,
          version: rangeCacheVersion,
          deliveryType: remoteRangeDeliveryType(chunkSize),
        },
        credentials,
        chunkSize,
        sha256Manifest: await loadSha256Manifest(integrity, fetchFn),
        fetchFn,
      });
    } else {
      // Use a null-prototype options bag so remote disk option reads can never observe inherited
      // values (e.g. `Object.prototype.cacheLimitBytes`).
      const opts = Object.create(null) as RemoteDiskOptions;
      opts.credentials = credentials;
      opts.cacheBackend = cacheBackend;
      // Preserve `null` semantics for unbounded cache; `undefined` selects defaults.
      opts.cacheLimitBytes = cacheLimitBytes as any;
      opts.cacheImageId = cacheImageId;
      opts.cacheVersion = rangeCacheVersion;
      opts.blockSize = chunkSize;
      base = await RemoteStreamingDisk.open(url, opts);
    }
  } else {
    const manifestUrlRaw = hasOwn(remote, "manifestUrl") ? remote.manifestUrl : undefined;
    if (typeof manifestUrlRaw !== "string" || !manifestUrlRaw.trim()) {
      throw new Error("invalid remote manifestUrl");
    }
    const opts = Object.create(null) as RemoteChunkedDiskOpenOptions;
    (opts as any).credentials = credentials;
    (opts as any).cacheBackend = cacheBackend;
    (opts as any).cacheLimitBytes = cacheLimitBytes as any;
    (opts as any).cacheImageId = cacheImageId;
    (opts as any).cacheVersion = version;
    base = await RemoteChunkedDisk.open(manifestUrlRaw, opts);
  }

  // Remote base images are always treated as read-only. For HDDs, default to a local
  // COW overlay so the guest can write without mutating the base.
  if (mode === "cow" && !readOnlyBase && diskKind === "hdd") {
    const key = safeOpfsNameComponent(cacheKey);
    const overlayName = `${key}.overlay.aerospar`;
    let overlay: OpfsAeroSparseDisk | null = null;
    try {
      overlay = await openOpfsSparseDisk(overlayName, {
        diskSizeBytes: base.capacityBytes,
        blockSizeBytes: overlayBlockSizeBytes ?? 1024 * 1024,
      });

      return { disk: new OpfsCowDisk(base, overlay), readOnly: false, backendSnapshot: null } satisfies DiskEntry;
    } catch (err) {
      await overlay?.close?.();
      await base.close?.();
      throw err;
    }
  }

  return { disk: base, readOnly: true, backendSnapshot: null } satisfies DiskEntry;
}

export const defaultOpenDisk: OpenDiskFn = async (spec, mode, overlayBlockSizeBytes) => {
  if (spec.kind === "local") {
    return openDiskFromMetadata(spec.meta, mode, overlayBlockSizeBytes);
  }
  return openRemoteBackedDisk(spec, mode, overlayBlockSizeBytes);
};

export class RuntimeDiskWorker {
  private readonly disks = new Map<number, TrackedDiskEntry>();
  private nextHandle = 1;
  private requestChain: Promise<void> = Promise.resolve();

  constructor(
    private readonly postMessage: (msg: RuntimeDiskResponseMessage, transfer?: Transferable[]) => void,
    private readonly openDisk: OpenDiskFn = defaultOpenDisk,
  ) {}

  private postOk(requestId: number, result: unknown, transfer?: Transferable[]): void {
    const msg: RuntimeDiskResponseMessage = { type: "response", requestId, ok: true, result };
    this.postMessage(msg, transfer ?? []);
  }

  private postErr(requestId: number, err: unknown): void {
    const msg: RuntimeDiskResponseMessage = { type: "response", requestId, ok: false, error: serializeError(err) };
    this.postMessage(msg);
  }

  handleMessage(msg: unknown): Promise<void> {
    if (!isRecord(msg)) return Promise.resolve();
    // Treat postMessage payloads as untrusted; ignore inherited fields (prototype pollution).
    const type = hasOwn(msg, "type") ? msg.type : undefined;
    if (type !== "request") return Promise.resolve();
    const requestId = hasOwn(msg, "requestId") ? msg.requestId : undefined;
    if (typeof requestId !== "number" || !Number.isSafeInteger(requestId) || requestId < 0) {
      return Promise.resolve();
    }
    const op = hasOwn(msg, "op") ? msg.op : undefined;
    if (typeof op !== "string" || !op.trim()) {
      this.postErr(requestId, new Error(`invalid runtime disk op ${String(op)}`));
      return Promise.resolve();
    }
    const payload = hasOwn(msg, "payload") ? msg.payload : undefined;

    const req = Object.create(null) as RuntimeDiskRequestMessage;
    (req as any).type = "request";
    (req as any).requestId = requestId;
    (req as any).op = op;
    (req as any).payload = payload;

    this.requestChain = this.requestChain.then(async () => {
      try {
        await this.handleRequest(req);
      } catch (err) {
        this.postErr(requestId, err);
      }
    });
    return this.requestChain;
  }

  private async requireDisk(handle: number): Promise<TrackedDiskEntry> {
    const entry = this.disks.get(handle);
    if (!entry) throw new Error(`unknown disk handle ${handle}`);
    return entry;
  }

  private normalizeOpenPayload(payload: OpenRequestPayload | any): OpenRequestPayload {
    if (!isRecord(payload)) {
      throw new Error("invalid open payload (expected object)");
    }
    // Backward-compat: older clients sent `{ meta, ... }`.
    if (hasOwn(payload, "meta") && !hasOwn(payload, "spec")) {
      return { ...(payload as Record<string, unknown>), spec: { kind: "local", meta: (payload as Record<string, unknown>).meta } } as OpenRequestPayload;
    }
    if (!hasOwn(payload, "spec")) {
      throw new Error("invalid open payload (missing spec)");
    }
    return payload as OpenRequestPayload;
  }

  private async handleRequest(msg: RuntimeDiskRequestMessage): Promise<void> {
    switch (msg.op) {
      case "open": {
        const payload = this.normalizeOpenPayload(msg.payload);
        const payloadRec = payload as unknown as Record<string, unknown>;
        const spec = normalizeDiskOpenSpec(payload.spec);
        const modeRaw = hasOwn(payloadRec, "mode") ? payloadRec.mode : undefined;
        const mode = modeRaw === undefined ? "cow" : modeRaw;
        if (mode !== "cow" && mode !== "direct") {
          throw new Error(`invalid mode=${String(mode)}`);
        }
        const overlayBlockSizeBytes = hasOwn(payloadRec, "overlayBlockSizeBytes") ? payloadRec.overlayBlockSizeBytes : undefined;
        const entry = await this.openDisk(spec, mode, overlayBlockSizeBytes as any);
        const handle = this.nextHandle++;
        this.disks.set(handle, { ...entry, io: emptyIoTelemetry() });
        this.postOk(msg.requestId, {
          handle,
          sectorSize: entry.disk.sectorSize,
          capacityBytes: entry.disk.capacityBytes,
          readOnly: entry.readOnly,
        });
        return;
      }

      case "openRemote": {
        if (!isRecord(msg.payload)) {
          throw new Error("invalid openRemote payload (expected object)");
        }
        const payload = msg.payload as Record<string, unknown>;
        const urlRaw = hasOwn(payload, "url") ? payload.url : undefined;
        if (typeof urlRaw !== "string" || !urlRaw.trim()) {
          throw new Error("invalid openRemote url");
        }
        const optionsRaw = hasOwn(payload, "options") ? payload.options : undefined;
        const options = optionsRaw === undefined ? undefined : (optionsRaw as RemoteDiskOptions);
        const entry: DiskEntry = { disk: await openRemoteDisk(urlRaw, options), readOnly: true, backendSnapshot: null };
        const handle = this.nextHandle++;
        this.disks.set(handle, { ...entry, io: emptyIoTelemetry() });
        this.postOk(msg.requestId, {
          handle,
          sectorSize: entry.disk.sectorSize,
          capacityBytes: entry.disk.capacityBytes,
          readOnly: entry.readOnly,
        });
        return;
      }

      case "openChunked": {
        if (!isRecord(msg.payload)) {
          throw new Error("invalid openChunked payload (expected object)");
        }
        const payload = msg.payload as Record<string, unknown>;
        const manifestUrlRaw = hasOwn(payload, "manifestUrl") ? payload.manifestUrl : undefined;
        if (typeof manifestUrlRaw !== "string" || !manifestUrlRaw.trim()) {
          throw new Error("invalid openChunked manifestUrl");
        }
        const optionsRaw = hasOwn(payload, "options") ? payload.options : undefined;
        const options = optionsRaw === undefined ? undefined : (optionsRaw as RemoteChunkedDiskOpenOptions);
        const entry: DiskEntry = {
          disk: await RemoteChunkedDisk.open(manifestUrlRaw, options),
          readOnly: true,
          backendSnapshot: null,
        };
        const handle = this.nextHandle++;
        this.disks.set(handle, { ...entry, io: emptyIoTelemetry() });
        this.postOk(msg.requestId, {
          handle,
          sectorSize: entry.disk.sectorSize,
          capacityBytes: entry.disk.capacityBytes,
          readOnly: entry.readOnly,
        });
        return;
      }

      case "close": {
        if (!isRecord(msg.payload)) throw new Error("invalid close payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const entry = await this.requireDisk(handle);
        await entry.disk.close?.();
        this.disks.delete(handle);
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "flush": {
        if (!isRecord(msg.payload)) throw new Error("invalid flush payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const entry = await this.requireDisk(handle);
        const start = performance.now();
        entry.io.flushes++;
        entry.io.inflightFlushes++;
        try {
          await entry.disk.flush();
        } finally {
          entry.io.inflightFlushes--;
          entry.io.lastFlushMs = performance.now() - start;
        }
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "clearCache": {
        if (!isRecord(msg.payload)) throw new Error("invalid clearCache payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const entry = await this.requireDisk(handle);
        const diskAny = entry.disk as unknown as { clearCache?: () => Promise<void> };
        if (typeof diskAny.clearCache !== "function") {
          throw new Error("disk does not support cache clearing");
        }
        await diskAny.clearCache();
        entry.io = emptyIoTelemetry();
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "read": {
        if (!isRecord(msg.payload)) throw new Error("invalid read payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const lba = requireSafeNonNegativeInteger(hasOwn(payload, "lba") ? payload.lba : undefined, "lba");
        const byteLength = requireSafeNonNegativeInteger(hasOwn(payload, "byteLength") ? payload.byteLength : undefined, "byteLength");
        const entry = await this.requireDisk(handle);
        if (byteLength > RUNTIME_DISK_MAX_IO_BYTES) {
          throw new Error(`read too large: ${byteLength} bytes (max ${RUNTIME_DISK_MAX_IO_BYTES})`);
        }
        assertSectorAligned(byteLength, entry.disk.sectorSize);
        checkedOffset(lba, byteLength, entry.disk.sectorSize);
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
        this.postOk(msg.requestId, { data: buf }, [buf.buffer]);
        return;
      }

      case "readInto": {
        if (!isRecord(msg.payload)) throw new Error("invalid readInto payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const lba = requireSafeNonNegativeInteger(hasOwn(payload, "lba") ? payload.lba : undefined, "lba");
        const byteLengthRaw = hasOwn(payload, "byteLength") ? payload.byteLength : undefined;
        const dest = hasOwn(payload, "dest") ? payload.dest : undefined;
        const entry = await this.requireDisk(handle);

        const byteLength = requireSafeNonNegativeInteger(byteLengthRaw, "byteLength");
        if (byteLength > RUNTIME_DISK_MAX_IO_BYTES) {
          throw new Error(`readInto too large: ${byteLength} bytes (max ${RUNTIME_DISK_MAX_IO_BYTES})`);
        }
        assertSectorAligned(byteLength, entry.disk.sectorSize);
        checkedOffset(lba, byteLength, entry.disk.sectorSize);

        if (!isRecord(dest)) {
          throw new Error("invalid dest");
        }
        const destRecord = dest as Record<string, unknown>;
        const sab = hasOwn(destRecord, "sab") ? destRecord.sab : undefined;
        const offsetBytes = hasOwn(destRecord, "offsetBytes") ? destRecord.offsetBytes : undefined;
        const { view } = createSabView(sab, offsetBytes, byteLength, "dest");

        const start = performance.now();
        entry.io.reads++;
        entry.io.bytesRead += byteLength;
        entry.io.inflightReads++;
        try {
          if (byteLength > 0) {
            await entry.disk.readSectors(lba, view);
          }
        } finally {
          entry.io.inflightReads--;
          entry.io.lastReadMs = performance.now() - start;
        }
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "write": {
        if (!isRecord(msg.payload)) throw new Error("invalid write payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const lba = requireSafeNonNegativeInteger(hasOwn(payload, "lba") ? payload.lba : undefined, "lba");
        const data = hasOwn(payload, "data") ? payload.data : undefined;
        const entry = await this.requireDisk(handle);
        if (entry.readOnly) throw new Error("disk is read-only");
        if (!(data instanceof Uint8Array)) {
          throw new Error("invalid write payload (expected Uint8Array)");
        }
        if (data.byteLength > RUNTIME_DISK_MAX_IO_BYTES) {
          throw new Error(`write too large: ${data.byteLength} bytes (max ${RUNTIME_DISK_MAX_IO_BYTES})`);
        }
        assertSectorAligned(data.byteLength, entry.disk.sectorSize);
        checkedOffset(lba, data.byteLength, entry.disk.sectorSize);
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
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "writeFrom": {
        if (!isRecord(msg.payload)) throw new Error("invalid writeFrom payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const lba = requireSafeNonNegativeInteger(hasOwn(payload, "lba") ? payload.lba : undefined, "lba");
        const src = hasOwn(payload, "src") ? payload.src : undefined;
        const entry = await this.requireDisk(handle);
        if (entry.readOnly) throw new Error("disk is read-only");

        if (!isRecord(src)) {
          throw new Error("invalid src");
        }
        const srcRecord = src as Record<string, unknown>;
        const byteLength = requireSafeNonNegativeInteger(
          hasOwn(srcRecord, "byteLength") ? srcRecord.byteLength : undefined,
          "src.byteLength",
        );
        if (byteLength > RUNTIME_DISK_MAX_IO_BYTES) {
          throw new Error(`writeFrom too large: ${byteLength} bytes (max ${RUNTIME_DISK_MAX_IO_BYTES})`);
        }
        assertSectorAligned(byteLength, entry.disk.sectorSize);
        checkedOffset(lba, byteLength, entry.disk.sectorSize);

        const sab = hasOwn(srcRecord, "sab") ? srcRecord.sab : undefined;
        const offsetBytes = hasOwn(srcRecord, "offsetBytes") ? srcRecord.offsetBytes : undefined;
        const { view } = createSabView(sab, offsetBytes, byteLength, "src");

        const start = performance.now();
        entry.io.writes++;
        entry.io.bytesWritten += byteLength;
        entry.io.inflightWrites++;
        try {
          if (byteLength > 0) {
            await entry.disk.writeSectors(lba, view);
          }
        } finally {
          entry.io.inflightWrites--;
          entry.io.lastWriteMs = performance.now() - start;
        }
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "stats": {
        if (!isRecord(msg.payload)) throw new Error("invalid stats payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const entry = await this.requireDisk(handle);
        const diskAny = entry.disk as unknown as { getTelemetrySnapshot?: () => RemoteDiskTelemetrySnapshot };
        const remote = typeof diskAny.getTelemetrySnapshot === "function" ? diskAny.getTelemetrySnapshot() : null;
        this.postOk(msg.requestId, {
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
        for (const entry of this.disks.values()) {
          await entry.disk.flush();
          const backend = entry.backendSnapshot;
          if (!backend) {
            throw new Error("disk backend does not support snapshotting (missing backend descriptor)");
          }
          if (backend.kind === "remote") {
            if ((backend.backend ?? "opfs") === "opfs" && remoteDeliveryKind(backend.base.deliveryType) === "range") {
              await writeCacheBinding(cacheBindingFileName(backend.cache.fileName), { version: 1, base: backend.base });
            }
          }
        }

        const ordered = Array.from(this.disks.entries()).sort(([a], [b]) => a - b);
        const disksSnapshot: RuntimeDiskSnapshotEntry[] = ordered.map(([handle, entry]) => {
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
          nextHandle: this.nextHandle,
          disks: disksSnapshot,
        };
        const state = serializeRuntimeDiskSnapshot(snapshot);
        this.postOk(msg.requestId, { state }, [state.buffer]);
        return;
      }

      case "restoreFromSnapshot": {
        if (!isRecord(msg.payload)) throw new Error("invalid restoreFromSnapshot payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const state = hasOwn(payload, "state") ? payload.state : undefined;
        if (!(state instanceof Uint8Array)) {
          throw new Error("invalid restoreFromSnapshot state (expected Uint8Array)");
        }
        const snapshot = deserializeRuntimeDiskSnapshot(state);

        for (const entry of this.disks.values()) {
          await entry.disk.close?.();
        }
        this.disks.clear();

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

        this.nextHandle = desiredNextHandle;
        for (const [handle, entry] of opened.entries()) {
          this.disks.set(handle, { ...entry, io: emptyIoTelemetry() });
        }
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "bench": {
        if (!isRecord(msg.payload)) throw new Error("invalid bench payload (expected object)");
        const payload = msg.payload as Record<string, unknown>;
        const handle = requireSafeNonNegativeInteger(hasOwn(payload, "handle") ? payload.handle : undefined, "handle");
        const entry = await this.requireDisk(handle);

        const totalBytes = requireSafeNonNegativeInteger(hasOwn(payload, "totalBytes") ? payload.totalBytes : undefined, "totalBytes");
        const chunkBytesRaw = hasOwn(payload, "chunkBytes") ? payload.chunkBytes : undefined;
        const chunkBytes = chunkBytesRaw === undefined ? undefined : requireSafeNonNegativeInteger(chunkBytesRaw, "chunkBytes");
        if (chunkBytes !== undefined && chunkBytes > RUNTIME_DISK_MAX_IO_BYTES) {
          throw new Error(`bench chunkBytes too large: ${chunkBytes} bytes (max ${RUNTIME_DISK_MAX_IO_BYTES})`);
        }

        const mode = hasOwn(payload, "mode") ? payload.mode : undefined;
        const selected = mode ?? "rw";
        if (selected !== "read" && selected !== "write" && selected !== "rw") {
          throw new Error(`invalid mode=${String(selected)}`);
        }
        const results: Record<string, unknown> = {};

        if (selected === "write" || selected === "rw") {
          results.write = await benchSequentialWrite(entry.disk, { totalBytes, chunkBytes });
        }
        if (selected === "read" || selected === "rw") {
          results.read = await benchSequentialRead(entry.disk, { totalBytes, chunkBytes });
        }

        this.postOk(msg.requestId, results);
        return;
      }

      default: {
        // Defensive: the protocol evolves over time, and structured-cloned messages can
        // be malformed. Always reply with an error instead of silently dropping the request,
        // otherwise the client will hang forever waiting for a response.
        const op = (msg as unknown as { op?: unknown }).op;
        throw new Error(`unsupported runtime disk op ${String(op)}`);
      }
    }
  }
}
