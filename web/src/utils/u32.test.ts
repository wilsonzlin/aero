import { describe, expect, it } from "vitest";

import { u32Delta } from "./u32";

describe("u32Delta", () => {
  it("subtracts without wraparound", () => {
    expect(u32Delta(1_000, 900)).toBe(100);
  });

  it("subtracts across a u32 wrap boundary", () => {
    // Simulate a timestamp close to u32::MAX wrapping back to a small value.
    //
    // then = 0xffff_ff00
    // now  = 0x0000_0100
    // delta should be 0x0000_0200 (512)
    expect(u32Delta(0x0000_0100, 0xffff_ff00)).toBe(0x0000_0200);
  });
});

