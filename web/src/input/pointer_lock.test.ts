import { describe, expect, it, vi } from "vitest";

import { PointerLock } from "./pointer_lock";

type ListenerMap = Map<string, EventListenerOrEventListenerObject>;

function withStubbedDocument<T>(run: (doc: any) => T): T {
  const original = (globalThis as any).document;
  const listeners: ListenerMap = new Map();
  const doc = {
    pointerLockElement: null as unknown,
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      listeners.set(type, listener);
    },
    removeEventListener: vi.fn((type: string, listener: EventListenerOrEventListenerObject) => {
      const existing = listeners.get(type);
      if (existing === listener) listeners.delete(type);
    }),
    exitPointerLock: vi.fn(),
    __listeners: listeners,
  };
  (globalThis as any).document = doc;
  try {
    return run(doc);
  } finally {
    (globalThis as any).document = original;
  }
}

describe("PointerLock", () => {
  it("initial isLocked reflects document.pointerLockElement", () => {
    withStubbedDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;

      doc.pointerLockElement = element;
      const locked = new PointerLock(element);
      expect(locked.isLocked).toBe(true);
      locked.dispose();

      doc.pointerLockElement = null;
      const unlocked = new PointerLock(element);
      expect(unlocked.isLocked).toBe(false);
      unlocked.dispose();
    });
  });

  it("invokes onChange exactly once per lock/unlock transition", () => {
    withStubbedDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;
      doc.pointerLockElement = null;

      const onChange = vi.fn();
      const pl = new PointerLock(element, { onChange });

      const plc = doc.__listeners.get("pointerlockchange");
      expect(typeof plc).toBe("function");

      // No-op if no transition.
      (plc as any)();
      expect(onChange).toHaveBeenCalledTimes(0);
      expect(pl.isLocked).toBe(false);

      // Transition to locked.
      doc.pointerLockElement = element;
      (plc as any)();
      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenLastCalledWith(true);
      expect(pl.isLocked).toBe(true);

      // No-op if still locked.
      (plc as any)();
      expect(onChange).toHaveBeenCalledTimes(1);

      // Transition to unlocked.
      doc.pointerLockElement = null;
      (plc as any)();
      expect(onChange).toHaveBeenCalledTimes(2);
      expect(onChange).toHaveBeenLastCalledWith(false);
      expect(pl.isLocked).toBe(false);

      // No-op if still unlocked.
      (plc as any)();
      expect(onChange).toHaveBeenCalledTimes(2);

      pl.dispose();
    });
  });

  it("request() no-ops if already locked", () => {
    withStubbedDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;
      doc.pointerLockElement = element;

      const pl = new PointerLock(element);
      pl.request();
      expect((element.requestPointerLock as any) as ReturnType<typeof vi.fn>).toHaveBeenCalledTimes(0);
      pl.dispose();
    });
  });

  it("request() no-ops if requestPointerLock is missing", () => {
    withStubbedDocument((doc) => {
      doc.pointerLockElement = null;
      const element = {} as unknown as HTMLElement;
      const pl = new PointerLock(element);
      expect(pl.isSupported).toBe(false);
      expect(() => pl.request()).not.toThrow();
      pl.dispose();
    });
  });

  it("exit() no-ops if not locked; calls document.exitPointerLock if locked", () => {
    withStubbedDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;

      doc.pointerLockElement = null;
      const unlocked = new PointerLock(element);
      unlocked.exit();
      expect(doc.exitPointerLock).toHaveBeenCalledTimes(0);
      unlocked.dispose();

      doc.pointerLockElement = element;
      const locked = new PointerLock(element);
      locked.exit();
      expect(doc.exitPointerLock).toHaveBeenCalledTimes(1);
      locked.dispose();
    });
  });

  it("dispose() unregisters listeners", () => {
    withStubbedDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;
      const pl = new PointerLock(element);

      const plc = doc.__listeners.get("pointerlockchange");
      const ple = doc.__listeners.get("pointerlockerror");
      expect(typeof plc).toBe("function");
      expect(typeof ple).toBe("function");

      pl.dispose();

      expect(doc.removeEventListener).toHaveBeenCalledTimes(2);
      expect(doc.removeEventListener).toHaveBeenNthCalledWith(1, "pointerlockchange", plc);
      expect(doc.removeEventListener).toHaveBeenNthCalledWith(2, "pointerlockerror", ple);

      expect(doc.__listeners.has("pointerlockchange")).toBe(false);
      expect(doc.__listeners.has("pointerlockerror")).toBe(false);
    });
  });
});

