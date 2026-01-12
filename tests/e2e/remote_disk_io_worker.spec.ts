import { expect, test } from "@playwright/test";

import { buildTestImage, startDiskImageServer, type DiskImageServer } from "../fixtures/servers";

test.describe("runtime disk worker (HTTP Range)", () => {
  const IMAGE_SIZE = 4096;
  const IMAGE = buildTestImage(IMAGE_SIZE);

  let server!: DiskImageServer;

  test.beforeAll(async () => {
    server = await startDiskImageServer({ data: IMAGE, enableCors: true });
  });

  test.afterAll(async () => {
    await server.close();
  });

  test("can open and read bytes via RuntimeDiskClient", async ({ page }) => {
    await page.goto("/", { waitUntil: "load" });

    const result = await page.evaluate(
      async ({ url }) => {
        const { RuntimeDiskClient } = await import("/web/src/storage/runtime_disk_client.ts");

        const io = new RuntimeDiskClient();
        let handle: number | null = null;
        try {
          const openRes = await io.openRemote(url, {
            blockSize: 1024,
            // Keep the test portable across browsers/contexts without OPFS/IDB.
            cacheLimitBytes: 0,
            prefetchSequentialBlocks: 0,
          });
          handle = openRes.handle;

          const diskOffset = 123;
          const length = 64;
          const guestOffset = 256;

          const guestMemory = new WebAssembly.Memory({ initial: 1, maximum: 1 });

          const sectorSize = openRes.sectorSize;
          const startLba = Math.floor(diskOffset / sectorSize);
          const offset = diskOffset - startLba * sectorSize;
          const end = diskOffset + length;
          const endLba = Math.ceil(end / sectorSize);
          const readBytes = Math.max(0, endLba - startLba) * sectorSize;

          const data = await io.read(openRes.handle, startLba, readBytes);
          const slice = data.slice(offset, offset + length);
          new Uint8Array(guestMemory.buffer, guestOffset, length).set(slice);

          const bytes = Array.from(new Uint8Array(guestMemory.buffer, guestOffset, length));

          return { size: openRes.capacityBytes, bytes, diskOffset };
        } finally {
          if (handle !== null) {
            try {
              await io.closeDisk(handle);
            } catch {
              // ignore
            }
          }
          io.close();
        }
      },
      { url: server.url("/disk.img") },
    );

    expect(result.size).toBe(IMAGE_SIZE);
    expect(result.bytes).toEqual(Array.from(IMAGE.subarray(result.diskOffset, result.diskOffset + result.bytes.length)));
  });
});
