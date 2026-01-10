/**
 * Disk image metadata schema and persistence helpers.
 *
 * Designed to run in both window and worker contexts.
 */

export const DISK_MANAGER_DB_NAME = "aero-disk-manager";
export const DISK_MANAGER_DB_VERSION = 1;

export const OPFS_AERO_DIR = "aero";
export const OPFS_DISKS_DIR = "disks";
export const OPFS_METADATA_FILE = "metadata.json";

export const METADATA_VERSION = 1;

export type DiskBackend = "opfs" | "idb";
export type DiskKind = "hdd" | "cd";
export type DiskFormat = "raw" | "iso" | "qcow2" | "unknown";

export type DiskChecksum = {
  algorithm: "crc32";
  value: string;
};

export type DiskImageMetadata = {
  id: string;
  name: string;
  backend: DiskBackend;
  kind: DiskKind;
  format: DiskFormat;
  fileName: string;
  sizeBytes: number;
  createdAtMs: number;
  lastUsedAtMs?: number;
  checksum?: DiskChecksum;
  sourceFileName?: string;
};

export type MountConfig = {
  hddId?: string;
  cdId?: string;
};

export type DiskManagerState = {
  version: number;
  disks: Record<string, DiskImageMetadata>;
  mounts: MountConfig;
};

export function hasOpfs(): boolean {
  return typeof navigator !== "undefined" && !!navigator.storage?.getDirectory;
}

export function pickDefaultBackend(): DiskBackend {
  return hasOpfs() ? "opfs" : "idb";
}

export function inferFormatFromFileName(fileName: string): DiskFormat {
  const lower = fileName.toLowerCase();
  if (lower.endsWith(".iso")) return "iso";
  if (lower.endsWith(".qcow2")) return "qcow2";
  if (lower.endsWith(".img")) return "raw";
  return "unknown";
}

export function extensionForFormat(format: DiskFormat): string {
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
}

export async function opfsGetDisksDir(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  const aeroDir = await root.getDirectoryHandle(OPFS_AERO_DIR, { create: true });
  return aeroDir.getDirectoryHandle(OPFS_DISKS_DIR, { create: true });
}

export function emptyState(): DiskManagerState {
  return { version: METADATA_VERSION, disks: {}, mounts: {} };
}

export async function opfsReadState(): Promise<DiskManagerState> {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  const file = await fileHandle.getFile();
  if (file.size === 0) return emptyState();
  const text = await file.text();
  if (!text.trim()) return emptyState();
  const parsed = JSON.parse(text) as Partial<DiskManagerState> | null;
  if (!parsed || parsed.version !== METADATA_VERSION || typeof parsed.disks !== "object") {
    return emptyState();
  }
  return {
    version: METADATA_VERSION,
    disks: (parsed.disks as DiskManagerState["disks"]) || {},
    mounts: parsed.mounts || {},
  };
}

export async function opfsWriteState(state: DiskManagerState): Promise<void> {
  const disksDir = await opfsGetDisksDir();
  const fileHandle = await disksDir.getFileHandle(OPFS_METADATA_FILE, { create: true });
  const writable = await fileHandle.createWritable({ keepExistingData: false });
  await writable.write(JSON.stringify(state, null, 2));
  await writable.close();
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
      return state.disks[id];
    },
    async putDisk(meta: DiskImageMetadata) {
      await opfsUpdateState((state) => {
        state.disks[meta.id] = meta;
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
      return state.mounts || {};
    },
    async setMounts(mounts: MountConfig) {
      await opfsUpdateState((state) => {
        state.mounts = { ...mounts };
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
      const values = (await idbReq(store.getAll())) as DiskImageMetadata[];
      await idbTxDone(tx);
      db.close();
      return values.sort((a, b) => (b.lastUsedAtMs || 0) - (a.lastUsedAtMs || 0));
    },
    async getDisk(id: string) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["disks"], "readonly");
      const store = tx.objectStore("disks");
      const value = (await idbReq(store.get(id))) as DiskImageMetadata | undefined;
      await idbTxDone(tx);
      db.close();
      return value || undefined;
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
      const mountsRec = (await idbReq(mountsStore.get("mounts"))) as MountsRecord | undefined;
      if (mountsRec?.value) {
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
      const rec = (await idbReq(tx.objectStore("mounts").get("mounts"))) as MountsRecord | undefined;
      await idbTxDone(tx);
      db.close();
      return (rec && rec.value) || {};
    },
    async setMounts(mounts: MountConfig) {
      const db = await openDiskManagerDb();
      const tx = db.transaction(["mounts"], "readwrite");
      tx.objectStore("mounts").put({ key: "mounts", value: { ...mounts } } satisfies MountsRecord);
      await idbTxDone(tx);
      db.close();
    },
  };
}

export function createMetadataStore(backend: DiskBackend): DiskMetadataStore {
  return backend === "opfs" ? createOpfsMetadataStore() : createIdbMetadataStore();
}
