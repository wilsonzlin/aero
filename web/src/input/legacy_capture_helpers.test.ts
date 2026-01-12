import { describe, expect, it, vi } from "vitest";

import { KeyboardCapture } from "./keyboard";
import { MouseCapture } from "./mouse";

type ListenerMap = Map<string, EventListenerOrEventListenerObject>;

function makeCanvasStub(listeners: ListenerMap): HTMLCanvasElement {
  let hasTabindex = false;
  const canvas = {
    tabIndex: -1,
    hasAttribute: (name: string) => (name === "tabindex" ? hasTabindex : false),
    focus: () => {},
    requestPointerLock: () => {},
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      listeners.set(type, listener);
      if (type === "keydown" || type === "keyup") {
        // Once we set tabIndex, treat the element as having the attribute.
        hasTabindex = true;
      }
    },
  } as unknown as HTMLCanvasElement;
  return canvas;
}

function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const docListeners: ListenerMap = new Map();
  const doc = {
    pointerLockElement: null as unknown,
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      docListeners.set(type, listener);
    },
    removeEventListener: () => {},
    __listeners: docListeners,
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

describe("legacy input capture helpers", () => {
  it("KeyboardCapture stops propagation when it prevents default", () => {
    const canvasListeners: ListenerMap = new Map();
    const canvas = makeCanvasStub(canvasListeners);

    const sink = vi.fn();
    const capture = new KeyboardCapture(canvas, sink);
    capture.attach();

    const keydown = canvasListeners.get("keydown");
    const keyup = canvasListeners.get("keyup");
    expect(typeof keydown).toBe("function");
    expect(typeof keyup).toBe("function");

    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();

    (keydown as any)({ code: "KeyA", repeat: false, preventDefault, stopPropagation } satisfies Partial<KeyboardEvent>);
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
    expect(sink).toHaveBeenCalledTimes(1);

    (keyup as any)({ code: "KeyA", preventDefault, stopPropagation } satisfies Partial<KeyboardEvent>);
    expect(preventDefault).toHaveBeenCalledTimes(2);
    expect(stopPropagation).toHaveBeenCalledTimes(2);
    expect(sink).toHaveBeenCalledTimes(2);
  });

  it("MouseCapture stops propagation for pointer-locked mouse events", () => {
    withStubbedDocument((doc) => {
      const canvasListeners: ListenerMap = new Map();
      const canvas = makeCanvasStub(canvasListeners);

      const onMove = vi.fn();
      const onButton = vi.fn();
      const capture = new MouseCapture(canvas, onMove, onButton);
      capture.attach();

      // Simulate pointer lock.
      doc.pointerLockElement = canvas;
      const plc = doc.__listeners.get("pointerlockchange");
      expect(typeof plc).toBe("function");
      (plc as any)();

      const move = canvasListeners.get("mousemove");
      const down = canvasListeners.get("mousedown");
      const up = canvasListeners.get("mouseup");
      const wheel = canvasListeners.get("wheel");
      expect(typeof move).toBe("function");
      expect(typeof down).toBe("function");
      expect(typeof up).toBe("function");
      expect(typeof wheel).toBe("function");

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();

      (move as any)({ movementX: 3, movementY: -2, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
      expect(onMove).toHaveBeenCalledWith(3, -2, 0);

      (down as any)({ button: 0, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(2);
      expect(stopPropagation).toHaveBeenCalledTimes(2);
      expect(onButton).toHaveBeenCalledWith(0, true);

      (up as any)({ button: 0, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(3);
      expect(stopPropagation).toHaveBeenCalledTimes(3);
      expect(onButton).toHaveBeenCalledWith(0, false);

      (wheel as any)({ deltaY: 5, preventDefault, stopPropagation } satisfies Partial<WheelEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(4);
      expect(stopPropagation).toHaveBeenCalledTimes(4);
      expect(onMove).toHaveBeenCalledWith(0, 0, 5);
    });
  });
});

