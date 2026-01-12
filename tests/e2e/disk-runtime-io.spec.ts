import { expect, test } from "@playwright/test";

test.describe("runtime disk IO worker", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/", { waitUntil: "load" });
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
           const stats1 = await client.stats(opened.handle);
           await client.closeDisk(opened.handle);

           // Re-open and ensure persistence across sessions (including OPFS COW overlays).
           const reopened = await client.open(meta);
           const roundTrip2 = await client.read(reopened.handle, lba, data.length);
           const stats2 = await client.stats(reopened.handle);
           await client.closeDisk(reopened.handle);
           client.close();

           return {
             meta,
             opened,
              roundTrip: Array.from(roundTrip),
              roundTrip2: Array.from(roundTrip2),
              expected: Array.from(data),
              stats1,
              stats2,
           };
         },
         { backend },
       );

       expect(result.meta.backend).toBe(backend);
       expect(result.opened.sectorSize).toBe(512);
       expect(result.roundTrip).toEqual(result.expected);
       expect(result.roundTrip2).toEqual(result.expected);
       expect(result.stats1.remote).toBeNull();
       expect(result.stats1.io.bytesWritten).toBe(result.expected.length);
       expect(result.stats1.io.bytesRead).toBe(result.expected.length);
       expect(result.stats2.io.bytesWritten).toBe(0);
       expect(result.stats2.io.bytesRead).toBe(result.expected.length);
     });
   }

  test("can write across IndexedDB chunk boundary", async ({ page }) => {
    const result = await page.evaluate(async () => {
      const { DiskManager } = await import("/web/src/storage/disk_manager.ts");
      const { RuntimeDiskClient } = await import("/web/src/storage/runtime_disk_client.ts");

      const dm = await DiskManager.create({ backend: "idb" });
      const meta = await dm.createBlankDisk({ name: "rt", sizeBytes: 8 * 1024 * 1024 });
      dm.close();

      const client = new RuntimeDiskClient();
      const opened = await client.open(meta);

      // IDB chunks are 4 MiB. Write 4 KiB straddling the 4 MiB boundary.
      const offsetBytes = 4 * 1024 * 1024 - 2048;
      const lba = offsetBytes / 512;
      const data = new Uint8Array(4096);
      for (let i = 0; i < data.length; i++) data[i] = (i * 13 + 17) & 0xff;

      await client.write(opened.handle, lba, data);
      await client.flush(opened.handle);
      const roundTrip = await client.read(opened.handle, lba, data.length);
      await client.closeDisk(opened.handle);
      client.close();

      return { roundTrip: Array.from(roundTrip), expected: Array.from(data) };
    });

    expect(result.roundTrip).toEqual(result.expected);
  });
});
