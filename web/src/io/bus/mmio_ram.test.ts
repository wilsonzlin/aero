import { describe, expect, it } from "vitest";

import { defaultReadValue } from "../ipc/io_protocol.ts";
import { MmioRamHandler } from "./mmio_ram.ts";

describe("io/bus/mmio_ram", () => {
  it("implements little-endian reads/writes for sizes 1/2/4", () => {
    const backing = new Uint8Array(16);
    const h = new MmioRamHandler(backing);

    h.mmioWrite(0n, 1, 0xaa);
    expect(backing[0]).toBe(0xaa);
    expect(h.mmioRead(0n, 1)).toBe(0xaa);

    h.mmioWrite(1n, 2, 0xbeef);
    expect(backing[1]).toBe(0xef);
    expect(backing[2]).toBe(0xbe);
    expect(h.mmioRead(1n, 2)).toBe(0xbeef);

    h.mmioWrite(4n, 4, 0x1122_3344);
    expect(backing[4]).toBe(0x44);
    expect(backing[5]).toBe(0x33);
    expect(backing[6]).toBe(0x22);
    expect(backing[7]).toBe(0x11);
    expect(h.mmioRead(4n, 4)).toBe(0x1122_3344);
  });

  it("returns defaultReadValue on out-of-bounds reads and ignores out-of-bounds writes", () => {
    const backing = new Uint8Array(16);
    backing[15] = 0x7a;
    const h = new MmioRamHandler(backing);

    // Crosses the end of the buffer => OOB.
    expect(h.mmioRead(15n, 2)).toBe(defaultReadValue(2));
    // Completely out of bounds.
    expect(h.mmioRead(16n, 1)).toBe(defaultReadValue(1));

    // Ensure OOB writes do not mutate bytes.
    h.mmioWrite(15n, 2, 0xbeef);
    expect(backing[15]).toBe(0x7a);
  });
});

