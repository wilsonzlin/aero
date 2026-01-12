import { describe, expect, it } from "vitest";

import { VmTimebase } from "./vm_timebase";

describe("VmTimebase", () => {
  it("tracks host time while running", () => {
    const tb = new VmTimebase();

    expect(tb.tick(100, false)).toBe(100);
    expect(tb.tick(150, false)).toBe(150);
    expect(tb.tick(151, false)).toBe(151);
  });

  it("does not advance while paused (even if host time advances)", () => {
    const tb = new VmTimebase();

    expect(tb.tick(100, false)).toBe(100);
    expect(tb.tick(200, false)).toBe(200);

    expect(tb.tick(1_000, true)).toBe(200);
    expect(tb.tick(2_000, true)).toBe(200);

    // Once unpaused, VM time advances only by the delta since the last observed host tick.
    expect(tb.tick(2_010, false)).toBe(210);
  });

  it("resetHostNowMs prevents wall-clock fast-forward when ticks were stalled during pause", () => {
    const tb = new VmTimebase();

    expect(tb.tick(100, false)).toBe(100);
    expect(tb.tick(200, false)).toBe(200);

    // Simulate a pause where no intermediate ticks were observed (e.g. snapshot streaming).
    // Without `resetHostNowMs`, the next tick would advance VM time by ~1000ms.
    tb.resetHostNowMs(1_200);
    expect(tb.tick(1_210, false)).toBe(210);
  });
});

