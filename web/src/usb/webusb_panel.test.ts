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
    Reflect.deleteProperty(globalThis, "navigator");
  }

  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "document");
  }

  if (originalSecureContextDescriptor) {
    Object.defineProperty(globalThis, "isSecureContext", originalSecureContextDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "isSecureContext");
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
  it("includes a site settings link in the revocation hint", () => {
    stubIsSecureContext(true);
    stubNavigator({ usb: {} });
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
    const links = findAll(panel, (el) => el.tagName === "A" && el.textContent === "site settings");
    expect(links).toHaveLength(1);
    const href = (links[0] as unknown as { href?: unknown }).href;
    expect(typeof href).toBe("string");
    expect(String(href)).toContain("chrome://settings/content/siteDetails");
  });

  it("shows per-device Forget only when USBDevice.forget() exists", async () => {
    const forgettable = {
      vendorId: 0x1234,
      productId: 0x5678,
      opened: false,
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget: vi.fn(async () => {}),
    } as unknown as USBDevice;
    const normal = {
      vendorId: 0x1111,
      productId: 0x2222,
      opened: false,
      close: vi.fn(async () => {}),
    } as unknown as USBDevice;

    const usb = {
      getDevices: vi.fn(async () => [forgettable, normal]),
    } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb });
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

    const forgetButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget");
    expect(forgetButtons).toHaveLength(1);
  });

  it("closes the device before forgetting from the permitted-device list", async () => {
    const callOrder: string[] = [];
    let opened = true;
    const close = vi.fn(async () => {
      callOrder.push("close");
      opened = false;
    });
    const forget = vi.fn(async () => {
      callOrder.push("forget");
    });
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      get opened() {
        return opened;
      },
      close,
      forget,
    } as unknown as USBDevice;
    const getDevices = vi.fn(async () => [device]);

    const usb = { getDevices } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb });
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
    expect(getDevices).toHaveBeenCalledTimes(1);

    const rowForget = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget")[0];
    expect(rowForget).toBeTruthy();
    await (rowForget.onclick as () => Promise<void>)();

    expect(callOrder).toEqual(["close", "forget"]);
    expect(close).toHaveBeenCalledTimes(1);
    expect(forget).toHaveBeenCalledTimes(1);
    // Successful forget triggers a list refresh (getDevices called again).
    expect(getDevices).toHaveBeenCalledTimes(2);
  });

  it("forgets the selected device permission when USBDevice.forget() is available", async () => {
    let permittedDevices: USBDevice[] = [];
    const forget = vi.fn(async () => {
      permittedDevices = [];
    });
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      opened: false,
      productName: "Example USB Device",
      manufacturerName: "Acme",
      serialNumber: "sn-1",
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget,
    } as unknown as USBDevice;
    permittedDevices = [device];
    const getDevices = vi.fn(async () => permittedDevices);

    const usb = {
      getDevices,
    } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb });
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
    expect(getDevices).toHaveBeenCalledTimes(1);

    const selectButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Select");
    expect(selectButtons.length).toBe(1);
    (selectButtons[0].onclick as () => void)();

    const status = findAll(panel, (el) => el.tagName === "PRE")[0];
    expect(status.textContent).toContain("Selected device:");

    const forgetButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget permission")[0];
    expect(forgetButton).toBeTruthy();
    expect(forgetButton.hidden).toBe(false);

    await (forgetButton.onclick as () => Promise<void>)();

    expect(forget).toHaveBeenCalledTimes(1);
    expect(getDevices).toHaveBeenCalledTimes(2);
    expect(status.textContent).toContain("No device selected");
    expect(forgetButton.hidden).toBe(true);

    const remainingSelectButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Select");
    expect(remainingSelectButtons).toHaveLength(0);
  });

  it("surfaces forget() errors without breaking the panel", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const forget = vi.fn(async () => {
      throw new Error("boom");
    });
    const device = {
      vendorId: 0x1234,
      productId: 0x5678,
      opened: false,
      productName: "Example USB Device",
      close: vi.fn(async () => {}),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      forget,
    } as unknown as USBDevice;

    const usb = {
      getDevices: vi.fn(async () => [device]),
    } satisfies Partial<USB>;

    stubIsSecureContext(true);
    stubNavigator({ usb });
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
    try {
      await (listButton.onclick as () => Promise<void>)();

      const selectButtons = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Select");
      (selectButtons[0].onclick as () => void)();

      const forgetButton = findAll(panel, (el) => el.tagName === "BUTTON" && el.textContent === "Forget permission")[0];
      await (forgetButton.onclick as () => Promise<void>)();

      const errorTitle = findAll(panel, (el) => el.tagName === "DIV" && el.className === "bad")[0];
      expect(errorTitle.textContent).toContain("WebUSB");
      expect(forget).toHaveBeenCalledTimes(1);
      expect(forgetButton.hidden).toBe(false);
    } finally {
      consoleError.mockRestore();
    }
  });
});
