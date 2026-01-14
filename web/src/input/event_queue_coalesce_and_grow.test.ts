import { describe, expect, it } from "vitest";

import { InputEventQueue, InputEventType, type InputBatchMessage, type InputBatchTarget } from "./event_queue";

const flushWords = (queue: InputEventQueue): Int32Array => {
  const state: { posted: InputBatchMessage | null } = { posted: null };
  const target: InputBatchTarget = {
    postMessage: (msg, _transfer) => {
      state.posted = msg;
    },
  };
  queue.flush(target);
  if (!state.posted) throw new Error("expected flush to post a batch");
  return new Int32Array(state.posted.buffer);
};

describe("InputEventQueue coalescing", () => {
  it("coalesces consecutive MouseMove events by summing dx/dy", () => {
    const queue = new InputEventQueue(8);
    queue.pushMouseMove(10, 1, 2);
    queue.pushMouseMove(20, 3, -1);
    expect(queue.size).toBe(1);

    const words = flushWords(queue);
    expect(words[0]).toBe(1);

    const ev0 = 2;
    expect(words[ev0]).toBe(InputEventType.MouseMove);
    expect(words[ev0 + 1]).toBe(20);
    expect(words[ev0 + 2]).toBe(4);
    expect(words[ev0 + 3]).toBe(1);
  });

  it("coalesces consecutive MouseWheel events by summing dz", () => {
    const queue = new InputEventQueue(8);
    queue.pushMouseWheel(10, 1);
    queue.pushMouseWheel(20, 2);
    expect(queue.size).toBe(1);

    const words = flushWords(queue);
    expect(words[0]).toBe(1);

    const ev0 = 2;
    expect(words[ev0]).toBe(InputEventType.MouseWheel);
    expect(words[ev0 + 1]).toBe(20);
    expect(words[ev0 + 2]).toBe(3);
    expect(words[ev0 + 3]).toBe(0);
  });

  it("does not coalesce across an intervening event type", () => {
    const queue = new InputEventQueue(8);
    queue.pushMouseMove(1, 1, 2);
    queue.pushMouseWheel(2, 3);
    queue.pushMouseMove(3, 4, 5);
    queue.pushMouseWheel(4, 6);
    expect(queue.size).toBe(4);

    const words = flushWords(queue);
    expect(words[0]).toBe(4);

    const base = 2;
    const ev0 = base + 0 * 4;
    expect(words[ev0]).toBe(InputEventType.MouseMove);
    expect(words[ev0 + 1]).toBe(1);
    expect(words[ev0 + 2]).toBe(1);
    expect(words[ev0 + 3]).toBe(2);

    const ev1 = base + 1 * 4;
    expect(words[ev1]).toBe(InputEventType.MouseWheel);
    expect(words[ev1 + 1]).toBe(2);
    expect(words[ev1 + 2]).toBe(3);
    expect(words[ev1 + 3]).toBe(0);

    const ev2 = base + 2 * 4;
    expect(words[ev2]).toBe(InputEventType.MouseMove);
    expect(words[ev2 + 1]).toBe(3);
    expect(words[ev2 + 2]).toBe(4);
    expect(words[ev2 + 3]).toBe(5);

    const ev3 = base + 3 * 4;
    expect(words[ev3]).toBe(InputEventType.MouseWheel);
    expect(words[ev3 + 1]).toBe(4);
    expect(words[ev3 + 2]).toBe(6);
    expect(words[ev3 + 3]).toBe(0);
  });
});

describe("InputEventQueue growth", () => {
  it("grows the backing buffer without losing queued events", () => {
    const queue = new InputEventQueue(2);

    queue.pushMouseButtons(10, 5);
    queue.pushMouseMove(20, 1, 2);
    queue.pushMouseMove(30, 3, 4); // coalesce with previous MouseMove
    queue.pushKeyHidUsage(40, 0x04, true); // forces a grow (capacity 2 -> 4)
    queue.pushMouseWheel(50, 1);
    queue.pushMouseWheel(60, 2); // coalesce with previous MouseWheel
    queue.pushGamepadReport(70, 0x11223344, 0x55667788); // forces another grow (capacity 4 -> 8)

    expect(queue.size).toBe(5);

    const words = flushWords(queue);
    expect(words[0]).toBe(5);

    const base = 2;
    const ev0 = base + 0 * 4;
    expect(words[ev0]).toBe(InputEventType.MouseButtons);
    expect(words[ev0 + 1]).toBe(10);
    expect(words[ev0 + 2]).toBe(5);
    expect(words[ev0 + 3]).toBe(0);

    const ev1 = base + 1 * 4;
    expect(words[ev1]).toBe(InputEventType.MouseMove);
    expect(words[ev1 + 1]).toBe(30);
    expect(words[ev1 + 2]).toBe(4);
    expect(words[ev1 + 3]).toBe(6);

    const ev2 = base + 2 * 4;
    expect(words[ev2]).toBe(InputEventType.KeyHidUsage);
    expect(words[ev2 + 1]).toBe(40);
    expect(words[ev2 + 2]).toBe(0x104);
    expect(words[ev2 + 3]).toBe(0);

    const ev3 = base + 3 * 4;
    expect(words[ev3]).toBe(InputEventType.MouseWheel);
    expect(words[ev3 + 1]).toBe(60);
    expect(words[ev3 + 2]).toBe(3);
    expect(words[ev3 + 3]).toBe(0);

    const ev4 = base + 4 * 4;
    expect(words[ev4]).toBe(InputEventType.GamepadReport);
    expect(words[ev4 + 1]).toBe(70);
    expect(words[ev4 + 2] >>> 0).toBe(0x11223344);
    expect(words[ev4 + 3] >>> 0).toBe(0x55667788);
  });
});

