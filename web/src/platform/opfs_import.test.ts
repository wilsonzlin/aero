import { afterEach, describe, expect, it } from "vitest";

import { installOpfsMock } from "../../test/opfs_mock";
import { importFileToOpfs, openFileHandle } from "./opfs";

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

describe("importFileToOpfs", () => {
  it("truncates the destination when createWritable options are unsupported", async () => {
    const nav = globalThis.navigator as unknown as { storage?: unknown };
    realNavigatorStorage = nav.storage;
    hadNavigatorStorage = Object.prototype.hasOwnProperty.call(nav, "storage");

    installOpfsMock();

    const path = "imports/test.bin";
    const destHandle = await openFileHandle(path, { create: true });

    // Seed an existing longer file.
    const w1 = await destHandle.createWritable({ keepExistingData: false });
    await w1.write(new Uint8Array([1, 2, 3, 4, 5, 6]));
    await w1.close();

    // Simulate an implementation that rejects the options bag, forcing importFileToOpfs to fall
    // back to `createWritable()` (which can behave like keepExistingData=true).
    const dest = destHandle as unknown as { createWritable: (...args: unknown[]) => Promise<unknown> };
    const originalCreateWritable = dest.createWritable;
    dest.createWritable = async (...args: unknown[]) => {
      if (args.length > 0) throw new Error("synthetic createWritable options not supported");
      return originalCreateWritable.call(destHandle);
    };

    const srcFile = new File([new Uint8Array([9, 9])], "src.bin");
    await importFileToOpfs(srcFile, path);

    const outFile = await (await openFileHandle(path, { create: false })).getFile();
    const outBytes = new Uint8Array(await outFile.arrayBuffer());
    expect(Array.from(outBytes)).toEqual([9, 9]);
  });
});
