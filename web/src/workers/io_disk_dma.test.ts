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

  it("reads unaligned ranges correctly while chunking", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);
    for (let i = 0; i < diskData.length; i++) diskData[i] = (0x80 + i) & 0xff;

    const { client, calls } = createMockClient(diskData, sectorSize);

    const guest = new Uint8Array(2000);
    const range = computeAlignedDiskIoRange(1n, guest.byteLength, sectorSize);
    expect(range).toEqual({ lba: 0, byteLength: 2048, offset: 1 });

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
    ]);
    expect(Array.from(guest)).toEqual(Array.from(diskData.subarray(1, 1 + guest.byteLength)));
  });

  it("does not use readInto() for unaligned disk offsets even when guest memory is SharedArrayBuffer", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);
    for (let i = 0; i < diskData.length; i++) diskData[i] = (0x33 + i) & 0xff;

    const calls: Array<{ op: "read"; lba: number; byteLength: number }> = [];
    const client: RuntimeDiskClientLike = {
      async read(_handle, lba, byteLength) {
        calls.push({ op: "read", lba, byteLength });
        const start = lba * sectorSize;
        const end = start + byteLength;
        return diskData.slice(start, end);
      },
      async readInto() {
        throw new Error("unexpected readInto()");
      },
      async write() {
        throw new Error("unexpected write()");
      },
    };

    const guestLen = 2000;
    const sab = new SharedArrayBuffer(guestLen);
    const guest = new Uint8Array(sab);
    const range = computeAlignedDiskIoRange(1n, guestLen, sectorSize);
    expect(range).toEqual({ lba: 0, byteLength: 2048, offset: 1 });

    await diskReadIntoGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    // Still uses read() because disk offset is unaligned.
    expect(calls).toEqual([
      { op: "read", lba: 0, byteLength: 1024 },
      { op: "read", lba: 2, byteLength: 1024 },
    ]);
    expect(Array.from(guest)).toEqual(Array.from(diskData.subarray(1, 1 + guestLen)));
  });

  it("chunks aligned reads via readInto() when guest memory is SharedArrayBuffer", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 5);
    for (let i = 0; i < diskData.length; i++) diskData[i] = (i * 11) & 0xff;

    const calls: Array<{ op: "readInto"; lba: number; byteLength: number; offsetBytes: number }> = [];
    const client: RuntimeDiskClientLike = {
      async read() {
        throw new Error("unexpected read()");
      },
      async write() {
        throw new Error("unexpected write()");
      },
      async readInto(_handle, lba, byteLength, dest) {
        calls.push({ op: "readInto", lba, byteLength, offsetBytes: dest.offsetBytes });
        const start = lba * sectorSize;
        new Uint8Array(dest.sab, dest.offsetBytes, byteLength).set(diskData.subarray(start, start + byteLength));
      },
    };

    const sab = new SharedArrayBuffer(diskData.byteLength + 16);
    const guest = new Uint8Array(sab, 4, diskData.byteLength);
    const range = computeAlignedDiskIoRange(0n, guest.byteLength, sectorSize);
    expect(range).toEqual({ lba: 0, byteLength: guest.byteLength, offset: 0 });

    await diskReadIntoGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    expect(calls).toEqual([
      { op: "readInto", lba: 0, byteLength: 1024, offsetBytes: 4 },
      { op: "readInto", lba: 2, byteLength: 1024, offsetBytes: 4 + 1024 },
      { op: "readInto", lba: 4, byteLength: 512, offsetBytes: 4 + 2048 },
    ]);
    expect(Array.from(guest)).toEqual(Array.from(diskData));
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

  it("does not use writeFrom() when guest memory is not SharedArrayBuffer", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);

    const calls: Array<{ op: "write"; lba: number; byteLength: number }> = [];
    const client: RuntimeDiskClientLike = {
      async read() {
        throw new Error("unexpected read()");
      },
      async readInto() {
        throw new Error("unexpected readInto()");
      },
      async write(_handle, lba, data) {
        calls.push({ op: "write", lba, byteLength: data.byteLength });
        diskData.set(data, lba * sectorSize);
      },
      async writeFrom() {
        throw new Error("unexpected writeFrom()");
      },
    };

    const guest = new Uint8Array(sectorSize * 5);
    for (let i = 0; i < guest.length; i++) guest[i] = (0x6f + i) & 0xff;

    const range = computeAlignedDiskIoRange(0n, guest.byteLength, sectorSize);
    expect(range).toEqual({ lba: 0, byteLength: guest.byteLength, offset: 0 });

    await diskWriteFromGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    // writeFrom() requires SharedArrayBuffer; fall back to write().
    expect(calls).toEqual([
      { op: "write", lba: 0, byteLength: 1024 },
      { op: "write", lba: 2, byteLength: 1024 },
      { op: "write", lba: 4, byteLength: 512 },
    ]);
    expect(Array.from(diskData.subarray(0, guest.byteLength))).toEqual(Array.from(guest));
  });

  it("chunks aligned writes via writeFrom() when guest memory is SharedArrayBuffer", async () => {
    const sectorSize = 512;
    const diskData = new Uint8Array(sectorSize * 10);

    const calls: Array<{ op: "writeFrom"; lba: number; byteLength: number; offsetBytes: number }> = [];
    const client: RuntimeDiskClientLike = {
      async read() {
        throw new Error("unexpected read()");
      },
      async write() {
        throw new Error("unexpected write()");
      },
      async writeFrom(_handle, lba, src) {
        calls.push({ op: "writeFrom", lba, byteLength: src.byteLength, offsetBytes: src.offsetBytes });
        diskData.set(new Uint8Array(src.sab, src.offsetBytes, src.byteLength), lba * sectorSize);
      },
    };

    const sab = new SharedArrayBuffer(sectorSize * 5 + 16);
    const guest = new Uint8Array(sab, 8, sectorSize * 5);
    for (let i = 0; i < guest.length; i++) guest[i] = (0x5a + i) & 0xff;

    const range = computeAlignedDiskIoRange(0n, guest.byteLength, sectorSize);
    expect(range).toEqual({ lba: 0, byteLength: guest.byteLength, offset: 0 });

    await diskWriteFromGuest({
      client,
      handle: 1,
      range: range!,
      sectorSize,
      guestView: guest,
      maxIoBytes: sectorSize * 2,
    });

    expect(calls).toEqual([
      { op: "writeFrom", lba: 0, byteLength: 1024, offsetBytes: 8 },
      { op: "writeFrom", lba: 2, byteLength: 1024, offsetBytes: 8 + 1024 },
      { op: "writeFrom", lba: 4, byteLength: 512, offsetBytes: 8 + 2048 },
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
