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

  it("renders scanout format names and last GPU event when present in telemetry", () => {
    const parent = { appendChild: vi.fn() } as unknown as HTMLElement;
    const snapshot = {
      framesReceived: 1,
      framesPresented: 2,
      framesDropped: 3,
      droppedFrames: 3,
      outputSource: "wddm_scanout",
      presentUpload: { kind: "none" },
      scanout: {
        source: 2,
        generation: 7,
        base_paddr: "0x0000000000001000",
        width: 800,
        height: 600,
        pitchBytes: 3200,
        format: 2, // AerogpuFormat.B8G8R8X8Unorm
      },
      gpuEvents: [{ severity: "warn", category: "CursorReadback", backend_kind: "webgpu", message: "vram missing" }],
      gpuStats: {
        backendKind: "webgpu",
        counters: {
          presents_attempted: 2,
          presents_succeeded: 1,
          recoveries_attempted: 3,
          recoveries_succeeded: 1,
          surface_reconfigures: 4,
          recoveries_attempted_wddm: 1,
          recoveries_succeeded_wddm: 1,
        },
      },
    };

    const overlay = new DebugOverlay(() => snapshot, { parent, toggleKey: "F3", updateIntervalMs: 10 });
    overlay.show();

    const root = (overlay as any)._root as FakeDiv;
    expect(root.textContent).toContain("Backend: webgpu");
    expect(root.textContent).toContain("Presents: 1/2  Recoveries: 1/3  Surface reconfigures: 4");
    expect(root.textContent).toContain("Recoveries (WDDM): 1/1");
    expect(root.textContent).toContain("Scanout:");
    expect(root.textContent).toContain("fmt=B8G8R8X8Unorm (2)");
    expect(root.textContent).toContain("Last event: warn/CursorReadback (webgpu): vram missing");

    overlay.detach();
  });
});
