/**
 * Disk image metadata schema and persistence helpers.
 *
 * Designed to run in both window and worker contexts.
 */

export const DISK_MANAGER_DB_NAME = "aero-disk-manager";
export const DISK_MANAGER_DB_VERSION = 3;

export const OPFS_AERO_DIR = "aero";
export const OPFS_DISKS_DIR = "disks";
// Legacy v1 disk images live in OPFS under `images/` (no metadata). The v2 disk
// manager can optionally adopt these without copying.
export const OPFS_LEGACY_IMAGES_DIR = "images";
export const OPFS_DISKS_PATH = `${OPFS_AERO_DIR}/${OPFS_DISKS_DIR}`;
export const OPFS_METADATA_FILE = "metadata.json";
export const OPFS_REMOTE_CACHE_DIR = "remote-cache";

export const METADATA_VERSION = 2;
export const DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES = 512 * 1024 * 1024; // 512 MiB

// Defensive bound: OPFS metadata can become corrupt/attacker-controlled; avoid reading/parsing
// arbitrarily large JSON blobs.
const MAX_OPFS_METADATA_BYTES = 64 * 1024 * 1024; // 64 MiB

export type DiskBackend = "opfs" | "idb";
export type DiskKind = "hdd" | "cd";
export type DiskFormat = "raw" | "iso" | "qcow2" | "vhd" | "aerospar" | "unknown";

export type DiskChecksum = {
  algorithm: "crc32";
  value: string;
};

export type RemoteDiskDelivery = "range" | "chunked";

export type RemoteDiskValidator = {
  etag?: string;
  lastModified?: string;
};

export type RemoteDiskUrls = {
  /**
   * Stable, non-secret URL to the disk bytes endpoint (Range) or manifest (chunked).
   *
   * MUST NOT be a signed URL containing embedded credentials (query params, etc).
   */
  url?: string;
  /**
   * Stable, same-origin API endpoint that returns a temporary signed URL.
   *
   * Storing this endpoint is safe; storing the signed URL is not.
   */
  leaseEndpoint?: string;
};

export type LocalDiskImageMetadata = {
  source: "local";
  id: string;
  name: string;
  backend: DiskBackend;
  kind: DiskKind;
  format: DiskFormat;
  fileName: string;
  /**
   * For OPFS-backed disks, the directory containing `fileName` relative to the
   * OPFS root. Defaults to {@link OPFS_DISKS_PATH}.
   *
   * This supports adopting legacy v1 images in `images/` without copying.
   */
  opfsDirectory?: string;
  sizeBytes: number;
  createdAtMs: number;
  lastUsedAtMs?: number;
  checksum?: DiskChecksum;
  sourceFileName?: string;
  /**
   * Remote streaming source for this disk. When set, the disk's bytes are
   * fetched on-demand via HTTP Range requests and cached in OPFS.
   */
  remote?: {
    url: string;
    blockSizeBytes?: number;
    cacheLimitBytes?: number | null;
    prefetchSequentialBlocks?: number;
  };
};

export type RemoteDiskImageMetadata = {
  source: "remote";
  id: string;
  name: string;
  kind: DiskKind;
  format: DiskFormat;
  /**
   * Expected total disk size in bytes.
   *
   * Used as a cache binding check (size mismatch => cached bytes are invalid).
   */
  sizeBytes: number;
  createdAtMs: number;
  lastUsedAtMs?: number;
  remote: {
    imageId: string;
    version: string;
    delivery: RemoteDiskDelivery;
    urls: RemoteDiskUrls;
    /**
     * Optional expected validator for cache binding. If the remote validator changes,
     * any previously cached bytes must be treated as stale.
     */
    validator?: RemoteDiskValidator;
  };
  cache: {
    chunkSizeBytes: number;
    backend: DiskBackend;
    fileName: string;
    overlayFileName: string;
    overlayBlockSizeBytes: number;
    /**
     * Persistent cache size limit for remote disk delivery.
     *
     * - `undefined` (default): {@link DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES}
     * - `null`: unbounded (no eviction; subject to browser storage quota)
     * - `0`: disable caching entirely (always fetch from network)
     */
    cacheLimitBytes?: number | null;
  };
};

export type DiskImageMetadata = LocalDiskImageMetadata | RemoteDiskImageMetadata;

export type MountConfig = {
  hddId?: string;
  cdId?: string;
};

export type DiskManagerState = {
  version: number;
  disks: Record<string, DiskImageMetadata>;
  mounts: MountConfig;
};

type DiskImageMetadataV1 = Omit<LocalDiskImageMetadata, "source">;
type DiskManagerStateV1 = { version: 1; disks: Record<string, DiskImageMetadataV1>; mounts: MountConfig };

function hasOwnProp(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function emptyMountConfig(): MountConfig {
  // Use a null-prototype object so callers never observe inherited mount IDs (e.g. if
  // `Object.prototype.hddId` is polluted).
  return Object.create(null) as MountConfig;
}

// Avoid prototype pollution when treating untrusted disk IDs as record keys.
//
// Setting `obj["__proto__"] = value` mutates the object's prototype; using `defineProperty` creates
// an own data property instead.
function safeRecordSet<T>(record: Record<string, T>, key: string, value: T): void {
  Object.defineProperty(record, key, {
    value,
    enumerable: true,
    configurable: true,
    writable: true,
  });
}

function normalizeMountConfig(raw: unknown): MountConfig {
  const mounts: MountConfig = emptyMountConfig();
  if (!raw || typeof raw !== "object") return mounts;
  const rec = raw as Record<string, unknown>;
  const sanitizeId = (value: unknown): string | undefined => {
    if (typeof value !== "string") return undefined;
    const trimmed = value.trim();
    return trimmed ? trimmed : undefined;
  };
  if (hasOwnProp(rec, "hddId")) {
    const hddId = sanitizeId(rec.hddId);
    if (hddId) mounts.hddId = hddId;
  }
  if (hasOwnProp(rec, "cdId")) {
    const cdId = sanitizeId(rec.cdId);
    if (cdId) mounts.cdId = cdId;
  }
  return mounts;
}

export function isRemoteDisk(meta: DiskImageMetadata): meta is RemoteDiskImageMetadata {
  return meta.source === "remote";
}

export function isLocalDisk(meta: DiskImageMetadata): meta is LocalDiskImageMetadata {
  return meta.source === "local";
}

export function upgradeDiskMetadata(record: unknown): DiskImageMetadata | undefined {
  if (!record || typeof record !== "object") return undefined;
  const r = record as Partial<DiskImageMetadata> & { source?: unknown };
  // Treat persisted metadata as untrusted. Only accept discriminants from own properties so
  // prototype pollution (e.g. `Object.prototype.source = "remote"`) cannot change how legacy
  // records are interpreted.
  const source = hasOwnProp(r, "source") ? r.source : undefined;

  if (source === "remote") {
    const remote = r as RemoteDiskImageMetadata;
    // Backfill default remote cache limit for older metadata that predated `cacheLimitBytes`.
    // `undefined` means "use default limit".
    const cache = (remote as Partial<RemoteDiskImageMetadata>).cache as RemoteDiskImageMetadata["cache"] | undefined;
    if (
      cache &&
      // Ignore inherited values (prototype pollution); `cacheLimitBytes` must be an own property.
      (!hasOwnProp(cache, "cacheLimitBytes") || (cache as { cacheLimitBytes?: unknown }).cacheLimitBytes === undefined)
    ) {
      // Use `defineProperty` so we can override non-writable inherited properties (e.g. if the
      // global prototype is polluted with a read-only `cacheLimitBytes`).
      Object.defineProperty(cache, "cacheLimitBytes", {
        value: DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES,
        enumerable: true,
        configurable: true,
        writable: true,
      });
    }
    return remote;
  }
  if (source === "local") return r as LocalDiskImageMetadata;

  // v1 records had no `source` field. Treat them as local disks.
  const maybeV1 = record as Partial<DiskImageMetadataV1>;
  if (typeof maybeV1.id === "string" && typeof maybeV1.backend === "string" && typeof maybeV1.fileName === "string") {
    return { ...(maybeV1 as DiskImageMetadataV1), source: "local" };
  }

  return undefined;
}

export function upgradeDiskManagerStateJson(text: string): { state: DiskManagerState; migrated: boolean } {
  if (!text.trim()) return { state: emptyState(), migrated: false };

  let parsed: unknown;
  try {
    parsed = JSON.parse(text) as unknown;
  } catch {
    return { state: emptyState(), migrated: false };
  }

  if (!parsed || typeof parsed !== "object") return { state: emptyState(), migrated: false };
  const raw = parsed as Partial<DiskManagerStateV1 & DiskManagerState>;
  const version = hasOwnProp(raw, "version") ? raw.version : undefined;

  if (version !== 1 && version !== METADATA_VERSION) return { state: emptyState(), migrated: false };

  let migrated = version === 1;

  const out: DiskManagerState = {
    version: METADATA_VERSION,
    disks: {},
    mounts: normalizeMountConfig(hasOwnProp(raw, "mounts") ? raw.mounts : undefined),
  };

  const disks = hasOwnProp(raw, "disks") ? raw.disks : undefined;
  if (disks && typeof disks === "object") {
    for (const [key, value] of Object.entries(disks as Record<string, unknown>)) {
      // v2 -> v2+backfill: older remote disk records may be missing `cache.cacheLimitBytes`.
      // If we detect a missing field, treat this as a migration so OPFS metadata.json can be
      // rewritten with explicit defaults.
      if (!migrated && value && typeof value === "object") {
        const maybe = value as { source?: unknown; cache?: unknown };
        const source = hasOwnProp(maybe, "source") ? maybe.source : undefined;
        const cache = hasOwnProp(maybe, "cache") ? maybe.cache : undefined;
        if (source === "remote" && cache && typeof cache === "object") {
          // Ignore inherited values (prototype pollution). We want to rewrite metadata when
          // `cacheLimitBytes` is missing as an own property so the on-disk JSON is self-contained.
          if (!hasOwnProp(cache, "cacheLimitBytes")) {
            migrated = true;
          }
        }
      }
      const upgraded = upgradeDiskMetadata(value);
      if (upgraded) safeRecordSet(out.disks, upgraded.id || key, upgraded);
    }
  }

  return { state: out, migrated };
}

export function hasOpfs(): boolean {
  return typeof navigator !== "undefined" && !!navigator.storage?.getDirectory;
}

export function hasOpfsSyncAccessHandle(): boolean {
  if (!hasOpfs()) return false;
  const ctor = (globalThis as typeof globalThis & { FileSystemFileHandle?: unknown }).FileSystemFileHandle;
  if (!ctor) return false;
  const proto = (ctor as { prototype?: unknown }).prototype;
  const createSyncAccessHandle = (proto as { createSyncAccessHandle?: unknown } | undefined)?.createSyncAccessHandle;
  return typeof createSyncAccessHandle === "function";
}

export function pickDefaultBackend(): DiskBackend {
  // The current OPFS disk backends require `createSyncAccessHandle()` for random I/O.
  // If sync handles are unavailable, fall back to IndexedDB.
  return hasOpfsSyncAccessHandle() ? "opfs" : "idb";
}

export function inferFormatFromFileName(fileName: string): DiskFormat {
  const lower = fileName.toLowerCase();
  if (lower.endsWith(".iso")) return "iso";
  if (lower.endsWith(".qcow2")) return "qcow2";
  if (lower.endsWith(".vhd")) return "vhd";
  if (lower.endsWith(".aerospar") || lower.endsWith(".aerosparse")) return "aerospar";
  if (lower.endsWith(".img")) return "raw";
  return "unknown";
}

export function extensionForFormat(format: DiskFormat): string {
  switch (format) {
    case "aerospar":
      return "aerospar";
    case "iso":
      return "iso";
    case "qcow2":
      return "qcow2";
    case "vhd":
      return "vhd";
    case "raw":
      return "img";
    default:
      return "bin";
  }
}

export function buildDiskFileName(id: string, format: DiskFormat): string {
  return `${id}.${extensionForFormat(format)}`;
}

export function inferKindFromFileName(fileName: string): DiskKind {
  const format = inferFormatFromFileName(fileName);
  if (format === "iso") return "cd";
  return "hdd";
}

export function newDiskId(): string {
  // randomUUID is available in modern browsers and workers.
  if (typeof crypto !== "undefined" && crypto.randomUUID) return crypto.randomUUID();
  // Very small fallback for older environments (not cryptographically strong).
  return `disk_${Date.now()}_${Math.random().toString(16).slice(2)}`;
}

export function idbReq<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error || new Error("IndexedDB request failed"));
  });
}

export function idbTxDone(tx: IDBTransaction): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error || new Error("IndexedDB transaction aborted"));
    tx.onerror = () => reject(tx.error || new Error("IndexedDB transaction failed"));
  });
}

export async function openDiskManagerDb(): Promise<IDBDatabase> {
  const req = indexedDB.open(DISK_MANAGER_DB_NAME, DISK_MANAGER_DB_VERSION);
  req.onupgradeneeded = (event) => {
    const db = req.result;
    const upgradeTx = req.transaction;
    if (!upgradeTx) throw new Error("IndexedDB upgrade transaction missing");

    const disksStore = db.objectStoreNames.contains("disks")
      ? upgradeTx.objectStore("disks")
      : db.createObjectStore("disks", { keyPath: "id" });

    if (!db.objectStoreNames.contains("mounts")) {
      db.createObjectStore("mounts", { keyPath: "key" });
    }
    if (!db.objectStoreNames.contains("chunks")) {
      const chunks = db.createObjectStore("chunks", { keyPath: ["id", "index"] });
      chunks.createIndex("by_id", "id", { unique: false });
    }
    // v2: add `source` discriminant to disk metadata records so we can support remote-backed disks.
    if (event.oldVersion < 2) {
      const cursorReq = disksStore.openCursor();
      cursorReq.onsuccess = () => {
        const cursor = cursorReq.result;
        if (!cursor) return;
        const upgraded = upgradeDiskMetadata(cursor.value);
        // Treat stored records as untrusted: never observe inherited `source` (prototype pollution).
        const prev = cursor.value;
        const prevSource = prev && typeof prev === "object" && hasOwnProp(prev as object, "source") ? (prev as { source?: unknown }).source : undefined;
        if (upgraded && prevSource !== (upgraded as { source?: unknown }).source) {
          cursor.update(upgraded);
        }
        cursor.continue();
      };
    }

    // Remote streaming disk cache (HTTP Range -> persisted chunks).
    // Keyed by `{cacheKey, chunkIndex}` so multiple remote images can share the same DB.
    if (!db.objectStoreNames.contains("remote_chunk_meta")) {
      db.createObjectStore("remote_chunk_meta", { keyPath: "cacheKey" });
    }
    if (!db.objectStoreNames.contains("remote_chunks")) {
      const remoteChunks = db.createObjectStore("remote_chunks", { keyPath: ["cacheKey", "chunkIndex"] });
      remoteChunks.createIndex("by_cacheKey", "cacheKey", { unique: false });
      remoteChunks.createIndex("by_cacheKey_lastAccess", ["cacheKey", "lastAccess"], { unique: false });
    }
  };
  return idbReq(req);
}

export async function clearIdb(): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const req = indexedDB.deleteDatabase(DISK_MANAGER_DB_NAME);
    req.onsuccess = () => resolve(undefined);
    req.onerror = () => reject(req.error || new Error("IndexedDB deleteDatabase failed"));
    req.onblocked = () => reject(new Error("IndexedDB deleteDatabase blocked"));
  });
}

export async function clearOpfs(): Promise<void> {
  if (!hasOpfs()) return;
  const root = await navigator.storage.getDirectory();
  try {
    await root.removeEntry(OPFS_AERO_DIR, { recursive: true });
  } catch (err) {
    // ignore NotFoundError
  }
  try {
    await root.removeEntry(OPFS_LEGACY_IMAGES_DIR, { recursive: true });
  } catch (err) {
    // ignore NotFoundError
  }
}

function normalizeOpfsRelPath(path: string): string[] {
  const parts = path
    .split("/")
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  for (const p of parts) {
    if (p === "." || p === "..") {
      throw new Error('OPFS paths must not contain "." or "..".');
    }
  }
  return parts;
}

export async function opfsGetDir(dirPath: string, options: { create?: boolean } = {}): Promise<FileSystemDirectoryHandle> {
  const create = options.create ?? false;
  const parts = normalizeOpfsRelPath(dirPath);
  const root = await navigator.storage.getDirectory();
  let dir: FileSystemDirectoryHandle = root;
  for (const part of parts) {
    dir = await dir.getDirectoryHandle(part, { create });
  }
  return dir;
}

export async function opfsGetDisksDir(): Promise<FileSystemDirectoryHandle> {
  return await opfsGetDir(OPFS_DISKS_PATH, { create: true });
}

export async function opfsGetRemoteCacheDir(): Promise<FileSystemDirectoryHandle> {
  const disksDir = await opfsGetDisksDir();
  return disksDir.getDirectoryHandle(OPFS_REMOTE_CACHE_DIR, { create: true });
}

export function emptyState(): DiskManagerState {
  return { version: METADATA_VERSION, disks: {}, mounts: emptyMountConfig() };
}

export async function opfsReadState(): Promise<DiskManagerState> {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  const file = await fileHandle.getFile();
  if (file.size === 0) return emptyState();
  if (!Number.isFinite(file.size) || file.size < 0) return emptyState();
  if (file.size > MAX_OPFS_METADATA_BYTES) {
    // Treat absurdly large metadata as corrupt and start fresh. This is best-effort; callers can
    // recreate disk records by re-importing if needed.
    return emptyState();
  }
  const text = await file.text();
  if (!text.trim()) return emptyState();
  const { state, migrated } = upgradeDiskManagerStateJson(text);
  if (migrated) {
    try {
      await opfsWriteState(state);
    } catch {
      // If the migration write fails (quota, transient errors), keep using the upgraded state.
    }
  }
  return state;
}

export async function opfsWriteState(state: DiskManagerState): Promise<void> {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  let writable: FileSystemWritableFileStream;
  let truncateFallback = false;
  try {
    writable = await fileHandle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await fileHandle.createWritable();
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
    await writable.write(JSON.stringify(state, null, 2));
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

export async function opfsUpdateState<T>(
  mutator: (state: DiskManagerState) => Promise<T> | T,
): Promise<T> {
  const state = await opfsReadState();
  const result = await mutator(state);
  await opfsWriteState(state);
  return result;
}

export type DiskMetadataStore = {
  listDisks(): Promise<DiskImageMetadata[]>;
  getDisk(id: string): Promise<DiskImageMetadata | undefined>;
  putDisk(meta: DiskImageMetadata): Promise<void>;
  deleteDisk(id: string): Promise<void>;
  getMounts(): Promise<MountConfig>;
  setMounts(mounts: MountConfig): Promise<void>;
};

export function createOpfsMetadataStore(): DiskMetadataStore {
  return {
    async listDisks() {
      const state = await opfsReadState();
      return Object.values(state.disks).sort((a, b) => (b.lastUsedAtMs || 0) - (a.lastUsedAtMs || 0));
    },
    async getDisk(id: string) {
      const state = await opfsReadState();
      return hasOwnProp(state.disks, id) ? state.disks[id] : undefined;
    },
    async putDisk(meta: DiskImageMetadata) {
      await opfsUpdateState((state) => {
        safeRecordSet(state.disks, meta.id, meta);
      });
    },
    async deleteDisk(id: string) {
      await opfsUpdateState((state) => {
        delete state.disks[id];
        if (state.mounts.hddId === id) state.mounts.hddId = undefined;
        if (state.mounts.cdId === id) state.mounts.cdId = undefined;
      });
    },
    async getMounts() {
      const state = await opfsReadState();
      return state.mounts || emptyMountConfig();
    },
    async setMounts(mounts: MountConfig) {
      await opfsUpdateState((state) => {
        state.mounts = normalizeMountConfig(mounts);
      });
    },
  };
}

type MountsRecord = { key: "mounts"; value: MountConfig };

export function createIdbMetadataStore(): DiskMetadataStore {
  return {
    async listDisks() {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readonly");
      const store = tx.objectStore("disks");
      const raw = (await idbReq(store.getAll())) as unknown[];
      const values = raw.map((v) => upgradeDiskMetadata(v)).filter(Boolean) as DiskImageMetadata[];
      await idbTxDone(tx);
      db.close();
      return values.sort((a, b) => (b.lastUsedAtMs || 0) - (a.lastUsedAtMs || 0));
    },
    async getDisk(id: string) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readonly");
      const store = tx.objectStore("disks");
      const raw = (await idbReq(store.get(id))) as unknown;
      await idbTxDone(tx);
      db.close();
      return upgradeDiskMetadata(raw) || undefined;
    },
    async putDisk(meta: DiskImageMetadata) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readwrite");
      tx.objectStore("disks").put(meta);
      await idbTxDone(tx);
      db.close();
    },
    async deleteDisk(id: string) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks", "mounts"], "readwrite");
      tx.objectStore("disks").delete(id);
      const mountsStore = tx.objectStore("mounts");
      const mountsRec = (await idbReq(mountsStore.get("mounts"))) as unknown;
      // Treat stored mounts as untrusted: never observe inherited IDs (prototype pollution).
      const mountsRaw =
        mountsRec && typeof mountsRec === "object" && hasOwnProp(mountsRec as object, "value")
          ? (mountsRec as { value?: unknown }).value
          : undefined;
      const normalized = normalizeMountConfig(mountsRaw);
      let changed = false;
      if (normalized.hddId === id) {
        normalized.hddId = undefined;
        changed = true;
      }
      if (normalized.cdId === id) {
        normalized.cdId = undefined;
        changed = true;
      }
      if (changed) {
        mountsStore.put({ key: "mounts", value: { ...normalized } } satisfies MountsRecord);
      }
      await idbTxDone(tx);
      db.close();
    },
    async getMounts() {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["mounts"], "readonly");
      const rec = (await idbReq(tx.objectStore("mounts").get("mounts"))) as MountsRecord | undefined;
      await idbTxDone(tx);
      db.close();
      return normalizeMountConfig(rec?.value);
    },
    async setMounts(mounts: MountConfig) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["mounts"], "readwrite");
      const normalized = normalizeMountConfig(mounts);
      tx.objectStore("mounts").put({ key: "mounts", value: { ...normalized } } satisfies MountsRecord);
      await idbTxDone(tx);
      db.close();
    },
  };
}

export function createMetadataStore(backend: DiskBackend): DiskMetadataStore {
  return backend === "opfs" ? createOpfsMetadataStore() : createIdbMetadataStore();
}
