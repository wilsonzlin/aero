import "fake-indexeddb/auto";

import { describe, expect, it } from "vitest";

import { DISK_MANAGER_DB_NAME, METADATA_VERSION, clearIdb, idbReq, idbTxDone, openDiskManagerDb, upgradeDiskManagerStateJson, upgradeDiskMetadata } from "./metadata";

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
