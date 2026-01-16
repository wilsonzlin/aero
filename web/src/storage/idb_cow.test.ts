import "../../test/fake_indexeddb_auto.ts";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { IdbCowDisk } from "./idb_cow";
import { clearIdb, openDiskManagerDb, idbTxDone } from "./metadata";

class MemoryReadOnlyDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readCalls = 0;

  private readonly bytes: Uint8Array;

  constructor(bytes: Uint8Array) {
    this.bytes = bytes;
    this.capacityBytes = bytes.byteLength;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    this.readCalls += 1;
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    buffer.set(this.bytes.subarray(offset, offset + buffer.byteLength));
  }

  async writeSectors(): Promise<void> {
    throw new Error("MemoryReadOnlyDisk is read-only");
  }

  async flush(): Promise<void> {}
}

function buildPatternBytes(size: number): Uint8Array {
  const out = new Uint8Array(size);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = i & 0xff;
  }
  return out;
}

describe("IdbCowDisk", () => {
  beforeEach(async () => {
    await clearIdb();
  });

  afterEach(async () => {
    await clearIdb();
  });

  it("reads through from base and persists overlay writes", async () => {
    // `IdbChunkDisk` uses a fixed 4 MiB chunk size; use a 2-chunk disk so we can
    // exercise the COW overlay without allocating huge buffers.
    const capacityBytes = 8 * 1024 * 1024;
    const baseBytes = buildPatternBytes(capacityBytes);

    const base1 = new MemoryReadOnlyDisk(baseBytes);
    const cow1 = await IdbCowDisk.open(base1, "overlay1", capacityBytes);

    const initial = new Uint8Array(3 * SECTOR_SIZE);
    await cow1.readSectors(0, initial);
    expect(initial).toEqual(baseBytes.slice(0, initial.byteLength));

    // First partial write should seed from base.
    const patch1 = new Uint8Array(SECTOR_SIZE).fill(0xaa);
    base1.readCalls = 0;
    await cow1.writeSectors(1, patch1);
    expect(base1.readCalls).toBeGreaterThan(0);

    // Second write to the same overlay block should not consult base again.
    const patch2 = new Uint8Array(SECTOR_SIZE).fill(0xbb);
    base1.readCalls = 0;
    await cow1.writeSectors(2, patch2);
    expect(base1.readCalls).toBe(0);

    const readBack = new Uint8Array(3 * SECTOR_SIZE);
    await cow1.readSectors(0, readBack);

    const expected = baseBytes.slice(0, readBack.byteLength);
    expected.set(patch1, 1 * SECTOR_SIZE);
    expected.set(patch2, 2 * SECTOR_SIZE);
    expect(readBack).toEqual(expected);

    await cow1.close();

    // Re-open: ensure the overlay persists across sessions.
    const base2 = new MemoryReadOnlyDisk(baseBytes);
    const cow2 = await IdbCowDisk.open(base2, "overlay1", capacityBytes);
    const readBack2 = new Uint8Array(3 * SECTOR_SIZE);
    await cow2.readSectors(0, readBack2);
    expect(readBack2).toEqual(expected);
    await cow2.close();
  });

  it("ignores IDB chunk records with inherited index (prototype pollution)", async () => {
    const capacityBytes = 8 * 1024 * 1024;
    const baseBytes = buildPatternBytes(capacityBytes);

    const indexExisting = Object.getOwnPropertyDescriptor(Object.prototype, "index");
    if (indexExisting && indexExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    try {
      // Pollute Object.prototype so keyPath extraction and later reads can observe an inherited
      // `index` even when the stored record lacks an own `index` field.
      Object.defineProperty(Object.prototype, "index", { value: 0, configurable: true, writable: true });

      // Insert a corrupt chunk record that omits `index` (it will be satisfied via prototype).
      const db = await openDiskManagerDb();
      try {
        const tx = db.transaction(["chunks"], "readwrite");
        tx.objectStore("chunks").put({ id: "overlay_proto", data: new ArrayBuffer(512) });
        await idbTxDone(tx);
      } finally {
        db.close();
      }

      const base = new MemoryReadOnlyDisk(baseBytes);
      const cow = await IdbCowDisk.open(base, "overlay_proto", capacityBytes);

      const read = new Uint8Array(SECTOR_SIZE);
      await cow.readSectors(0, read);

      // If `loadAllocatedChunks` mistakenly treats the corrupt record as allocated, the COW disk
      // will read from the overlay (which ignores the record) and return zeros instead of base data.
      expect(read).toEqual(baseBytes.slice(0, SECTOR_SIZE));

      await cow.close();
    } finally {
      if (indexExisting) Object.defineProperty(Object.prototype, "index", indexExisting);
      else Reflect.deleteProperty(Object.prototype, "index");
    }
  });
});
