import {
  buildDiskFileName,
  createMetadataStore,
  inferFormatFromFileName,
  inferKindFromFileName,
  newDiskId,
  idbReq,
  idbTxDone,
  OPFS_LEGACY_IMAGES_DIR,
  OPFS_DISKS_PATH,
  OPFS_REMOTE_CACHE_DIR,
  openDiskManagerDb,
  opfsGetDir,
  opfsGetDisksDir,
  opfsGetRemoteCacheDir,
  DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES,
  type DiskBackend,
  type DiskFormat,
  type DiskImageMetadata,
  type DiskKind,
  type MountConfig,
  type RemoteDiskDelivery,
  type RemoteDiskValidator,
  type RemoteDiskUrls,
} from "./metadata";
import { planLegacyOpfsImageAdoptions, type LegacyOpfsFile } from "./legacy_images";
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
import { probeRemoteDisk, stableCacheKey } from "../platform/remote_disk";
import { removeOpfsEntry } from "../platform/opfs";
import { CHUNKED_DISK_CHUNK_SIZE, RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes.ts";
import { RemoteCacheManager, remoteChunkedDeliveryType, remoteRangeDeliveryType, type RemoteCacheStatus } from "./remote_cache_manager";
import { assertNonSecretUrl, assertValidLeaseEndpoint } from "./url_safety";

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
const OPFS_REMOTE_CHUNK_MAX_BYTES = 64 * 1024 * 1024;

// Keep in sync with `platform/remote_disk.ts` (RemoteStreamingDisk).
const MAX_REMOTE_BLOCK_SIZE_BYTES = 64 * 1024 * 1024; // 64 MiB
const MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS = 1024;
const MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES = 512 * 1024 * 1024; // 512 MiB
const MAX_REMOTE_CACHES_LIST = 10_000;

function assertValidIdbRemoteChunkSize(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
  if (value < IDB_REMOTE_CHUNK_MIN_BYTES || value > IDB_REMOTE_CHUNK_MAX_BYTES) {
    throw new Error(`${field} must be within ${IDB_REMOTE_CHUNK_MIN_BYTES}..${IDB_REMOTE_CHUNK_MAX_BYTES} bytes`);
  }
}

function assertValidOpfsRemoteChunkSize(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
  if (value % 512 !== 0 || !isPowerOfTwo(value)) {
    throw new Error(`${field} must be a power of two and a multiple of 512`);
  }
  if (value > OPFS_REMOTE_CHUNK_MAX_BYTES) {
    throw new Error(`${field} must be <= ${OPFS_REMOTE_CHUNK_MAX_BYTES} bytes`);
  }
}

function assertValidCacheLimitBytes(value: unknown, field: string): asserts value is number | null {
  if (value === null) return;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${field} must be a non-negative safe integer or null`);
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

function idbOverlayBindingKey(overlayDiskId: string): string {
  return `overlay-binding:${overlayDiskId}`;
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

async function idbSumDiskChunkBytes(db: IDBDatabase, diskId: string): Promise<number> {
  const tx = db.transaction(["chunks"], "readonly");
  const store = tx.objectStore("chunks").index("by_id");
  const range = IDBKeyRange.only(diskId);

  let total = 0;
  await new Promise<void>((resolve, reject) => {
    const req = store.openCursor(range);
    req.onerror = () => reject(req.error || new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return resolve(undefined);
      const value = cursor.value as { data?: unknown } | undefined;
      const data = value?.data;
      if (data && typeof (data as ArrayBufferLike).byteLength === "number") {
        total += (data as ArrayBufferLike).byteLength;
      }
      cursor.continue();
    };
  });

  await idbTxDone(tx);
  return total;
}

async function opfsReadLruChunkCacheBytes(
  remoteCacheDir: FileSystemDirectoryHandle,
  cacheKey: string,
  opts: { scanChunksFallback?: boolean } = {},
): Promise<number> {
  // Keep in sync with `OpfsLruChunkCache`'s index bounds.
  const MAX_LRU_INDEX_JSON_BYTES = 64 * 1024 * 1024; // 64 MiB
  const MAX_LRU_INDEX_CHUNK_ENTRIES = 1_000_000;
  const scanChunksFallback = opts.scanChunksFallback ?? true;

  try {
    const cacheDir = await remoteCacheDir.getDirectoryHandle(cacheKey, { create: false });

    // Prefer parsing the `OpfsLruChunkCache` index to avoid walking every file.
    try {
      const indexHandle = await cacheDir.getFileHandle("index.json", { create: false });
      const file = await indexHandle.getFile();
      if (!Number.isFinite(file.size) || file.size < 0 || file.size > MAX_LRU_INDEX_JSON_BYTES) {
        // Treat absurdly large indices as corrupt and fall back to scanning.
        throw new Error("index.json too large");
      }
      const raw = await file.text();
      if (raw.trim()) {
        try {
          const parsed = JSON.parse(raw) as unknown;
          if (parsed && typeof parsed === "object") {
            const chunks = (parsed as { chunks?: unknown }).chunks;
            if (chunks && typeof chunks === "object") {
              let total = 0;
              const obj = chunks as Record<string, unknown>;
              let entries = 0;
              for (const key in obj) {
                const meta = obj[key];
                if (!meta || typeof meta !== "object") continue;
                entries += 1;
                if (entries > MAX_LRU_INDEX_CHUNK_ENTRIES) {
                  // Treat pathological indices as corrupt and fall back to scanning.
                  throw new Error("index.json chunk entries too large");
                }
                const byteLength = (meta as { byteLength?: unknown }).byteLength;
                if (typeof byteLength === "number" && Number.isFinite(byteLength) && byteLength > 0) total += byteLength;
              }
              return total;
            }
          }
        } catch {
          // ignore and fall back to scanning
        }
      }
    } catch {
      // ignore and fall back to scanning
    }

    if (scanChunksFallback) {
      // Fall back to scanning the chunk files if the index is missing/corrupt.
      try {
        const chunksDir = await cacheDir.getDirectoryHandle("chunks", { create: false });
        let total = 0;
        for await (const [name, handle] of chunksDir.entries()) {
          if (handle.kind !== "file") continue;
          if (!name.endsWith(".bin")) continue;
          const file = await (handle as FileSystemFileHandle).getFile();
          total += file.size;
        }
        return total;
      } catch {
        // ignore
      }
    }
  } catch {
    // cache directory missing or OPFS unavailable
  }
  return 0;
}

async function opfsReadLruChunkCacheIndexStats(
  remoteCacheDir: FileSystemDirectoryHandle,
  cacheKey: string,
): Promise<{ totalBytes: number; chunkCount: number; lastModifiedMs?: number } | null> {
  // Keep in sync with `OpfsLruChunkCache`'s index bounds.
  const MAX_LRU_INDEX_JSON_BYTES = 64 * 1024 * 1024; // 64 MiB
  const MAX_LRU_INDEX_CHUNK_ENTRIES = 1_000_000;

  try {
    const cacheDir = await remoteCacheDir.getDirectoryHandle(cacheKey, { create: false });
    const indexHandle = await cacheDir.getFileHandle("index.json", { create: false });
    const file = await indexHandle.getFile();
    if (!Number.isFinite(file.size) || file.size < 0 || file.size > MAX_LRU_INDEX_JSON_BYTES) return null;
    const lastModifiedMs =
      typeof (file as unknown as { lastModified?: unknown }).lastModified === "number" &&
      Number.isFinite((file as unknown as { lastModified: number }).lastModified) &&
      (file as unknown as { lastModified: number }).lastModified >= 0
        ? (file as unknown as { lastModified: number }).lastModified
        : undefined;
    const raw = await file.text();
    if (!raw.trim()) return null;

    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return null;
    const chunks = (parsed as { chunks?: unknown }).chunks;
    if (!chunks || typeof chunks !== "object") return null;

    let totalBytes = 0;
    let chunkCount = 0;
    const obj = chunks as Record<string, unknown>;
    for (const key in obj) {
      if (!Object.prototype.hasOwnProperty.call(obj, key)) continue;
      const meta = obj[key];
      if (!meta || typeof meta !== "object") continue;
      chunkCount += 1;
      if (chunkCount > MAX_LRU_INDEX_CHUNK_ENTRIES) return null;
      const byteLength = (meta as { byteLength?: unknown }).byteLength;
      if (typeof byteLength === "number" && Number.isFinite(byteLength) && byteLength > 0) totalBytes += byteLength;
    }

    return { totalBytes, chunkCount, lastModifiedMs };
  } catch {
    return null;
  }
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
    case "adopt_legacy_images": {
      if (backend !== "opfs") {
        postOk(requestId, { ok: true, adopted: 0, found: 0 });
        return;
      }

      let legacyFiles: LegacyOpfsFile[] = [];
      try {
        const imagesDir = await opfsGetDir(OPFS_LEGACY_IMAGES_DIR, { create: false });
        for await (const [name, handle] of imagesDir.entries()) {
          if (handle.kind !== "file") continue;
          const file = await (handle as FileSystemFileHandle).getFile();
          legacyFiles.push({ name, sizeBytes: file.size, lastModifiedMs: file.lastModified });
        }
      } catch (err) {
        // If the legacy directory is missing, treat as no-op.
        if (!(err instanceof DOMException && err.name === "NotFoundError")) throw err;
      }

      const existing = await store.listDisks();
      const now = Date.now();
      const newMetas = planLegacyOpfsImageAdoptions({
        existingDisks: existing,
        legacyFiles,
        nowMs: now,
        newId: newDiskId,
      });

      for (const meta of newMetas) {
        await store.putDisk(meta);
      }

      postOk(requestId, { ok: true, adopted: newMetas.length, found: legacyFiles.length });
      return;
    }

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

    case "add_remote": {
      if (backend !== "opfs") {
        throw new Error("Remote disks are only supported when using the OPFS backend.");
      }

      const rawBlockSizeBytes: unknown = msg.payload?.blockSizeBytes;
      let blockSizeBytes: number | undefined;
      if (rawBlockSizeBytes !== undefined) {
        if (typeof rawBlockSizeBytes !== "number" || !Number.isSafeInteger(rawBlockSizeBytes) || rawBlockSizeBytes <= 0) {
          throw new Error("blockSizeBytes must be a positive safe integer");
        }
        if (rawBlockSizeBytes % 512 !== 0) {
          throw new Error("blockSizeBytes must be a multiple of 512");
        }
        if (rawBlockSizeBytes > MAX_REMOTE_BLOCK_SIZE_BYTES) {
          throw new Error(`blockSizeBytes must be <= ${MAX_REMOTE_BLOCK_SIZE_BYTES} bytes (64 MiB)`);
        }
        blockSizeBytes = rawBlockSizeBytes;
      }

      const rawPrefetchSequentialBlocks: unknown = msg.payload?.prefetchSequentialBlocks;
      let prefetchSequentialBlocks: number | undefined;
      if (rawPrefetchSequentialBlocks !== undefined) {
        if (
          typeof rawPrefetchSequentialBlocks !== "number" ||
          !Number.isSafeInteger(rawPrefetchSequentialBlocks) ||
          rawPrefetchSequentialBlocks < 0
        ) {
          throw new Error("prefetchSequentialBlocks must be a non-negative safe integer");
        }
        if (rawPrefetchSequentialBlocks > MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS) {
          throw new Error(`prefetchSequentialBlocks must be <= ${MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS}`);
        }
        const effectiveBlockSize = blockSizeBytes ?? RANGE_STREAM_CHUNK_SIZE;
        const totalPrefetchBytes = BigInt(rawPrefetchSequentialBlocks) * BigInt(effectiveBlockSize);
        if (totalPrefetchBytes > BigInt(MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES)) {
          throw new Error(
            `prefetchSequentialBlocks * blockSizeBytes must be <= ${MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES} bytes (512 MiB)`,
          );
        }
        prefetchSequentialBlocks = rawPrefetchSequentialBlocks;
      }

      const rawCacheLimitBytes: unknown = msg.payload?.cacheLimitBytes;
      let cacheLimitBytes: number | null | undefined;
      if (rawCacheLimitBytes !== undefined) {
        if (rawCacheLimitBytes === null) {
          cacheLimitBytes = null;
        } else {
          if (typeof rawCacheLimitBytes !== "number" || !Number.isSafeInteger(rawCacheLimitBytes) || rawCacheLimitBytes < 0) {
            throw new Error("cacheLimitBytes must be null or a non-negative safe integer");
          }
          cacheLimitBytes = rawCacheLimitBytes;
        }
      }

      const url = String(msg.payload?.url ?? "").trim();
      if (!url) throw new Error("Missing url");

      // Validate URL early to provide a clearer error than `fetch` might.
      let parsed: URL;
      try {
        parsed = new URL(url);
      } catch {
        throw new Error("Invalid URL");
      }
      if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
        throw new Error("Remote disks require an http(s) URL.");
      }

      try {
        assertNonSecretUrl(url);
      } catch {
        throw new Error(
          "Refusing to persist a signed/secret URL in remote disk metadata; provide a stable URL or use the remote-disk flow with leaseEndpoint.",
        );
      }

      const probe = await probeRemoteDisk(url);
      if (probe.size % 512 !== 0) {
        throw new Error(`Remote disk size is not sector-aligned (size=${probe.size}, sector=512).`);
      }

      const filename = msg.payload?.name ? String(msg.payload.name) : parsed.pathname.split("/").filter(Boolean).pop() || "remote.img";
      const format = inferFormatFromFileName(filename);
      if (format === "qcow2" || format === "vhd" || format === "aerospar") {
        throw new Error(`Remote format ${format} is not supported for streaming mounts (use a raw .img or .iso).`);
      }
      const kind = inferKindFromFileName(filename);

      const id = newDiskId();
      const fileName = buildDiskFileName(id, format === "iso" ? "iso" : "raw");

      const meta: DiskImageMetadata = {
        source: "local",
        id,
        name: filename,
        backend,
        kind,
        format: format === "iso" ? "iso" : "raw",
        fileName,
        sizeBytes: probe.size,
        createdAtMs: Date.now(),
        lastUsedAtMs: undefined,
        checksum: undefined,
        remote: {
          url,
          blockSizeBytes,
          cacheLimitBytes,
          prefetchSequentialBlocks,
        },
      };

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
      const defaultChunkSizeBytes = delivery === "chunked" ? CHUNKED_DISK_CHUNK_SIZE : RANGE_STREAM_CHUNK_SIZE;
      const chunkSizeBytes =
        typeof payload.chunkSizeBytes === "number" && Number.isFinite(payload.chunkSizeBytes) && payload.chunkSizeBytes > 0
          ? payload.chunkSizeBytes
          : defaultChunkSizeBytes;

      const overlayBlockSizeBytes =
        typeof payload.overlayBlockSizeBytes === "number" && Number.isFinite(payload.overlayBlockSizeBytes) && payload.overlayBlockSizeBytes > 0
          ? payload.overlayBlockSizeBytes
          : RANGE_STREAM_CHUNK_SIZE;

      const cacheLimitBytesRaw = payload.cacheLimitBytes as unknown;
      const cacheLimitBytes =
        cacheLimitBytesRaw === undefined ? DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES : (cacheLimitBytesRaw as number | null);
      assertValidCacheLimitBytes(cacheLimitBytes, "cacheLimitBytes");
      if (cacheBackend === "idb") {
        if (chunkSizeBytes % 512 !== 0 || !isPowerOfTwo(chunkSizeBytes)) {
          throw new Error("chunkSizeBytes must be a power of two and a multiple of 512");
        }
        if (overlayBlockSizeBytes % 512 !== 0 || !isPowerOfTwo(overlayBlockSizeBytes)) {
          throw new Error("overlayBlockSizeBytes must be a power of two and a multiple of 512");
        }
        assertValidIdbRemoteChunkSize(chunkSizeBytes, "chunkSizeBytes");
        assertValidIdbRemoteChunkSize(overlayBlockSizeBytes, "overlayBlockSizeBytes");
      } else {
        assertValidOpfsRemoteChunkSize(chunkSizeBytes, "chunkSizeBytes");
        assertValidOpfsRemoteChunkSize(overlayBlockSizeBytes, "overlayBlockSizeBytes");
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
          cacheLimitBytes,
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
      if (payload.cacheLimitBytes !== undefined) {
        const next = payload.cacheLimitBytes as unknown;
        assertValidCacheLimitBytes(next, "cacheLimitBytes");
        meta.cache.cacheLimitBytes = next;
      }
      if (meta.cache.backend === "opfs") {
        assertValidOpfsRemoteChunkSize(meta.cache.chunkSizeBytes, "chunkSizeBytes");
        assertValidOpfsRemoteChunkSize(meta.cache.overlayBlockSizeBytes, "overlayBlockSizeBytes");
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
      let actualSizeBytes = meta.sizeBytes;

      if (meta.source === "local") {
        if (meta.backend === "opfs") {
          if (!meta.remote) {
            actualSizeBytes = await opfsGetDiskSizeBytes(meta.fileName, meta.opfsDirectory);
          } else {
            let totalBytes = 0;
            // Remote-streaming disks store local writes in a runtime overlay.
            try {
              totalBytes += await opfsGetDiskSizeBytes(`${meta.id}.overlay.aerospar`, meta.opfsDirectory);
            } catch {
              // ignore
            }
            // Count cached bytes stored by RemoteStreamingDisk (OpfsLruChunkCache).
            try {
              const cacheKey = await stableCacheKey(meta.remote.url, { blockSize: meta.remote.blockSizeBytes });
              const remoteCacheDir = await opfsGetRemoteCacheDir();
              totalBytes += await opfsReadLruChunkCacheBytes(remoteCacheDir, cacheKey);
            } catch {
              // ignore
            }
            actualSizeBytes = totalBytes;
          }
        } else if (meta.backend === "idb") {
          const db = await openDiskManagerDb();
          try {
            actualSizeBytes = await idbSumDiskChunkBytes(db, meta.id);
          } finally {
            db.close();
          }
        }
        postOk(requestId, { meta, actualSizeBytes });
        return;
      }

      // Remote disks: report local storage usage best-effort.
      if (meta.cache.backend === "idb") {
        const db = await openDiskManagerDb();
        try {
          let totalBytes = 0;
          try {
            // Overlay bytes (user state) live in the `chunks` store under the overlay ID.
            totalBytes += await idbSumDiskChunkBytes(db, meta.cache.overlayFileName);
          } catch {
            // ignore
          }
          try {
            // Legacy per-disk cache may have been stored in the `chunks` store too.
            if (meta.cache.fileName !== meta.cache.overlayFileName) {
              totalBytes += await idbSumDiskChunkBytes(db, meta.cache.fileName);
            }
          } catch {
            // ignore
          }

          try {
            const deliveryTypes =
              meta.remote.delivery === "range"
                ? [remoteRangeDeliveryType(meta.cache.chunkSizeBytes), "range"]
                : [remoteChunkedDeliveryType(meta.cache.chunkSizeBytes), "chunked"];
            const derivedKeys = await Promise.all(
              deliveryTypes.map((deliveryType) =>
                RemoteCacheManager.deriveCacheKey({
                  imageId: meta.remote.imageId,
                  version: meta.remote.version,
                  deliveryType,
                }),
              ),
            );

            const keysToProbe = new Set<string>([
              ...derivedKeys,
              // Legacy IDB caches used un-derived cache identifiers.
              meta.cache.fileName,
              meta.cache.overlayFileName,
              idbOverlayBindingKey(meta.cache.overlayFileName),
            ]);

            const tx = db.transaction(["remote_chunk_meta"], "readonly");
            const metaStore = tx.objectStore("remote_chunk_meta");
            const reqs = Array.from(keysToProbe).map(async (cacheKey) => {
              try {
                return (await idbReq(metaStore.get(cacheKey))) as unknown;
              } catch {
                return null;
              }
            });
            const records = await Promise.all(reqs);
            await idbTxDone(tx);

            for (const rec of records) {
              if (!rec || typeof rec !== "object") continue;
              const bytesUsed = (rec as { bytesUsed?: unknown }).bytesUsed;
              if (typeof bytesUsed === "number" && Number.isFinite(bytesUsed) && bytesUsed > 0) {
                totalBytes += bytesUsed;
              }
            }
          } catch {
            // ignore remote cache probing failures
          }

          actualSizeBytes = totalBytes;
        } finally {
          db.close();
        }
        postOk(requestId, { meta, actualSizeBytes });
        return;
      }

      if (meta.cache.backend !== "opfs") {
        postOk(requestId, { meta, actualSizeBytes });
        return;
      }

      let overlayBytes = 0;
      try {
        overlayBytes = await opfsGetDiskSizeBytes(meta.cache.overlayFileName);
      } catch {
        // ignore (overlay may not exist yet)
      }

      let cacheBytes = 0;

      try {
        const deliveryTypes =
          meta.remote.delivery === "range"
            ? [remoteRangeDeliveryType(meta.cache.chunkSizeBytes), "range"]
            : [remoteChunkedDeliveryType(meta.cache.chunkSizeBytes), "chunked"];

        if (meta.remote.delivery === "range") {
          const remoteCacheDir = await opfsGetRemoteCacheDir();

          for (const deliveryType of deliveryTypes) {
            const cacheKey = await RemoteCacheManager.deriveCacheKey({
              imageId: meta.remote.imageId,
              version: meta.remote.version,
              deliveryType,
            });
            cacheBytes += await opfsReadLruChunkCacheBytes(remoteCacheDir, cacheKey);
          }
        } else {
          const manager = await RemoteCacheManager.openOpfs();
          for (const deliveryType of deliveryTypes) {
            const cacheKey = await RemoteCacheManager.deriveCacheKey({
              imageId: meta.remote.imageId,
              version: meta.remote.version,
              deliveryType,
            });
            const status = await manager.getCacheStatus(cacheKey);
            if (status) cacheBytes += status.cachedBytes;
          }
        }
      } catch {
        // ignore cache probing failures
      }

      // Backwards compatibility: some older remote images stored cached bytes in a single sparse file.
      // Always include it when present so we don't under-count if both legacy + new caches exist.
      if (meta.cache.fileName !== meta.cache.overlayFileName) {
        try {
          cacheBytes += await opfsGetDiskSizeBytes(meta.cache.fileName);
        } catch {
          // ignore
        }
      }

      // Older RemoteRangeDisk versions persisted a cache file keyed by the remote base identity
      // (in addition to the per-disk cache file above). Include it when present so stat_disk
      // can attribute orphaned legacy bytes before the disk is opened (where we now delete it).
      if (meta.remote.delivery === "range") {
        try {
          const imageKey = `${meta.remote.imageId}:${meta.remote.version}:${meta.remote.delivery}`;
          const cacheId = await stableCacheId(imageKey);
          cacheBytes += await opfsGetDiskSizeBytes(`remote-range-cache-${cacheId}.aerospar`).catch(() => 0);
          cacheBytes += await opfsGetDiskSizeBytes(`remote-range-cache-${cacheId}.json`).catch(() => 0);
        } catch {
          // ignore
        }
      }

      actualSizeBytes = overlayBytes + cacheBytes;
      postOk(requestId, { meta, actualSizeBytes });
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
      if (meta.remote) {
        throw new Error("Remote disks cannot be resized.");
      }

      const progressCb = (p: ImportProgress) => postProgress(requestId, p);

      if (meta.backend === "opfs") {
        await opfsResizeDisk(meta.fileName, newSizeBytes, progressCb, meta.opfsDirectory);
        // Resizing invalidates COW overlays (table size depends on disk size).
        await opfsDeleteDisk(`${meta.id}.overlay.aerospar`, meta.opfsDirectory);
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
          if (meta.remote) {
            // Best-effort cache cleanup for remote-streaming disks.
            try {
              const cacheKey = await stableCacheKey(meta.remote.url, { blockSize: meta.remote.blockSizeBytes });
              await removeOpfsEntry(`${OPFS_DISKS_PATH}/${OPFS_REMOTE_CACHE_DIR}/${cacheKey}`, { recursive: true });
            } catch {
              // ignore
            }
          } else {
            await opfsDeleteDisk(meta.fileName, meta.opfsDirectory);
          }

          // Converted images write a sidecar manifest (best-effort cleanup).
          await opfsDeleteDisk(`${meta.id}.manifest.json`);
          // Best-effort cleanup of runtime COW overlay files.
          await opfsDeleteDisk(`${meta.id}.overlay.aerospar`, meta.opfsDirectory);
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
            const manager = await RemoteCacheManager.openOpfs();
            const deliveryTypes =
              meta.remote.delivery === "range"
                ? [remoteRangeDeliveryType(meta.cache.chunkSizeBytes), "range"]
                : meta.remote.delivery === "chunked"
                  ? [remoteChunkedDeliveryType(meta.cache.chunkSizeBytes), "chunked"]
                : [meta.remote.delivery];
            for (const deliveryType of deliveryTypes) {
              const cacheKey = await RemoteCacheManager.deriveCacheKey({
                imageId: meta.remote.imageId,
                version: meta.remote.version,
                deliveryType,
              });
              await manager.clearCache(cacheKey);
            }
          } catch {
            // best-effort cleanup
          }

          await opfsDeleteDisk(meta.cache.fileName);
          // Legacy versions used a small binding file to associate the OPFS Range cache file with the
          // immutable remote base identity. Best-effort cleanup when the disk is deleted.
          await opfsDeleteDisk(`${meta.cache.fileName}.binding.json`);
          // RuntimeDiskWorker `RemoteRangeDisk` stores per-disk cache metadata in a sidecar file.
          if (meta.remote.delivery === "range") {
            await opfsDeleteDisk(`${meta.cache.fileName}.remote-range-meta.json`);
          }
          // Legacy RemoteRangeDisk persisted its own sparse cache + metadata keyed by the remote base identity.
          // Best-effort cleanup when deleting the disk.
          if (meta.remote.delivery === "range") {
            const imageKey = `${meta.remote.imageId}:${meta.remote.version}:${meta.remote.delivery}`;
            const cacheId = await stableCacheId(imageKey);
            await opfsDeleteDisk(`remote-range-cache-${cacheId}.aerospar`);
            await opfsDeleteDisk(`remote-range-cache-${cacheId}.json`);
          }
          await opfsDeleteDisk(meta.cache.overlayFileName);
          // Remote overlays also store a base identity binding so they can be invalidated safely.
          // Best-effort cleanup when deleting the disk.
          await opfsDeleteDisk(`${meta.cache.overlayFileName}.binding.json`);
        } else {
          const db = await openDiskManagerDb();
          try {
            // Remote disk caches may be stored in the dedicated `remote_chunks` store (LRU cache)
            // and/or in the legacy `chunks` store (disk-style sparse chunks).
            // Best-effort cleanup: try both.
            const deliveryTypes =
              meta.remote.delivery === "range"
                ? [remoteRangeDeliveryType(meta.cache.chunkSizeBytes), "range"]
                : meta.remote.delivery === "chunked"
                  ? [remoteChunkedDeliveryType(meta.cache.chunkSizeBytes), "chunked"]
                : [meta.remote.delivery];
            for (const deliveryType of deliveryTypes) {
              const derivedCacheKey = await RemoteCacheManager.deriveCacheKey({
                imageId: meta.remote.imageId,
                version: meta.remote.version,
                deliveryType,
              });
              await idbDeleteRemoteChunkCache(db, derivedCacheKey);
            }
            await idbDeleteRemoteChunkCache(db, meta.cache.fileName);
            await idbDeleteRemoteChunkCache(db, meta.cache.overlayFileName);
            await idbDeleteRemoteChunkCache(db, idbOverlayBindingKey(meta.cache.overlayFileName));
            await idbDeleteDiskData(db, meta.cache.fileName);
            await idbDeleteDiskData(db, meta.cache.overlayFileName);
          } finally {
            db.close();
          }
        }

        // Best-effort cleanup for RemoteStreamingDisk / RemoteRangeDisk / RemoteChunkedDisk cache directories
        // keyed by URL (legacy / openRemote-style paths), if present.
        const url = meta.remote.urls.url;
        if (url && meta.remote.delivery === "range") {
          const blockSizes = new Set([meta.cache.chunkSizeBytes, RANGE_STREAM_CHUNK_SIZE]);
          for (const blockSize of blockSizes) {
            try {
              const cacheKey = await stableCacheKey(url, { blockSize });
              await removeOpfsEntry(`${OPFS_DISKS_PATH}/${OPFS_REMOTE_CACHE_DIR}/${cacheKey}`, { recursive: true });
            } catch {
              // ignore
            }
          }
        }
      }
      await store.deleteDisk(meta.id);
      postOk(requestId, { ok: true });
      return;
    }

    case "prune_remote_caches": {
      if (backend !== "opfs") {
        // Remote cache pruning is only supported for the OPFS backend.
        postOk(requestId, { ok: true, pruned: 0, examined: 0 });
        return;
      }

      const payload = (msg.payload || {}) as { olderThanMs?: unknown; maxCaches?: unknown; dryRun?: unknown };

      const olderThanMs = Number(payload.olderThanMs);
      if (!Number.isFinite(olderThanMs) || olderThanMs < 0) {
        throw new Error("olderThanMs must be a non-negative number");
      }

      let maxCaches: number | undefined = undefined;
      if (payload.maxCaches !== undefined) {
        maxCaches = Number(payload.maxCaches);
        if (!Number.isSafeInteger(maxCaches) || maxCaches < 0) {
          throw new Error("maxCaches must be a non-negative safe integer");
        }
      }

      const dryRun = !!payload.dryRun;

      const remoteCacheDir = await opfsGetRemoteCacheDir();
      const entries = remoteCacheDir.entries?.bind(remoteCacheDir);
      if (!entries) {
        // Best-effort: if directory iteration is unavailable, we cannot enumerate caches.
        postOk(requestId, { ok: true, pruned: 0, examined: 0, ...(dryRun ? { prunedKeys: [] } : {}) });
        return;
      }

      const manager = await RemoteCacheManager.openOpfs();
      const nowMs = Date.now();
      const cutoffMs = nowMs - olderThanMs;

      type Candidate = { cacheKey: string; lastAccessedAtMs: number; stale: boolean };
      const candidates: Candidate[] = [];
      let examined = 0;

      for await (const [name, handle] of entries()) {
        // Caches are stored as directories under `aero/disks/remote-cache/<cacheKey>/`.
        if (handle.kind !== "directory") continue;
        examined += 1;

        let meta = null;
        try {
          meta = await manager.readMeta(name);
        } catch {
          meta = null;
        }

        const last = meta?.lastAccessedAtMs;
        let lastAccessedAtMs = typeof last === "number" && Number.isFinite(last) ? last : Number.NEGATIVE_INFINITY;

        // Some cache implementations (e.g. OPFS LRU chunk cache used by RemoteStreamingDisk) do not
        // update `meta.json` on each access, but *do* persist an `index.json` file on use. Use the
        // index file's `lastModified` timestamp as a best-effort last-access signal so we do not
        // accidentally prune actively-used caches.
        if (meta) {
          try {
            const indexHandle = await (handle as FileSystemDirectoryHandle).getFileHandle("index.json", { create: false });
            const file = await indexHandle.getFile();
            const lm = (file as unknown as { lastModified?: unknown }).lastModified;
            if (typeof lm === "number" && Number.isFinite(lm) && lm > lastAccessedAtMs) {
              lastAccessedAtMs = lm;
            }
          } catch {
            // ignore
          }
        }

        const stale = !meta || lastAccessedAtMs < cutoffMs;
        candidates.push({ cacheKey: name, lastAccessedAtMs, stale });
      }

      const keysToPrune = new Set<string>();
      for (const c of candidates) {
        if (c.stale) keysToPrune.add(c.cacheKey);
      }

      if (maxCaches !== undefined) {
        const remaining = candidates.filter((c) => !keysToPrune.has(c.cacheKey));
        if (remaining.length > maxCaches) {
          const toPrune = remaining
            .slice()
            .sort((a, b) => a.lastAccessedAtMs - b.lastAccessedAtMs || a.cacheKey.localeCompare(b.cacheKey))
            .slice(0, remaining.length - maxCaches);
          for (const c of toPrune) keysToPrune.add(c.cacheKey);
        }
      }

      const orderedToPrune = candidates
        .filter((c) => keysToPrune.has(c.cacheKey))
        .sort((a, b) => a.lastAccessedAtMs - b.lastAccessedAtMs || a.cacheKey.localeCompare(b.cacheKey))
        .map((c) => c.cacheKey);

      let pruned = 0;
      const prunedKeys: string[] = [];
      for (const cacheKey of orderedToPrune) {
        if (dryRun) {
          pruned += 1;
          prunedKeys.push(cacheKey);
          continue;
        }
        try {
          await manager.clearCache(cacheKey);
          pruned += 1;
        } catch {
          // best-effort
        }
      }

      postOk(
        requestId,
        dryRun ? { ok: true, pruned, examined, prunedKeys } : { ok: true, pruned, examined },
      );
      return;
    }

    case "list_remote_caches": {
      if (backend !== "opfs") {
        // Remote cache inspection is currently only supported for the OPFS backend.
        postOk(requestId, { ok: true, caches: [], corruptKeys: [] });
        return;
      }

      const remoteCacheDir = await opfsGetRemoteCacheDir();
      const entries = remoteCacheDir.entries?.bind(remoteCacheDir);
      if (!entries) {
        // Best-effort: if directory iteration is unavailable, we cannot enumerate caches.
        postOk(requestId, { ok: true, caches: [], corruptKeys: [] });
        return;
      }

      const manager = await RemoteCacheManager.openOpfs();

      const caches: RemoteCacheStatus[] = [];
      const corruptKeys: string[] = [];
      let seen = 0;

      for await (const [name, handle] of entries()) {
        // Caches are stored as directories under `aero/disks/remote-cache/<cacheKey>/`.
        if (handle.kind !== "directory") continue;

        seen += 1;
        if (seen > MAX_REMOTE_CACHES_LIST) {
          // Defensive bound: avoid unbounded allocations / work on attacker-controlled OPFS state.
          throw new Error(
            `Refusing to list remote caches: found more than ${MAX_REMOTE_CACHES_LIST} cache directories`,
          );
        }

        let status: RemoteCacheStatus | null = null;
        try {
          status = await manager.getCacheStatus(name);
        } catch {
          status = null;
        }
        if (status) {
          // RemoteCacheManager's persisted metadata tracks cached ranges for some cache types
          // (e.g. sparse caches), but other implementations (e.g. OPFS LRU chunk caches) persist
          // cache bytes in an `index.json` file. Best-effort: if `cachedBytes` is zero, try to
          // derive a better estimate from the LRU index without walking the chunk directory.
          if (status.cachedBytes === 0) {
            try {
              const stats = await opfsReadLruChunkCacheIndexStats(remoteCacheDir, name);
              if (stats && stats.totalBytes > status.cachedBytes) {
                status.cachedBytes = stats.totalBytes;
                status.cachedChunks = Math.max(status.cachedChunks, stats.chunkCount);
                if (typeof stats.lastModifiedMs === "number" && stats.lastModifiedMs > status.lastAccessedAtMs) {
                  status.lastAccessedAtMs = stats.lastModifiedMs;
                }
              }
            } catch {
              // best-effort
            }
          }
          caches.push(status);
        } else {
          corruptKeys.push(name);
        }
      }

      caches.sort((a, b) => b.lastAccessedAtMs - a.lastAccessedAtMs || a.cacheKey.localeCompare(b.cacheKey));
      corruptKeys.sort((a, b) => a.localeCompare(b));

      postOk(requestId, { ok: true, caches, corruptKeys });
      return;
    }

    case "export_disk": {
      const meta = await requireDisk(backend, msg.payload.id);
      if (meta.source !== "local") {
        throw new Error("Remote disks cannot be exported");
      }
      if (meta.remote) {
        throw new Error("Export is not supported for remote streaming disks; download from the original source instead.");
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
            await opfsExportToPort(meta.fileName, port, options, progressCb, meta.opfsDirectory);
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
