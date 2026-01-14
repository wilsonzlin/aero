import { describe, expect, it } from "vitest";

import { InputEventQueue, InputEventType, type InputBatchMessage, type InputBatchTarget } from "./event_queue";

describe("InputEventQueue.flush() postMessage errors", () => {
  it("drops the batch, resets the queue, and remains usable", () => {
    const queue = new InputEventQueue(8);
    queue.pushKeyScancode(10, 0xaa, 1);

    const throwingTarget: InputBatchTarget = {
      postMessage: (msg) => {
        // Simulate the worst-case: the buffer has already been transferred/detached
        // when the failure occurs.
        structuredClone(msg.buffer, { transfer: [msg.buffer] });
        throw new Error("postMessage failed");
      },
    };

    let droppedLatencyUs: number | null = null;
    expect(() => {
      droppedLatencyUs = queue.flush(throwingTarget);
    }).not.toThrow();
    expect(droppedLatencyUs).toBeNull();
    expect(queue.size).toBe(0);

    const state: { posted: InputBatchMessage | null } = { posted: null };
    const okTarget: InputBatchTarget = {
      postMessage: (msg) => {
        state.posted = msg;
      },
    };

    queue.pushMouseButtons(20, 3);
    const latencyUs = queue.flush(okTarget);
    expect(latencyUs).not.toBeNull();
    expect(queue.size).toBe(0);

    expect(state.posted).not.toBeNull();
    const posted = state.posted!;
    expect(posted.type).toBe("in:input-batch");
    expect(posted.buffer.byteLength).toBeGreaterThan(0);

    const words = new Int32Array(posted.buffer);
    expect(words[0]).toBe(1); // count
    expect(words[2]).toBe(InputEventType.MouseButtons);
    expect(words[3]).toBe(20);
    expect(words[4]).toBe(3);
    expect(words[5]).toBe(0);
  });
});
