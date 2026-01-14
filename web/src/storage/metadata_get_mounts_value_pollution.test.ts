import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearIdb, createIdbMetadataStore, idbTxDone, openDiskManagerDb } from "./metadata";

describe("metadata getMounts value pollution handling", () => {
  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    await clearIdb();
  });

  it("does not read mounts record value from inherited Object.prototype.value", async () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "value");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      const db = await openDiskManagerDb();
      try {
        const tx = db.transaction(["mounts"], "readwrite");
        // Simulate corrupted/foreign IDB state: a mounts record without an own `value` field.
        tx.objectStore("mounts").put({ key: "mounts" } as any);
        await idbTxDone(tx);
      } finally {
        db.close();
      }

      // Pollute Object.prototype with an inherited `value` field so naive `rec.value` reads would observe it.
      Object.defineProperty(Object.prototype, "value", {
        value: { hddId: "disk1", cdId: "disk2" },
        configurable: true,
        writable: true,
      });

      const store = createIdbMetadataStore();
      const mounts = await store.getMounts();

      expect(mounts.hddId).toBeUndefined();
      expect(mounts.cdId).toBeUndefined();
      expect(Object.prototype.hasOwnProperty.call(mounts, "hddId")).toBe(false);
      expect(Object.prototype.hasOwnProperty.call(mounts, "cdId")).toBe(false);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "value", existing);
      else delete (Object.prototype as any).value;
    }
  });
});

