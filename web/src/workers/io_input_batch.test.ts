import { describe, expect, it } from "vitest";

import { INPUT_BATCH_HEADER_BYTES, MAX_INPUT_EVENTS_PER_BATCH, validateInputBatchBuffer } from "./io_input_batch";

describe("workers/io_input_batch.validateInputBatchBuffer", () => {
  it("rejects buffers that are too small", () => {
    const res = validateInputBatchBuffer(new ArrayBuffer(4));
    expect(res).toEqual({ ok: false, error: "buffer_too_small" });
  });

  it("rejects buffers whose byteLength is not divisible by 4", () => {
    const res = validateInputBatchBuffer(new ArrayBuffer(INPUT_BATCH_HEADER_BYTES + 2));
    expect(res).toEqual({ ok: false, error: "buffer_unaligned" });
  });

  it("clamps count to the number of events representable by the buffer", () => {
    // Header (2 words) + 2 events * 4 words/event = 10 words.
    const buf = new ArrayBuffer((2 + 2 * 4) * 4);
    const words = new Int32Array(buf);
    words[0] = 1_000_000;

    const decoded = validateInputBatchBuffer(buf);
    expect(decoded.ok).toBe(true);
    expect(decoded.ok && decoded.maxCount).toBe(2);
    expect(decoded.ok && decoded.count).toBe(2);
  });

  it("clamps extremely large claimed counts without hanging", () => {
    // Header (2 words) + 1 event * 4 words/event.
    const buf = new ArrayBuffer((2 + 1 * 4) * 4);
    const words = new Int32Array(buf);
    words[0] = 0x7fffffff;

    const decoded = validateInputBatchBuffer(buf);
    expect(decoded.ok).toBe(true);
    expect(decoded.ok && decoded.count).toBe(1);
  });

  it("enforces MAX_INPUT_EVENTS_PER_BATCH even when the buffer is larger", () => {
    const capacity = MAX_INPUT_EVENTS_PER_BATCH + 10;
    const buf = new ArrayBuffer((2 + capacity * 4) * 4);
    const words = new Int32Array(buf);
    words[0] = capacity;

    const decoded = validateInputBatchBuffer(buf);
    expect(decoded.ok).toBe(true);
    expect(decoded.ok && decoded.maxCount).toBe(capacity);
    expect(decoded.ok && decoded.count).toBe(MAX_INPUT_EVENTS_PER_BATCH);
  });
});

