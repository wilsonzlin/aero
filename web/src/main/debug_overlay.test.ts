import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DebugOverlay } from "../../ui/debug_overlay";

describe("DebugOverlay hotkey handling", () => {
  const originalWindow = (globalThis as any).window;
  const originalDocument = (globalThis as any).document;
  const originalHTMLElement = (globalThis as any).HTMLElement;

  type Listener = { listener: (ev: any) => void; options?: unknown };
  let keydownListeners: Listener[] = [];

  class FakeHTMLElement {
    readonly tagName: string;
    isContentEditable = false;
    constructor(tagName: string) {
      this.tagName = tagName.toUpperCase();
    }
  }

  class FakeDiv {
    style: Record<string, string> = {};
    textContent: string | null = "";
    remove = vi.fn();
  }

  beforeEach(() => {
    keydownListeners = [];

    (globalThis as any).HTMLElement = FakeHTMLElement;
    (globalThis as any).document = {
      createElement: () => new FakeDiv(),
      body: { appendChild: vi.fn() },
    };
    (globalThis as any).window = {
      addEventListener: (type: string, listener: (ev: any) => void, options?: unknown) => {
        if (type === "keydown") keydownListeners.push({ listener, options });
      },
      removeEventListener: vi.fn(),
      setInterval: () => 0,
      clearInterval: () => {},
    };
  });

  afterEach(() => {
    (globalThis as any).window = originalWindow;
    (globalThis as any).document = originalDocument;
    (globalThis as any).HTMLElement = originalHTMLElement;
  });

  it("toggles only on unmodified keydown events that are not already defaultPrevented", () => {
    const parent = { appendChild: vi.fn() } as unknown as HTMLElement;
    const overlay = new DebugOverlay(() => null, { parent, toggleKey: "F3", updateIntervalMs: 10 });

    overlay.show();

    expect(keydownListeners).toHaveLength(1);
    expect(keydownListeners[0]!.options).toBeUndefined();

    const root = (overlay as any)._root as FakeDiv;
    expect(root.style.display).toBe("block");

    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();

    // Swallowed key toggles overlay.
    keydownListeners[0]!.listener({
      code: "F3",
      repeat: false,
      defaultPrevented: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: null,
      preventDefault,
      stopPropagation,
    });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
    expect(root.style.display).toBe("none");

    // Already-consumed events do not toggle.
    preventDefault.mockClear();
    stopPropagation.mockClear();
    keydownListeners[0]!.listener({
      code: "F3",
      repeat: false,
      defaultPrevented: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: null,
      preventDefault,
      stopPropagation,
    });
    expect(preventDefault).toHaveBeenCalledTimes(0);
    expect(stopPropagation).toHaveBeenCalledTimes(0);
    expect(root.style.display).toBe("none");

    // Modifier chords do not toggle.
    keydownListeners[0]!.listener({
      code: "F3",
      repeat: false,
      defaultPrevented: false,
      ctrlKey: true,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: null,
      preventDefault,
      stopPropagation,
    });
    expect(root.style.display).toBe("none");

    // Text input targets do not toggle.
    keydownListeners[0]!.listener({
      code: "F3",
      repeat: false,
      defaultPrevented: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: new FakeHTMLElement("input"),
      preventDefault,
      stopPropagation,
    });
    expect(root.style.display).toBe("none");
  });
});

