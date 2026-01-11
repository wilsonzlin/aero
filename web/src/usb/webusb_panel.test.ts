import { afterEach, describe, expect, it, vi } from "vitest";

import { renderWebUsbPanel } from "./webusb_panel";
import type { PlatformFeatureReport } from "../platform/features";

const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
const originalDocumentDescriptor = Object.getOwnPropertyDescriptor(globalThis, "document");
const originalSecureContextDescriptor = Object.getOwnPropertyDescriptor(globalThis, "isSecureContext");

function stubNavigator(value: unknown): void {
  Object.defineProperty(globalThis, "navigator", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

function stubDocument(value: unknown): void {
  Object.defineProperty(globalThis, "document", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

function stubIsSecureContext(value: unknown): void {
  Object.defineProperty(globalThis, "isSecureContext", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

afterEach(() => {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "navigator");
  }

  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "document");
  }

  if (originalSecureContextDescriptor) {
    Object.defineProperty(globalThis, "isSecureContext", originalSecureContextDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as any, "isSecureContext");
  }
});

class FakeElement {
  readonly tagName: string;
  className = "";
  disabled = false;
  hidden = false;
  readonly children: FakeElement[] = [];
  readonly attributes: Record<string, string> = {};
  onclick?: () => unknown;
  style: Record<string, string> = {};

  private _text = "";

  constructor(tagName: string) {
    this.tagName = tagName.toUpperCase();
  }

  setAttribute(name: string, value: string): void {
    this.attributes[name] = value;
  }

  append(...children: Array<FakeElement | string>): void {
    for (const child of children) {
      if (child === null || child === undefined) continue;
      if (typeof child === "string") {
        const text = new FakeElement("#text");
        text.textContent = child;
        this.children.push(text);
      } else {
        this.children.push(child);
      }
    }
  }

  replaceChildren(...children: FakeElement[]): void {
    this.children.length = 0;
    this.children.push(...children.filter(Boolean));
  }

  set textContent(value: string | null) {
    this._text = value ?? "";
    this.children.length = 0;
  }

  get textContent(): string {
    return [this._text, ...this.children.map((c) => c.textContent)].join("");
  }
}

class FakeDocument {
  createElement(tag: string): FakeElement {
    return new FakeElement(tag);
  }
}

function findAll(root: FakeElement, predicate: (el: FakeElement) => boolean): FakeElement[] {
  const out: FakeElement[] = [];
  const walk = (node: FakeElement): void => {
    if (predicate(node)) out.push(node);
    for (const child of node.children) walk(child);
  };
  walk(root);
  return out;
}

describe("renderWebUsbPanel UI (mocked WebUSB)", () => {
  it("forgets the selected device permission when USBDevice.forget() is available", async () => {
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      opened: false,
      productName: "Example USB Device",
      manufacturerName: "Acme",
      serialNumber: "sn-1",
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget: vi.fn(async () => {}),
    } as unknown as USBDevice;

    const usb = {
      getDevices: vi.fn(async () => [device]),
    } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb } as any);
    stubDocument(new FakeDocument());

    const report: PlatformFeatureReport = {
      crossOriginIsolated: false,
      sharedArrayBuffer: false,
      wasmSimd: false,
      wasmThreads: false,
      jit_dynamic_wasm: false,
      webgpu: false,
      webusb: true,
      webhid: false,
      webgl2: false,
      opfs: false,
      opfsSyncAccessHandle: false,
      audioWorklet: false,
      offscreenCanvas: false,
    };

    const panel = renderWebUsbPanel(report) as unknown as FakeElement;

    const listButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "List permitted devices (getDevices)")[0];
    expect(listButton).toBeTruthy();
    await (listButton.onclick as () => Promise<void>)();

    const selectButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Select");
    expect(selectButtons.length).toBe(1);
    (selectButtons[0].onclick as () => void)();

    const status = findAll(panel, (el) => el.tagName === "PRE")[0];
    expect(status.textContent).toContain("Selected device:");

    const forgetButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget permission")[0];
    expect(forgetButton).toBeTruthy();
    expect(forgetButton.hidden).toBe(false);

    await (forgetButton.onclick as () => Promise<void>)();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect((device as any).forget).toHaveBeenCalledTimes(1);
    expect(status.textContent).toContain("No device selected");
    expect(forgetButton.hidden).toBe(true);
  });

  it("surfaces forget() errors without breaking the panel", async () => {
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      opened: false,
      productName: "Example USB Device",
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget: vi.fn(async () => {
        throw new Error("boom");
      }),
    } as unknown as USBDevice;

    const usb = {
      getDevices: vi.fn(async () => [device]),
    } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb } as any);
    stubDocument(new FakeDocument());

    const report: PlatformFeatureReport = {
      crossOriginIsolated: false,
      sharedArrayBuffer: false,
      wasmSimd: false,
      wasmThreads: false,
      jit_dynamic_wasm: false,
      webgpu: false,
      webusb: true,
      webhid: false,
      webgl2: false,
      opfs: false,
      opfsSyncAccessHandle: false,
      audioWorklet: false,
      offscreenCanvas: false,
    };

    const panel = renderWebUsbPanel(report) as unknown as FakeElement;

    const listButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "List permitted devices (getDevices)")[0];
    await (listButton.onclick as () => Promise<void>)();

    const selectButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Select");
    (selectButtons[0].onclick as () => void)();

    const forgetButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget permission")[0];
    await (forgetButton.onclick as () => Promise<void>)();

    const errorTitle = findAll(panel, (el) => el.tagName === "DIV" && el.className === "bad")[0];
    expect(errorTitle.textContent).toContain("WebUSB");
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect((device as any).forget).toHaveBeenCalledTimes(1);
    expect(forgetButton.hidden).toBe(false);
  });
});
