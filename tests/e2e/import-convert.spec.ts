import { expect, test } from "@playwright/test";

test.describe("web import_convert pipeline (OPFS aerosparse)", () => {
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
      if (!navigator.storage?.getDirectory) return;
      const root = await navigator.storage.getDirectory();
      try {
        await root.removeEntry("import-convert-tests", { recursive: true });
      } catch (err) {
        // ignore NotFoundError
      }
    });
  });

  test("qcow2 -> aerosparse + manifest persisted + logical bytes preserved", async ({ page }) => {
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

      function parseAeroSparse(file: Uint8Array): { blockSizeBytes: number; diskSizeBytes: number; table: number[] } {
        const magic = new TextDecoder().decode(file.slice(0, 8));
        if (magic !== "AEROSPAR") throw new Error(`bad aerosparse magic: ${magic}`);
        const view = new DataView(file.buffer, file.byteOffset, file.byteLength);
        const version = view.getUint32(8, true);
        if (version !== 1) throw new Error(`bad aerosparse version: ${version}`);
        const headerSize = view.getUint32(12, true);
        if (headerSize !== 64) throw new Error(`bad aerosparse header size: ${headerSize}`);
        const blockSizeBytes = view.getUint32(16, true);
        const diskSizeBytes = Number(view.getBigUint64(24, true));
        const tableOffset = Number(view.getBigUint64(32, true));
        if (tableOffset !== 64) throw new Error(`unexpected tableOffset=${tableOffset}`);
        const tableEntries = Number(view.getBigUint64(40, true));
        const tableView = new DataView(file.buffer, file.byteOffset + tableOffset, tableEntries * 8);
        const table = new Array<number>(tableEntries);
        for (let i = 0; i < tableEntries; i++) table[i] = Number(tableView.getBigUint64(i * 8, true));
        return { blockSizeBytes, diskSizeBytes, table };
      }

      function readLogical(
        sparse: { blockSizeBytes: number; diskSizeBytes: number; table: number[] },
        file: Uint8Array,
        offset: number,
        length: number,
      ): Uint8Array {
        const out = new Uint8Array(length);
        let pos = 0;
        while (pos < length) {
          const abs = offset + pos;
          const blockIndex = Math.floor(abs / sparse.blockSizeBytes);
          const within = abs % sparse.blockSizeBytes;
          const chunkLen = Math.min(sparse.blockSizeBytes - within, length - pos);
          const phys = sparse.table[blockIndex] ?? 0;
          if (phys !== 0) {
            out.set(file.subarray(phys + within, phys + within + chunkLen), pos);
          }
          pos += chunkLen;
        }
        return out;
      }

      const { file, logical } = buildQcow2Fixture();
      const input = new File([file], "fixture.qcow2", { type: "application/octet-stream" });

      const worker = new Worker("/web/src/storage/import_convert_worker.ts", { type: "module" });
      const requestId = 1;
      const baseName = "qcow2-fixture";

      const manifest = await new Promise<any>((resolve, reject) => {
        worker.onmessage = (event) => {
          const msg = event.data as any;
          if (msg?.type === "result" && msg.requestId === requestId) {
            if (msg.ok) resolve(msg.manifest);
            else reject(Object.assign(new Error(msg.error?.message || "convert failed"), msg.error));
          }
        };
        worker.onerror = (ev) => reject(ev.error ?? new Error(ev.message));
        worker.postMessage({
          type: "convert",
          requestId,
          source: { kind: "file", file: input },
          destDirPath: "import-convert-tests",
          baseName,
          options: { blockSizeBytes: 512 },
        });
      });
      worker.terminate();

      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle("import-convert-tests", { create: false });
      const manifestFile = await (await dir.getFileHandle(`${baseName}.manifest.json`)).getFile();
      const manifestFromDisk = JSON.parse(await manifestFile.text());

      const sparseFileHandle = await dir.getFileHandle(`${baseName}.aerospar`, { create: false });
      const sparseBytes = new Uint8Array(await (await sparseFileHandle.getFile()).arrayBuffer());
      const parsed = parseAeroSparse(sparseBytes);
      const roundtrip = readLogical(parsed, sparseBytes, 0, logical.byteLength);

      return {
        manifest,
        manifestFromDisk,
        expected: Array.from(logical),
        actual: Array.from(roundtrip),
      };
    });

    expect(result.manifestFromDisk).toEqual(result.manifest);
    expect(result.actual).toEqual(result.expected);
    expect(result.manifest.originalFormat).toBe("qcow2");
    expect(result.manifest.convertedFormat).toBe("aerospar");
  });

  test("dynamic vhd -> aerosparse + logical bytes preserved", async ({ page }) => {
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

      function vhdChecksum(bytes: Uint8Array, checksumOffset: number): number {
        const copy = bytes.slice();
        copy.fill(0, checksumOffset, checksumOffset + 4);
        let sum = 0;
        for (const b of copy) sum = (sum + b) >>> 0;
        return (~sum) >>> 0;
      }

      function buildDynamicVhdFixture(): { file: Uint8Array; logical: Uint8Array } {
        const footerSize = 512;
        const dynHeaderOffset = 512;
        const dynHeaderSize = 1024;
        const batOffset = 1536;
        const blockOff = 2048;

        const blockSize = 1024; // 2 sectors
        const bitmapSize = 512;
        const logicalSize = 1024;

        const footer = new Uint8Array(footerSize);
        footer.set(new TextEncoder().encode("conectix"), 0);
        writeU64BE(footer, 16, BigInt(dynHeaderOffset));
        writeU64BE(footer, 48, BigInt(logicalSize));
        writeU32BE(footer, 60, 3);
        writeU32BE(footer, 64, vhdChecksum(footer, 64));

        const dyn = new Uint8Array(dynHeaderSize);
        dyn.set(new TextEncoder().encode("cxsparse"), 0);
        writeU64BE(dyn, 16, BigInt(batOffset));
        writeU32BE(dyn, 28, 1);
        writeU32BE(dyn, 32, blockSize);
        writeU32BE(dyn, 36, vhdChecksum(dyn, 36));

        const fileSize = blockOff + bitmapSize + blockSize + footerSize;
        const file = new Uint8Array(fileSize);
        file.set(footer, 0);
        file.set(dyn, dynHeaderOffset);

        writeU32BE(file, batOffset, blockOff / 512);

        file[blockOff] = 0x80; // sector 0 allocated
        const dataBase = blockOff + bitmapSize;
        const sector0 = new Uint8Array(512);
        for (let i = 0; i < sector0.length; i++) sector0[i] = (0xa0 + i) & 0xff;
        file.set(sector0, dataBase);
        file.fill(0x55, dataBase + 512, dataBase + 1024);

        file.set(footer, fileSize - footerSize);

        const logical = new Uint8Array(logicalSize);
        logical.set(sector0, 0);
        return { file, logical };
      }

      function parseAeroSparse(file: Uint8Array): { blockSizeBytes: number; diskSizeBytes: number; table: number[] } {
        const magic = new TextDecoder().decode(file.slice(0, 8));
        if (magic !== "AEROSPAR") throw new Error(`bad aerosparse magic: ${magic}`);
        const view = new DataView(file.buffer, file.byteOffset, file.byteLength);
        const version = view.getUint32(8, true);
        if (version !== 1) throw new Error(`bad aerosparse version: ${version}`);
        const headerSize = view.getUint32(12, true);
        if (headerSize !== 64) throw new Error(`bad aerosparse header size: ${headerSize}`);
        const blockSizeBytes = view.getUint32(16, true);
        const diskSizeBytes = Number(view.getBigUint64(24, true));
        const tableOffset = Number(view.getBigUint64(32, true));
        if (tableOffset !== 64) throw new Error(`unexpected tableOffset=${tableOffset}`);
        const tableEntries = Number(view.getBigUint64(40, true));
        const tableView = new DataView(file.buffer, file.byteOffset + tableOffset, tableEntries * 8);
        const table = new Array<number>(tableEntries);
        for (let i = 0; i < tableEntries; i++) table[i] = Number(tableView.getBigUint64(i * 8, true));
        return { blockSizeBytes, diskSizeBytes, table };
      }

      function readLogical(
        sparse: { blockSizeBytes: number; diskSizeBytes: number; table: number[] },
        file: Uint8Array,
        offset: number,
        length: number,
      ): Uint8Array {
        const out = new Uint8Array(length);
        let pos = 0;
        while (pos < length) {
          const abs = offset + pos;
          const blockIndex = Math.floor(abs / sparse.blockSizeBytes);
          const within = abs % sparse.blockSizeBytes;
          const chunkLen = Math.min(sparse.blockSizeBytes - within, length - pos);
          const phys = sparse.table[blockIndex] ?? 0;
          if (phys !== 0) {
            out.set(file.subarray(phys + within, phys + within + chunkLen), pos);
          }
          pos += chunkLen;
        }
        return out;
      }

      const { file, logical } = buildDynamicVhdFixture();
      const input = new File([file], "fixture.vhd", { type: "application/octet-stream" });

      const worker = new Worker("/web/src/storage/import_convert_worker.ts", { type: "module" });
      const requestId = 1;
      const baseName = "vhd-fixture";
      const manifest = await new Promise<any>((resolve, reject) => {
        worker.onmessage = (event) => {
          const msg = event.data as any;
          if (msg?.type === "result" && msg.requestId === requestId) {
            if (msg.ok) resolve(msg.manifest);
            else reject(Object.assign(new Error(msg.error?.message || "convert failed"), msg.error));
          }
        };
        worker.onerror = (ev) => reject(ev.error ?? new Error(ev.message));
        worker.postMessage({
          type: "convert",
          requestId,
          source: { kind: "file", file: input },
          destDirPath: "import-convert-tests",
          baseName,
          options: { blockSizeBytes: 512 },
        });
      });
      worker.terminate();

      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle("import-convert-tests", { create: false });
      const sparseFileHandle = await dir.getFileHandle(`${baseName}.aerospar`, { create: false });
      const sparseBytes = new Uint8Array(await (await sparseFileHandle.getFile()).arrayBuffer());
      const parsed = parseAeroSparse(sparseBytes);
      const roundtrip = readLogical(parsed, sparseBytes, 0, logical.byteLength);

      return {
        manifest,
        expected: Array.from(logical),
        actual: Array.from(roundtrip),
      };
    });

    expect(result.actual).toEqual(result.expected);
    expect(result.manifest.originalFormat).toBe("vhd");
    expect(result.manifest.convertedFormat).toBe("aerospar");
  });

  test("conversion can be canceled", async ({ page }) => {
    const result = await page.evaluate(async () => {
      const bytes = new Uint8Array(8 * 1024 * 1024);
      bytes[1024] = 0x5a;
      bytes[bytes.length - 1] = 0xa5;
      const input = new File([bytes], "cancel.img", { type: "application/octet-stream" });

      const worker = new Worker("/web/src/storage/import_convert_worker.ts", { type: "module" });
      const requestId = 1;

      const res = await new Promise<{ ok: boolean; errorName?: string; errorMessage?: string }>((resolve) => {
        worker.onmessage = (event) => {
          const msg = event.data as any;
          if (msg?.type === "progress" && msg.requestId === requestId) {
            if (msg.processedBytes >= 1024 * 1024) {
              worker.postMessage({ type: "abort", requestId });
            }
          }
          if (msg?.type === "result" && msg.requestId === requestId) {
            if (msg.ok) resolve({ ok: true });
            else resolve({ ok: false, errorName: msg.error?.name, errorMessage: msg.error?.message });
          }
        };
        worker.postMessage({
          type: "convert",
          requestId,
          source: { kind: "file", file: input },
          destDirPath: "import-convert-tests",
          baseName: "cancel-fixture",
          options: { blockSizeBytes: 1024 * 1024 },
        });
      });

      worker.terminate();
      return res;
    });

    expect(result.ok).toBe(false);
    expect(result.errorName).toBe("AbortError");
  });
});
