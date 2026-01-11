import { describe, expect, it } from "vitest";

import { InputEventQueue, InputEventType, type InputBatchMessage, type InputBatchTarget } from "./event_queue";

describe("InputEventQueue", () => {
  it("enqueues GamepadReport without changing the existing wire format", () => {
    const queue = new InputEventQueue(8);
    queue.pushKeyScancode(10, 0xaa, 1);
    queue.pushGamepadReport(20, 0x11223344, 0x55667788);

    const state: { posted: InputBatchMessage | null } = { posted: null };
    const target: InputBatchTarget = {
      postMessage: (msg, _transfer) => {
        state.posted = msg;
      },
    };

    queue.flush(target);
    if (!state.posted) throw new Error("expected flush to post a batch");

    const words = new Int32Array(state.posted.buffer);
    expect(words[0]).toBe(2); // count

    // Event 0: key scancode
    expect(words[2]).toBe(InputEventType.KeyScancode);
    expect(words[3]).toBe(10);
    expect(words[4] >>> 0).toBe(0xaa);
    expect(words[5]).toBe(1);

    // Event 1: gamepad report
    expect(words[6]).toBe(InputEventType.GamepadReport);
    expect(words[7]).toBe(20);
    expect(words[8] >>> 0).toBe(0x11223344);
    expect(words[9] >>> 0).toBe(0x55667788);
  });
});
