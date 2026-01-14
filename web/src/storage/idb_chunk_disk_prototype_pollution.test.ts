import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { IdbChunkDisk } from "./idb_chunk_disk";
import { clearIdb, idbTxDone, openDiskManagerDb } from "./metadata";

describe("IdbChunkDisk prototype pollution hardening", () => {
  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    await clearIdb();
  });

  it("does not observe chunk record data inherited from Object.prototype", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "data");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const diskId = "disk1";
    const capacityBytes = 512;

    // Create a corrupt chunk record that is missing an own `data` field.
    const db = await openDiskManagerDb();
    try {
      const tx = db.transaction(["chunks"], "readwrite");
      tx.objectStore("chunks").put({ id: diskId, index: 0 });
      await idbTxDone(tx);
    } finally {
      db.close();
    }

    try {
      Object.defineProperty(Object.prototype, "data", { value: new Uint8Array([1, 2, 3, 4]).buffer, configurable: true });

      const disk = await IdbChunkDisk.open(diskId, capacityBytes);
      try {
        const buf = new Uint8Array(capacityBytes);
        await disk.readSectors(0, buf);
        // Missing chunk records should be treated as zero-filled, even if `Object.prototype.data`
        // is polluted.
        expect(buf.subarray(0, 8)).toEqual(new Uint8Array(8));
      } finally {
        await disk.close();
      }
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "data", existing);
      else Reflect.deleteProperty(Object.prototype, "data");
    }
  });
});
