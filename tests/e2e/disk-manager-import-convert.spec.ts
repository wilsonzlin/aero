import { expect, test } from "@playwright/test";

test.describe("disk manager: importDiskConverted()", () => {
  test.skip(({ browserName }) => browserName !== "chromium", "requires Chromium OPFS sync access handles");

  test.beforeEach(async ({ page }, testInfo) => {
    await page.goto("/", { waitUntil: "load" });

    const supported = await page.evaluate(async () => {
      if (!navigator.storage?.getDirectory) return false;
      const url = URL.createObjectURL(
        new Blob(
          [
            `
              self.onmessage = async () => {
                try {
                  const root = await navigator.storage.getDirectory();
                  const file = await root.getFileHandle("tmp-opfs-sync-check", { create: true });
                  self.postMessage({ supported: typeof file.createSyncAccessHandle === "function" });
                } catch (err) {
                  self.postMessage({ supported: false, error: String(err) });
                }
              };
            `,
          ],
          { type: "text/javascript" },
        ),
      );
      return await new Promise<boolean>((resolve) => {
        const w = new Worker(url);
        w.onmessage = (event) => {
          resolve(Boolean((event.data as any)?.supported));
          w.terminate();
          URL.revokeObjectURL(url);
        };
        w.postMessage(null);
      });
    });
    if (!supported) testInfo.skip("OPFS SyncAccessHandle is not supported in this browser");

    await page.evaluate(async () => {
      const { DiskManager } = await import("/web/src/storage/disk_manager.ts");
      await DiskManager.clearAllStorage();
    });
  });

  test("qcow2 imports as aerospar and is readable via RuntimeDiskClient", async ({ page }) => {
    const result = await page.evaluate(async () => {
      function writeU32BE(buf: Uint8Array, offset: number, value: number): void {
        buf[offset] = (value >>> 24) & 0xff;
        buf[offset + 1] = (value >>> 16) & 0xff;
        buf[offset + 2] = (value >>> 8) & 0xff;
        buf[offset + 3] = value & 0xff;
      }

      function writeU64BE(buf: Uint8Array, offset: number, value: bigint): void {
        writeU32BE(buf, offset, Number((value >> 32n) & 0xffff_ffffn));
        writeU32BE(buf, offset + 4, Number(value & 0xffff_ffffn));
      }

      function buildQcow2Fixture(): { file: Uint8Array; logical: Uint8Array } {
        const clusterSize = 512;
        const logicalSize = 1024;

        const l1Offset = 512;
        const l2Offset = 1024;
        const data0Offset = 1536;
        const fileSize = data0Offset + clusterSize;

        const file = new Uint8Array(fileSize);
        file.set([0x51, 0x46, 0x49, 0xfb], 0); // "QFI\xfb"
        writeU32BE(file, 4, 2); // version
        writeU64BE(file, 8, 0n); // backing offset
        writeU32BE(file, 16, 0); // backing size
        writeU32BE(file, 20, 9); // cluster bits
        writeU64BE(file, 24, BigInt(logicalSize));
        writeU32BE(file, 32, 0); // crypt method
        writeU32BE(file, 36, 1); // l1 size
        writeU64BE(file, 40, BigInt(l1Offset));

        // L1 table
        writeU64BE(file, l1Offset + 0, BigInt(l2Offset));

        // L2 table: cluster0 allocated, cluster1 unallocated
        writeU64BE(file, l2Offset + 0, BigInt(data0Offset));
        writeU64BE(file, l2Offset + 8, 0n);

        const cluster0 = new Uint8Array(clusterSize);
        for (let i = 0; i < cluster0.length; i++) cluster0[i] = i & 0xff;
        file.set(cluster0, data0Offset);

        const logical = new Uint8Array(logicalSize);
        logical.set(cluster0, 0);
        return { file, logical };
      }

      const { DiskManager } = await import("/web/src/storage/disk_manager.ts");
      const { RuntimeDiskClient } = await import("/web/src/storage/runtime_disk_client.ts");

      const { file, logical } = buildQcow2Fixture();
      const input = new File([file], "fixture.qcow2", { type: "application/octet-stream" });

      const dm = await DiskManager.create({ backend: "opfs" });
      const meta = await dm.importDiskConverted(input, { name: "converted", blockSizeBytes: 512 });
      dm.close();

      const client = new RuntimeDiskClient();
      const opened = await client.open(meta);
      const roundTrip = await client.read(opened.handle, 0, logical.byteLength);
      await client.closeDisk(opened.handle);
      client.close();

      return { meta, expected: Array.from(logical), actual: Array.from(roundTrip) };
    });

    expect(result.meta.backend).toBe("opfs");
    expect(result.meta.format).toBe("aerospar");
    expect(result.actual).toEqual(result.expected);
  });
});
