import { describe, expect, it } from "vitest";

import { computeAlignedDiskIoRange, diskReadIntoGuest, diskWriteFromGuest, type RuntimeDiskClientLike } from "./io_disk_dma";

function createMockClient(diskData: Uint8Array, sectorSize: number): {
  client: RuntimeDiskClientLike;
  calls: Array<{ op: "read" | "write"; lba: number; byteLength: number }>;
} {
  const calls: Array<{ op: "read" | "write"; lba: number; byteLength: number }> = [];
  const client: RuntimeDiskClientLike = {
    async read(_handle, lba, byteLength) {
      calls.push({ op: "read", lba, byteLength });
      const start = lba * sectorSize;
      const end = start + byteLength;
      return diskData.slice(start, end);
    },
    async write(_handle, lba, data) {
      calls.push({ op: "write", lba, byteLength: data.byteLength });
      diskData.set(data, lba * sectorSize);
    },
  };
  return { client, calls };
}

describe("workers/io_disk_dma", () => {
  it("chunks large reads into bounded runtime-disk requests", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);
    for (let i = 0; i < diskData.length; i++) diskData[i] = (i * 13) & 0xff;

    const { client, calls } = createMockClient(diskData, sectorSize);

    const guest = new Uint8Array(sectorSize * 5);
    const range = computeAlignedDiskIoRange(0n, guest.byteLength, sectorSize);
    expect(range).not.toBeNull();

    await diskReadIntoGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    expect(calls).toEqual([
      { op: "read", lba: 0, byteLength: 1024 },
      { op: "read", lba: 2, byteLength: 1024 },
      { op: "read", lba: 4, byteLength: 512 },
    ]);
    expect(Array.from(guest)).toEqual(Array.from(diskData.subarray(0, guest.byteLength)));
  });

  it("chunks large aligned writes into bounded runtime-disk requests", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);

    const { client, calls } = createMockClient(diskData, sectorSize);

    const guest = new Uint8Array(sectorSize * 5);
    for (let i = 0; i < guest.length; i++) guest[i] = (i * 7) & 0xff;

    const range = computeAlignedDiskIoRange(0n, guest.byteLength, sectorSize);
    expect(range).not.toBeNull();

    await diskWriteFromGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    expect(calls).toEqual([
      { op: "write", lba: 0, byteLength: 1024 },
      { op: "write", lba: 2, byteLength: 1024 },
      { op: "write", lba: 4, byteLength: 512 },
    ]);
    expect(Array.from(diskData.subarray(0, guest.byteLength))).toEqual(Array.from(guest));
  });

  it("preserves surrounding bytes for unaligned writes while chunking", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);
    for (let i = 0; i < diskData.length; i++) diskData[i] = i & 0xff;
    const original = diskData.slice();

    const { client, calls } = createMockClient(diskData, sectorSize);

    const guest = new Uint8Array(2000);
    for (let i = 0; i < guest.length; i++) guest[i] = (0xa0 + i) & 0xff;

    const range = computeAlignedDiskIoRange(1n, guest.byteLength, sectorSize);
    expect(range).not.toBeNull();
    expect(range).toEqual({ lba: 0, byteLength: 2048, offset: 1 });

    await diskWriteFromGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    expect(calls).toEqual([
      { op: "read", lba: 0, byteLength: 1024 },
      { op: "write", lba: 0, byteLength: 1024 },
      { op: "read", lba: 2, byteLength: 1024 },
      { op: "write", lba: 2, byteLength: 1024 },
    ]);

    // The updated payload is applied at byte offset 1; surrounding bytes are preserved.
    expect(diskData[0]).toBe(original[0]);
    expect(Array.from(diskData.subarray(1, 1 + guest.byteLength))).toEqual(Array.from(guest));
    expect(Array.from(diskData.subarray(1 + guest.byteLength, range!.byteLength))).toEqual(
      Array.from(original.subarray(1 + guest.byteLength, range!.byteLength)),
    );
    expect(Array.from(diskData.subarray(range!.byteLength))).toEqual(Array.from(original.subarray(range!.byteLength)));
  });
});

