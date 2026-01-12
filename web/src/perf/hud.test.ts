import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { installHud } from "./hud";
import type { PerfApi, PerfHudSnapshot } from "./types";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (err: unknown) => void;
};

function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("Perf HUD Trace JSON export", () => {
  const originalDocument = (globalThis as typeof globalThis & { document?: unknown }).document;
  const originalWindow = (globalThis as typeof globalThis & { window?: unknown }).window;
  const originalHTMLElement = (globalThis as typeof globalThis & { HTMLElement?: unknown }).HTMLElement;
  const originalHTMLCanvasElement = (globalThis as typeof globalThis & { HTMLCanvasElement?: unknown }).HTMLCanvasElement;
  const originalHTMLAnchorElement = (globalThis as typeof globalThis & { HTMLAnchorElement?: unknown }).HTMLAnchorElement;
  const originalHTMLButtonElement = (globalThis as typeof globalThis & { HTMLButtonElement?: unknown }).HTMLButtonElement;
  const originalHTMLDivElement = (globalThis as typeof globalThis & { HTMLDivElement?: unknown }).HTMLDivElement;
  const originalCreateObjectURL = (URL as unknown as { createObjectURL?: unknown }).createObjectURL;
  const originalRevokeObjectURL = (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL;
  const originalBlob = globalThis.Blob;

  class FakeEventTarget {
    private readonly listeners = new Map<string, Set<(...args: unknown[]) => unknown>>();

    addEventListener(type: string, listener: (...args: unknown[]) => unknown): void {
      const set = this.listeners.get(type) ?? new Set();
      set.add(listener);
      this.listeners.set(type, set);
    }

    removeEventListener(type: string, listener: (...args: unknown[]) => unknown): void {
      this.listeners.get(type)?.delete(listener);
    }

    protected dispatch(type: string): void {
      for (const listener of this.listeners.get(type) ?? []) {
        try {
          const result = listener();
          if (result && typeof (result as Promise<unknown>).then === "function") {
            void (result as Promise<unknown>).catch(() => {});
          }
        } catch {
          // Ignore to mimic browser event dispatch (errors surface via console).
        }
      }
    }
  }

  class FakeHTMLElement extends FakeEventTarget {
    readonly tagName: string;
    id = "";
    className = "";
    textContent: string | null = "";
    type = "";
    hidden = false;
    disabled = false;
    href = "";
    download = "";
    isContentEditable = false;
    parentElement: FakeHTMLElement | null = null;
    private readonly childrenInternal: FakeHTMLElement[] = [];

    constructor(tagName: string) {
      super();
      this.tagName = tagName.toUpperCase();
    }

    append(...nodes: FakeHTMLElement[]): void {
      for (const node of nodes) {
        node.parentElement = this;
        this.childrenInternal.push(node);
      }
    }

    remove(): void {
      const parent = this.parentElement;
      if (!parent) return;
      const idx = parent.childrenInternal.indexOf(this);
      if (idx >= 0) parent.childrenInternal.splice(idx, 1);
      this.parentElement = null;
    }

    set innerHTML(_value: string) {
      this.childrenInternal.length = 0;
      this.textContent = "";
    }

    querySelectorAll<T extends FakeHTMLElement = FakeHTMLElement>(selector: string): T[] {
      const out: FakeHTMLElement[] = [];
      const isId = selector.startsWith("#");
      const needle = isId ? selector.slice(1) : selector.toUpperCase();
      const visit = (el: FakeHTMLElement): void => {
        for (const child of el.childrenInternal) {
          if (isId ? child.id === needle : child.tagName === needle) out.push(child);
          visit(child);
        }
      };
      visit(this);
      return out as T[];
    }

    click(): void {
      if (this.disabled) return;
      this.dispatch("click");
    }
  }

  class FakeHTMLCanvasElement extends FakeHTMLElement {
    width = 0;
    height = 0;
    constructor() {
      super("canvas");
    }
    getContext(_type: string): CanvasRenderingContext2D | null {
      return null;
    }
  }

  class FakeHTMLAnchorElement extends FakeHTMLElement {
    constructor() {
      super("a");
    }
  }

  class FakeHTMLButtonElement extends FakeHTMLElement {
    constructor() {
      super("button");
    }
  }

  class FakeHTMLDivElement extends FakeHTMLElement {
    constructor() {
      super("div");
    }
  }

  class FakeDocument {
    readonly body = new FakeHTMLElement("body");

    createElement(tagName: string): FakeHTMLElement {
      switch (tagName.toLowerCase()) {
        case "canvas":
          return new FakeHTMLCanvasElement();
        case "a":
          return new FakeHTMLAnchorElement();
        case "button":
          return new FakeHTMLButtonElement();
        case "div":
          return new FakeHTMLDivElement();
        default:
          return new FakeHTMLElement(tagName);
      }
    }

    querySelector<T extends FakeHTMLElement = FakeHTMLElement>(selector: string): T | null {
      return this.body.querySelectorAll<T>(selector)[0] ?? null;
    }

    querySelectorAll<T extends FakeHTMLElement = FakeHTMLElement>(selector: string): T[] {
      return this.body.querySelectorAll<T>(selector);
    }
  }

  beforeEach(() => {
    const g = globalThis as unknown as {
      window?: unknown;
      document?: unknown;
      HTMLElement?: unknown;
      HTMLCanvasElement?: unknown;
      HTMLAnchorElement?: unknown;
      HTMLButtonElement?: unknown;
      HTMLDivElement?: unknown;
    };
    g.window = {
      devicePixelRatio: 1,
      setInterval: globalThis.setInterval.bind(globalThis),
      clearInterval: globalThis.clearInterval.bind(globalThis),
      setTimeout: globalThis.setTimeout.bind(globalThis),
      clearTimeout: globalThis.clearTimeout.bind(globalThis),
      addEventListener: () => {},
      removeEventListener: () => {},
    };
    g.document = new FakeDocument();
    g.HTMLElement = FakeHTMLElement;
    g.HTMLCanvasElement = FakeHTMLCanvasElement;
    g.HTMLAnchorElement = FakeHTMLAnchorElement;
    g.HTMLButtonElement = FakeHTMLButtonElement;
    g.HTMLDivElement = FakeHTMLDivElement;
  });

  afterEach(() => {
    (globalThis as typeof globalThis & { document?: { body?: { innerHTML: string } } }).document?.body && (document.body.innerHTML = "");
    vi.restoreAllMocks();
    (URL as unknown as { createObjectURL?: unknown }).createObjectURL = originalCreateObjectURL;
    (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL = originalRevokeObjectURL;
    globalThis.Blob = originalBlob;
    (globalThis as typeof globalThis & { document?: unknown }).document = originalDocument;
    (globalThis as typeof globalThis & { window?: unknown }).window = originalWindow;
    (globalThis as typeof globalThis & { HTMLElement?: unknown }).HTMLElement = originalHTMLElement;
    (globalThis as typeof globalThis & { HTMLCanvasElement?: unknown }).HTMLCanvasElement = originalHTMLCanvasElement;
    (globalThis as typeof globalThis & { HTMLAnchorElement?: unknown }).HTMLAnchorElement = originalHTMLAnchorElement;
    (globalThis as typeof globalThis & { HTMLButtonElement?: unknown }).HTMLButtonElement = originalHTMLButtonElement;
    (globalThis as typeof globalThis & { HTMLDivElement?: unknown }).HTMLDivElement = originalHTMLDivElement;
  });

  it("awaits exportTrace({ asString: true }) and downloads the returned string", async () => {
    class FakeBlob {
      readonly parts: unknown[];
      readonly type: string;
      constructor(parts: unknown[], opts?: { type?: string }) {
        this.parts = parts;
        this.type = opts?.type ?? "";
      }
    }

    globalThis.Blob = FakeBlob as unknown as typeof Blob;

    const ctx = {
      setTransform: vi.fn(),
      clearRect: vi.fn(),
      beginPath: vi.fn(),
      moveTo: vi.fn(),
      lineTo: vi.fn(),
      stroke: vi.fn(),
      strokeStyle: "",
      lineWidth: 0,
    };
    vi.spyOn(
      HTMLCanvasElement.prototype as unknown as { getContext: (contextId: string, options?: unknown) => unknown },
      "getContext",
    ).mockImplementation((contextId) => (contextId === "2d" ? ctx : null));
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    const createdUrls: Blob[] = [];
    const createObjectURL = vi.fn((blob: Blob) => {
      createdUrls.push(blob);
      return "blob:trace";
    });
    const revokeObjectURL = vi.fn();
    (URL as unknown as { createObjectURL?: unknown }).createObjectURL = createObjectURL;
    (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL = revokeObjectURL;

    const deferred = defer<string>();
    const exportTrace = vi.fn(() => deferred.promise);

    const perf = {
      getHudSnapshot: (out: PerfHudSnapshot) => out,
      setHudActive: () => {},
      captureStart: () => {},
      captureStop: () => {},
      captureReset: () => {},
      export: () => ({}),
      traceStart: () => {},
      traceStop: () => {},
      exportTrace,
      traceEnabled: false,
    } as unknown as PerfApi;

    installHud(perf);

    const traceButton = Array.from(
      (globalThis.document as unknown as { querySelectorAll: (sel: string) => HTMLButtonElement[] }).querySelectorAll("button"),
    ).find((btn) => btn.textContent === "Trace JSON");
    expect(traceButton).toBeTruthy();

    traceButton!.click();

    expect(exportTrace).toHaveBeenCalledWith({ asString: true });
    expect(traceButton!.disabled).toBe(true);
    expect(createObjectURL).not.toHaveBeenCalled();

    const payload = '{"traceEvents":[]}';
    deferred.resolve(payload);
    await deferred.promise;

    // Let the async click handler resume after the awaited exportTrace promise.
    for (let i = 0; i < 5 && createObjectURL.mock.calls.length === 0; i += 1) {
      await Promise.resolve();
    }

    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(createdUrls).toHaveLength(1);
    const blob = createdUrls[0] as unknown as FakeBlob;
    expect(blob.type).toBe("application/json");
    expect(blob.parts).toEqual([payload]);
    expect(traceButton!.disabled).toBe(false);
  });

  it("does not trigger HUD hotkeys when the keydown event is already preventDefault()'d", () => {
    const keydownListeners: Array<(ev: any) => void> = [];
    const windowWithEvents = globalThis.window as unknown as {
      addEventListener: (type: string, listener: (ev: any) => void) => void;
      removeEventListener: (type: string, listener: (ev: any) => void) => void;
      setInterval?: (cb: () => void, ms: number) => unknown;
      clearInterval?: (id: unknown) => void;
    };
    windowWithEvents.addEventListener = (type, listener) => {
      if (type === "keydown") keydownListeners.push(listener);
    };
    windowWithEvents.removeEventListener = () => {};
    // Avoid creating real timers in unit tests.
    windowWithEvents.setInterval = () => 0;
    windowWithEvents.clearInterval = () => {};

    const ctx = {
      setTransform: vi.fn(),
      clearRect: vi.fn(),
      beginPath: vi.fn(),
      moveTo: vi.fn(),
      lineTo: vi.fn(),
      stroke: vi.fn(),
      strokeStyle: "",
      lineWidth: 0,
    };
    vi.spyOn(
      HTMLCanvasElement.prototype as unknown as { getContext: (contextId: string, options?: unknown) => unknown },
      "getContext",
    ).mockImplementation((contextId) => (contextId === "2d" ? ctx : null));

    const perf = {
      getHudSnapshot: (out: PerfHudSnapshot) => out,
      setHudActive: () => {},
      captureStart: () => {},
      captureStop: () => {},
      captureReset: () => {},
      export: () => ({}),
    } as unknown as PerfApi;

    installHud(perf);

    expect(keydownListeners).toHaveLength(1);

    const hud = (globalThis.document as unknown as FakeDocument)
      .querySelectorAll<FakeHTMLElement>("div")
      .find((el) => el.className === "aero-perf-hud");
    expect(hud).toBeTruthy();
    expect(hud!.hidden).toBe(true);

    const invoke = (defaultPrevented: boolean) => {
      const preventDefault = vi.fn();
      keydownListeners[0]!({
        repeat: false,
        target: null,
        key: "F2",
        code: "F2",
        ctrlKey: false,
        shiftKey: false,
        defaultPrevented,
        preventDefault,
      });
      return preventDefault;
    };

    const alreadyPrevented = invoke(true);
    expect(alreadyPrevented).not.toHaveBeenCalled();
    expect(hud!.hidden).toBe(true);

    const fresh = invoke(false);
    expect(fresh).toHaveBeenCalledTimes(1);
    expect(hud!.hidden).toBe(false);
  });
});
