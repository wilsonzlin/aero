import { describe, expect, it } from "vitest";

import {
  IRQ_REFCOUNT_ASSERT,
  IRQ_REFCOUNT_DEASSERT,
  IRQ_REFCOUNT_MAX,
  IRQ_REFCOUNT_SATURATED,
  IRQ_REFCOUNT_UNDERFLOW,
  applyIrqRefCountChange,
} from "./irq_refcount.ts";

describe("io/irq_refcount", () => {
  it("tracks refcounted level semantics and exposes only 0->1 / 1->0 transitions", () => {
    const counts = new Uint16Array(256);
    const irq = 1;

    // First raise asserts the line (0 -> 1).
    expect(applyIrqRefCountChange(counts, irq, true)).toBe(IRQ_REFCOUNT_ASSERT);
    expect(counts[irq]).toBe(1);

    // Additional raises are legal, but do not create another assertion transition.
    expect(applyIrqRefCountChange(counts, irq, true)).toBe(0);
    expect(counts[irq]).toBe(2);

    // Lowers decrement; only the final 1 -> 0 transition deasserts the line.
    expect(applyIrqRefCountChange(counts, irq, false)).toBe(0);
    expect(counts[irq]).toBe(1);
    expect(applyIrqRefCountChange(counts, irq, false)).toBe(IRQ_REFCOUNT_DEASSERT);
    expect(counts[irq]).toBe(0);
  });

  it("treats edge-style pulses as no-ops when the line is already asserted (wire-OR)", () => {
    const counts = new Uint16Array(256);
    const irq = 12;

    // Some other device is already asserting the line.
    expect(applyIrqRefCountChange(counts, irq, true)).toBe(IRQ_REFCOUNT_ASSERT);
    expect(counts[irq]).toBe(1);

    // An edge-triggered source "pulses" by doing raise then lower. Because the line is already
    // high, this should not create any observable transitions.
    expect(applyIrqRefCountChange(counts, irq, true)).toBe(0);
    expect(applyIrqRefCountChange(counts, irq, false)).toBe(0);
    expect(counts[irq]).toBe(1);

    // When the original device deasserts, the line finally drops.
    expect(applyIrqRefCountChange(counts, irq, false)).toBe(IRQ_REFCOUNT_DEASSERT);
    expect(counts[irq]).toBe(0);
  });

  it("guards against refcount underflow and overflow (saturating)", () => {
    const counts = new Uint16Array(256);
    const irq = 7;

    // Underflow: lowering a line that's already deasserted is ignored.
    expect(applyIrqRefCountChange(counts, irq, false)).toBe(IRQ_REFCOUNT_UNDERFLOW);
    expect(counts[irq]).toBe(0);

    // Saturation: raising past 0xffff is clamped (no Uint16Array wraparound).
    counts[irq] = IRQ_REFCOUNT_MAX;
    expect(applyIrqRefCountChange(counts, irq, true)).toBe(IRQ_REFCOUNT_SATURATED);
    expect(counts[irq]).toBe(IRQ_REFCOUNT_MAX);
  });
});
