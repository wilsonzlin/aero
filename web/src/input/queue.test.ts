import { describe, expect, it } from "vitest";

import { InMemoryInputQueue } from "./queue";

describe("InMemoryInputQueue", () => {
  it("sanitizes NaN capacity to avoid unbounded growth", () => {
    const queue = new InMemoryInputQueue({ capacity: Number.NaN });
    expect(Number.isFinite(queue.capacity)).toBe(true);

    for (let i = 0; i < queue.capacity; i += 1) {
      expect(queue.push({ id: i + 1, kind: "unknown", t_capture_ms: 0 })).toBe(true);
    }
    expect(queue.push({ id: queue.capacity + 1, kind: "unknown", t_capture_ms: 0 })).toBe(false);
  });
});

