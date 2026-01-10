// @ts-check

/**
 * Disk image metadata schema and persistence helpers.
 *
 * This module is designed to run in both window and worker contexts.
 * It intentionally avoids any framework/runtime assumptions (Vite, etc).
 */

export const DISK_MANAGER_DB_NAME = "aero-disk-manager";
export const DISK_MANAGER_DB_VERSION = 1;

export const OPFS_AERO_DIR = "aero";
export const OPFS_DISKS_DIR = "disks";
export const OPFS_METADATA_FILE = "metadata.json";

export const METADATA_VERSION = 1;

/**
 * @typedef {"opfs" | "idb"} DiskBackend
 */

/**
 * @typedef {"hdd" | "cd"} DiskKind
 */

/**
 * @typedef {"raw" | "iso" | "qcow2" | "unknown"} DiskFormat
 */

/**
 * @typedef DiskChecksum
 * @property {"crc32"} algorithm
 * @property {string} value
 */

/**
 * @typedef DiskImageMetadata
 * @property {string} id
 * @property {string} name
 * @property {DiskBackend} backend
 * @property {DiskKind} kind
 * @property {DiskFormat} format
 * @property {string} fileName
 * @property {number} sizeBytes
 * @property {number} createdAtMs
 * @property {number | undefined} lastUsedAtMs
 * @property {DiskChecksum | undefined} checksum
 * @property {string | undefined} sourceFileName
 */

/**
 * @typedef MountConfig
 * @property {string | undefined} hddId
 * @property {string | undefined} cdId
 */

/**
 * @typedef DiskManagerState
 * @property {number} version
 * @property {Record<string, DiskImageMetadata>} disks
 * @property {MountConfig} mounts
 */

export function hasOpfs() {
  return !!navigator.storage?.getDirectory;
}

/**
 * @returns {DiskBackend}
 */
export function pickDefaultBackend() {
  return hasOpfs() ? "opfs" : "idb";
}

/**
 * @param {string} fileName
 * @returns {DiskFormat}
 */
export function inferFormatFromFileName(fileName) {
  const lower = fileName.toLowerCase();
  if (lower.endsWith(".iso")) return "iso";
  if (lower.endsWith(".qcow2")) return "qcow2";
  if (lower.endsWith(".img")) return "raw";
  return "unknown";
}

/**
 * @param {DiskFormat} format
 * @returns {string}
 */
export function extensionForFormat(format) {
  switch (format) {
    case "iso":
      return "iso";
    case "qcow2":
      return "qcow2";
    case "raw":
      return "img";
    default:
      return "bin";
  }
}

/**
 * @param {string} id
 * @param {DiskFormat} format
 * @returns {string}
 */
export function buildDiskFileName(id, format) {
  return `${id}.${extensionForFormat(format)}`;
}

/**
 * @param {string} fileName
 * @returns {DiskKind}
 */
export function inferKindFromFileName(fileName) {
  const format = inferFormatFromFileName(fileName);
  if (format === "iso") return "cd";
  return "hdd";
}

/**
 * @returns {string}
 */
export function newDiskId() {
  // randomUUID is available in modern browsers and workers.
  if (crypto?.randomUUID) return crypto.randomUUID();
  // Very small fallback for older environments (not cryptographically strong).
  return `disk_${Date.now()}_${Math.random().toString(16).slice(2)}`;
}

/**
 * @template T
 * @param {IDBRequest<T>} req
 * @returns {Promise<T>}
 */
export function idbReq(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error || new Error("IndexedDB request failed"));
  });
}

/**
 * @param {IDBTransaction} tx
 * @returns {Promise<void>}
 */
export function idbTxDone(tx) {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error || new Error("IndexedDB transaction aborted"));
    tx.onerror = () => reject(tx.error || new Error("IndexedDB transaction failed"));
  });
}

/**
 * @returns {Promise<IDBDatabase>}
 */
export async function openDiskManagerDb() {
  const req = indexedDB.open(DISK_MANAGER_DB_NAME, DISK_MANAGER_DB_VERSION);
  req.onupgradeneeded = () => {
    const db = req.result;
    if (!db.objectStoreNames.contains("disks")) {
      db.createObjectStore("disks", { keyPath: "id" });
    }
    if (!db.objectStoreNames.contains("mounts")) {
      db.createObjectStore("mounts", { keyPath: "key" });
    }
    if (!db.objectStoreNames.contains("chunks")) {
      const chunks = db.createObjectStore("chunks", { keyPath: ["id", "index"] });
      chunks.createIndex("by_id", "id", { unique: false });
    }
  };
  return idbReq(req);
}

/**
 * @returns {Promise<void>}
 */
export async function clearIdb() {
  await new Promise((resolve, reject) => {
    const req = indexedDB.deleteDatabase(DISK_MANAGER_DB_NAME);
    req.onsuccess = () => resolve(undefined);
    req.onerror = () => reject(req.error || new Error("IndexedDB deleteDatabase failed"));
    req.onblocked = () => reject(new Error("IndexedDB deleteDatabase blocked"));
  });
}

/**
 * @returns {Promise<void>}
 */
export async function clearOpfs() {
  if (!hasOpfs()) return;
  const root = await navigator.storage.getDirectory();
  try {
    await root.removeEntry(OPFS_AERO_DIR, { recursive: true });
  } catch (err) {
    // ignore NotFoundError
  }
}

/**
 * @returns {Promise<FileSystemDirectoryHandle>}
 */
export async function opfsGetDisksDir() {
  const root = await navigator.storage.getDirectory();
  const aeroDir = await root.getDirectoryHandle(OPFS_AERO_DIR, { create: true });
  return aeroDir.getDirectoryHandle(OPFS_DISKS_DIR, { create: true });
}

/**
 * @returns {DiskManagerState}
 */
export function emptyState() {
  return { version: METADATA_VERSION, disks: {}, mounts: {} };
}

/**
 * @returns {Promise<DiskManagerState>}
 */
export async function opfsReadState() {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  const file = await fileHandle.getFile();
  if (file.size === 0) return emptyState();
  const text = await file.text();
  if (!text.trim()) return emptyState();
  /** @type {DiskManagerState} */
  const parsed = JSON.parse(text);
  if (!parsed || parsed.version !== METADATA_VERSION || typeof parsed.disks !== "object") {
    return emptyState();
  }
  parsed.mounts ||= {};
  return parsed;
}

/**
 * @param {DiskManagerState} state
 * @returns {Promise<void>}
 */
export async function opfsWriteState(state) {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  const writable = await fileHandle.createWritable({ keepExistingData: false });
  await writable.write(JSON.stringify(state, null, 2));
  await writable.close();
}

/**
 * @template T
 * @param {(state: DiskManagerState) => Promise<T> | T} mutator
 * @returns {Promise<T>}
 */
export async function opfsUpdateState(mutator) {
  const state = await opfsReadState();
  const result = await mutator(state);
  await opfsWriteState(state);
  return result;
}

/**
 * Metadata store interface used by the disk worker.
 * @typedef DiskMetadataStore
 * @property {() => Promise<DiskImageMetadata[]>} listDisks
 * @property {(id: string) => Promise<DiskImageMetadata | undefined>} getDisk
 * @property {(meta: DiskImageMetadata) => Promise<void>} putDisk
 * @property {(id: string) => Promise<void>} deleteDisk
 * @property {() => Promise<MountConfig>} getMounts
 * @property {(mounts: MountConfig) => Promise<void>} setMounts
 */

/**
 * @returns {DiskMetadataStore}
 */
export function createOpfsMetadataStore() {
  return {
    async listDisks() {
      const state = await opfsReadState();
      return Object.values(state.disks).sort((a, b) => (b.lastUsedAtMs || 0) - (a.lastUsedAtMs || 0));
    },
    async getDisk(id) {
      const state = await opfsReadState();
      return state.disks[id];
    },
    async putDisk(meta) {
      await opfsUpdateState((state) => {
        state.disks[meta.id] = meta;
      });
    },
    async deleteDisk(id) {
      await opfsUpdateState((state) => {
        delete state.disks[id];
        if (state.mounts.hddId === id) state.mounts.hddId = undefined;
        if (state.mounts.cdId === id) state.mounts.cdId = undefined;
      });
    },
    async getMounts() {
      const state = await opfsReadState();
      return state.mounts || {};
    },
    async setMounts(mounts) {
      await opfsUpdateState((state) => {
        state.mounts = { ...mounts };
      });
    },
  };
}

/**
 * @returns {DiskMetadataStore}
 */
export function createIdbMetadataStore() {
  return {
    async listDisks() {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readonly");
      const store = tx.objectStore("disks");
      const values = await idbReq(store.getAll());
      await idbTxDone(tx);
      db.close();
      return values.sort((a, b) => (b.lastUsedAtMs || 0) - (a.lastUsedAtMs || 0));
    },
    async getDisk(id) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readonly");
      const store = tx.objectStore("disks");
      const value = await idbReq(store.get(id));
      await idbTxDone(tx);
      db.close();
      return value || undefined;
    },
    async putDisk(meta) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readwrite");
      tx.objectStore("disks").put(meta);
      await idbTxDone(tx);
      db.close();
    },
    async deleteDisk(id) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks", "mounts"], "readwrite");
      tx.objectStore("disks").delete(id);
      const mountsStore = tx.objectStore("mounts");
      const mountsRec = await idbReq(mountsStore.get("mounts"));
      if (mountsRec && mountsRec.value) {
        if (mountsRec.value.hddId === id) mountsRec.value.hddId = undefined;
        if (mountsRec.value.cdId === id) mountsRec.value.cdId = undefined;
        mountsStore.put(mountsRec);
      }
      await idbTxDone(tx);
      db.close();
    },
    async getMounts() {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["mounts"], "readonly");
      const rec = await idbReq(tx.objectStore("mounts").get("mounts"));
      await idbTxDone(tx);
      db.close();
      return (rec && rec.value) || {};
    },
    async setMounts(mounts) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["mounts"], "readwrite");
      tx.objectStore("mounts").put({ key: "mounts", value: { ...mounts } });
      await idbTxDone(tx);
      db.close();
    },
  };
}

/**
 * @param {DiskBackend} backend
 * @returns {DiskMetadataStore}
 */
export function createMetadataStore(backend) {
  return backend === "opfs" ? createOpfsMetadataStore() : createIdbMetadataStore();
}

