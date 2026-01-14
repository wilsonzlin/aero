import { describe, expect, it, vi } from "vitest";

import { InputCapture } from "./input_capture";
import { makeCanvasStub, withStubbedDocument } from "./test_utils";

async function withFakeNavigatorKeyboard<T>(keyboard: unknown, run: () => Promise<T> | T): Promise<T> {
  const nav = globalThis.navigator as unknown as Record<string, unknown>;
  const originalDescriptor = Object.getOwnPropertyDescriptor(nav, "keyboard");
  Object.defineProperty(nav, "keyboard", { value: keyboard, configurable: true });
  try {
    return await run();
  } finally {
    if (originalDescriptor) {
      Object.defineProperty(nav, "keyboard", originalDescriptor);
    } else {
      delete (nav as { keyboard?: unknown }).keyboard;
    }
  }
}

describe("InputCapture Keyboard Lock integration", () => {
  it("attempts navigator.keyboard.lock() only when enabled and available", async () => {
    await withStubbedDocument(async () => {
      const lock = vi.fn<[readonly string[]], Promise<void>>().mockResolvedValue(undefined);
      const unlock = vi.fn();

      await withFakeNavigatorKeyboard({ lock, unlock }, async () => {
        const canvas = makeCanvasStub({ requestPointerLock: () => {} });

        const ioWorker = { postMessage: () => {} };
        const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true });

        (capture as any).hasFocus = true;
        (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);

        (capture as any).pointerLock.locked = true;
        (capture as any).handlePointerLockChange(true);

        expect(lock).toHaveBeenCalledTimes(1);
        // Prefer an explicit key list so we can reliably capture keys like Escape + function keys.
        expect(lock.mock.calls[0]?.[0]).toContain("Escape");

        // Disable the feature: lock should not be attempted.
        const captureDisabled = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: false });
        (captureDisabled as any).hasFocus = true;
        (captureDisabled as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);
        (captureDisabled as any).pointerLock.locked = true;
        (captureDisabled as any).handlePointerLockChange(true);
        expect(lock).toHaveBeenCalledTimes(1);
      });
    });
  });

  it("does not attempt navigator.keyboard.lock() when the API is unavailable", async () => {
    await withStubbedDocument(() => {
      const canvas = makeCanvasStub({ requestPointerLock: () => {} });

      const ioWorker = { postMessage: () => {} };
      const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true });

      (capture as any).hasFocus = true;
      (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);

      (capture as any).pointerLock.locked = true;
      // No navigator.keyboard => should be a no-op.
      (capture as any).handlePointerLockChange(true);
    });
  });

  it("calls navigator.keyboard.unlock() on blur and on pointer lock exit", async () => {
    await withStubbedDocument(async () => {
      const lock = vi.fn<[readonly string[]], Promise<void>>().mockResolvedValue(undefined);
      const unlock = vi.fn();

      await withFakeNavigatorKeyboard({ lock, unlock }, async () => {
        const canvas = makeCanvasStub({ requestPointerLock: () => {} });

        const ioWorker = { postMessage: () => {} };
        const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true, recycleBuffers: false });

        (capture as any).hasFocus = true;
        (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);
        (capture as any).pointerLock.locked = true;
        (capture as any).handlePointerLockChange(true);
        expect(lock).toHaveBeenCalledTimes(1);

        // Leaving capture via canvas blur should release the lock.
        (capture as any).handleBlur();
        expect(unlock).toHaveBeenCalledTimes(1);

        // Re-enter capture and ensure pointer lock exit also unlocks.
        (capture as any).hasFocus = true;
        (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);
        (capture as any).pointerLock.locked = true;
        (capture as any).handlePointerLockChange(true);

        (capture as any).pointerLock.locked = false;
        (capture as any).handlePointerLockChange(false);
        expect(unlock).toHaveBeenCalledTimes(2);
      });
    });
  });

  it("falls back to calling keyboard.lock() with no args when passing a key list is rejected", async () => {
    await withStubbedDocument(async () => {
      const lock = vi.fn().mockImplementation((codes?: readonly string[]) => {
        if (codes) {
          return Promise.reject(new TypeError("unsupported key list"));
        }
        return Promise.resolve();
      });
      const unlock = vi.fn();

      await withFakeNavigatorKeyboard({ lock, unlock }, async () => {
        const canvas = makeCanvasStub({ requestPointerLock: () => {} });

        const ioWorker = { postMessage: () => {} };
        const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true });

        (capture as any).hasFocus = true;
        (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);

        // Let the TypeError rejection be handled and the fallback call occur.
        await new Promise((resolve) => setTimeout(resolve, 0));

        expect(lock).toHaveBeenCalledTimes(2);
        expect(lock.mock.calls[0]?.[0]).toContain("Escape");
        expect(lock.mock.calls[1]?.length).toBe(0);
      });
    });
  });

  it("does not throw or leak unhandled rejections when keyboard lock is rejected", async () => {
    await withStubbedDocument(async () => {
      const lock = vi.fn<[readonly string[]], Promise<void>>().mockRejectedValue(new Error("nope"));
      const unlock = vi.fn();

      const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
      try {
        await withFakeNavigatorKeyboard({ lock, unlock }, async () => {
          const canvas = makeCanvasStub({ requestPointerLock: () => {} });

          const ioWorker = { postMessage: () => {} };
          const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true });

          (capture as any).hasFocus = true;
          (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);

          (capture as any).pointerLock.locked = true;
          (capture as any).handlePointerLockChange(true);

          // Give the promise rejection a chance to be observed by our catch handler.
          await new Promise((resolve) => setTimeout(resolve, 0));

          expect(lock).toHaveBeenCalledTimes(1);
          expect(debug).toHaveBeenCalled();
        });
      } finally {
        debug.mockRestore();
      }
    });
  });

  it("unlocks on pointerlockerror", async () => {
    await withStubbedDocument(async () => {
      const lock = vi.fn<[readonly string[]], Promise<void>>().mockResolvedValue(undefined);
      const unlock = vi.fn();

      await withFakeNavigatorKeyboard({ lock, unlock }, async () => {
        const canvas = makeCanvasStub({ requestPointerLock: () => {} });

        const ioWorker = { postMessage: () => {} };
        const capture = new InputCapture(canvas, ioWorker, { enableGamepad: false, enableKeyboardLock: true });

        (capture as any).hasFocus = true;
        (capture as any).handleClick({ preventDefault: () => {}, stopPropagation: () => {} } as unknown as MouseEvent);

        // Let the lock promise resolve so we don't race the in-flight `.then()` handler.
        await Promise.resolve();

        (capture as any).handlePointerLockError();
        expect(unlock).toHaveBeenCalled();
      });
    });
  });
});
