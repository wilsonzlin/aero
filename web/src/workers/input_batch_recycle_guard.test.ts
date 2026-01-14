import { describe, expect, it } from "vitest";

import { MAX_INPUT_BATCH_RECYCLE_BYTES, shouldRecycleInputBatchByteLength } from "./input_batch_recycle_guard";

describe("workers/input_batch_recycle_guard", () => {
  it("allows recycling buffers up to the configured byte limit", () => {
    expect(shouldRecycleInputBatchByteLength(0)).toBe(true);
    expect(shouldRecycleInputBatchByteLength(1)).toBe(true);
    expect(shouldRecycleInputBatchByteLength(MAX_INPUT_BATCH_RECYCLE_BYTES)).toBe(true);
  });

  it("rejects recycling buffers larger than the byte limit", () => {
    expect(shouldRecycleInputBatchByteLength(MAX_INPUT_BATCH_RECYCLE_BYTES + 1)).toBe(false);
    expect(shouldRecycleInputBatchByteLength(MAX_INPUT_BATCH_RECYCLE_BYTES + 1024)).toBe(false);
  });

  it("honors custom limits", () => {
    expect(shouldRecycleInputBatchByteLength(9, 8)).toBe(false);
    expect(shouldRecycleInputBatchByteLength(8, 8)).toBe(true);
  });
});

