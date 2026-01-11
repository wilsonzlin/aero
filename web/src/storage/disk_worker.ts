import {
  buildDiskFileName,
  createMetadataStore,
  inferFormatFromFileName,
  inferKindFromFileName,
  newDiskId,
  idbTxDone,
  openDiskManagerDb,
  opfsGetDisksDir,
  type DiskBackend,
  type DiskFormat,
  type DiskImageMetadata,
  type DiskKind,
  type MountConfig,
  type RemoteDiskDelivery,
  type RemoteDiskValidator,
  type RemoteDiskUrls,
} from "./metadata";
import { importConvertToOpfs } from "./import_convert.ts";
import {
  idbCreateBlankDisk,
  idbDeleteDiskData,
  idbExportToPort,
  idbImportFile,
  idbResizeDisk,
  opfsCreateBlankDisk,
  opfsDeleteDisk,
  opfsExportToPort,
  opfsGetDiskSizeBytes,
  opfsImportFile,
  opfsResizeDisk,
  type ImportProgress,
} from "./import_export";
import { RemoteCacheManager } from "./remote_cache_manager";

type DiskWorkerError = { message: string; name?: string; stack?: string };

function serializeError(err: unknown): DiskWorkerError {
  if (err instanceof Error) {
    return { message: err.message, name: err.name, stack: err.stack };
  }
  return { message: String(err) };
}

function isPowerOfTwo(n: number): boolean {
  if (!Number.isSafeInteger(n) || n <= 0) return false;
  // Use bigint to avoid 32-bit truncation.
  const b = BigInt(n);
  return (b & (b - 1n)) === 0n;
}

function assertValidDiskBackend(backend: unknown): asserts backend is DiskBackend {
  if (backend !== "opfs" && backend !== "idb") {
    throw new Error("cacheBackend must be 'opfs' or 'idb'");
  }
}

function assertValidOpfsFileName(name: string, field: string): void {
  // OPFS file names are path components; reject separators to avoid confusion about directories.
  if (name.includes("/") || name.includes("\\") || name.includes("\0")) {
    throw new Error(`${field} must be a simple file name (no path separators)`);
  }
}

const IDB_REMOTE_CHUNK_MIN_BYTES = 512 * 1024;
const IDB_REMOTE_CHUNK_MAX_BYTES = 8 * 1024 * 1024;

function assertValidIdbRemoteChunkSize(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
  if (value < IDB_REMOTE_CHUNK_MIN_BYTES || value > IDB_REMOTE_CHUNK_MAX_BYTES) {
    throw new Error(`${field} must be within ${IDB_REMOTE_CHUNK_MIN_BYTES}..${IDB_REMOTE_CHUNK_MAX_BYTES} bytes`);
  }
}

/**
 * @param {number} requestId
 * @param {any} payload
 */
function postProgress(requestId: number, payload: ImportProgress): void {
  (self as DedicatedWorkerGlobalScope).postMessage({ type: "progress", requestId, ...payload });
}

/**
 * @param {number} requestId
 * @param {any} result
 */
function postOk(requestId: number, result: unknown): void {
  (self as DedicatedWorkerGlobalScope).postMessage({ type: "response", requestId, ok: true, result });
}

/**
 * @param {number} requestId
 * @param {any} error
 */
function postErr(requestId: number, error: unknown): void {
  (self as DedicatedWorkerGlobalScope).postMessage({
    type: "response",
    requestId,
    ok: false,
    error: serializeError(error),
  });
}

function assertNonSecretUrl(url: string | undefined): void {
  if (!url) return;
  let parsed: URL;
  try {
    parsed = new URL(url, "https://example.invalid");
  } catch {
    // If URL parsing fails, fall back to best-effort substring checks.
    const lower = url.toLowerCase();
    if (lower.includes("x-amz-signature") || lower.includes("key-pair-id=") || lower.includes("signature=")) {
      throw new Error("Refusing to persist what looks like a signed URL; store a stable URL or a leaseEndpoint instead.");
    }
    return;
  }

  if (parsed.username || parsed.password) {
    throw new Error("Refusing to persist a URL with embedded credentials; store a stable URL or a leaseEndpoint instead.");
  }

  const banned = new Set([
    // AWS S3 presigned query params.
    "x-amz-algorithm",
    "x-amz-credential",
    "x-amz-date",
    "x-amz-expires",
    "x-amz-security-token",
    "x-amz-signature",
    "x-amz-signedheaders",
    // CloudFront signed URL params (and other common CDNs).
    "expires",
    "key-pair-id",
    "policy",
    "signature",
  ]);

  for (const [key] of parsed.searchParams) {
    if (banned.has(key.toLowerCase())) {
      throw new Error("Refusing to persist what looks like a signed URL; store a stable URL or a leaseEndpoint instead.");
    }
  }
}

function assertValidLeaseEndpoint(endpoint: string | undefined): void {
  if (!endpoint) return;
  if (!endpoint.startsWith("/")) {
    throw new Error("leaseEndpoint must be a same-origin path starting with '/'");
  }
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i]!.toString(16).padStart(2, "0");
  }
  return out;
}

async function stableCacheId(key: string): Promise<string> {
  try {
    const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
    if (!subtle) throw new Error("missing crypto.subtle");
    const data = new TextEncoder().encode(key);
    const digest = await subtle.digest("SHA-256", data);
    return bytesToHex(new Uint8Array(digest));
  } catch {
    return encodeURIComponent(key).replaceAll("%", "_").slice(0, 128);
  }
}

async function idbDeleteRemoteChunkCache(db: IDBDatabase, cacheKey: string): Promise<void> {
  const tx = db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
  const chunksStore = tx.objectStore("remote_chunks");
  const metaStore = tx.objectStore("remote_chunk_meta");
  metaStore.delete(cacheKey);

  const range = IDBKeyRange.bound([cacheKey, -Infinity], [cacheKey, Infinity]);
  await new Promise<void>((resolve, reject) => {
    const req = chunksStore.openCursor(range);
    req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return resolve(undefined);
      cursor.delete();
      cursor.continue();
    };
  });

  await idbTxDone(tx);
}

/**
 * @param {DiskBackend} backend
 */
function getStore(backend: DiskBackend) {
  return createMetadataStore(backend);
}

/**
 * @param {DiskBackend} backend
 * @param {string} id
 * @returns {Promise<DiskImageMetadata>}
 */
async function requireDisk(backend: DiskBackend, id: string): Promise<DiskImageMetadata> {
  const meta = await getStore(backend).getDisk(id);
  if (!meta) throw new Error(`Disk not found: ${id}`);
  return meta;
}

/**
 * @param {DiskBackend} backend
 * @param {DiskImageMetadata} meta
 */
async function putDisk(backend: DiskBackend, meta: DiskImageMetadata): Promise<void> {
  await getStore(backend).putDisk(meta);
}

/**
 * @param {DiskBackend} backend
 * @param {{ hddId?: string; cdId?: string }} mounts
 */
async function validateMounts(backend: DiskBackend, mounts: MountConfig): Promise<void> {
  if (mounts.hddId) {
    const hdd = await requireDisk(backend, mounts.hddId);
    if (hdd.kind !== "hdd") throw new Error("hddId must refer to a HDD image");
  }
  if (mounts.cdId) {
    const cd = await requireDisk(backend, mounts.cdId);
    if (cd.kind !== "cd") throw new Error("cdId must refer to a CD image");
  }
}

type DiskWorkerRequest = {
  type: "request";
  requestId: number;
  backend: DiskBackend;
  op: string;
  payload?: any;
  port?: MessagePort;
};

(self as DedicatedWorkerGlobalScope).onmessage = (event: MessageEvent<DiskWorkerRequest>) => {
  const msg = event.data;
  if (!msg || msg.type !== "request") return;
  const { requestId } = msg;
  handleRequest(msg).catch((err) => postErr(requestId, err));
};

async function handleRequest(msg: DiskWorkerRequest): Promise<void> {
  const requestId = msg.requestId;
  const backend = msg.backend;
  const op = msg.op;
  const store = getStore(backend);

  switch (op) {
    case "list_disks": {
      const disks = await store.listDisks();
      postOk(requestId, disks);
      return;
    }

    case "get_mounts": {
      const mounts = await store.getMounts();
      postOk(requestId, mounts);
      return;
    }

    case "set_mounts": {
      const mounts = (msg.payload || {}) as MountConfig;
      await validateMounts(backend, mounts);

      const now = Date.now();
      if (mounts.hddId) {
        const meta = await requireDisk(backend, mounts.hddId);
        meta.lastUsedAtMs = now;
        await putDisk(backend, meta);
      }
      if (mounts.cdId) {
        const meta = await requireDisk(backend, mounts.cdId);
        meta.lastUsedAtMs = now;
        await putDisk(backend, meta);
      }

      await store.setMounts(mounts);
      postOk(requestId, mounts);
      return;
    }

    case "create_blank": {
      const { name, sizeBytes } = msg.payload;
      const kind = (msg.payload.kind || "hdd") as DiskKind;
      const format = (msg.payload.format || "raw") as DiskFormat;
      if (kind !== "hdd") throw new Error("Only HDD images can be created as blank disks");

      const id = newDiskId();
      const fileName = buildDiskFileName(id, format);

      const progressCb = (p: ImportProgress) => postProgress(requestId, p);

      let checksumCrc32;
      if (backend === "opfs") {
        const res = await opfsCreateBlankDisk(fileName, sizeBytes, progressCb);
        checksumCrc32 = res.checksumCrc32;
      } else {
        await idbCreateBlankDisk(id, sizeBytes);
        checksumCrc32 = undefined;
      }

      const meta = {
        source: "local",
        id,
        name,
        backend,
        kind,
        format,
        fileName,
        sizeBytes,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: checksumCrc32 ? { algorithm: "crc32", value: checksumCrc32 } : undefined,
      } satisfies DiskImageMetadata;

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "import_file": {
      const file = msg.payload.file as File | undefined;
      if (!file) throw new Error("Missing file");

      const fileNameOverride = msg.payload.name;
      const name = (fileNameOverride && String(fileNameOverride)) || file.name;

      const kind = (msg.payload.kind || inferKindFromFileName(file.name)) as DiskKind;
      const format = (msg.payload.format || inferFormatFromFileName(file.name)) as DiskFormat;

      const id = newDiskId();
      const fileName = buildDiskFileName(id, format);

      const progressCb = (p: ImportProgress) => postProgress(requestId, p);

      let sizeBytes;
      let checksumCrc32: string | undefined;

      if (backend === "opfs") {
        const res = await opfsImportFile(fileName, file, progressCb);
        sizeBytes = res.sizeBytes;
        checksumCrc32 = res.checksumCrc32;
      } else {
        const res = await idbImportFile(id, file, progressCb);
        sizeBytes = res.sizeBytes;
        checksumCrc32 = res.checksumCrc32;
      }

      const meta = {
        source: "local",
        id,
        name,
        backend,
        kind,
        format,
        fileName,
        sizeBytes,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: checksumCrc32 ? { algorithm: "crc32", value: checksumCrc32 } : undefined,
        sourceFileName: file.name,
      } satisfies DiskImageMetadata;

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "import_convert": {
      if (backend !== "opfs") {
        throw new Error("import_convert is only supported for the OPFS backend");
      }

      const file = msg.payload.file as File | undefined;
      if (!file) throw new Error("Missing file");

      const fileNameOverride = msg.payload.name;
      const name = (fileNameOverride && String(fileNameOverride)) || file.name;

      const id = newDiskId();
      const baseName = id;

      const destDir = await opfsGetDisksDir();

      const manifest = await importConvertToOpfs({ kind: "file", file }, destDir, baseName, {
        blockSizeBytes: typeof msg.payload.blockSizeBytes === "number" ? msg.payload.blockSizeBytes : undefined,
        onProgress(p) {
          postProgress(requestId, { phase: "import", processedBytes: p.processedBytes, totalBytes: p.totalBytes });
        },
      });

      let kind: DiskKind;
      let format: DiskFormat;
      let fileName: string;

      if (manifest.convertedFormat === "iso") {
        kind = "cd";
        format = "iso";
        fileName = `${id}.iso`;
      } else {
        kind = "hdd";
        format = "aerospar";
        fileName = `${id}.aerospar`;
      }

      const meta: DiskImageMetadata = {
        source: "local",
        id,
        name,
        backend,
        kind,
        format,
        fileName,
        sizeBytes: manifest.logicalSize,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: manifest.checksum,
        sourceFileName: file.name,
      };

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "create_remote": {
      const payload = msg.payload || {};
      const name = String(payload.name || "");
      const imageId = String(payload.imageId || "");
      const version = String(payload.version || "");
      const delivery = payload.delivery as RemoteDiskDelivery;
      const sizeBytes = payload.sizeBytes;
      const kind = (payload.kind || "hdd") as DiskKind;
      const format = (payload.format || "raw") as DiskFormat;

      if (!name.trim()) throw new Error("Remote disk name is required");
      if (!imageId) throw new Error("imageId is required");
      if (!version) throw new Error("version is required");
      if (delivery !== "range" && delivery !== "chunked") {
        throw new Error("delivery must be 'range' or 'chunked'");
      }
      if (kind !== "hdd" && kind !== "cd") throw new Error("kind must be 'hdd' or 'cd'");
      if (typeof sizeBytes !== "number" || !Number.isFinite(sizeBytes) || sizeBytes <= 0) {
        throw new Error("sizeBytes must be a positive number");
      }
      if (sizeBytes % 512 !== 0) {
        throw new Error("sizeBytes must be a multiple of 512");
      }

      const id = newDiskId();
      const cacheBackendRaw = payload.cacheBackend ?? backend;
      assertValidDiskBackend(cacheBackendRaw);
      const cacheBackend = cacheBackendRaw;
      const defaultChunkSizeBytes = delivery === "chunked" ? 4 * 1024 * 1024 : 1024 * 1024;
      const chunkSizeBytes =
        typeof payload.chunkSizeBytes === "number" && Number.isFinite(payload.chunkSizeBytes) && payload.chunkSizeBytes > 0
          ? payload.chunkSizeBytes
          : defaultChunkSizeBytes;
      if (chunkSizeBytes % 512 !== 0 || !isPowerOfTwo(chunkSizeBytes)) {
        throw new Error("chunkSizeBytes must be a power of two and a multiple of 512");
      }

      const overlayBlockSizeBytes =
        typeof payload.overlayBlockSizeBytes === "number" && Number.isFinite(payload.overlayBlockSizeBytes) && payload.overlayBlockSizeBytes > 0
          ? payload.overlayBlockSizeBytes
          : 1024 * 1024;
      if (overlayBlockSizeBytes % 512 !== 0 || !isPowerOfTwo(overlayBlockSizeBytes)) {
        throw new Error("overlayBlockSizeBytes must be a power of two and a multiple of 512");
      }
      if (cacheBackend === "idb") {
        assertValidIdbRemoteChunkSize(chunkSizeBytes, "chunkSizeBytes");
        assertValidIdbRemoteChunkSize(overlayBlockSizeBytes, "overlayBlockSizeBytes");
      }

      const urls: RemoteDiskUrls = {
        ...((payload.urls || {}) as RemoteDiskUrls),
        ...(payload.url ? { url: String(payload.url) } : {}),
        ...(payload.leaseEndpoint ? { leaseEndpoint: String(payload.leaseEndpoint) } : {}),
      };
      if (!urls.url && !urls.leaseEndpoint) {
        throw new Error("Remote disks must provide either urls.url (stable) or urls.leaseEndpoint (same-origin)");
      }
      assertValidLeaseEndpoint(urls.leaseEndpoint);
      assertNonSecretUrl(urls.url);
      assertNonSecretUrl(urls.leaseEndpoint);
      const validator = payload.validator as RemoteDiskValidator | undefined;

      const cacheFileName = typeof payload.cacheFileName === "string" && payload.cacheFileName ? payload.cacheFileName : `${id}.cache.aerospar`;
      const overlayFileName = typeof payload.overlayFileName === "string" && payload.overlayFileName ? payload.overlayFileName : `${id}.overlay.aerospar`;
      if (cacheBackend === "opfs") {
        assertValidOpfsFileName(cacheFileName, "cacheFileName");
        assertValidOpfsFileName(overlayFileName, "overlayFileName");
      }

      const meta: DiskImageMetadata = {
        source: "remote",
        id,
        name,
        kind,
        format,
        sizeBytes,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        remote: {
          imageId,
          version,
          delivery,
          urls,
          validator,
        },
        cache: {
          chunkSizeBytes,
          backend: cacheBackend,
          fileName: cacheFileName,
          overlayFileName,
          overlayBlockSizeBytes,
        },
      };

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "update_remote": {
      const payload = msg.payload || {};
      const id = String(payload.id || "");
      if (!id) throw new Error("Missing remote disk id");

      const meta = await requireDisk(backend, id);
      if (meta.source !== "remote") {
        throw new Error("update_remote can only be used with remote disks");
      }

      if (payload.name !== undefined) meta.name = String(payload.name);
      if (payload.kind !== undefined) meta.kind = payload.kind as DiskKind;
      if (payload.format !== undefined) meta.format = payload.format as DiskFormat;
      if (payload.sizeBytes !== undefined) {
        const next = Number(payload.sizeBytes);
        if (!Number.isFinite(next) || next <= 0) {
          throw new Error("sizeBytes must be a positive number");
        }
        if (next % 512 !== 0) {
          throw new Error("sizeBytes must be a multiple of 512");
        }
        meta.sizeBytes = next;
      }

      if (payload.imageId !== undefined) meta.remote.imageId = String(payload.imageId);
      if (payload.version !== undefined) meta.remote.version = String(payload.version);
      if (payload.delivery !== undefined) meta.remote.delivery = payload.delivery as RemoteDiskDelivery;
      if (payload.urls !== undefined || payload.url !== undefined || payload.leaseEndpoint !== undefined) {
        const nextUrls: RemoteDiskUrls = {
          ...meta.remote.urls,
          ...(payload.urls ? (payload.urls as RemoteDiskUrls) : {}),
          ...(payload.url ? { url: String(payload.url) } : {}),
          ...(payload.leaseEndpoint ? { leaseEndpoint: String(payload.leaseEndpoint) } : {}),
        };
        if (!nextUrls.url && !nextUrls.leaseEndpoint) {
          throw new Error("Remote disks must provide either urls.url (stable) or urls.leaseEndpoint (same-origin)");
        }
        assertValidLeaseEndpoint(nextUrls.leaseEndpoint);
        assertNonSecretUrl(nextUrls.url);
        assertNonSecretUrl(nextUrls.leaseEndpoint);
        meta.remote.urls = nextUrls;
      }
      if (payload.validator !== undefined) meta.remote.validator = payload.validator as RemoteDiskValidator;

      if (payload.cacheBackend !== undefined) {
        assertValidDiskBackend(payload.cacheBackend);
        meta.cache.backend = payload.cacheBackend;
      }
      if (payload.chunkSizeBytes !== undefined) {
        const next = Number(payload.chunkSizeBytes);
        if (next % 512 !== 0 || !isPowerOfTwo(next)) {
          throw new Error("chunkSizeBytes must be a power of two and a multiple of 512");
        }
        meta.cache.chunkSizeBytes = next;
      }
      if (payload.cacheFileName !== undefined) meta.cache.fileName = String(payload.cacheFileName);
      if (payload.overlayFileName !== undefined) meta.cache.overlayFileName = String(payload.overlayFileName);
      if (payload.overlayBlockSizeBytes !== undefined) {
        const next = Number(payload.overlayBlockSizeBytes);
        if (next % 512 !== 0 || !isPowerOfTwo(next)) {
          throw new Error("overlayBlockSizeBytes must be a power of two and a multiple of 512");
        }
        meta.cache.overlayBlockSizeBytes = next;
      }
      if (meta.cache.backend === "opfs") {
        assertValidOpfsFileName(meta.cache.fileName, "cacheFileName");
        assertValidOpfsFileName(meta.cache.overlayFileName, "overlayFileName");
      }
      if (meta.cache.backend === "idb") {
        assertValidIdbRemoteChunkSize(meta.cache.chunkSizeBytes, "chunkSizeBytes");
        assertValidIdbRemoteChunkSize(meta.cache.overlayBlockSizeBytes, "overlayBlockSizeBytes");
      }

      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "stat_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      let actualSize = meta.sizeBytes;
      if (meta.source === "local") {
        if (meta.backend === "opfs") {
          actualSize = await opfsGetDiskSizeBytes(meta.fileName);
        }
      } else {
        if (meta.cache.backend === "opfs") {
          try {
            actualSize = await opfsGetDiskSizeBytes(meta.cache.fileName);
          } catch {
            actualSize = meta.sizeBytes;
          }
        }
      }
      postOk(requestId, { meta, actualSizeBytes: actualSize });
      return;
    }

    case "resize_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      if (meta.source !== "local") {
        throw new Error("Remote disks cannot be resized");
      }
      const newSizeBytes = msg.payload.newSizeBytes;
      if (typeof newSizeBytes !== "number" || newSizeBytes < 0) throw new Error("Invalid newSizeBytes");
      if (meta.kind !== "hdd") {
        throw new Error("Only HDD images can be resized");
      }

      const progressCb = (p: ImportProgress) => postProgress(requestId, p);

      if (meta.backend === "opfs") {
        await opfsResizeDisk(meta.fileName, newSizeBytes, progressCb);
        // Resizing invalidates COW overlays (table size depends on disk size).
        await opfsDeleteDisk(`${meta.id}.overlay.aerospar`);
      } else {
        await idbResizeDisk(meta.id, meta.sizeBytes, newSizeBytes, progressCb);
      }

      meta.sizeBytes = newSizeBytes;
      // Resizing invalidates checksums.
      meta.checksum = undefined;
      await store.putDisk(meta);
      postOk(requestId, meta);
      return;
    }

    case "delete_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      if (meta.source === "local") {
        if (meta.backend === "opfs") {
          await opfsDeleteDisk(meta.fileName);
          // Best-effort cleanup of runtime COW overlay files.
          await opfsDeleteDisk(`${meta.id}.overlay.aerospar`);
        } else {
          const db = await openDiskManagerDb();
          try {
            await idbDeleteDiskData(db, meta.id);
          } finally {
            db.close();
          }
        }
      } else {
        if (meta.cache.backend === "opfs") {
          // Remote delivery caches bytes under the RemoteCacheManager directory (derived key).
          // Best-effort cleanup when deleting the disk.
          try {
            const cacheKey = await RemoteCacheManager.deriveCacheKey({
              imageId: meta.remote.imageId,
              version: meta.remote.version,
              deliveryType: meta.remote.delivery,
            });
            const manager = await RemoteCacheManager.openOpfs();
            await manager.clearCache(cacheKey);
          } catch {
            // best-effort cleanup
          }

          await opfsDeleteDisk(meta.cache.fileName);
          // Legacy versions used a small binding file to associate the OPFS Range cache file with the
          // immutable remote base identity. Best-effort cleanup when the disk is deleted.
          await opfsDeleteDisk(`${meta.cache.fileName}.binding.json`);
          // Legacy RemoteRangeDisk persisted its own small metadata file keyed by the remote base identity.
          // Best-effort cleanup when deleting the disk.
          if (meta.remote.delivery === "range") {
            const imageKey = `${meta.remote.imageId}:${meta.remote.version}:${meta.remote.delivery}`;
            const cacheId = await stableCacheId(imageKey);
            await opfsDeleteDisk(`remote-range-cache-${cacheId}.json`);
          }
          await opfsDeleteDisk(meta.cache.overlayFileName);
        } else {
          const db = await openDiskManagerDb();
          try {
            // Remote disk caches may be stored in the dedicated `remote_chunks` store (LRU cache)
            // and/or in the legacy `chunks` store (disk-style sparse chunks).
            // Best-effort cleanup: try both.
            const derivedCacheKey = await RemoteCacheManager.deriveCacheKey({
              imageId: meta.remote.imageId,
              version: meta.remote.version,
              deliveryType: meta.remote.delivery,
            });
            await idbDeleteRemoteChunkCache(db, derivedCacheKey);
            await idbDeleteRemoteChunkCache(db, meta.cache.fileName);
            await idbDeleteRemoteChunkCache(db, meta.cache.overlayFileName);
            await idbDeleteDiskData(db, meta.cache.fileName);
            await idbDeleteDiskData(db, meta.cache.overlayFileName);
          } finally {
            db.close();
          }
        }
      }
      await store.deleteDisk(meta.id);
      postOk(requestId, { ok: true });
      return;
    }

    case "export_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      if (meta.source !== "local") {
        throw new Error("Remote disks cannot be exported");
      }
      const port = msg.port;
      if (!port) throw new Error("Missing MessagePort for export");

      const options = msg.payload.options || {};
      const progressCb = (p: ImportProgress) => postProgress(requestId, p);

      // Respond immediately so the main thread can start consuming the stream.
      postOk(requestId, { started: true, meta });

      const now = Date.now();
      meta.lastUsedAtMs = now;
      await store.putDisk(meta);

      void (async () => {
        try {
          if (meta.backend === "opfs") {
            await opfsExportToPort(meta.fileName, port, options, progressCb);
          } else {
            await idbExportToPort(meta.id, meta.sizeBytes, port, options, progressCb);
          }
        } catch (err) {
          try {
            port.postMessage({ type: "error", error: serializeError(err) });
          } finally {
            port.close();
          }
        }
      })();

      return;
    }

    default:
      throw new Error(`Unknown op: ${op}`);
  }
}
