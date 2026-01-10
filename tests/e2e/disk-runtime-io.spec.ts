import { expect, test } from "@playwright/test";

test.describe("runtime disk IO worker", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("http://127.0.0.1:5173/", { waitUntil: "load" });
    await page.evaluate(async () => {
      const { DiskManager } = await import("/web/src/storage/disk_manager.ts");
      await DiskManager.clearAllStorage();
    });
  });

  for (const backend of ["idb", "opfs"] as const) {
    test(`can read/write sectors (${backend})`, async ({ page }) => {
      if (backend === "opfs") {
        const supported = await page.evaluate(() => typeof navigator.storage?.getDirectory === "function");
        test.skip(!supported, "OPFS is not supported in this browser");
      }

      const result = await page.evaluate(
        async ({ backend }) => {
          const { DiskManager } = await import("/web/src/storage/disk_manager.ts");
          const { RuntimeDiskClient } = await import("/web/src/storage/runtime_disk_client.ts");

          const dm = await DiskManager.create({ backend });
          const meta = await dm.createBlankDisk({ name: "rt", sizeBytes: 2 * 1024 * 1024 });
          dm.close();

          const client = new RuntimeDiskClient();
          const opened = await client.open(meta);

          const lba = 4;
          const data = new Uint8Array(512 * 2);
          for (let i = 0; i < data.length; i++) data[i] = (i * 7) & 0xff;
          await client.write(opened.handle, lba, data);
          await client.flush(opened.handle);

          const roundTrip = await client.read(opened.handle, lba, data.length);
          await client.closeDisk(opened.handle);
          client.close();

          return { meta, opened, roundTrip: Array.from(roundTrip), expected: Array.from(data) };
        },
        { backend },
      );

      expect(result.meta.backend).toBe(backend);
      expect(result.opened.sectorSize).toBe(512);
      expect(result.roundTrip).toEqual(result.expected);
    });
  }
});
