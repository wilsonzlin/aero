import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { clearIdb, createIdbMetadataStore, idbReq, idbTxDone, openDiskManagerDb } from "./metadata";

describe("metadata deleteDisk mount handling", () => {
  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    await clearIdb();
  });

  it("does not clear mounts based on inherited Object.prototype.hddId", async () => {
    const hddIdExisting = Object.getOwnPropertyDescriptor(Object.prototype, "hddId");
    if (hddIdExisting && hddIdExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Persist a mounts record that has no own `hddId` (simulates corrupted/foreign IDB state).
      const db = await openDiskManagerDb();
      try {
        const tx = db.transaction(["mounts"], "readwrite");
        tx.objectStore("mounts").put({ key: "mounts", value: {} });
        await idbTxDone(tx);
      } finally {
        db.close();
      }

      // Pollute Object.prototype so naive property reads would observe an inherited mount id.
      Object.defineProperty(Object.prototype, "hddId", { value: "disk1", configurable: true, writable: true });

      const store = createIdbMetadataStore();
      await store.deleteDisk("disk1");

      const db2 = await openDiskManagerDb();
      try {
        const tx2 = db2.transaction(["mounts"], "readonly");
        const rec = (await idbReq(tx2.objectStore("mounts").get("mounts"))) as unknown;
        await idbTxDone(tx2);
        expect(rec).toBeTruthy();
        const recObj = rec as { value?: unknown } | null | undefined;
        expect(recObj?.value).toBeTruthy();
        const value = recObj?.value;
        if (!value || typeof value !== "object") {
          throw new Error("expected mounts record value");
        }
        // The deleteDisk path should not write an explicit `hddId` field just because
        // `Object.prototype.hddId` is polluted.
        expect(Object.prototype.hasOwnProperty.call(value, "hddId")).toBe(false);
      } finally {
        db2.close();
      }
    } finally {
      if (hddIdExisting) Object.defineProperty(Object.prototype, "hddId", hddIdExisting);
      else Reflect.deleteProperty(Object.prototype, "hddId");
    }
  });
});
