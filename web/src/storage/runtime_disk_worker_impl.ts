import { OpfsCowDisk } from "./opfs_cow";
import { OpfsRawDisk } from "./opfs_raw";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import type { AsyncSectorDisk } from "./disk";
import { IdbCowDisk } from "./idb_cow";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { benchSequentialRead, benchSequentialWrite } from "./bench";
import {
  hasOpfsSyncAccessHandle,
  idbReq,
  idbTxDone,
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
import { RemoteChunkedDisk } from "./remote_chunked_disk";
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
  const cacheBackend: DiskBackend = options?.cacheBackend ?? pickDefaultBackend();
  const cacheLimitBytes = options?.cacheLimitBytes;
  // `RemoteRangeDisk` uses OPFS sparse files (requires SyncAccessHandle) and does not
  // implement cache eviction. Only select it when OPFS is explicitly requested and
  // caching has not been disabled via `cacheLimitBytes: 0`.
  if (cacheBackend === "opfs" && cacheLimitBytes !== 0 && hasOpfsSyncAccessHandle()) {
    const chunkSize = options?.blockSize ?? RANGE_STREAM_CHUNK_SIZE;
    const cacheKeyParts = {
      imageId: (options?.cacheImageId ?? stableImageIdFromUrl(url)).trim(),
      version: (options?.cacheVersion ?? "1").trim(),
      deliveryType: remoteRangeDeliveryType(chunkSize),
    };
    if (!cacheKeyParts.imageId) throw new Error("cacheImageId must not be empty");
    if (!cacheKeyParts.version) throw new Error("cacheVersion must not be empty");

    // Backward-compat cleanup: older clients used `deliveryType: "range"`, which can collide
    // across different chunkSize configurations for the same image/version.
    await bestEffortDeleteLegacyRemoteRangeCache(cacheKeyParts.imageId, cacheKeyParts.version);
    return await RemoteRangeDisk.open(url, {
      cacheKeyParts,
      credentials: options?.credentials,
      chunkSize,
      readAheadChunks: options?.prefetchSequentialBlocks,
    });
  }

  return await RemoteStreamingDisk.open(url, { ...(options ?? {}), cacheBackend });
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
  opts: { diskSizeBytes: number; blockSizeBytes: number },
): Promise<OpfsAeroSparseDisk> {
  try {
    const opened = await OpfsAeroSparseDisk.open(name);
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
      const writable = await handle.createWritable({ keepExistingData: false });
      await writable.write(JSON.stringify(meta, null, 2));
      await writable.close();
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
    if (!parsed || typeof parsed !== "object") return null;
    const obj = parsed as Partial<RemoteCacheBinding>;
    if (obj.version !== 1) return null;
    if (!obj.base || typeof obj.base !== "object") return null;
    return obj as RemoteCacheBinding;
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
    if (!rec || typeof rec !== "object") return null;
    const maybe = rec as Partial<RemoteCacheBinding> & { cacheKey?: unknown };
    if (maybe.version !== 1) return null;
    if (!maybe.base || typeof maybe.base !== "object") return null;
    return maybe as RemoteCacheBinding;
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
    const remoteCacheBackend = meta.cache.backend;
    if (remoteCacheBackend !== "opfs" && remoteCacheBackend !== "idb") {
      throw new Error(`unsupported remote cache backend ${String(remoteCacheBackend)}`);
    }
    if (meta.remote.delivery !== "range" && meta.remote.delivery !== "chunked") {
      throw new Error(`unsupported remote delivery ${meta.remote.delivery}`);
    }

    const expectedValidator = meta.remote.validator?.etag
      ? { kind: "etag" as const, value: meta.remote.validator.etag }
      : meta.remote.validator?.lastModified
        ? { kind: "lastModified" as const, value: meta.remote.validator.lastModified }
        : undefined;

    const stableUrl = typeof meta.remote.urls.url === "string" ? meta.remote.urls.url.trim() : "";
    const leaseEndpoint = typeof meta.remote.urls.leaseEndpoint === "string" ? meta.remote.urls.leaseEndpoint.trim() : "";

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
      cache: { fileName: meta.cache.fileName },
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

    if (remoteCacheBackend === "opfs") {
      if (meta.remote.delivery === "range") {
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

        if (stableUrl) {
          if (shouldEnableSnapshotForUrl(stableUrl, defaultRemoteRangeUrl(base))) {
            backendSnapshot = candidateSnapshot;
          }
        } else if (leaseEndpoint) {
          // Lease-based remote URLs can include short-lived secrets (query params). Snapshots
          // remain safe because we persist only the lease endpoint, not the resolved URL.
          backendSnapshot = candidateSnapshot;
        }
      } else {
        if (stableUrl) {
          baseDisk = await RemoteChunkedDisk.open(stableUrl, {
            cacheBackend: remoteCacheBackend,
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
      }
    } else {
      if (meta.remote.delivery === "range") {
        const expectedEtag = expectedValidator?.kind === "etag" ? expectedValidator.value : undefined;
        if (stableUrl) {
          baseDisk = await RemoteStreamingDisk.open(stableUrl, {
            blockSize: meta.cache.chunkSizeBytes,
            cacheBackend: remoteCacheBackend,
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
            cacheImageId: meta.remote.imageId,
            cacheVersion: meta.remote.version,
            cacheEtag: expectedEtag,
            expectedSizeBytes: meta.sizeBytes,
          });
        } else {
          throw new Error("remote disk metadata missing urls.url and urls.leaseEndpoint");
        }
        if (stableUrl && shouldEnableSnapshotForUrl(stableUrl, defaultRemoteRangeUrl(base))) {
          backendSnapshot = candidateSnapshot;
        } else if (!stableUrl && leaseEndpoint) {
          backendSnapshot = candidateSnapshot;
        }
      } else if (meta.remote.delivery === "chunked") {
        if (stableUrl) {
          baseDisk = await RemoteChunkedDisk.open(stableUrl, {
            cacheBackend: remoteCacheBackend,
            credentials: "same-origin",
            cacheImageId: meta.remote.imageId,
            cacheVersion: meta.remote.version,
          });
        } else if (leaseEndpoint) {
          const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "chunked" });
          await lease.refresh();
          baseDisk = await RemoteChunkedDisk.openWithLease({ sourceId: meta.remote.imageId, lease }, {
            cacheBackend: remoteCacheBackend,
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
  if (localMeta.backend === "opfs") {
    const fileName = localMeta.fileName;
    const sizeBytes = localMeta.sizeBytes;

    async function openBase(): Promise<AsyncSectorDisk> {
      switch (localMeta.format) {
        case "aerospar": {
          const disk = await OpfsAeroSparseDisk.open(fileName);
          if (disk.capacityBytes !== sizeBytes) {
            await disk.close?.();
            throw new Error(`disk size mismatch: expected=${sizeBytes} actual=${disk.capacityBytes}`);
          }
          return disk;
        }
        case "raw":
        case "iso":
        case "unknown":
          return await OpfsRawDisk.open(fileName, { create: false, sizeBytes });
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
        });

        return {
          disk: new OpfsCowDisk(base, overlay),
          readOnly: false,
          backendSnapshot: {
            kind: "local",
            backend: "opfs",
            key: localMeta.fileName,
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
        // If SyncAccessHandle isn't available, sparse overlays can't work efficiently.
        // Fall back to direct raw writes (still in a worker, but slower).
        if (localMeta.format !== "raw" && localMeta.format !== "iso" && localMeta.format !== "unknown") throw err;
      }
    }

    const disk = await openBase();
    return {
      disk,
      readOnly,
      backendSnapshot: {
        kind: "local",
        backend: "opfs",
        key: localMeta.fileName,
        format: localMeta.format,
        diskKind: localMeta.kind,
        sizeBytes: localMeta.sizeBytes,
      },
    };
  }

  // IndexedDB backend: disk data is stored in the `chunks` store (sparse).
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

      return { disk: base, readOnly: entry.readOnly, backendSnapshot: backend };
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
  if (remoteCacheBackend === "opfs" && kind === "range") {
    await ensureRemoteCacheBinding(backend.base, backend.cache.fileName);
  }

  if (kind !== "range" && kind !== "chunked") {
    throw new Error(`unsupported remote deliveryType=${backend.base.deliveryType}`);
  }

  const leaseEndpoint = typeof backend.base.leaseEndpoint === "string" ? backend.base.leaseEndpoint.trim() : "";

  let base: AsyncSectorDisk;
  if (remoteCacheBackend === "opfs") {
    if (kind === "range") {
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
        base = await RemoteRangeDisk.open(url, { cacheKeyParts, chunkSize: backend.base.chunkSize, metadataStore, sparseCacheFactory });
      }

      if (base.capacityBytes !== backend.sizeBytes) {
        await base.close?.();
        throw new Error(`disk size mismatch: expected=${backend.sizeBytes} actual=${base.capacityBytes}`);
      }
    } else {
      if (leaseEndpoint) {
        const lease = createDiskAccessLeaseFromLeaseEndpoint(leaseEndpoint, { delivery: "chunked" });
        await lease.refresh();
        base = await RemoteChunkedDisk.openWithLease({ sourceId: backend.base.imageId, lease }, {
          cacheBackend: remoteCacheBackend,
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      } else {
        const manifestUrl = defaultRemoteChunkedManifestUrl(backend.base);
        base = await RemoteChunkedDisk.open(manifestUrl, {
          cacheBackend: remoteCacheBackend,
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
          cacheImageId: backend.base.imageId,
          cacheVersion: backend.base.version,
        });
      } else {
        const manifestUrl = defaultRemoteChunkedManifestUrl(backend.base);
        base = await RemoteChunkedDisk.open(manifestUrl, {
          cacheBackend: remoteCacheBackend,
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
  const remote = remoteSpec.remote;
  const readOnlyBase = remote.kind === "cd" || remote.format === "iso";

  const credentials = remote.credentials ?? "same-origin";
  const fetchFn: typeof fetch = (input, init = {}) => fetch(input, { ...init, credentials: init.credentials ?? credentials });

  const cacheImageId = (remote.imageId ?? remote.cacheKey).trim();
  if (!cacheImageId) throw new Error("remote cacheKey must not be empty");
  const rangeCacheVersion = (remote.version ?? "1").trim();
  if (!rangeCacheVersion) throw new Error("remote version must not be empty");

  const base: AsyncSectorDisk =
    remote.delivery === "range"
      ? await (async () => {
          const chunkSize = remote.chunkSizeBytes ?? RANGE_STREAM_CHUNK_SIZE;

          // Backward-compat cleanup: older clients keyed range caches as `deliveryType: "range"`,
          // which collides across different chunkSize values.
          await bestEffortDeleteLegacyRemoteRangeCache(cacheImageId, rangeCacheVersion);

          return await RemoteRangeDisk.open(remote.url, {
            cacheKeyParts: {
              imageId: cacheImageId,
              version: rangeCacheVersion,
              deliveryType: remoteRangeDeliveryType(chunkSize),
            },
            credentials,
            chunkSize,
            sha256Manifest: await loadSha256Manifest(remote.integrity, fetchFn),
            fetchFn,
          });
        })()
      : await RemoteChunkedDisk.open(remote.manifestUrl, {
          credentials,
          cacheImageId,
          cacheVersion: remote.version,
        });

  // Remote base images are always treated as read-only. For HDDs, default to a local
  // COW overlay so the guest can write without mutating the base.
  if (mode === "cow" && !readOnlyBase && remote.kind === "hdd") {
    const key = safeOpfsNameComponent(remote.cacheKey);
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

  handleMessage(msg: RuntimeDiskRequestMessage): Promise<void> {
    if (!msg || msg.type !== "request") return Promise.resolve();
    this.requestChain = this.requestChain.then(async () => {
      try {
        await this.handleRequest(msg);
      } catch (err) {
        this.postErr(msg.requestId, err);
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
    // Backward-compat: older clients sent `{ meta, ... }`.
    if (payload && typeof payload === "object" && payload.meta && !payload.spec) {
      return { ...payload, spec: { kind: "local", meta: payload.meta } };
    }
    return payload as OpenRequestPayload;
  }

  private async handleRequest(msg: RuntimeDiskRequestMessage): Promise<void> {
    switch (msg.op) {
      case "open": {
        const payload = this.normalizeOpenPayload((msg as any).payload);
        const spec = normalizeDiskOpenSpec(payload.spec);
        const entry = await this.openDisk(spec, payload.mode ?? "cow", payload.overlayBlockSizeBytes);
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
        const { url, options } = msg.payload;
        const entry: DiskEntry = { disk: await openRemoteDisk(url, options), readOnly: true, backendSnapshot: null };
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
        const { manifestUrl, options } = msg.payload;
        const entry: DiskEntry = { disk: await RemoteChunkedDisk.open(manifestUrl, options), readOnly: true, backendSnapshot: null };
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
        const { handle } = msg.payload;
        const entry = await this.requireDisk(handle);
        await entry.disk.close?.();
        this.disks.delete(handle);
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "flush": {
        const { handle } = msg.payload;
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
        const { handle } = msg.payload;
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
        const { handle, lba, byteLength } = msg.payload;
        const entry = await this.requireDisk(handle);
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

      case "write": {
        const { handle, lba, data } = msg.payload;
        const entry = await this.requireDisk(handle);
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
        this.postOk(msg.requestId, { ok: true });
        return;
      }

      case "stats": {
        const { handle } = msg.payload;
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
        const snapshot = deserializeRuntimeDiskSnapshot(msg.payload.state);

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
        const { handle, totalBytes, chunkBytes, mode } = msg.payload;
        const entry = await this.requireDisk(handle);

        const selected = mode ?? "rw";
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
    }
  }
}
