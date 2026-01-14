import "../../test/fake_indexeddb_auto.ts";

import { describe, expect, it } from "vitest";

import {
  DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES,
  DISK_MANAGER_DB_NAME,
  METADATA_VERSION,
  clearIdb,
  idbReq,
  idbTxDone,
  openDiskManagerDb,
  upgradeDiskManagerStateJson,
  upgradeDiskMetadata,
} from "./metadata";

describe("disk metadata schema migration", () => {
  it("upgrades OPFS metadata.json v1 -> v2 in-memory", () => {
    const v1 = {
      version: 1,
      disks: {
        d1: {
          id: "d1",
          name: "disk.img",
          backend: "opfs",
          kind: "hdd",
          format: "raw",
          fileName: "d1.img",
          sizeBytes: 1024,
          createdAtMs: 100,
          lastUsedAtMs: 200,
          checksum: { algorithm: "crc32", value: "deadbeef" },
          sourceFileName: "orig.img",
        },
      },
      mounts: { hddId: "d1" },
    };

    const { state, migrated } = upgradeDiskManagerStateJson(JSON.stringify(v1));
    expect(migrated).toBe(true);
    expect(state.version).toBe(METADATA_VERSION);
    expect(state.mounts).toEqual({ hddId: "d1" });

    expect(state.disks.d1).toBeDefined();
    const meta = state.disks.d1!;
    expect(meta.source).toBe("local");
    if (meta.source !== "local") throw new Error("expected local disk");
    expect(meta.id).toBe("d1");
    expect(meta.backend).toBe("opfs");
    expect(meta.fileName).toBe("d1.img");
    expect(meta.checksum).toEqual({ algorithm: "crc32", value: "deadbeef" });
  });

  it("does not allow __proto__ disk IDs to pollute the disks record prototype", () => {
    const protoDisk = {
      source: "local",
      id: "__proto__",
      name: "proto disk",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "__proto__.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const disks = {};
    // Define as an own property without triggering the `__proto__` setter.
    Object.defineProperty(disks, "__proto__", { value: protoDisk, enumerable: true, configurable: true, writable: true });
    const v2 = {
      version: METADATA_VERSION,
      disks,
      mounts: { hddId: "__proto__" },
    };

    const { state, migrated } = upgradeDiskManagerStateJson(JSON.stringify(v2));
    expect(migrated).toBe(false);
    // `__proto__` must be treated as a plain key, not a prototype setter.
    expect(Object.getPrototypeOf(state.disks)).toBe(Object.prototype);
    expect(Object.prototype.hasOwnProperty.call(state.disks, "__proto__")).toBe(true);
    expect((state.disks as any)["__proto__"]?.id).toBe("__proto__");
  });

  it("does not allow Object.prototype mount IDs to affect upgraded mount configs", () => {
    const v2 = {
      version: METADATA_VERSION,
      disks: {},
      mounts: {},
    };

    const hddExisting = Object.getOwnPropertyDescriptor(Object.prototype, "hddId");
    const cdExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cdId");
    if ((hddExisting && hddExisting.configurable === false) || (cdExisting && cdExisting.configurable === false)) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "hddId", { value: "evil", configurable: true });
      Object.defineProperty(Object.prototype, "cdId", { value: "evil2", configurable: true });
      const { state } = upgradeDiskManagerStateJson(JSON.stringify(v2));
      expect(state.mounts.hddId).toBeUndefined();
      expect(state.mounts.cdId).toBeUndefined();
    } finally {
      if (hddExisting) Object.defineProperty(Object.prototype, "hddId", hddExisting);
      else delete (Object.prototype as any).hddId;
      if (cdExisting) Object.defineProperty(Object.prototype, "cdId", cdExisting);
      else delete (Object.prototype as any).cdId;
    }
  });

  it("upgrades v1 disk records (IndexedDB) by adding the v2 discriminant", () => {
    const v1Disk = {
      id: "d2",
      name: "disk2.img",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "d2.img",
      sizeBytes: 2048,
      createdAtMs: 123,
    };

    const upgraded = upgradeDiskMetadata(v1Disk);
    expect(upgraded).toBeDefined();
    expect(upgraded!.source).toBe("local");
    if (upgraded!.source !== "local") throw new Error("expected local disk");
    expect(upgraded).toMatchObject({ id: "d2", backend: "idb", fileName: "d2.img" });
  });

  it("ignores inherited source discriminants when upgrading disk metadata", () => {
    const proto = { source: "remote" };
    const v1Disk = Object.create(proto) as any;
    v1Disk.id = "d2";
    v1Disk.name = "disk2.img";
    v1Disk.backend = "idb";
    v1Disk.kind = "hdd";
    v1Disk.format = "raw";
    v1Disk.fileName = "d2.img";
    v1Disk.sizeBytes = 2048;
    v1Disk.createdAtMs = 123;
 
    const upgraded = upgradeDiskMetadata(v1Disk);
    expect(upgraded).toBeDefined();
    expect(upgraded!.source).toBe("local");
  });

  it("does not accept v1 disk fields inherited from Object.prototype", () => {
    const idExisting = Object.getOwnPropertyDescriptor(Object.prototype, "id");
    const backendExisting = Object.getOwnPropertyDescriptor(Object.prototype, "backend");
    const fileNameExisting = Object.getOwnPropertyDescriptor(Object.prototype, "fileName");
    if (
      (idExisting && idExisting.configurable === false) ||
      (backendExisting && backendExisting.configurable === false) ||
      (fileNameExisting && fileNameExisting.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "id", { value: "polluted", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "backend", { value: "idb", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "fileName", { value: "polluted.img", configurable: true, writable: true });

      expect(upgradeDiskMetadata({})).toBeUndefined();
    } finally {
      if (idExisting) Object.defineProperty(Object.prototype, "id", idExisting);
      else delete (Object.prototype as any).id;
      if (backendExisting) Object.defineProperty(Object.prototype, "backend", backendExisting);
      else delete (Object.prototype as any).backend;
      if (fileNameExisting) Object.defineProperty(Object.prototype, "fileName", fileNameExisting);
      else delete (Object.prototype as any).fileName;
    }
  });

  it("backfills remote disk cacheLimitBytes default when missing", () => {
    const legacyRemote = {
      source: "remote",
      id: "r1",
      name: "Remote",
      kind: "cd",
      format: "iso",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.iso" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    };

    const upgraded = upgradeDiskMetadata(legacyRemote);
    expect(upgraded?.source).toBe("remote");
    if (upgraded?.source !== "remote") throw new Error("expected remote disk");
    expect(upgraded.cache.cacheLimitBytes).toBe(DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES);
  });

  it("backfills remote disk cacheLimitBytes when inherited via prototype pollution", () => {
    const protoCache = { cacheLimitBytes: 123 };
    const cache = Object.create(protoCache) as any;
    cache.chunkSizeBytes = 1024;
    cache.backend = "opfs";
    cache.fileName = "cache.aerospar";
    cache.overlayFileName = "overlay.aerospar";
    cache.overlayBlockSizeBytes = 1024;

    const legacyRemote = {
      source: "remote",
      id: "r1",
      name: "Remote",
      kind: "cd",
      format: "iso",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.iso" },
      },
      cache,
    };

    const upgraded = upgradeDiskMetadata(legacyRemote);
    expect(upgraded?.source).toBe("remote");
    if (upgraded?.source !== "remote") throw new Error("expected remote disk");
    expect(upgraded.cache.cacheLimitBytes).toBe(DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES);
    expect(Object.prototype.hasOwnProperty.call(upgraded.cache, "cacheLimitBytes")).toBe(true);
  });

  it("backfills remote disk cacheLimitBytes in OPFS metadata.json and marks migrated", () => {
    const legacyRemote = {
      source: "remote",
      id: "r1",
      name: "Remote",
      kind: "cd",
      format: "iso",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.iso" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    };

    const v2 = {
      version: METADATA_VERSION,
      disks: { r1: legacyRemote },
      mounts: {},
    };

    const { state, migrated } = upgradeDiskManagerStateJson(JSON.stringify(v2));
    expect(migrated).toBe(true);
    const meta = state.disks.r1;
    expect(meta?.source).toBe("remote");
    if (meta?.source !== "remote") throw new Error("expected remote disk");
    expect(meta.cache.cacheLimitBytes).toBe(DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES);
  });

  it("backfills remote disk cacheLimitBytes even when Object.prototype is polluted", () => {
    const legacyRemote = {
      source: "remote",
      id: "r1",
      name: "Remote",
      kind: "cd",
      format: "iso",
      sizeBytes: 1024,
      createdAtMs: 0,
      remote: {
        imageId: "img1",
        version: "v1",
        delivery: "range",
        urls: { url: "https://example.invalid/disk.iso" },
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "opfs",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    };

    const v2 = {
      version: METADATA_VERSION,
      disks: { r1: legacyRemote },
      mounts: {},
    };

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "cacheLimitBytes");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      Object.defineProperty(Object.prototype, "cacheLimitBytes", { value: 123, configurable: true });
      const { state, migrated } = upgradeDiskManagerStateJson(JSON.stringify(v2));
      expect(migrated).toBe(true);
      const meta = state.disks.r1;
      expect(meta?.source).toBe("remote");
      if (meta?.source !== "remote") throw new Error("expected remote disk");
      expect(meta.cache.cacheLimitBytes).toBe(DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES);
      expect(Object.prototype.hasOwnProperty.call(meta.cache, "cacheLimitBytes")).toBe(true);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "cacheLimitBytes", existing);
      else delete (Object.prototype as any).cacheLimitBytes;
    }
  });

  it("upgrades existing IndexedDB records when opening the DiskManager DB", async () => {
    await clearIdb();

    // Simulate a v1 database (no `source` discriminant on disk records).
    const v1db = await new Promise<IDBDatabase>((resolve, reject) => {
      const req = indexedDB.open(DISK_MANAGER_DB_NAME, 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains("disks")) {
          db.createObjectStore("disks", { keyPath: "id" });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error ?? new Error("IndexedDB open failed"));
    });

    const legacyDisk = {
      id: "legacy",
      name: "legacy.img",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "legacy.img",
      sizeBytes: 1024,
      createdAtMs: 0,
    };
    const tx = v1db.transaction(["disks"], "readwrite");
    tx.objectStore("disks").put(legacyDisk);
    await idbTxDone(tx);
    v1db.close();

    const db = await openDiskManagerDb();
    try {
      const tx2 = db.transaction(["disks"], "readonly");
      const rec = (await idbReq(tx2.objectStore("disks").get("legacy"))) as { source?: string } | undefined;
      await idbTxDone(tx2);

      expect(rec?.source).toBe("local");
      expect(db.objectStoreNames.contains("remote_chunks")).toBe(true);
      expect(db.objectStoreNames.contains("remote_chunk_meta")).toBe(true);
    } finally {
      db.close();
      await clearIdb();
    }
  });
});
