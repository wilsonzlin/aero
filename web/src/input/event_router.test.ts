import { describe, expect, it, vi } from "vitest";

import { InputEventRouter } from "./event_router";
import { InMemoryInputQueue } from "./queue";

type ListenerMap = Map<string, EventListenerOrEventListenerObject>;

function makeTarget(listeners: ListenerMap): EventTarget {
  return {
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      listeners.set(type, listener);
    },
    removeEventListener: () => {},
  } as unknown as EventTarget;
}

describe("InputEventRouter", () => {
  it("prevents default + stops propagation for captured events", () => {
    const listeners: ListenerMap = new Map();
    const target = makeTarget(listeners);
    const queue = new InMemoryInputQueue();

    const router = new InputEventRouter({ target, queue });
    router.start();

    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();

    const keydown = listeners.get("keydown");
    if (typeof keydown !== "function") throw new Error("missing keydown listener");
    keydown({
      code: "KeyA",
      key: "a",
      location: 0,
      repeat: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      preventDefault,
      stopPropagation,
    } as unknown as KeyboardEvent);

    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);

    const pointerdown = listeners.get("pointerdown");
    if (typeof pointerdown !== "function") throw new Error("missing pointerdown listener");
    pointerdown({
      pointerId: 1,
      pointerType: "mouse",
      button: 0,
      buttons: 1,
      clientX: 0,
      clientY: 0,
      movementX: 0,
      movementY: 0,
      pressure: 0,
      tiltX: 0,
      tiltY: 0,
      preventDefault,
      stopPropagation,
    } as unknown as PointerEvent);

    expect(preventDefault).toHaveBeenCalledTimes(2);
    expect(stopPropagation).toHaveBeenCalledTimes(2);

    const wheel = listeners.get("wheel");
    if (typeof wheel !== "function") throw new Error("missing wheel listener");
    wheel({
      deltaX: 0,
      deltaY: 1,
      deltaZ: 0,
      deltaMode: 0,
      preventDefault,
      stopPropagation,
    } as unknown as WheelEvent);

    expect(preventDefault).toHaveBeenCalledTimes(3);
    expect(stopPropagation).toHaveBeenCalledTimes(3);

    const contextmenu = listeners.get("contextmenu");
    if (typeof contextmenu !== "function") throw new Error("missing contextmenu listener");
    contextmenu({
      preventDefault,
      stopPropagation,
    } as unknown as Event);

    expect(preventDefault).toHaveBeenCalledTimes(4);
    expect(stopPropagation).toHaveBeenCalledTimes(4);
  });
});
