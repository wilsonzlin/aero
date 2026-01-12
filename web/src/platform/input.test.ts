import { describe, expect, it, vi } from "vitest";

import { attachKeyboard, attachPointerLock } from "./input";

type ListenerMap = Map<string, EventListenerOrEventListenerObject>;

function makeCanvasStub(listeners: ListenerMap): HTMLCanvasElement {
  return {
    tabIndex: -1,
    focus: vi.fn(),
    requestPointerLock: vi.fn(),
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      listeners.set(type, listener);
    },
    removeEventListener: () => {},
  } as unknown as HTMLCanvasElement;
}

function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const docListeners: ListenerMap = new Map();
  const doc = {
    pointerLockElement: null as unknown,
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      docListeners.set(type, listener);
    },
    removeEventListener: vi.fn(),
    __listeners: docListeners,
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

describe("platform input helpers", () => {
  it("attachKeyboard prevents default and stops propagation", () => {
    const canvasListeners: ListenerMap = new Map();
    const canvas = makeCanvasStub(canvasListeners);

    const onEvent = vi.fn();
    attachKeyboard(canvas, { onEvent });

    expect(canvas.tabIndex).toBe(0);

    const keydown = canvasListeners.get("keydown");
    const keyup = canvasListeners.get("keyup");
    expect(typeof keydown).toBe("function");
    expect(typeof keyup).toBe("function");

    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();

    (keydown as any)({
      code: "KeyA",
      repeat: false,
      altKey: false,
      ctrlKey: false,
      shiftKey: false,
      metaKey: false,
      preventDefault,
      stopPropagation,
    } satisfies Partial<KeyboardEvent>);

    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
    expect(onEvent).toHaveBeenCalledWith({
      type: "keyDown",
      code: "KeyA",
      repeat: false,
      altKey: false,
      ctrlKey: false,
      shiftKey: false,
      metaKey: false,
    });

    (keyup as any)({
      code: "KeyA",
      altKey: false,
      ctrlKey: false,
      shiftKey: false,
      metaKey: false,
      preventDefault,
      stopPropagation,
    } satisfies Partial<KeyboardEvent>);

    expect(preventDefault).toHaveBeenCalledTimes(2);
    expect(stopPropagation).toHaveBeenCalledTimes(2);
    expect(onEvent).toHaveBeenCalledWith({
      type: "keyUp",
      code: "KeyA",
      altKey: false,
      ctrlKey: false,
      shiftKey: false,
      metaKey: false,
    });
  });

  it("attachPointerLock prevents default and stops propagation for click + pointer-locked mouse events", () => {
    withStubbedDocument((doc) => {
      const canvasListeners: ListenerMap = new Map();
      const canvas = makeCanvasStub(canvasListeners);

      const onEvent = vi.fn();
      attachPointerLock(canvas, { onEvent });

      const click = canvasListeners.get("click");
      expect(typeof click).toBe("function");

      const preventDefault = vi.fn();
      const stopPropagation = vi.fn();
      (click as any)({ preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(1);
      expect(stopPropagation).toHaveBeenCalledTimes(1);
      expect((canvas.requestPointerLock as any) as ReturnType<typeof vi.fn>).toHaveBeenCalledTimes(1);
      expect((canvas.focus as any) as ReturnType<typeof vi.fn>).toHaveBeenCalledTimes(1);

      // Simulate pointer lock transition to locked.
      doc.pointerLockElement = canvas;
      const plc = doc.__listeners.get("pointerlockchange");
      expect(typeof plc).toBe("function");
      (plc as any)();

      expect(onEvent).toHaveBeenCalledWith({ type: "pointerLockChange", locked: true });

      const move = doc.__listeners.get("mousemove");
      const down = doc.__listeners.get("mousedown");
      const up = doc.__listeners.get("mouseup");
      const wheel = doc.__listeners.get("wheel");
      expect(typeof move).toBe("function");
      expect(typeof down).toBe("function");
      expect(typeof up).toBe("function");
      expect(typeof wheel).toBe("function");

      (move as any)({ movementX: 3, movementY: -2, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(2);
      expect(stopPropagation).toHaveBeenCalledTimes(2);
      expect(onEvent).toHaveBeenCalledWith({ type: "mouseMove", dx: 3, dy: -2 });

      (down as any)({ button: 1, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(3);
      expect(stopPropagation).toHaveBeenCalledTimes(3);
      expect(onEvent).toHaveBeenCalledWith({ type: "mouseButton", button: 1, down: true });

      (up as any)({ button: 1, preventDefault, stopPropagation } satisfies Partial<MouseEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(4);
      expect(stopPropagation).toHaveBeenCalledTimes(4);
      expect(onEvent).toHaveBeenCalledWith({ type: "mouseButton", button: 1, down: false });

      (wheel as any)({ deltaX: 1, deltaY: 2, deltaZ: 0, preventDefault, stopPropagation } satisfies Partial<WheelEvent>);
      expect(preventDefault).toHaveBeenCalledTimes(5);
      expect(stopPropagation).toHaveBeenCalledTimes(5);
      expect(onEvent).toHaveBeenCalledWith({ type: "mouseWheel", deltaX: 1, deltaY: 2, deltaZ: 0 });
    });
  });
});

