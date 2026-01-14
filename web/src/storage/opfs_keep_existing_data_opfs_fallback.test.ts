import { afterEach, describe, expect, it } from "vitest";

import { installOpfsMock, MemFileSystemFileHandle } from "../../test/opfs_mock.ts";
import { opfsGetDiskFileHandle, opfsResizeDisk } from "./import_export.ts";
import { OpfsRawDisk } from "./opfs_raw.ts";

let realNavigatorStorage: unknown = undefined;
let hadNavigatorStorage = false;

afterEach(() => {
  // Restore `navigator.storage` after OPFS mock tests.
  const nav = globalThis.navigator as unknown as { storage?: unknown };
  if (hadNavigatorStorage) {
    nav.storage = realNavigatorStorage;
  } else {
    delete nav.storage;
  }
  realNavigatorStorage = undefined;
  hadNavigatorStorage = false;
});

describe("OPFS createWritable({ keepExistingData: true }) options-bag fallback", () => {
  it("opfsResizeDisk falls back to createWritable() when options are rejected", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    installOpfsMock();

    const fileName = "resize-test.bin";
    const handle = await opfsGetDiskFileHandle(fileName, { create: true });
    const seed = Uint8Array.from([1, 2, 3, 4]);
    const w0 = await handle.createWritable({ keepExistingData: false });
    await w0.write(seed);
    await w0.close();

    const originalCreateWritable = MemFileSystemFileHandle.prototype.createWritable;
    (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemFileSystemFileHandle,
      ...args: any[]
    ) {
      if (this.name === fileName && args.length > 0) {
        throw new Error("synthetic createWritable options not supported");
      }
      return await originalCreateWritable.call(this, ...(args as Parameters<typeof originalCreateWritable>));
    };

    try {
      await opfsResizeDisk(fileName, 6, undefined);
    } finally {
      (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    const out = await handle.getFile();
    expect(out.size).toBe(6);
    expect(Array.from(new Uint8Array(await out.arrayBuffer()))).toEqual([1, 2, 3, 4, 0, 0]);
  });

  it("OpfsRawDisk.writeSectors falls back to createWritable() when options are rejected", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    installOpfsMock();

    const fileName = "raw-test.img";
    const disk = await OpfsRawDisk.open(fileName, { create: true, sizeBytes: 4096 });

    const originalCreateWritable = MemFileSystemFileHandle.prototype.createWritable;
    (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemFileSystemFileHandle,
      ...args: any[]
    ) {
      if (this.name === fileName && args.length > 0) {
        throw new Error("synthetic createWritable options not supported");
      }
      return await originalCreateWritable.call(this, ...(args as Parameters<typeof originalCreateWritable>));
    };

    const payload = new Uint8Array(1024);
    for (let i = 0; i < payload.length; i++) payload[i] = (i * 7) & 0xff;

    try {
      await disk.writeSectors(1, payload);
    } finally {
      (MemFileSystemFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    const got = new Uint8Array(1024);
    await disk.readSectors(1, got);
    expect(Array.from(got)).toEqual(Array.from(payload));

    const untouched0 = new Uint8Array(512);
    await disk.readSectors(0, untouched0);
    expect(Array.from(untouched0)).toEqual(Array.from(new Uint8Array(512)));

    const untouched3 = new Uint8Array(512);
    await disk.readSectors(3, untouched3);
    expect(Array.from(untouched3)).toEqual(Array.from(new Uint8Array(512)));
  });
});
