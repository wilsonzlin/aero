import { describe, expect, it } from "vitest";

import { InputEventType } from "../input/event_queue";
import { validateInputBatchBuffer } from "./io_input_batch";

describe("workers/io_input_batch.validateInputBatchBuffer", () => {
  it("rejects an undersized buffer", () => {
    const res = validateInputBatchBuffer(new ArrayBuffer(4));
    expect(res).toEqual({ ok: false, error: "buffer_too_small" });
  });

  it("rejects count too large for the buffer", () => {
    // Header (2 words) + 1 event (4 words) = 6 words.
    const buf = new ArrayBuffer(6 * 4);
    const words = new Int32Array(buf);
    words[0] = 2; // count claims 2 events.
    // Event 0 is present; the second event would be out-of-bounds.
    words[2] = InputEventType.MouseMove;
    const res = validateInputBatchBuffer(buf);
    expect(res).toEqual({ ok: false, error: "count_out_of_bounds" });
  });

  it("rejects an unknown event type", () => {
    const buf = new ArrayBuffer(6 * 4);
    const words = new Int32Array(buf);
    words[0] = 1; // count
    words[2] = 0x7fff_ffff; // unknown type
    const res = validateInputBatchBuffer(buf);
    expect(res).toEqual({ ok: false, error: "unknown_event_type" });
  });

  it("rejects a scancode event with len > 4", () => {
    const buf = new ArrayBuffer(6 * 4);
    const words = new Int32Array(buf);
    words[0] = 1;
    words[2] = InputEventType.KeyScancode;
    words[4] = 0x11223344; // packed bytes
    words[5] = 5; // invalid len
    const res = validateInputBatchBuffer(buf);
    expect(res).toEqual({ ok: false, error: "invalid_scancode_len" });
  });
});

