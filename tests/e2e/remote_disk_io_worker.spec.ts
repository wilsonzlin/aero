import { expect, test } from "@playwright/test";

import { buildTestImage, startDiskImageServer, type DiskImageServer } from "../fixtures/servers";

test.describe("remote disk IO worker (HTTP Range → shared memory)", () => {
  const IMAGE_SIZE = 4096;
  const IMAGE = buildTestImage(IMAGE_SIZE);

  let server!: DiskImageServer;

  test.beforeAll(async () => {
    server = await startDiskImageServer({ data: IMAGE, enableCors: true });
  });

  test.afterAll(async () => {
    await server.close();
  });

  test("can open and diskRead into shared WebAssembly.Memory", async ({ page }) => {
    await page.goto("http://127.0.0.1:5173/", { waitUntil: "load" });

    const result = await page.evaluate(
      async ({ url }) => {
        if (!globalThis.crossOriginIsolated || typeof SharedArrayBuffer === "undefined") {
          throw new Error("test requires crossOriginIsolated + SharedArrayBuffer");
        }

        const { IoWorkerClient } = await import("/web/src/workers/io_worker_client.ts");

        const io = new IoWorkerClient();
        try {
          const openRes = await io.openRemoteDisk(url, {
            blockSize: 1024,
            // Keep the test portable across browsers/contexts without OPFS.
            cacheLimitMiB: null,
            prefetchSequentialBlocks: 0,
          });

          const diskOffset = 123;
          const length = 64;
          const guestOffset = 256;

          const guestMemory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });

          await io.diskReadIntoSharedMemory({ diskOffset, guestMemory, guestOffset, length });

          const bytes = Array.from(new Uint8Array(guestMemory.buffer, guestOffset, length));

          return { size: openRes.size, bytes, diskOffset };
        } finally {
          io.close();
        }
      },
      { url: server.url("/disk.img") },
    );

    expect(result.size).toBe(IMAGE_SIZE);
    expect(result.bytes).toEqual(Array.from(IMAGE.subarray(result.diskOffset, result.diskOffset + result.bytes.length)));
  });
});

