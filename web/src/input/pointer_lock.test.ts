import { describe, expect, it, vi } from "vitest";

import { PointerLock } from "./pointer_lock";
import { withStubbedDocument } from "./test_utils";

type ListenerMap = Map<string, EventListenerOrEventListenerObject>;

function withListenerTrackingDocument<T>(run: (doc: any) => T): T {
  const listeners: ListenerMap = new Map();
  return withStubbedDocument((doc) => {
    doc.addEventListener = (type: string, listener: EventListenerOrEventListenerObject) => {
      listeners.set(type, listener);
    };
    doc.removeEventListener = vi.fn((type: string, listener: EventListenerOrEventListenerObject) => {
      const existing = listeners.get(type);
      if (existing === listener) listeners.delete(type);
    });
    doc.exitPointerLock = vi.fn();
    doc.__listeners = listeners;
    return run(doc);
  });
}

describe("PointerLock", () => {
  it("initial isLocked reflects document.pointerLockElement", () => {
    withListenerTrackingDocument((doc) => {
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
    withListenerTrackingDocument((doc) => {
      const element = { requestPointerLock: vi.fn() } as unknown as HTMLElement;
      doc.pointerLockElement = null;

      const onChange = vi.fn();
      const pl = new PointerLock(element, { onChange });

      const plc = doc.__listeners.get("pointerlockchange");
      expect(typeof plc).toBe("function");
      const plcFn = plc as unknown as () => void;

      // No-op if no transition.
      plcFn();
      expect(onChange).toHaveBeenCalledTimes(0);
      expect(pl.isLocked).toBe(false);

      // Transition to locked.
      doc.pointerLockElement = element;
      plcFn();
      expect(onChange).toHaveBeenCalledTimes(1);
      expect(onChange).toHaveBeenLastCalledWith(true);
      expect(pl.isLocked).toBe(true);

      // No-op if still locked.
      plcFn();
      expect(onChange).toHaveBeenCalledTimes(1);

      // Transition to unlocked.
      doc.pointerLockElement = null;
      plcFn();
      expect(onChange).toHaveBeenCalledTimes(2);
      expect(onChange).toHaveBeenLastCalledWith(false);
      expect(pl.isLocked).toBe(false);

      // No-op if still unlocked.
      plcFn();
      expect(onChange).toHaveBeenCalledTimes(2);

      pl.dispose();
    });
  });

  it("request() no-ops if already locked", () => {
    withListenerTrackingDocument((doc) => {
      const requestPointerLock = vi.fn();
      const element = { requestPointerLock } as unknown as HTMLElement;
      doc.pointerLockElement = element;

      const pl = new PointerLock(element);
      pl.request();
      expect(requestPointerLock).toHaveBeenCalledTimes(0);
      pl.dispose();
    });
  });

  it("request() no-ops if requestPointerLock is missing", () => {
    withListenerTrackingDocument((doc) => {
      doc.pointerLockElement = null;
      const element = {} as unknown as HTMLElement;
      const pl = new PointerLock(element);
      expect(pl.isSupported).toBe(false);
      expect(() => pl.request()).not.toThrow();
      pl.dispose();
    });
  });

  it("exit() no-ops if not locked; calls document.exitPointerLock if locked", () => {
    withListenerTrackingDocument((doc) => {
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
    withListenerTrackingDocument((doc) => {
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
